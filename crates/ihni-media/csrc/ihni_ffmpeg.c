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
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>

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
} ihni_frame_view;

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
            out->luma_pixel_format = ihni_luma_format((enum AVPixelFormat)frame->format);
            out->stride = frame->linesize[0];
            out->data = frame->data[0];
            out->has_pts = frame->pts != AV_NOPTS_VALUE;
            out->pts = out->has_pts ? frame->pts : 0;
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

void ihni_input_close(ihni_input *in)
{
    if (in == NULL) {
        return;
    }
    av_frame_free(&in->frame);
    av_packet_free(&in->packet);
    ihni_release_decoder(in);
    if (in->format != NULL) {
        avformat_close_input(&in->format);
    }
    free(in);
}
