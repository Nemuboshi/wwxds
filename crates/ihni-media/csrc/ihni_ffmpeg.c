/*
 * In-process FFmpeg boundary for ihni-media.
 *
 * This is the only file in the project that sees an AVFormatContext,
 * AVCodecContext, AVFrame, or AVPacket layout. The Rust side holds an opaque
 * handle and declares exactly the functions at the bottom of this file; it
 * never includes a libav header and never assumes a struct offset.
 *
 * The handle owns one input file and one selected stream. Decoded frames are
 * returned as a view into the decoder's current frame, valid only until the
 * next call on the same handle.
 */

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>

#include <d3d11.h>
#include <d3d11_1.h>
#include <d3d11_3.h>
#include <d3dcompiler.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* Status codes shared with the Rust declarations in src/ffi.rs. */
#define IHNI_STATUS_OPEN 1
#define IHNI_STATUS_STREAM_NOT_FOUND 2
#define IHNI_STATUS_NO_VIDEO 3
#define IHNI_STATUS_CODEC 4

/*
 * Luma formats this shim reports and accepts, matching the Rust `LumaFormat`
 * enum. They are this project's own codes rather than AVPixelFormat values, so
 * no libav enum constant is mirrored in Rust and the mapping from a decoded
 * frame to a scaler source format stays on this side of the boundary.
 */
#define IHNI_LUMA_GRAY8 0
#define IHNI_LUMA_GRAY10LE 1
#define IHNI_LUMA_GRAY10BE 2
#define IHNI_LUMA_UNSUPPORTED (-1)

/* Largest diagnostic text kept for one run, matching the reader's old limit. */
#define IHNI_MAX_DIAGNOSTIC 8192

/*
 * Flat value types exchanged with Rust. They contain no libav struct and no
 * pointer that outlives the call that produced it, so Rust can declare them
 * field for field without depending on any libav layout.
 */
typedef struct ihni_stream_info {
    int stream_index;
    int codec_type;
    int width;
    int height;
    int pixel_format;
    int bits_per_raw_sample;
    int frame_rate_num;
    int frame_rate_den;
    int average_frame_rate_num;
    int average_frame_rate_den;
    int time_base_num;
    int time_base_den;
    int has_start_time;
    double start_time_seconds;
} ihni_stream_info;

typedef struct ihni_frame_view {
    int width;
    int height;
    int pixel_format;
    /* An IHNI_LUMA_* code, not an AVPixelFormat value. */
    int luma_pixel_format;
    int stride;
    const uint8_t *data;
    int has_pts;
    int64_t pts;
    /* D3D11 hardware frames: `is_d3d11` selects the texture fields, and the
     * software plane fields are then unused. The texture pointer is only
     * valid until the next call on the same handle, like `data`. */
    int is_d3d11;
    const void *texture;
    int subresource;
} ihni_frame_view;

/*
 * The analysis-plane filter parameters of one GPU run. They are the exact
 * integers the CPU box filter computes (`docs/Architecture.md` §4): every
 * output sample is the rounded mean of a `block_w * block_h` whole-pixel
 * block, reduced by `out_shift` after rounding for 10-bit sources, and the
 * raw hardware sample is reduced by `sample_shift` first (P010 stores its
 * 10 bits in the high half of each 16-bit sample).
 */
typedef struct ihni_d3d11_params {
    int dst_width;
    int dst_height;
    int block_w;
    int block_h;
    int half;
    int block;
    int out_shift;
    int sample_shift;
} ihni_d3d11_params;

typedef struct ihni_input {
    AVFormatContext *format;
    AVCodecContext *codec;
    const AVCodec *decoder;
    AVPacket *packet;
    AVFrame *frame;
    int stream_index;
    int decoder_threads;
    int demux_eof;
    int have_packet;
    /* D3D11 hardware decode and filter state; unused on the CPU path. */
    int use_d3d11;
    AVBufferRef *hw_device;
    ID3D11Device3 *d3d11_device;
    ID3D11DeviceContext *d3d11_context;
    ID3D11Texture2D *d3d11_frame_texture;
    ID3D11ShaderResourceView1 *d3d11_srv;
    ID3D11ComputeShader *d3d11_shader;
    ID3D11Texture2D *d3d11_output;
    ID3D11UnorderedAccessView *d3d11_uav;
    ID3D11Texture2D *d3d11_staging;
    ihni_d3d11_params d3d11_params;
} ihni_input;

/*
 * The production analysis-plane filter is the project's own integer box
 * average in Rust; this shim only decodes.
 */
void ihni_input_close(ihni_input *in);

/* ------------------------------------------------------------------------- */
/* Diagnostics                                                               */
/* ------------------------------------------------------------------------- */

/*
 * libav reports problems through one process-global callback, and a decoder with
 * frame threading calls av_log from worker threads. The buffer is therefore
 * per-thread: a reader is used from exactly one thread, so each reader
 * accumulates its own diagnostics and two readers in one process cannot see
 * each other's. Sharing one buffer would let a second reader take the first
 * one's diagnostics, which would turn a damaged input into a clean one.
 */
#if defined(_MSC_VER)
#define IHNI_THREAD_LOCAL __declspec(thread)
#else
#define IHNI_THREAD_LOCAL _Thread_local
#endif

static IHNI_THREAD_LOCAL char g_diagnostic[IHNI_MAX_DIAGNOSTIC];
static IHNI_THREAD_LOCAL size_t g_diagnostic_len;
static IHNI_THREAD_LOCAL int g_diagnostic_truncated;

static void ihni_append_diagnostic(const char *text, size_t length)
{
    if (length == 0) {
        return;
    }

    size_t room = sizeof(g_diagnostic) - g_diagnostic_len;
    if (room <= 1) {
        g_diagnostic_truncated = 1;
        return;
    }
    size_t take = length < room - 1 ? length : room - 1;
    memcpy(g_diagnostic + g_diagnostic_len, text, take);
    g_diagnostic_len += take;
    if (take < length) {
        g_diagnostic_truncated = 1;
    } else if (g_diagnostic_len + 1 < sizeof(g_diagnostic)) {
        g_diagnostic[g_diagnostic_len++] = '\n';
    } else {
        g_diagnostic_truncated = 1;
    }
    g_diagnostic[g_diagnostic_len] = '\0';
}

static void ihni_log_callback(void *avcl, int level, const char *format,
                              va_list args)
{
    if (level > AV_LOG_ERROR) {
        return;
    }
    (void)avcl;

    char line[2048];
    if (vsnprintf(line, sizeof(line), format, args) <= 0) {
        return;
    }
    size_t length = strlen(line);
    while (length > 0 && (line[length - 1] == '\n' || line[length - 1] == '\r')) {
        line[--length] = '\0';
    }
    ihni_append_diagnostic(line, length);
}

/* Logs a libav return code through the callback, so it reaches the caller. */
static void ihni_log_error_code(const char *operation, int code)
{
    char detail[AV_ERROR_MAX_STRING_SIZE];
    detail[0] = '\0';
    av_strerror(code, detail, sizeof(detail));
    av_log(NULL, AV_LOG_ERROR, "%s failed: %s", operation, detail);
}

static void ihni_log_reset(void)
{
    g_diagnostic_len = 0;
    g_diagnostic_truncated = 0;
    g_diagnostic[0] = '\0';
}

/*
 * Must be called once before any other entry point. Rust guards this with a
 * std::sync::Once. The callback is process-global; the buffer it writes to is
 * per-thread, so no lock is involved.
 */
void ihni_ffmpeg_init(void)
{
    av_log_set_callback(ihni_log_callback);
}

const char *ihni_av_version(void)
{
    return av_version_info();
}

/*
 * Names of value types, so Rust never needs a libav enum constant. Each returns
 * a pointer to static storage that stays valid for the process; an unknown
 * value becomes an empty string rather than a null pointer.
 */
const char *ihni_pix_fmt_name(int format)
{
    const char *name = av_get_pix_fmt_name((enum AVPixelFormat)format);
    return name != NULL ? name : "";
}

const char *ihni_media_type_name(int type)
{
    const char *name = av_get_media_type_string((enum AVMediaType)type);
    return name != NULL ? name : "unknown";
}

/* Reports whether a stream's codec type is video, without exposing the enum. */
int ihni_media_type_is_video(int type)
{
    return type == AVMEDIA_TYPE_VIDEO;
}

/* Copies the accumulated diagnostics into caller storage and clears them. */
int ihni_log_take(char *buffer, size_t length)
{
    if (buffer == NULL || length == 0) {
        ihni_log_reset();
        return 0;
    }

    size_t take = g_diagnostic_len;
    if (take >= length) {
        take = length - 1;
    }
    if (take > 0) {
        memcpy(buffer, g_diagnostic, take);
    }
    if (g_diagnostic_truncated && take + 16 < length) {
        const char marker[] = "[truncated]";
        memcpy(buffer + take, marker, sizeof(marker) - 1);
        take += sizeof(marker) - 1;
    }
    buffer[take] = '\0';
    int had = g_diagnostic_len > 0;
    ihni_log_reset();
    return had ? (int)take : 0;
}

static void ihni_set_error(char *err, size_t errlen, const char *text)
{
    if (err == NULL || errlen == 0) {
        return;
    }
    snprintf(err, errlen, "%s", text);
}

static void ihni_set_error_code(char *err, size_t errlen, const char *operation,
                                int code)
{
    if (err == NULL || errlen == 0) {
        return;
    }
    char detail[AV_ERROR_MAX_STRING_SIZE];
    detail[0] = '\0';
    av_strerror(code, detail, sizeof(detail));
    snprintf(err, errlen, "%s failed: %s", operation, detail);
}

/* Copies the accumulated diagnostics into `err` and clears them. */
static void ihni_error_with_diagnostic(char *err, size_t errlen,
                                       const char *fallback)
{
    if (err == NULL || errlen == 0) {
        ihni_log_reset();
        return;
    }
    int take = ihni_log_take(err, errlen);
    if (take <= 0) {
        ihni_set_error(err, errlen, fallback);
    }
}

/* ------------------------------------------------------------------------- */
/* Pixel formats                                                             */
/* ------------------------------------------------------------------------- */

/*
 * The gray equivalent of a source depth, which is the format `extractplanes=y`
 * hands to the scaler. IHNI_LUMA_UNSUPPORTED means the depth is unsupported.
 */
static int ihni_luma_format(enum AVPixelFormat format)
{
    const AVPixFmtDescriptor *descriptor = av_pix_fmt_desc_get(format);
    if (descriptor == NULL) {
        return IHNI_LUMA_UNSUPPORTED;
    }
    int depth = descriptor->comp[0].depth;
    int big_endian = (descriptor->flags & AV_PIX_FMT_FLAG_BE) != 0;

    if (depth == 8) {
        return IHNI_LUMA_GRAY8;
    }
    if (depth == 10) {
        return big_endian ? IHNI_LUMA_GRAY10BE : IHNI_LUMA_GRAY10LE;
    }
    return IHNI_LUMA_UNSUPPORTED;
}

/* Reports whether a luma format from ihni_luma_format can be prepared. */
int ihni_luma_format_supported(int luma_format)
{
    switch (luma_format) {
    case IHNI_LUMA_GRAY8:
    case IHNI_LUMA_GRAY10LE:
    case IHNI_LUMA_GRAY10BE:
        return 1;
    default:
        return 0;
    }
}

/* ------------------------------------------------------------------------- */
/* Input                                                                     */
/* ------------------------------------------------------------------------- */

/*
 * Forces the d3d11va pixel format for GPU runs. libavcodec calls this after
 * the hardware device is known; returning D3D11 makes every decoded frame a
 * D3D11 texture. A stream the hardware cannot decode reaches this callback
 * with D3D11 missing from the list, and the run then fails instead of
 * silently mixing hardware and software frames.
 */
static enum AVPixelFormat ihni_d3d11_get_format(
    AVCodecContext *context, const enum AVPixelFormat *formats)
{
    for (const enum AVPixelFormat *candidate = formats; *candidate != AV_PIX_FMT_NONE;
         candidate++) {
        if (*candidate == AV_PIX_FMT_D3D11) {
            return AV_PIX_FMT_D3D11;
        }
    }
    return formats[0];
}

static int ihni_open_decoder(ihni_input *in, char *err, size_t errlen)
{
    AVCodecParameters *parameters = in->format->streams[in->stream_index]->codecpar;

    in->decoder = avcodec_find_decoder(parameters->codec_id);
    if (in->decoder == NULL) {
        const char *name = avcodec_get_name(parameters->codec_id);
        snprintf(err, errlen,
                 "no decoder is available for codec '%s' in the selected stream",
                 name);
        return IHNI_STATUS_CODEC;
    }

    in->codec = avcodec_alloc_context3(in->decoder);
    if (in->codec == NULL) {
        ihni_set_error(err, errlen, "cannot allocate the decoder context");
        return IHNI_STATUS_CODEC;
    }

    int result = avcodec_parameters_to_context(in->codec, parameters);
    if (result < 0) {
        ihni_set_error_code(err, errlen, "copying the stream parameters", result);
        return IHNI_STATUS_CODEC;
    }

    /*
     * D3D11 hardware decode: register the hardware device and force the
     * d3d11va pixel format through `get_format`, so the decoder's frames stay
     * in GPU memory as NV12 or P010 textures. The decoder creates its own
     * frame pool; FFmpeg binds its textures for shader access by default,
     * and a failure to do so surfaces as a filter error the caller falls
     * back from.
     */
    if (in->use_d3d11) {
        int hwresult = av_hwdevice_ctx_create(&in->hw_device, AV_HWDEVICE_TYPE_D3D11VA,
                                              NULL, NULL, 0);
        if (hwresult < 0) {
            ihni_error_with_diagnostic(err, errlen,
                                       "cannot create the D3D11 device for decoding");
            return IHNI_STATUS_CODEC;
        }
        AVHWDeviceContext *device_context = (AVHWDeviceContext *)in->hw_device->data;
        AVD3D11VADeviceContext *d3d11 = device_context->hwctx;
        HRESULT status;

        /*
         * The filter binds video-plane views, a D3D11.3 capability
         * (ID3D11Device3), so the device is queried for that interface.
         * Windows 10 1511 or newer is the practical floor; the failure is an
         * error the caller falls back from. The context is shared as-is.
         */
        status = d3d11->device->lpVtbl->QueryInterface(
            d3d11->device, &IID_ID3D11Device3, (void **)&in->d3d11_device);
        if (FAILED(status)) {
            ihni_set_error(err, errlen,
                           "the D3D11 device lacks the 11.3 interface (Windows 10 "
                           "1511 or newer is required)");
            return IHNI_STATUS_CODEC;
        }
        d3d11->device_context->lpVtbl->AddRef(d3d11->device_context);
        in->d3d11_context = d3d11->device_context;
        in->codec->hw_device_ctx = av_buffer_ref(in->hw_device);
        if (in->codec->hw_device_ctx == NULL) {
            ihni_set_error(err, errlen, "cannot reference the D3D11 device");
            return IHNI_STATUS_CODEC;
        }
        in->codec->get_format = ihni_d3d11_get_format;
        /*
         * The decoder's own frame pool is left as libavcodec builds it. Its
         * textures are decoder-bound only, so the filter copies each frame
         * into the filter's own shader-readable texture before reading it
         * (see ihni_d3d11_build); a GPU-to-GPU copy per frame is far cheaper
         * than a host round trip.
         */
    }

    /* `-hwaccel none`: this decoder is the software one found above. */
    in->codec->thread_count = in->decoder_threads;

    result = avcodec_open2(in->codec, in->decoder, NULL);
    if (result < 0) {
        ihni_set_error_code(err, errlen, "opening the decoder", result);
        return IHNI_STATUS_CODEC;
    }
    return 0;
}

int ihni_input_open(const char *path, int stream_index, int decoder_threads,
                    ihni_input **out, char *err, size_t errlen)
{
    return ihni_input_open_internal(path, stream_index, decoder_threads, 0, out, err,
                                    errlen);
}

int ihni_input_open_gpu(const char *path, int stream_index, int decoder_threads,
                        ihni_input **out, char *err, size_t errlen)
{
    return ihni_input_open_internal(path, stream_index, decoder_threads, 1, out, err,
                                    errlen);
}

static int ihni_input_open_internal(const char *path, int stream_index,
                                    int decoder_threads, int use_d3d11,
                                    ihni_input **out, char *err, size_t errlen);

static int ihni_input_open_internal(const char *path, int stream_index,
                                    int decoder_threads, int use_d3d11,
                                    ihni_input **out, char *err, size_t errlen)
{
    *out = NULL;
    ihni_ffmpeg_init();
    ihni_log_reset();

    ihni_input *in = calloc(1, sizeof(*in));
    if (in == NULL) {
        ihni_set_error(err, errlen, "cannot allocate the input state");
        return IHNI_STATUS_OPEN;
    }
    in->stream_index = -1;
    in->decoder_threads = decoder_threads;
    in->use_d3d11 = use_d3d11;

    int result = avformat_open_input(&in->format, path, NULL, NULL);
    if (result < 0) {
        ihni_error_with_diagnostic(err, errlen, "the input could not be opened");
        free(in);
        return IHNI_STATUS_OPEN;
    }

    result = avformat_find_stream_info(in->format, NULL);
    if (result < 0) {
        ihni_error_with_diagnostic(err, errlen,
                                   "the stream information could not be read");
        ihni_input_close(in);
        return IHNI_STATUS_OPEN;
    }

    if (stream_index >= 0) {
        if ((unsigned)stream_index >= in->format->nb_streams) {
            snprintf(err, errlen,
                     "the input has no stream with absolute index %d",
                     stream_index);
            ihni_input_close(in);
            return IHNI_STATUS_STREAM_NOT_FOUND;
        }
        in->stream_index = stream_index;
    } else {
        for (unsigned i = 0; i < in->format->nb_streams; i++) {
            if (in->format->streams[i]->codecpar->codec_type ==
                AVMEDIA_TYPE_VIDEO) {
                in->stream_index = (int)i;
                break;
            }
        }
        if (in->stream_index < 0) {
            ihni_set_error(err, errlen, "the input has no video stream");
            ihni_input_close(in);
            return IHNI_STATUS_NO_VIDEO;
        }
    }

    AVStream *stream = in->format->streams[in->stream_index];
    if (stream->codecpar->codec_type == AVMEDIA_TYPE_VIDEO) {
        int opened = ihni_open_decoder(in, err, errlen);
        if (opened != 0) {
            ihni_input_close(in);
            return opened;
        }
    }

    in->packet = av_packet_alloc();
    in->frame = av_frame_alloc();
    if (in->packet == NULL || in->frame == NULL) {
        ihni_set_error(err, errlen, "cannot allocate the packet or frame");
        ihni_input_close(in);
        return IHNI_STATUS_OPEN;
    }

    *out = in;
    return 0;
}

void ihni_input_stream(const ihni_input *in, ihni_stream_info *out)
{
    const AVStream *stream = in->format->streams[in->stream_index];
    const AVCodecParameters *parameters = stream->codecpar;

    memset(out, 0, sizeof(*out));
    out->stream_index = in->stream_index;
    out->codec_type = (int)parameters->codec_type;
    out->width = parameters->width;
    out->height = parameters->height;
    out->pixel_format = (int)parameters->format;
    out->bits_per_raw_sample = parameters->bits_per_raw_sample;
    out->frame_rate_num = stream->r_frame_rate.num;
    out->frame_rate_den = stream->r_frame_rate.den;
    out->average_frame_rate_num = stream->avg_frame_rate.num;
    out->average_frame_rate_den = stream->avg_frame_rate.den;
    out->time_base_num = stream->time_base.num;
    out->time_base_den = stream->time_base.den;
    if (stream->start_time != AV_NOPTS_VALUE) {
        out->has_start_time = 1;
        out->start_time_seconds =
            (double)stream->start_time * av_q2d(stream->time_base);
    }
}

/*
 * Produces the next frame in display order.
 *
 * Returns 1 with a view into the decoder's frame, 0 at the end of the stream,
 * or a negative status. The view's pointers are invalidated by the next call.
 *
 * Decode problems are recorded as diagnostics and decoding continues, which is
 * what the FFmpeg command-line tool does. The caller reports the run as
 * incomplete at the end, so a damaged input never looks like a clean stream.
 */
int ihni_input_next_frame(ihni_input *in, ihni_frame_view *out, char *err,
                          size_t errlen)
{
    if (in->codec == NULL) {
        ihni_set_error(err, errlen,
                       "the selected stream is not decodable as video");
        return -1;
    }

    av_frame_unref(in->frame);

    for (;;) {
        int result = avcodec_receive_frame(in->codec, in->frame);
        if (result == 0) {
            const AVFrame *frame = in->frame;
            out->width = frame->width;
            out->height = frame->height;
            out->pixel_format = (int)frame->format;
            out->has_pts = frame->pts != AV_NOPTS_VALUE;
            out->pts = out->has_pts ? frame->pts : 0;
            if (frame->format == AV_PIX_FMT_D3D11) {
                out->is_d3d11 = 1;
                out->texture = frame->data[0];
                out->subresource = (int)(intptr_t)frame->data[1];
                out->stride = 0;
                out->data = NULL;
                /*
                 * The logical luma depth of the hardware frame: the decoder's
                 * software format names it (NV12 is 8-bit, P010 is 10-bit),
                 * which is what the filter parameters and the format-change
                 * checks are built from.
                 */
                out->luma_pixel_format = ihni_luma_format(in->codec->sw_pix_fmt);
            } else {
                out->is_d3d11 = 0;
                out->texture = NULL;
                out->subresource = 0;
                out->luma_pixel_format = ihni_luma_format((enum AVPixelFormat)frame->format);
                out->stride = frame->linesize[0];
                out->data = frame->data[0];
            }
            return 1;
        }
        if (result == AVERROR_EOF) {
            return 0;
        }
        if (result != AVERROR(EAGAIN)) {
            ihni_log_error_code("decoding a frame", result);
            if (in->demux_eof) {
                return 0;
            }
        } else if (in->demux_eof) {
            return 0;
        }

        if (in->have_packet) {
            result = avcodec_send_packet(in->codec, in->packet);
            if (result == AVERROR(EAGAIN)) {
                continue;
            }
            av_packet_unref(in->packet);
            in->have_packet = 0;
            if (result < 0 && result != AVERROR_EOF) {
                ihni_log_error_code("sending a packet to the decoder", result);
            }
            continue;
        }

        for (;;) {
            result = av_read_frame(in->format, in->packet);
            if (result == AVERROR_EOF) {
                if (!in->demux_eof) {
                    in->demux_eof = 1;
                    int flushed = avcodec_send_packet(in->codec, NULL);
                    if (flushed < 0 && flushed != AVERROR_EOF &&
                        flushed != AVERROR(EAGAIN)) {
                        ihni_log_error_code("flushing the decoder", flushed);
                    }
                }
                break;
            }
            if (result < 0) {
                ihni_log_error_code("reading a packet", result);
                if (!in->demux_eof) {
                    in->demux_eof = 1;
                    avcodec_send_packet(in->codec, NULL);
                }
                break;
            }
            if (in->packet->stream_index == in->stream_index) {
                in->have_packet = 1;
                break;
            }
            av_packet_unref(in->packet);
        }
    }
}

static void ihni_release_decoder(ihni_input *in)
{
    if (in->codec != NULL) {
        avcodec_free_context(&in->codec);
    }
    in->decoder = NULL;
}

/*
 * Releases the D3D11 filter objects. The context must be flushed and nothing
 * may hold the staging texture mapped at this point.
 */
static void ihni_release_d3d11(ihni_input *in)
{
    if (in->d3d11_srv != NULL) {
        in->d3d11_srv->lpVtbl->Release(in->d3d11_srv);
        in->d3d11_srv = NULL;
    }
    if (in->d3d11_frame_texture != NULL) {
        in->d3d11_frame_texture->lpVtbl->Release(in->d3d11_frame_texture);
        in->d3d11_frame_texture = NULL;
    }
    if (in->d3d11_staging != NULL) {
        in->d3d11_staging->lpVtbl->Release(in->d3d11_staging);
        in->d3d11_staging = NULL;
    }
    if (in->d3d11_uav != NULL) {
        in->d3d11_uav->lpVtbl->Release(in->d3d11_uav);
        in->d3d11_uav = NULL;
    }
    if (in->d3d11_output != NULL) {
        in->d3d11_output->lpVtbl->Release(in->d3d11_output);
        in->d3d11_output = NULL;
    }
    if (in->d3d11_shader != NULL) {
        in->d3d11_shader->lpVtbl->Release(in->d3d11_shader);
        in->d3d11_shader = NULL;
    }
    if (in->d3d11_context != NULL) {
        in->d3d11_context->lpVtbl->Release(in->d3d11_context);
        in->d3d11_context = NULL;
    }
    if (in->d3d11_device != NULL) {
        in->d3d11_device->lpVtbl->Release(in->d3d11_device);
        in->d3d11_device = NULL;
    }
    if (in->hw_device != NULL) {
        av_buffer_unref(&in->hw_device);
    }
}

void ihni_input_close(ihni_input *in)
{
    if (in == NULL) {
        return;
    }
    ihni_release_d3d11(in);
    av_frame_free(&in->frame);
    av_packet_free(&in->packet);
    ihni_release_decoder(in);
    if (in->format != NULL) {
        avformat_close_input(&in->format);
    }
    free(in);
}

/* ------------------------------------------------------------------------- */
/* D3D11 analysis filter                                                     */
/* ------------------------------------------------------------------------- */

/*
 * The GPU implementation of the whole-pixel box filter. The constants are
 * baked into the source at filter start because one run has one fixed
 * geometry; the arithmetic mirrors the CPU fast path exactly (`sum + half)
 * / block`, then the documented output shift), so the two implementations
 * produce byte-identical planes by construction and by test.
 *
 * The input is the decoder's NV12 or P010 luma plane bound as an unsigned
 * integer view; the output is one `R8_UINT` texel per analysis sample.
 */
static const char IHNI_D3D11_SHADER[] =
    "Texture2D<uint> luma : register(t0);\n"
    "RWTexture2D<uint> output_plane : register(u0);\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 tid : SV_DispatchThreadID)\n"
    "{\n"
    "    uint x = tid.x;\n"
    "    uint y = tid.y;\n"
    "    if (x >= DST_WIDTH || y >= DST_HEIGHT) {\n"
    "        return;\n"
    "    }\n"
    "    uint sum = 0;\n"
    "    for (uint k = 0; k < BLOCK_H; k++) {\n"
    "        for (uint m = 0; m < BLOCK_W; m++) {\n"
    "            uint raw_sample = luma.Load(int3(x * BLOCK_W + m, y * BLOCK_H + k, 0));\n"
    "            sum += raw_sample >> SAMPLE_SHIFT;\n"
    "        }\n"
    "    }\n"
    "    uint mean = (sum + HALF) / BLOCK;\n"
    "    output_plane[int2(x, y)] = mean >> OUT_SHIFT;\n"
    "}\n";

/*
 * Compiles the shader for one fixed geometry. `params` carries the integers
 * the source references by name.
 */
/*
 * Replaces every whole-token occurrence of `name` in `buffer` with `value`.
 * A whole token is delimited by characters outside [A-Za-z0-9_], so BLOCK
 * never matches inside BLOCK_W. The buffer must be large enough for the
 * replacement; overflow is a hard error.
 */
static int ihni_shader_replace(char *buffer, size_t capacity, const char *name,
                               const char *value)
{
    const size_t name_len = strlen(name);
    const size_t value_len = strlen(value);
    char *cursor = buffer;
    while ((cursor = strstr(cursor, name)) != NULL) {
        int before_ok = cursor == buffer || !(cursor[-1] == '_' ||
            (cursor[-1] >= 'A' && cursor[-1] <= 'Z') ||
            (cursor[-1] >= 'a' && cursor[-1] <= 'z') ||
            (cursor[-1] >= '0' && cursor[-1] <= '9'));
        char after = cursor[name_len];
        int after_ok = !(after == '_' ||
            (after >= 'A' && after <= 'Z') ||
            (after >= 'a' && after <= 'z') ||
            (after >= '0' && after <= '9'));
        if (!before_ok || !after_ok) {
            cursor += name_len;
            continue;
        }
        if (strlen(cursor) - name_len + value_len + 1 > (size_t)(buffer + capacity - cursor)) {
            return -1;
        }
        memmove(cursor + value_len, cursor + name_len, strlen(cursor + name_len) + 1);
        memcpy(cursor, value, value_len);
        cursor += value_len;
    }
    return 0;
}

static int ihni_d3d11_build(ihni_input *in, const ihni_frame_view *view,
                            const ihni_d3d11_params *params,
                            char *err, size_t errlen)
{
    /*
     * The decoder's own textures are decoder-bound only, so the filter owns
     * one NV12 or P010 copy texture with a shader-resource binding. Each
     * frame is copied into it on the device first; a GPU-to-GPU copy is far
     * cheaper than a host round trip. One slot suffices because the staging
     * read-back at the end of every filter call synchronizes the context.
     */
    D3D11_TEXTURE2D_DESC source_desc;
    ((ID3D11Texture2D *)view->texture)->lpVtbl->GetDesc(
        (ID3D11Texture2D *)view->texture, &source_desc);
    D3D11_TEXTURE2D_DESC copy_desc = source_desc;
    copy_desc.MipLevels = 1;
    copy_desc.ArraySize = 1;
    copy_desc.SampleDesc.Count = 1;
    copy_desc.SampleDesc.Quality = 0;
    copy_desc.Usage = D3D11_USAGE_DEFAULT;
    copy_desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    copy_desc.CPUAccessFlags = 0;
    copy_desc.MiscFlags = 0;
    HRESULT status = in->d3d11_device->lpVtbl->CreateTexture2D(
        in->d3d11_device, &copy_desc, NULL, &in->d3d11_frame_texture);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot create the filter's frame texture");
        return -1;
    }

    /*
     * The luma plane of the copy texture, bound as unsigned integers so the
     * shader's `Load` returns raw sample bits. NV12 planes view as R8, P010
     * planes as R16.
     */
    D3D11_SHADER_RESOURCE_VIEW_DESC1 srv_desc = {
        .Format = source_desc.Format == DXGI_FORMAT_P010 ? DXGI_FORMAT_R16_UINT
                                                         : DXGI_FORMAT_R8_UINT,
        .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
        .Texture2D = { .MostDetailedMip = 0, .MipLevels = 1, .PlaneSlice = 0 },
    };
    status = in->d3d11_device->lpVtbl->CreateShaderResourceView1(
        in->d3d11_device, (ID3D11Resource *)in->d3d11_frame_texture, &srv_desc,
        &in->d3d11_srv);
    if (FAILED(status)) {
        snprintf(err, errlen, "cannot bind the filter's frame texture (hr=0x%08lX)",
                 (unsigned long)status);
        return -1;
    }

    char source[sizeof(IHNI_D3D11_SHADER) + 128];
    strcpy(source, IHNI_D3D11_SHADER);

    /*
     * Replace the placeholder names with the run's integers. The replacement
     * is whole-token, so the order of the calls does not matter.
     */
    char number[16];
    struct { const char *name; int value; } constants[] = {
        { "DST_WIDTH", params->dst_width },
        { "DST_HEIGHT", params->dst_height },
        { "BLOCK_W", params->block_w },
        { "BLOCK_H", params->block_h },
        { "HALF", params->half },
        { "BLOCK", params->block },
        { "SAMPLE_SHIFT", params->sample_shift },
        { "OUT_SHIFT", params->out_shift },
    };
    for (size_t i = 0; i < sizeof(constants) / sizeof(constants[0]); i++) {
        snprintf(number, sizeof(number), "%d", constants[i].value);
        if (ihni_shader_replace(source, sizeof(source), constants[i].name, number) != 0) {
            ihni_set_error(err, errlen, "the shader source overflowed");
            return -1;
        }
    }
    fprintf(stderr, "%s\n", source);

    ID3DBlob *code = NULL;
    ID3DBlob *diagnostics = NULL;
    status = D3DCompile(source, strlen(source), NULL, NULL, NULL, "main",
                                "cs_5_0", 0, 0, &code, &diagnostics);
    if (FAILED(status)) {
        const char *detail = diagnostics != NULL
                                 ? (const char *)diagnostics->lpVtbl->GetBufferPointer(diagnostics)
                                 : NULL;
        if (detail != NULL && detail[0] != '\0') {
            snprintf(err, errlen, "compiling the D3D11 filter shader failed: %.256s",
                     detail);
        } else {
            ihni_set_error(err, errlen, "compiling the D3D11 filter shader failed");
        }
        if (diagnostics != NULL) {
            diagnostics->lpVtbl->Release(diagnostics);
        }
        return -1;
    }
    if (diagnostics != NULL) {
        diagnostics->lpVtbl->Release(diagnostics);
    }

    status = in->d3d11_device->lpVtbl->CreateComputeShader(        in->d3d11_device, code->lpVtbl->GetBufferPointer(code),
        code->lpVtbl->GetBufferSize(code), NULL, &in->d3d11_shader);
    code->lpVtbl->Release(code);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot create the D3D11 compute shader");
        return -1;
    }

    /*
     * The output is one unsigned byte per analysis sample, written through a
     * UAV and read back through a staging twin. Both live for the whole run.
     */
    D3D11_TEXTURE2D_DESC output_desc = {
        .Width = (UINT)params->dst_width,
        .Height = (UINT)params->dst_height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = DXGI_FORMAT_R8_UINT,
        .SampleDesc = { .Count = 1, .Quality = 0 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
        .CPUAccessFlags = 0,
        .MiscFlags = 0,
    };
    status = in->d3d11_device->lpVtbl->CreateTexture2D(in->d3d11_device, &output_desc,
                                                       NULL, &in->d3d11_output);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot create the D3D11 output texture");
        return -1;
    }
    D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc = {
        .Format = DXGI_FORMAT_R8_UINT,
        .ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D,
        .Texture2D = { .MipSlice = 0 },
    };
    status = in->d3d11_device->lpVtbl->CreateUnorderedAccessView(
        in->d3d11_device, (ID3D11Resource *)in->d3d11_output, &uav_desc,
        &in->d3d11_uav);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot create the D3D11 output view");
        return -1;
    }
    D3D11_TEXTURE2D_DESC staging_desc = output_desc;
    staging_desc.Usage = D3D11_USAGE_STAGING;
    staging_desc.BindFlags = 0;
    staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    status = in->d3d11_device->lpVtbl->CreateTexture2D(in->d3d11_device, &staging_desc,
                                                       NULL, &in->d3d11_staging);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot create the D3D11 staging texture");
        return -1;
    }

    in->d3d11_params = *params;
    return 0;
}

int ihni_input_d3d11_filter(ihni_input *in, const ihni_frame_view *view,
                            const ihni_d3d11_params *params, uint8_t *dst,
                            char *err, size_t errlen)
{
    if (!in->use_d3d11 || in->d3d11_device == NULL) {
        ihni_set_error(err, errlen, "the input is not a D3D11 hardware run");
        return -1;
    }
    if (in->d3d11_shader == NULL) {
        if (ihni_d3d11_build(in, view, params, err, errlen) != 0) {
            return -1;
        }
    }

    /*
     * Copy the decoded frame's luma into the filter's own texture, then run
     * the shader over it. The copy source is the decoder texture's array
     * element for this frame; the destination is the single element the
     * filter owns.
     */
    in->d3d11_context->lpVtbl->CopySubresourceRegion(
        in->d3d11_context, (ID3D11Resource *)in->d3d11_frame_texture, 0, 0, 0, 0,
        (ID3D11Resource *)view->texture, (UINT)view->subresource, NULL);

    ID3D11ShaderResourceView *srv = in->d3d11_srv;
    ID3D11UnorderedAccessView *uav = in->d3d11_uav;
    in->d3d11_context->lpVtbl->CSSetShader(in->d3d11_context, in->d3d11_shader,
                                           NULL, 0);
    in->d3d11_context->lpVtbl->CSSetShaderResources(in->d3d11_context, 0, 1, &srv);
    in->d3d11_context->lpVtbl->CSSetUnorderedAccessViews(
        in->d3d11_context, 0, 1, &uav, NULL);
    UINT groups_x = (UINT)((params->dst_width + 15) / 16);
    UINT groups_y = (UINT)((params->dst_height + 15) / 16);
    in->d3d11_context->lpVtbl->Dispatch(in->d3d11_context, groups_x, groups_y, 1);
    ID3D11ShaderResourceView *released_srv = NULL;
    in->d3d11_context->lpVtbl->CSSetShaderResources(in->d3d11_context, 0, 1,
                                                    &released_srv);
    ID3D11UnorderedAccessView *released_uav = NULL;
    in->d3d11_context->lpVtbl->CSSetUnorderedAccessViews(in->d3d11_context, 0, 1,
                                                         &released_uav, NULL);
    in->d3d11_context->lpVtbl->CSSetShader(in->d3d11_context, NULL, NULL, 0);

    in->d3d11_context->lpVtbl->CopyResource(in->d3d11_context,
                                            (ID3D11Resource *)in->d3d11_staging,
                                            (ID3D11Resource *)in->d3d11_output);
    D3D11_MAPPED_SUBRESOURCE mapped;
    HRESULT status = in->d3d11_context->lpVtbl->Map(in->d3d11_context,
                                            (ID3D11Resource *)in->d3d11_staging, 0,
                                            D3D11_MAP_READ, 0, &mapped);
    if (FAILED(status)) {
        ihni_set_error(err, errlen, "cannot read the filtered plane back");
        return -1;
    }
    const uint8_t *source_row = (const uint8_t *)mapped.pData;
    for (int y = 0; y < params->dst_height; y++) {
        memcpy(dst + (size_t)y * params->dst_width,
               source_row + (size_t)y * mapped.RowPitch, (size_t)params->dst_width);
    }
    in->d3d11_context->lpVtbl->Unmap(in->d3d11_context,
                                     (ID3D11Resource *)in->d3d11_staging, 0);
    return 0;
}
