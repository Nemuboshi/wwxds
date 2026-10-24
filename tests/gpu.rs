//! End-to-end GPU parity: the D3D11 backend must produce the same boundary
//! list as the CPU path on the same video.
//!
//! Like the media-level GPU test, this needs the 1080p benchmark clip in the
//! gitignored target directory and a usable D3D11 device, and skips with a
//! message when either is absent. The detection result never depends on the
//! selected backend (`docs/Architecture.md` §8), and this is the test that
//! pins that claim at the pipeline level.

use std::path::PathBuf;

use wwxds::{RunOptions, detect_video};

fn bench_clip() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench-1080p.mkv");
    path.is_file().then_some(path)
}

#[test]
fn the_gpu_backend_reports_the_same_boundaries_as_the_cpu_path() {
    let Some(path) = bench_clip() else {
        eprintln!("skipping: the 1080p benchmark clip is absent");
        return;
    };

    let cpu = detect_video(
        &path,
        RunOptions {
            gpu: wwxds::decode::GpuMode::Off,
            ..RunOptions::default()
        },
    )
    .expect("the CPU run succeeds");

    let gpu = detect_video(
        &path,
        RunOptions {
            gpu: wwxds::decode::GpuMode::Auto,
            ..RunOptions::default()
        },
    )
    .expect("the GPU run succeeds");

    if gpu.stats.backend != "d3d11" {
        eprintln!("skipping: the D3D11 backend is unavailable on this machine");
        return;
    }

    assert!(
        !cpu.boundaries.is_empty(),
        "the benchmark clip produced no boundaries"
    );
    assert_eq!(
        cpu.boundaries, gpu.boundaries,
        "the GPU backend changed the boundary list"
    );
    assert_eq!(cpu.stats.frames, gpu.stats.frames);
}
