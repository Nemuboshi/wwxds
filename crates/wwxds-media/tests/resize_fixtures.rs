//! Offline regression of the production analysis-plane filter against
//! committed output planes.
//!
//! `tests/fixtures/resize/` holds 13 source planes and the exact analysis
//! planes `FrameResizer` produces for them — the integer box filter of
//! `docs/Architecture.md` §4. The committed generator
//! (`examples/gen_resize_fixtures`) is the only thing that writes the `.out`
//! planes: a filter change regenerates them through it, and the new planes
//! are committed together with the change. A mismatch without such a change
//! means the filter or the resize path changed the analysis bytes — a review
//! event, not a routine update.
//!
//! The cases cover upsampling, downsampling, odd sizes, constant planes,
//! identity geometry, and 10-bit in both endiannesses, over padded source
//! strides. It needs no FFmpeg, so it runs in CI on every checkout.

use std::fs;
use std::path::PathBuf;

use wwxds_media::LumaFormat;
use wwxds_media::decode::FrameResizer;

/// One recorded case, as `index.txt` describes it.
struct Case {
    name: String,
    format: LumaFormat,
    source_width: u32,
    source_height: u32,
    source_stride: usize,
    target_width: u32,
    target_height: u32,
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
        assert_eq!(fields.len(), 7, "malformed index line: {line}");
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
fn production_resize_reproduces_the_recorded_filter_bytes() {
    for case in parse_index() {
        let source_path = fixture_directory().join(format!("{}.in", case.name));
        let target_path = fixture_directory().join(format!("{}.out", case.name));
        let source = fs::read(&source_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));
        let expected = fs::read(&target_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", target_path.display()));

        let expected_pixels = (case.target_width as usize) * (case.target_height as usize);
        assert_eq!(
            expected.len(),
            expected_pixels,
            "{}: the recorded plane does not match the index geometry",
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
