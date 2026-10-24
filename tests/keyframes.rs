//! The Aegisub keyframe output layer.
//!
//! These tests pin the public surface of `wwxds::keyframes`: the exact document
//! bytes, frame 0 written once by the output layer, the FPS metadata precedence,
//! the default path rule, and the publish-on-success contract (never overwrite,
//! never publish a failed write, and leave no temporary file behind).
//!
//! The byte-exact fixture expectation is `target/m1-fixtures/cut8.mkv`: twelve
//! frames at an average frame rate of `12/1` with one hard cut at frame 6. Like
//! the other media suites these skip (report and return without asserting) when
//! a fixture is absent, because generating the fixtures needs FFmpeg and is a
//! separate documented step.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use wwxds::decode::FrameRate;
use wwxds::decode::analysis::AnalysisSize;
use wwxds::decode::probe::{LumaDepth, SourceInfo};
use wwxds::error::AppError;
use wwxds::frame_index::{BoundaryFrameIndex, SourceFrameIndex};
use wwxds::keyframes::{default_output_path, write_kf_file};
use wwxds::{DetectSummary, RunOptions, RunStats, detect_video};

fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/m1-fixtures")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!(
            "skipping: fixture '{}' is absent; run bash scripts/make-m1-fixtures.sh",
            path.display()
        );
        None
    }
}

/// Builds a summary without a decoder, so the output layer is tested
/// independently of any container. `frames` is the run frame count, which is
/// what decides whether a video is empty. The boundaries go through the same
/// non-zero type the detector produces.
fn summary(frames: u64, boundaries: &[u64], source: SourceInfo) -> DetectSummary {
    let kept = boundaries
        .iter()
        .map(|value| {
            BoundaryFrameIndex::new(SourceFrameIndex::new(*value)).unwrap_or_else(|error| {
                panic!("boundary {value} must be above frame zero: {error}")
            })
        })
        .collect();
    DetectSummary {
        decode: wwxds::decode::DecodeSummary {
            source,
            analysis_size: AnalysisSize::new(96, 64).expect("96x64 is inside the analysis bounds"),
            decoded_frame_count: frames,
        },
        boundaries: kept,
        stats: RunStats {
            frames,
            ..RunStats::default()
        },
    }
}

/// A synthetic one-video-stream source with the given average and nominal rates.
fn source(average: Option<(u32, u32)>, nominal: Option<(u32, u32)>) -> SourceInfo {
    let rate = |pair: Option<(u32, u32)>| {
        pair.map(|(numerator, denominator)| {
            FrameRate::new(numerator, denominator).expect("test rates are positive")
        })
    };
    SourceInfo {
        stream_index: 0,
        width: 96,
        height: 64,
        pixel_format: "yuv420p".to_owned(),
        depth: LumaDepth::Eight,
        frame_rate: rate(nominal),
        average_frame_rate: rate(average),
        start_time_seconds: None,
    }
}

/// A non-empty run with three cuts at a nominal 23.976 drop-frame rate.
fn three_cut_summary() -> DetectSummary {
    summary(1000, &[1, 42, 125], source(Some((24000, 1001)), None))
}

/// The names of every entry directly inside one directory, sorted.
fn entries(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<OsString> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("listing '{}' failed: {error}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("directory entry under '{}' failed: {error}", dir.display())
                })
                .file_name()
        })
        .collect();
    names.sort();
    names
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path)
        .unwrap_or_else(|error| panic!("reading '{}' failed: {error}", path.display()))
}

/// Writes one summary and returns the published bytes.
fn published(summary: &DetectSummary, output: &Path) -> Vec<u8> {
    write_kf_file(output, summary).unwrap_or_else(|error| panic!("{}: {error}", output.display()));
    read(output)
}

#[test]
fn writes_aegisub_v1() {
    // The complete path over a real fixture, byte for byte: two header lines,
    // frame 0 added by the file layer, then the cut at frame 6.
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("cut8.mkv");
    std::fs::copy(&path, &input).expect("the fixture is copied into the temporary directory");
    let summary = detect_video(&input, RunOptions::default())
        .expect("the complete path over the real fixture succeeds");
    assert_eq!(
        summary
            .boundaries
            .iter()
            .copied()
            .map(BoundaryFrameIndex::get)
            .collect::<Vec<_>>(),
        vec![6],
        "the cut is still reported at frame 6"
    );

    let output = default_output_path(&input);
    assert_eq!(
        output
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        Some("cut8.mkv.kf.txt".to_owned())
    );
    let bytes = published(&summary, &output);
    assert_eq!(
        bytes, b"# keyframe format v1\nfps 12.000000\n0\n6\n",
        "the document is the two headers, then frame 0, then the cut"
    );
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "no UTF-8 BOM");
    assert!(!bytes.contains(&b'\r'), "LF newlines only");
    assert_eq!(bytes.last(), Some(&b'\n'), "trailing newline");
    assert_eq!(
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| *line == b"0\n")
            .count(),
        1,
        "frame 0 appears on exactly one line"
    );
    // The temporary was published, not left behind: only input and output remain.
    assert_eq!(
        entries(temporary.path()),
        vec![
            OsString::from("cut8.mkv"),
            OsString::from("cut8.mkv.kf.txt")
        ]
    );
}

#[test]
fn first_frame_is_written_once() {
    // Whatever the cut list holds, the output layer adds frame 0 exactly once
    // and keeps the WWXD boundaries in their strictly increasing order.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let output = temporary.path().join("clip.mkv.kf.txt");
    assert_eq!(
        published(&three_cut_summary(), &output),
        b"# keyframe format v1\nfps 23.976024\n0\n1\n42\n125\n"
    );
    // The same rule over a video whose only cut is frame 1: the `0` line and
    // the `1` line are distinct lines, not one merged number.
    let single = summary(9, &[1], source(Some((24000, 1001)), None));
    let second = temporary.path().join("single.mkv.kf.txt");
    assert_eq!(
        published(&single, &second),
        b"# keyframe format v1\nfps 23.976024\n0\n1\n"
    );
    let document = String::from_utf8(read(&second)).expect("the document is valid UTF-8");
    let lines: Vec<&str> = document.lines().collect();
    assert_eq!(
        lines.iter().filter(|line| **line == "0").count(),
        1,
        "frame 0 is written once: {lines:?}"
    );
}

#[test]
fn nonempty_no_cut_writes_zero() {
    // A video without any detected cut is still a video: headers plus frame 0.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let no_cuts = summary(12, &[], source(Some((24000, 1001)), None));
    let output = temporary.path().join("nocut.mkv.kf.txt");
    assert_eq!(
        published(&no_cuts, &output),
        b"# keyframe format v1\nfps 23.976024\n0\n"
    );
    // One decoded frame counts as non-empty.
    let one_frame = summary(1, &[], source(Some((30, 1)), None));
    let second = temporary.path().join("one.mkv.kf.txt");
    assert_eq!(
        published(&one_frame, &second),
        b"# keyframe format v1\nfps 30.000000\n0\n"
    );
}

#[test]
fn zero_frames_is_error() {
    // A stream that yields no decodable frame has no keyframe list to record:
    // the write fails and publishes nothing at all.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let output = temporary.path().join("empty.mkv.kf.txt");
    let empty = summary(0, &[1, 2, 3], source(Some((24000, 1001)), None));
    let error =
        write_kf_file(&output, &empty).expect_err("a zero-frame run must not produce a file");
    assert!(
        matches!(error, AppError::EmptyVideo { .. }),
        "an empty run is its own error kind, not an I/O failure: {error}"
    );
    assert!(error.to_string().contains("empty.mkv.kf.txt"), "{error}");
    assert!(!output.exists(), "a rejected run leaves no output");
    assert!(
        entries(temporary.path()).is_empty(),
        "and no temporary file"
    );
}

#[test]
fn fps_metadata_does_not_reindex_frames() {
    // Variable frame rate and a nonzero stream start change only the FPS header
    // line; the frame indexes stay the display-order ones the detector found.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    if let Some(path) = fixture("vfr.mp4") {
        let summary = detect_video(&path, RunOptions::default())
            .expect("the variable frame rate fixture decodes");
        // The fixture is built so nominal and average differ: 30/1 versus 75/16.
        assert_eq!(summary.decode.decoded_frame_count, 20);
        assert_eq!(
            summary
                .decode
                .source
                .frame_rate
                .map(|rate| (rate.numerator(), rate.denominator())),
            Some((30, 1))
        );
        assert_eq!(
            summary
                .decode
                .source
                .average_frame_rate
                .map(|rate| (rate.numerator(), rate.denominator())),
            Some((75, 16))
        );
        assert_eq!(
            summary
                .boundaries
                .iter()
                .copied()
                .map(BoundaryFrameIndex::get)
                .collect::<Vec<_>>(),
            vec![10],
            "the cut keeps its display-order index"
        );
        let output = temporary.path().join("vfr.mp4.kf.txt");
        assert_eq!(
            published(&summary, &output),
            b"# keyframe format v1\nfps 4.687500\n0\n10\n",
            "the average rate is metadata: six decimals, frames untouched"
        );
    }

    if let Some(path) = fixture("late-start.mp4") {
        let summary =
            detect_video(&path, RunOptions::default()).expect("the late-start fixture decodes");
        assert!(
            summary.decode.source.start_time_seconds.unwrap_or(0.0) > 0.0,
            "the fixture carries a nonzero stream start"
        );
        assert_eq!(
            summary
                .boundaries
                .iter()
                .copied()
                .map(BoundaryFrameIndex::get)
                .collect::<Vec<_>>(),
            vec![3],
            "a late start does not shift frame 0"
        );
    }

    // FPS precedence: average first, nominal second, zero only when the
    // container reports neither. `0.000000` is an unknown rate, not a rate of
    // zero, and 24000/1001 renders as 23.976024.
    for (label, average, nominal, expected) in [
        (
            "average wins",
            Some((24000u32, 1001u32)),
            Some((30u32, 1u32)),
            b"# keyframe format v1\nfps 23.976024\n0\n1\n".to_vec(),
        ),
        (
            "nominal second",
            None,
            Some((1000u32, 1u32)),
            b"# keyframe format v1\nfps 1000.000000\n0\n1\n".to_vec(),
        ),
        (
            "unknown zero",
            None,
            None,
            b"# keyframe format v1\nfps 0.000000\n0\n1\n".to_vec(),
        ),
    ] {
        let output = temporary.path().join(format!("{label}.kf.txt"));
        let summary = summary(9, &[1], source(average, nominal));
        assert_eq!(published(&summary, &output), expected, "{label}");
    }
}

#[test]
fn default_path_appends_suffix() {
    // The default output is the input's full file name plus `.kf.txt`: the
    // extension is never replaced, and a name without one still gets a usable
    // suffix. The directory part is kept.
    for (input, expected) in [
        ("video.mkv", "video.mkv.kf.txt"),
        ("dir/clip.mp4", "dir/clip.mp4.kf.txt"),
        ("dir/no-extension", "dir/no-extension.kf.txt"),
        ("dir/twice.mkv.mkv", "dir/twice.mkv.mkv.kf.txt"),
    ] {
        assert_eq!(
            default_output_path(Path::new(input)),
            PathBuf::from(expected),
            "'{input}' must take the appended suffix"
        );
    }
    let absolute = Path::new(env!("CARGO_MANIFEST_DIR")).join("cut8.mkv");
    assert_eq!(
        default_output_path(&absolute),
        PathBuf::from(format!("{}.kf.txt", absolute.display())),
        "an absolute input keeps its directory"
    );
}

#[test]
fn existing_output_is_untouched() {
    // Re-running, or pointing at someone else's file, never overwrites it: the
    // write fails, the old bytes stay, and no temporary file is left behind.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let output = temporary.path().join("clip.mkv.kf.txt");
    std::fs::write(&output, b"hand-written keyframes\n").expect("the existing output is written");
    let error = write_kf_file(&output, &three_cut_summary())
        .expect_err("an existing output must not be overwritten");
    assert!(
        matches!(error, AppError::OutputWrite { .. }),
        "the refusal is an output-write failure: {error}"
    );
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(
        read(&output),
        b"hand-written keyframes\n",
        "the existing file keeps its bytes"
    );
    assert_eq!(
        entries(temporary.path()),
        vec![OsString::from("clip.mkv.kf.txt")],
        "the refused run leaves no temporary file"
    );
}

#[test]
fn output_equal_to_input_is_rejected() {
    // Writing over the input would destroy the video it came from. The library
    // layer refuses an existing target, so a caller that points `-o` at the
    // input cannot truncate it; the input bytes are provably unchanged.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("clip.mkv");
    std::fs::write(&input, b"video bytes that must survive").expect("the input is written");
    let error = write_kf_file(&input, &three_cut_summary())
        .expect_err("an output that already exists must be refused");
    assert!(
        matches!(error, AppError::OutputWrite { .. }),
        "the refusal is an output-write failure: {error}"
    );
    assert_eq!(
        read(&input),
        b"video bytes that must survive",
        "a refused output must not truncate the input"
    );
    assert_eq!(
        entries(temporary.path()),
        vec![OsString::from("clip.mkv")],
        "the refused run leaves nothing behind"
    );
}

#[test]
fn unicode_and_spaced_path() {
    // Non-ASCII and spaces in the directory and the file name work, including a
    // file name without an extension. The published name is exactly the one
    // requested.
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let dir = temporary.path().join("键帧 dir 测试");
    std::fs::create_dir(&dir).expect("the non-ASCII directory is created");
    let input = dir.join("影片 no extension");
    std::fs::copy(&path, &input).expect("the fixture is copied to the non-ASCII path");
    let summary = detect_video(&input, RunOptions::default())
        .unwrap_or_else(|error| panic!("a non-ASCII input path must decode: {error}"));

    let expected = dir.join("影片 no extension.kf.txt");
    assert_eq!(default_output_path(&input), expected);
    assert_eq!(
        published(&summary, &expected),
        b"# keyframe format v1\nfps 12.000000\n0\n6\n"
    );
    assert_eq!(
        entries(&dir),
        vec![
            OsString::from("影片 no extension"),
            OsString::from("影片 no extension.kf.txt")
        ],
        "the published name is exactly the requested one"
    );
}

#[test]
fn write_failure_publishes_nothing() {
    // Mid-write and flush failures are injected with failing `Write` sinks in
    // the library's own unit tests, where the document builder lives. Through
    // the public API the same guarantee is checked with a publish collision:
    // the target already exists when the publish runs, so the run publishes
    // nothing, leaves the existing bytes alone, and removes its temporary.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let output = temporary.path().join("clip.mkv.kf.txt");
    std::fs::write(&output, b"the other run's keyframes\n")
        .expect("the target exists before the write starts");
    let error = write_kf_file(&output, &three_cut_summary())
        .expect_err("a colliding publish must fail, not overwrite");
    assert!(matches!(error, AppError::OutputWrite { .. }), "{error}");
    assert_eq!(
        read(&output),
        b"the other run's keyframes\n",
        "a failed publish leaves the existing file alone"
    );
    // An empty run publishes nothing either, through the same entry point.
    let second = temporary.path().join("second.mkv.kf.txt");
    let empty = summary(0, &[4], source(None, None));
    write_kf_file(&second, &empty).expect_err("an empty run fails");
    assert!(!second.exists(), "an empty run creates no file");
    assert_eq!(
        entries(temporary.path()),
        vec![OsString::from("clip.mkv.kf.txt")],
        "only the existing published file is in the directory"
    );
}

#[test]
fn a_failed_run_publishes_nothing() {
    // The complete path over a truncated container fails, so the CLI never
    // reaches the writer at all: no file, no temporary, nothing published.
    let Some(path) = fixture("truncated.mkv") else {
        return;
    };
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("truncated.mkv");
    std::fs::copy(&path, &input).expect("the fixture is copied");
    let error = detect_video(&input, RunOptions::default())
        .expect_err("a truncated container is not a complete run");
    assert!(error.to_string().contains("did not complete"), "{error}");
    let output = default_output_path(&input);
    assert!(!output.exists(), "a failed run leaves no keyframe file");
    assert_eq!(
        entries(temporary.path()),
        vec![OsString::from("truncated.mkv")],
        "and no temporary file"
    );
}
