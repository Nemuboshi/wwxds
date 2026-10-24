# 解码、Resize 与构建

## 状态和目标

本文合并原 TOOLCHAIN、NATIVE_FFMPEG 和 SCALE 中仍有用的构建、FFI 和兼容性证据。
当前代码是 direct-libav 解码 + 静态链接 libswscale resize（`SWS_BICUBIC`、`gray16le >> 8`，单 context，
新 frame API 的 dynamic 模式）。
用户已确定保留 FFmpeg 库读取，resize 也改用 swscale；迁移按 [ROADMAP.md](ROADMAP.md) 任务 3 完成，体积见下。
缩小发布体积仍是目标；此次不切换 FFMS2。
旧 Rust 缩放器（`ihni-scale`）与其 `oracle` 对照已按任务 5 完全移除，正式路径是唯一 resize 实现；
下节的算术与对照证据是迁移当时的历史记录。
本轮未改运行逻辑的其余历史验证记录没有在文档清理时重跑。

目标接口只需按显示顺序提供分析帧及源元数据。core 不知道容器或媒体后端。
不附带完整 ffmpeg.exe/ffprobe.exe；需要它们的 fixture 生成与运行时依赖分开。
直接用 [libswscale](https://ffmpeg.org/libswscale.html)无需使用 FFmpeg CLI。
FFMS2 自身也依赖 FFmpeg；本项目不再评估 FFMS2 迁移。

## 当前帧接口与规则

- 默认选择第一个视频流，`--stream-index` 是绝对流号；指定非视频流或不存在的流必须报错，不回退。
- 所有解码帧按 presentation order 返回，保留零基序号、有效 PTS 和 stream time base。
  B-frames/VFR/非零起点不重排错误；缺 PTS 明确为空，不从平均 FPS 补源时间。
- 默认分析高 360，`width = 4 * round(source_width * height / (4 * source_height))`，半值向上；
  检查算术溢出、48..4096 的尺寸和 2,097,152 像素上限。
- 当前支持 8/10-bit luma，灰度源为 gray8/gray10le/gray10be；按真实 stride 读取，不转 RGB。
  先取 Y 平面再以灰度格式缩放，避免直接缩放 YUV 时把 limited-range Y 改为 full-range。
- 正式路径固定单个 SwsContext、`SWS_BICUBIC` 默认参数（B=0、C=0.6）、gray16le 输出，再逐样本 `>> 8`
  得到 8-bit 分析平面；`crates/ihni-media/src/decode/resize.rs` 持有 context 与可复用 16-bit 平面。
  直接让 swscale 输出 gray8 在历史 H.264 样本约 6% 字节不同，不能当成等价替换。
- 该 context 走新 API 的 dynamic 模式：`sws_alloc_context` + `sws_scale_frame`，不调用
  `sws_init_context`（无几何信息时每帧返回 `EAGAIN`）。旧 `sws_getContext`/`sws_scale` 恒为
  `SWS_BACKEND_LEGACY`，单线程且静默忽略 `SwsContext.threads`；新 API 才让线程数生效。
  两种 API 在各线程数下输出字节完全相同，digest fixtures 因此仍然有效。
- 视频中尺寸或像素格式变化须明确失败，不通过自动归一化掩盖输入变化。
- 默认两个可复用分析帧槽位，producer 解码/resize，consumer 串行检测；只保留当前检测窗口。
  提前退出必须关闭 reader 并 join producer，不能遗留阻塞线程。

现有库还保留 unavailable / derived_cfr / decoder_pts 时间模式；修正后的主 CLI 只输出帧号文件，
不再暴露时间派生选项。格式中的 FPS 只是元数据，具体规则见 PROJECT.md。

## FFI 和资源边界

当前入口在 `crates/ihni-media/src/ffi.rs`，所有 AVFormatContext、AVCodecContext、AVFrame、
AVPacket、SwsContext 的访问留在 `crates/ihni-media/csrc/ihni_ffmpeg.c`。
Rust 只接收 opaque handle、状态码、标量和指针/长度/stride，不镜像 libav 布局；无 bindgen/libclang。
每个 unsafe 块写 SAFETY 条件，crate 禁止 undocumented unsafe 和 unsafe_op_in_unsafe_fn。

decoder frame 指针只到同 handle 的下次调用前有效；在下一次读取之前，必须按实际 stride 完成消费。
现有 handles 可移动给单个生产者线程，但不可并发调用同一 context。
Drop 关闭资源；所有尺寸、offset、buffer 长度与整数运算在访问前校验。

任务 3 之后 `ihni_scaler_open/scale/close` 和 libswscale 在所有构建编译并链接，是唯一的正式 resize 入口；
旧 Rust 缩放器与 `oracle` 对照 feature 已在任务 5 移除。swscale 的输出 SIMD 会写到对齐跨度内，目的缓冲须满足库的
对齐/尾部空间要求；只比较有效像素。正式路径的可复用 16-bit 平面行跨度向上对齐到 64 字节，与
harness 记录 fixture 时同一契约；不把紧密的 width*height Rust 缓冲直接当作安全的 swscale 输出缓冲。
`>> 8` 在 Rust 侧逐样本完成（`gray16le` 的第二个字节），C 端不做缩减。

## 当前构建方式

当前 build.rs 支持 Windows x64 MSVC，使用 stable Rust、rustfmt、clippy，依赖版本在 Cargo.lock。
原记录工具链为 rustc 1.98.1 (48a229cea 2026-09-01)、cargo 1.98.1、rustfmt 1.9.0、clippy 0.1.98，
这是历史环境记录，不是本轮实际检查输出。
clap 用于 CLI，cc 编译 shim，serde_json 当前用于测试，tempfile 用于关键帧文件发布；core 无第三方依赖。

```bash
export FFMPEG_DIR="$(bash scripts/fetch-ffmpeg-libs.sh)"
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

fetch 脚本使用 gh 下载并验证字节数/hash，输出完整 dev prefix；不可把该目录整体放入发布包。

| 项目 | 当前固定值 |
| --- | --- |
| 来源 | Nemuboshi/GenKFR release `ffmpeg-libs-9.0.2` |
| 资产 | `ffmpeg-libs-9.0.2.zip` |
| 字节数 | 80,867,368 |
| SHA256 | `d6de00b8f68694c50f29cfbc2ec9c466b4ba80f90f523efd20fc661174833649` |
| triplet | x64-windows-static-md-release |
| prefix | target/ffmpeg-libs（忽略目录） |
| libavcodec / libavformat / libavutil | 63.1.102 / 63.1.102 / 61.1.102 |
| libswresample / libswscale / zlib | 7.1.102 / 10.1.102 / 1.3.2 |

当前静态链接 avformat、avcodec、swscale、swresample、avutil、zlib（build.rs 固定列表）。
静态 FFmpeg 配动态 CRT，build.rs 拒绝 `+crt-static`。Windows 系统库列表以 build.rs 为准。
链接静态库不等于复制全部 `.lib` 字节；发布体积应检查最终 exe 和必需 DLL，不能拿 SDK/下载包大小相加。
任务 3 实测（本机、release、`x64-windows-static-md` 前缀，全新 CARGO_TARGET_DIR 各建一次，同 commit 前后）：
迁移前 18,222,592 字节 / 迁移后 19,160,064 字节（+937,472）；必需 DLL 与迁移前逐项相同
（系统 DLL + VCRUNTIME140，零 FFmpeg DLL）。ledger 早先记录的 18,219,520 是一次增量构建的旧值，
以本轮全新构建为准。
`scripts/check-link-gate.sh` 第一项（拒绝 FFmpeg DLL 导入）不变；第二项在任务 5 后为硬断言：
oracle 对照移除后不存在合法的无 swscale 构建，release 镜像无 COFF 符号表（在 .pdb），故以 `grep -a`
直接探测镜像内的 swscale 文本（构建路径/日志符号名）证明静态嵌入，0 命中即失败（Git-Bash 无 `strings`，
runner 上不可依赖；GNU grep 保证存在）。
oracle/ihni-scale 工具链已在任务 5 移除，体积不因此变化：正式路径一直会链入全部用到的库。
任务 5 实测（本机、release、同一 `x64-windows-static-md` 前缀、全新 CARGO_TARGET_DIR 各建一次，
移除前后两个 commit 对比）：`4005600` 与任务 5 收尾后的树均为 19,164,672 字节，移除 delta = 0；
必需 DLL 仍为系统 DLL + VCRUNTIME140，零 FFmpeg DLL（link gate 以 26 处镜像文本命中确认 swscale 嵌入）。
任务 3 评审补记（review round 1）：
- 压缩包量不适用：发布物是裸 `ihni.exe`（发布集 = 单一 exe + 零 FFmpeg DLL，见上），
  不存在归档制品；PROJECT.md 明确不附带 ffmpeg.exe/ffprobe.exe，故未测压缩体积，此为 N/A 声明。
- 初始化耗时本轮只有小输入整程代理测量（`cut8.mkv` 含进程启动，交替三轮：迁移前
  0.066/0.070/0.064 s，迁移后 0.044/0.044/0.043 s，新构建不慢于旧），未做独立 init 分解测量。

原资产未带 license 文本，仅 PACKAGES.txt。旧调查依据 feature 列表无 gpl/nonfree 推断为 LGPL 配置，
未取得精确 configure summary，不能把推断当成已核实。发布前保留所用构建对应的许可文本和来源；
自建裁剪库后应记录实际构建信息，而不是沿用旧推断。

## 测试输入和复现

```bash
bash scripts/make-m1-fixtures.sh
cargo test -p ihni-media --all-features
cargo test --test pipeline
cargo test -p ihni-media --test resize_fixtures
bash scripts/check-error-semantics.sh target/release/ihni.exe target/m1-fixtures
```

fixture 脚本需要 PATH 上的 ffmpeg/ffprobe、libx264 和 ffv1，输出到 target/m1-fixtures。
这些工具只为生成测试视频；验收须检查 fixture 存在，避免缺输入时测试跳过。
analysis digest 测试在 `crates/ihni-media/tests/analysis_digests.rs`；迁移前后固定同一输入与分析尺寸比较。
`examples/digest.rs`、`scripts/compare-digest.sh` 可记录/比较视频逐帧 digest。

WWXD oracle 是独立工作区，正常 cargo test 不需重新编译参考 C。
在支持 gcc 的环境按固定源再生，确认 golden 无差异：

```bash
git clone https://github.com/dubhatervapoursynth/vapoursynth-wwxd.git target/wwxd-reference
git -C target/wwxd-reference checkout --detach a5870862fd85138b5e4822a076377b96592c9cd2
cargo run --manifest-path oracle/wwxd-golden/Cargo.toml
git diff --exit-code -- tests/fixtures/detect/golden.json
```

## 旧缩放器迁移依据

以下是已退出版式正式路径的 ihni-scale 的算术与历史对照证据（工具链与 `oracle` feature 已随任务 5 移除，
本节保留为迁移当时的记录）。
任务 3 的 swscale 正式路径一致性已验证通过：committed 分析/native digest 列表、ramparts 四种分析尺寸的
digest 列表、以及 `scale_parity` 全部对照均 0 差异字节。本节说明为什么这些结果与旧 Rust 缩放器等价。

### The arithmetic

The implementation in `crates/ihni-scale/src/filter.rs` keeps swscale's names in
its comments so each step can be checked against `libswscale`. The fixed-point
scales are not interchangeable:

| Quantity | Scale | Source |
| --- | --- | --- |
| horizontal coefficients | `1 << 14` | `initFilter(..., 1 << 14, ...)` |
| vertical coefficients | `1 << 12` | `initFilter(..., 1 << 12, ...)` |
| internal precision `fone` | `1 << (54 - min(av_log2(srcW/dstW), 8))` | `initFilter` |
| bicubic kernel argument | `1 << 30` per source pixel | `d = |xx * (1<<17) - xDstInSrc| << 13` |

Coefficient generation follows `initFilter` in order: the unscaled
short-circuit, `filterSize` from `sizeFactor = 4`, the tap loop, the
`SWS_MAX_REDUCE_CUTOFF` trim, alignment to 4 (horizontal) and 2 (vertical), the
two border fixups, and the running-error quantizer
`ROUNDED_DIV(sum + one/2, one)`.

The two passes are `hScale8To19_c` / `hScale16To19_c` and
`yuv2planeX_16_c_template`:

- **Horizontal**: raw samples are multiplied with no depth expansion, accumulated,
  then shifted right by `depth - 5` (3 for 8-bit, 5 for 10-bit) and clamped at
  `(1 << 19) - 1`. Only the top is clamped; negative intermediates from the
  kernel's negative lobes survive into the vertical pass.
- **Vertical**: `((1 << 14) + Σ h_j · vf_j) >> 15`, clamped to `[0, 65535]`. The
  template's `- 0x40000000` accumulator offset and `+ 0x8000` bias cancel
  exactly. With a single tap the template is not used: `ff_init_vscale_pfn` binds
  `yuv2plane1_16_c`, which computes `clamp_uint16((src19 + 4) >> 3)`.

#### The depth promotion is not `v * 257`

This is the one place where an intuitive guess is wrong. In the filtered path the
8-bit sample is multiplied raw and the shift happens after accumulation, so an
interior pixel round-trips as `v << 8`, not `v * 257`. For 10-bit it is `v << 6`.
The 8-bit reduction (`>> 8`) yields the same value either way, which is why the
committed analysis-plane digests could not reveal the difference; the 16-bit
parity harness of the migration could, and did. The recorded fixtures still
hold those 16-bit bytes.

### Identity geometry

When the source and destination sizes are equal, `sws_init_context` sets
`unscaled` and `ff_get_unscaled_swscale` binds `planarCopyWrapper`. `handle_jpeg`
forces `src_range` and `dst_range` to full for every GRAY format, so `shiftonly`
is true and the copy promotes with

```text
(v << (16 - depth)) | (v >> (2 * depth - 16))
```

which is `v * 257` for 8-bit and `(v << 6) | (v >> 4)` for 10-bit — a different
result from the filtered path's `v << 8`. This is what swscale does here; the
old Rust resampler reproduced the branch (`copy_pass`) rather than running the
filter. The identity geometries stay covered by the recorded
`identity-96x64` and `tenle-identity-96x64` fixtures.

### Cascade geometries are refused

`initFilter` refuses a filter of `SWS_MAX_FILTER_SIZE` taps or more (256 by
default, without `SWS_ACCURATE_RND`) and the caller then scales through an
intermediate `YUV420P` plane at the geometric mean size, converting the sample
range on the way. That output is not the output of a single filtered pass.

The old `Scaler` returned `ScaleError::CascadeRequired` for such a geometry
instead of returning different bytes; native swscale handles the geometry
exactly. Only extreme downscales reach it — for a 641-wide source, anything
below about 15 output pixels wide. No analysis size is remotely that small.

### Parity evidence

The gate is measured, not asserted. The pre-migration record compared the Rust
scaler against the oracle across 2880 synthetic cases, 8637 ramparts cases and
every m1 fixture frame, all with 0 differing bytes; those instruments
(`examples/scale_parity`, `crates/ihni-scale/tests/parity.rs`) were retired
with task 5 and the equivalence they established is what the committed lists
and fixtures below still hold. Task 3 (2026-10-19, this machine, rustc 1.98.1,
FFmpeg 9.0.2 prefix) measured the production swscale path against the committed
lists rather than against the oracle; it re-ran after the oracle and the
parity-control row were removed at task 5, with the same result. The live gates:

| Instrument | Scope | Result |
| --- | --- | --- |
| `cargo test -p ihni-media --test analysis_digests` | 9 fixtures × analysis + native heights | **passes**, 0 differing frames |
| `examples/digest` vs `target/digest-baseline/*` | ramparts-120s 2879 frames × 640x360, 1280x720, 1920x1080, native | `diff` **byte-identical** on all four lists |
| `cargo test --test pipeline` + `golden.json` diff | WWXD boundaries on fixtures | **passes**, golden.json unchanged |
| `cargo test -p ihni-media --test resize_fixtures` | 13 recorded cases, production `FrameResizer`, offline | **passes**, 0 differing bytes |
| `scripts/check-error-semantics.sh` vs `error-semantics-native.txt` | 7 failure inputs | **identical** |
| `bash scripts/make-m1-fixtures.sh` | all fixtures, incl. new `mixed-depth.mp4`/`mixed-size.mp4` | regenerates; verified in-script by ffprobe |

The migration control — the oracle `scale_parity` run that confirmed the Rust
scaler and swscale agreed while the lists were still being handed over — is
gone with the toolchain; what it proved is now held directly by the committed
lists and the recorded fixture planes.

Both output forms stay defined by one computation: the 16-bit plane is what
swscale writes, and the 8-bit analysis plane is the high byte of that same
16-bit result (`>> 8`).

#### Measurement conditions

The resize destination uses a 64-byte-aligned stride, which is what a real
libav caller passes. swscale's SIMD output writes whole vectors and will
overrun a tight stride that no caller produces; that is an allocation contract
of the library, not a difference in results. Only the pixels of each row are
part of a result — the padding belongs to no scaler's output.

#### Residual risk

FFmpeg's x86 SIMD (`ff_hscale8to19_*`, `ff_yuv2planeX_16_*`) replaces the C
functions with no C fallback. Byte identity between the two is FFmpeg's design
intent, and it is what the measurements above observe on this machine, but the
comparison is against whatever variant the linked library selects on the machine
running it. A platform whose SIMD variant disagrees with its C reference would
show up as a parity failure on that platform.

### Offline fixtures

`crates/ihni-media/tests/fixtures/resize/` holds 13 source planes and the exact
`gray16le` planes swscale produced from them. They cover upsampling,
downsampling, odd sizes, constant planes (which expose a mis-normalized
coefficient row), identity geometry, and 10-bit in both endiannesses.
`crates/ihni-media/tests/resize_fixtures.rs` drives the production
`FrameResizer` over the sources and compares the 8-bit analysis plane against
the high byte of every recorded sample — the `>> 8` reduction the path
performs — so the analysis bytes the detector sees are held to the recorded
swscale output. The test needs no FFmpeg, so CI verifies it on every run.

The files are frozen regression data. The recording harness
(`examples/scale_fixtures`) and the retired Rust resampler were removed with
task 5; nothing in the repository regenerates these planes. A mismatch means
the linked swscale or the resize path changed the recorded bytes — a review
event, not a routine update.

## Diagnostics

libav reports problems through one process-global `av_log` callback, and a decoder
with frame threading calls it from worker threads. The shim therefore registers
its own callback at load time and accumulates the messages at `AV_LOG_ERROR` and
above into a buffer that is:

- **per-thread** (`__declspec(thread)` under MSVC, `_Thread_local` otherwise), so
  a reader is used from exactly one thread and two readers in one process cannot
  see each other's diagnostics. A shared buffer would let a second reader take
  the first one's diagnostics and turn a damaged input into a clean one; that
  defect was found by test and this storage is the fix.
- **bounded to 8 KiB**, with a `(truncated)` marker rather than unbounded growth.

Any non-empty diagnostic makes the run incomplete. That is the rule that replaced
the child process's exit code: libav reports a damaged container **through the
log** and can still reach a clean end of stream, so a diagnostic is treated as
incomplete processing rather than as a warning. The reader takes the diagnostic
buffer exactly once, at the end of the stream, and reports
`MediaError::IncompleteProcessing`.

The shim does **not** reset the diagnostic buffer when the resampler is built.
Resampling happens mid-run, and a container that reported a problem must keep
reporting it.

**The diagnostic text is not part of the output contract.** With frame threading
enabled, libav merges or drops some per-slice messages that a single-threaded
decode reports, so the same damaged file produces a different set of lines:

| `--decoder-threads` | Diagnostics for `damaged.mkv` |
| --- | --- |
| 1 | `Slice pointer chain broken`, `sending a packet to the decoder failed: Invalid data found when processing input`, `Unknown-sized element at 0x90a inside parent with finite size` |
| 2, or omitted | `Unknown-sized element at 0x90a inside parent with finite size`, `sending a packet to the decoder failed: Invalid data found when processing input` |

Each is deterministic for its own thread count, and the difference is a property
of libav's threaded error handling, not of this project. What *is* a contract, and
what the tests and `scripts/check-error-semantics.sh` gate on, is the error class,
the exit code, and the empty standard output — all of which are identical in both
rows. Do not assert on the diagnostic text, and do not treat a shorter list as a
cleaner decode.


## Recorded failure behaviour, before and after

The child-process pipeline and the in-process decoder were compared item by item
with `scripts/check-error-semantics.sh`, which masks the object addresses FFmpeg
writes into every diagnostic and leaves the exit code, the error class, and the
wording exactly as produced. `error-semantics-midrun.masked.txt` in
`target/digest-baseline/` is the recorded "before".

| Input | Before (child process) | Now (in-process) |
| --- | --- | --- |
| `empty.bin` | `could not interpret the media probe output for '…': ffprobe exited with Some(1): …: Invalid data found when processing input` | `cannot read the container of '…': the input could not be opened` |
| `invalid.bin` | as above | as above |
| `audio-only.m4a` | `input '…' has no video stream to analyse` | unchanged |
| `cut8.mkv --stream-index 7` | `input '…' has no stream with absolute index 7` | unchanged |
| `multi-stream.mp4 --stream-index 1` | `stream 1 of '…' is 'audio', not a video stream` | unchanged |
| `truncated.mkv` | `…did not complete; no frame sequence is reported; decoder exit code 0; diagnostics: [matroska,webm @ ADDR] File ended prematurely` | `…did not complete; no frame sequence is reported; diagnostics: File ended prematurely` |
| `damaged.mkv` | `…; decoder exit code 0; diagnostics: [matroska,webm @ ADDR] Unknown-sized element…` / `[ffv1 @ ADDR] Slice pointer chain broken` / `[vist#0:0/ffv1 @ ADDR] [dec:ffv1 @ ADDR] Error submitting packet to decoder: Invalid data found…` | `…; diagnostics: Unknown-sized element…` / `sending a packet to the decoder failed: Invalid data found…` (with `--decoder-threads 1`, also `Slice pointer chain broken`) |

Three changes, all of them consequences of dropping the subprocess:

1. **The probe subprocess's message is gone.** A non-media file is now reported
   by the container reader instead of by ffprobe's exit status.
2. **The `-loglevel error` decoration is gone.** No `[component @ ADDR]` prefix
   and no `decoder exit code 0`, because there is no process whose exit code
   could be reported, and the shim records the message rather than libav's
   formatted line. The decoder's own wording is preserved
   (`File ended prematurely`, `Unknown-sized element at 0x90a…`,
   `Slice pointer chain broken`, `Invalid data found when processing input`).
3. **The diagnostic set for a damaged container depends on the decoder thread
   count**, as tabulated above.

Unchanged: every case still exits 1, writes nothing to standard output, and is
classified as a container, stream-selection, or incomplete-processing error.
