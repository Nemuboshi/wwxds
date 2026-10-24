//! Display-order frame reader.
//!
//! Frames are decoded in this process through [`crate::ffi`], which drives
//! libavformat and libavcodec and returns the decoder's luma plane in
//! presentation order. No child process and no pipe is involved.
//!
//! The plane is then reduced to the fixed analysis size by
//! [`FrameResizer`](super::resize::FrameResizer), the project's own integer
//! box filter (`docs/Architecture.md`, §4): exact integer area averages,
//! rounded once, with no intermediate plane and no library scaler.
//!
//! Time stamps come from the decoder. The default mode still reports none: a
//! source time stamp is only reported when the caller asks for it explicitly.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ihni_core::frame_index::SourceFrameIndex;

use crate::error::MediaError;
use crate::ffi;
use crate::luma::LumaFormat;

use super::ProducerTimings;
use super::analysis::AnalysisSize;
use super::gpu::{D3d11Filter, GpuMode};
use super::probe::SourceInfo;
use super::resize::FrameResizer;
use super::time::TimeMode;

/// A borrowed view of one complete analysis frame.
///
/// The luma plane holds 8-bit samples converted from the source depth by the
/// documented shift. The bytes belong to the reader's reusable buffer, so a
/// view is valid only until the next read.
#[derive(Clone, Copy, Debug)]
pub struct PreparedFrame<'a> {
    /// Zero-based index of this frame in display order.
    pub index: SourceFrameIndex,
    /// Analysis height in pixels.
    pub height: u32,
    /// Bytes between the start of consecutive rows.
    pub stride: usize,
    /// Elapsed derived seconds, present only in `derived_cfr` mode.
    pub derived_seconds: Option<f64>,
    /// Source presentation time in seconds, present only in `decoder_pts` mode.
    pub source_seconds: Option<f64>,
    luma: &'a [u8],
}

impl<'a> PreparedFrame<'a> {
    /// The 8-bit luma plane for this frame.
    ///
    /// The plane is `height * stride` bytes. Rows are `stride` bytes apart.
    pub fn luma(&self) -> &'a [u8] {
        self.luma
    }
}

/// Read outcomes reported by [`FrameReader::next_frame`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOutcome {
    /// A complete frame was placed in the reusable buffer.
    Frame,
    /// The decoder produced no more frames.
    EndOfStream,
}

/// Options for one reader.
#[derive(Clone, Copy, Debug)]
pub struct ReaderOptions {
    /// Absolute input stream index. `None` selects the first video stream.
    pub stream_index: Option<u32>,
    /// How the run reports frame time.
    pub time_mode: TimeMode,
    /// Analysis height override. `None` uses [`super::analysis::ANALYSIS_HEIGHT`].
    pub analysis_height: Option<u32>,
    /// Decoder thread count.
    ///
    /// `None` lets libav choose. Changing this does not change the decoded luma
    /// bytes: thread count is a scheduling setting, not a quality one.
    pub decoder_threads: Option<u32>,
    /// Number of frames the decoder may read ahead when the caller overlaps
    /// decoding with detection.
    ///
    /// The producer holds this many reusable frame slots, so peak memory is
    /// bounded by the slot count rather than by the frame count. `1` means no
    /// read-ahead: the producer waits for the consumer after every frame.
    pub read_ahead: usize,
    /// Which analysis-plane backend the run uses. `Off` decodes and filters
    /// on the CPU, the historical behavior; `Auto` and `D3d11` are described
    /// in [`GpuMode`](super::gpu::GpuMode) and `docs/Architecture.md` §8.
    pub gpu: GpuMode,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        Self {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            analysis_height: None,
            decoder_threads: None,
            read_ahead: 1,
            gpu: GpuMode::Off,
        }
    }
}

impl ReaderOptions {
    /// The slot count this configuration asks for.
    ///
    /// A zero request would deadlock the producer, so it is clamped to one.
    pub const fn slot_count(self) -> usize {
        if self.read_ahead == 0 {
            1
        } else {
            self.read_ahead
        }
    }
}

/// A display-order frame reader over one video stream.
pub struct FrameReader {
    input: ffi::Input,
    /// Built from the first frame's own luma format and geometry, then reused
    /// for every later frame.
    filter: Option<PlaneFilter>,
    path: PathBuf,
    source: SourceInfo,
    size: AnalysisSize,
    time_mode: TimeMode,
    /// Stream time base, for reporting a source time stamp in seconds.
    time_base: (i32, i32),
    /// Reused 8-bit luma plane exposed to callers.
    luma_buffer: Vec<u8>,
    /// Presentation time stamp of the frame just decoded.
    pending_pts: Option<i64>,
    next_index: u64,
    /// Metadata for the frame currently in the buffer, absent before the first
    /// frame and after the end of the stream.
    current: Option<FrameMeta>,
    finished: bool,
    /// Producer-side stage time accumulated over this reader's frames.
    ///
    /// `decode` is the libavcodec call that produced each frame, `resize` the
    /// analysis-plane work on it. Both are one `Instant` pair per frame around
    /// the existing calls, never inside a pixel loop.
    producer: ProducerTimings,
    /// Why an `Auto` GPU attempt did not happen, for the caller to report.
    /// `None` on the CPU path and on a successful GPU path.
    gpu_fallback: Option<String>,
    /// Which analysis-plane backend the open settled on.
    gpu_backend: &'static str,
}

/// The analysis-plane filter of one run: the CPU implementation, or the D3D11
/// backend whose geometry was planned at open time.
enum PlaneFilter {
    Cpu(Box<FrameResizer>),
    D3d11(D3d11Filter),
}

impl PlaneFilter {
    /// The luma format the CPU filter was built for. Hardware frames track
    /// their format inside the GPU filter, so this is `None` there.
    fn source_format(&self) -> Option<LumaFormat> {
        match self {
            Self::Cpu(resizer) => Some(resizer.source_format()),
            Self::D3d11(_) => None,
        }
    }
}

/// Metadata for one complete frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameMeta {
    /// Zero-based index of this frame in display order.
    pub index: SourceFrameIndex,
    /// Elapsed derived seconds, present only in `derived_cfr` mode.
    pub derived_seconds: Option<f64>,
    /// Source presentation time in seconds, present only in `decoder_pts` mode.
    pub source_seconds: Option<f64>,
}

impl FrameReader {
    /// Opens the input, selects one stream, and starts decoding.
    ///
    /// The stream is selected and described before any frame is read, so an
    /// unusable selection fails here rather than part-way through a run.
    ///
    /// When a GPU mode is requested, the run reopens the input with D3D11
    /// hardware decoding after the software open has described the stream. An
    /// `Auto` attempt that cannot proceed — no Direct3D 11 device, no d3d11va
    /// decoder, a geometry the GPU filter does not cover — falls back to the
    /// software input and records the reason in [`FrameReader::gpu_fallback`];
    /// an explicit `D3d11` request fails the open instead.
    pub fn open(path: &Path, options: ReaderOptions) -> Result<Self, MediaError> {
        let mut input = ffi::Input::open(path, options.stream_index, options.decoder_threads)?;
        let source = super::probe::describe(&input, path)?;
        let size = source
            .analysis_size_with_height(options.analysis_height)
            .map_err(|error| MediaError::AnalysisSize {
                path: path.to_path_buf(),
                source_width: source.width,
                source_height: source.height,
                reason: error.to_string(),
            })?;

        let mut gpu_fallback = None;
        let mut gpu_backend = "cpu";
        if options.gpu != GpuMode::Off {
            let attempt =
                D3d11Filter::plan(source.width, source.height, size.width(), size.height())
                    .and_then(|_| {
                        ffi::Input::open_gpu(path, options.stream_index, options.decoder_threads)
                            .map_err(|error| error.to_string())
                    });
            match attempt {
                Ok(gpu_input) => {
                    // The GPU input describes the same file and stream, so the
                    // software probe stays authoritative for the run. The
                    // filter itself is built from the first frame's own luma
                    // depth, like the CPU resizer.
                    drop(input);
                    input = gpu_input;
                    gpu_backend = "d3d11";
                }
                Err(reason) if options.gpu == GpuMode::Auto => {
                    gpu_fallback = Some(reason);
                }
                Err(reason) => {
                    return Err(MediaError::Decode {
                        operation: "open the D3D11 backend",
                        path: path.to_path_buf(),
                        source: io::Error::other(reason),
                    });
                }
            }
        }
        let time_base = input.stream().time_base();

        Ok(Self {
            input,
            filter: None,
            path: path.to_path_buf(),
            source,
            size,
            time_mode: options.time_mode,
            time_base,
            luma_buffer: Vec::new(),
            pending_pts: None,
            next_index: 0,
            current: None,
            finished: false,
            producer: ProducerTimings::default(),
            gpu_fallback,
            gpu_backend,
        })
    }

    /// Metadata for the selected source stream.
    pub fn source(&self) -> &SourceInfo {
        &self.source
    }

    /// The fixed analysis size of this run.
    pub fn analysis_size(&self) -> AnalysisSize {
        self.size
    }

    /// The most recently read frame, if the last read returned a frame.
    pub fn frame(&self) -> Option<PreparedFrame<'_>> {
        let meta = self.current?;
        Some(PreparedFrame {
            index: meta.index,
            height: self.size.height(),
            stride: self.size.width() as usize,
            derived_seconds: meta.derived_seconds,
            source_seconds: meta.source_seconds,
            luma: &self.luma_buffer,
        })
    }

    /// Metadata for the frame currently held in the buffer.
    pub fn current_meta(&self) -> Option<FrameMeta> {
        self.current
    }

    /// Producer-side decode and resize time accumulated over the frames read
    /// so far.
    ///
    /// A caller that runs this reader on its own thread reads this after the
    /// thread joins; the overlapped [`super::overlap::FrameStream`] reports it
    /// for its producer. The numbers describe this reader's own frames only.
    pub fn producer_timings(&self) -> ProducerTimings {
        self.producer
    }

    /// The analysis-plane backend this run uses: `"cpu"` or `"d3d11"`.
    pub fn gpu_backend(&self) -> &'static str {
        self.gpu_backend
    }

    /// Why an `Auto` GPU attempt fell back to the CPU path, if it did.
    ///
    /// `None` on an unrequested GPU path, on a successful GPU run, and on an
    /// explicit `D3d11` request (which fails the open instead).
    pub fn gpu_fallback(&self) -> Option<&str> {
        self.gpu_fallback.as_deref()
    }

    /// Reads the next complete frame into the reusable buffer.
    pub fn next_frame(&mut self) -> Result<ReadOutcome, MediaError> {
        // Move the buffer out so the read can borrow it mutably while `self`
        // also stays borrowed, then put it back on every path.
        let mut buffer = std::mem::take(&mut self.luma_buffer);
        let outcome = self.next_frame_into(&mut buffer);
        self.luma_buffer = buffer;
        outcome
    }

    /// Reads the next complete frame into `luma`.
    ///
    /// This is [`FrameReader::next_frame`] with a caller-owned plane, so an
    /// overlapping producer can fill a reusable slot without an extra copy.
    /// `luma` is resized to the analysis plane size and is unchanged when the
    /// read fails.
    pub fn next_frame_into(&mut self, luma: &mut Vec<u8>) -> Result<ReadOutcome, MediaError> {
        if self.finished {
            return Err(MediaError::Decode {
                operation: "read a frame after the end of the stream",
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::UnexpectedEof, "stream already ended"),
            });
        }

        let index = SourceFrameIndex::new(self.next_index);
        // Producer-side decode stage: the wall time of the libavcodec call,
        // measured on the thread that made it. The end-of-stream drain is the
        // decoder flushing its queue, so its time belongs to this stage too.
        // The returned plane borrows `self.input`; `self.producer` is a
        // disjoint field, so the accumulator updates while that borrow is live.
        let decode_start = Instant::now();
        let next = self.input.next_frame();
        self.producer.decode += decode_start.elapsed();
        let Some(frame) = next? else {
            self.current = None;
            self.finish()?;
            return Ok(ReadOutcome::EndOfStream);
        };

        let width = frame.width();
        let height = frame.height();
        if width != self.source.width || height != self.source.height {
            // A stream that changes size part-way through is rejected rather
            // than resized with a context built for the first size.
            return Err(MediaError::Decode {
                operation: "resize a frame whose size changed",
                path: self.path.clone(),
                source: io::Error::other(format!(
                    "the stream is {}x{} but the decoder produced {width}x{height}",
                    self.source.width, self.source.height
                )),
            });
        }

        let Some(luma_format) = frame.luma_format() else {
            return Err(MediaError::UnsupportedPixelFormat {
                path: self.path.clone(),
                pixel_format: frame.pixel_format_name(),
            });
        };

        // The filter is built from the first frame's own luma format, so the
        // format that is filtered is the one the decoder actually produced
        // rather than what the container header promised. Every later frame is
        // checked against it: a midstream luma-format change would feed a
        // plane of a different sample layout to a filter built for the first
        // format, so it is rejected like a size change rather than silently
        // rescaled.
        //
        // The fields are read individually rather than through a method, because
        // the frame borrows `self.input` for as long as it is alive; building
        // and filtering borrow only `self.filter` and `self.path`, disjoint
        // fields, so the frame's borrow can stay live across them.
        if self.filter.is_none() {
            let size = self.size;
            let filter = if frame.is_d3d11() {
                PlaneFilter::D3d11(
                    D3d11Filter::plan(width, height, size.width(), size.height()).map_err(
                        |error| MediaError::Decode {
                            operation: "plan the D3D11 analysis filter",
                            path: self.path.clone(),
                            source: io::Error::other(error),
                        },
                    )?,
                )
            } else {
                let resizer = build_resizer(width, height, luma_format, size).map_err(|error| {
                    MediaError::Decode {
                        operation: "build the frame resizer",
                        path: self.path.clone(),
                        source: io::Error::other(error),
                    }
                })?;
                PlaneFilter::Cpu(Box::new(resizer))
            };
            self.filter = Some(filter);
        }
        // The CPU filter pins the format it was built for; hardware frames
        // track theirs inside the GPU filter.
        if let Some(filter_format) = self.filter.as_ref().and_then(PlaneFilter::source_format)
            && filter_format != luma_format
        {
            return Err(MediaError::Decode {
                operation: "resize a frame whose pixel format changed",
                path: self.path.clone(),
                source: io::Error::other(format!(
                    "the stream filtered as {} but the decoder produced a frame in {}",
                    filter_format.name(),
                    luma_format.name()
                )),
            });
        }

        // The filter writes the tight analysis plane directly from the
        // decoded luma, so the detector's bytes are exactly the filter's
        // output.
        let pixels = self.size.luma_bytes();
        if luma.len() != pixels {
            luma.resize(pixels, 0);
        }
        let filter = self.filter.as_mut().ok_or_else(|| MediaError::Decode {
            operation: "resize a decoded frame",
            path: self.path.clone(),
            source: io::Error::other("the filter was not built"),
        })?;
        // Producer-side resize stage: one `Instant` pair around the existing
        // filter call. The frame borrow above keeps `self.input` claimed, and
        // `self.producer` is a disjoint field.
        let resize_start = Instant::now();
        match filter {
            PlaneFilter::Cpu(resizer) => {
                resizer
                    .resize(frame.plane(), frame.stride(), luma)
                    .map_err(|error| MediaError::Decode {
                        operation: "resize a decoded frame",
                        path: self.path.clone(),
                        source: io::Error::other(error),
                    })?;
            }
            // Hardware frames carry no host plane; the D3D11 filter does its
            // own format tracking and produces the analysis plane on the
            // device.
            PlaneFilter::D3d11(gpu) => {
                gpu.filter(&frame, luma)
                    .map_err(|error| MediaError::Decode {
                        operation: "resize a decoded frame",
                        path: self.path.clone(),
                        source: io::Error::other(error),
                    })?
            }
        }
        self.producer.resize += resize_start.elapsed();

        self.pending_pts = frame.pts();
        self.next_index += 1;
        self.current = Some(FrameMeta {
            index,
            derived_seconds: self.time_mode.elapsed_seconds(index.get()),
            source_seconds: self
                .time_mode
                .source_seconds(self.pending_pts, self.time_base),
        });
        Ok(ReadOutcome::Frame)
    }

    /// Confirms that decoding reported no problem, and that every frame was
    /// read.
    ///
    /// A caller that stops early must call [`FrameReader::close`] instead. Any
    /// diagnostic the decoder reported is incomplete processing, so a partial
    /// frame sequence is never reported as a complete result.
    pub fn finish(&mut self) -> Result<(), MediaError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        // libav reports a damaged or truncated container through its log and can
        // still return a clean end of stream. A diagnostic therefore means the
        // frame sequence may be incomplete.
        let diagnostics = ffi::take_diagnostic();
        if !diagnostics.is_empty() {
            return Err(MediaError::IncompleteProcessing {
                path: self.path.clone(),
                diagnostics,
            });
        }
        Ok(())
    }

    /// Stops reading without requiring success.
    ///
    /// This is the early-termination path. It discards any diagnostic the
    /// decoder reported, because the caller is not claiming a complete result.
    pub fn close(&mut self) -> Result<(), MediaError> {
        let _ = ffi::take_diagnostic();
        self.current = None;
        self.finished = true;
        Ok(())
    }
}

/// Builds the swscale-backed resizer for one source geometry and format.
///
/// This is a free function rather than a method because the caller builds it
/// while a decoded frame still borrows the input; taking `&self` there would
/// conflict with that borrow.
///
/// The error names the conversion and the reason, because a rejected geometry
/// is a configuration the caller can act on.
fn build_resizer(
    width: u32,
    height: u32,
    format: LumaFormat,
    size: AnalysisSize,
) -> Result<FrameResizer, String> {
    FrameResizer::open(width, height, format, size.width(), size.height()).map_err(|error| {
        format!(
            "cannot resize {width}x{height} {} to {}x{}: {error}",
            format.name(),
            size.width(),
            size.height()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::ReaderOptions;

    #[test]
    fn a_zero_read_ahead_is_clamped_to_one_slot() {
        // The producer would deadlock with no slot to fill, so the request is
        // clamped rather than honoured.
        let none = ReaderOptions {
            read_ahead: 0,
            ..ReaderOptions::default()
        };
        assert_eq!(none.slot_count(), 1);

        let two = ReaderOptions {
            read_ahead: 2,
            ..ReaderOptions::default()
        };
        assert_eq!(two.slot_count(), 2);
    }
}
