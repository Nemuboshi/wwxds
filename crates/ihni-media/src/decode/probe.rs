//! Stream selection and source metadata.
//!
//! This stage selects one absolute stream index and records what the container
//! states: the codec type, the source size, the pixel format, the raw sample
//! depth, the reported rates, and the stream start time.
//!
//! The metadata comes from the same in-process decoder the reader uses, so a
//! selection that this stage accepts is one the reader can decode.

use std::path::Path;

use crate::error::MediaError;
use crate::ffi;

use super::analysis::{AnalysisSize, AnalysisSizeError};
use super::time::FrameRate;

/// The luma sample depth of a supported source.
///
/// The decoder writes one 8-bit analysis plane for both depths. FFmpeg's `gray`
/// output reduces a deeper source to 8 bits by reducing the numeric sample
/// value, so a 10-bit sample of 880 becomes 220, the same limited-range level
/// as an 8-bit source sample of 220. An 8-bit source passes through unchanged.
/// No shift is applied by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LumaDepth {
    /// 8-bit source samples, for example `yuv420p`.
    Eight,
    /// 10-bit source samples, for example `yuv420p10le`.
    Ten,
}

/// Metadata for the selected video stream.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    /// Absolute input stream index, as requested by the caller.
    pub stream_index: u32,
    /// Source width in pixels.
    pub width: u32,
    /// Source height in pixels.
    pub height: u32,
    /// Source pixel format name.
    pub pixel_format: String,
    /// Raw luma sample depth.
    pub depth: LumaDepth,
    /// Frame rate of the stream, when the container reports a usable value.
    pub frame_rate: Option<FrameRate>,
    /// Average frame rate of the stream, when the container reports one.
    pub average_frame_rate: Option<FrameRate>,
    /// Stream start time in seconds, when the container reports one.
    pub start_time_seconds: Option<f64>,
}

impl SourceInfo {
    /// Derives the fixed analysis size for this source at the default height.
    ///
    /// The caller attaches the input path to any error.
    pub fn analysis_size(&self) -> Result<AnalysisSize, AnalysisSizeError> {
        AnalysisSize::for_source(self.width, self.height)
    }

    /// Derives the analysis size for this source at a chosen height.
    ///
    /// `height` is `None` for the default [`super::analysis::ANALYSIS_HEIGHT`].
    pub fn analysis_size_with_height(
        &self,
        height: Option<u32>,
    ) -> Result<AnalysisSize, AnalysisSizeError> {
        match height {
            Some(height) => AnalysisSize::for_source_height(self.width, self.height, height),
            None => self.analysis_size(),
        }
    }
}

/// Selects one video stream and returns its source metadata.
///
/// `stream_index` is an absolute input stream index. When it is `None`, the
/// first video stream is selected. A selected stream that is not video is an
/// error, not a fallback to another stream.
///
/// This opens the input and closes it again. Use [`ReaderOptions`]-based
/// [`super::reader::FrameReader::open`] for a run, which opens once and keeps
/// the decoder.
///
/// [`ReaderOptions`]: super::reader::ReaderOptions
pub fn probe(path: &Path, stream_index: Option<u32>) -> Result<SourceInfo, MediaError> {
    let input = ffi::Input::open(path, stream_index, None)?;
    describe(&input, path)
}

/// Records the source metadata of an already-open input.
///
/// The stream selection itself happened in [`ffi::Input::open`], which reports
/// an absent index and an absent video stream. This stage reports a selected
/// stream that is not video, an unusable size, and an unsupported depth.
pub(crate) fn describe(input: &ffi::Input, path: &Path) -> Result<SourceInfo, MediaError> {
    let stream = input.stream();
    let selected = u32::try_from(stream.stream_index).unwrap_or(0);

    if !stream.is_video() {
        return Err(MediaError::NotVideoStream {
            path: path.to_path_buf(),
            stream_index: selected,
            codec_type: stream.codec_type_name(),
        });
    }

    let width = stream.width();
    let height = stream.height();
    if width == 0 || height == 0 {
        return Err(MediaError::SourceDimensions {
            path: path.to_path_buf(),
            width,
            height,
        });
    }

    // A format this build cannot name is rejected before the depth is resolved:
    // `LumaDepth::resolve` falls back to the name, and an unnamed format must
    // not be read as an 8-bit one.
    let Some(pixel_format) = stream.pixel_format_name() else {
        return Err(MediaError::UnsupportedPixelFormat {
            path: path.to_path_buf(),
            pixel_format: "unknown".to_owned(),
        });
    };
    let depth =
        LumaDepth::resolve(&pixel_format, stream.bits_per_raw_sample()).ok_or_else(|| {
            MediaError::UnsupportedPixelFormat {
                path: path.to_path_buf(),
                pixel_format: pixel_format.clone(),
            }
        })?;

    Ok(SourceInfo {
        stream_index: selected,
        width,
        height,
        pixel_format,
        depth,
        frame_rate: stream.frame_rate().and_then(usable_rate),
        average_frame_rate: stream.average_frame_rate().and_then(usable_rate),
        start_time_seconds: stream.start_time_seconds(),
    })
}

fn usable_rate((numerator, denominator): (u32, u32)) -> Option<FrameRate> {
    FrameRate::new(numerator, denominator).ok()
}

impl LumaDepth {
    /// Resolves a supported sample depth from a pixel format and an optional
    /// reported raw sample depth.
    fn resolve(pixel_format: &str, reported_bits: Option<u32>) -> Option<Self> {
        match reported_bits {
            Some(8) => return Some(Self::Eight),
            Some(10) => return Some(Self::Ten),
            // An unsupported reported depth is not overridden by the name.
            Some(_) => return None,
            None => {}
        }

        let lower = pixel_format.to_ascii_lowercase();
        if lower.contains("10") {
            return Some(Self::Ten);
        }
        if lower.contains("12") || lower.contains("14") || lower.contains("16") {
            return None;
        }
        if lower.contains("9") {
            return None;
        }
        if lower.is_empty() {
            return None;
        }
        Some(Self::Eight)
    }
}

#[cfg(test)]
mod tests {
    use super::LumaDepth;

    #[test]
    fn resolves_supported_sample_depths() {
        assert_eq!(
            LumaDepth::resolve("yuv420p", Some(8)),
            Some(LumaDepth::Eight)
        );
        assert_eq!(
            LumaDepth::resolve("yuv420p10le", Some(10)),
            Some(LumaDepth::Ten)
        );
        assert_eq!(LumaDepth::resolve("gray10le", None), Some(LumaDepth::Ten));
        assert_eq!(LumaDepth::resolve("yuv420p", None), Some(LumaDepth::Eight));
    }

    #[test]
    fn rejects_unsupported_sample_depths() {
        assert_eq!(LumaDepth::resolve("yuv420p12le", Some(12)), None);
        assert_eq!(LumaDepth::resolve("yuv420p12le", None), None);
        assert_eq!(LumaDepth::resolve("gray16le", None), None);
        assert_eq!(LumaDepth::resolve("", None), None);
        // A reported depth outside the supported set is not overridden.
        assert_eq!(LumaDepth::resolve("yuv420p", Some(12)), None);
    }
}
