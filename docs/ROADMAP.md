# IHNI 修正实施计划

> 执行者：逐项实施时使用 executing-plans；未经用户要求不创建 Git commit。

**Goal:** 实现视频读取、resize、WWXD、Aegisub `.kf.txt` 和 CLI 进度，同时控制发布体积。

**Architecture:** core 只做 WWXD，media 提供分析帧，根包串联输出和进度。先解决用户可见流程，
再将 resize 迁回 swscale 并验证体积与一致性，最后 profiling 优化 WWXD。无需框架或近似模式。

**Tech Stack:** stable Rust、现有 clap、进程内 FFmpeg 库与 C shim；libavformat/libavcodec 读取，libswscale resize。

**Spec:** [PROJECT.md](PROJECT.md)。本计划替代旧吞吐量分期。
状态（任务 5 收尾）：任务 1–5 的自动化验收已通过——`.kf.txt` 输出、stderr 进度、swscale 正式 resize
（旧 Rust 缩放器与 `oracle` 已移除）、RunStats 分阶段与 SSE2 内核；fmt/clippy/test/build、link gate、
error-semantics、digest 与 golden 全部 0 差异。未闭合项为环境/人工项：Aegisub 实际导入、TTY 外观
（需人工终端），Linux 固定 WWXD C oracle 再生（本机为 Windows，由 CI `wwxd-golden-parity` job 执行）。

## 全局约束

- WWXD 固定参考 `a5870862fd85138b5e4822a076377b96592c9cd2`，不得修改 golden 迁就优化。
- core 无外部依赖；libav 结构访问全部留在 media C shim，不引入 bindgen。
- 高度默认 360，宽度取 4 的倍数、半值向上；48..4096 且面积最多 2,097,152。
- 显示顺序零基帧号，不用 FPS 推导源 PTS，不增加无界队列或整片缓存。
- 默认 `.kf.txt`，stdout 空，stderr 进度；退出码 0/1/2；不覆盖已有文件。
- 不随程序打包完整 ffmpeg/ffprobe 命令行工具；体积按 exe 加必需 DLL 的总量评估。
- 仓库已有未提交改动，不重置、不顺手改依赖版本；自动化使用 Bash。
- 每项先添加有意义的失败测试，再最小实现并验证；当前项通过才进入下一项。

## 容易遗漏的验收场景

| 场景 | 期望 | 所属任务 |
| --- | --- | --- |
| VFR、非零起始 PTS、缺 PTS/FPS | 按真实显示序号，FPS 行只作元数据 | 1、3 |
| 重复运行、输出恰好指向输入 | 不覆盖已有文件 | 1 |
| 中文/空格/无扩展名路径、写入失败 | 路径保真，不发布部分文件 | 1 |
| stderr 重定向、未知总帧数、进度写入失败 | 有节流，不伪造百分比，不改变处理结果 | 2 |
| 尾部损坏、中途尺寸/格式变化、提前退出 | 显式失败，无成功文件，无挂起 | 1、2、3 |

## 任务 1：生成 Aegisub 文件

**文件：** 新建 `src/keyframes.rs`、`tests/keyframes.rs`；修改 `src/{cli,main,lib,error}.rs`、
`tests/{cli,pipeline}.rs`、`Cargo.toml`。

**接口：** 输出层消费现有 `DetectSummary`，通过 `summary.stats.frames` 判断非空，读取 `summary.decode.source` 和 boundaries。
定义 `default_output_path(input: &Path) -> PathBuf`、
`write_kf_file(path: &Path, summary: &DetectSummary) -> Result<(), AppError>`；公开模块供集成测试使用。
输出错误包含操作、路径和 I/O 原因。

- [ ] 添加 `writes_aegisub_v1`：固定 fixture 的输出逐字节等于两行头、`0`、已有切点和末尾 LF；
  `first_frame_is_written_once`、`nonempty_no_cut_writes_zero`、`zero_frames_is_error` 覆盖首帧语义。
  core 的非零边界类型和不输出 0 的测试不改。
- [ ] 添加 `fps_metadata_does_not_reindex_frames`：24000/1001 输出 `23.976024`；平均 FPS 优先，
  nominal FPS 次之，都缺少时 `0.000000`；VFR/缺 PTS 不改变帧序号。
- [ ] 添加 `default_path_appends_suffix`、`existing_output_is_untouched`、`output_equal_to_input_is_rejected`、
  `unicode_and_spaced_path`、`write_failure_publishes_nothing`。用失败的 `Write` 单测覆盖中途写入和 flush 错误，
  不能只靠环境相关的“不可写目录”测试。损坏尾部输入不得发布文件。
- [ ] 运行 `cargo test --test keyframes --test cli --test pipeline`，确认新测试因缺少行为失败，而非缺 fixture。
- [ ] 实现完整检测成功后才写文件；同目录临时文件写完、flush 后不覆盖发布。
  可将已有 dev dependency `tempfile` 提升为 runtime，复用 `NamedTempFile` 与 `persist_noclobber`；
  不手写跨平台替换。发布阶段防竞争覆盖；清理失败报告残留路径。
- [ ] 新增 `-o/--output`，默认追加 `.kf.txt`；移除主 CLI 的 `--format`、`--derived-cfr`、`--source-time`、`--threads`。
  保留 `--analysis-height`、`--stream-index`、`--decoder-threads`、`--stats`，库内部必要时间元数据保留。
  删除旧 text/JSON stdout 分支；空视频和运行/写入错误退出 1，参数错误退出 2，成功信息最后发出。
- [ ] 重跑上述测试；确认成功 fixture 的切点不变，只有文件层补 0；输出失败时旧文件与输入字节不变。

**验收：** `ihni video.mkv` 生成可导入的 `video.mkv.kf.txt`，无需重定向或二次转换。

## 任务 2：运行中显示进度

**文件：** 修改 `src/{lib,main,cli}.rs`、`tests/{pipeline,cli}.rs`；新建 `src/progress.rs`。

**接口：** 定义 `Progress { processed_frames: u64, keyframes: u64, elapsed: Duration }`；新增
`detect_video_with_progress(input: &Path, options: RunOptions, on_progress: impl FnMut(Progress)) -> Result<DetectSummary, AppError>`。
保留 `detect_video` 作为无回调包装。每帧完成检测后回调，keyframes 包括输出层将写的首帧；不做事件框架。

- [ ] 添加 `progress_is_monotonic_and_before_return`、`progress_matches_completed_frames`：回调帧数从 1 开始，
  最后与 summary 相等，帧数/关键帧数不回退；损坏文件可以有中间进度但最终返回错误。
- [ ] 渲染器用可控制的经过时长测试 `tty_throttles_at_200ms`、`redirected_output_throttles_at_2s`、
  `unknown_total_has_no_percent`、`progress_write_failure_is_nonfatal`；不在测试里 sleep。
- [ ] 运行 `cargo test --test pipeline --test cli` 确认新接口/选项测试先失败。
- [ ] 回调接入现有 detector 消费循环；CLI 以 `IsTerminal` 判断，TTY 原地更新、非 TTY 每 2 s 按行。
  进度包含帧数、关键帧数、耗时、平均 FPS；无总量时不显示百分比/ETA。渲染失败停止后续进度，不吞处理错误。
- [ ] 实现 `--no-progress`，完成/失败时结束进度行。文件发布成功才显示完成路径。
  保留独立 `--stats`，基准运行能关闭周期进度；更新 CLI 快照与错误测试。
- [ ] 重跑测试；长 fixture 在返回前能观察到进度，短视频也有最终消息，stdout 空。

**验收：** 等待期间有信息，重定向日志可读；失败不会显示成功。

## 任务 3：保留 FFmpeg 读取，迁移到 swscale resize

**文件：** 先核对 `crates/ihni-media/{build.rs,Cargo.toml,csrc/ihni_ffmpeg.c,src/ffi.rs,src/decode/reader.rs}`、
`scripts/fetch-ffmpeg-libs.sh`、`scripts/check-link-gate.sh`、`.github/workflows/check.yml`、root Cargo 配置；
测试位于 `crates/ihni-media/tests/{analysis_digests,decode}.rs`、`tests/pipeline.rs`。

**接口：** media 仍输出紧密 gray8 分析帧和真实源元数据；core 不变。正式发布只保留一种读取/resize 路径。
当前 direct-libav + ihni-scale 作为基线；目标为 direct-libav + swscale，用户已明确不切换 FFMS2。

- [ ] 记录当前 release 构建参数、exe 字节数、所有必需 DLL 和总发布字节数，不拿 `.lib` 大小当成发布体积。
  固定原视频支持范围，不通过悄悄删掉 codec 支持制造缩小结果；检查链接图是否包含无用编码/音频/过滤组件。
- [ ] 用单独临时构建目录比较当前基线和 direct-libav+swscale。
  记录 exe+DLL 总量、压缩包量、依赖列表、初始化耗时和处理 FPS；发布包不增加外部 ffmpeg.exe。
- [ ] 先添加 `midstream_geometry_change_fails`、`midstream_pixel_format_change_fails`、
  `resize_keeps_pts_and_display_order`，以及无 `oracle` feature 也能调用正式 scaler 的测试。
  运行 `cargo test -p ihni-media`，确认正式 scaler 测试因当前 feature 限制失败；已有行为测试先通过。
- [ ] 复用现有 opaque `ihni_scaler_open/scale/close`，正常链接 swscale；
  单 context、灰度源、`SWS_BICUBIC` 默认参数、`gray16le >> 8`。C 端使用满足 SIMD 的对齐缓冲，按实际 stride 读取，
  context/输出复用。将 media 使用的最小 `LumaFormat` 类型移入 media，避免继续依赖旧 scaler 类型。
- [ ] 复用原分析 digest 和 synthetic 对照，测试 8/10-bit、两种端序、identity、上/下采样、
  奇数尺寸、真实 stride、VFR/B-frames/缺 PTS。运行新增的源变化与帧序号测试，以及现有 golden，切点不得变化。
- [ ] 运行 `cargo test -p ihni-media --all-features`、`cargo test --test pipeline` 和完整 parity。
  记录总发布体积和端到端性能变化；增大时先查链接图及无用组件，不以缩小为名削减既有视频支持。
  若无法保持 parity，停止切换并报告具体差异，不通过改 golden 接受回归。
- [ ] link gate 允许静态 `sws_` 符号，继续检查意外 FFmpeg DLL；CI 改为验证 swscale 正式路径。
  构建时的 headers/static libs、测试用 ffmpeg/ffprobe 不属于发布内容。

**验收：** FFmpeg 库读取并 resize，只有一条正式路径，无完整 FFmpeg CLI 包袱；有真实体积/速度数据，分析输入和 WWXD 结果一致。

## 任务 4：实测并优化 WWXD

**文件：** 修改 `src/lib.rs`、`examples/bench.rs`、media 计时汇总、`docs/PERFORMANCE.md`；
根据 profiler 局部修改 `crates/ihni-core/src/detect/{core,state}.rs`、`crates/ihni-core/tests/detect.rs`。

**接口：** `RunStats` 分开真实 producer decode、resize、consumer queue wait、padding、core detection、total wall。
等待时间不再叫解码成本，汇总随现有 stream summary 返回，不在像素/块循环打时间戳。
bench 增加 detector-only 重放固定短分析序列，每轮重建 Detector，不缓存完整长视频。

- [ ] 验证计时汇总、帧数、失败行为；decode/resize 在生产者计时，queue wait 单列，并行阶段不相加冒充 wall。
- [ ] release、同输入/硬件/参数：一轮 warm-up，加至少五组交错前后运行，记录 median/min/max、帧数和 FPS。
  分别测 detector-only 与含解码的端到端；噪声大时延长重复次数并记录。
- [ ] 用可用 profiler 采样两种运行，保存工具/命令和热点摘要。`sad16/sad32v/dev16`、搜索、padding 只是候选，
  不能未测量就预定优化对象。先查内存访问、重复计算和分配，再考虑复杂方案。
- [ ] 对测得热点做最小修改，不改阈值、预测器顺序或 early-exit 残留状态；不默认引入线程、手写 SIMD、近似模式。
- [ ] 每项运行 `cargo test -p ihni-core`、固定 golden、同视频切点对照；保留重复帧、运动、闪光和短镜头测试。
  前后复测只有超出噪声且无一致性回归的优化才接受。
- [ ] PERFORMANCE.md 记录真实环境、输入、配置、命令、前后数据和失败/回退。
  没有可靠收益时明确说明，不承诺未经测量的倍数，不用 CPU 占用率当成果。

**验收：** 有可信 profiling 和前后数据；接受的优化保持全部 golden 与视频结果。

## 任务 5：撤掉自研缩放路径并完成验收

**文件：** swscale 迁移通过后清理旧 media/scaler 路径、Cargo features、专用 examples 与 CI 步骤；更新文档和源码注释。
删除 `crates/ihni-scale/` 与 `examples/scale_{parity,fixtures}.rs`；
有用 regression fixture 移到 media，保留视频 digest、通用诊断脚本和 `oracle/wwxd-golden`。

- [x] `rg` 核对旧实现引用、依赖与文档链接；不把 WWXD oracle 当成 scaler 工具删掉。
- [x] 生成 `target/m1-fixtures/` 后运行完整检查，缺 fixture 不得以测试 skip 计为覆盖。
  （149 tests，0 skipping 行；digest/decode/pipeline 全实际执行）
- [x] 运行 `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test --all-targets --all-features`、`cargo build --release`，均须退出 0。
- [ ] 运行更新后的 link gate、`bash scripts/check-error-semantics.sh`，Linux 固定 WWXD C oracle 再生后 golden 无差异。
  无法运行的环境项明确列出，不能声称通过。（link gate 与 error-semantics 本机已通过：67 swscale 字符串、
  与 `error-semantics-native.txt` 逐字节相同；Linux C oracle 再生在本 Windows 环境无法运行——无 gcc 参考构建，
  由 CI `wwxd-golden-parity` job 承担，golden.json 本机仅验证未被改动，故整项不勾选）
- [ ] 人工验证 `.kf.txt` 可在 Aegisub 导入，运行中有进度，已有文件不被覆盖，坏输入/写入失败不发布文件；
  `--no-progress` 无周期输出，stdout 始终空。核对最终发布包没有开发库或重复 FFmpeg。
  （可脚本化部分已验证：cut8 输出字节精确、重跑拒绝覆盖且旧文件不变、`-o`==输入退出 2、
  `--no-progress` 0 条周期行、截断输入不发布、stdout 全为空、发布集 = 单一 exe + 系统 DLL/VCRUNTIME140、
  零 FFmpeg DLL；Aegisub 实际导入与交互式 TTY 外观无法在本环境验证，留给人工，故整项不勾选）
- [x] 仅在实际验收后勾选任务和更新当前状态；汇报文件、命令、实际结果和未验证项。
  （汇报已并入提交内容：本文件顶部状态、任务 5 各项备注、`docs/DECODE.md` 体积与链接门实测、
  `docs/PERFORMANCE.md` 测量记录、`docs/PROJECT.md` 当前状态；SDD 会话工作区为 gitignore 临时件，收尾后不保留）

**最终完成条件：** 一条命令得到关键帧文件与实时进度；发布体积有依据，WWXD 正确且有可信性能记录。
