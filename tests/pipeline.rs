//! The complete video processing path, reader to detector.
//!
//! These tests run `wwxds::detect_video` over the decode fixtures, so the
//! borrowed prepared frames, the display-order contract, and the ordered
//! detector state are exercised together on real decoded media rather than on
//! the saved byte blocks of `crates/wwxds-core/tests/detect.rs`. The expected boundary indexes
//! are the `change_frame` values in `scripts/make-m1-fixtures.sh`. They are
//! known by construction: the script verifies each clip's luma mean before and
//! at that frame before writing it.
//!
//! Like the decode tests, these skip (report and return without asserting)
//! when the fixtures are absent, because generating them needs FFmpeg and is a
//! separate documented step. CI generates the fixtures before testing.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use wwxds::error::AppError;
use wwxds::frame_index::BoundaryFrameIndex;
use wwxds::progress::Progress;
use wwxds::{RunOptions, detect_video, detect_video_with_progress};
use wwxds_media::MediaError;

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

/// Runs the full path once and returns the boundary indexes with the summary,
/// asserting the frame index contract on every run.
fn boundaries(name: &str) -> Option<(Vec<u64>, wwxds::DetectSummary)> {
    let path = fixture(name)?;
    let summary = detect_video(&path, RunOptions::default())
        .unwrap_or_else(|error| panic!("{name}: the complete path failed: {error}"));
    let indexes: Vec<u64> = summary
        .boundaries
        .iter()
        .copied()
        .map(BoundaryFrameIndex::get)
        .collect();
    // Frame 0 is never a boundary, and boundaries strictly increase in display
    // order. The type already forbids zero; this pins the whole sequence.
    assert!(
        indexes.iter().all(|index| *index >= 1),
        "{name}: a boundary was reported at frame 0 or below: {indexes:?}"
    );
    for pair in indexes.windows(2) {
        assert!(
            pair[0] < pair[1],
            "{name}: boundaries are not strictly increasing: {indexes:?}"
        );
    }
    assert!(
        indexes.len() <= usize::try_from(summary.decode.decoded_frame_count).unwrap(),
        "{name}: more boundaries than decoded frames"
    );
    Some((indexes, summary))
}

#[test]
fn the_complete_path_reports_the_fixture_hard_cut() {
    // 8-bit FFV1 MKV, luma 32 then 220 from frame 6.
    let Some((indexes, summary)) = boundaries("cut8.mkv") else {
        return;
    };
    assert_eq!(
        summary.decode.decoded_frame_count, 12,
        "cut8.mkv decodes twelve frames"
    );
    assert_eq!(
        indexes,
        vec![6],
        "the cut reports the new shot's first frame"
    );
}

#[test]
fn a_b_frame_input_reports_the_same_boundary_in_display_order() {
    // The MP4 twin carries two B-frames. An encoded-order leak would move or
    // duplicate the boundary; the recipe label is the same frame 6.
    let Some((indexes, summary)) = boundaries("cut8-bframes.mp4") else {
        return;
    };
    assert_eq!(
        summary.decode.decoded_frame_count, 12,
        "the B-frame fixture decodes twelve frames"
    );
    assert_eq!(
        indexes,
        vec![6],
        "B-frame reordering must not change the display-order boundary"
    );
}

#[test]
fn a_ten_bit_input_reports_the_same_boundary() {
    // Same recipe at 10-bit depth, so the documented >> 8 reduction feeds the
    // detector the equivalent 8-bit planes.
    let Some((indexes, _)) = boundaries("cut10.mkv") else {
        return;
    };
    assert_eq!(indexes, vec![6], "the 10-bit cut is reported at frame 6");
}

#[test]
fn odd_and_minimum_dimensions_run_the_complete_path() {
    // 97x65 source (odd) and the 48x48 minimum: both are analyzed natively
    // under the integer scale rule, and both carry the change at frame 3.
    for (name, analysis) in [("odd-97x65.mkv", (97, 65)), ("small-48x48.mkv", (48, 48))] {
        let Some((indexes, summary)) = boundaries(name) else {
            return;
        };
        assert_eq!(indexes, vec![3], "{name}: the cut is reported at frame 3");
        assert_eq!(
            (
                summary.decode.analysis_size.width(),
                summary.decode.analysis_size.height()
            ),
            analysis,
            "{name}: analysis size follows the documented rule"
        );
    }
}

#[test]
fn a_thirty_six_frame_input_finds_one_cut_at_the_recipe_frame() {
    let Some((indexes, summary)) = boundaries("long.mkv") else {
        return;
    };
    assert_eq!(
        summary.decode.decoded_frame_count, 36,
        "long.mkv decodes thirty-six frames"
    );
    assert_eq!(
        indexes,
        vec![18],
        "the single cut of the longer fixture is reported at frame 18"
    );
}

#[test]
fn invalid_incomplete_and_audio_only_inputs_return_an_error_not_a_result() {
    // AGENTS.md: a truncated, damaged, unsupported, or empty input never
    // becomes a complete result with fewer frames, on the detection path either.
    for name in [
        "empty.bin",
        "invalid.bin",
        "truncated.mkv",
        "damaged.mkv",
        "audio-only.m4a",
    ] {
        let Some(path) = fixture(name) else {
            return;
        };
        match detect_video(&path, RunOptions::default()) {
            Err(error) => {
                // The failure is a structured error naming the input path, from
                // any stage: a container that cannot be opened, an unusable
                // stream selection, or an incomplete decode. None of them yields
                // an `Ok` summary.
                let message = error.to_string();
                assert!(
                    message.contains(name),
                    "{name}: the error does not name the input: {message}"
                );
                assert!(
                    matches!(
                        error,
                        AppError::File { .. }
                            | AppError::InvalidInput { .. }
                            | AppError::Media(
                                MediaError::ContainerOpen { .. }
                                    | MediaError::StreamNotFound { .. }
                                    | MediaError::NoVideoStream { .. }
                                    | MediaError::NotVideoStream { .. }
                                    | MediaError::UnsupportedPixelFormat { .. }
                                    | MediaError::IncompleteProcessing { .. }
                            )
                    ),
                    "{name}: unexpected error variant: {message}"
                );
            }
            Ok(summary) => panic!(
                "{name}: the complete path reported {} boundary(s) over {} frames \
                 for an input that must fail",
                summary.boundaries.len(),
                summary.decode.decoded_frame_count
            ),
        }
    }
}

#[test]
fn the_keyframe_file_adds_only_frame_zero() {
    // The output layer and the detector are checked together over the whole
    // fixture list: the published document is the headers, one frame `0`, then
    // exactly the boundaries `detect_video` reported. A file-layer change that
    // moved or duplicated a cut would fail here, not only in the output tests.
    for name in [
        "cut8.mkv",
        "cut8-bframes.mp4",
        "cut10.mkv",
        "odd-97x65.mkv",
        "small-48x48.mkv",
        "long.mkv",
        "vfr.mp4",
        "late-start.mp4",
        "multi-stream.mp4",
    ] {
        let Some(path) = fixture(name) else {
            return;
        };
        let summary = detect_video(&path, RunOptions::default())
            .unwrap_or_else(|error| panic!("{name}: the complete path failed: {error}"));
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("{name}: no temporary directory: {error}"));
        let output = temporary.path().join(format!("{name}.kf.txt"));
        wwxds::keyframes::write_kf_file(&output, &summary)
            .unwrap_or_else(|error| panic!("{name}: the keyframe file failed: {error}"));

        let text = std::fs::read_to_string(&output)
            .unwrap_or_else(|error| panic!("{name}: reading the document failed: {error}"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            summary.boundaries.len() + 3,
            "{name}: unexpected line count: {lines:?}"
        );
        assert_eq!(lines[0], "# keyframe format v1", "{name}: header line");
        assert!(
            lines[1].starts_with("fps "),
            "{name}: fps line: {:?}",
            lines[1]
        );
        assert_eq!(lines[2], "0", "{name}: frame 0 is the file layer's line");
        let from_file: Vec<u64> = lines[3..]
            .iter()
            .map(|line| {
                line.parse::<u64>().unwrap_or_else(|error| {
                    panic!("{name}: '{line}' is not a frame index: {error}")
                })
            })
            .collect();
        let from_detector: Vec<u64> = summary
            .boundaries
            .iter()
            .copied()
            .map(BoundaryFrameIndex::get)
            .collect();
        assert_eq!(
            from_file, from_detector,
            "{name}: the file layer changed the cuts"
        );
    }
}

/// The progress values of one run, in arrival order.
#[derive(Default)]
struct ProgressLog {
    values: RefCell<Vec<Progress>>,
    returned: Cell<bool>,
    callback_after_return: Cell<bool>,
}

impl ProgressLog {
    /// Runs the full path with progress and returns the result.
    ///
    /// `returned` is set the moment `detect_video_with_progress` comes back,
    /// so any callback that fires after the run returned — a leaked thread or
    /// a late flush — is recorded and failed instead of silently accepted.
    fn run(&self, path: &Path) -> Result<wwxds::DetectSummary, AppError> {
        let result = detect_video_with_progress(path, RunOptions::default(), |progress| {
            if self.returned.get() {
                self.callback_after_return.set(true);
            }
            self.values.borrow_mut().push(progress);
        });
        self.returned.set(true);
        assert!(
            !self.callback_after_return.get(),
            "a progress callback fired after detect_video_with_progress returned"
        );
        result
    }
}

#[test]
fn progress_is_monotonic_and_before_return() {
    // The cut8 fixture decodes 12 frames with the cut at frame 6, so the
    // expected keyframe counts are known exactly: 1 while frame 0 is the only
    // keyframe, then 2 from the cut onward.
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let log = ProgressLog::default();
    let summary = log
        .run(&path)
        .unwrap_or_else(|error| panic!("cut8.mkv: the complete path failed: {error}"));

    let values = log.values.borrow();
    assert!(!values.is_empty(), "a twelve-frame run reports progress");
    assert_eq!(
        values.first().unwrap().processed_frames,
        1,
        "the first callback reports frame 1, not frame 0"
    );

    // Frame numbers and keyframe numbers never regress, elapsed never regresses,
    // and the values track the real detector state.
    for pair in values.windows(2) {
        assert!(
            pair[0].processed_frames < pair[1].processed_frames,
            "one callback per completed frame, strictly increasing: {:?}",
            values
                .iter()
                .map(|p| p.processed_frames)
                .collect::<Vec<_>>()
        );
        assert!(
            pair[0].keyframes <= pair[1].keyframes,
            "keyframes must not regress: {:?}",
            values.iter().map(|p| p.keyframes).collect::<Vec<_>>()
        );
        assert!(
            pair[0].elapsed <= pair[1].elapsed,
            "elapsed must not regress"
        );
    }

    // With 12 frames and exactly one boundary at 6 the last callback is the
    // completed picture: all frames, both keyframes (frame 0 plus the cut).
    let last = values.last().unwrap();
    assert_eq!(
        last.processed_frames, summary.stats.frames,
        "the last callback equals the summary frame count"
    );
    assert_eq!(last.processed_frames, 12, "cut8.mkv completes on frame 12");
    assert_eq!(
        last.keyframes,
        u64::try_from(summary.boundaries.len() + 1).expect("one boundary plus frame 0"),
        "the keyframe count includes the output layer's frame 0"
    );
    assert!(
        last.elapsed > std::time::Duration::ZERO,
        "elapsed is the run's real wall time, not a placeholder"
    );
}

#[test]
fn progress_matches_completed_frames() {
    // One callback per completed frame, in lockstep with the detector, over
    // the whole fixture list.
    for name in ["cut8.mkv", "cut10.mkv", "long.mkv", "small-48x48.mkv"] {
        let Some(path) = fixture(name) else {
            return;
        };
        let log = ProgressLog::default();
        let summary = log
            .run(&path)
            .unwrap_or_else(|error| panic!("{name}: the complete path failed: {error}"));

        let values = log.values.borrow();
        let frames = usize::try_from(summary.stats.frames).expect("fixture frame counts are small");
        assert_eq!(
            values.len(),
            frames,
            "{name}: exactly one callback per completed frame"
        );
        for (position, progress) in values.iter().enumerate() {
            assert_eq!(
                progress.processed_frames,
                u64::try_from(position + 1).expect("position fits a frame count"),
                "{name}: callback {position} must report the frame it completed"
            );
        }
        let last = values.last().unwrap();
        assert_eq!(
            last.processed_frames, summary.stats.frames,
            "{name}: the last callback equals summary.stats.frames"
        );
        assert_eq!(
            last.keyframes,
            u64::try_from(summary.boundaries.len() + 1).expect("boundary counts are small"),
            "{name}: final keyframe count is every boundary plus the output layer's frame 0"
        );
    }
}

#[test]
fn a_corrupted_input_reports_progress_and_still_returns_the_error() {
    // A truncated container can show intermediate progress while frames
    // complete, and must still return the decode error rather than a result.
    let Some(path) = fixture("truncated.mkv") else {
        return;
    };
    let log = ProgressLog::default();
    let error = log.run(&path).expect_err("truncated.mkv must fail the run");

    let message = error.to_string();
    assert!(message.contains("did not complete"), "{message}");
    assert!(
        !log.values.borrow().is_empty(),
        "the run decoded frames, so it must have shown intermediate progress: {message}"
    );
    let values = log.values.borrow();
    for pair in values.windows(2) {
        assert!(
            pair[0].processed_frames < pair[1].processed_frames,
            "intermediate progress still never regresses"
        );
    }
}

#[test]
fn detect_video_without_callback_still_completes() {
    // The no-callback wrapper is what bench and (before Task 2's CLI wiring)
    // the caller use: same summary, no progress machinery in the way.
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let summary = detect_video(&path, RunOptions::default())
        .unwrap_or_else(|error| panic!("cut8.mkv: the wrapper failed: {error}"));
    assert_eq!(summary.stats.frames, 12);
    assert_eq!(summary.boundaries.len(), 1);
}
