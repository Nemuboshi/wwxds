//! The production analysis-plane filter, called directly.
//!
//! This runs the [`FrameResizer`] that `FrameReader` uses, as the direct link
//! gate for the production path: every resize entry point the crate exposes
//! goes through the integer box filter of `docs/Architecture.md` §4.
//!
//! The asserted identities follow from the filter definition, not from a
//! library:
//!
//! - identity geometry returns every 8-bit sample unchanged, and a 10-bit
//!   sample reduced by `>> 2` of its exact value;
//! - a constant plane scales to that same constant in every geometry, because
//!   the weights of one output pixel sum to the source area it covers.
//!
//! Filtered-path equality on real media stays with `tests/analysis_digests.rs`,
//! which digests every frame of every fixture through this same path.

use wwxds_media::LumaFormat;
use wwxds_media::decode::FrameResizer;

/// Builds a gray8 plane at a deliberately padded stride, so a resizer that
/// assumed `stride == width` cannot pass.
fn plane8(
    width: usize,
    height: usize,
    pad: usize,
    value: impl Fn(usize, usize) -> u8,
) -> (Vec<u8>, usize) {
    let stride = align64(width + pad);
    let mut plane = vec![0u8; stride * height];
    for y in 0..height {
        for x in 0..width {
            plane[y * stride + x] = value(x, y);
        }
    }
    (plane, stride)
}

/// Builds a 10-bit plane (either endianness) at a padded stride.
fn plane10(
    width: usize,
    height: usize,
    format: LumaFormat,
    value: impl Fn(usize, usize) -> u16,
) -> (Vec<u8>, usize) {
    let stride = align64(width * 2 + 64);
    let mut plane = vec![0u8; stride * height];
    for y in 0..height {
        for x in 0..width {
            let sample = value(x, y);
            let offset = y * stride + x * 2;
            match format {
                LumaFormat::Gray10Le => {
                    plane[offset..offset + 2].copy_from_slice(&sample.to_le_bytes())
                }
                LumaFormat::Gray10Be => {
                    plane[offset..offset + 2].copy_from_slice(&sample.to_be_bytes())
                }
                LumaFormat::Gray8 => panic!("plane10 asked for an 8-bit format"),
            }
        }
    }
    (plane, stride)
}

/// Rounds up to a multiple of 64, keeping the padded-stride coverage honest.
fn align64(value: usize) -> usize {
    (value + 63) & !63
}

#[test]
fn identity_geometry_round_trips_an_8_bit_ramp_exactly() {
    // Identity geometry: each output pixel covers exactly its source pixel,
    // so the rounded mean is the sample itself. Any value, not just constants.
    let (plane, stride) = plane8(96, 64, 17, |x, y| ((x * 7 + y * 13) % 256) as u8);
    let mut resizer =
        FrameResizer::open(96, 64, LumaFormat::Gray8, 96, 64).expect("a supported geometry");
    let mut out = vec![0u8; 96 * 64];
    resizer
        .resize(&plane, stride, &mut out)
        .expect("the plane resizes");
    for y in 0..64 {
        for x in 0..96 {
            assert_eq!(
                out[y * 96 + x],
                ((x * 7 + y * 13) % 256) as u8,
                "identity 8-bit pixel ({x},{y}) must return to its source value"
            );
        }
    }
}

#[test]
fn identity_geometry_reduces_10_bit_samples_by_two_bits() {
    // Identity geometry on a 10-bit source: the mean is the sample exactly,
    // and the documented reduction is `>> 2` of that value. This is a plain
    // two-bit shift, not a rescale to the full 8-bit range.
    for format in [LumaFormat::Gray10Le, LumaFormat::Gray10Be] {
        let (plane, stride) = plane10(96, 64, format, |x, y| ((x * 11 + y * 23) % 1024) as u16);
        let mut resizer = FrameResizer::open(96, 64, format, 96, 64).expect("a supported geometry");
        let mut out = vec![0u8; 96 * 64];
        resizer
            .resize(&plane, stride, &mut out)
            .expect("the plane resizes");
        for y in 0..64 {
            for x in 0..96 {
                let v = (x * 11 + y * 23) % 1024;
                assert_eq!(
                    out[y * 96 + x] as usize,
                    v >> 2,
                    "identity {name} pixel ({x},{y}) disagrees with the documented reduction",
                    name = format.name()
                );
            }
        }
    }
}

#[test]
fn constant_planes_scale_to_the_same_constant() {
    // A constant plane must survive the filter unchanged, in both directions:
    // the weights of one output pixel sum to the source area it covers, so
    // the rounded mean of a constant is that constant. A mis-normalized
    // weight row breaks this first.
    for (value, label) in [(0u8, "zero"), (255u8, "maximum")] {
        let (plane, stride) = plane8(96, 64, 64, |_, _| value);
        for (dst_w, dst_h) in [(160usize, 90usize), (48usize, 32usize)] {
            let mut resizer =
                FrameResizer::open(96, 64, LumaFormat::Gray8, dst_w as u32, dst_h as u32)
                    .expect("a supported geometry");
            let mut out = vec![0u8; dst_w * dst_h];
            resizer
                .resize(&plane, stride, &mut out)
                .expect("the plane resizes");
            assert!(
                out.iter().all(|byte| *byte == value),
                "the {label} 8-bit constant changed at {dst_w}x{dst_h}: {:?}",
                &out[..8.min(out.len())]
            );
        }
    }

    let (plane, stride) = plane10(96, 64, LumaFormat::Gray10Be, |_, _| 0x3FF);
    let mut resizer =
        FrameResizer::open(96, 64, LumaFormat::Gray10Be, 160, 90).expect("a supported geometry");
    let mut out = vec![0u8; 160 * 90];
    resizer
        .resize(&plane, stride, &mut out)
        .expect("the plane resizes");
    assert!(
        out.iter().all(|byte| *byte == 255),
        "the maximum 10-bit constant did not scale to 255"
    );
}

#[test]
fn a_short_destination_is_rejected_before_any_write() {
    // The resizer must check the caller's buffer instead of writing past its
    // end.
    let (plane, stride) = plane8(96, 64, 0, |_, _| 32);
    let mut resizer = FrameResizer::open(96, 64, LumaFormat::Gray8, 160, 90).expect("opens");
    let mut out = vec![0u8; 160 * 90 - 1];
    let error = resizer
        .resize(&plane, stride, &mut out)
        .expect_err("a short destination must be rejected");
    assert!(
        error.contains("bytes"),
        "the rejection should name the sizes: {error}"
    );
    assert!(
        out.iter().all(|byte| *byte == 0),
        "a rejected resize must not write"
    );
}

#[test]
fn a_short_source_plane_is_rejected() {
    // `stride * height` must fit inside the source slice before any row is
    // read.
    let mut plane = vec![0u8; 96 * 64];
    plane.fill(200);
    let mut resizer = FrameResizer::open(96, 64, LumaFormat::Gray8, 48, 32).expect("opens");
    let mut out = vec![0u8; 48 * 32];
    // Ask for a stride the slice cannot satisfy: 96 bytes per row for 64 rows
    // needs 6144 bytes and the plane holds 96 * 63 + 1.
    let short = 96 * 63 + 1;
    let error = resizer
        .resize(&plane[..short], 96, &mut out)
        .expect_err("a source shorter than stride * height must be rejected");
    assert!(
        error.contains("bytes"),
        "the rejection should name the sizes: {error}"
    );
}

#[test]
fn a_stride_smaller_than_one_row_is_rejected() {
    // 10-bit rows need `width * 2` bytes; a stride below that cannot address
    // the plane.
    let (plane, _) = plane10(96, 64, LumaFormat::Gray10Le, |_, _| 512);
    let mut resizer = FrameResizer::open(96, 64, LumaFormat::Gray10Le, 48, 32).expect("opens");
    let mut out = vec![0u8; 48 * 32];
    let error = resizer
        .resize(&plane, 96, &mut out)
        .expect_err("a stride below one row must be rejected");
    assert!(
        error.contains("stride"),
        "the rejection should name the stride: {error}"
    );
}
