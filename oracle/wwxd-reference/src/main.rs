//! Reference-vector generator driver.
//!
//! Replays every committed detector fixture through the pinned WWXD C control and
//! writes `tests/fixtures/detect/reference.json`. Run it with:
//!
//! ```text
//! cargo run --manifest-path oracle/wwxd-reference/Cargo.toml
//! ```
//!
//! from anywhere in the repository. It reads the committed blobs, so it never
//! recompiles fixture data, and it is the only writer of `reference.json`; the Rust
//! integration test only reads that file.

use std::ffi::{c_int, c_void};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

unsafe extern "C" {
    fn bench_wwxd_create(width: c_int, height: c_int) -> *mut c_void;
    fn bench_wwxd_process(state: *mut c_void, luma: *const u8, stride: c_int) -> c_int;
    fn bench_wwxd_shot_age(state: *const c_void) -> c_int;
    fn bench_wwxd_digest_previous(state: *const c_void) -> u64;
    fn bench_wwxd_digest_predictors(state: *const c_void) -> u64;
    fn bench_wwxd_free(state: *mut c_void);
}

/// Repository root, resolved from the crate manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("oracle is two directories below the repository root")
        .to_path_buf()
}

fn fixture_dir() -> PathBuf {
    repo_root().join("tests").join("fixtures").join("detect")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = fixture_dir();
    let manifest_path = dir.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|_| {
        format!(
            "missing {}; run gen_detect_fixtures first",
            manifest_path.display()
        )
    })?;
    let manifest: Value = serde_json::from_str(&manifest_text)?;
    let fixtures = manifest["fixtures"]
        .as_array()
        .ok_or("manifest.json has no fixtures array")?;

    let mut outputs = Vec::new();
    for entry in fixtures {
        let name = entry["name"].as_str().ok_or("fixture missing name")?;
        let file = entry["file"].as_str().ok_or("fixture missing file")?;
        let width = entry["width"].as_i64().ok_or("fixture missing width")? as c_int;
        let height = entry["height"].as_i64().ok_or("fixture missing height")? as c_int;
        let stride = entry["stride"].as_i64().ok_or("fixture missing stride")? as c_int;
        let frames = entry["frames"].as_i64().ok_or("fixture missing frames")? as usize;

        let blob = std::fs::read(dir.join(file))?;
        let frame_bytes = (height as usize) * (stride as usize);
        if blob.len() < frame_bytes * frames {
            return Err(format!("{file} is shorter than its declared frame count").into());
        }

        // SAFETY: dimensions are within the bounds that create validates;
        // the returned state is uniquely owned and freed at the end of the loop.
        let state = unsafe { bench_wwxd_create(width, height) };
        if state.is_null() {
            return Err(format!("cannot create WWXD control for {name}").into());
        }

        let mut decisions = Vec::new();
        for index in 0..frames {
            let plane = &blob[index * frame_bytes..(index + 1) * frame_bytes];
            // SAFETY: state is a live control; plane is a valid borrowed slice for
            // the duration of the call, and stride >= width is checked in C.
            let cut = unsafe { bench_wwxd_process(state, plane.as_ptr(), stride) };
            let shot_age = unsafe { bench_wwxd_shot_age(state) };
            let predictors = unsafe { bench_wwxd_digest_predictors(state) };
            let previous = unsafe { bench_wwxd_digest_previous(state) };
            decisions.push(json!({
                "cut": cut != 0,
                "shot_age": shot_age,
                "predictors": predictors,
                "previous": previous,
            }));
        }

        // SAFETY: this uniquely owned state was created by the matching allocator.
        unsafe { bench_wwxd_free(state) };

        outputs.push(json!({
            "name": name,
            "width": width,
            "height": height,
            "stride": stride,
            "frames": decisions,
        }));
    }

    let document = json!({
        "reference_commit": env!("WWXD_REFERENCE_COMMIT"),
        "fixtures": outputs,
    });
    let text = format!("{}\n", serde_json::to_string_pretty(&document)?);
    let output_path = dir.join("reference.json");
    std::fs::write(&output_path, text)?;
    println!("wrote {}", output_path.display());
    Ok(())
}
