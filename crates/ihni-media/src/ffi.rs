//! Hand-written declarations for the C shim in `csrc/ihni_ffmpeg.c`.
//!
//! This module is the only Rust code that names a libav entry point, and it
//! names no libav type: the C side converts every result into the flat structs
//! below. Keeping the struct accesses in C means Rust cannot be broken by a
//! layout change between FFmpeg releases, and it is why the build needs no
//! libclang.
//!
//! # Lifetimes and threads
//!
//! An [`Input`] owns one open file, one selected stream, and one decoder. It is
//! created on the thread that opens the reader and then moved to the decode
//! producer thread, where it is the only caller; [`Input`] is therefore `Send`
//! but neither `Sync` nor `Clone`.
//!
//! A decoded frame is a view into the decoder's current frame. [`FrameRef`]
//! borrows the input, so the borrow checker rejects a second decode while a
//! view is alive. The view's slice is only valid until that borrow ends.

use std::ffi::{CStr, CString, c_char, c_int};
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::Once;

use crate::error::MediaError;
use crate::luma::LumaFormat;

/// `ihni_input_open` status codes, matching the `IHNI_STATUS_*` macros.
const STATUS_STREAM_NOT_FOUND: c_int = 2;
const STATUS_NO_VIDEO: c_int = 3;
const STATUS_CODEC: c_int = 4;

/// The luma format a shim code names, or `None` for the unsupported marker.
fn luma_format_from_code(code: c_int) -> Option<LumaFormat> {
    match code {
        0 => Some(LumaFormat::Gray8),
        1 => Some(LumaFormat::Gray10Le),
        2 => Some(LumaFormat::Gray10Be),
        _ => None,
    }
}

/// Largest diagnostic or error text the shim can produce, plus the terminating
/// NUL. The shim's own diagnostic buffer is 8192 bytes.
const TEXT_BUFFER: usize = 8193;

/// Opaque input handle owned by `ihni_input_open`.
#[repr(C)]
pub(crate) struct IhniInput {
    _private: [u8; 0],
}

/// Flat description of the selected stream.
///
/// Field order and types must match `ihni_stream_info` in the C shim.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamInfo {
    pub(crate) stream_index: c_int,
    pub(crate) codec_type: c_int,
    pub(crate) width: c_int,
    pub(crate) height: c_int,
    pub(crate) pixel_format: c_int,
    pub(crate) bits_per_raw_sample: c_int,
    pub(crate) frame_rate_num: c_int,
    pub(crate) frame_rate_den: c_int,
    pub(crate) average_frame_rate_num: c_int,
    pub(crate) average_frame_rate_den: c_int,
    pub(crate) time_base_num: c_int,
    pub(crate) time_base_den: c_int,
    pub(crate) has_start_time: c_int,
    pub(crate) start_time_seconds: f64,
}

impl StreamInfo {
    /// Source width in pixels.
    pub(crate) fn width(&self) -> u32 {
        u32::try_from(self.width).unwrap_or(0)
    }

    /// Source height in pixels.
    pub(crate) fn height(&self) -> u32 {
        u32::try_from(self.height).unwrap_or(0)
    }

    /// The source pixel format name, as FFprobe spells it.
    ///
    /// `None` when libav does not know the value, which means the container
    /// states a format this build cannot name; that is not the same as an
    /// 8-bit format.
    pub(crate) fn pixel_format_name(&self) -> Option<String> {
        pixel_format_name(self.pixel_format)
    }

    /// The depth the container reports, or `None` when it reports none.
    ///
    /// A reported depth of zero means the field is absent, not that the source
    /// is zero bits deep.
    pub(crate) fn bits_per_raw_sample(&self) -> Option<u32> {
        if self.bits_per_raw_sample <= 0 {
            None
        } else {
            Some(self.bits_per_raw_sample as u32)
        }
    }

    /// Whether the selected stream is a video stream.
    pub(crate) fn is_video(&self) -> bool {
        media_type_is_video(self.codec_type)
    }

    /// The codec type name, as FFprobe spells it, for an error message.
    pub(crate) fn codec_type_name(&self) -> String {
        media_type_name(self.codec_type)
    }

    /// The reported frame rate, or `None` when it is absent or unusable.
    pub(crate) fn frame_rate(&self) -> Option<(u32, u32)> {
        usable_rate(self.frame_rate_num, self.frame_rate_den)
    }

    /// The reported average frame rate, or `None` when it is absent or unusable.
    pub(crate) fn average_frame_rate(&self) -> Option<(u32, u32)> {
        usable_rate(self.average_frame_rate_num, self.average_frame_rate_den)
    }

    /// The stream time base as a `NUM/DEN` pair.
    pub(crate) fn time_base(&self) -> (i32, i32) {
        (self.time_base_num, self.time_base_den)
    }

    /// The stream start time in seconds, when the container reports one.
    pub(crate) fn start_time_seconds(&self) -> Option<f64> {
        (self.has_start_time != 0).then_some(self.start_time_seconds)
    }
}

/// Drops a rational whose numerator or denominator is zero or negative.
///
/// FFmpeg reports `0/0` for a stream without a usable rate. That is unknown, not
/// a rate of zero.
fn usable_rate(numerator: c_int, denominator: c_int) -> Option<(u32, u32)> {
    if numerator <= 0 || denominator <= 0 {
        return None;
    }
    Some((numerator as u32, denominator as u32))
}

/// Flat description of one decoded frame.
///
/// `data` points into the decoder's frame and is invalidated by the next call on
/// the same input. Field order and types must match `ihni_frame_view`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameView {
    pub(crate) width: c_int,
    pub(crate) height: c_int,
    pub(crate) pixel_format: c_int,
    pub(crate) luma_pixel_format: c_int,
    pub(crate) stride: c_int,
    pub(crate) data: *const u8,
    pub(crate) has_pts: c_int,
    pub(crate) pts: i64,
}

// SAFETY: every declaration names a function the shim in `csrc/ihni_ffmpeg.c`
// defines with exactly this signature. The shim is the project's own C, compiled
// into this crate by `build.rs`; nothing else supplies these symbols.
unsafe extern "C" {
    fn ihni_ffmpeg_init();

    fn ihni_av_version() -> *const c_char;

    fn ihni_pix_fmt_name(format: c_int) -> *const c_char;

    fn ihni_media_type_name(codec_type: c_int) -> *const c_char;

    fn ihni_media_type_is_video(codec_type: c_int) -> c_int;

    fn ihni_luma_format_supported(luma_format: c_int) -> c_int;

    fn ihni_log_take(buffer: *mut c_char, length: usize) -> c_int;

    fn ihni_input_open(
        path: *const c_char,
        stream_index: c_int,
        decoder_threads: c_int,
        out: *mut *mut IhniInput,
        err: *mut c_char,
        errlen: usize,
    ) -> c_int;

    fn ihni_input_stream(input: *const IhniInput, out: *mut StreamInfo);

    fn ihni_input_next_frame(
        input: *mut IhniInput,
        out: *mut FrameView,
        err: *mut c_char,
        errlen: usize,
    ) -> c_int;

    fn ihni_input_close(input: *mut IhniInput);
}

static INIT: Once = Once::new();

/// Installs the process-wide libav log callback.
///
/// `Once` guarantees the C shim's lock exists before any decoder worker thread
/// can reach the callback.
fn init() {
    INIT.call_once(|| {
        // SAFETY: the shim initialises its lock and installs its callback; it
        // takes no arguments and touches no caller memory. `Once` makes this the
        // only call in the process.
        unsafe { ihni_ffmpeg_init() };
    });
}

/// Version string of the linked libav libraries.
///
/// Used by this module's test to prove the static libraries linked and are
/// callable, and recorded by `docs/Architecture.md`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn library_version() -> String {
    init();
    // SAFETY: the shim returns a pointer to a static NUL-terminated string that
    // libav owns for the lifetime of the process.
    let pointer = unsafe { ihni_av_version() };
    static_name(pointer)
}

/// Reads a NUL-terminated string libav owns for the process lifetime.
fn static_name(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: the shim only returns pointers to static NUL-terminated strings
    // owned by libav, which stay valid for the lifetime of the process.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    String::from_utf8_lossy(bytes).into_owned()
}

/// Names one libav pixel format, or `None` when libav does not know the value.
fn pixel_format_name(format: c_int) -> Option<String> {
    // SAFETY: any integer is a valid argument; the shim returns an empty string
    // for a value it does not know.
    let name = static_name(unsafe { ihni_pix_fmt_name(format) });
    (!name.is_empty()).then_some(name)
}

/// Names one libav media (codec) type.
fn media_type_name(codec_type: c_int) -> String {
    // SAFETY: any integer is a valid argument; the shim returns "unknown" for a
    // value it does not know.
    let name = static_name(unsafe { ihni_media_type_name(codec_type) });
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name
    }
}

/// Reports whether a libav media type is video.
fn media_type_is_video(codec_type: c_int) -> bool {
    // SAFETY: any integer is a valid argument.
    unsafe { ihni_media_type_is_video(codec_type) != 0 }
}

/// Reports whether a luma format can be scaled by this crate.
fn luma_format_supported(luma_format: c_int) -> bool {
    // SAFETY: any integer is a valid argument.
    unsafe { ihni_luma_format_supported(luma_format) != 0 }
}

/// Takes and clears the diagnostics accumulated by the shim.
///
/// The text covers errors reported by libav itself: a damaged or truncated
/// container is reported here rather than through a return code, and the caller
/// turns any non-empty result into incomplete processing.
pub(crate) fn take_diagnostic() -> String {
    init();
    let mut buffer = vec![0u8; TEXT_BUFFER];
    // SAFETY: `buffer` is writable for `TEXT_BUFFER` bytes and the shim writes at
    // most that many bytes including the terminating NUL.
    let written = unsafe { ihni_log_take(buffer.as_mut_ptr().cast::<c_char>(), TEXT_BUFFER) };
    if written <= 0 {
        return String::new();
    }
    let length = usize::try_from(written)
        .unwrap_or(0)
        .min(buffer.len().saturating_sub(1));
    String::from_utf8_lossy(&buffer[..length]).trim().to_owned()
}

/// Reads a NUL-terminated error buffer filled by the shim.
fn error_text(buffer: &[u8]) -> String {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).trim().to_owned()
}

/// An open input and its selected stream.
pub(crate) struct Input {
    raw: NonNull<IhniInput>,
    path: PathBuf,
}

// SAFETY: the handle owns decoder state that is not shared. It is created on the
// opening thread and then moved to the decode producer thread, where it is the
// only user. No reference to it is sent across a thread boundary.
unsafe impl Send for Input {}

impl Input {
    /// Opens `path` and selects a stream.
    ///
    /// `stream_index` selects an absolute index; `None` selects the first video
    /// stream. `decoder_threads` of `None` lets libav choose. A selected stream
    /// that is not a video stream is opened without a decoder, so the caller
    /// reports it as a stream selection error rather than a decode failure.
    pub(crate) fn open(
        path: &Path,
        stream_index: Option<u32>,
        decoder_threads: Option<u32>,
    ) -> Result<Self, MediaError> {
        init();

        let index = match stream_index {
            None => -1,
            Some(index) => i32::try_from(index).map_err(|_| MediaError::StreamNotFound {
                path: path.to_path_buf(),
                stream_index: index,
            })?,
        };
        let threads = match decoder_threads {
            None => 0,
            Some(threads) => i32::try_from(threads).unwrap_or(i32::MAX),
        };

        let text = path.to_str().ok_or_else(|| MediaError::Decode {
            operation: "convert the input path to UTF-8 for the decoder",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "the input path is not valid UTF-8",
            ),
        })?;
        let c_path = CString::new(text).map_err(|_| MediaError::Decode {
            operation: "pass the input path to the decoder",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "the input path contains a NUL byte",
            ),
        })?;

        let mut raw: *mut IhniInput = ptr::null_mut();
        let mut error = vec![0u8; TEXT_BUFFER];
        // SAFETY: the path is a valid NUL-terminated string, `raw` and `error`
        // are writable for the sizes passed, and the shim writes at most
        // `TEXT_BUFFER` bytes to `error`. On success it sets `raw` to a handle
        // this function then owns.
        let status = unsafe {
            ihni_input_open(
                c_path.as_ptr(),
                index,
                threads,
                &mut raw,
                error.as_mut_ptr().cast::<c_char>(),
                TEXT_BUFFER,
            )
        };

        if status != 0 {
            let reason = error_text(&error);
            return Err(match status {
                STATUS_STREAM_NOT_FOUND => MediaError::StreamNotFound {
                    path: path.to_path_buf(),
                    stream_index: stream_index.unwrap_or(0),
                },
                STATUS_NO_VIDEO => MediaError::NoVideoStream {
                    path: path.to_path_buf(),
                },
                STATUS_CODEC => MediaError::Decode {
                    operation: "open the selected stream's decoder",
                    path: path.to_path_buf(),
                    source: io::Error::other(reason),
                },
                _ => MediaError::ContainerOpen {
                    path: path.to_path_buf(),
                    reason,
                },
            });
        }

        let raw = NonNull::new(raw).ok_or_else(|| MediaError::ContainerOpen {
            path: path.to_path_buf(),
            reason: "the decoder returned no input handle".to_owned(),
        })?;
        Ok(Self {
            raw,
            path: path.to_path_buf(),
        })
    }

    /// Describes the selected stream.
    pub(crate) fn stream(&self) -> StreamInfo {
        // `StreamInfo` is all integers and one f64, so a zeroed value is a valid
        // `repr(C)` struct that the shim overwrites completely.
        let mut info = std::mem::MaybeUninit::<StreamInfo>::zeroed();
        // SAFETY: the handle is live, and `info` is writable for one
        // `StreamInfo`, which the shim fills field by field.
        unsafe { ihni_input_stream(self.raw.as_ptr(), info.as_mut_ptr()) };
        // SAFETY: the shim wrote every field of the struct.
        unsafe { info.assume_init() }
    }

    /// Decodes the next frame in display order.
    ///
    /// `None` is the end of the stream. A `Some` view borrows the input until it
    /// is dropped, and its plane is only valid for that borrow.
    pub(crate) fn next_frame(&mut self) -> Result<Option<FrameRef<'_>>, MediaError> {
        let mut view = std::mem::MaybeUninit::<FrameView>::zeroed();
        let mut error = vec![0u8; TEXT_BUFFER];
        // SAFETY: the handle is live and exclusively borrowed, `view` is writable
        // for one `FrameView`, and `error` is writable for `TEXT_BUFFER` bytes.
        let status = unsafe {
            ihni_input_next_frame(
                self.raw.as_ptr(),
                view.as_mut_ptr(),
                error.as_mut_ptr().cast::<c_char>(),
                TEXT_BUFFER,
            )
        };

        match status {
            1 => {
                // SAFETY: a status of 1 means the shim filled every field, and the
                // frame data is valid until the next call on this handle, which
                // the returned borrow prevents.
                let view = unsafe { view.assume_init() };
                Ok(Some(FrameRef {
                    view,
                    _input: PhantomData,
                }))
            }
            0 => Ok(None),
            _ => Err(MediaError::Decode {
                operation: "decode the next frame",
                path: self.path.clone(),
                source: io::Error::other(error_text(&error)),
            }),
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `ihni_input_open` and is closed exactly once,
        // here, after which no reference to it is used.
        unsafe { ihni_input_close(self.raw.as_ptr()) };
    }
}

/// A decoded frame borrowed from an [`Input`].
pub(crate) struct FrameRef<'a> {
    view: FrameView,
    _input: PhantomData<&'a mut Input>,
}

impl FrameRef<'_> {
    pub(crate) fn width(&self) -> u32 {
        u32::try_from(self.view.width).unwrap_or(0)
    }

    pub(crate) fn height(&self) -> u32 {
        u32::try_from(self.view.height).unwrap_or(0)
    }

    /// Bytes between the start of consecutive rows.
    pub(crate) fn stride(&self) -> usize {
        usize::try_from(self.view.stride).unwrap_or(0)
    }

    /// The luma plane, `stride * height` bytes.
    pub(crate) fn plane(&self) -> &[u8] {
        let length = self.stride().saturating_mul(self.height() as usize);
        if length == 0 || self.view.data.is_null() {
            return &[];
        }
        // SAFETY: libav allocates the luma plane with at least `linesize[0]`
        // bytes per row for `height` rows, and the borrow of the input keeps the
        // frame alive. The shim reports `stride` as `linesize[0]` and `height` as
        // the frame height, so `stride * height` is inside the allocation.
        unsafe { slice::from_raw_parts(self.view.data, length) }
    }

    /// The presentation time stamp in the stream time base, when there is one.
    pub(crate) fn pts(&self) -> Option<i64> {
        (self.view.has_pts != 0).then_some(self.view.pts)
    }

    /// The pixel format of the decoded frame, for an error message.
    pub(crate) fn pixel_format_name(&self) -> String {
        pixel_format_name(self.view.pixel_format).unwrap_or_else(|| "unknown".to_owned())
    }

    /// The scaler source format for this frame, when the crate supports it.
    ///
    /// `None` means the frame's depth is outside the supported set, so it is
    /// rejected with the frame's own pixel format name.
    pub(crate) fn luma_format(&self) -> Option<LumaFormat> {
        let code = self.view.luma_pixel_format;
        luma_format_supported(code).then(|| luma_format_from_code(code))?
    }
}

#[cfg(test)]
mod tests {
    use super::{error_text, library_version};

    #[test]
    fn the_linked_libraries_are_callable() {
        // This is the link gate: if the static libav archives were not found, or
        // were linked against an incompatible C runtime, the process would fail
        // to start rather than reach this assertion.
        let version = library_version();
        assert!(
            version.starts_with("9.") || version.starts_with("10."),
            "unexpected libav version reported: '{version}'"
        );
    }

    #[test]
    fn an_error_buffer_stops_at_its_nul() {
        assert_eq!(
            error_text(b"failed: bad data\0trailing"),
            "failed: bad data"
        );
    }

    #[test]
    fn an_unterminated_error_buffer_is_read_to_the_end() {
        assert_eq!(error_text(b"no terminator"), "no terminator");
    }
}
