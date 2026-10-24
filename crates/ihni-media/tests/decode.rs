//! Decode and preparation tests.
//!
//! These tests read the fixtures written by `scripts/make-m1-fixtures.sh`.
//! When that script has not been run, the tests report the missing fixture and
//! are skipped rather than failing, because fixture generation needs FFmpeg and
//! is a separate documented step.

use std::path::{Path, PathBuf};

use ihni_media::decode::{
    FrameRate, FrameReader, LumaDepth, ReadOutcome, ReaderOptions, TimeMode, mean_luma,
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

/// Reads a whole fixture and returns per-frame index, mean luma, and derived time.
struct ReadFrames {
    indexes: Vec<u64>,
    means: Vec<f64>,
    seconds: Vec<Option<f64>>,
    depth: LumaDepth,
}

fn read_frames(path: &Path, time_mode: TimeMode) -> ReadFrames {
    let mut reader = FrameReader::open(
        path,
        ReaderOptions {
            stream_index: None,
            time_mode,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens as a supported video");

    let mut result = ReadFrames {
        indexes: Vec::new(),
        means: Vec::new(),
        seconds: Vec::new(),
        depth: reader.source().depth,
    };

    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        result.indexes.push(frame.index.get());
        result.means.push(mean_luma(frame.luma()));
        result.seconds.push(frame.derived_seconds);
    }

    reader.finish().expect("the fixture decodes completely");
    result
}

fn close_to(value: f64, expected: f64) -> bool {
    (value - expected).abs() < 0.01
}

/// Asserts that opening a fixture fails, and returns the error message.
///
/// `FrameReader` owns a decoder handle and is not `Debug`, so the result is
/// matched rather than unwrapped with `expect_err`.
fn open_error(path: &Path, options: ReaderOptions) -> String {
    match FrameReader::open(path, options) {
        Ok(_) => panic!("opening {} should have failed", path.display()),
        Err(error) => error.to_string(),
    }
}

/// Decodes a whole fixture, expecting it to fail, and returns the error message.
fn decode_error(path: &Path, options: ReaderOptions) -> String {
    let mut reader = match FrameReader::open(path, options) {
        Ok(reader) => reader,
        Err(error) => return error.to_string(),
    };
    loop {
        match reader.next_frame() {
            Ok(ReadOutcome::Frame) => continue,
            Ok(ReadOutcome::EndOfStream) => match reader.finish() {
                Ok(()) => panic!("{} should not decode completely", path.display()),
                Err(error) => return error.to_string(),
            },
            Err(error) => return error.to_string(),
        }
    }
}

#[test]
fn mp4_and_mkv_return_the_fixture_frame_counts() {
    let Some(mp4) = fixture("cut8-bframes.mp4") else {
        return;
    };
    let Some(mkv) = fixture("cut8.mkv") else {
        return;
    };

    for path in [&mp4, &mkv] {
        let frames = read_frames(path, TimeMode::Unavailable);
        assert_eq!(
            frames.indexes.len(),
            12,
            "{} returns the recipe frame count",
            path.display()
        );
    }
}

#[test]
fn b_frame_input_arrives_in_display_order() {
    let Some(path) = fixture("cut8-bframes.mp4") else {
        return;
    };

    let frames = read_frames(&path, TimeMode::Unavailable);

    // Display order means contiguous zero-based indexes with the luma pattern
    // in presentation order. The fixture is six dark frames then six bright
    // ones, and the MP4 carries B-frames, so a decode-order bug would place a
    // bright frame among the dark ones.
    assert_eq!(frames.indexes, (0..12).collect::<Vec<u64>>());
    for (index, mean) in frames.means.iter().enumerate().take(6) {
        assert!(
            close_to(*mean, 32.0),
            "frame {index} should be the first pattern, read {mean}"
        );
    }
    for (index, mean) in frames.means.iter().enumerate().skip(6) {
        assert!(
            close_to(*mean, 220.0),
            "frame {index} should be the second pattern, read {mean}"
        );
    }
}

#[test]
fn analysis_size_follows_the_documented_rule() {
    let cases = [
        ("cut8.mkv", 540, 360),
        ("odd-97x65.mkv", 536, 360),
        ("small-48x48.mkv", 360, 360),
        ("vfr.mp4", 480, 360),
    ];

    for (name, width, height) in cases {
        let Some(path) = fixture(name) else {
            continue;
        };
        let reader = FrameReader::open(
            &path,
            ReaderOptions {
                stream_index: None,
                time_mode: TimeMode::Unavailable,
                ..ReaderOptions::default()
            },
        )
        .expect("the fixture opens");
        let size = reader.analysis_size();
        assert_eq!(
            (size.width(), size.height()),
            (width, height),
            "{name} analysis size"
        );
    }
}

#[test]
fn ten_bit_luma_follows_the_documented_reduction() {
    let Some(path) = fixture("cut10.mkv") else {
        return;
    };

    let frames = read_frames(&path, TimeMode::Unavailable);
    assert_eq!(frames.depth, LumaDepth::Ten);
    assert_eq!(frames.indexes.len(), 12);

    // The fixture uses 10-bit luma 128 then 880, which is the documented
    // equivalent of 8-bit 32 then 220. The reduction must reach those values
    // exactly, not a dithered neighbour.
    for (index, mean) in frames.means.iter().enumerate().take(6) {
        assert!(
            close_to(*mean, 32.0),
            "10-bit frame {index} should reduce to 32, read {mean}"
        );
    }
    for (index, mean) in frames.means.iter().enumerate().skip(6) {
        assert!(
            close_to(*mean, 220.0),
            "10-bit frame {index} should reduce to 220, read {mean}"
        );
    }
}

#[test]
fn eight_bit_luma_is_preserved_exactly() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };

    let frames = read_frames(&path, TimeMode::Unavailable);
    assert_eq!(frames.depth, LumaDepth::Eight);
    // The fixture source is a flat 32 then a flat 220. Preserving the numeric
    // value means the mean is exactly that value, not a range-expanded 19.
    assert!(close_to(frames.means[0], 32.0), "read {}", frames.means[0]);
    assert!(
        close_to(frames.means[11], 220.0),
        "read {}",
        frames.means[11]
    );
}

#[test]
fn unavailable_mode_never_invents_time() {
    let Some(path) = fixture("vfr.mp4") else {
        return;
    };

    let frames = read_frames(&path, TimeMode::Unavailable);
    assert_eq!(frames.indexes.len(), 20);
    assert!(
        frames.seconds.iter().all(Option::is_none),
        "unavailable mode reports no time for variable frame rate input"
    );
}

#[test]
fn derived_cfr_uses_the_index_and_declared_rate() {
    let Some(path) = fixture("late-start.mp4") else {
        return;
    };

    // The fixture starts at source time 2.0 seconds. Derived time must start at
    // zero for frame 0 and never include the source start time.
    let rate = FrameRate::new(10, 1).expect("10/1 is a valid rate");
    let frames = read_frames(&path, TimeMode::DerivedCfr { rate });

    assert_eq!(frames.indexes.len(), 5);
    assert_eq!(frames.seconds[0], Some(0.0), "frame 0 starts at zero");
    assert_eq!(frames.seconds[4], Some(0.4), "frame 4 is four tenths");
}

#[test]
fn a_fractional_rate_derives_exact_elapsed_time() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };

    let rate = FrameRate::new(30000, 1001).expect("30000/1001 is a valid rate");
    let frames = read_frames(&path, TimeMode::DerivedCfr { rate });

    let expected = 6.0 * 1001.0 / 30000.0;
    let actual = frames.seconds[6].expect("derived mode reports time");
    assert!(
        (actual - expected).abs() < 1e-9,
        "frame 6 derived time was {actual}, expected {expected}"
    );
}

#[test]
fn reading_the_same_input_twice_is_identical() {
    let Some(path) = fixture("long.mkv") else {
        return;
    };

    let first = read_frames(&path, TimeMode::Unavailable);
    let second = read_frames(&path, TimeMode::Unavailable);

    assert_eq!(first.indexes, second.indexes);
    assert_eq!(first.means, second.means);
}

#[test]
fn a_truncated_container_is_incomplete_not_a_short_result() {
    let Some(path) = fixture("truncated.mkv") else {
        return;
    };

    let message = decode_error(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    );

    assert!(
        message.contains("did not complete"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_damaged_container_is_incomplete() {
    let Some(path) = fixture("damaged.mkv") else {
        return;
    };

    let message = decode_error(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    );

    assert!(
        message.contains("did not complete"),
        "unexpected message: {message}"
    );
}

#[test]
fn an_audio_only_input_has_no_video_stream() {
    let Some(path) = fixture("audio-only.m4a") else {
        return;
    };

    let message = open_error(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    );

    assert!(
        message.contains("no video stream"),
        "unexpected message: {message}"
    );
}

#[test]
fn a_non_video_absolute_index_is_rejected() {
    let Some(path) = fixture("multi-stream.mp4") else {
        return;
    };

    let message = open_error(
        &path,
        ReaderOptions {
            stream_index: Some(1),
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    );

    assert!(
        message.contains("not a video stream"),
        "unexpected message: {message}"
    );
}

#[test]
fn an_absolute_video_index_selects_that_stream() {
    let Some(path) = fixture("multi-stream.mp4") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: Some(0),
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("stream 0 is video");

    assert_eq!(reader.source().stream_index, 0);
    let mut count = 0;
    while let ReadOutcome::Frame = reader.next_frame().expect("a frame is read") {
        count += 1;
    }
    reader.finish().expect("the stream completes");
    assert_eq!(count, 5);
}

#[test]
fn an_absent_stream_index_is_reported() {
    let Some(path) = fixture("multi-stream.mp4") else {
        return;
    };

    let message = open_error(
        &path,
        ReaderOptions {
            stream_index: Some(9),
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    );

    assert!(
        message.contains("no stream with absolute index 9"),
        "unexpected message: {message}"
    );
}

#[test]
fn decoder_pts_reports_monotonic_source_time_on_a_b_frame_input() {
    // A B-frame input is decoded out of order, so this also proves the stamps
    // follow presentation order rather than decode order.
    let Some(path) = fixture("cut8-bframes.mp4") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::DecoderPts,
            ..ReaderOptions::default()
        },
    )
    .expect("cut8-bframes.mp4 opens as a supported video");
    let declared = reader.source().frame_rate;

    let mut steps = Vec::new();
    let mut previous: Option<f64> = None;
    let mut count = 0u64;
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        // The mode reports source time and never a derived value: the two are
        // separate fields so one cannot be mistaken for the other.
        assert!(
            frame.derived_seconds.is_none(),
            "frame {} carries a derived time in decoder_pts mode",
            frame.index.get()
        );
        let seconds = frame
            .source_seconds
            .unwrap_or_else(|| panic!("frame {} has no source time stamp", frame.index.get()));
        assert!(
            seconds.is_finite(),
            "frame {} reports {seconds}",
            frame.index.get()
        );
        if let Some(previous) = previous {
            assert!(
                seconds > previous,
                "frame {} reports {seconds}, which is not after {previous}",
                frame.index.get()
            );
            steps.push(seconds - previous);
        }
        previous = Some(seconds);
        count += 1;
    }
    reader.finish().expect("the fixture decodes completely");
    assert_eq!(count, 12, "the fixture holds 12 frames");

    // Every stamp is a whole number of stream time base ticks, so consecutive
    // steps of a constant-rate input repeat. Comparing a step against the
    // declared rate ties the stamps to the container's own time base instead of
    // to anything this crate computes.
    if let Some(rate) = declared {
        let expected = f64::from(rate.denominator()) / f64::from(rate.numerator());
        for step in &steps {
            assert!(
                (step - expected).abs() < 1e-6,
                "a step of {step} does not match the declared rate {rate}"
            );
        }
    }
}

#[test]
fn the_default_mode_reports_no_source_time() {
    let Some(path) = fixture("cut8.mkv") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("cut8.mkv opens as a supported video");

    let mut count = 0u64;
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        assert_eq!(
            frame.source_seconds,
            None,
            "frame {} reports a source time in the default mode",
            frame.index.get()
        );
        count += 1;
    }
    reader.finish().expect("the fixture decodes completely");
    assert_eq!(count, 12);
}

#[test]
fn early_exit_releases_the_decoder() {
    let Some(path) = fixture("long.mkv") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens");

    for _ in 0..3 {
        assert_eq!(
            reader.next_frame().expect("a frame is read"),
            ReadOutcome::Frame
        );
    }
    reader.close().expect("early termination succeeds");
}

#[test]
fn resize_keeps_pts_and_display_order() {
    // The resized plane must stay tied to the display index and the source
    // presentation stamp: B-frame input is decoded out of order, so a resized
    // plane that followed decode order would break both the index sequence and
    // the monotonic stamps. The luma pattern proves which plane belongs to
    // which stamp.
    let Some(path) = fixture("cut8-bframes.mp4") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::DecoderPts,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens as a supported video");

    let mut indexes = Vec::new();
    let mut seconds = Vec::new();
    let mut means = Vec::new();
    while let ReadOutcome::Frame = reader.next_frame().expect("a complete frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        indexes.push(frame.index.get());
        seconds.push(
            frame
                .source_seconds
                .unwrap_or_else(|| panic!("frame {} has no source time stamp", frame.index.get())),
        );
        means.push(mean_luma(frame.luma()));
    }
    reader.finish().expect("the fixture decodes completely");

    assert_eq!(
        indexes,
        (0..12).collect::<Vec<u64>>(),
        "resized frames keep the display-order index"
    );
    for pair in seconds.windows(2) {
        assert!(
            pair[1] > pair[0],
            "source stamps are not monotonic in display order: {pair:?}"
        );
    }
    for (index, mean) in means.iter().enumerate().take(6) {
        assert!(
            close_to(*mean, 32.0),
            "frame {index} should be the dark pattern after the resize, read {mean}"
        );
    }
    for (index, mean) in means.iter().enumerate().skip(6) {
        assert!(
            close_to(*mean, 220.0),
            "frame {index} should be the bright pattern after the resize, read {mean}"
        );
    }
}

#[test]
fn midstream_geometry_change_fails() {
    // The concat fixture starts at 96x64 and switches to 64x48 at frame 6.
    // A scaler built for the first size must not be used for the second one.
    let Some(path) = fixture("mixed-size.mp4") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens as a supported video");

    for index in 0..6 {
        assert_eq!(
            reader.next_frame().expect("a frame is read"),
            ReadOutcome::Frame,
            "frame {index} before the size change"
        );
    }

    let error = reader
        .next_frame()
        .expect_err("the frame after the size change must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("size changed") && message.contains("64x48"),
        "unexpected rejection message: {message}"
    );
}

#[test]
fn midstream_pixel_format_change_fails() {
    // The concat fixture starts at yuv420p and switches to yuv420p10le at
    // frame 6, so the decoded luma format changes midstream while the size
    // stays 96x64. Feeding the deeper plane to the scaler built for the first
    // format would silently reinterpret every sample, so it must be rejected.
    let Some(path) = fixture("mixed-depth.mp4") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens as a supported video");

    for index in 0..6 {
        assert_eq!(
            reader.next_frame().expect("a frame is read"),
            ReadOutcome::Frame,
            "frame {index} before the format change"
        );
    }

    let error = reader
        .next_frame()
        .expect_err("the frame after the format change must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("pixel format changed") && message.contains("gray10le"),
        "unexpected rejection message: {message}"
    );
}

#[test]
fn frame_buffers_stay_bounded_across_a_long_input() {
    let Some(path) = fixture("long.mkv") else {
        return;
    };

    let mut reader = FrameReader::open(
        &path,
        ReaderOptions {
            stream_index: None,
            time_mode: TimeMode::Unavailable,
            ..ReaderOptions::default()
        },
    )
    .expect("the fixture opens");

    let expected_bytes = reader.analysis_size().luma_bytes();
    let mut count = 0;
    while let ReadOutcome::Frame = reader.next_frame().expect("a frame is read") {
        let frame = reader.frame().expect("a complete frame has a view");
        // One analysis plane per frame, reused. The luma length never grows
        // with the number of frames read.
        assert_eq!(frame.luma().len(), expected_bytes);
        assert_eq!(frame.luma().len(), frame.stride * frame.height as usize);
        count += 1;
    }
    reader.finish().expect("the stream completes");
    assert_eq!(count, 36);
}
