//! Micro-benchmark of the analysis-plane filter on a synthetic plane.
//!
//! This isolates [`FrameResizer`] from decoding and detection, so filter
//! revisions can be compared without the pipeline's decoder contention. It
//! times whole `resize` calls over a synthetic 1920x1080 plane at the default
//! 640x360 analysis size, for one 8-bit and one little-endian 10-bit source.
//! The numbers are wall times on the running machine; record them in
//! `docs/Performance.md` only next to a same-machine, same-build comparison.
//!
//! Usage: cargo run --release --example bench_resize

use std::time::Instant;

use ihni_media::LumaFormat;
use ihni_media::decode::FrameResizer;

/// One geometry timed over a synthetic plane.
struct Case {
    label: &'static str,
    format: LumaFormat,
    /// Bytes per source sample.
    sample_bytes: usize,
    /// Maximum sample value, for the synthetic ramp.
    maximum: usize,
}

fn main() {
    let (source_width, source_height) = (1920u32, 1080u32);
    let (dest_width, dest_height) = (640u32, 360u32);
    let cases = [
        Case {
            label: "gray8 1920x1080 -> 640x360",
            format: LumaFormat::Gray8,
            sample_bytes: 1,
            maximum: 256usize,
        },
        Case {
            label: "gray10le 1920x1080 -> 640x360",
            format: LumaFormat::Gray10Le,
            sample_bytes: 2,
            maximum: 1024usize,
        },
    ];
    let rounds = 20u32;
    for case in cases {
        let sample_bytes = case.sample_bytes;
        let stride = source_width as usize * sample_bytes + 64;
        let mut source = vec![0u8; stride * source_height as usize];
        for y in 0..source_height as usize {
            for x in 0..source_width as usize {
                let sample = ((x * 7 + y * 13) % case.maximum) as u16;
                let offset = y * stride + x * sample_bytes;
                match case.format {
                    LumaFormat::Gray8 => source[offset] = sample as u8,
                    LumaFormat::Gray10Le => {
                        source[offset..offset + 2].copy_from_slice(&sample.to_le_bytes())
                    }
                    LumaFormat::Gray10Be => {
                        source[offset..offset + 2].copy_from_slice(&sample.to_be_bytes())
                    }
                }
            }
        }
        let mut resizer = FrameResizer::open(
            source_width,
            source_height,
            case.format,
            dest_width,
            dest_height,
        )
        .expect("a supported geometry");
        let mut out = vec![0u8; dest_width as usize * dest_height as usize];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        let start = Instant::now();
        for _ in 0..rounds {
            resizer.resize(&source, stride, &mut out).expect("steady");
        }
        println!(
            "{}: {:?} per frame (mean of {} runs, one warmup discarded)",
            case.label,
            start.elapsed() / rounds,
            rounds
        );
    }
}
