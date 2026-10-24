# WWXD Detector Design and Algorithm Specification

## 1. Reference Specification and Lineage

The detection core (`crates/wwxds-core/src/detect/`) implements the pure-Rust scene hard-cut detector. It reproduces the exact mathematical decisions of the reference implementation:

- **Reference Source**: `vapoursynth-wwxd` at commit `a5870862fd85138b5e4822a076377b96592c9cd2`.
- **Decision Core**: `detection.c` (`MEanalysis` and its internal static helpers).
- **Harness and State Reference**: `wwxd.c` and the reference oracle in `oracle/wwxd-golden`.
- **Fixed Constants**: `fcode = 4`, `edge_size = 64`, initial `intra_count = 1`.

The detector reproduces WWXD's motion-compensated block SAD and spatial energy algorithm. It is not an implementation of the full Fraunhofer 2004 research paper. Adaptive thresholds, flash detection, and slow-motion stages from the paper do not exist in `detection.c` and are not included here.

---

## 2. Data Types and Memory Structures

The data structures in `crates/wwxds-core/src/detect/core.rs` map directly to the reference types in `detection.h`:

| C Type (`detection.h`) | Rust Type (`core.rs`) | Definition and Purpose |
| :--- | :--- | :--- |
| `VECTOR` | `Vector { x: i32, y: i32 }` | Two-dimensional motion vector. |
| `MACROBLOCK` | `MacroBlock` | Stores four sub-block vectors (`mvs[4]`), `sad16`, and `mode`. |
| `MBParam` | `MbParam` | Grid geometry parameters and edge sizes. |
| `SearchData` | `SearchData` | Scratch memory reused across frames. |

`MODE_INTER = 0` and `MODE_INTRA = 3`. The `mode` field is diagnostic output and is not read during decision steps.

---

## 3. Plane Geometry and Edge Padding

Geometry values are computed once during detector initialization:

```text
mb_width     = (width  + 15) / 16
mb_height    = (height + 15) / 16
edged_width  = mb_width  * 16 + 128
edged_height = mb_height * 16 + 128
plane_bytes  = edged_width * (edged_height + 1) + 64
```

The detector allocates two ping-pong buffers (`previous` and `current`). Each frame is copied into `current` at byte offset `64 * edged_width + 64`:

1. **Source Rows**: For row $y \in [0, \text{height})$, copy `width` bytes from input offset $y \times \text{stride}$. Replicate the last pixel to column $\text{mb\_width} \times 16$.
2. **Padding Rows**: For row $y \in [\text{height}, \text{mb\_height} \times 16)$, replicate the last active row across `edged_width`.

Input strides are handled explicitly. The detector requires at least $(\text{height} - 1) \times \text{stride} + \text{width}$ bytes of valid input buffer.

Peak memory usage is strictly bounded:
- A single 4096×512 corner plane requires approximately 2.7 MB.
- Two ping-pong planes require approximately 5.4 MB.
- The macroblock table (`pMBs`) requires at most 320 KB.
- Per-pixel loops perform zero heap allocations.

---

## 4. Decision Core and Mathematical Rules

The function `MEanalysis` processes frames using fixed constants and adaptive thresholds:

### Numerical Constants
- `MV_MAX_ERROR = 1_048_576` ($4096 \times 256$)
- `iDiamondSize = 2`
- `IntraThresh` initial base = `2000`
- `IntraThresh2` initial base = `90`
- `blocks` initial base = `10`

### Shot-Age Adaptive Thresholds
Let $ic$ equal the current `shot_age` (`intra_count`). When $0 < ic < 30$:

$$\begin{aligned}
\text{If } ic < 10: \quad &\text{IntraThresh} \mathrel{+}= 15 \times (10 - ic)^2 \\
&\text{IntraThresh2} \mathrel{+}= 4 \times (30 - ic)
\end{aligned}$$

### Sampling Loop
The detector processes odd macroblock pairs on a stepped grid ($x, y$ step by 2):

1. **Candidate Evaluation**:
   - Reset pair anchor: `pMBs[x + y * mb_width].mvs[0] = (0, 0)`.
   - Run `MEanalyzeMB` to calculate motion vectors and SAD values.
2. **Energy and Mode Comparison**:
   - For each sub-block $i \in [0, 4)$:
     $$\text{dev} = \operatorname{dev16}(\text{current}, \text{origin})$$
     $$\text{complexity} \mathrel{+}= \max(\text{dev}, 300)$$
     $$\text{If } \text{dev} + \text{IntraThresh} < \text{pMB.sad16}: \quad \text{pMB.mode} = \text{MODE\_INTRA}, \quad \text{intra} \mathrel{+}= 1$$
3. **Early Exit Condition**:
   - If $\text{intra} > \frac{(\text{mb\_height} - 2) \times (\text{mb\_width} - 2)}{2}$, return immediately with a scene cut decision (`IntraMajority`).
4. **Accumulation**:
   - If motion is zero (`mvs[0] == (0, 0)`), $\text{dev} > 1000$, and $\text{sad16} < 1000$, add bias: $\text{sSAD} \mathrel{+}= 512$.
   - If $\text{dev} < 3000$, add $\text{sad16}$; otherwise add $\frac{\text{sad16}}{2}$.

### Final Decision
$$\text{complexity} \mathrel{\gg}= 7$$
$$\text{mean\_sad} = \left\lfloor \frac{\text{sSAD}}{\text{complexity} + 4 \times \text{blocks}} \right\rfloor$$
$$\text{cut} = (\text{mean\_sad} > \text{IntraThresh2})$$

---

## 5. Integer Bounds and Overflow Proofs

All core arithmetic uses signed 32-bit integers (`i32`). Overflow is mathematically impossible within documented bounds:

| Quantity | Upper Bound Formula | Maximum Value | Integer Range |
| :--- | :--- | :--- | :--- |
| Sub-block SAD (`sad16`) | $16 \times 16 \times 255$ | 65,280 | Within `i32` |
| Macroblock sum (`sad32v`) | $4 \times 65,280$ | 261,120 | Within `i32` |
| Block deviation (`dev16`) | $16 \times 16 \times 255$ | 65,280 | Within `i32` |
| Adaptive `IntraThresh2` | $90 + 4 \times 29$ | 206 | Within `i32` |
| Total sub-blocks ($G$) | Maximum pixel limit / 256 | 8,323 | Within `i32` |
| Total motion sum (`sSAD`) | $8,323 \times (65,280 + 512)$ | $\approx 5.5 \times 10^8$ | $< 2^{31} - 1$ |
| Total `complexity` | $8,323 \times 65,280$ | $\approx 5.4 \times 10^8$ | $< 2^{31} - 1$ |
| Divisor | $(\text{complexity} \gg 7) + 4 \times \text{blocks}$ | $\ge 40$ | Never zero |

Signed integer division truncates toward zero identically in Rust and C.

---

## 6. Detector State Machine and Execution Order

The `Detector` state machine manages deterministic per-frame processing:

```text
Input Frame
    │
    ▼
1. Validate Frame (Reject invalid dimensions, gap in index, or bad stride)
    │
    ▼
2. Edge Padding (Copy and replicate pixels into `current` plane)
    │
    ▼
3. Compute Decision (Skip MEanalysis on frame 0; execute MEanalysis on index >= 1)
    │
    ▼
4. Update Shot Age (Reset to 1 on cut; increment by 1 on non-cut)
    │
    ▼
5. Swap Buffers (Ping-pong swap `previous` and `current`)
    │
    ▼
6. Increment Index (Advance `next_index` by 1)
```

### State Isolation Invariants
- If a frame is rejected in step 1, the detector state does not change.
- `pMBs` retains unwritten boundary cells across frames. Macroblock predictor state carries over in display order.
- Frame 0 is special: it seeds the `previous` buffer and returns `reason = FirstFrame`. It never generates a detector cut boundary. The external CLI writer adds index 0 to `.kf.txt`.
- Stream completion (`finish()` or `flush()`) is idempotent. A completed detector rejects subsequent frames.

---

## 7. Decision Output and Diagnostics

The detector emits a `Decision` struct for each frame:

```rust
pub struct Decision {
    pub boundary: Option<BoundaryFrameIndex>,
    pub reason: DecisionReason,
    pub scores: Scores,
}
```

- **`boundary`**: Contains `Some(index)` when a hard cut occurs on index $\ge 1$. Contains `None` for non-cuts and frame 0.
- **`reason`**:
  - `FirstFrame`: Emitted on stream start (frame index 0).
  - `IntraMajority`: Emitted when intra-block count triggers early return.
  - `MeanSadHigh`: Emitted when `mean_sad` exceeds `IntraThresh2`.
  - `NoCut`: Emitted when no scene boundary is detected.
- **`scores`**: Exposes diagnostic metrics (`motion_sum`, `activity`, `mean_sad`, `threshold`, `intra_blocks`). When early exit occurs, uncomputed fields are set to `None`.

---

## 8. Determinism and Execution Model

- **Deterministic Output**: For identical input pixel bytes and frame sequences, the detector produces bit-identical decisions and scores.
- **Single-Threaded State**: The `Detector` executes on a single thread. It cannot be parallelized across macroblocks because spatial predictor propagation and early exit raster states are strictly serial.
- **Decoupled Decoding**: Frame decoding and analysis-plane production run on a background producer thread (see `docs/Architecture.md`). The detector consumes complete frames sequentially through a bounded channel.

---

## 9. Fidelity Proof and Golden Oracle Verification

Correctness is proved through automated oracle parity checks:

- **Scope**: The golden vectors pin the *decision core* — the state machine over prepared 8-bit planes. They are independent of how those planes were produced: the analysis-plane filter lives upstream in `wwxds-media` (`docs/Architecture.md`, §4), and changing it requires corpus boundary-comparison evidence, not oracle regeneration.
- **Reference Oracle**: `oracle/wwxd-golden` compiles the pinned reference C code and runs against committed fixtures in `tests/fixtures/detect/`.
- **Golden Vectors**: The oracle generates `golden.json`, recording observable decisions, `shot_age`, and FNV-1a digests of macroblock predictors and padded reference regions.
- **Automated Verification**: Regular tests check Rust output against `golden.json` without requiring a C compiler. Dedicated CI jobs recompile the reference C code on Linux and verify zero diff against `golden.json`.
- **Stock Quirks**: Known stock omissions in the original WWXD reference (e.g., frames 3064, 3447, and 361 on the reference clip) are deliberately preserved to guarantee bit-for-bit parity.
