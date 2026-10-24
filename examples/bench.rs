//! Repeatable throughput benchmark for the complete detection path, plus a
//! detector-only replay mode.
//!
//! Run with a release build and a long input:
//!
//! ```text
//! cargo run --release --example bench -- <INPUT> [--iterations N] [--analysis-height H]
//!     [--decoder-threads N] [--detector-only [FRAMES]]
//! ```
//!
//! End-to-end mode re-opens the input on every run and starts a fresh decoder,
//! so `wall_s` includes process start-up, decode, scaling, the plane reduction,
//! padding, and detection. The stage times are measured independently by the
//! library and are never derived from the total. Decoding and resizing run on a
//! producer thread: `producer_decode_s` and `producer_resize_s` are that
//! thread's own cost, and `queue_wait_s` is the time the consumer spent waiting
//! for a prepared frame — the part of decoding detection did not hide. The
//! producer stages overlap the wait, so they are reported side by side and must
//! never be added together to reconstruct the wall time. `read_ahead` reports
//! how many frames the decoder may hold ahead of the detector.
//!
//! Detector-only mode (`--detector-only [FRAMES]`, default 120) separates the
//! detector's throughput from the pipeline's. It decodes the input exactly
//! once, keeping the first `FRAMES` analysis planes as gray8 buffers (a fixed,
//! bounded short sequence: 120 frames at 640x360 is about 27 MB, never a whole
//! long video), then replays that sequence `--iterations` times through a
//! freshly built `Detector` for every iteration. The replay includes the same
//! padding and block-decision work the pipeline does, and excludes the
//! decoder, the resize, the producer thread, and the queue wait entirely. The
//! two numbers are reported separately: `detector_fps` (replay only) and
//! `fps` (end-to-end, decoding included) measure different things and must not
//! be compared as one.
//!
//! CPU seconds are not reported: measuring them portably would need a platform
//! call or an extra crate. Use the shell's `time` around this command instead,
//! and keep the input, build profile, thread counts, and hardware fixed when
//! comparing runs.
//!
//! Runs on a shared machine drift with whatever else is running. Three wall
//! statistics are therefore reported together: `wall_median_s` is the number to
//! compare across builds, `wall_min_s` is the least contaminated estimate of
//! what this build can do (other processes can only make a run slower, never
//! faster), and `wall_max_s` bounds the ambient spread over the measured runs.
//! A difference between two builds that is smaller than that spread is not
//! evidence; run interleaved pairs of both builds and compare the pairs.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use ihni::decode::{FrameStream, ReaderOptions};
use ihni::detect::{Detector, DetectorFrame};
use ihni::frame_index::SourceFrameIndex;
use ihni::{RunOptions, detect_video};

/// Analysis frames kept for the detector-only replay when no count is given.
const DEFAULT_REPLAY_FRAMES: usize = 120;

struct Args {
    input: PathBuf,
    iterations: u32,
    analysis_height: Option<u32>,
    decoder_threads: Option<u32>,
    detector_only: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut iterations = 5u32;
    let mut analysis_height = None;
    let mut decoder_threads = None;
    let mut detector_only = None;

    // A queue rather than a plain iterator, because `--detector-only`'s value
    // is optional: the peeked argument has to be put back when it is another
    // option instead of a count.
    let mut args: std::collections::VecDeque<String> = std::env::args().skip(1).collect();
    while let Some(arg) = args.pop_front() {
        match arg.as_str() {
            "--iterations" => {
                let value = args.pop_front().ok_or("--iterations needs a value")?;
                iterations = value
                    .parse()
                    .map_err(|_| format!("invalid --iterations '{value}'"))?;
                if iterations == 0 {
                    return Err("--iterations must be at least 1".to_owned());
                }
            }
            "--analysis-height" => {
                let value = args.pop_front().ok_or("--analysis-height needs a value")?;
                analysis_height = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid --analysis-height '{value}'"))?,
                );
            }
            "--decoder-threads" => {
                let value = args.pop_front().ok_or("--decoder-threads needs a value")?;
                decoder_threads = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid --decoder-threads '{value}'"))?,
                );
            }
            "--detector-only" => {
                detector_only = Some(peek_count(&mut args));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            path => {
                if input.is_some() {
                    return Err("only one input path is accepted".to_owned());
                }
                input = Some(PathBuf::from(path));
            }
        }
    }

    Ok(Args {
        input: input.ok_or("an input path is required")?,
        iterations,
        analysis_height,
        decoder_threads,
        detector_only,
    })
}

/// The optional frame count after `--detector-only`.
///
/// The flag works bare (`--detector-only`) and with a value
/// (`--detector-only 240`). A value is accepted only when the next argument is
/// not another option: an option-looking argument is put back on the queue and
/// parsed on its own, so `bench A --detector-only --iterations 3` keeps the
/// default and does not swallow `--iterations`.
fn peek_count(args: &mut std::collections::VecDeque<String>) -> usize {
    match args.pop_front() {
        Some(value) if !value.starts_with('-') => value.parse().unwrap_or(DEFAULT_REPLAY_FRAMES),
        other => {
            // Not a count (an option, or nothing): put it back for the main
            // loop to parse, and keep the documented default.
            if let Some(value) = other {
                args.push_front(value);
            }
            DEFAULT_REPLAY_FRAMES
        }
    }
}

fn median_min_max(values: &[f64]) -> (f64, f64, f64) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    (
        sorted[sorted.len() / 2],
        sorted[0],
        sorted[sorted.len() - 1],
    )
}

/// Captures the first `frames` analysis planes of the input, in display order.
///
/// The sequence is decoded exactly once through the same overlapped stream the
/// pipeline uses, then kept as owned gray8 planes. The count is fixed and
/// small (120 frames at 640x360 is about 27 MB); the benchmark never holds a
/// whole video.
fn capture_replay(
    input: &Path,
    frames: usize,
    height: Option<u32>,
    decoder_threads: Option<u32>,
) -> Result<(u32, u32, Vec<Vec<u8>>), String> {
    let mut stream = FrameStream::open(
        input,
        ReaderOptions {
            stream_index: None,
            time_mode: ihni::decode::TimeMode::Unavailable,
            analysis_height: height,
            decoder_threads,
            read_ahead: ihni::READ_AHEAD_FRAMES,
            gpu: ihni::decode::GpuMode::Off,
        },
    )
    .map_err(|error| format!("the input could not be opened: {error}"))?;
    let size = stream.analysis_size();
    let mut planes = Vec::with_capacity(frames);
    while planes.len() < frames {
        match stream.next_frame() {
            Ok(Some(frame)) => {
                planes.push(frame.luma().to_vec());
                stream.recycle(frame);
            }
            Ok(None) => break,
            Err(error) => return Err(format!("the capture run failed: {error}")),
        }
    }
    if planes.is_empty() {
        return Err("the input produced no analysis frames".to_owned());
    }
    Ok((size.width(), size.height(), planes))
}

/// Replays the captured sequence through a fresh detector, once per iteration.
///
/// Every iteration builds a new `Detector` and feeds it the same frame
/// sequence a pipeline run would have produced, so the replay starts from the
/// same first-frame marker state. Returns the per-iteration wall times and the
/// boundary count (identical every iteration: the same bytes through the same
/// exact schedule), plus the summed prepare and detect stage times as the
/// library reports them.
fn replay(
    width: u32,
    height: u32,
    planes: &[Vec<u8>],
    iterations: u32,
) -> Result<(Vec<f64>, usize, f64, f64), String> {
    let mut walls = Vec::with_capacity(iterations as usize);
    let mut prepare = 0u128;
    let mut detect = 0u128;
    let mut boundaries = None;
    for _ in 0..iterations {
        let mut detector = Detector::new(width, height)
            .map_err(|error| format!("the detector could not be created: {error}"))?;
        let start = Instant::now();
        let mut iteration_boundaries = 0usize;
        for (index, plane) in planes.iter().enumerate() {
            let decision = detector
                .process(DetectorFrame {
                    index: SourceFrameIndex::new(index as u64),
                    width,
                    height,
                    stride: width as usize,
                    luma: plane,
                })
                .map_err(|error| format!("the replay rejected a frame: {error}"))?;
            if decision.boundary.is_some() {
                iteration_boundaries += 1;
            }
            let timings = detector.last_timings();
            prepare += timings.prepare.as_nanos();
            detect += timings.analyze.as_nanos();
        }
        detector
            .finish()
            .map_err(|error| format!("the replay could not finish: {error}"))?;
        walls.push(start.elapsed().as_secs_f64());
        match boundaries {
            None => boundaries = Some(iteration_boundaries),
            Some(expected) if expected != iteration_boundaries => {
                return Err(format!(
                    "the replay is not deterministic: iteration reported {iteration_boundaries} boundaries, an earlier one {expected}"
                ));
            }
            _ => {}
        }
    }
    let iterations = u128::from(iterations);
    Ok((
        walls,
        boundaries.unwrap_or(0),
        nanoseconds_to_seconds(prepare / iterations),
        nanoseconds_to_seconds(detect / iterations),
    ))
}

fn nanoseconds_to_seconds(value: u128) -> f64 {
    value as f64 / 1e9
}

fn run_detector_only(args: &Args) -> ExitCode {
    let frames = args.detector_only.unwrap_or(DEFAULT_REPLAY_FRAMES);
    let (width, height, planes) = match capture_replay(
        &args.input,
        frames,
        args.analysis_height,
        args.decoder_threads,
    ) {
        Ok(captured) => captured,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(1);
        }
    };
    let replayed = planes.len();

    // One warm-up replay so the allocator and the caches are in a steady
    // state, then the measured iterations.
    let _ = replay(width, height, &planes, 1);
    let (walls, boundaries, prepare_s, detect_s) =
        match replay(width, height, &planes, args.iterations) {
            Ok(result) => result,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(1);
            }
        };

    let (median_wall, min_wall, max_wall) = median_min_max(&walls);
    let frames_per_replay = replayed as f64;
    println!(
        "mode=detector-only input={} replay_frames={} analysis={}x{} boundaries={} iterations={}",
        args.input.display(),
        replayed,
        width,
        height,
        boundaries,
        walls.len(),
    );
    println!(
        "wall_median_s={:.6} wall_min_s={:.6} wall_max_s={:.6} prepare_s={:.6} detect_s={:.6}",
        median_wall, min_wall, max_wall, prepare_s, detect_s,
    );
    println!(
        "detector_fps={:.3} (frames/wall_median; excludes decode, resize, and queue wait)",
        frames_per_replay / median_wall,
    );
    ExitCode::SUCCESS
}

fn run_end_to_end(args: &Args) -> ExitCode {
    let options = RunOptions {
        analysis_height: args.analysis_height,
        decoder_threads: args.decoder_threads,
        ..RunOptions::default()
    };

    // One warm-up run so the decoder and the page cache are in a steady state.
    if let Err(error) = detect_video(&args.input, options) {
        eprintln!("warm-up run failed: {error}");
        return ExitCode::from(1);
    }

    let mut runs = Vec::with_capacity(args.iterations as usize);
    for _ in 0..args.iterations {
        let start = Instant::now();
        match detect_video(&args.input, options) {
            Ok(summary) => runs.push((start.elapsed(), summary)),
            Err(error) => {
                eprintln!("run failed: {error}");
                return ExitCode::from(1);
            }
        }
    }

    let Some((_, first)) = runs.first() else {
        return ExitCode::from(1);
    };
    let frames = first.stats.frames;
    let analysis = first.decode.analysis_size;
    let boundaries = first.boundaries.len();

    let count = runs.len() as f64;
    let mean = |value: fn(&ihni::RunStats) -> std::time::Duration| -> f64 {
        runs.iter()
            .map(|(_, summary)| value(&summary.stats).as_secs_f64())
            .sum::<f64>()
            / count
    };

    // Report the median wall time too: it is the number to compare across
    // builds, because one slow run does not move it. The minimum is the least
    // contaminated run (ambient load can only add time), and the maximum bounds
    // the spread, so a reader can tell a real difference from noise.
    let walls: Vec<f64> = runs.iter().map(|(wall, _)| wall.as_secs_f64()).collect();
    let (median_wall, min_wall, max_wall) = median_min_max(&walls);

    println!(
        "input={} frames={} analysis={}x{} boundaries={} iterations={} read_ahead={}",
        args.input.display(),
        frames,
        analysis.width(),
        analysis.height(),
        boundaries,
        runs.len(),
        first.stats.read_ahead,
    );
    println!(
        "wall_s={:.6} wall_median_s={:.6} wall_min_s={:.6} wall_max_s={:.6} \
         producer_decode_s={:.6} producer_resize_s={:.6} queue_wait_s={:.6} prepare_s={:.6} detect_s={:.6}",
        mean(|stats| stats.total),
        median_wall,
        min_wall,
        max_wall,
        mean(|stats| stats.producer_decode),
        mean(|stats| stats.producer_resize),
        mean(|stats| stats.queue_wait),
        mean(|stats| stats.prepare),
        mean(|stats| stats.detect),
    );
    // The end-to-end rate includes decoding. The consumer-only rates use only
    // the consumer thread's own stages; the producer stages overlap the wait,
    // so no combination of these columns adds up to the wall time, and none is
    // reported as if it did.
    println!(
        "fps={:.3} queue_wait_fps={:.3} detect_fps={:.3}",
        frames as f64 / mean(|stats| stats.total),
        frames as f64 / mean(|stats| stats.queue_wait),
        if mean(|stats| stats.detect) > 0.0 {
            frames as f64 / mean(|stats| stats.detect)
        } else {
            f64::INFINITY
        },
    );
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!(
                "usage: bench <INPUT> [--iterations N] [--analysis-height H] \
                 [--decoder-threads N] [--detector-only [FRAMES]]"
            );
            return ExitCode::from(2);
        }
    };

    if cfg!(debug_assertions) {
        // Release builds are the only measurements that mean anything.
        eprintln!("warning: this is a debug build; build with --release for measurements");
    }

    if args.detector_only.is_some() {
        run_detector_only(&args)
    } else {
        run_end_to_end(&args)
    }
}
