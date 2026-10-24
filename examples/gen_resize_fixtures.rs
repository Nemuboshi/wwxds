//! Regenerates the committed resize fixtures with the production filter.
//!
//! `tests/fixtures/resize/` holds one source plane and one recorded analysis
//! plane per case. The `.out` planes are the exact bytes `FrameResizer`
//! produces — the integer box filter of `docs/Architecture.md` §4 — and this
//! example is the only thing that writes them. Run it whenever the filter
//! changes in a reviewed way, commit the new planes together with the change,
//! and say so in the change description:
//!
//! ```text
//! cargo run --release --example gen_resize_fixtures
//! git diff -- crates/ihni-media/tests/fixtures/resize
//! ```
//!
//! The `.in` source planes are frozen inputs and are never rewritten.

use std::fs;
use std::path::PathBuf;

use ihni_media::LumaFormat;
use ihni_media::decode::FrameResizer;

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
    // The example lives in the workspace root, so the path is absolute from
    // the manifest instead of relative to this file.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates")
        .join("ihni-media")
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

fn main() {
    for case in parse_index() {
        let source_path = fixture_directory().join(format!("{}.in", case.name));
        let target_path = fixture_directory().join(format!("{}.out", case.name));
        let source = fs::read(&source_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", source_path.display()));

        let pixels = case.target_width as usize * case.target_height as usize;
        let need = case.source_stride * case.source_height as usize;
        assert!(
            source.len() >= need,
            "{}: the source plane needs {need} bytes but holds {}",
            case.name,
            source.len()
        );

        let mut resizer = FrameResizer::open(
            case.source_width,
            case.source_height,
            case.format,
            case.target_width,
            case.target_height,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let mut out = vec![0u8; pixels];
        resizer
            .resize(&source, case.source_stride, &mut out)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));

        fs::write(&target_path, &out)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", target_path.display()));
        println!("{}: wrote {} bytes", case.name, out.len());
    }
}
