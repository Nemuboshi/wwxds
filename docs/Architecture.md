# System Pipeline and FFI Architecture

## 1. Overview and Data Pipeline

IHNI detects scene hard-cuts and outputs keyframe lists for video files. The processing pipeline runs in one linear direction:

```text
Video File
    │
    ▼
Demux and Decode (libavformat / libavcodec)
    │  (display order, 8/10-bit luma)
    ▼
Analysis Plane (integer box filter)
    │  (exact integer area average, 8-bit samples)
    ▼
Bounded Channel (Producer-Consumer Queue)
    │  (bounded read-ahead slots)
    ▼
Scene Detection (ihni-core)
    │  (WWXD algorithm state machine)
    ▼
Atomic File Publisher
    │  (Aegisub .kf.txt format)
    ▼
Standard Error Progress Output
```

- Standard output (`stdout`) remains completely empty during execution.
- Keyframe results are published atomically to the target `.kf.txt` file after complete processing.
- Progress updates, operational statistics, and error messages write to standard error (`stderr`).

---

## 2. Crate Architecture and Boundaries

The workspace separates responsibilities into three isolated crates with strict one-way dependencies:

```text
ihni (CLI root binary)
  ├── depends on ──► ihni-media
  └── depends on ──► ihni-core
                      ▲
ihni-media ───────────┘ (depends on ihni-core)
```

### `ihni-core`
- Implements the pure WWXD detection algorithm and state machine.
- Has zero external dependencies.
- Has no knowledge of video containers, FFmpeg, file I/O, or command-line interfaces.
- Consumes only prepared 8-bit analysis frames and frame sequence metadata.

### `ihni-media`
- Handles video file probing, packet demuxing, video decoding, and frame scaling.
- Encapsulates FFmpeg libraries through an internal C shim.
- Provides a bounded producer-consumer channel for display-order frames.
- Depends only on `ihni-core`. It never depends on the root CLI package.

### Root Package (`ihni`)
- Parses CLI arguments with `clap`.
- Renders throttled progress updates and optional `RunStats` summaries to `stderr`.
- Writes keyframe lists atomically to disk through `tempfile`.

---

## 3. Video Decoding and Frame Contract

- **Presentation Order**: All decoded video frames are processed in presentation order (display order).
- **Stream Selection**: The reader selects the first video stream by default. Use `--stream-index` to set an absolute stream index. Invalid stream selections fail immediately.
- **Frame Indexing**: Frame indexes are zero-based, strictly continuous, and follow presentation order.
- **Timestamps**: Presentation timestamps (PTS) and stream time bases are preserved as exact rationals. Missing timestamps are kept as `None`. Timestamps are never generated from average frame rates.
- **Pixel Formats**: Supports 8-bit and 10-bit luma planes (`gray8`, `gray10le`, `gray10be`, and planar YUV luma). Color planes are not decoded. Frames are never converted to RGB.
- **Format Consistency**: Frame dimensions and pixel formats must remain constant. Mid-stream changes fail immediately.

---

## 4. Analysis Plane Contract

The analysis plane is produced by the project's own integer box (area-average) filter. It replaced the former swscale bicubic path, whose cost dominated the producer; the filter is defined so that every backend — CPU scalar/SIMD, CUDA, and compute shaders — computes byte-identical output.

- **Target Geometry**:
  - The default analysis height is 360 pixels.
  - The analysis width is calculated from the source aspect ratio:
    $$\text{width} = 4 \times \operatorname{round}\left(\frac{\text{source\_width} \times \text{height}}{4 \times \text{source\_height}}\right)$$
  - Dimensions must stay within 48 to 4096 pixels. Total frame area must not exceed 2,097,152 pixels.
- **Filter Definition**: One output sample is the area average of the source samples its unit square covers.
  - Output column $i$ covers the source interval $[i \cdot W / w,\ (i+1) \cdot W / w)$ in source-pixel units, where $W$ is the source width and $w$ the analysis width; rows are the same along $H$ and $h$.
  - Weights are exact integers: multiply both intervals by $w$ (respectively $h$), and the overlap of source pixel $x$ with output column $i$ is $\max(0, \min((i+1)W, (x+1)w) - \max(iW, xw))$ — an integer number of $1/w$ units.
  - The accumulator $\text{acc} = \sum \text{sample}(x, y) \cdot \text{weight}_x \cdot \text{weight}_y$ is exact in unsigned 64-bit integers for every accepted geometry.
  - The result is $\lfloor (\text{acc} + \lfloor w^2 h^2 / 2 \rfloor) / (w^2 h^2) \rfloor$: a rounded integer mean in the source bit depth. 10-bit sources are reduced to 8 bits afterwards by `>> 2` of the rounded mean; 8-bit sources are used directly. No floating-point arithmetic participates anywhere.
  - When $w > W$ (upsampling) the weights degenerate to 0/1 and the filter samples the nearest source pixel; the arithmetic is unchanged.
- **Strides**: Source planes are read under their actual strides. The analysis plane handed to the detector is tight ($w$ bytes per row).
- **Backend Parity**: The CPU implementation is the reference. A GPU backend must produce byte-identical planes on the committed fixtures; cross-backend agreement is tested, not assumed.
- **No Intermediates**: There is no 16-bit intermediate plane and no secondary reduction pass; the box sum produces the 8-bit sample in one operation.

---

## 5. FFI and C Shim Memory Model

All interactions with FFmpeg structures stay in `crates/ihni-media/csrc/ihni_ffmpeg.c`:

- **Encapsulation**:
  - Rust code never includes libav headers, names libav struct types, or assumes struct layouts.
  - `bindgen` and `libclang` are prohibited.
  - Rust uses hand-written `extern "C"` declarations with opaque pointers in `crates/ihni-media/src/ffi.rs`.
- **Safety Invariants**:
  - Every `unsafe` block must include a `SAFETY` comment that explains soundness.
  - Compiler flags deny `unsafe_op_in_unsafe_fn` and undocumented unsafe blocks.
- **Buffer Validity**:
  - Pointers returned from decoder frames are valid only until the next read on the same handle.
  - Frame data must be read or copied under its actual stride before the next decoder invocation.
- **Resource Management**:
  - Opaque handles implement `Drop` to release libav resources cleanly.

---

## 6. Concurrency and Threading Model

- **Producer-Consumer Design**:
  - A dedicated background thread executes demuxing, decoding, and analysis-plane production.
  - The main thread executes scene detection and progress reporting.
- **Bounded Channel**:
  - Communication uses a bounded channel with limited capacity (default 2 slots).
  - The channel prevents uncontrolled memory growth and eliminates full-video memory caching.
- **Shutdown Lifecycle**:
  - If detection completes early or fails, the reader handle drops.
  - The background worker thread detects the closed channel, exits its loop, and is joined cleanly.

---

## 7. Build and Static Link Contract

- **Platform Target**: Windows x64 MSVC using stable Rust.
- **FFmpeg Dependencies**:
  - Environment variable `FFMPEG_DIR` points to the static dev prefix.
  - `build.rs` compiles the C shim with the `cc` crate.
  - Static libraries linked: `avformat`, `avcodec`, `swresample`, `avutil`, `zlib`. swscale is no longer linked; the analysis-plane filter is the project's own integer code.
  - Dynamic runtime: links dynamically with the system CRT (`VCRUNTIME140.dll`).
- **Link Gate Verification**:
  - `scripts/check-link-gate.sh` inspects the release executable.
  - Rejects external FFmpeg dynamic DLL imports.

---

## 8. GPU Backends

GPU acceleration is phased work inside `ihni-media`; the CPU path is always built and is the fallback. The three phases:

1. **Phase 1 — Integer box filter on the CPU**: the analysis-plane filter of §4 replaces swscale, removing the dominant producer cost without any GPU dependency.
2. **Phase 2 — CUDA backend**: on NVIDIA hardware, decoding (NVDEC), the box filter, and the detector's per-macroblock feature extraction run on the device; only per-macroblock statistics cross back to the host for the serial state machine. All CUDA access lives in the C shim, compiled by `nvcc`; a build without a CUDA toolkit produces the CPU-only binary.
3. **Phase 3 — Vendor-neutral compute backend**: on integrated GPUs (shared memory), the CPU decodes and the device computes the box filter and feature extraction through a compute API. Adopted only if measured to beat the CPU path.

- **Selection**: `--gpu auto|cuda|vulkan|off` with `auto` as the default; probing failure degrades to the CPU path with a note on standard error.
- **Parity**: every backend produces the CPU implementation's bytes exactly (§4); results never depend on the selected backend.
- **Data residency**: decoded frames stay in device memory across the decode → filter → feature stages whenever the backend allows it; a per-frame host round trip must be measured before it is accepted.
- **Boundaries**: `ihni-core` stays device-agnostic and dependency-free; backend code never enters the root CLI beyond flag parsing.
