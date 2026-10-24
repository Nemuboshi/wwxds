//! Display-order frame decoding and preparation.
//!
//! The decoder stays separate from the detector. It returns complete prepared
//! frames, explicit metadata, and an optional derived time. It never invents a
//! source time stamp.
//!
//! The stages are:
//!
//! 1. [`probe`] selects one absolute stream index and records source metadata.
//! 2. [`analysis::AnalysisSize`] derives the fixed analysis size for the run.
//! 3. [`reader::FrameReader`] decodes complete frames in display order and
//!    resamples luma to the 8-bit analysis plane through the linked libswscale
//!    ([`resize::FrameResizer`]).
//! 4. [`overlap::FrameStream`] runs the reader on a producer thread so decoding
//!    overlaps detection without changing the frame sequence.

pub mod analysis;
pub mod overlap;
pub mod probe;
pub mod reader;
pub mod resize;
pub mod time;

pub use analysis::{ANALYSIS_HEIGHT, AnalysisSize, AnalysisSizeError};
pub use overlap::{Frame, FrameStream};
pub use probe::{LumaDepth, SourceInfo};
pub use reader::{FrameMeta, FrameReader, PreparedFrame, ReadOutcome, ReaderOptions};
pub use resize::FrameResizer;
pub use time::{FrameRate, TimeMode};

use std::time::Duration;

/// Producer-side stage times for one decode run.
///
/// `decode` and `resize` are measured on the thread that runs the reader:
/// `decode` is the time the linked libavcodec spent producing decoded frames,
/// `resize` is the time the same thread spent scaling them into the analysis
/// plane. They are independent of the consumer's wait for a prepared frame
/// (`ihni::RunStats::queue_wait`) and they overlap it, so the two must never
/// be added to each other as if they were sequential, and the producer stages
/// are not bounded by the consumer-side stages. One `Instant` pair per frame
/// around the existing calls is the measurement granularity; nothing is
/// stamped inside a pixel or block loop.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProducerTimings {
    /// Time the producer spent obtaining decoded frames.
    pub decode: Duration,
    /// Time the producer spent resizing them into the analysis plane.
    pub resize: Duration,
}

/// Result of reading every frame of one stream.
#[derive(Clone, Debug)]
pub struct DecodeSummary {
    /// Metadata for the selected source stream.
    pub source: SourceInfo,
    /// The fixed analysis size of this run.
    pub analysis_size: AnalysisSize,
    /// Number of complete frames decoded in display order.
    pub decoded_frame_count: u64,
}

/// Mean of one luma plane, used for preparation diagnostics and tests.
pub fn mean_luma(plane: &[u8]) -> f64 {
    if plane.is_empty() {
        return 0.0;
    }
    let sum: u64 = plane.iter().map(|value| u64::from(*value)).sum();
    sum as f64 / plane.len() as f64
}

#[cfg(test)]
mod tests {
    use super::mean_luma;

    #[test]
    fn mean_of_an_empty_plane_is_zero() {
        assert_eq!(mean_luma(&[]), 0.0);
    }

    #[test]
    fn mean_of_a_uniform_plane_is_that_value() {
        assert_eq!(mean_luma(&[32, 32, 32, 32]), 32.0);
    }

    #[test]
    fn mean_of_mixed_values_is_exact() {
        assert_eq!(mean_luma(&[0, 10, 20, 30]), 15.0);
    }
}
