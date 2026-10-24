//! Overlapped decoding: the producer/consumer frame stream.
//!
//! [`wwxds_media::decode::FrameStream`] decodes ahead of the caller on a producer
//! thread. The detector consumes frames one at a time in display order either
//! way, so the overlap must not be observable in the frame sequence: the same
//! frames, indexes, derived times, and luma bytes, whatever the slot count.
//! These tests pin that equivalence against the serial [`FrameReader`], and pin
//! the two failure rules that matter for an overlap: an incomplete input stays
//! an error rather than a clean end, and dropping the stream early releases the
//! decoder instead of draining the rest of the file.
//!
//! Like the other decode tests, these skip (report and return without asserting)
//! when the fixtures are absent, because generating them needs FFmpeg and is a
//! separate documented step. CI generates the fixtures before testing.

use std::path::{Path, PathBuf};

use wwxds_media::decode::{
    FrameRate, FrameReader, FrameStream, ReadOutcome, ReaderOptions, TimeMode,
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

/// One frame's observable content, copied out of its reusable buffer.
struct Plane {
    index: u64,
    seconds: Option<f64>,
    luma: Vec<u8>,
}

fn options(time_mode: TimeMode, read_ahead: usize) -> ReaderOptions {
    ReaderOptions {
        stream_index: None,
        time_mode,
        read_ahead,
        ..ReaderOptions::default()
    }
}

/// Reads a whole fixture through the serial reader.
fn read_serially(path: &Path, options: ReaderOptions) -> Vec<Plane> {
    let mut reader = FrameReader::open(path, options).expect("the fixture opens");
    let mut frames = Vec::new();
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        frames.push(Plane {
            index: frame.index.get(),
            seconds: frame.derived_seconds,
            luma: frame.luma().to_vec(),
        });
    }
    reader.finish().expect("the fixture decodes completely");
    frames
}

/// Reads a whole fixture through the overlapped stream, recycling every buffer
/// as the caller of the library does.
fn read_through_the_stream(path: &Path, options: ReaderOptions) -> Vec<Plane> {
    let mut stream = FrameStream::open(path, options).expect("the fixture opens");
    let mut frames = Vec::new();
    while let Some(frame) = stream.next_frame().expect("a complete frame is read") {
        frames.push(Plane {
            index: frame.index().get(),
            seconds: frame.derived_seconds(),
            luma: frame.luma().to_vec(),
        });
        stream.recycle(frame);
    }
    frames
}

/// Asserts two frame sequences are identical, reporting where they diverge.
///
/// Comparing whole planes with `assert_eq!` would print megabytes of samples on
/// failure, so the luma compare reports the first differing byte instead.
fn assert_same(label: &str, expected: &[Plane], actual: &[Plane]) {
    assert_eq!(expected.len(), actual.len(), "{label}: frame count");
    for (position, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        assert_eq!(
            expected.index, actual.index,
            "{label}: frame {position} index"
        );
        assert_eq!(
            expected.seconds, actual.seconds,
            "{label}: frame {position} derived seconds"
        );
        if expected.luma != actual.luma {
            let first = expected
                .luma
                .iter()
                .zip(&actual.luma)
                .position(|(a, b)| a != b);
            let differing = expected
                .luma
                .iter()
                .zip(&actual.luma)
                .filter(|(a, b)| a != b)
                .count();
            panic!(
                "{label}: frame {position} luma differs first at byte {first:?}, \
                 {differing} of {} bytes differ",
                expected.luma.len()
            );
        }
    }
}

#[test]
fn every_slot_count_returns_the_serial_frame_sequence() {
    // A B-frame input is decoded out of order and reordered into display order,
    // so it is the case where a producer that sent frames out of order would
    // show.
    for name in ["long.mkv", "cut8-bframes.mp4"] {
        let Some(path) = fixture(name) else {
            return;
        };
        let expected = read_serially(&path, options(TimeMode::Unavailable, 1));
        assert!(
            !expected.is_empty(),
            "{name}: the fixture decoded no frames"
        );
        for read_ahead in [1, 2, 3, 4] {
            let actual = read_through_the_stream(&path, options(TimeMode::Unavailable, read_ahead));
            assert_same(
                &format!("{name} with read_ahead {read_ahead}"),
                &expected,
                &actual,
            );
        }
    }
}

#[test]
fn the_stream_frames_are_the_documented_display_order() {
    let Some(path) = fixture("cut8-bframes.mp4") else {
        return;
    };

    // The MP4 twin carries B-frames. A producer that forwarded decode order
    // would reorder or duplicate these contiguous display indexes.
    let frames = read_through_the_stream(&path, options(TimeMode::Unavailable, 3));
    let indexes: Vec<u64> = frames.iter().map(|frame| frame.index).collect();
    assert_eq!(indexes, (0..12).collect::<Vec<u64>>());
}

#[test]
fn derived_time_survives_the_overlap() {
    let Some(path) = fixture("late-start.mp4") else {
        return;
    };

    // The source starts at 2.0 s. Derived time starts at zero for frame 0 and
    // never includes the source start, on the stream path as on the serial one.
    let rate = FrameRate::new(10, 1).expect("10/1 is a valid rate");
    let mode = TimeMode::DerivedCfr { rate };
    let expected = read_serially(&path, options(mode, 1));
    let actual = read_through_the_stream(&path, options(mode, 2));

    assert_same("late-start.mp4", &expected, &actual);
    assert_eq!(actual[0].seconds, Some(0.0), "frame 0 starts at zero");
    assert_eq!(actual[4].seconds, Some(0.4), "frame 4 is four tenths");
}

/// Reads until the stream ends or fails, and returns the failure message.
///
/// A stream that reports a clean end for an incomplete input panics here: that
/// is the mistake this helper exists to catch.
fn stream_failure(path: &Path, options: ReaderOptions) -> String {
    let mut stream = match FrameStream::open(path, options) {
        Ok(stream) => stream,
        // Probing or starting the decoder can fail before a thread exists, which
        // is still a failure and never a result.
        Err(error) => return error.to_string(),
    };
    loop {
        match stream.next_frame() {
            Ok(Some(frame)) => stream.recycle(frame),
            Ok(None) => panic!(
                "{}: the stream reported a clean end for an input that must fail",
                path.display()
            ),
            Err(error) => return error.to_string(),
        }
    }
}

#[test]
fn an_incomplete_input_fails_instead_of_ending_cleanly() {
    for name in ["truncated.mkv", "damaged.mkv", "invalid.bin", "empty.bin"] {
        let Some(path) = fixture(name) else {
            return;
        };
        let message = stream_failure(&path, options(TimeMode::Unavailable, 2));
        assert!(
            message.contains(name),
            "{name}: the failure does not name the input: {message}"
        );
    }
}

#[test]
fn an_audio_only_input_fails_without_starting_a_producer() {
    let Some(path) = fixture("audio-only.m4a") else {
        return;
    };

    let message = match FrameStream::open(&path, options(TimeMode::Unavailable, 2)) {
        Ok(_) => panic!("an audio-only input should not open as a video stream"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("no video stream"),
        "unexpected message: {message}"
    );
}

#[test]
fn reading_past_the_end_is_an_error_not_a_second_end() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };

    let mut stream =
        FrameStream::open(&path, options(TimeMode::Unavailable, 2)).expect("the fixture opens");
    let mut count = 0;
    while let Some(frame) = stream.next_frame().expect("a complete frame is read") {
        count += 1;
        stream.recycle(frame);
    }
    assert_eq!(count, 12, "cut8.mkv decodes twelve frames");

    // The end was reported once. Asking again is a misuse, and it must not look
    // like another end of stream.
    let error = match stream.next_frame() {
        Ok(_) => panic!("a read after the end of the stream should fail"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("after the end"),
        "unexpected message: {error}"
    );
}

#[test]
fn an_early_drop_releases_the_decoder() {
    let Some(path) = fixture("long.mkv") else {
        return;
    };

    {
        let mut stream =
            FrameStream::open(&path, options(TimeMode::Unavailable, 3)).expect("the fixture opens");
        for _ in 0..3 {
            let frame = stream
                .next_frame()
                .expect("a complete frame is read")
                .expect("the fixture is longer than three frames");
            stream.recycle(frame);
        }
        // Dropping the stream here must join the producer and let the reader
        // kill the decoder child, rather than draining the remaining 33 frames.
    }

    // The same input opens and decodes completely straight afterwards, so the
    // dropped stream left no decoder behind.
    let frames = read_serially(&path, options(TimeMode::Unavailable, 1));
    assert_eq!(frames.len(), 36, "long.mkv decodes thirty-six frames");
}
