//! The production analysis-plane filter: an exact integer box (area) average.
//!
//! [`FrameResizer`] maps one source luma geometry to one analysis size where
//! every axis divides into the target in whole pixels — the only shape the
//! analysis-size derivation produces (`docs/Architecture.md` §4). Every
//! output sample is the rounded integer mean of the `sx * sy` source samples
//! its block covers, accumulated in exact unsigned integers. There is no
//! floating point and no library scaler; the same arithmetic is what the
//! D3D11 shader computes.
//!
//! The filter runs as two separable passes over one reusable intermediate:
//! a horizontal pass reduces every source row into per-output-column sums,
//! and a vertical pass folds `sy` of those rows into the rounded 8-bit
//! samples. Both passes read the source plane under its actual stride and
//! reuse their buffers across frames, so a steady-state run allocates
//! nothing.

use crate::luma::LumaFormat;

/// Shift of the magic multiplier for division by the block size.
const MAGIC_SHIFT: u32 = 36;

/// Resizes one source geometry and format to one fixed analysis size.
///
/// One filter is built per run and reused; the intermediate plane and the
/// accumulator row are reused per frame. A resizer is bound to the geometry
/// and format it was opened with — feeding it a plane of any other size or
/// depth is rejected rather than silently rescaled.
pub struct FrameResizer {
    source_width: usize,
    source_height: usize,
    source_format: LumaFormat,
    destination_width: usize,
    destination_height: usize,
    /// Whole source pixels one output sample covers, per axis.
    block_width: usize,
    block_height: usize,
    /// The block count's rounding half and the precomputed magic multiplier
    /// for exact division by it (`(n * magic) >> MAGIC_SHIFT` equals
    /// `(n + half) / block` for every reachable sum).
    block_half: u32,
    block_magic: u64,
    /// Reused horizontal intermediate, `destination_width * source_height`.
    horizontal: Vec<u32>,
    /// Reused accumulator row for the vertical pass. The raw block sums fit
    /// `u32` because `open` rejects a block of 4096 or more.
    vertical: Vec<u32>,
}

impl FrameResizer {
    /// Opens the filter for one source and one destination.
    ///
    /// Both destination axes must divide their source axis in whole pixels;
    /// anything else is rejected, because the filter computes whole-block
    /// means and has no fractional coverage path. The returned message names
    /// the failed conversion so the reader can add the input path: a filter
    /// knows nothing about the file it was built for.
    pub fn open(
        source_width: u32,
        source_height: u32,
        source_format: LumaFormat,
        destination_width: u32,
        destination_height: u32,
    ) -> Result<Self, String> {
        if source_width == 0 || source_height == 0 {
            return Err(format!(
                "the source size {source_width}x{source_height} has a zero dimension"
            ));
        }
        if destination_width == 0 || destination_height == 0 {
            return Err(format!(
                "the destination size {destination_width}x{destination_height} has a zero \
                 dimension"
            ));
        }
        if !source_width.is_multiple_of(destination_width) {
            return Err(format!(
                "the source width {source_width} does not divide into {destination_width} in \
                 whole pixels"
            ));
        }
        if !source_height.is_multiple_of(destination_height) {
            return Err(format!(
                "the source height {source_height} does not divide into {destination_height} in \
                 whole pixels"
            ));
        }
        let block_width = (source_width / destination_width) as usize;
        let block_height = (source_height / destination_height) as usize;
        let block = block_width * block_height;
        // m = ceil(2^36 / block); the identity `(n * m) >> 36 == (n + half) /
        // block` holds for every reachable sum because `n * block < 2^36`:
        // the largest sum is 1023 * block + half, so the requirement is
        // 1024 * block^2 < 2^36, i.e. block < 2^12 — exactly the guard
        // below.
        if block >= 4096 {
            return Err(format!(
                "the {block_width}x{block_height} source block covers {block} pixels, above the \
                 4095 the exact division supports"
            ));
        }
        let block = block as u32;
        let magic = (1u64 << MAGIC_SHIFT).div_ceil(u64::from(block));
        Ok(Self {
            source_width: source_width as usize,
            source_height: source_height as usize,
            source_format,
            destination_width: destination_width as usize,
            destination_height: destination_height as usize,
            block_width,
            block_height,
            block_half: block / 2,
            block_magic: magic,
            horizontal: Vec::new(),
            vertical: Vec::new(),
        })
    }

    /// The luma format the context was built for.
    pub fn source_format(&self) -> LumaFormat {
        self.source_format
    }

    /// Resizes one source plane into `target`, producing 8-bit samples.
    ///
    /// `source` is the decoder's plane addressed by its real `source_stride`;
    /// `target` must hold exactly the tight `width * height` analysis plane.
    /// Every length is checked before anything is written, so a short buffer
    /// is a rejection, not an overrun.
    pub fn resize(
        &mut self,
        source: &[u8],
        source_stride: usize,
        target: &mut [u8],
    ) -> Result<(), String> {
        let source_row = self
            .source_width
            .checked_mul(self.source_format.sample_bytes())
            .ok_or_else(|| "the source row length overflowed".to_owned())?;
        if source_stride < source_row {
            return Err(format!(
                "the source stride {source_stride} is smaller than one row of {source_row} bytes"
            ));
        }
        let source_need = source_stride
            .checked_mul(self.source_height)
            .ok_or_else(|| "the source plane size overflowed".to_owned())?;
        if source.len() < source_need {
            return Err(format!(
                "the source plane needs {source_need} bytes but the slice holds {}",
                source.len()
            ));
        }
        let target_need = self
            .destination_width
            .checked_mul(self.destination_height)
            .ok_or_else(|| "the destination plane size overflowed".to_owned())?;
        if target.len() < target_need {
            return Err(format!(
                "the 8-bit destination needs {target_need} bytes but the slice holds {}",
                target.len()
            ));
        }

        self.filter(source, source_stride, target);
        Ok(())
    }

    /// The two-pass filter body. The caller has checked every length.
    fn filter(&mut self, source: &[u8], source_stride: usize, target: &mut [u8]) {
        let width = self.destination_width;
        let height = self.destination_height;
        let source_height = self.source_height;
        if self.horizontal.len() != width * source_height {
            self.horizontal = vec![0u32; width * source_height];
        }
        if self.vertical.len() != width {
            self.vertical = vec![0u32; width];
        }

        // Horizontal pass: reduce every source row into per-output-column
        // raw block sums, in order, into the reusable plane.
        let format = self.source_format;
        let block_width = self.block_width;
        for y in 0..source_height {
            let row = &source[y * source_stride..];
            let out_row = &mut self.horizontal[y * width..][..width];
            horizontal_row(row, out_row, block_width, format);
        }

        // Vertical pass: fold the `block_height` covered intermediate rows
        // into one accumulator row, then round once per output pixel.
        let shift = match format {
            LumaFormat::Gray8 => 0,
            LumaFormat::Gray10Le | LumaFormat::Gray10Be => 2,
        };
        let block_height = self.block_height;
        let (half, magic) = (self.block_half, self.block_magic);
        for j in 0..height {
            let accumulator = &mut self.vertical[..width];
            accumulator.fill(0);
            for row_index in 0..block_height {
                let intermediate =
                    &self.horizontal[(j * block_height + row_index) * width..][..width];
                for (sum, value) in accumulator.iter_mut().zip(intermediate) {
                    *sum += *value;
                }
            }
            let out_row = &mut target[j * width..][..width];
            for (out, sum) in out_row.iter_mut().zip(accumulator.iter()) {
                let mean = ((u64::from(sum + half)) * magic) >> MAGIC_SHIFT;
                *out = (mean >> shift) as u8;
            }
        }
    }
}

/// Filters one source row into its output row of the horizontal pass.
///
/// Output `i` is the raw sum of the `block_width` source samples the output
/// covers; the vertical pass applies the single shared division.
fn horizontal_row(row: &[u8], out_row: &mut [u32], block_width: usize, format: LumaFormat) {
    match format {
        LumaFormat::Gray8 => uniform_gray8_row(row, out_row, block_width),
        _ => uniform_gray10_row(row, out_row, block_width, format == LumaFormat::Gray10Be),
    }
}

/// Sums every whole-pixel block of one gray8 row into one output sum.
///
/// Output `i` is the byte sum of `row[i * step .. i * step + step]`. The
/// blocks are disjoint, so there is no sliding-window shortcut; each output
/// sums its own run. An earlier SSE2 version of this pass computed shifted
/// sliding windows instead of block sums for steps above one and was
/// removed; this form stays a plain block sum, which the GPU cross-check in
/// `tests/gpu.rs` pins byte for byte on real media. The common steps run
/// through a fixed-size kernel so the block width is known to the compiler.
fn uniform_gray8_row(row: &[u8], out: &mut [u32], step: usize) {
    debug_assert!(row.len() >= out.len() * step);
    match step {
        1 => uniform_gray8_row_const::<1>(row, out),
        2 => uniform_gray8_row_const::<2>(row, out),
        3 => uniform_gray8_row_const::<3>(row, out),
        4 => uniform_gray8_row_const::<4>(row, out),
        _ => {
            for (output, out) in out.iter_mut().enumerate() {
                let run = &row[output * step..][..step];
                *out = run.iter().map(|byte| u32::from(*byte)).sum();
            }
        }
    }
}

/// The fixed-width gray8 block-sum kernel.
fn uniform_gray8_row_const<const STEP: usize>(row: &[u8], out: &mut [u32]) {
    let blocks = row[..out.len() * STEP].as_chunks::<STEP>().0;
    for (block, out) in blocks.iter().zip(out.iter_mut()) {
        let mut sum = 0u32;
        for byte in block {
            sum += u32::from(*byte);
        }
        *out = sum;
    }
}

/// Sums every whole-pixel block of one 10-bit row into one output sum.
///
/// Output `i` is the sample sum of `row16[i * step .. i * step + step]`;
/// the samples are little-endian, or byte-swapped when `swap` is set
/// (big-endian sources). Like [`uniform_gray8_row`], the common steps use a
/// fixed-size kernel.
fn uniform_gray10_row(row: &[u8], out: &mut [u32], step: usize, swap: bool) {
    debug_assert!(row.len() >= out.len() * step * 2);
    match (step, swap) {
        (1, false) => uniform_gray10_row_const::<1, false>(row, out),
        (2, false) => uniform_gray10_row_const::<2, false>(row, out),
        (3, false) => uniform_gray10_row_const::<3, false>(row, out),
        (4, false) => uniform_gray10_row_const::<4, false>(row, out),
        (1, true) => uniform_gray10_row_const::<1, true>(row, out),
        (2, true) => uniform_gray10_row_const::<2, true>(row, out),
        (3, true) => uniform_gray10_row_const::<3, true>(row, out),
        (4, true) => uniform_gray10_row_const::<4, true>(row, out),
        _ => {
            for (output, out) in out.iter_mut().enumerate() {
                let run = &row[output * step * 2..][..step * 2];
                *out = run
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| {
                        sample_pair(
                            pair,
                            if swap {
                                LumaFormat::Gray10Be
                            } else {
                                LumaFormat::Gray10Le
                            },
                        )
                    })
                    .sum();
            }
        }
    }
}

/// The fixed-width 10-bit block-sum kernel.
fn uniform_gray10_row_const<const STEP: usize, const SWAP: bool>(row: &[u8], out: &mut [u32]) {
    let pairs = row[..out.len() * STEP * 2].as_chunks::<2>().0;
    for (index, out) in out.iter_mut().enumerate() {
        let mut sum = 0u32;
        for pair in &pairs[index * STEP..][..STEP] {
            let value = if SWAP {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            };
            sum += u32::from(value);
        }
        *out = sum;
    }
}

/// Reads one little- or big-endian sample pair as its unsigned integer value.
fn sample_pair(pair: &[u8], format: LumaFormat) -> u32 {
    match format {
        LumaFormat::Gray10Le => u32::from(u16::from_le_bytes([pair[0], pair[1]])),
        _ => u32::from(u16::from_be_bytes([pair[0], pair[1]])),
    }
}

#[cfg(test)]
mod tests {
    use super::FrameResizer;
    use crate::luma::LumaFormat;

    /// A tight gray8 plane with one value per pixel.
    fn plane(values: &[u8], width: usize) -> (Vec<u8>, usize) {
        (values.to_vec(), width)
    }

    #[test]
    fn identity_geometry_copies_the_samples_through() {
        // k = 1: one sample per block, so the output is the source unchanged.
        let values: Vec<u8> = (0..96 * 64).map(|i| (i * 7 % 251) as u8).collect();
        let (source, stride) = plane(&values, 96);
        let mut resizer =
            FrameResizer::open(96, 64, LumaFormat::Gray8, 96, 64).expect("an identity geometry");
        let mut out = vec![0u8; 96 * 64];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        assert_eq!(out, values);
    }

    #[test]
    fn a_two_by_two_mean_rounds_half_up() {
        // 2x2 source, one output: the four cases pin the rounding: sums 0..4
        // map to 0, 0, 1, 1 through (sum + 2) / 4.
        for (values, expected) in [
            ([0u8, 0, 0, 0], 0u8),
            ([0, 0, 0, 1], 0),
            ([0, 0, 1, 1], 1),
            ([1, 1, 1, 1], 1),
        ] {
            let (source, stride) = plane(&values, 2);
            let mut resizer =
                FrameResizer::open(2, 2, LumaFormat::Gray8, 1, 1).expect("a supported geometry");
            let mut out = [0u8];
            resizer
                .resize(&source, stride, &mut out)
                .expect("the plane resizes");
            assert_eq!(out[0], expected, "values {values:?}");
        }
    }

    #[test]
    fn downsampling_by_three_averages_exact_blocks() {
        // A 3x downscale covers whole source blocks, so each output is the
        // exact mean of a 3x3 block. The 6x6 ramp `4 * i` gives the four
        // block means 28, 40, 100, 112 by hand.
        let values: Vec<u8> = (0..36).map(|i| (i * 4) as u8).collect();
        let (source, stride) = plane(&values, 6);
        let mut resizer =
            FrameResizer::open(6, 6, LumaFormat::Gray8, 2, 2).expect("a supported geometry");
        let mut out = vec![0u8; 4];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        assert_eq!(out, vec![28, 40, 100, 112]);
    }

    #[test]
    fn non_square_blocks_divide_per_axis() {
        // 8x4 source to 2x2: sx = 4, sy = 2, block = 8. The four block sums
        // are 44, 76, 172, 204, and (sum + 4) / 8 is 6, 10, 22, 26.
        let values: Vec<u8> = (0..32).collect();
        let (source, stride) = plane(&values, 8);
        let mut resizer =
            FrameResizer::open(8, 4, LumaFormat::Gray8, 2, 2).expect("a supported geometry");
        let mut out = vec![0u8; 4];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        assert_eq!(out, vec![6, 10, 22, 26]);
    }

    #[test]
    fn ten_bit_means_reduce_by_two_bits_after_rounding() {
        // A 2x2 block of 10-bit samples {0, 0, 0, 1023} has mean 255.75,
        // which rounds to 256 and reduces by >> 2 to 64 — the reduction is
        // applied to the rounded mean.
        let mut plane = vec![0u8; 2 * 2 * 2];
        let samples = [0u16, 0, 0, 1023];
        for (index, value) in samples.iter().enumerate() {
            plane[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        let mut resizer =
            FrameResizer::open(2, 2, LumaFormat::Gray10Le, 1, 1).expect("a supported geometry");
        let mut out = [0u8];
        resizer
            .resize(&plane, 4, &mut out)
            .expect("the plane resizes");
        assert_eq!(out[0], 64);
    }

    #[test]
    fn rejects_geometries_that_do_not_divide_in_whole_pixels() {
        // 320/96 is fractional: no general path exists to fall back to.
        assert!(FrameResizer::open(320, 180, LumaFormat::Gray8, 96, 64).is_err());
        // 180/64 is fractional.
        assert!(FrameResizer::open(320, 180, LumaFormat::Gray8, 160, 64).is_err());
        // A destination larger than its source cannot divide it.
        assert!(FrameResizer::open(96, 64, LumaFormat::Gray8, 192, 128).is_err());
        assert!(FrameResizer::open(0, 64, LumaFormat::Gray8, 48, 32).is_err());
    }
}
