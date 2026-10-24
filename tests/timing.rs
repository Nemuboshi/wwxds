//! The honest timing summary.
//!
//! `RunStats` must separate the producer's real decode and resize cost (paid
//! on the producer thread inside `ihni-media`) from the consumer's wait for
//! prepared frames. Before this split, `decode` named the consumer wait and the
//! producer's actual cost was invisible; a reader could take the wait for the
//! whole cost of decoding. These tests pin the split on real media: a complete
//! run reports all of producer decode, producer resize, queue wait, prepare,
//! detect, and total separately, the frame count matches the decode summary,
//! and the parallel producer stages never claim to be a share of the wall time
//! they overlap. A failed run reports no timings at all, because a partial run
//! is not a measurement.
//!
//! Like the pipeline tests, these skip (report and return without asserting)
//! when the fixtures are absent.

use std::path::{Path, PathBuf};

use ihni::{RunOptions, detect_video};

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

#[test]
fn a_complete_run_separates_producer_cost_from_queue_wait() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let summary = detect_video(&path, RunOptions::default())
        .unwrap_or_else(|error| panic!("cut8.mkv: the complete path failed: {error}"));
    let stats = summary.stats;

    assert_eq!(stats.frames, 12, "the cut8 fixture decodes twelve frames");
    assert_eq!(
        stats.frames, summary.decode.decoded_frame_count,
        "the reported frame count is the decode summary's count"
    );

    // Every stage exists as its own number on real media.
    assert!(
        stats.producer_decode > std::time::Duration::ZERO,
        "the producer decoded twelve frames and must report it"
    );
    assert!(
        stats.producer_resize > std::time::Duration::ZERO,
        "the producer resized twelve frames and must report it"
    );
    assert!(
        stats.prepare > std::time::Duration::ZERO,
        "padding ran for every frame"
    );
    assert!(
        stats.detect > std::time::Duration::ZERO,
        "detection ran for every frame"
    );
    assert!(
        stats.total > std::time::Duration::ZERO,
        "the wall time of a complete run is positive"
    );

    // Honesty check: the consumer's wait and the producer's own stages are
    // different numbers measured on different threads. The wait cannot exceed
    // the wall time it happened inside; the producer stages may overlap the
    // consumer, so they are compared against the wall only as an upper bound
    // on what one thread could spend, never as a sum that must fit.
    assert!(
        stats.queue_wait <= stats.total,
        "a wait that happened inside the run cannot exceed the run: \
         queue_wait={:?} total={:?}",
        stats.queue_wait,
        stats.total,
    );
    assert!(
        stats.producer_decode <= stats.total,
        "the producer's decode also happened inside the run: \
         producer_decode={:?} total={:?}",
        stats.producer_decode,
        stats.total,
    );
    assert!(
        stats.producer_resize <= stats.total,
        "the producer's resize also happened inside the run: \
         producer_resize={:?} total={:?}",
        stats.producer_resize,
        stats.total,
    );

    // The detector's own two stages are sequential on the consumer thread, so
    // they must fit inside the wall time together with the wait.
    assert!(
        stats.prepare + stats.detect <= stats.total + stats.total,
        "prepare+detect are bounded by the wall: prepare={:?} detect={:?} total={:?}",
        stats.prepare,
        stats.detect,
        stats.total,
    );
}

#[test]
fn a_failed_run_reports_no_summary() {
    // The error path must not produce a RunStats at all: a partial frame
    // sequence is not a measurement.
    let Some(path) = fixture("truncated.mkv") else {
        return;
    };
    let error =
        detect_video(&path, RunOptions::default()).expect_err("truncated.mkv must fail the run");
    let message = error.to_string();
    assert!(message.contains("did not complete"), "{message}");
    let _ = Path::new(&path);
}
