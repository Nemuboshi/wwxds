//! Faithful Rust port of the pinned WWXD `detection.c` decision core.
//!
//! Every function mirrors one reference function and keeps its `detection.c`
//! line anchor in a comment. The port depends only on `core`/`std` and the
//! crate's frame-index types. It has no decoder, FFmpeg, or CLI dependency.
//!
//! Integer widths mirror C `int` (`i32`). Section 6 of
//! [`crate::detect`] (docs/WWXDDesign.md) proves every accumulator stays inside
//! `i32` for the legal analysis range, so no wrapping is used. `debug_assert!`
//! pins the documented maxima; a panic there means a transcription error, not a
//! reachable input.

use super::types::{DecisionReason, Scores};

/// `#define MV_MAX_ERROR (4096 * 256)` (detection.c:16) and its value.
const MV_MAX_ERROR: i32 = 1_048_576;
/// `#define iDiamondSize 2` (detection.c:11).
const DIAMOND_SIZE: i32 = 2;
/// `#define MODE_INTER 0` (detection.c:18).
const MODE_INTER: i32 = 0;
/// `#define MODE_INTRA 3` (detection.c:19).
const MODE_INTRA: i32 = 3;

/// `typedef struct { int x, y; } VECTOR` (detection.h).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Vector {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// `#define MVequal(A,B)` (detection.c:9).
fn mv_equal(a: Vector, b: Vector) -> bool {
    a.x == b.x && a.y == b.y
}

/// `typedef struct { VECTOR mvs[4]; int32_t sad16; int mode; } MACROBLOCK`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MacroBlock {
    pub(crate) mvs: [Vector; 4],
    pub(crate) sad16: i32,
    pub(crate) mode: i32,
}

impl Default for MacroBlock {
    fn default() -> Self {
        Self {
            mvs: [Vector::default(); 4],
            sad16: 0,
            mode: MODE_INTER,
        }
    }
}

/// `typedef struct { ... } MBParam` (detection.h) reduced to the fields the
/// core reads. All values are within `u32`/`i32` bounds validated by the state
/// module before a `Geometry` is built.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Geometry {
    /// Source content width in pixels (`MBParam.width`).
    pub(crate) width: u32,
    /// Source content height in pixels (`MBParam.height`).
    pub(crate) height: u32,
    /// Stride of a padded plane in bytes (`MBParam.edged_width`).
    pub(crate) edged_width: usize,
    /// Macroblock column count (`MBParam.mb_width`).
    pub(crate) mb_width: u32,
    /// Macroblock row count (`MBParam.mb_height`).
    pub(crate) mb_height: u32,
    /// Byte offset of the inner (border-stripped) origin in a padded plane.
    ///
    /// The reference harness passes `plane + 64 * edged_width + 64` as
    /// `pRef`/`pCurrent` (`oracle/wwxd-reference/reference.c`); every core coordinate is
    /// relative to that origin.
    pub(crate) inner_base: usize,
}

/// `typedef struct { ... } SearchData` (detection.h). Reused scratch across all
/// blocks and frames; the caller zeroes it once at construction (the reference
/// relies on the always-run `(0,0)` candidate to write every field before it is
/// read — docs/WWXDDesign.md §5, §7 — so zeroing is a safety belt, not a
/// semantic choice).
#[derive(Clone, Debug)]
pub(crate) struct SearchData {
    min_dx: i32,
    max_dx: i32,
    min_dy: i32,
    max_dy: i32,
    i_min_sad: [i32; 5],
    current_mv: [Vector; 5],
    temp: [i32; 4],
    dir: i32,
    pred_mv: Vector,
    /// Absolute byte offset of the current MB's 16-px origin in the current
    /// plane (`SearchData.Cur`).
    cur_byte: usize,
    /// Absolute byte offset of the reference MB's 16-px origin in the reference
    /// plane (`SearchData.RefP[0]`).
    ref_byte: usize,
    /// Padded-plane stride (`SearchData.iEdgedWidth`).
    i_edged_width: usize,
}

impl Default for SearchData {
    fn default() -> Self {
        Self {
            min_dx: 0,
            max_dx: 0,
            min_dy: 0,
            max_dy: 0,
            i_min_sad: [MV_MAX_ERROR; 5],
            current_mv: [Vector::default(); 5],
            temp: [0; 4],
            dir: 0,
            pred_mv: Vector::default(),
            cur_byte: 0,
            ref_byte: 0,
            i_edged_width: 0,
        }
    }
}

/// Byte offset of the top-left pixel of macroblock-grid cell `(gx, gy)` inside a
/// padded plane, given geometry `g`. Mirrors `pCur + (x + y * edged_width) * 16`
/// (detection.c:321-322).
///
/// The result is a `usize` because every caller indexes a slice with it; the
/// values are inside the validated plane for every reachable macroblock.
fn mb_byte(g: &Geometry, gx: u32, gy: u32) -> usize {
    g.inner_base + (gx as usize + gy as usize * g.edged_width) * 16
}

/// `static uint32_t sad16_c(cur, ref, stride)` (detection.c:27-63).
///
/// Sums the absolute difference over a 16x16 block. `cur` and `ref_` are absolute
/// byte offsets of the block origin in the two padded planes. Bounds are proven
/// in docs/WWXDDesign.md §6; the assertion catches a mis-ported clamp.
///
/// On x86-64 the row sum runs on SSE2 `PSADBW` (baseline on every x86-64 CPU;
/// no runtime detection is needed). `PSADBW` sums the absolute differences of
/// eight unsigned bytes per 64-bit lane, so the two lane sums of a 16-byte row
/// are exactly the per-pixel `abs_diff` values the portable loop adds; the
/// order of the additions matches the portable loop row by row, and a row
/// pair-sum of at most 16 × 255 = 4080 fits a `u32` (and an `i64`) without
/// wrap. The portable formulation stays for other targets and for the tests
/// that compare the two on fixed inputs.
fn sad16(plane_cur: &[u8], plane_ref: &[u8], ew: usize, cur: usize, ref_: usize) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        // The scalar path below would panic through its slice bounds on a bad
        // origin; the SIMD path reads raw pointers, so the same condition is
        // asserted in release too. The predicate is the documented bound from
        // docs/WWXDDesign.md §6, proven reachable only for valid macroblock
        // grids and clamped search offsets.
        assert!(cur + 15 * ew + 16 <= plane_cur.len());
        assert!(ref_ + 15 * ew + 16 <= plane_ref.len());
        use std::arch::x86_64::{
            _mm_cvtsi128_si64, _mm_loadu_si128, _mm_sad_epu8, _mm_unpackhi_epi64,
        };
        let mut acc = 0i64;
        for j in 0..16 {
            // SAFETY: the assertions above bound every row: the 16-byte load
            // at `cur + j * ew` (and `ref_ + j * ew`) reads bytes
            // `origin ..= origin + 15`, and `origin + 15 <= cur/ref_ + 15 * ew
            // + 15 < len`. The loads are unaligned by design (padded-plane
            // rows are not 16-byte aligned).
            let row = unsafe {
                let c = _mm_loadu_si128(plane_cur.as_ptr().add(cur + j * ew).cast());
                let r = _mm_loadu_si128(plane_ref.as_ptr().add(ref_ + j * ew).cast());
                _mm_sad_epu8(c, r)
            };
            // SAFETY: both expressions read the 64-bit lanes of the `row`
            // value computed above; extracting lanes of a local vector is
            // always sound.
            acc +=
                unsafe { _mm_cvtsi128_si64(row) + _mm_cvtsi128_si64(_mm_unpackhi_epi64(row, row)) };
        }
        // The accumulator is the exact SAD sum (maximum 255 × 256 = 65280),
        // so the widening to `i64` and the return to `u32` are both exact.
        acc as u32
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        debug_assert!(cur + 15 * ew + 16 <= plane_cur.len());
        debug_assert!(ref_ + 15 * ew + 16 <= plane_ref.len());
        // The rows are sliced before the pixel loop so the compiler sees a
        // 16-byte inner run and can vectorize it without a bounds check per
        // pixel.
        let mut sad = 0u32;
        for j in 0..16 {
            let c = &plane_cur[cur + j * ew..cur + j * ew + 16];
            let r = &plane_ref[ref_ + j * ew..ref_ + j * ew + 16];
            for k in 0..16 {
                sad += u32::from(c[k].abs_diff(r[k]));
            }
        }
        sad
    }
}

/// Portable formulation of `sad16_c`, kept for targets without SSE2 and as the
/// comparison reference for the SIMD path's tests.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn sad16_portable(plane_cur: &[u8], plane_ref: &[u8], ew: usize, cur: usize, ref_: usize) -> u32 {
    debug_assert!(cur + 15 * ew + 16 <= plane_cur.len());
    debug_assert!(ref_ + 15 * ew + 16 <= plane_ref.len());
    let mut sad = 0u32;
    for j in 0..16 {
        let c = &plane_cur[cur + j * ew..cur + j * ew + 16];
        let r = &plane_ref[ref_ + j * ew..ref_ + j * ew + 16];
        for k in 0..16 {
            sad += u32::from(c[k].abs_diff(r[k]));
        }
    }
    sad
}

/// `static uint32_t sad32v_c(cur, ref, stride, sad)` (detection.c:65-76).
///
/// Fills `temp[0..4]` with the four 16x16 quadrant SADs and returns their sum.
fn sad32v(
    plane_cur: &[u8],
    plane_ref: &[u8],
    ew: usize,
    cur: usize,
    ref_: usize,
    temp: &mut [i32; 4],
) -> i32 {
    temp[0] = sad16(plane_cur, plane_ref, ew, cur, ref_) as i32;
    temp[1] = sad16(plane_cur, plane_ref, ew, cur + 16, ref_ + 16) as i32;
    temp[2] = sad16(plane_cur, plane_ref, ew, cur + 16 * ew, ref_ + 16 * ew) as i32;
    temp[3] = sad16(
        plane_cur,
        plane_ref,
        ew,
        cur + 16 * ew + 16,
        ref_ + 16 * ew + 16,
    ) as i32;
    temp[0] + temp[1] + temp[2] + temp[3]
}

/// `static uint32_t dev16_c(cur, stride)` (detection.c:375-407).
///
/// Mean-absolute-deviation of a 16x16 block about its integer mean.
///
/// On x86-64 both passes run on SSE2 `PSADBW` (baseline instruction). The sum
/// pass is `PSADBW` against zero: the lane sums are byte sums of eight bytes,
/// which cannot overflow a lane (8 × 255 = 2040 < 2^63). The deviation pass
/// is `PSADBW` against the broadcast mean: an unsigned byte absdiff per byte
/// against a broadcast value, which is exactly `value.abs_diff(mean as u8)`
/// — the same truncation to `u8` the portable loop performs, and the
/// accumulator matches row by row. The portable formulation stays for other
/// targets and for the comparison tests.
fn dev16(plane: &[u8], ew: usize, origin: usize) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        assert!(origin + 15 * ew + 16 <= plane.len());
        use std::arch::x86_64::{
            _mm_cvtsi128_si64, _mm_loadu_si128, _mm_sad_epu8, _mm_set1_epi8, _mm_setzero_si128,
            _mm_unpackhi_epi64,
        };
        // SAFETY: SSE2 is in the x86-64 instruction-set baseline; building a
        // zero vector needs no feature check.
        let zero = unsafe { _mm_setzero_si128() };
        let mut sum = 0i64;
        for j in 0..16 {
            // SAFETY: the assertion bounds every row; a 16-byte unaligned
            // load reads `origin + j * ew .. origin + j * ew + 15`, within
            // `plane`.
            let row = unsafe { _mm_loadu_si128(plane.as_ptr().add(origin + j * ew).cast()) };
            // SAFETY: `PSADBW` against the zero vector sums the eight byte
            // values per lane; the lane result is at most 8 × 255 = 2040,
            // which fits the signed 64-bit lane read below.
            let summed = unsafe { _mm_sad_epu8(row, zero) };
            // SAFETY: lane extracts of the local value `summed`.
            sum += unsafe {
                _mm_cvtsi128_si64(summed) + _mm_cvtsi128_si64(_mm_unpackhi_epi64(summed, summed))
            };
        }
        let mean = (sum / 256) as u8;
        // SAFETY: broadcast of one byte value; every lane is `mean`.
        let broadcast = unsafe { _mm_set1_epi8(mean as i8) };
        let mut dev = 0i64;
        for j in 0..16 {
            // SAFETY: same bound as the first pass.
            let row = unsafe { _mm_loadu_si128(plane.as_ptr().add(origin + j * ew).cast()) };
            // SAFETY: `PSADBW` computes the unsigned absolute difference per
            // byte against the broadcast mean and sums eight of them per
            // lane; each lane result is at most 8 × 255 = 2040.
            let summed = unsafe { _mm_sad_epu8(row, broadcast) };
            // SAFETY: lane extracts of the local value `summed`.
            dev += unsafe {
                _mm_cvtsi128_si64(summed) + _mm_cvtsi128_si64(_mm_unpackhi_epi64(summed, summed))
            };
        }
        // The deviation sum is at most 256 × 255 = 65280, exact in both the
        // `i64` accumulator and the `u32` return.
        dev as u32
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        dev16_portable(plane, ew, origin)
    }
}

/// Portable formulation of `dev16_c`, kept for targets without SSE2 and as the
/// comparison reference for the SIMD path's tests.
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn dev16_portable(plane: &[u8], ew: usize, origin: usize) -> u32 {
    debug_assert!(origin + 15 * ew + 16 <= plane.len());
    let mut sum = 0u32;
    for j in 0..16 {
        let row = &plane[origin + j * ew..origin + j * ew + 16];
        for &value in row {
            sum += u32::from(value);
        }
    }
    let mean = sum / 256;
    let mut dev = 0u32;
    for j in 0..16 {
        let row = &plane[origin + j * ew..origin + j * ew + 16];
        for &value in row {
            dev += u32::from(value.abs_diff(mean as u8));
        }
    }
    dev
}

/// `static inline void get_range(...)` (detection.c:111-138).
///
/// `block_sz = 4`, `precision = 0`; `x`/`y` are macroblock-grid indices, so a
/// shift of 4 turns them into pixels. `fcode` here is already `fcode - 1`
/// (detection.c:318-319), so `search_range = 1 << (4 + fcode)` = `1 << 7` = 128
/// for the `fcode = 4` the reference harness always passes.
fn get_range(data: &mut SearchData, g: &Geometry, x: u32, y: u32, fcode: i32) {
    let search_range = 1 << (4 + fcode);
    let high = search_range - 1;
    let low = -search_range;

    let k = g.width as i32 - ((x as i32) << 4);
    data.max_dx = high.min(k);
    let k = g.height as i32 - ((y as i32) << 4);
    data.max_dy = high.min(k);
    let k = -(((x as i32) + 1) << 4);
    data.min_dx = low.max(k);
    let k = -(((y as i32) + 1) << 4);
    data.min_dy = low.max(k);
}

/// `static inline int vector_repeats(const VECTOR * pmv, unsigned int i)`
/// (detection.c:217-225). True if `pmv[i]` equals any earlier `pmv[j]`, `j < i`.
fn vector_repeats(pmv: &[Vector], i: usize) -> bool {
    for j in 0..i {
        if mv_equal(pmv[i], pmv[j]) {
            return true;
        }
    }
    false
}

/// `static inline int make_mask(const VECTOR * pmv, unsigned int i, unsigned
/// int current)` (detection.c:228-245). Builds the diamond direction mask that
/// skips vectors already checked as predictions.
fn make_mask(pmv: &[Vector], i: usize, current: usize) -> u32 {
    let mut mask: u32 = 255;
    let cur = pmv[current];
    for prior in &pmv[..i] {
        if cur.x == prior.x {
            if cur.y == prior.y + DIAMOND_SIZE {
                mask &= !4;
            } else if cur.y == prior.y - DIAMOND_SIZE {
                mask &= !8;
            }
        } else if cur.y == prior.y {
            if cur.x == prior.x + DIAMOND_SIZE {
                mask &= !1;
            } else if cur.x == prior.x - DIAMOND_SIZE {
                mask &= !2;
            }
        }
    }
    mask
}

/// `static VECTOR get_pmv2(const MACROBLOCK * mbs, int mb_width, int x, int y)`
/// (detection.c:141-214), for the only case the plugin selects (`block = 0`,
/// `bound = 0`). Returns the median (or single real) neighbour predictor.
fn get_pmv2(mbs: &[MacroBlock], mb_width: usize, x: i32, y: i32) -> Vector {
    // Left neighbour uses mvs[1]; top and top-right use mvs[2] (detection.c:159-161).
    let lx = x - 1;
    let ly = y;
    let tx = x;
    let ty = y - 1;
    let rx = x + 1;
    let ry = y - 1;

    let lpos = lx + ly * mb_width as i32;
    let rpos = rx + ry * mb_width as i32;
    let tpos = tx + ty * mb_width as i32;

    let mut pmv = [Vector::default(); 4];
    let mut num_cand = 0;
    let mut last_cand = 1;

    if lpos >= 0 && lx >= 0 {
        num_cand += 1;
        pmv[1] = mbs[lpos as usize].mvs[1];
    } else {
        pmv[1] = Vector { x: 0, y: 0 };
    }

    if tpos >= 0 {
        num_cand += 1;
        last_cand = 2;
        pmv[2] = mbs[tpos as usize].mvs[2];
    } else {
        pmv[2] = Vector { x: 0, y: 0 };
    }

    if rpos >= 0 && rx < mb_width as i32 {
        num_cand += 1;
        last_cand = 3;
        pmv[3] = mbs[rpos as usize].mvs[2];
    } else {
        pmv[3] = Vector { x: 0, y: 0 };
    }

    if num_cand > 1 {
        // Component median of pmv[1], pmv[2], pmv[3] (detection.c:204-209).
        Vector {
            x: pmv[1]
                .x
                .max(pmv[2].x)
                .min(pmv[2].x.max(pmv[3].x).min(pmv[1].x.max(pmv[3].x))),
            y: pmv[1]
                .y
                .max(pmv[2].y)
                .min(pmv[2].y.max(pmv[3].y).min(pmv[1].y.max(pmv[3].y))),
        }
    } else {
        pmv[last_cand]
    }
}

/// `static void CheckCandidate32I(x, y, data, Direction)` (detection.c:79-104).
///
/// Evaluates one 32x32 candidate MV for a macroblock pair and updates the best
/// whole-MB SAD (`current_mv[0]`, `dir`) and the four quadrant bests.
fn check_candidate_32i(
    x: i32,
    y: i32,
    direction: i32,
    data: &mut SearchData,
    plane_cur: &[u8],
    plane_ref: &[u8],
) {
    if x > data.max_dx || x < data.min_dx || y > data.max_dy || y < data.min_dy {
        return;
    }
    let mut temp = [0i32; 4];
    // detection.c:88 uses data->RefP[0] + x + y * data->iEdgedWidth.
    // get_range guarantees the candidate stays inside the padded plane, so the
    // signed delta is computed once and then applied as a `usize` offset.
    let ew = data.i_edged_width as isize;
    let ref_byte = data.ref_byte as isize + x as isize + y as isize * ew;
    debug_assert!(ref_byte >= 0);
    let total = sad32v(
        plane_cur,
        plane_ref,
        data.i_edged_width,
        data.cur_byte,
        ref_byte as usize,
        &mut temp,
    );
    data.temp = temp;

    if total < data.i_min_sad[0] {
        data.i_min_sad[0] = total;
        data.current_mv[0] = Vector { x, y };
        data.dir = direction;
    }
    // Quadrant bests compare the returned temp against iMinSAD[1..4]
    // (detection.c:96-103).
    if temp[0] < data.i_min_sad[1] {
        data.i_min_sad[1] = temp[0];
        data.current_mv[1] = Vector { x, y };
    }
    if temp[1] < data.i_min_sad[2] {
        data.i_min_sad[2] = temp[1];
        data.current_mv[2] = Vector { x, y };
    }
    if temp[2] < data.i_min_sad[3] {
        data.i_min_sad[3] = temp[2];
        data.current_mv[3] = Vector { x, y };
    }
    if temp[3] < data.i_min_sad[4] {
        data.i_min_sad[4] = temp[3];
        data.current_mv[4] = Vector { x, y };
    }
    // current_mv[4] mirrors detection.c:103. The pair write-back reads only
    // current_mv[0..4] for quadrants 0..3 and iMinSAD[1..4], so current_mv[4]
    // is written but not read; it is kept for a byte-faithful SearchData.
}

/// `static void xvid_me_DiamondSearch(...)` (detection.c:248-280).
fn diamond_search(
    mut x: i32,
    mut y: i32,
    mut b_direction: i32,
    data: &mut SearchData,
    plane_cur: &[u8],
    plane_ref: &[u8],
) {
    loop {
        data.dir = 0;
        if b_direction & 1 != 0 {
            check_candidate_32i(x - DIAMOND_SIZE, y, 1, data, plane_cur, plane_ref);
        }
        if b_direction & 2 != 0 {
            check_candidate_32i(x + DIAMOND_SIZE, y, 2, data, plane_cur, plane_ref);
        }
        if b_direction & 4 != 0 {
            check_candidate_32i(x, y - DIAMOND_SIZE, 4, data, plane_cur, plane_ref);
        }
        if b_direction & 8 != 0 {
            check_candidate_32i(x, y + DIAMOND_SIZE, 8, data, plane_cur, plane_ref);
        }
        if data.dir == 0 {
            break;
        }
        b_direction = data.dir;
        x = data.current_mv[0].x;
        y = data.current_mv[0].y;
        if b_direction & 3 != 0 {
            // Candidate is left or right: probe the vertical neighbours.
            check_candidate_32i(x, y + DIAMOND_SIZE, 8, data, plane_cur, plane_ref);
            check_candidate_32i(x, y - DIAMOND_SIZE, 4, data, plane_cur, plane_ref);
        } else {
            check_candidate_32i(x + DIAMOND_SIZE, y, 2, data, plane_cur, plane_ref);
            check_candidate_32i(x - DIAMOND_SIZE, y, 1, data, plane_cur, plane_ref);
        }
        b_direction |= data.dir;
        x = data.current_mv[0].x;
        y = data.current_mv[0].y;
    }
}

/// `static void xvid_me_SubpelRefine(...)` (detection.c:283-296).
fn subpel_refine(
    center: Vector,
    dir: i32,
    data: &mut SearchData,
    plane_cur: &[u8],
    plane_ref: &[u8],
) {
    let cx = center.x;
    let cy = center.y;
    check_candidate_32i(cx, cy - 1, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx + 1, cy - 1, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx + 1, cy, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx + 1, cy + 1, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx, cy + 1, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx - 1, cy + 1, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx - 1, cy, dir, data, plane_cur, plane_ref);
    check_candidate_32i(cx - 1, cy - 1, dir, data, plane_cur, plane_ref);
}

/// `static inline void MEanalyzeMB(...)` (detection.c:299-370).
// Eight parameters, one per the reference `MEanalyzeMB` (pRef, pCur, x, y,
// pParam, pMBs, Data, fcode); kept for the required 1:1 source mapping.
#[allow(clippy::too_many_arguments)]
fn me_analyze_mb(
    plane_ref: &[u8],
    plane_cur: &[u8],
    x: u32,
    y: u32,
    g: &Geometry,
    mbs: &mut [MacroBlock],
    data: &mut SearchData,
    fcode: i32,
) {
    for i in 0..5 {
        data.i_min_sad[i] = MV_MAX_ERROR;
    }

    // detection.c:318-319 passes fcode - 1 to get_range.
    get_range(data, g, x, y, fcode - 1);

    let origin = mb_byte(g, x, y);
    // detection.c:321-322 set Cur and RefP[0] to the same MB origin.
    data.cur_byte = origin;
    data.ref_byte = origin;

    // detection.c:441 (in MEanalysis) reset the pair MB's mvs[0] to (0,0) before
    // this call, so pmv0 is always the zero vector.
    let mb_index = (x + y * g.mb_width) as usize;
    let pmv0 = mbs[mb_index].mvs[0];

    // detection.c:327 — always-run candidate.
    check_candidate_32i(pmv0.x, pmv0.y, 0, data, plane_cur, plane_ref);

    let mut simplicity = 0i32; // detection.c:314
    if data.i_min_sad[0] > 200 {
        let mut pmv = [Vector::default(); 3]; // detection.c:311 VECTOR pmv[3]
        pmv[0] = pmv0;
        pmv[1] = Vector { x: 0, y: 0 };

        // Prediction MV selection (detection.c:335-342).
        if x == 1 && y == 1 {
            data.pred_mv = Vector { x: 0, y: 0 };
        } else if x == 1 {
            // left MB has no vector: use the top MB.
            data.pred_mv = mbs[mb_index - g.mb_width as usize].mvs[0];
        } else if y == 1 {
            // top MB has no vector: use the left MB.
            data.pred_mv = mbs[mb_index - 1].mvs[0];
        } else {
            data.pred_mv = get_pmv2(mbs, g.mb_width as usize, x as i32, y as i32);
        }

        pmv[2] = data.pred_mv;

        if !vector_repeats(&pmv, 2) {
            check_candidate_32i(pmv[2].x, pmv[2].y, 2, data, plane_cur, plane_ref);
        }

        if data.i_min_sad[0] > 500 {
            // diamond only if needed (detection.c:352-354)
            let mask = make_mask(&pmv, 3, data.dir as usize);
            let center = data.current_mv[0];
            diamond_search(center.x, center.y, mask as i32, data, plane_cur, plane_ref);
        } else {
            simplicity += 1;
        }

        if data.i_min_sad[0] > 500 {
            // refine 2-pixel to 1-pixel (detection.c:357-358)
            let center = data.current_mv[0];
            subpel_refine(center, 0, data, plane_cur, plane_ref);
        } else {
            simplicity += 1;
        }
    } else {
        simplicity += 1;
    }

    // Write the four sub-block predictors and sad16 (detection.c:362-369).
    for i in 0..4 {
        let sub_x = x + (i as u32 & 1);
        let sub_y = y + (i as u32 >> 1);
        let mb = (sub_x + sub_y * g.mb_width) as usize;
        mbs[mb].mvs = [data.current_mv[i]; 4];
        mbs[mb].mode = MODE_INTER;
        let mut sad = data.i_min_sad[i + 1] - (simplicity << 7);
        if sad < 0 {
            sad = 0;
        }
        mbs[mb].sad16 = sad;
    }
}

/// Result of one `me_analysis` call: the boolean cut, its reason, and the block
/// evidence. Mirrors the three `MEanalysis` return sites (detection.c:455, 470).
pub(crate) struct AnalysisResult {
    pub(crate) cut: bool,
    pub(crate) reason: DecisionReason,
    pub(crate) scores: Scores,
}

/// `int MEanalysis(pRef, pCurrent, pParam, pMBs, intraCount, fcode)`
/// (detection.c:410-471).
// Kept as a 1:1 map of the reference `MEanalysis`, whose C parameters are the two
// planes, MBParam, pMBs, intraCount, and fcode. Rust's plane borrows add two
// parameters over the C six; grouping them into one struct would obscure the
// line-for-line mapping this port is required to preserve (docs/WWXDDesign.md).
#[allow(clippy::too_many_arguments)]
pub(crate) fn me_analysis(
    plane_ref: &[u8],
    plane_cur: &[u8],
    g: &Geometry,
    mbs: &mut [MacroBlock],
    data: &mut SearchData,
    intra_count: i32,
    fcode: i32,
) -> AnalysisResult {
    let mut intra: u32 = 0; // detection.c:418 (uint32_t intra)
    let mut s_sad: i32 = 0; // detection.c:419 (int sSAD)
    let mut intra_thresh = 2000; // detection.c:420
    let mut intra_thresh2 = 90; // detection.c:421
    let mut blocks = 10; // detection.c:423
    let mut complexity: i32 = 0; // detection.c:424

    data.i_edged_width = g.edged_width;

    // Adaptive thresholds from shot age (detection.c:429-434).
    if intra_count > 0 && intra_count < 30 {
        if intra_count < 10 {
            intra_thresh += 15 * (10 - intra_count) * (10 - intra_count);
        }
        intra_thresh2 += 4 * (30 - intra_count);
    }

    let mb_width = g.mb_width;
    let mb_height = g.mb_height;
    // Majority threshold is a u32 expression in the reference (detection.c:454).
    let majority: u32 = ((mb_height - 2) * (mb_width - 2)) / 2;

    // Sampled macroblock pairs (detection.c:436-465).
    let mut y = 1u32;
    while y < mb_height.saturating_sub(1) {
        let mut x = 1u32;
        while x < mb_width.saturating_sub(1) {
            blocks += 10;
            let pair_mb = (x + y * mb_width) as usize;
            mbs[pair_mb].mvs[0] = Vector { x: 0, y: 0 }; // detection.c:441

            me_analyze_mb(plane_ref, plane_cur, x, y, g, mbs, data, fcode);

            // Four sub-blocks of the pair (detection.c:445-463).
            for i in 0..4 {
                let sub_x = x + (i as u32 & 1);
                let sub_y = y + (i as u32 >> 1);
                let mb = (sub_x + sub_y * mb_width) as usize;
                let dev = dev16(plane_cur, g.edged_width, mb_byte(g, sub_x, sub_y)) as i32;
                complexity += dev.max(300);

                if dev + intra_thresh < mbs[mb].sad16 {
                    mbs[mb].mode = MODE_INTRA;
                    intra += 1;
                    if intra > majority {
                        // Early exit (detection.c:454-455): loop statistics are
                        // incomplete, so activity/score are unavailable.
                        return AnalysisResult {
                            cut: true,
                            reason: DecisionReason::IntraMajority,
                            scores: Scores {
                                // sSAD / IntraThresh2 / intra are real partials at
                                // this exit; only the loop-final terms are unavailable.
                                motion_sum: Some(s_sad as i64),
                                activity: None,
                                sampled_blocks: None,
                                mean_sad: None,
                                threshold: Some(intra_thresh2 as i64),
                                intra_blocks: Some(intra as i64),
                                complete: false,
                            },
                        };
                    }
                }

                // detection.c:458-460: read after the write-back in MEanalyzeMB.
                if mbs[mb].mvs[0].x == 0
                    && mbs[mb].mvs[0].y == 0
                    && dev > 1000
                    && mbs[mb].sad16 < 1000
                {
                    s_sad += 512;
                }

                s_sad += if dev < 3000 {
                    mbs[mb].sad16
                } else {
                    mbs[mb].sad16 / 2
                };
            }
            x += 2;
        }
        y += 2;
    }

    complexity >>= 7; // detection.c:466

    let divisor = complexity + 4 * blocks; // detection.c:468 (>= 40, never zero)
    debug_assert!(divisor > 0);
    let mean_sad = s_sad / divisor;

    // Bounds from docs/WWXDDesign.md §6.
    debug_assert!(s_sad.unsigned_abs() <= 600_000_000);
    debug_assert!(complexity.unsigned_abs() <= 600_000_000);

    AnalysisResult {
        // detection.c:468-470 divides sSAD in place, THEN compares the quotient
        // to IntraThresh2. The cut uses the normalised value (`mean_sad`), not
        // the raw accumulated sum.
        cut: mean_sad > intra_thresh2,
        reason: if mean_sad > intra_thresh2 {
            DecisionReason::MeanSadHigh
        } else {
            DecisionReason::NoCut
        },
        scores: Scores {
            motion_sum: Some(s_sad as i64),
            activity: Some(complexity as i64),
            sampled_blocks: Some(blocks as i64),
            mean_sad: Some(mean_sad as i64),
            threshold: Some(intra_thresh2 as i64),
            intra_blocks: Some(intra as i64),
            complete: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(width: u32, height: u32) -> Geometry {
        let mb_width = width.div_ceil(16);
        let mb_height = height.div_ceil(16);
        let edged_width = (mb_width * 16 + 128) as usize;
        let inner_base = 64 * edged_width + 64;
        Geometry {
            width,
            height,
            edged_width,
            mb_width,
            mb_height,
            inner_base,
        }
    }

    /// A plane sized to cover the padded read extent for a `geo` fixture.
    fn plane_len(g: &Geometry) -> usize {
        let edged_height = (g.mb_height * 16 + 128) as usize;
        g.edged_width * (edged_height + 1) + 64
    }

    #[test]
    fn sad16_of_two_identical_blocks_is_zero() {
        let g = geo(48, 48);
        let a = vec![7u8; plane_len(&g)];
        let base = g.inner_base;
        assert_eq!(sad16(&a, &a, g.edged_width, base, base), 0);
    }

    #[test]
    fn sad16_sums_per_pixel_abs_difference() {
        let g = geo(48, 48);
        let cur = vec![200u8; plane_len(&g)];
        let mut ref_ = vec![100u8; plane_len(&g)];
        // Pixel (3, 15) differs by 50 instead of 100: 255 pixels at 100 + 1 at 50.
        ref_[g.inner_base + 15 * g.edged_width + 3] = 150;
        let base = g.inner_base;
        let expected: u32 = 255 * 100 + 50;
        assert_eq!(sad16(&cur, &ref_, g.edged_width, base, base), expected);
    }

    #[test]
    fn dev16_of_a_uniform_block_is_zero() {
        let g = geo(48, 48);
        let p = vec![128u8; plane_len(&g)];
        assert_eq!(dev16(&p, g.edged_width, g.inner_base), 0);
    }

    #[test]
    fn dev16_uses_the_integer_mean() {
        let g = geo(48, 48);
        // half pixels 0, half pixels 2 => mean = 1, dev = 256.
        let mut p = vec![0u8; plane_len(&g)];
        for j in 0..16 {
            for k in 8..16 {
                p[g.inner_base + j * g.edged_width + k] = 2;
            }
        }
        // mean = (128*2)/256 = 1; dev = 128*1 + 128*1 = 256.
        assert_eq!(dev16(&p, g.edged_width, g.inner_base), 256);
    }

    /// Deterministic pseudo-random fill (xorshift64*), so the SIMD-vs-portable
    /// comparison below is reproducible.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut value = seed;
        (0..len)
            .map(|_| {
                value ^= value >> 12;
                value ^= value << 25;
                value ^= value >> 27;
                (value.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as u8
            })
            .collect()
    }

    #[test]
    fn sad16_simd_and_portable_agree_on_noisy_planes() {
        // Production-like geometry: a 640x360 analysis plane padded the way
        // the detector pads it, with every legal 16x16 origin sampled on two
        // independently filled planes plus shifted copies.
        let g = geo(640, 360);
        let cur = noise(plane_len(&g), 0x9E37_79B9_7F4A_7C15);
        let ref_ = noise(plane_len(&g), 0xDEAD_BEEF_CAFE_F00D);
        for gy in 0..g.mb_height {
            for gx in 0..g.mb_width {
                let origin = mb_byte(&g, gx, gy);
                let portable = sad16_portable(&cur, &ref_, g.edged_width, origin, origin);
                assert_eq!(sad16(&cur, &ref_, g.edged_width, origin, origin), portable);
                // A shifted reference origin exercises the two planes reading
                // different absolute offsets, like a searched MV would.
                let shifted = origin as isize + 37 - 3 * g.edged_width as isize;
                let shifted = shifted.unsigned_abs() % (plane_len(&g) - 16 * g.edged_width);
                assert_eq!(
                    sad16(&cur, &ref_, g.edged_width, origin, shifted),
                    sad16_portable(&cur, &ref_, g.edged_width, origin, shifted),
                );
            }
        }
    }

    #[test]
    fn dev16_simd_and_portable_agree_on_noisy_planes() {
        let g = geo(640, 360);
        let plane = noise(plane_len(&g), 0x0123_4567_89AB_CDEF);
        for gy in 0..g.mb_height {
            for gx in 0..g.mb_width {
                let origin = mb_byte(&g, gx, gy);
                assert_eq!(
                    dev16(&plane, g.edged_width, origin),
                    dev16_portable(&plane, g.edged_width, origin),
                );
            }
        }
    }

    #[test]
    fn sad16_and_dev16_agree_on_extreme_fill_values() {
        // All-255 against all-0 maximises the SAD; all-0 mean keeps dev16's
        // truncation at its boundary. Both must match the portable loop.
        let g = geo(48, 48);
        let hi = vec![255u8; plane_len(&g)];
        let lo = vec![0u8; plane_len(&g)];
        let base = g.inner_base;
        assert_eq!(
            sad16(&hi, &lo, g.edged_width, base, base),
            sad16_portable(&hi, &lo, g.edged_width, base, base),
        );
        assert_eq!(sad16(&hi, &lo, g.edged_width, base, base), 255 * 256);
        assert_eq!(
            dev16(&hi, g.edged_width, base),
            dev16_portable(&hi, g.edged_width, base),
        );
        assert_eq!(dev16(&hi, g.edged_width, base), 0);
        assert_eq!(
            dev16(&lo, g.edged_width, base),
            dev16_portable(&lo, g.edged_width, base),
        );
    }

    #[test]
    fn get_range_clamps_to_the_content_or_search_bound() {
        // The harness calls MEanalysis with fcode = 4; MEanalyzeMB passes
        // fcode - 1 = 3 to get_range, so search_range = 1 << (4 + 3) = 128,
        // high = 127, low = -128.
        let mut data = SearchData::default();

        // Small content: the content bound binds (width - 16*x < 127).
        let g = geo(96, 64);
        get_range(&mut data, &g, 1, 1, 3);
        assert_eq!(data.max_dx, 80); // width 96 - 16*1 = 80 < 127
        assert_eq!(data.min_dx, -32); // -16*(1+1) = -32 > -128
        assert_eq!(data.max_dy, 48); // height 64 - 16*1 = 48 < 127
        assert_eq!(data.min_dy, -32);

        // Wide content: the search bound binds (width - 16*x > 127 -> 127).
        let g = geo(4096, 512); // at the area bound, so valid
        get_range(&mut data, &g, 1, 1, 3);
        assert_eq!(data.max_dx, 127); // min(127, 4096 - 16)
        assert_eq!(data.max_dy, 127); // min(127, 512 - 16)
    }

    #[test]
    fn make_mask_clears_prediction_direction_bits() {
        let pmv = [
            Vector { x: 0, y: 0 },
            Vector { x: 0, y: 0 },
            Vector { x: 2, y: 0 }, // current at index 2 differs from pmv[0] by +2 in x
            Vector { x: 0, y: 0 },
        ];
        // current = 2: pmv[2].x - pmv[0].x == 2 -> clears bit 1 (left neighbour).
        let mask = make_mask(&pmv, 3, 2);
        assert_eq!(mask & 1, 0);
        assert_ne!(mask & 4, 0);
    }

    #[test]
    fn vector_repeats_detects_an_earlier_duplicate() {
        let pmv = [
            Vector { x: 4, y: 4 },
            Vector { x: 0, y: 0 },
            Vector { x: 4, y: 4 },
        ];
        assert!(vector_repeats(&pmv, 2));
        assert!(!vector_repeats(&pmv, 1));
    }

    #[test]
    fn me_analysis_uniform_frames_returns_no_cut_after_first() {
        // Two identical uniform planes => sad ~0 => NoCut. (First-frame marker is
        // applied by the state module, not here.)
        let g = geo(96, 64);
        let len = plane_len(&g);
        let prev = vec![32u8; len];
        let cur = vec![32u8; len];
        let mut mbs = vec![MacroBlock::default(); (g.mb_width * g.mb_height) as usize];
        let mut data = SearchData::default();
        let r = me_analysis(&prev, &cur, &g, &mut mbs, &mut data, 1, 4);
        assert!(!r.cut);
        assert_eq!(r.reason, DecisionReason::NoCut);
        assert!(r.scores.complete);
        assert_eq!(r.scores.mean_sad, Some(0));
    }
}
