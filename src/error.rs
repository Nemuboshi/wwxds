use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;
use std::path::PathBuf;

use ihni_core::detect::DetectError;
use ihni_media::MediaError;

/// Failure returned by a pipeline entry point.
///
/// This type covers the pipeline's own failures and wraps the two component
/// errors. The pipeline owns the input path check, so [`AppError::File`] and
/// [`AppError::InvalidInput`] are its own; everything that happens to a media
/// file is [`AppError::Media`], and every detector failure is
/// [`AppError::Detection`].
///
/// The wrapped errors already name the input path and the failed operation, so
/// the wrapper adds no text of its own: `Display` for [`AppError::Media`] is
/// exactly the media error's message.
#[derive(Debug)]
pub enum AppError {
    /// A filesystem operation on the input path failed.
    File {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The input path is not a usable regular file.
    InvalidInput { path: PathBuf, reason: &'static str },
    /// The media layer failed: probing, stream selection, or decoding.
    Media(MediaError),
    /// The detector rejected a prepared frame or could not be created.
    ///
    /// The detector is an independent component with its own error type, so a
    /// library-level path that joins decoder and detector wraps that error here
    /// with the failed operation and the input path. The detection stage never
    /// mutates detector state when this is returned.
    Detection {
        operation: &'static str,
        path: PathBuf,
        source: DetectError,
    },
    /// A keyframe output file could not be created, written, or published.
    ///
    /// `path` is the requested output path, `operation` names the failed stage,
    /// and `source` is the underlying I/O reason. The output layer writes
    /// through a same-directory temporary file and publishes only a complete
    /// document, so this error always means nothing was published; a cleanup
    /// failure adds the leftover temporary path to the I/O reason.
    OutputWrite {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The run decoded no frames, so there is no keyframe list to record.
    ///
    /// `path` is the output path that was not written.
    EmptyVideo { path: PathBuf },
}

impl From<MediaError> for AppError {
    fn from(source: MediaError) -> Self {
        Self::Media(source)
    }
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::File {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} '{}': {source}",
                path.display()
            ),
            Self::InvalidInput { path, reason } => {
                write!(formatter, "invalid input '{}': {reason}", path.display())
            }
            Self::Media(source) => write!(formatter, "{source}"),
            Self::Detection {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} while detecting '{}': {source}",
                path.display()
            ),
            Self::OutputWrite {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} '{}': {source}",
                path.display()
            ),
            Self::EmptyVideo { path } => write!(
                formatter,
                "no frames were decoded, so no keyframe file was written for '{}'",
                path.display()
            ),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::File { source, .. } => Some(source),
            Self::Media(source) => Some(source),
            Self::Detection { source, .. } => Some(source),
            Self::OutputWrite { source, .. } => Some(source),
            Self::InvalidInput { .. } | Self::EmptyVideo { .. } => None,
        }
    }
}
