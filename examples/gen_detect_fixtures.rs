//! Generator for the detector parity fixture blobs.
//!
//! Run with `cargo run --example gen_detect_fixtures`. It writes small,
//! deterministic luma blobs and a `manifest.json` under
//! `tests/fixtures/detect/`. The committed blobs are the single source of truth
//! for the detector parity inputs: the Rust integration test and the pinned C
//! oracle both read the identical prepared bytes, so parity is measured on
//! one input, not two preparation paths.
//!
//! This example is a repository tool, not part of the library or the default
//! test run.

use std::fs;
use std::io::Write as _;
use std::path::Path;

fn uniform(w: u32, h: u32, value: u8) -> Vec<u8> {
    vec![value; (w * h) as usize]
}

/// A block-tiled pattern: each 16x16 tile takes a value from a fixed table, so
/// the frame is deterministic and has real structure for the motion search.
fn tiled(w: u32, h: u32, shift: u32) -> Vec<u8> {
    let mut plane = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let tx = ((x + shift) / 16) % 8;
            let ty = (y / 16) % 4;
            let value = ((tx * 37 + ty * 61) % 200 + 20) as u8;
            plane[(y * w + x) as usize] = value;
        }
    }
    plane
}

/// Flip a single 16x16 macroblock tile to a high value on one frame, so an
/// isolated local change is exercised against an otherwise static clip.
fn with_flipped_tile(w: u32, h: u32, tile_x: u32, tile_y: u32) -> Vec<u8> {
    let mut plane = uniform(w, h, 32);
    for y in 0..16 {
        for x in 0..16 {
            let px = tile_x * 16 + x;
            let py = tile_y * 16 + y;
            if px < w && py < h {
                plane[(py * w + px) as usize] = 220;
            }
        }
    }
    plane
}

struct Fixture {
    name: &'static str,
    width: u32,
    height: u32,
    frames: Vec<Vec<u8>>,
}

fn fixtures() -> Vec<Fixture> {
    // Base analysis size 96x64 (mb 6x4). The intra-majority threshold for a
    // 6x4 grid is ((4-2)*(6-2))/2 = 4, so >= 5 intra sub-blocks trips the early
    // return; a global step produces that path, while a single flipped tile does
    // not, so both the partial and complete statistic paths are covered.
    //
    // The full-loop sSAD / complexity accumulators are exercised by the non-cut
    // fixtures (uniform_repeat, bright_ramp, translation_motion). Those run
    // under `cargo test`, which is the debug profile, so the overflow bound
    // asserted in WWXDDesign.md §6 (core.rs debug_assert!) is validated live on
    // every parity run; no dedicated adversarial blob is needed. The exact-area
    // buffer arithmetic is covered by a unit test in state.rs instead of a
    // multi-megabyte committed fixture.
    vec![
        Fixture {
            name: "uniform_repeat",
            width: 96,
            height: 64,
            frames: (0..8).map(|_| uniform(96, 64, 32)).collect(),
        },
        Fixture {
            name: "single_hard_cut",
            width: 96,
            height: 64,
            frames: vec![
                uniform(96, 64, 32),
                uniform(96, 64, 32),
                uniform(96, 64, 32),
                uniform(96, 64, 220),
                uniform(96, 64, 220),
                uniform(96, 64, 220),
            ],
        },
        Fixture {
            name: "two_adjacent_cuts",
            width: 96,
            height: 64,
            frames: vec![
                uniform(96, 64, 32),
                uniform(96, 64, 220),
                uniform(96, 64, 32),
                uniform(96, 64, 220),
            ],
        },
        Fixture {
            name: "single_tile_flip",
            width: 96,
            height: 64,
            frames: vec![
                uniform(96, 64, 32),
                uniform(96, 64, 32),
                with_flipped_tile(96, 64, 2, 1),
                uniform(96, 64, 32),
            ],
        },
        Fixture {
            name: "bright_ramp",
            width: 96,
            height: 64,
            frames: (0..8).map(|i| uniform(96, 64, 32 + i * 6)).collect(),
        },
        Fixture {
            name: "translation_motion",
            width: 96,
            height: 64,
            frames: (0..8).map(|i| tiled(96, 64, i)).collect(),
        },
        Fixture {
            name: "odd_size_cut",
            width: 90,
            height: 60,
            frames: vec![
                uniform(90, 60, 32),
                uniform(90, 60, 32),
                uniform(90, 60, 220),
                uniform(90, 60, 220),
            ],
        },
    ]
}

fn main() {
    let dir = Path::new("tests/fixtures/detect");
    fs::create_dir_all(dir).expect("create fixture directory");

    let mut manifest = String::from("{\n  \"fixtures\": [\n");
    let all = fixtures();
    for (position, fixture) in all.iter().enumerate() {
        let file_name = format!("{}.bin", fixture.name);
        let mut blob = Vec::new();
        let frame_len = (fixture.width * fixture.height) as usize;
        for frame in &fixture.frames {
            assert_eq!(frame.len(), frame_len, "{} frame size", fixture.name);
            blob.extend_from_slice(frame);
        }
        let mut file = fs::File::create(dir.join(&file_name)).expect("write blob");
        file.write_all(&blob).expect("write blob bytes");

        manifest.push_str(&format!(
            "    {{ \"name\": \"{}\", \"file\": \"{}\", \"width\": {}, \"height\": {}, \"stride\": {}, \"frames\": {} }}{}",
            fixture.name,
            file_name,
            fixture.width,
            fixture.height,
            fixture.width, // tight stride, matching the decoder's output
            fixture.frames.len(),
            if position + 1 == all.len() { "" } else { "," }
        ));
        manifest.push('\n');
    }
    manifest.push_str("  ]\n}\n");
    fs::write(dir.join("manifest.json"), manifest).expect("write manifest");

    println!("wrote {} fixtures to {}", all.len(), dir.display());
}
