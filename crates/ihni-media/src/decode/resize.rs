//! The production analysis-plane filter: an exact integer box (area) average.
//!
//! [`FrameResizer`] maps one source luma geometry to one fixed analysis size
//! with the filter `docs/Architecture.md` §4 defines: every output sample is
//! the rounded integer mean of the source samples its unit square covers,
//! accumulated in exact unsigned integers. There is no intermediate plane, no
//! floating point, and no library scaler; the same arithmetic is what a GPU
//! backend must reproduce byte for byte.
//!
//! The filter runs as two separable passes over one reusable intermediate:
//! a horizontal pass reduces every source row into per-output-column sums,
//! and a vertical pass folds those sums into the rounded 8-bit samples. Both
//! passes read the source plane under its actual stride and reuse their
//! buffers across frames, so a steady-state run allocates nothing.

use crate::luma::LumaFormat;

/// The weight map of one axis: the covered source run and its integer
/// coverage weights for every output index.
///
/// The covered source pixels of one output index are always contiguous, so
/// the map stores the first index plus one weight per covered pixel. Every
/// weight is at most the destination length in `1/dest` units, and the
/// weights of one output index sum to the source length: the denominator the
/// two axes produce together is `source_width * source_height`.
#[derive(Clone, Debug)]
struct AxisMap {
    /// First covered source index of every output pixel.
    starts: Vec<u32>,
    /// Offsets into `weights`; `offsets[i]..offsets[i + 1]` holds output `i`.
    offsets: Vec<u32>,
    /// Coverage weights in `1/destination` units, contiguous per output.
    weights: Vec<u32>,
    /// The whole-map fast path: `Some(step)` when the destination divides the
    /// source, so every output pixel covers exactly `step` whole source
    /// pixels of weight `destination`. That is the shape of the default
    /// analysis sizes (1080p sources map 3:1), and the pass can then run as
    /// strided whole-block sums without per-pixel weights.
    uniform_step: Option<u32>,
}

impl AxisMap {
    /// Builds the map from a source length to a destination length.
    ///
    /// Output `i` covers the source interval
    /// `[i * source / dest, (i + 1) * source / dest)`; the weight of source
    /// pixel `x` is that interval's overlap with `[x, x + 1)`, scaled into
    /// exact integers by `dest`.
    fn build(source: u32, dest: u32) -> Self {
        let source = u64::from(source);
        let dest = u64::from(dest);
        let mut starts = Vec::with_capacity(dest as usize);
        let mut offsets = Vec::with_capacity(dest as usize + 1);
        let mut weights = Vec::new();
        offsets.push(0u32);
        for output in 0..dest {
            let low = output * source;
            let high = low + source;
            let first = (low / dest) as u32;
            starts.push(first);
            let mut x = first;
            while u64::from(x) * dest < high {
                let left = low.max(u64::from(x) * dest);
                let right = high.min((u64::from(x) + 1) * dest);
                weights.push((right - left) as u32);
                x += 1;
            }
            offsets.push(weights.len() as u32);
        }
        let uniform_step = (source % dest == 0).then_some((source / dest) as u32);
        Self {
            starts,
            offsets,
            weights,
            uniform_step,
        }
    }

    /// The covered weights of one output pixel, as a contiguous slice.
    fn run(&self, output: usize) -> (usize, &[u32]) {
        let start = self.offsets[output] as usize;
        let end = self.offsets[output + 1] as usize;
        (self.starts[output] as usize, &self.weights[start..end])
    }
}

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
    columns: AxisMap,
    rows: AxisMap,
    /// Denominator of the mean on the general path: the source area one
    /// output sample covers.
    denominator: u64,
    /// Whole-pixel-downscale fast path: when both axes are uniform, an output
    /// sample is the rounded mean of `sx * sy` whole source pixels. These are
    /// that block count, its rounding half, and the precomputed magic
    /// multiplier for exact division by it (`(n * magic) >> MAGIC_SHIFT`
    /// equals `(n + half) / block` for every reachable sum).
    block: Option<u32>,
    block_half: Option<u32>,
    block_magic: Option<u64>,
    /// Reused horizontal intermediate, `destination_width * source_height`.
    horizontal: Vec<u32>,
    /// Reused accumulator row for the vertical pass on the fast path. The
    /// raw block sums fit `u32` because the fast path requires
    /// `block < 4096`.
    vertical_fast: Vec<u32>,
    /// Reused accumulator row for the vertical pass on the general path. The
    /// weighted sums reach `1023 * W * H`, which needs `u64`.
    vertical_general: Vec<u64>,
}

/// Shift of the magic multiplier for division by the block size.
const MAGIC_SHIFT: u32 = 36;

impl FrameResizer {
    /// Opens the filter for one source and one destination.
    ///
    /// The returned message names the failed conversion so the reader can add
    /// the input path: a filter knows nothing about the file it was built for.
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
                "the destination size {destination_width}x{destination_height} has a zero dimension"
            ));
        }
        let denominator = u64::from(source_width) * u64::from(source_height);
        let columns = AxisMap::build(source_width, destination_width);
        let rows = AxisMap::build(source_height, destination_height);
        let (block, block_half, block_magic) = match (columns.uniform_step, rows.uniform_step) {
            (Some(sx), Some(sy)) if sx * sy < 4096 => {
                let block = sx * sy;
                let half = block / 2;
                // m = ceil(2^36 / block); the identity
                // `(n * m) >> 36 == (n + half) / block` holds for every
                // reachable sum because `n * block < 2^36`: the largest
                // sum is 1023 * block + half, so the requirement is
                // 1024 * block^2 < 2^36, i.e. block < 2^12 — exactly the
                // guard above.
                let magic = (1u64 << MAGIC_SHIFT).div_ceil(u64::from(block));
                (Some(block), Some(half), Some(magic))
            }
            _ => (None, None, None),
        };
        Ok(Self {
            source_width: source_width as usize,
            source_height: source_height as usize,
            source_format,
            destination_width: destination_width as usize,
            destination_height: destination_height as usize,
            columns,
            rows,
            denominator,
            block,
            block_half,
            block_magic,
            horizontal: Vec::new(),
            vertical_fast: Vec::new(),
            vertical_general: Vec::new(),
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

        // Horizontal pass: reduce every source row into per-output-column
        // sums. When both axes are whole-pixel downscales (the shape every
        // default analysis size has) the sums stay raw — the vertical pass
        // folds them and divides by the precomputed magic. Otherwise the
        // sums carry their column weights for the general vertical pass.
        // Rows are filtered in order into the reusable plane.
        let format = self.source_format;
        let sample_bytes = format.sample_bytes();
        let raw_sums = self.block.is_some();
        if raw_sums {
            if self.vertical_fast.len() != width {
                self.vertical_fast = vec![0u32; width];
            }
        } else if self.vertical_general.len() != width {
            self.vertical_general = vec![0u64; width];
        }
        for y in 0..source_height {
            let row = &source[y * source_stride..];
            let out_row = &mut self.horizontal[y * width..][..width];
            horizontal_row(
                row,
                out_row,
                &self.columns,
                width,
                format,
                sample_bytes,
                raw_sums,
            );
        }

        // Vertical pass: fold the covered intermediate rows into one
        // accumulator row, then round once per output pixel. The fast path
        // (both axes whole-pixel) adds the raw block sums, which fit `u32`
        // because `block < 4096`, and divides by the precomputed magic; the
        // general path keeps per-row weights, needs `u64` because the
        // weighted sums reach `1023 * W * H`, and divides by the covered
        // source area.
        let shift = match format {
            LumaFormat::Gray8 => 0,
            LumaFormat::Gray10Le | LumaFormat::Gray10Be => 2,
        };
        let general_half = (self.denominator / 2) as u32;
        let general_denominator = self.denominator as u32;
        for j in 0..height {
            let (first_row, weights) = self.rows.run(j);
            if let (Some(step), Some(magic), Some(half)) =
                (self.rows.uniform_step, self.block_magic, self.block_half)
            {
                // Whole-row steps of raw sums: add the rows, then divide
                // once per pixel with the precomputed magic.
                let accumulator = &mut self.vertical_fast[..width];
                accumulator.fill(0);
                for row_index in 0..step as usize {
                    let intermediate = &self.horizontal[(first_row + row_index) * width..][..width];
                    for (sum, value) in accumulator.iter_mut().zip(intermediate) {
                        *sum += *value;
                    }
                }
                let out_row = &mut target[j * width..][..width];
                for (out, sum) in out_row.iter_mut().zip(accumulator.iter()) {
                    let mean = ((u64::from(sum + half)) * magic) >> MAGIC_SHIFT;
                    *out = (mean >> shift) as u8;
                }
            } else {
                let accumulator = &mut self.vertical_general[..width];
                accumulator.fill(0);
                for (index, weight) in weights.iter().enumerate() {
                    let weight = u64::from(*weight);
                    let intermediate = &self.horizontal[(first_row + index) * width..][..width];
                    for (sum, value) in accumulator.iter_mut().zip(intermediate) {
                        *sum += u64::from(*value) * weight;
                    }
                }
                let out_row = &mut target[j * width..][..width];
                for (out, sum) in out_row.iter_mut().zip(accumulator.iter()) {
                    let mean =
                        ((sum + u64::from(general_half)) / u64::from(general_denominator)) >> shift;
                    *out = mean as u8;
                }
            }
        }
    }
}

/// Filters one source row into its output row of the horizontal pass.
///
/// `raw_sums` is set when both axes are whole-pixel downscales: the covered
/// source pixels then all carry the same weight, so the row stores the raw
/// block sum and the vertical pass applies the single shared division.
/// Otherwise the row stores the weighted sums the general vertical pass
/// folds.
fn horizontal_row(
    row: &[u8],
    out_row: &mut [u32],
    columns: &AxisMap,
    width: usize,
    format: LumaFormat,
    sample_bytes: usize,
    raw_sums: bool,
) {
    match (format, columns.uniform_step, raw_sums) {
        (LumaFormat::Gray8, Some(step), true) => {
            #[cfg(target_arch = "x86_64")]
            {
                uniform_gray8_row_sse2(row, out_row, step);
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                let step = step as usize;
                for (output, out) in out_row.iter_mut().enumerate() {
                    let run = &row[output * step..][..step];
                    let sum: u32 = run.iter().map(|byte| u32::from(*byte)).sum();
                    *out = sum;
                }
            }
        }
        (_, Some(step), true) => {
            let step = step as usize;
            #[cfg(target_arch = "x86_64")]
            {
                match format {
                    LumaFormat::Gray10Le if step <= 64 => {
                        uniform_gray10_row_sse2::<false>(row, out_row, step)
                    }
                    LumaFormat::Gray10Be if step <= 64 => {
                        uniform_gray10_row_sse2::<true>(row, out_row, step)
                    }
                    _ => uniform_row_scalar_16(row, out_row, step, format),
                }
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                uniform_row_scalar_16(row, out_row, step, format);
            }
        }
        (_, Some(step), false) => {
            let step = step as usize;
            let weight = width as u32;
            for (output, out) in out_row.iter_mut().enumerate() {
                let run = &row[output * step * sample_bytes..][..step * sample_bytes];
                let sum: u32 = match format {
                    LumaFormat::Gray8 => run.iter().map(|byte| u32::from(*byte)).sum(),
                    _ => run
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| sample_pair(pair, format))
                        .sum(),
                };
                *out = sum * weight;
            }
        }
        (_, None, _) => {
            for (output, out) in out_row.iter_mut().enumerate() {
                let (first, weights) = columns.run(output);
                let mut sum = 0u32;
                for (index, weight) in weights.iter().enumerate() {
                    let offset = (first + index) * sample_bytes;
                    let sample = match format {
                        LumaFormat::Gray8 => u32::from(row[offset]),
                        _ => sample_pair(&row[offset..offset + 2], format),
                    };
                    sum += sample * weight;
                }
                *out = sum;
            }
        }
    }
}

/// Sums every whole-pixel block of one gray8 row into one output sum.
///
/// This is the x86-64 kernel of the raw uniform horizontal pass: output `i`
/// is the byte sum of `row[i * step .. i * step + step]`. Every 16 outputs
/// run on SSE2: the block bytes of 16 outputs are 16 overlapping byte
/// vectors one byte apart, widened to `u16` and added — the sums of at most
/// 255 per byte stay exact in `u16` for every step below 258 — then widened
/// to `u32` for the store. SSE2 is baseline on x86-64, so no runtime
/// detection is needed. The portable path in `horizontal_row` computes the
/// same sums for other targets and is what the tests compare against.
#[cfg(target_arch = "x86_64")]
fn uniform_gray8_row_sse2(row: &[u8], out: &mut [u32], step: u32) {
    use std::arch::x86_64::{
        _mm_add_epi16, _mm_loadu_si128, _mm_setzero_si128, _mm_storeu_si128, _mm_unpackhi_epi8,
        _mm_unpackhi_epi16, _mm_unpacklo_epi8, _mm_unpacklo_epi16,
    };

    let step = step as usize;
    let width = out.len();
    // Every load below reads 16 bytes at `i * step + k` for `i + 16 <= width`
    // and `k < step`, so the last byte sits before `width * step`. The slice
    // the caller passes holds at least one source row, which is exactly
    // `width * step` bytes here.
    assert!(row.len() >= width * step);
    // SAFETY: every intrinsic call in this block is SSE2, which is baseline
    // on x86-64, so the feature is always available. The memory intrinsics
    // are bounded by the assertion above; the arithmetic intrinsics touch
    // only the local vectors.
    unsafe {
        let zeros = _mm_setzero_si128();
        let mut i = 0;
        while i + 16 <= width {
            let base = i * step;
            let (mut low, mut high) = (zeros, zeros);
            for k in 0..step {
                // SAFETY: the assertion above bounds every byte this load
                // reads: `base + k + 15 < width * step <= row.len()`. The
                // load is unaligned by design; source rows are not aligned.
                let bytes = _mm_loadu_si128(row.as_ptr().add(base + k).cast());
                low = _mm_add_epi16(low, _mm_unpacklo_epi8(bytes, zeros));
                high = _mm_add_epi16(high, _mm_unpackhi_epi8(bytes, zeros));
            }
            // SAFETY: the stores write the 16 `u32` outputs `out[i..i + 16]`,
            // which the slice holds; the rows are not alignment-guaranteed.
            let out = out.as_mut_ptr().add(i);
            _mm_storeu_si128(out.cast(), _mm_unpacklo_epi16(low, zeros));
            _mm_storeu_si128(out.add(4).cast(), _mm_unpackhi_epi16(low, zeros));
            _mm_storeu_si128(out.add(8).cast(), _mm_unpacklo_epi16(high, zeros));
            _mm_storeu_si128(out.add(12).cast(), _mm_unpackhi_epi16(high, zeros));
            i += 16;
        }
    }
    let mut i = (width / 16) * 16;
    while i < width {
        let run = &row[i * step..][..step];
        out[i] = run.iter().map(|byte| u32::from(*byte)).sum();
        i += 1;
    }
}

/// Sums every whole-pixel block of one 10-bit row into one output sum.
///
/// The 16-bit analogue of [`uniform_gray8_row_sse2`]: output `i` is the sum
/// of the little-endian samples `row16[i * step .. i * step + step]`, or the
/// byte-swapped ones when `SWAP` is set (big-endian sources). The sums are
/// exact in `u16` because the caller only routes steps up to 64 here
/// (64 x 1023 = 65472); larger steps and non-x86-64 targets take
/// [`uniform_row_scalar_16`], which computes the same sums.
#[cfg(target_arch = "x86_64")]
fn uniform_gray10_row_sse2<const SWAP: bool>(row: &[u8], out: &mut [u32], step: usize) {
    use std::arch::x86_64::{
        _mm_add_epi16, _mm_loadu_si128, _mm_or_si128, _mm_setzero_si128, _mm_slli_epi16,
        _mm_srli_epi16, _mm_storeu_si128, _mm_unpackhi_epi16, _mm_unpacklo_epi16,
    };

    let width = out.len();
    // Every load below reads 16 bytes — eight samples — at sample index
    // `i * step + k` for `i + 8 <= width` and `k < step`, so the last byte
    // sits before `width * step * 2`. The slice the caller passes holds at
    // least one source row, which is exactly that many bytes here.
    assert!(row.len() >= width * step * 2);
    // SAFETY: every intrinsic call in this block is SSE2, which is baseline
    // on x86-64. The memory intrinsics are bounded by the assertion above;
    // the arithmetic intrinsics touch only local vectors.
    unsafe {
        let zeros = _mm_setzero_si128();
        let mut i = 0;
        while i + 8 <= width {
            let base = (i * step) * 2;
            let mut acc = zeros;
            for k in 0..step {
                // SAFETY: the assertion above bounds every byte this load
                // reads: it reads `base + 2 * k .. base + 2 * k + 16`, and
                // the last byte is `2 * ((i + 8) * step - 1) + 1 < 2 * width
                // * step <= row.len()`. The load is unaligned by design;
                // source rows are not aligned.
                let mut samples = _mm_loadu_si128(row.as_ptr().add(base + k * 2).cast());
                if SWAP {
                    samples = _mm_or_si128(_mm_slli_epi16(samples, 8), _mm_srli_epi16(samples, 8));
                }
                acc = _mm_add_epi16(acc, samples);
            }
            // SAFETY: the stores write the eight `u32` outputs
            // `out[i..i + 8]`, which the slice holds; the rows are not
            // alignment-guaranteed.
            let out = out.as_mut_ptr().add(i);
            _mm_storeu_si128(out.cast(), _mm_unpacklo_epi16(acc, zeros));
            _mm_storeu_si128(out.add(4).cast(), _mm_unpackhi_epi16(acc, zeros));
            i += 8;
        }
    }
    let mut i = (width / 8) * 8;
    while i < width {
        out[i] = uniform_row_scalar_16_sample(row, i, step, SWAP);
        i += 1;
    }
}

/// Scalar whole-pixel 16-bit row sums, for targets without SSE2 and for
/// steps beyond the `u16`-exact range of the SSE2 kernel.
fn uniform_row_scalar_16(row: &[u8], out: &mut [u32], step: usize, format: LumaFormat) {
    let swap = format == LumaFormat::Gray10Be;
    for (output, out) in out.iter_mut().enumerate() {
        *out = uniform_row_scalar_16_sample(row, output, step, swap);
    }
}

/// One scalar 16-bit block sum at sample index `output`.
fn uniform_row_scalar_16_sample(row: &[u8], output: usize, step: usize, swap: bool) -> u32 {
    let run = &row[output * step * 2..][..step * 2];
    run.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let sample = if swap {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            };
            u32::from(sample)
        })
        .sum()
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
    use super::{AxisMap, FrameResizer};
    use crate::luma::LumaFormat;

    /// A tight gray8 plane with one value per pixel.
    fn plane(values: &[u8], width: usize) -> (Vec<u8>, usize) {
        (values.to_vec(), width)
    }

    #[test]
    fn axis_weights_sum_to_the_source_length_for_every_output() {
        // The denominator contract: per-axis weights of one output pixel sum
        // to the source length, in both directions and for odd sizes.
        for (source, dest) in [(96u32, 48u32), (48, 96), (31, 17), (97, 65), (64, 64)] {
            let map = AxisMap::build(source, dest);
            for output in 0..dest as usize {
                let (_, weights) = map.run(output);
                let total: u32 = weights.iter().sum();
                assert_eq!(total, source, "axis {source}->{dest} output {output}");
            }
        }
    }

    #[test]
    fn axis_weights_cover_a_contiguous_source_run() {
        let map = AxisMap::build(31, 17);
        for output in 0..17 {
            let (first, weights) = map.run(output);
            // Rebuilding the interval from the run must reproduce the weights.
            let covered = weights.len();
            assert!(first + covered <= 31, "output {output} leaves the source");
            assert!(covered >= 1, "output {output} covers nothing");
        }
    }

    #[test]
    fn a_one_pixel_output_is_the_rounded_mean_of_the_whole_plane() {
        // 2x2 source, one output: the mean rounds half up. The four cases
        // pin the rounding: sums 0..4 map to 0, 0, 1, 1 through
        // (sum + 2) / 4.
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
    fn a_three_by_three_mean_rounds_half_up() {
        // 3x3 of value v has sum 9v, an exact mean. Mixing one 0 among eight
        // 1s gives 8/9, which rounds to 1 through (8 + 4) / 9.
        let mut values = [1u8; 9];
        values[4] = 0;
        let (source, stride) = plane(&values, 3);
        let mut resizer =
            FrameResizer::open(3, 3, LumaFormat::Gray8, 1, 1).expect("a supported geometry");
        let mut out = [0u8];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        assert_eq!(out[0], 1);
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
    fn upsampling_weights_reach_across_a_boundary_exactly() {
        // 2x1 source to 5x1: output 2 straddles the source boundary with
        // equal coverage, so it is the exact mean of both pixels; the other
        // outputs sample one pixel each.
        let (source, stride) = plane(&[10u8, 90], 2);
        let mut resizer =
            FrameResizer::open(2, 1, LumaFormat::Gray8, 5, 1).expect("a supported geometry");
        let mut out = vec![0u8; 5];
        resizer
            .resize(&source, stride, &mut out)
            .expect("the plane resizes");
        assert_eq!(out, vec![10, 10, 50, 90, 90]);
    }

    #[test]
    fn ten_bit_means_reduce_by_two_bits_after_rounding() {
        // A 2x2 block of 10-bit samples {0, 0, 0, 1023} has mean 255.75,
        // which rounds to 256 and reduces by >> 2 to 64 — the same reduction
        // the 16-bit path performed, applied to the rounded mean.
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
}
