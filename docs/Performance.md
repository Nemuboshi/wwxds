# Performance Record

## 1. Methodology

- Benchmarks use release builds.
- Builds are compared on identical inputs, analysis sizes, and thread counts, in interleaved A/B runs (A, B, A, B).
- `wall_median_s` is the primary comparison metric; min and max show run-to-run variation.
- Report throughput in FPS and state whether decoding is included.

The `--stats` stages are not additive:

```text
producer thread:  producer_decode, producer_resize
consumer thread:  queue_wait, prepare, detect
```

Producer stages run concurrently with consumer stages. Never add producer and
consumer stage times together. `total_s` is end-to-end wall time.

## 2. Test Environment

- Windows x64, stable Rust, in-process static FFmpeg libraries (libav 9.0.2).
- Baseline input: `ramparts-120s.mkv`, H.264 1920x1080 8-bit, 23.976 FPS
  (2879 frames). Analysis sizes 640x360 (default), 1280x720, 1920x1080.
- Machine-identifying hardware and OS-edition details are deliberately not
  recorded here.

## 3. Stage Split (swscale era, 640x360, median of 6 interleaved runs)

```text
stats frames=2879 producer_decode_s=2.902 producer_resize_s=7.287 queue_wait_s=8.814 prepare_s=0.090 detect_s=1.306 total_s=10.225 fps=282.139
```

At the default analysis size the detector is not the bottleneck: `detect_s` is
about 13% of wall time, and scaling dominated the producer stage. This split
motivated §4.3 and §4.4. Per-frame `RunStats` clock reads cost less than
ambient noise (< 0.1% measured difference).

## 4. Accepted Optimizations

### 4.1 Row-slice inner loops (`wwxds-core`)
Scalar array indexing replaced with row slices and `usize` offsets.
`detect_s` 10.3 s → 1.5 s; wall 13.5 s → 9.9 s. Boundary lists identical.

### 4.2 Overlapped decoding (`wwxds-media`)
Decoding and scaling moved to a producer thread over a bounded channel.
At 1080p analysis, wall 25.4 s → 22.8 s; neutral at 640x360, where the
single-threaded decoder saturates. Boundary lists identical.

### 4.3 SSE2 `sad16`/`dev16` kernels (`wwxds-core`)
`PSADBW` row kernels with portable fallbacks. 120-frame replay, 5 interleaved
rounds: `sad16` 35.1 ms → 22.3 ms per replay, then `dev16` 22.3 ms → 18.7 ms.
Detector throughput 3,429 → 6,402 FPS. Reference parity and boundary lists
unchanged.

### 4.4 Integer box analysis filter; swscale removal (`wwxds-media`)
The swscale bicubic + `>> 8` producer path was replaced with the project's
exact integer box filter (`docs/Architecture.md` §4). swscale is no longer
linked.
- Parity: per-frame analysis digests and boundary lists identical to the
  swscale baseline on the whole corpus and both benchmark clips; the committed
  digest lists are unchanged.
- A/B (720-frame 1080p clip, interleaved): `producer_resize_s` 1.392 s →
  1.197 s, wall 1.661 s → 1.457 s, 433 → 494 FPS.
- Correction, recorded with §4.5: the first SSE2 row-sum kernel computed
  shifted sliding windows instead of block sums. The committed fixtures could
  not catch it; the GPU backend's byte-for-byte cross-check on real media
  did. The kernel was replaced with plain block sums, the
  `down-320x180-160x90` fixture was regenerated, and all digest and boundary
  comparisons were re-verified identical.

### 4.5 D3D11 hardware decode and filter backend (`wwxds-media`)
`--gpu auto|d3d11|off`. d3d11va keeps frames in GPU memory; a compute shader in
the C shim applies the whole-pixel box filter, and only the 8-bit analysis
plane crosses back to the host (`docs/Architecture.md` §8). The pinned FFmpeg
build ships no NVDEC/CUDA, which is why the backend is D3D11 rather than a
CUDA zero-copy path; integrated GPUs are covered by the same backend.
- Parity: per-plane byte-for-byte and end-to-end boundary parity with the CPU
  path, pinned by the GPU integration tests; corpus digest lists unchanged
  (their sources upscale, which the GPU backend declines).
- Measurement (720-frame 1080p clip, interleaved): `producer_resize_s`
  1.29-1.34 s (CPU) vs 0.86-0.89 s (D3D11); wall time about 1.5 s on both.
  On the reference machine the consumer stages and ambient noise dominate the
  wall clock, so the stage-level win does not yet change end-to-end time
  there; the removed filter contention is what scales on slower machines.
- Limitations: the shader covers every geometry the analysis-size derivation
  produces (§4.6); only a missing device or decoder capability falls back to
  the CPU filter. Requires Direct3D 11.3.

### 4.6 Integer-scale analysis sizes; fractional geometries removed
- **Change**: the analysis size is now the source divided by the smallest
  integer factor that brings the height to the target (360) or below; a
  source the factor cannot reach is still divided by its smallest common
  factor above one, and only a source with no common factor is analyzed
  natively — never upscaled or fractionally scaled (`docs/Architecture.md`
  §4). The filter's fractional
  coverage path — weight tables, 64-bit accumulators, a general vertical
  pass — is deleted, and the D3D11 shader now covers every geometry a run
  can have.
- **Motivation**: scaling is a speed heuristic, not a fidelity requirement.
  Whole-pixel-only scaling keeps the two backends on one arithmetic and
  removes half the filter code.
- **Behavior changes**: non-standard sources change analysis size — 720x576:
  452x360 → 360x288; 960x432: 800x360 → 480x216; 3840x2160: 640x360 →
  512x270 (the native area exceeds the pixel cap). Sources already at or
  below the 360 target — the whole m1 corpus — are analyzed natively instead
  of upscaled.
- **Parity evidence**: the committed analysis digests were regenerated
  through `examples/digest` (the corpus now digests at native sizes); the
  native digests are unchanged except `odd-97x65`, whose old "native" row
  was a fractional 97→96 width scale and whose new row is the true native
  plane. Every synthetic cut stays at its recipe frame (6 for the 96x64
  clips, 3 for the odd and minimum sizes), and the GPU/CPU byte parity tests
  pass on real media. The resize fixtures were rebuilt around whole-pixel
  geometries through the committed generator.

## 5. Rejected or Superseded Approaches

| Candidate | Outcome | Reason |
| :--- | :--- | :--- |
| Direct gray8 swscale output | ~6% of luma bytes differ | Breaks reference-vector parity. |
| Standalone d3d11va decode (pre-backend trial) | 7.4 s vs 8.0 s wall | Memory use and copy overhead; superseded by the §4.5 backend, which never brings full frames to the host. |
| Parallel macroblock search | Not evaluated | Motion predictors and early exits require strict serial order. |
| Bit-reduction rewrite (`chunks_exact`) | 0.1091 ms vs 0.1091 ms | Within measurement noise. |

## 6. Rules for Future Records

1. Never sum producer and consumer stages into a total.
2. High CPU utilization is not evidence of a speedup.
3. Record regressions next to gains.
4. The reference vectors of `docs/WWXDDesign.md` §9 are never regenerated to make
   an optimization pass.
5. An analysis-plane change must ship a corpus boundary comparison recorded
   here, plus regenerated fixtures and digest lists.
6. Do not record machine-identifying hardware, OS edition, or license details
   in this document.
