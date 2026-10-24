//! IHNI: a standalone Rust hard-cut detector based on WWXD.
//!
//! The workspace keeps three concerns apart. `ihni-core` is the detector alone:
//! ordered state over prepared frames, with no decoder, FFmpeg, or CLI
//! dependency. `ihni-media` owns every media file operation. This crate is the
//! pipeline that joins them, and [`detect_video`] is the only place the two
//! meet. It depends on neither a CLI type nor a JSON serializer.
//!
//! The component modules are re-exported at their existing paths, so
//! [`detect`], [`decode`], and [`frame_index`] resolve here exactly as they did
//! before the split.

pub use ihni_core::{detect, frame_index};
pub use ihni_media::decode;

pub mod error;
pub mod keyframes;
pub mod progress;

use std::path::Path;
use std::time::{Duration, Instant};

use decode::{FrameStream, GpuMode, ReaderOptions, TimeMode};
use detect::{Detector, DetectorFrame};
use error::AppError;
use progress::Progress;

/// Number of frames the decoder may hold while detection runs.
///
/// Two slots are enough to keep the decoder busy across one frame of detection
/// and bound peak memory to the slot count rather than to the length of the
/// video. The value is a scheduling choice, not a detection parameter.
pub const READ_AHEAD_FRAMES: usize = 2;

/// Run configuration for one video.
#[derive(Clone, Copy, Debug)]
pub struct RunOptions {
    /// Absolute input video stream index. `None` selects the first video stream.
    pub stream_index: Option<u32>,
    /// How the run reports frame time.
    ///
    /// The default is [`TimeMode::Unavailable`], which reports no time at all.
    /// [`TimeMode::DerivedCfr`] reports elapsed time from a declared rate and
    /// [`TimeMode::DecoderPts`] reports the decoder's own presentation time
    /// stamps. One field holds the choice, so a run cannot be configured with
    /// two time sources at once.
    pub time_mode: TimeMode,
    /// Analysis height override. `None` uses [`decode::ANALYSIS_HEIGHT`].
    pub analysis_height: Option<u32>,
    /// Decoder thread count. `None` lets the decoder choose.
    pub decoder_threads: Option<u32>,
    /// Which analysis-plane backend the run uses. The default is
    /// [`GpuMode::Off`]; see `docs/Architecture.md` §8 for the backends.
    pub gpu: GpuMode,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            analysis_height: None,
            decoder_threads: None,
            gpu: GpuMode::Off,
        }
    }
}

/// Wall time of one run, split by stage.
///
/// The producer thread (inside `ihni-media`) runs the decoder and the resize;
/// `producer_decode` and `producer_resize` are its own measured cost. The
/// consumer waits for prepared frames; `queue_wait` is that wait and is *not*
/// the cost of decoding — the wait and the producer stages overlap, so they
/// must never be added together to reconstruct the wall time. `prepare` and
/// `detect` are the detector's own two consumer-thread stages, summed over
/// every frame. `total` is the wall time of the whole call. Stages are
/// measured independently and never derived from the total.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunStats {
    /// Complete frames the detector processed.
    pub frames: u64,
    /// Time the producer thread spent decoding frames.
    pub producer_decode: Duration,
    /// Time the producer thread spent resizing them into the analysis plane.
    pub producer_resize: Duration,
    /// Time the consumer spent waiting for a prepared frame.
    ///
    /// This was the field named `decode` before the producer split; waiting is
    /// not decoding, and the two are measured on different threads.
    pub queue_wait: Duration,
    /// Time spent padding prepared frames into the detector's plane.
    pub prepare: Duration,
    /// Time spent in the block decision.
    pub detect: Duration,
    /// Total wall time of the run.
    pub total: Duration,
    /// Frames the decoder produced ahead of the detector.
    pub read_ahead: usize,
    /// The analysis-plane backend the run settled on: `"cpu"` or `"d3d11"`.
    pub backend: &'static str,
}

impl RunStats {
    /// Frames per second over the total wall time.
    ///
    /// `None` when the run took no measurable time, so a zero denominator is
    /// never reported as a fabricated rate.
    pub fn frames_per_second(&self) -> Option<f64> {
        let seconds = self.total.as_secs_f64();
        if seconds <= 0.0 {
            return None;
        }
        Some(self.frames as f64 / seconds)
    }
}

/// Result of detecting hard cuts across one video.
#[derive(Clone, Debug)]
pub struct DetectSummary {
    /// The decode summary of the run that produced the analysed frames.
    pub decode: decode::DecodeSummary,
    /// Cut boundaries in display order, first frame of each new shot.
    ///
    /// Indexes are strictly increasing and always 1 or greater: frame 0 is a
    /// valid source index but never a boundary.
    pub boundaries: Vec<frame_index::BoundaryFrameIndex>,
    /// Stage wall times for this run.
    pub stats: RunStats,
}

/// Decodes one video and returns its hard-cut boundaries.
///
/// This is [`detect_video_with_progress`] without a callback: the run reports
/// nothing while it works.
pub fn detect_video(input_path: &Path, options: RunOptions) -> Result<DetectSummary, AppError> {
    detect_video_with_progress(input_path, options, |_progress| {})
}

/// Decodes one video, reporting [`Progress`] after every completed frame.
///
/// This is the library-level end-to-end path: prepared frames flow from the
/// reader to the detector one at a time, in display order, with bounded memory
/// (the reader's reused planes plus the detector's two padded planes). Decoding
/// runs on a producer thread with [`READ_AHEAD_FRAMES`] reusable slots, so it
/// overlaps detection; the detector itself stays on one thread and sees the
/// same frame sequence a serial read would produce. Any decode or detection
/// error is returned instead of a partial summary, so a failed run never
/// reports boundaries for frames it did not fully process.
pub fn detect_video_with_progress(
    input_path: &Path,
    options: RunOptions,
    mut on_progress: impl FnMut(Progress),
) -> Result<DetectSummary, AppError> {
    let total_start = Instant::now();

    // Check the path shape before opening it. A directory can be opened on some
    // platforms and would otherwise fail later with a message about a missing
    // stream rather than about the input path.
    let metadata = std::fs::metadata(input_path).map_err(|source| AppError::File {
        operation: "open input file",
        path: input_path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(AppError::InvalidInput {
            path: input_path.to_path_buf(),
            reason: "input path is not a regular file",
        });
    }

    let mut stream = FrameStream::open(
        input_path,
        ReaderOptions {
            stream_index: options.stream_index,
            time_mode: options.time_mode,
            analysis_height: options.analysis_height,
            decoder_threads: options.decoder_threads,
            read_ahead: READ_AHEAD_FRAMES,
            gpu: options.gpu,
        },
    )?;
    // An `Auto` GPU attempt that could not proceed is a diagnostic, not an
    // error: the run continues on the CPU path and says so on standard error.
    if let Some(reason) = stream.gpu_fallback() {
        eprintln!(
            "note: the D3D11 backend is unavailable ({reason}); using the CPU analysis filter"
        );
    }
    let backend = stream.gpu_backend();
    let size = stream.analysis_size();
    let mut detector =
        Detector::new(size.width(), size.height()).map_err(|source| AppError::Detection {
            operation: "create the detector",
            path: input_path.to_path_buf(),
            source,
        })?;

    let mut boundaries = Vec::new();
    let mut decoded_frame_count: u64 = 0;
    let mut queue_wait = Duration::ZERO;
    let mut prepare_time = Duration::ZERO;
    let mut detect_time = Duration::ZERO;

    loop {
        let wait_start = Instant::now();
        let frame = stream.next_frame()?;
        queue_wait += wait_start.elapsed();
        let Some(frame) = frame else {
            break;
        };
        decoded_frame_count += 1;

        let decision = detector
            .process(DetectorFrame {
                index: frame.index(),
                width: size.width(),
                height: size.height(),
                stride: size.width() as usize,
                luma: frame.luma(),
            })
            .map_err(|source| AppError::Detection {
                operation: "process a prepared frame",
                path: input_path.to_path_buf(),
                source,
            })?;

        let timings = detector.last_timings();
        prepare_time += timings.prepare;
        detect_time += timings.analyze;

        if let Some(boundary) = decision.boundary {
            boundaries.push(boundary);
        }

        // Report after this frame's detection has fully completed. The
        // callback runs on the consumer thread with the current counters;
        // nothing is queued. `+ 1` counts the frame-0 line the output layer
        // will publish for this non-empty run.
        on_progress(Progress {
            processed_frames: decoded_frame_count,
            keyframes: u64::try_from(boundaries.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            elapsed: total_start.elapsed(),
        });

        // The detector copies the plane into its own padded buffer, so the
        // frame buffer is free to be refilled.
        stream.recycle(frame);
    }

    detector.finish().map_err(|source| AppError::Detection {
        operation: "finalise the detector",
        path: input_path.to_path_buf(),
        source,
    })?;

    // The stream ended cleanly, so the producer published its final snapshot
    // before the end-of-stream item this consumer received.
    let producer = stream.producer_timings().unwrap_or_default();

    Ok(DetectSummary {
        decode: decode::DecodeSummary {
            source: stream.source().clone(),
            analysis_size: size,
            decoded_frame_count,
        },
        boundaries,
        stats: RunStats {
            frames: decoded_frame_count,
            producer_decode: producer.decode,
            producer_resize: producer.resize,
            queue_wait,
            prepare: prepare_time,
            detect: detect_time,
            total: total_start.elapsed(),
            read_ahead: READ_AHEAD_FRAMES,
            backend,
        },
    })
}
