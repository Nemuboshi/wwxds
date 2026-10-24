//! Per-frame digests of the plane the detector is actually handed.
//!
//! `tests/decode.rs` checks decoded properties -- index order, timestamps, mean
//! luma, depth. This test checks the plane *bytes*, frame by frame, against a
//! recorded list. That is the finer signal: a change that keeps every index and
//! every mean correct while altering one luma value passes the first test and
//! fails this one.
//!
//! Two lists are committed, and they fail for different reasons:
//!
//! - `fixtures/analysis-digests.txt` digests the analysis plane at the default
//!   360-height, so it covers decoding *and* scaling together. It is the gate
//!   for any change to either stage.
//! - `fixtures/native-digests.txt` digests the plane at the source height. At
//!   the source dimensions swscale takes its unscaled copy branch, which
//!   promotes and reduces each sample without filtering, so this list isolates
//!   the decoder from the filtered resampling path.
//!
//! Between them, a failure points at a stage: if the native list still matches
//! and the analysis list does not, the resampler moved.
//!
//! The digests come from `examples/digest`, whose output format they keep, so a
//! recorded list and a fresh run can also be compared with `diff` or with
//! `scripts/compare-digest.sh`.
//!
//! Like the other media tests, this one needs the fixtures written by
//! `scripts/make-m1-fixtures.sh` and reports them as absent rather than failing
//! when that separate step has not run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wwxds_media::decode::{FrameStream, ReaderOptions};

/// FNV-1a 64, the same construction the detector uses for its state digests.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One recorded input: its header and the digest of every frame.
struct Expected {
    /// The `# input=...` header exactly as recorded.
    header: String,
    /// Frame index to digest, in recorded order.
    frames: Vec<(u64, u64)>,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/m1-fixtures")
}

fn parse_expected(text: &str) -> Vec<Expected> {
    let mut out: Vec<Expected> = Vec::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("# input=") {
            out.push(Expected {
                header: format!("# input={header}"),
                frames: Vec::new(),
            });
        } else if line.starts_with("# end") {
            continue;
        } else if !line.trim().is_empty() {
            let current = out
                .last_mut()
                .expect("a frame line must follow an input header");
            let (index, digest) = line
                .split_once(' ')
                .expect("a frame line is '<index> <digest>'");
            current.frames.push((
                index.parse().expect("frame index is a number"),
                u64::from_str_radix(digest, 16).expect("digest is hexadecimal"),
            ));
        }
    }
    out
}

/// FNV-1a over the first `width` bytes of every row.
///
/// The stride comes from the buffer length rather than from the width, so row
/// padding never enters the digest and an implementation that assumes
/// `stride == width` cannot pass by accident.
fn plane_digest(plane: &[u8], width: usize, height: usize) -> u64 {
    let stride = plane.len() / height;
    let mut hash = FNV_OFFSET_BASIS;
    for row in 0..height {
        let start = row * stride;
        for byte in &plane[start..start + width] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

/// The analysis height recorded in an `# input=...` header.
fn recorded_analysis_height(header: &str) -> u32 {
    let analysis = header
        .split_whitespace()
        .find_map(|field| field.strip_prefix("analysis="))
        .expect("the header records the analysis size");
    let (_, height) = analysis.split_once('x').expect("the analysis size is WxH");
    height.parse().expect("the analysis height is a number")
}

/// The input file name recorded in an `# input=...` header.
fn recorded_input_name(header: &str) -> &str {
    let name = header
        .strip_prefix("# input=")
        .expect("the header starts with '# input='");
    name.split_whitespace()
        .next()
        .expect("the header records the input name")
}

fn check(list: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(list);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let expected = parse_expected(&text);
    assert!(
        !expected.is_empty(),
        "{}: no inputs recorded",
        path.display()
    );

    let root = fixture_root();
    if !root.is_dir() {
        eprintln!(
            "skipping {list}: '{}' is absent; run bash scripts/make-m1-fixtures.sh",
            root.display()
        );
        return;
    }

    for entry in &expected {
        let name = recorded_input_name(&entry.header);
        let input = root.join(name);
        if !input.is_file() {
            eprintln!("skipping {list}: fixture '{name}' is absent");
            continue;
        }

        let options = ReaderOptions {
            analysis_height: Some(recorded_analysis_height(&entry.header)),
            ..ReaderOptions::default()
        };
        let mut stream =
            FrameStream::open(&input, options).unwrap_or_else(|error| panic!("{name}: {error}"));
        let size = stream.analysis_size();

        let mut got: BTreeMap<u64, u64> = BTreeMap::new();
        while let Some(frame) = stream
            .next_frame()
            .unwrap_or_else(|error| panic!("{name}: {error}"))
        {
            got.insert(
                frame.index().get(),
                plane_digest(frame.luma(), size.width() as usize, size.height() as usize),
            );
            stream.recycle(frame);
        }

        assert_eq!(
            got.len(),
            entry.frames.len(),
            "{name}: frame count differs from {}",
            path.display()
        );
        for (index, want) in &entry.frames {
            let have = got
                .get(index)
                .unwrap_or_else(|| panic!("{name}: frame {index} is missing"));
            assert_eq!(
                have,
                want,
                "{name} frame {index}: analysis-plane digest differs from {}. \
                 The recorded header was '{}'",
                path.display(),
                entry.header
            );
        }
    }
}

#[test]
fn the_analysis_plane_matches_its_recorded_digests() {
    check("analysis-digests.txt");
}

#[test]
fn the_native_luma_plane_matches_its_recorded_digests() {
    check("native-digests.txt");
}
