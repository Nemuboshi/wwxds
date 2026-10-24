//! Producer-side timing plumbing.
//!
//! The pipeline summary must tell the truth about where time goes: decoding and
//! resizing happen on the producer thread, and the consumer's time is the wait
//! for prepared frames, not the cost of decoding. These tests pin the three
//! producer-side contracts the split rests on:
//!
//! 1. [`FrameReader::producer_timings`] reports the decode and resize cost of
//!    the frames it has read, and reports none before any frame is read.
//! 2. [`FrameStream::producer_timings`] reports what its producer thread
//!    measured, once the stream has run to its end.
//! 3. The frame sequence the stream delivers is the sequence the serial reader
//!    delivers with the same options; the timing work must not disturb it.
//!
//! Like the other decode tests, these skip (report and return without
//! asserting) when the fixtures are absent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use wwxds_media::decode::{
    FrameReader, FrameStream, ProducerTimings, ReadOutcome, ReaderOptions, TimeMode,
};

fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/m1-fixtures")
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

fn options(read_ahead: usize) -> ReaderOptions {
    ReaderOptions {
        stream_index: None,
        time_mode: TimeMode::Unavailable,
        read_ahead,
        ..ReaderOptions::default()
    }
}

#[test]
fn a_fresh_reader_reports_no_producer_time() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let reader = FrameReader::open(&path, options(1)).expect("the fixture opens");
    assert_eq!(
        reader.producer_timings(),
        ProducerTimings {
            decode: Duration::ZERO,
            resize: Duration::ZERO,
        },
        "no frame has been read, so no producer stage has run"
    );
}

#[test]
fn a_serial_reader_accumulates_decode_and_resize_over_its_frames() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let mut reader = FrameReader::open(&path, options(1)).expect("the fixture opens");
    let mut frames = 0u64;
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        frames += 1;
    }
    reader.finish().expect("the fixture decodes completely");
    assert_eq!(frames, 12, "the cut8 fixture decodes twelve frames");

    let timings = reader.producer_timings();
    assert!(
        timings.decode > Duration::ZERO,
        "twelve decoded frames must report a nonzero decode cost"
    );
    assert!(
        timings.resize > Duration::ZERO,
        "twelve resized frames must report a nonzero resize cost"
    );
}

#[test]
fn the_producer_thread_reports_its_timings_through_the_stream() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let mut stream = FrameStream::open(&path, options(2)).expect("the fixture opens");
    while let Some(frame) = stream.next_frame().expect("a complete frame is read") {
        stream.recycle(frame);
    }

    let timings = stream
        .producer_timings()
        .expect("a stream that ran to its end reports producer timings");
    assert!(
        timings.decode > Duration::ZERO,
        "the producer decoded frames, so it must report decode cost: {timings:?}"
    );
    assert!(
        timings.resize > Duration::ZERO,
        "the producer resized frames, so it must report resize cost: {timings:?}"
    );
}

#[test]
fn timing_collection_keeps_the_frame_sequence_intact() {
    // The same fixture read serially and through the stream must deliver the
    // same frame bytes; collecting timings may not change what is produced.
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };
    let mut reader = FrameReader::open(&path, options(1)).expect("the fixture opens");
    let mut serial: Vec<Vec<u8>> = Vec::new();
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        serial.push(
            reader
                .frame()
                .expect("a complete frame has a view")
                .luma()
                .to_vec(),
        );
    }
    reader.finish().expect("the fixture decodes completely");

    let mut stream = FrameStream::open(&path, options(2)).expect("the fixture opens");
    let mut overlapped: Vec<Vec<u8>> = Vec::new();
    while let Some(frame) = stream.next_frame().expect("a complete frame is read") {
        overlapped.push(frame.luma().to_vec());
        stream.recycle(frame);
    }

    assert_eq!(
        serial.len(),
        overlapped.len(),
        "the same frame count either way"
    );
    for (index, (expected, actual)) in serial.iter().zip(&overlapped).enumerate() {
        assert_eq!(
            expected, actual,
            "frame {index} luma bytes differ between serial and overlapped"
        );
    }
    // Both readers must still report a real producer cost for their own frames.
    assert!(reader.producer_timings().decode > Duration::ZERO);
    assert!(stream.producer_timings().expect("stream timings").decode > Duration::ZERO);
}
