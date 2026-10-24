# Agent Engineering Rules

## Reading and Evidence

- Define the question and the required scope before you read files.
- Combine related queries into one operation. Do not start a separate agent for each file.
- For JSON, JSONL, and large log files, inspect the structure, key fields, counts, or match locations first.
- Read source code by symbol, function, or line range.
- Read only the relevant sections of documents. Identify diff scope before you inspect hunks.
- Do not output large arrays, pixel data, or iteration records.
- Truncated output is not complete evidence. If output is truncated, use a more specific query.
- Identify the source, location, configuration, actual result, failures, and unknown items in reports.
- Keep missing fields, `null`, and `false` as separate states.
- Reuse existing results for the same question.

## CI and Repository Scripts

- Use Bash for all CI steps and repository automation scripts, including on Windows runners.
- Do not add PowerShell steps or scripts.
- Set `shell: bash` explicitly for all GitHub Actions command steps.
- Keep all Bash script files in LF line endings.

## Rust Development Rules

- Use stable Rust. Do not use nightly features unless approved.
- Follow Rust API Guidelines and standard Rust naming rules.
- Use explicit types for frame indexes, timestamps, dimensions, and scores to prevent invalid states.
- Keep functions small and focused. Do not add abstractions without a clear need.
- Keep dependencies to a minimum. Check the standard library before you add a crate.
- Do not use `unsafe` by default. If `unsafe` is necessary, document its safety conditions in a `SAFETY` comment and add tests.
- Do not use `unwrap()` or `expect()` on external input or recoverable errors. Return a structured error with context.
- Do not suppress compiler warnings or Clippy diagnostics without a documented reason.

## Architecture and Dependencies

- Keep video decoding separate from scene detection.
- The detection core must not depend on FFmpeg, a video container, or a CLI framework.
- Pass decoded frame data and frame metadata through explicit interfaces.
- Do not add video encoding, rate control, or bitstream generation to the detection core.
- Keep the detection algorithm independent of the input video codec.
- Do not copy Xvid or SCXvid implementation details into the detector.
- Follow `docs/Architecture.md` and `docs/WWXDDesign.md`. Do not add features outside the documented scope.
- Do not add a plugin system or an unused abstraction layer.
- Keep crate dependencies strictly one-way:
  - `wwxds-core` has no external dependencies.
  - `wwxds-media` depends only on `wwxds-core`.
  - The root package handles CLI, file output, and progress reporting.
  - Never make `wwxds-core` depend on `wwxds-media` or the root package.

## FFI and FFmpeg

- All access to `AVFormatContext`, `AVCodecContext`, `AVFrame`, and `AVPacket` must stay in `crates/wwxds-media/csrc/wwxds_ffmpeg.c`.
- No Rust file may include a libav header, name a libav type, or assume a struct offset.
- Do not add `bindgen` or a `libclang` dependency. Hand-written `extern "C"` declarations and the C shim are the contract.
- Every `unsafe` block must include a `SAFETY` comment that explains why the block is sound.
- A pointer to a decoded frame buffer is valid only until the next call on the same handle. Read data under its actual stride immediately.
- Use FFmpeg libraries directly through the C shim for reading. The analysis-plane filter no longer uses swscale; do not relink it.
- Do not bundle `ffmpeg.exe` or `ffprobe.exe`. Do not introduce FFMS2.
- Verify that the link gate allows intended symbols and rejects unexpected dependencies.

## GPU Backends

- GPU acceleration is an optional backend inside `wwxds-media` behind a documented selection flag (`--gpu`); the CPU path is always present and is the fallback.
- The backend is D3D11: d3d11va hardware decode keeps frames in GPU memory, and a compute shader in the C shim produces the analysis plane. Only the tight analysis plane crosses back to the host.
- All Direct3D and COM access stays in `crates/wwxds-media/csrc/`. No Rust file may include a Direct3D header or name a Direct3D type; hand-written `extern "C"` declarations are the contract.
- A GPU backend must reproduce the CPU analysis planes and boundary lists byte for byte on real media, which the GPU integration tests pin; it covers whole-pixel downscale geometries and falls back to the CPU filter for anything else, with the reason on standard error.
- Backend selection and fallback are explicit: `auto` probes D3D11 and degrades silently with a note, `d3d11` fails the run when unavailable, `off` never touches the device.
- CUDA and Vulkan paths are future work that requires a rebuilt FFmpeg asset with NVDEC; do not add them without that dependency decision.

## Analysis Plane

- Produce the 8-bit analysis plane with the project's integer box (area-average) filter, defined exactly in `docs/Architecture.md`.
- Keep the filter arithmetic in exact integers (weight accumulation and rounded division), never floating point, so every backend produces byte-identical planes.
- The CPU implementation is the reference. A GPU backend is accepted only when its planes match the CPU implementation byte for byte — on synthetic fixtures where the geometry applies, and on real media through the GPU integration tests.
- Read decoded luma planes under their actual strides and reduce 10-bit sources to 8-bit only after their integer mean is computed, by `>> 2` of the rounded mean.
- Do not add a second filter with different arithmetic to the same backend.
- Do not break byte parity with committed fixtures or digest lists; regenerate them only through the committed generator and record the change.

## Memory and Data Processing

- Prefer borrowed frame data when lifetimes permit.
- Reuse allocated buffers during repeated frame operations. Avoid allocations in per-pixel and per-block loops.
- Use bounded buffers and queues. Do not keep the complete video in memory.
- Keep only the frames and statistics necessary for the current detection window.
- Process image planes with their actual strides. Do not assume that the stride equals the image width.
- Verify image dimensions, buffer lengths, offsets, and integer arithmetic before memory access.
- Do not convert video frames to RGB.
- Avoid unnecessary frame copies and pixel format conversions.

## Video Decoding and Time

- Process all decoded video frames in presentation order.
- Do not use encoded keyframes as scene boundaries.
- Preserve the original zero-based frame index and valid presentation timestamps.
- Do not calculate timestamps from an assumed constant frame rate.
- Support variable frame rates and nonzero stream start times.
- Handle missing timestamps explicitly. Do not invent timestamps.
- Report a hard cut at the first frame of a new shot.
- Handle changes in frame dimensions and pixel format explicitly.
- Report decoder errors. Do not silently ignore damaged frames.

## Detection Algorithm

- Keep feature extraction separate from temporal decisions.
- Use deterministic calculations for identical input and configuration.
- Do not change detection thresholds without test evidence.
- Do not use undocumented constants. Give each parameter a clear name.
- Do not confuse a low score with a confirmed absence of a scene cut.
- Keep detection scores available for tests and diagnostics.
- Do not remove short shots only because they contain few frames.
- Do not classify every bright frame as a flash.
- Do not add motion estimation, edge processing, or expensive operations without measured benefit.
- The reference implementation is the pinned WWXD `detection.c` in `oracle/wwxd-reference`.
- All changes to the decision core must reproduce the committed reference vectors: decision, shot age, macroblock predictors, and plane digests.
- The decision core is independent of the analysis-plane filter. A filter change must be justified by a boundary comparison over the committed video corpus, recorded in `docs/Performance.md`, before it is accepted.
- Do not claim exact reproduction of the Fraunhofer 2004 system without evidence.

## Performance Rules

- Measure performance before and after each optimization using release builds.
- Separate decode time, resize time, queue wait time, detection time, and total time.
- Use identical input, configuration, and test environment for comparisons.
- Profile code to identify expensive operations before you optimize them.
- Optimize memory access, allocations, and redundant calculations before adding parallel processing or SIMD.
- Keep a portable implementation when you add architecture-specific optimizations.
- Use bounded channels when decoding and detection run in separate threads.
- Do not change algorithm behavior only to improve benchmark results.
- Report throughput in frames per second (FPS). Always state whether decoding is included.

## Tests and Validation

- Add unit tests for each new calculation and decision rule.
- Add integration tests for the complete video processing pipeline.
- Test edge cases: repeated frames, hard cuts, flashes, short shots, truncated input, invalid strides, VFR, and missing timestamps.
- Verify frame indexes and timestamps independently of detection results.
- Compare detector results against the reference oracle in `oracle/wwxd-reference`.
- Compare analysis planes of every GPU backend against the CPU implementation byte for byte — on real media through the GPU integration tests, not only on synthetic fixtures.
- Do not change expected test results to make a failing test pass.
- Keep benchmark inputs and configurations consistent across revisions.

## Error Handling and CLI Output

- Return structured errors from library crates with operational context.
- Do not panic on invalid video files or ordinary decoder failures.
- Follow the CLI output contract defined in `docs/Architecture.md`:
  - Standard output must remain empty.
  - Write keyframe cuts to the designated output file (`.kf.txt`).
  - Send progress indicators, completion messages, statistics, and diagnostic logs to standard error.
- Report incomplete processing explicitly. Never report partial output as a successful result.

## Build and Change Control

- Run `cargo fmt --all -- --check` after code changes.
- Run `cargo clippy --all-targets --all-features -- -D warnings`.
- Run `cargo test --all-targets --all-features`.
- Run `cargo build --release` when changes affect the executable or release behavior.
- Update tests and documentation when public behavior changes.
- Do not modify unrelated code.
- Do not perform broad refactoring during a focused bug fix.
- Do not change dependency versions without a clear reason.
- Do not edit generated files manually.
- Do not claim that a test passed if the test did not run.
- Do not create a Git commit unless the user explicitly requests one.

## Agent Completion Rules

- Follow `docs/Architecture.md` and the task requirements.
- Confirm acceptance criteria before you report a task as complete.
- Record measured benchmark results in `docs/Performance.md` only after verification.
- State which files changed and explain why.
- Report the exact commands executed and their actual results.
- Report all known failures, regressions, and unverified items.
- Do not report a task as complete if required checks fail.
- Prefer small, verified changes over large, unverified changes.
