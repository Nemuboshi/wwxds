use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;
use std::path::PathBuf;

/// Failure returned by the media layer.
///
/// Every variant names the failed operation and the input path so a caller can
/// report the failure without guessing which stage failed. The path is carried
/// here rather than supplied by the caller because each stage phrases it
/// differently, and the message the user sees is part of this crate's contract.
#[derive(Debug)]
pub enum MediaError {
    /// The container could not be opened or its streams could not be read.
    ContainerOpen { path: PathBuf, reason: String },
    /// No stream has the requested absolute index.
    StreamNotFound { path: PathBuf, stream_index: u32 },
    /// The input has no video stream to analyse.
    NoVideoStream { path: PathBuf },
    /// The selected stream is not a video stream.
    NotVideoStream {
        path: PathBuf,
        stream_index: u32,
        codec_type: String,
    },
    /// The source reports a dimension that cannot be analysed.
    SourceDimensions {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    /// The fixed analysis size is outside the documented safety bounds.
    AnalysisSize {
        path: PathBuf,
        source_width: u32,
        source_height: u32,
        reason: String,
    },
    /// The source pixel format is outside the documented supported set.
    UnsupportedPixelFormat { path: PathBuf, pixel_format: String },
    /// The decode producer thread could not be started.
    ThreadSpawn { path: PathBuf, source: io::Error },
    /// Decoding failed.
    Decode {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The decoder reported a problem, so no frame sequence is reported.
    IncompleteProcessing { path: PathBuf, diagnostics: String },
}

impl Display for MediaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainerOpen { path, reason } => write!(
                formatter,
                "cannot read the container of '{}': {reason}",
                path.display()
            ),
            Self::StreamNotFound { path, stream_index } => write!(
                formatter,
                "input '{}' has no stream with absolute index {stream_index}",
                path.display()
            ),
            Self::NoVideoStream { path } => write!(
                formatter,
                "input '{}' has no video stream to analyse",
                path.display()
            ),
            Self::NotVideoStream {
                path,
                stream_index,
                codec_type,
            } => write!(
                formatter,
                "stream {stream_index} of '{}' is '{codec_type}', not a video stream",
                path.display()
            ),
            Self::SourceDimensions {
                path,
                width,
                height,
            } => write!(
                formatter,
                "input '{}' reports an unusable source size {width}x{height}",
                path.display()
            ),
            Self::AnalysisSize {
                path,
                source_width,
                source_height,
                reason,
            } => write!(
                formatter,
                "cannot derive an analysis size from source {}x{} in '{}': {reason}",
                source_width,
                source_height,
                path.display()
            ),
            Self::UnsupportedPixelFormat { path, pixel_format } => write!(
                formatter,
                "input '{}' uses unsupported pixel format '{pixel_format}'; \
                 supported 8-bit and 10-bit formats are listed in docs/Architecture.md",
                path.display()
            ),
            Self::ThreadSpawn { path, source } => write!(
                formatter,
                "failed to start the decode thread for '{}': {source}",
                path.display()
            ),
            Self::Decode {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} while decoding '{}': {source}",
                path.display()
            ),
            Self::IncompleteProcessing { path, diagnostics } => {
                write!(
                    formatter,
                    "processing '{}' did not complete; no frame sequence is reported",
                    path.display()
                )?;
                if !diagnostics.is_empty() {
                    write!(formatter, "; diagnostics: {diagnostics}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for MediaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ThreadSpawn { source, .. } | Self::Decode { source, .. } => Some(source),
            Self::ContainerOpen { .. }
            | Self::StreamNotFound { .. }
            | Self::NoVideoStream { .. }
            | Self::NotVideoStream { .. }
            | Self::SourceDimensions { .. }
            | Self::AnalysisSize { .. }
            | Self::UnsupportedPixelFormat { .. }
            | Self::IncompleteProcessing { .. } => None,
        }
    }
}
