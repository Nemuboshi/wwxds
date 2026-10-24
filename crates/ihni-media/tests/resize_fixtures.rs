//! Offline regression of the production resize path against recorded swscale
//! output.
//!
//! `tests/fixtures/resize/` holds 13 source planes and the exact `gray16le`
//! planes libswscale recorded once for them, through the very configuration
//! `FrameResizer` uses (gray source, `gray16le` destination, `SWS_BICUBIC`
//! defaults, 64-byte-aligned destination stride). The files are frozen
//! regression data, like the committed digest lists: the harness that recorded
//! them (`examples/scale_fixtures`) and the Rust scaler it replicated
//! (`ihni-scale`) were removed with task 5, so nothing in the repository
//! regenerates these bytes. A mismatch means the linked swscale or the resize
//! path changed the analysis bytes — a review event, not a routine update.
//!
//! The production path is public only as the 8-bit analysis plane, so the
//! comparison checks that plane against the high byte of every recorded
//! 16-bit sample — the reduction `FrameResizer` performs is exactly `>> 8`,
//! so agreement here means the same bytes the detector sees are unchanged.
//! The recorded low-byte precision has no production consumer any more; the
//! full `gray16le` planes stay committed so the recorded signal is preserved
//! byte-for-byte and can be re-derived from if a future path exposes it.
//!
//! The cases cover upsampling, downsampling, odd sizes, constant planes,
//! identity geometry, and 10-bit in both endiannesses, over padded source
//! strides. It needs no FFmpeg, so it runs in CI on every checkout.

use std::fs;
use std::path::PathBuf;

use ihni_media::LumaFormat;
use ihni_media::decode::FrameResizer;

/// One recorded case, as `index.txt` describes it.
struct Case {
    name: String,
    format: LumaFormat,
    source_width: u32,
    source_height: u32,
    source_stride: usize,
    target_width: u32,
    target_height: u32,
    target_stride: usize,
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resize")
}

fn format_named(name: &str) -> LumaFormat {
    match name {
        "gray8" => LumaFormat::Gray8,
        "gray10le" => LumaFormat::Gray10Le,
        "gray10be" => LumaFormat::Gray10Be,
        other => panic!("the fixture index names an unknown format: {other}"),
    }
}

fn parse_index() -> Vec<Case> {
    let path = fixture_directory().join("index.txt");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    let mut cases = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 8, "malformed index line: {line}");
        let number = |index: usize| -> u32 {
            fields[index]
                .parse()
                .unwrap_or_else(|error| panic!("malformed index line '{line}': {error}"))
        };
        cases.push(Case {
            name: fields[0].to_owned(),
            format: format_named(fields[1]),
            source_width: number(2),
            source_height: number(3),
            source_stride: number(4) as usize,
            target_width: number(5),
            target_height: number(6),
            target_stride: number(7) as usize,
        });
    }
    assert!(!cases.is_empty(), "the fixture index is empty");
    cases
}

/// Reports every differing offset of one plane, capped so a wrong resize does
/// not produce a million-line failure message.
fn compare(expected: &[u8], actual: &[u8], label: &str) {
    assert_eq!(
        expected.len(),
        actual.len(),
        "{label}: the planes have different lengths"
    );

    let mut differing = 0usize;
    let mut first = None;
    for (offset, (left, right)) in expected.iter().zip(actual).enumerate() {
        if left != right {
            differing += 1;
            if first.is_none() {
                first = Some((offset, *left, *right));
            }
        }
    }

    if let Some((offset, left, right)) = first {
        panic!(
            "{label}: {differing} of {} bytes differ; first at offset {offset}: \
             recorded {left:#04x}, production {right:#04x}",
            expected.len()
        );
    }
}

#[test]
fn production_resize_reproduces_the_recorded_swscale_bytes() {
    for case in parse_index() {
        let source_path = fixture_directory().join(format!("{}.in", case.name));
        let target_path = fixture_directory().join(format!("{}.out", case.name));
        let source = fs::read(&source_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));
        let recorded = fs::read(&target_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", target_path.display()));

        let expected_pixels = (case.target_width as usize) * (case.target_height as usize);
        let row_bytes = case.target_width as usize * 2;
        assert_eq!(
            recorded.len(),
            case.target_stride * case.target_height as usize,
            "{}: the recorded plane does not match the index geometry",
            case.name
        );

        // Only the pixels of each row are part of the recorded result: the
        // destination stride is the 64-byte-aligned one a real libav caller
        // passes, and swscale's SIMD output also writes into that padding
        // (docs/Architecture.md, "Measurement conditions").
        let expected: Vec<u8> = recorded
            .chunks_exact(case.target_stride)
            .flat_map(|row| {
                row[..row_bytes]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|sample| sample[1])
            })
            .collect();
        assert_eq!(
            expected.len(),
            expected_pixels,
            "{}: index mismatch",
            case.name
        );

        let mut resizer = FrameResizer::open(
            case.source_width,
            case.source_height,
            case.format,
            case.target_width,
            case.target_height,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let mut actual = vec![0u8; expected_pixels];
        resizer
            .resize(&source, case.source_stride, &mut actual)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        compare(&expected, &actual, &case.name);
    }
}
