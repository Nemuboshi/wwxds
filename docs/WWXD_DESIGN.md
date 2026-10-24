# WWXD Detector Design

Design and source-mapping note for the pure-Rust WWXD detection core
(`crates/ihni-core/src/detect/`). It records the source mapping, equations,
constants, score kinds, geometry, state order, and integer bounds.

This document maps every rule to the pinned reference and separates the WWXD /
xvidcore lineage from project-specific instrumentation. It does not claim that
this port exactly reproduces the original Fraunhofer system.

## 1. Reference identity

- Source: `vapoursynth-wwxd` at commit `a5870862fd85138b5e4822a076377b96592c9cd2`.
- Decision core: `detection.c` (`MEanalysis` and its static helpers). The
  production port reproduces this file's decision core.
- Frame state and padding: `wwxd.c` (the plugin) is the semantic reference. The
  C oracle in `oracle/wwxd-golden` reproduces its harness behaviour
  (`create` / `process`, same padding, same `fcode`, same first-frame marker and
  shot-age update) and is the byte-for-byte control the Rust detector is
  compared against.
- Constants: `fcode = 4`, `edge_size = 64`, initial `intra_count = 1`
  (`golden.c`, `wwxd.c:113,121-123`).

This port is a faithful reproduction of **WWXD's `detection.c`**, which
implements a motion-compensated block SAD/energy test with xvidcore-derived
search. It is an implementation of that code, not of the Fraunhofer paper. The
adaptive threshold, flash, and slow-motion stages of the paper are not present
in `detection.c` and are not claimed here. Section 9 lists every project
extension.

## 2. Data types (detection.h → core.rs)

| C (`detection.h`) | Rust (`core.rs`) | Notes |
| --- | --- | --- |
| `VECTOR { int x, y }` | `Vector { x: i32, y: i32 }` | motion vector |
| `MACROBLOCK { VECTOR mvs[4]; int32_t sad16; int mode }` | `MacroBlock { mvs: [Vector; 4], sad16: i32, mode: i32 }` | per-MB predictor + SAD + mode |
| `MBParam { u32 width,height,edged_width,edged_height,mb_width,mb_height; int edge_size }` | `MbParam { ...: u32, edge_size: i32 }` | grid geometry |
| `SearchData { ... }` | `SearchData { ... }` | reused per-frame scratch |

`MODE_INTER = 0`, `MODE_INTRA = 3` (`detection.c:18-19`). `mode` is written but
never read inside `MEanalysis`; it is pure output/evidence.

## 3. Geometry and padding (golden.c → state.rs)

Derived once per size, exactly as the reference harness does:

```
mb_width  = (width  + 15) / 16          // integer div, ceil to macroblocks
mb_height = (height + 15) / 16
edged_width  = mb_width  * 16 + 128     // == 2 * edge_size (64) border each side
edged_height = mb_height * 16 + 128
plane_bytes  = edged_width * (edged_height + 1) + 64
```

Each frame is copied into a two-plane ping-pong (`previous`, `current`, the two
reference-harness allocations), centred at byte offset `64 * edged_width + 64`,
then edge-replicated:

- Per source row `y` in `[0, height)`: copy `width` bytes from the input at
  `y * stride`, then replicate the last source pixel across the columns up to
  `padded_width = mb_width * 16`.
- Rows `[height, mb_height * 16)`: copy the last source row across the full
  `edged_width`.

`stride` is honoured: the input byte for `(x, y)` is `y * stride + x`. The
buffer-length requirement is `(height - 1) * stride + width` bytes (the last
row's final copied byte), matching the reference harness's read extent; a frame
that ends exactly there is accepted, one byte less is rejected. The decoder
emits a tight plane (`stride == width`); the detector is validated at the
boundary for the general stride case, and the port must not assume
`stride == width`.

Two padded planes are required for byte-exact parity: `current` is written while
`previous` is read by `MEanalysis`. Memory is bounded peak, not one plane: one
plane is at most ~2.7 MB at the 4096×512 corner, two planes at most ~5.4 MB, and
`pMBs` at most `8192 · 40 B ≈ 320 KB`. There is no per-pixel heap allocation in
the decision loops; buffers are reused across frames.

## 4. Decision core (detection.c → core.rs)

`MEanalysis(pRef, pCurrent, pParam, pMBs, intraCount, fcode)` maps line-for-line:

```
constants:  MV_MAX_ERROR = 4096*256 = 1048576      (detection.c:16)
            iDiamondSize = 2                        (detection.c:11)
            IntraThresh base = 2000, IntraThresh2 base = 90   (detection.c:420-421)
            blocks base = 10                        (detection.c:423)

adaptive shot-age thresholds (detection.c:429-434), with ic = intraCount:
  if 0 < ic < 30:
    if ic < 10:  IntraThresh  += 15 * (10 - ic) * (10 - ic)   // max +1215 at ic=1
    IntraThresh2 += 4 * (30 - ic)                              // max +116 at ic=1

sampling loop (detection.c:436-465): odd macroblock pairs,
  for y in 1 ..< mb_height-1 step 2:
    for x in 1 ..< mb_width-1  step 2:
      blocks += 10
      pMBs[x + y*mb_width].mvs[0] = (0, 0)          // reset the pair anchor
      MEanalyzeMB(...)                               // fills 2x2 sub-block predictors
      for i in 0..4:                                 // four sub-blocks of the pair
        dev = dev16(current, (x+(i&1)) + (y+(i>>1)) * edged_width)
        complexity += max(dev, 300)
        if dev + IntraThresh < pMB.sad16:
          pMB.mode = MODE_INTRA
          if ++intra > ((mb_height-2)*(mb_width-2))/2:   // u32 comparison
            return 1                                     // EARLY EXIT (partial stats)
        if pMB.mvs[0] == (0,0) and dev > 1000 and pMB.sad16 < 1000:
          sSAD += 512
        sSAD += if dev < 3000 { pMB.sad16 } else { pMB.sad16 / 2 }

  complexity >>= 7
  sSAD /= complexity + 4 * blocks                     // integer, trunc toward zero
  return sSAD > IntraThresh2                          // (detection.c:466-470)
```

`MEanalyzeMB` (detection.c:299-370) runs, per sampled pair, in this exact order:

1. `iMinSAD[0..5] = MV_MAX_ERROR`.
2. `get_range(..., fcode - 1)` → window (see §5).
3. Set `Cur`, `RefP[0]` at the pair's 16-px origin in the edged planes.
4. `pmv[0] = pMB.mvs[0]`; `CheckCandidate32I(pmv[0], direction = 0)` — the
   always-run candidate.
5. If `*iMinSAD > 200`: build `predMV` (top at `x==1`, left at `y==1`, else
   `get_pmv2` median), test it if not a repeat, then — only if `*iMinSAD > 500` —
   run `DiamondSearch` with `make_mask`; otherwise count a `simplicity` skip.
   Then if still `> 500` run `SubpelRefine`; else another skip.
6. Write the four sub-block predictors `currentMV[i]`, `mode = MODE_INTER`, and
   `sad16 = iMinSAD[i+1] - (simplicity << 7)`, clamped up to 0.

`get_pmv2` (detection.c:141-214): left neighbour uses `mvs[1]`, top and
top-right use `mvs[2]`; with more than one candidate it returns the component
median, else the single real candidate (`last_cand` is set only by the top and
top-right branches). The `block`, `bound` constants are the `0` case (the plugin
never selects another).

`sad16`/`sad32v`/`dev16` are the byte sums in `detection.c:27-76,375-407`.

## 5. Search window arithmetic (get_range)

`search_range = 1 << (4 + fcode)`, `high = search_range - 1`, `low = -search_range`
(`detection.c:125-127`). `MEanalysis` is always called with `fcode = 4`
(`golden.c`); `MEanalyzeMB` passes `fcode - 1 = 3` to `get_range`
(`detection.c:318-319`). So:

```
search_range = 1 << (4 + 3) = 128        high = 127        low = -128
max_dx = min(127, width  - 16*x)         min_dx = max(-128, -16*(x+1))
max_dy = min(127, height - 16*y)         min_dy = max(-128, -16*(y+1))
```

`width ≥ 48` and sampled `x ≤ mb_width-2` make `max_dx ≥ 17 > 0 ≥ min_dx`, and
likewise for y, so the always-run `(0,0)` candidate in step 4 is never clipped.
This is the reason `currentMV[0..4]` and `iMinSAD[1..4]` are always written
before they are read (§7), so the reused `SearchData` scratch carries no
indeterminate value into the write-back.

## 6. Integer widths and overflow bounds

All decision arithmetic uses signed `i32`, matching C `int`, with no wrapping
and no `i64` narrowing. The bound proof (why overflow is unreachable, and so a
plain-`i32` Rust port equals gcc's `int` on every `-O` level and compiler):

```
per-sub-block SAD  sad16 ≤ 16*16*255 = 65,280
per-MB    sum      sad32v ≤ 4 * 65,280 = 261,120   (fits i32; < MV_MAX_ERROR)
dev16              dev ≤ 65,280 ; dev + IntraThresh ≤ 65,280 + 3,215
IntraThresh2       ≤ 90 + 4*29 = 206
sampled sub-block visits G ≤ mb_width*mb_height ≤ 8,192 (2,097,152 px / 256, plus
                     the /16 ceil margin, bounded < 8,323)
|sSAD|             ≤ 8,323 * (65,280 + 512) ≈ 5.5e8  <  2^31 - 1
complexity (before >>=7) ≤ 8,323 * 65,280 ≈ 5.4e8    <  2^31 - 1
blocks             ≤ 10 + 10*(G/4) ≈ 20,820 ; divisor (complexity>>7)+4*blocks ≥ 40 > 0
```

`sSAD` is non-negative (all adds are ≥ 0), so `sSAD / divisor` and
`pMB.sad16 / 2` truncate toward zero identically in C and Rust (`/` on `i32`).
`dev16` uses signed `abs` of `(pixel - mean)`, each in `[-255, 255]`. `debug_assert!`
pins the maxima (`sad32v ≤ 261_120`, `sSAD ≤ 600_000_000`, `|complexity| ≤
600_000_000`) so a transcription error surfaces as a test failure, not a silent
decision flip. A checked-indexing debug mode asserts the 32×32 read window
`origin + 15*edged_width + 31 < len`; a panic means a mis-ported clamp.

## 7. State and ordering (state.rs)

One `Detector` owns `previous`, `current`, `pMBs`, `SearchData`, the geometry,
`next_index`, and `shot_age` (== C `intra_count`, `min` 1, saturating at
`i32::MAX`). Per frame, in the order the reference harness fixes
(a reordering here changes bits):

1. Validate the whole frame before any mutation (§8). A rejected frame leaves
   `previous`, `current`, `pMBs`, `shot_age`, and `next_index` byte-identical —
   this is the `AGENTS.md` stop condition "a detector state change after invalid
   input."
2. Pad the frame into `current`.
3. Compute the decision: `cut = first ? 1 : MEanalysis(previous, current, ...)`.
   At index 0 `MEanalysis` is NOT called (mirrors the reference harness).
4. Update `shot_age` BEFORE the swap: `cut → 1`, else `shot_age += 1`.
5. Swap `previous` and `current`. Advance `next_index`.

`pMBs` is never re-zeroed between frames. The 2×2 write-back updates only
sampled pairs and their `+1` neighbours; column 0 and (when `mb_width` is even)
the final column are never written, so their residue is real cross-frame
predictor state that `get_pmv2`'s left/top reads depend on. The port must
preserve that residue, exactly as the reference reuses one `calloc`'d array. This
is what keeps shot age and motion predictors in display order.

Index contract: the first accepted frame must have index 0 (there is no resume
API; a mid-stream start cannot establish shot age). Thereafter indices
increase by exactly 1; a gap, a duplicate, a downward index, or `finish`-then-`process`
is rejected before mutation. The detector rejects an analysis-size change
(width/height differing from construction).

## 8. Decision output and instrumentation (types.rs)

`Decision { boundary: Option<BoundaryFrameIndex>, reason: DecisionReason, scores: Scores }`.

- `reason` maps to the C return sites: `FirstFrame` (index 0), `IntraMajority`
  (early `return 1`, `detection.c:454-455`), `MeanSadHigh` (`sSAD > IntraThresh2`,
  `detection.c:470`), `NoCut`.
- `boundary` is `Some(BoundaryFrameIndex::new(index))` only when `cut` is true AND
  `index ≥ 1`. Frame 0 is `reason = FirstFrame, boundary = None`: the internal
  first-frame marker is never a detector boundary (enforced by
  `crates/ihni-core/src/frame_index.rs`). `crates/ihni-core/tests/detect.rs`
  explicitly checks that the detector never emits frame 0. The planned `.kf.txt`
  writer adds frame 0 once for a nonempty video, outside the detector.

This is the one deliberate divergence from the reference harness: the harness
keeps index 0 as a cut, the production detector discards it. `shot_age`
semantics are otherwise identical, so harness and production runs agree from
frame 1 onward.

`Scores` exposes the raw per-frame evidence the detector computes —
"raw motion, activity, block-count, score, and reason" — as diagnostics only; no
score is fed back into the decision:

| field | C source | meaning |
| --- | --- | --- |
| `motion_sum` | `sSAD` before `/=` | accumulated block motion |
| `activity` | `complexity >> 7` | block energy (None when partial) |
| `sampled_blocks` | `blocks` | sampled block counter (None when partial) |
| `mean_sad` | `sSAD` after `/=` | final score value (None when partial) |
| `threshold` | `IntraThresh2` | the value `mean_sad` is compared against |
| `intra_blocks` | `intra` | sub-blocks marked intra |
| `complete` | — | false on the `IntraMajority` early return |

On the `IntraMajority` early return the loop exits before `complexity >>= 7` and
`sSAD /= ...`, so `activity`, `sampled_blocks`, and `mean_sad` are `None` — never
fabricated as `0` — and `motion_sum`/`intra_blocks` are a partial tally
(missing statistics are marked partial or unavailable, never replaced with zero,
and a Boolean decision is never converted into a numeric change score). `Scores`
is library-internal and no production JSON schema is derived from it.

## 9. Project extensions vs the reference

Every rule in §4-§7 is from the pinned `detection.c` and the reference harness.
Project additions, none of which change a decision bit:

- `Scores` / `DecisionReason` instrumentation (§8) — diagnostic only.
- The frame-0 boundary discard for the production path (the reference harness
  keeps it as a cut).
- Gap/duplicate/downward index rejection and analysis-size-change rejection
  (`detection.c` has no index concept; these enforce the frame index contract).
- Validate-before-mutate ordering and the `finish`/`flush` EOF rule (§10).
- `debug_assert!` and the checked-indexing mode (§6).

## 10. EOF flush

`finish()` / `flush()` finalises the stream. It emits no `Decision`, advances no
index, and cannot invent a frame after the last. It is idempotent; a `process`
after `finish` returns `StateFinalized`. A zero-frame stream finalises cleanly.

## 11. Determinism and parallelism

Given identical prepared bytes and a fixed index sequence, repeated runs produce
identical `Scores` and boundaries. There is no RNG, clock, or unordered-iteration
dependence.

**One schedule.** One `Detector` runs on one thread and reproduces the
golden vectors bit for bit. No interior parallelism is introduced, because
parallel `pMBs` write-back would change the predictor-capture order: pair
`(ix, iy)` reads the predictors written by `(ix-1, iy)` and `(ix, iy-1)`, and
the intra-majority early exit leaves a raster prefix of written cells that the
next frame reads. Both are order-dependent state, not merely a data race.

**Frame supply.** Decoding may run on a producer thread ahead of the detector
(docs/DECODE.md). Frames still arrive one at a time, in display order, with the
bytes a serial read would produce, so the detector's input sequence and its
output are unchanged. The detector itself stays on one thread.

**Correction scope.** Approximate scheduling and a new detection mode are out
of scope. The correction plan removes the unused detection-thread CLI option.
Profile the exact implementation first and accept only measured optimizations
that preserve decisions and all recorded state; see [ROADMAP.md](ROADMAP.md).

## 12. Fidelity proof (golden vectors)

The claim "matches recorded WWXD decisions" is checked, not asserted:

- Input bytes are fixed and committed under `tests/fixtures/detect/` so the Rust
  detector and the C reference consume **identical prepared bytes**, not two
  independent preparation paths.
- The `oracle/wwxd-golden` crate links the pinned `detection.c` and its own
  independent harness (`oracle/wwxd-golden/golden.c`), replaying each fixture and
  emitting `golden.json`. Per frame it records the observables only: `cut`,
  `shot_age`, and FNV-1a digests over
  (a) the canonical raster-order field sequence of `pMBs` and (b) the padded
  reference-plane region. The digests pin every predictor vector, `sad16`, and
  `mode`, and the padding replication, without relying on C struct padding
  layout. `sSAD`, `complexity`, and `intra` are C locals with no accessor, so the
  oracle does **not** record them and no C-exactness is claimed for `Scores`.
- Default `cargo test` compares the Rust `cut`/`shot_age`/digests against the
  committed `golden.json` and never compiles C, so the offline suite needs no C
  toolchain. A dedicated CI step (and a local run, gcc is available) rebuilds
  the oracle from the pinned clone and asserts `git diff --exit-code` on
  `golden.json`, proving the committed vectors equal the live reference. The
  generation record notes the reference commit, the C compiler, and the toolchain.

The parity is scoped to the boolean decision and the pinned block state. The
three known stock omissions on the original diagnostic clip (frames 3064, 3447,
and 361) are preserved as-is: they are behaviour of the pinned reference, not
defects to be corrected in this port.
