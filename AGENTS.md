## Reading and Evidence

- Define the question and the required scope before you read files. Combine related queries into one batch. Do not create a separate agent for each file.
- For JSON, JSONL, and large log files, first inspect the structure, key fields, counts, or match locations. Read more data only when necessary. Do not use a "first N lines" limit for large JSON files. One line can contain a large array.
- Read source code by symbol, function, or specified line range. Read only the relevant sections of documents. Identify the scope of a diff before you inspect the relevant hunks.
- Do not output complete large arrays, pixel data, or iteration records by default. Output relevant fields, summary data, and locations of anomalies. Keep the source files available for later inspection.
- Truncated output is not complete evidence. If output is truncated, use a more specific field, range, or page. Do not output the same complete content again.
- In reports, identify the source, location, version or configuration, actual result, failures, and unknown items. Keep missing fields, `null`, and `false` as separate states. Do not report success without sufficient evidence.
- Reuse existing results for the same question. If a source or code changes, inspect the affected parts again. The main agent must verify critical evidence. Do not read all material again if a subagent has already examined it.
- Use simple scripts to extract fixed fields and counts. Assign comparisons across multiple sources and initial evidence checks to the Luna subagent. The main agent must make and verify all critical decisions.

## CI and Repository Scripts

- Use Bash for CI steps and repository automation scripts, including on Windows runners. Do not add PowerShell steps or scripts.
- Declare `shell: bash` explicitly for GitHub Actions command steps so native command failures can be handled consistently.
- Keep Bash script files in LF line endings; `.gitattributes` enforces this on Windows checkouts.

Rust Development Rules

* Use stable Rust. Do not use nightly features unless the task requires them and the main agent approves their use.
* Follow Rust API Guidelines and standard Rust naming rules.
* Use the Rust type system to prevent invalid states. Prefer explicit types for frame indexes, timestamps, dimensions, and detection scores when confusion is possible.
* Keep functions small and focused. Do not add abstractions without a clear use.
* Prefer simple data structures and direct control flow. Do not add traits, macros, generics, or design patterns without a technical need.
* Keep dependencies to a minimum. Check the standard library before you add a crate.
* Do not use unsafe by default. If unsafe is necessary, document its safety conditions and add tests.
* Do not use unwrap() or expect() on external input or recoverable errors. Return an error with useful context.
* Do not suppress compiler warnings or Clippy diagnostics without a documented reason.

Architecture and Dependencies

* Keep video decoding separate from scene detection.
* The detection core must not depend on FFmpeg, a video container, or a CLI framework.
* Pass decoded frame data and frame metadata through explicit interfaces.
* Do not add video encoding, rate control, or bitstream generation to the detection core.
* Keep the detection algorithm independent of the input video codec.
* Do not copy Xvid or SCXvid implementation details into the detector without an explicit requirement.
* Follow `docs/PROJECT.md` and `docs/ROADMAP.md`. Do not add features outside the current phase.
* Do not introduce a plugin system, a general processing framework, or a new abstraction layer for possible future requirements.
* Keep crate dependencies one-way: `ihni-core` has no dependencies; `ihni-media` handles decoding and resizing; the root package handles CLI and output. Never make core depend on media or the CLI.

FFI and FFmpeg

* Every `AVFormatContext`, `AVCodecContext`, `AVFrame`, `AVPacket`, and `SwsContext` access stays in `crates/ihni-media/csrc/ihni_ffmpeg.c`. No Rust file includes a libav header, names a libav type, or assumes a struct offset.
* Do not add bindgen or a libclang dependency. The hand-written `extern "C"` declarations plus the shim are the contract.
* Every `unsafe` block needs a SAFETY note stating the conditions that make it sound. `ihni-media` denies `unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks`, so this is enforced at compile time.
* A pointer into a decoder frame is valid only until the next call on the same handle. Read it under its actual stride before anything else touches the buffer.
* Use FFmpeg libraries for both reading and resizing, as explicitly requested. Link swscale in the normal release path after parity verification. Do not bundle ffmpeg.exe/ffprobe.exe or introduce FFMS2. Measure final exe plus required DLL size, and update the link gate to allow intended swscale symbols while rejecting unexpected dependencies.

Resampling

* The resize compatibility target is a single `SwsContext`, gray source, `gray16le` target, `SWS_BICUBIC` defaults. Migrate to the existing C shim without building a configurable scaler framework.
* The 8-bit analysis plane is the 16-bit result reduced by `>> 8` inside the same computation. Do not add a second 8-bit code path that could disagree with the first.
* The legacy arithmetic and identity-path differences are recorded in `docs/DECODE.md`. Preserve those bytes in migration; do not replace depth promotion with `v * 257` or direct gray8 output without evidence of equivalence.
* The retired Rust scaler refused cascade geometries; native swscale handles them exactly, and the recorded swscale fixtures do not include one (no analysis size reaches that ratio). Never silently approximate unsupported geometry.
* Breaking byte parity with the recorded fixtures or the digest lists is a regression, not an improvement. Do not relax the comparison to make it pass.

Memory and Data Processing

* Prefer borrowed frame data when the data lifetime permits it.
* Reuse buffers in repeated frame operations. Avoid heap allocations in per-pixel and per-block loops.
* Use bounded buffers and queues. Do not keep the complete video in memory.
* Keep only the frames and statistics necessary for the current detection window.
* Process image planes with their actual strides. Do not assume that the stride equals the image width.
* Check image dimensions, buffer lengths, offsets, and integer arithmetic before memory access.
* Do not convert frames to RGB unless a specific algorithm requires RGB data.
* Avoid unnecessary frame copies and pixel format conversions.
* Do not add a cache unless measurements show that it is useful.

Video Decoding and Time

* Process all decoded video frames in presentation order.
* Do not use encoded keyframes as scene boundaries.
* Preserve the original frame index and valid presentation timestamp.
* Do not calculate timestamps from an assumed constant frame rate.
* Support variable frame rates and nonzero stream start times.
* Handle missing timestamps explicitly. Do not invent a source timestamp.
* Use rational time-base values for timestamp conversion when possible.
* Define the frame index as a zero-based index in presentation order.
* Report a hard cut at the first frame of the new shot.
* Handle changes in frame dimensions and pixel format explicitly.
* Report decoder errors. Do not silently skip damaged frames.
* Keep the mapping between source frames and analysis frames correct.

Detection Algorithm

* Keep feature extraction separate from temporal decisions.
* Use deterministic calculations for the same input and configuration.
* Do not change detection thresholds without test evidence.
* Do not use undocumented constants. Give each adjustable parameter a name and document its purpose.
* Do not confuse a low detection score with a confirmed absence of a scene boundary.
* Keep detection scores available for tests and diagnostics.
* Do not remove short shots only because they contain few frames.
* Do not classify every bright frame as a flash.
* Do not add motion estimation, edge processing, or other expensive operations without a measured benefit.
* Identify which algorithm rules come from the pinned WWXD `detection.c` and which are project extensions. Keep the two lists separate.
* Exact mode must reproduce the committed golden vectors: decision, shot age, macroblock predictors, and padded plane digests. A change that breaks parity is a regression, not an improvement.
* Approximate mode is outside the correction scope. Optimize the exact WWXD implementation without changing decisions or introducing a new detection mode.
* Do not claim exact reproduction of the original Fraunhofer system unless evidence supports that claim.

Performance Rules

* Measure performance before and after each optimization.
* Separate decode time, resize time, detection time, and total processing time.
* Use the same input, configuration, and test environment for comparisons.
* Do not compare benchmark results from different conditions as equivalent results.
* Use a profiler to identify expensive operations before you optimize them.
* Optimize memory access, allocations, and redundant calculations before you add complex parallel processing.
* Do not add manual SIMD code or architecture-specific instructions without benchmark evidence.
* Keep a portable implementation when you add architecture-specific optimizations.
* Do not introduce concurrency without a measured benefit.
* Use bounded channels when decoding and detection run in separate threads.
* Do not change algorithm behavior only to improve benchmark results.
* Report throughput in frames per second. State whether the result includes decoding.
* Record the test hardware, input resolution, frame count, build profile, and relevant configuration.
* Use release builds for performance comparisons.

Tests and Validation

* Add unit tests for each new calculation and decision rule.
* Add integration tests for the complete video processing path.
* Test repeated frames, hard cuts, slow motion, camera movement, flashes, and short shots.
* Test the first frame, final frame, empty input, and truncated input.
* Test invalid dimensions, invalid strides, and incomplete frame buffers.
* Test videos with B-frames, variable frame rates, and missing timestamps.
* Verify frame indexes and timestamps independently of detection accuracy.
* Use small, fixed test inputs for regression tests.
* Compare the detector against the C oracle in `oracle/wwxd-golden`; it is the reference, not ground truth for arbitrary content.
* Do not add accuracy metrics, manual labels, or precision/recall/F1 reporting.
* Do not accept a performance optimization that causes an unexplained detection regression.
* Do not change expected test results only to make a failed test pass.
* Keep benchmark inputs and configurations consistent across revisions.

Error Handling and Diagnostics

* Return structured errors from library code.
* Give errors sufficient context to identify the failed operation.
* Do not use panics for invalid video files or ordinary decoder failures.
* Send diagnostic messages to standard error. Keep standard output suitable for machine-readable results.
* Do not output full frames, pixel arrays, or large intermediate buffers in logs.
* Make detailed per-frame diagnostics optional.
* Report incomplete processing explicitly. Do not report partial output as a complete result.
* Do not silently substitute default values for invalid configuration data.

Build and Change Control

* Run cargo fmt --all -- --check after code changes.
* Run cargo clippy --all-targets --all-features -- -D warnings.
* Run cargo test --all-targets --all-features.
* Run cargo build --release when a change affects the executable or release behavior.
* Update tests and documentation when public behavior changes.
* Do not modify unrelated code.
* Do not perform broad refactoring during a focused bug fix.
* Do not change dependency versions without a reason.
* Do not edit generated files by hand.
* Do not claim that a test passed if the test did not run.
* If a required check cannot run, report the cause and the unverified parts.
* Do not create a Git commit unless the user or current task explicitly requires one.

Agent Completion Rules

* Complete the current roadmap phase before work starts on the next phase.
* Confirm the acceptance criteria in `docs/ROADMAP.md`.
* Record measured before/after results in `docs/PERFORMANCE.md` only after verification.
* State which files changed and why.
* Report the commands that ran and their actual results.
* Report known failures, performance regressions, and unresolved questions.
* Do not report a task as complete when its required tests fail.
* Prefer a small, verified change over a large change with no tests.
