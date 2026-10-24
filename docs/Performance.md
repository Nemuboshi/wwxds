# Performance Record and Benchmark Methodology

## 1. Measurement Methodology and Protocols

Performance optimizations are accepted only when they improve measured throughput without changing detection output.

### Measurement Rules
- **Release Profile**: Use release builds (`cargo build --release`) for all benchmarks.
- **Identical Conditions**: Compare builds using identical input files, analysis dimensions, thread counts, and hardware.
- **Interleaved Execution**: Run builds in strict alternation (Build A, Build B, Build A, Build B). Interleaved runs distribute ambient operating system load equally.
- **Statistical Metrics**:
  - `wall_median_s`: Primary comparison metric across builds.
  - `wall_min_s`: Best estimate of minimum hardware execution time.
  - `wall_max_s`: Measure of run-to-run variation.
- **Throughput Reporting**: Report throughput in frames per second (FPS). Always state whether the number includes video decoding.

### Non-Additive Stage Times
The `--stats` output separates producer-side operations, queue waits, and consumer-side detection:

$$\text{Producer Thread: } [\text{producer\_decode}, \text{producer\_resize}]$$
$$\text{Consumer Thread: } [\text{queue\_wait}, \text{prepare}, \text{detect}]$$

- Producer stages run concurrently with consumer stages. Never add producer and consumer stage times together.
- `total_s` measures end-to-end wall time. It includes process start-up, probing, decoding, scaling, and detection.

---

## 2. Test Environment and Baseline Inputs

### Recorded Hardware and Software Environment
- **CPU**: a quad-core x86-64 CPU.
- **Operating System**: Windows 11 x64.
- **Rust Toolchain**: Stable Rust (Cargo and rustc 1.98.1).
- **FFmpeg Backend**: Static in-process FFmpeg libraries (libav* 9.0.2).

### Baseline Test Input
- **File**: `ramparts-120s.mkv` (120-second clip cut from 1080p source).
- **Format**: H.264, 1920×1080, 8-bit, 23.976 FPS (2879 frames).
- **Analysis Sizes**: 640×360 (default), 1280×720, and 1920×1080.
- **Decoder Threads**: FFmpeg automatic thread pool.

---

## 3. Stage Timing Analysis (`RunStats`)

The instrumented stage split localizes processing costs across the pipeline.

### Measured Stage Split (640×360 Analysis, Median of 6 Interleaved Runs)
```text
stats frames=2879 producer_decode_s=2.902 producer_resize_s=7.287 queue_wait_s=8.814 prepare_s=0.090 detect_s=1.306 total_s=10.225 fps=282.139
```

| Pipeline Component | Median Duration | Fraction of Pipeline | Analysis |
| :--- | ---: | ---: | :--- |
| `producer_decode` | 2.902 s | ~28% | H.264 decoding in libavcodec. |
| `producer_resize` | 7.287 s | ~71% | swscale scaling and `>> 8` reduction. |
| `queue_wait` | 8.814 s | Concurrent | Consumer waiting for ready frames. |
| `prepare` | 0.090 s | <1% | Edge replication padding in detector. |
| `detect` | 1.306 s | ~13% of wall | Serial WWXD decision engine. |
| `total_s` (wall) | 10.225 s | 100% | Full process elapsed time. |

### Timing Findings
1. **Resize Dominates Producer Time**: swscale reads full 1080p source frames and performs bicubic downscaling. It consumes more time than decoding.
2. **Decode-Bound at Default Resolution**: At 640×360, the detector runs faster than the producer. Detection accounts for only ~13% of total wall time.
3. **Instrumentation Cost**: Per-frame clock reads in `RunStats` cost less than ambient system noise (< 0.1% difference).

---

## 4. Accepted Optimizations and Verification Data

### 4.1 Inner-Loop Rewrite
- **Change**: Replaced scalar array indexing with row slices, `usize` offsets, and `slice::fill`.
- **Result**: `detect_s` dropped from 10.3 s to 1.5 s (6.8× speedup). Total wall time decreased from 13.5 s to 9.9 s.
- **Parity**: Generated 39 cut boundaries identical to the unoptimized baseline.

### 4.2 Overlapped In-Process Decoding
- **Change**: Decoupled decoding and scaling onto a dedicated background thread with a 2-slot bounded channel.
- **Result**:
  - At 1920×1080 analysis: Wall time decreased from 25.4 s to 22.8 s (~10% gain).
  - At 640×360 analysis: Wall time remained neutral due to single-core decoder saturation.
- **Parity**: Identical boundary lists (39, 46, and 50 boundaries across test heights).

### 4.3 SSE2 `sad16` SIMD Kernel (x86-64)
- **Change**: Implemented row-pair `PSADBW` instructions to compute absolute differences and horizontal sums. Maintained portable scalar fallback.
- **Replay Benchmark**: 120-frame replay through `examples/bench.rs --detector-only` (5 interleaved rounds):

| Round | Scalar Baseline Median | SSE2 `sad16` Median | Ratio |
| :---: | ---: | ---: | ---: |
| 1 | 34.992 ms | 22.958 ms | 0.656 |
| 2 | 35.282 ms | 22.555 ms | 0.639 |
| 3 | 35.593 ms | 22.212 ms | 0.624 |
| 4 | 35.107 ms | 22.273 ms | 0.634 |
| 5 | 34.646 ms | 22.159 ms | 0.640 |
| **Median** | **35.107 ms** | **22.273 ms** | **0.634** |

- **Throughput**: Detector throughput increased from 3,429 FPS to 5,388 FPS (~1.57× speedup).
- **Pipeline Effect**: Pipeline `detect_s` decreased from 1.221 s to 0.798 s on the full clip.
- **Parity**: All unit tests, digest tests, and `golden.json` replays pass with zero diff.

### 4.4 SSE2 `dev16` SIMD Kernel (x86-64)
- **Change**: Implemented `PSADBW` against zero vectors for block mean sums, and against broadcast means for deviation calculations.
- **Replay Benchmark**: 120-frame replay on top of SSE2 `sad16`:

| Round | `sad16`-Only Median | `sad16` + `dev16` Median | Ratio |
| :---: | ---: | ---: | ---: |
| 1 | 22.541 ms | 18.744 ms | 0.832 |
| 2 | 22.328 ms | 18.631 ms | 0.834 |
| 3 | 22.097 ms | 18.821 ms | 0.852 |
| 4 | 22.229 ms | 18.812 ms | 0.846 |
| 5 | 22.479 ms | 18.673 ms | 0.831 |
| **Median** | **22.328 ms** | **18.744 ms** | **0.840** |

- **Throughput**: Cumulative throughput rose from 3,429 FPS (scalar) to 6,402 FPS (1.85× total speedup).
- **Pipeline Effect**: Pipeline `detect_s` dropped to 0.560 s. At 1080p analysis, wall time dropped from 17.31 s to 16.17 s (~7% end-to-end gain).
- **Parity**: 149 test targets pass; digest lists and cut boundaries remain byte-identical.

### 4.5 swscale Dynamic Frame API Multithreading
- **Change**: Migrated from legacy `sws_scale` to `sws_alloc_context` and `sws_scale_frame` in dynamic mode. Enables multithreaded scaling.
- **End-to-End Benchmark** (1000-frame 1080p clips, default 488×360 analysis):

| Input Format | Pairs | Legacy Scaler Median | Dynamic Frame Scaler Median | Throughput Result |
| :--- | :---: | ---: | ---: | :--- |
| H.264 8-bit | 8 | 2.110 s | 1.891 s | **~10% faster** |
| H.264 10-bit | 4 | 2.122 s | 1.904 s | **~10% faster** |
| HEVC 8-bit | 4 | 2.259 s | 2.163 s | **~4% faster** |
| HEVC 10-bit | 8 | 4.272 s | 4.328 s | **~1.4% slower** (known regression) |

- **Parity**: Scaler outputs identical FNV-1a digests (`d170ba72a6f80fb5`) across thread counts 1 through 8.

### 4.6 Integer box analysis-plane filter (swscale removal)
- **Change**: Replaced the swscale `SWS_BICUBIC` + `gray16le >> 8` producer path with the project's own integer box (area-average) filter (`docs/Architecture.md` §4). The filter runs as two separable passes over a reused `u32` intermediate; whole-pixel downscales (every default analysis size) take an exact fast path with an SSE2 row-sum kernel on x86-64 and a precomputed magic multiplier for the per-pixel division. swscale is no longer linked.
- **Motivation**: `producer_resize` was the dominant producer stage (§3); the filter replacement is the CPU-side phase of the GPU plan (`docs/Architecture.md` §8).
- **Parity evidence**:
  - Per-frame analysis digests on all nine `m1-fixtures` corpus inputs are identical to the recorded swscale lists (both heights); the committed digest lists are unchanged.
  - Boundary lists are identical to the swscale baseline on all nine corpus inputs and on both 1080p benchmark clips.
  - The regenerated `resize` fixtures are deterministic across runs; the committed generator is `examples/gen_resize_fixtures`.
- **Filter micro-benchmark** (`examples/bench_resize`, 1080p source, 640x360 analysis, three runs, this machine):
  - gray8: 466–644 µs per frame
  - gray10le: 826–865 µs per frame
  - Reference swscale point: ~1.67 ms per frame at 8 decoder+scaler threads (§3-era measurement on the recorded hardware; not re-run in isolation).
- **End-to-end A/B, interleaved** (720-frame 1080p H.264 8-bit clip, default settings, three pairs, this machine):

| Build | `producer_resize_s` (median) | `total_s` (median) | FPS (median) |
| :--- | ---: | ---: | ---: |
| swscale bicubic | 1.392 s | 1.661 s | 433 |
| integer box filter | 1.197 s | 1.457 s | 494 |

- **Contention finding**: with `--decoder-threads 1`, the filter's pipeline stage drops to 0.617 s — matching the micro-benchmark — because FFmpeg frame-threading decodes the next frame concurrently with the resize of the current one. The remaining gap between micro-benchmark and pipeline time is that documented overlap, not filter cost.
- **Correction (recorded with the D3D11 backend work)**: the SSE2 row-sum kernel this section originally shipped computed shifted sliding windows instead of block sums for steps above one. The committed fixtures were generated by the same kernel and could not catch it — small-geometry unit tests only exercised the scalar tail, and the corpus exercises the general path. The defect surfaced through the GPU backend's byte-for-byte cross-check on real media; the kernel was replaced with plain block sums (`docs/Architecture.md` §4), the `down-320x180-160x90` fixture was regenerated, and the corpus digest lists and all boundary comparisons against the swscale baseline were re-verified identical. The micro-benchmark numbers above were measured with the defective kernel; the corrected build measures 1.12 ms (gray8) and 1.53 ms (gray10le) per frame on the same machine.

### 4.7 D3D11 hardware decode and analysis-filter backend
- **Change**: added `--gpu auto|d3d11|off`. In `d3d11` mode the decoder runs as d3d11va and its frames stay in GPU memory; a compute shader in the C shim computes the whole-pixel box filter and only the 8-bit analysis plane crosses back to the host (`docs/Architecture.md` §8). `auto` is the default and falls back to the CPU filter with a note on standard error.
- **Scope decision**: the pinned FFmpeg build disables NVDEC, Vulkan, and CUDA, so the originally planned NVDEC-to-CUDA zero-copy path is unavailable; the backend was re-based on the d3d11va support the pinned build ships. Integrated GPUs are covered by the same backend (shared memory, no PCIe copy), which folds the planned Vulkan phase into it.
- **Parity evidence**: `crates/ihni-media/tests/gpu.rs` compares every analysis plane of the two backends byte for byte on the 1080p benchmark clip; `tests/gpu.rs` compares the boundary lists end to end. Boundary lists match the CPU baseline on the whole corpus and both benchmark clips, 8-bit and 10-bit. The corpus digest lists are unchanged (their sources upscale, which the GPU backend declines and the CPU filter handles).
- **Measurement** (720-frame 1080p H.264 8-bit clip, default settings, this machine): `backend=d3d11` reports `producer_resize_s` ≈ 0.93 s against 1.20 s on the CPU filter — the per-frame staging read-back synchronizes the device, so the GPU stage is not free, but the decode stage drops to d3d11va cost and the CPU-side filter contention disappears. The 10-bit clip shows the same shape (0.65 s vs 0.87 s). Final interleaved A/B on the corrected CPU filter: `producer_resize_s` 1.29–1.34 s (CPU) vs 0.86–0.89 s (D3D11), while total wall time stays at ≈ 1.5 s on both — on this fast desktop the consumer stages and ambient noise dominate the wall clock, so the stage-level win does not yet translate end to end here; the hardware floor and the removed filter contention are what scale on slower machines.
- **Limitations**: the GPU path covers whole-pixel downscales only; other geometries fall back to the CPU filter. Requires Direct3D 11.3 (Windows 10 1511 or newer) and a decoder with d3d11va support.

---

## 5. Rejected or Deferred Approaches

| Candidate Optimization | Test Outcome | Rejection Reason |
| :--- | :--- | :--- |
| **Direct gray8 swscale output** | ~6% of luma bytes differ | Breaks bit-exact parity with WWXD golden vectors. Approximate scaling is prohibited. |
| **Hardware decoding (d3d11va)** | Wall time 7.4 s vs 8.0 s | Caused high memory usage, platform lock-in, and unpredictable copy overhead. |
| **Parallel macroblock search** | Not evaluated | Spatial motion predictors and early exit conditions require strict serial ordering. |
| **Parallel approximate detection** | Out of scope | Breaks golden vector reproduction and deterministic boundary generation. |
| **Bit-reduction rewrite** (`chunks_exact`) | 0.1091 ms vs 0.1091 ms | Performance change sat entirely within measurement noise (< 0.1%). |

---

## 6. Summary of Accepted Optimizations

| Optimization | Target Component | Measured Gain | Correctness Evidence |
| :--- | :--- | :--- | :--- |
| **Row-slice SAD and dev loops** | `ihni-core` | `detect_s`: 10.3 s → 1.5 s | Identical boundary lists; golden tests pass. |
| **In-process decoding and overlap** | `ihni-media` | Wall time: 25.4 s → 22.8 s (1080p) | 39/46/50 boundaries unchanged; identical digests. |
| **Producer/consumer stage split** | `ihni` CLI | Detailed timing visibility | Zero measurement overhead; no output changes. |
| **swscale dynamic frame API** | `ihni-media` | ~10% faster on 8-bit inputs | FNV-1a digests match across all thread counts. |
| **Integer box analysis filter** | `ihni-media` | `producer_resize_s`: 1.39 s → 1.20 s (§4.6) | Digests and boundaries identical to swscale baseline on the corpus. |
| **D3D11 hardware decode + filter** | `ihni-media` | `producer_resize_s`: 1.20 s → 0.93 s; decode stage moves to fixed-function hardware (§4.7) | Per-plane and boundary parity with the CPU path pinned by the GPU integration tests. |
| **SSE2 `sad16` SIMD kernel** | `ihni-core` | Replay: 35.1 ms → 22.3 ms | Golden parity verified; identical boundary lists. |
| **SSE2 `dev16` SIMD kernel** | `ihni-core` | Replay: 22.3 ms → 18.7 ms | 149 unit and integration tests pass. |

---

## 7. Performance Reporting Rules

When submitting future optimizations:
1. **Never add overlapping stages**: Do not sum `producer_decode`, `producer_resize`, and `queue_wait` into total elapsed time.
2. **No claims from CPU usage**: High CPU utilization is not proof of throughput improvement.
3. **Record regressions honestly**: Document any performance regressions alongside gains (e.g., 10-bit HEVC under multithreaded scaling).
4. **Decision-core parity is absolute**: The golden vectors of `docs/WwxdDesign.md` §9 are never regenerated to make an optimization pass.
5. **Analysis-plane changes need corpus evidence**: The analysis plane is now produced by the project's integer box filter (`docs/Architecture.md` §4), not swscale. A change to it must ship a boundary comparison over the committed video corpus, recorded here, alongside the regenerated plane fixtures and digest lists.
