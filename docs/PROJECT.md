# IHNI：项目目标

## 用户要求与本轮范围

做一个类似 GenKFR 的简单命令行工具：读取视频，resize，用 WWXD 生成关键帧列表，处理时显示进度。
用户已明确选择 Aegisub `.kf.txt` 输出格式，保留直接使用 FFmpeg 库读取视频，resize 也使用 FFmpeg。
发布体积是实际目标；不引入 FFMS2，也不附带完整的 ffmpeg.exe/ffprobe.exe。
修正计划已按 [ROADMAP.md](ROADMAP.md) 实施并通过自动化验收：一条命令生成 `.kf.txt` 并在 stderr 显示进度，
resize 走静态链接的 swscale 正式路径（自研缩放器已移除）。本文描述当前目标行为与验收范围。

## 最终流程

```text
视频 → FFmpeg 解码（显示顺序）→ FFmpeg resize（灰度分析平面）
     → WWXD（保持原有判定）→ <输入完整文件名>.kf.txt
                           ↘ CLI 进度（stderr）
```

沿用现有 Rust 项目和进程内 FFmpeg C shim。正式 resize 是静态链接的 libswscale；已验证的自研缩放器
与 `oracle` 对照随迁移收尾移除，仓库只保留一条 resize 实现。
仅链接所需库与组件，以 exe 加必需 DLL 的总发布体积评估变化；开发 SDK 和静态库文件大小不等于发布体积。
静态链接 swscale 不要求打包 FFmpeg CLI，具体体积增量必须构建后测量。

职责保持简单：`ihni-core` 只做 WWXD，`ihni-media` 负责解码与 resize，根包负责串联、文件输出和进度。
core 不依赖 FFmpeg、容器、CLI 或文件输出。保留有用的现有接口，不为了减少文件数重写整个仓库。

## CLI 与文件输出

日常调用：

```text
ihni video.mkv
ihni video.mkv -o cuts.kf.txt --analysis-height 360
```

默认输出 `video.mkv.kf.txt`（追加后缀，不替换原扩展名）。提供 `-o/--output`、`--analysis-height`、
`--no-progress`；现有 `--stream-index`、`--decoder-threads`、`--stats` 作为有实际用途的选项保留。
主 CLI 移除 JSON、时间派生、检测模式和未实现的检测线程参数；不借此删除库内部必要的帧元数据。

格式为 UTF-8、无 BOM、LF 换行，文件末尾有换行。例如：

```text
# keyframe format v1
fps 23.976024
0
42
125
```

- 帧号是解码显示顺序中的零基索引，切点指向新镜头第一帧，不是编码 I 帧。
- 非空视频由输出层补一次 `0`，之后写 WWXD 的严格递增切点；不改 core 的非零边界类型和 golden。
- FPS 行优先使用有效的 `average_frame_rate`，其次 `frame_rate`，保留六位小数；都不可用时写 `fps 0.000000`。
  它只是格式元数据，不能用来重算帧号或伪造源 PTS；VFR 同样按真实解码帧计数。
- 有视频流但零个可解码帧的输入报错，不产生关键帧文件。没有切点的非空视频仍输出 `0`。
- 默认拒绝覆盖已有文件，不截断输入；路径含中文、空格或无扩展名均正常工作。
- 完整解码成功后才写文件。同目录临时文件写完、检查 flush，再以不覆盖方式发布；失败不发布部分结果。
  临时文件清理失败也报告具体路径。成功信息必须等输出发布成功后才能显示。
- stdout 保持为空，进度、完成路径、错误和可选统计写 stderr。退出码：成功 0，运行或写入错误 1，参数错误 2。

上述路径、FPS 回退、拒绝覆盖和错误策略是本次设计的默认选择。
[GenKFR 输出实现](https://github.com/Nemuboshi/GenKFR/blob/main/src/main.rs)提供格式和后缀参考；
[Aegisub 自身读写实现](https://github.com/TypesettingTools/Aegisub/blob/master/libaegisub/common/keyframe.cpp)
接受 FPS 行且自身保存为零，因此缺少 FPS 不应阻止按帧号输出。
GenKFR 的检测实现使用 SCXvid；这里只借鉴使用方式，检测算法仍是 WWXD。

## Resize、解码和进度

分析高度默认 360，宽度按源宽高比四舍五入到 4 的倍数，半值向上。
沿用现有安全界限：各维度 48..4096，总像素不超过 2,097,152。
支持现有 8/10-bit 灰度与 YUV 亮度输入；使用真实 stride，不转换 RGB。
正式路径固定单个 swscale context、灰度源、`SWS_BICUBIC` 默认参数、`gray16le` 结果再 `>> 8`；
digest 与 WWXD 结果已验证不变（任务 3），见 DECODE.md。直接输出 gray8 已有不一致证据，不能无验证替换。

所有帧按显示顺序处理，不丢帧、不采样；保留原始帧号和有效 PTS。
缺少 PTS 不妨碍按帧号检测，不从平均 FPS 生成源时间戳。
尺寸或像素格式中途变化明确报错；损坏、截断和解码错误必须标为未完成。
保留有界缓冲和复用内存，不保存完整视频。

进度默认开启，显示已处理帧数、关键帧数（包括输出层的首帧）、耗时和平均处理 FPS。
不要求知道总帧数，不显示估算为精确值的百分比或 ETA。
TTY 最多约每 200 ms 原地更新，重定向 stderr 时约每 2 s 一行；`--no-progress` 只关闭周期进度。
解码结束后仍须等待输出写入，失败时不得显示成功。进度渲染失败停止后续进度，不改变实际处理结果。

## WWXD 正确性和性能

参考固定为 `vapoursynth-wwxd` 的 `a5870862fd85138b5e4822a076377b96592c9cd2`。
保留 cut、shot age、macroblock predictor 和 padded-plane digest golden 校验。
不调整阈值，不加入近似模式、镜头过滤或新检测算法。

性能工作以 release 同条件前后测量为依据：先拆清解码、resize、检测、总耗时，再 profiling，优化实际热点。
检测状态依赖顺序，不能把随意并行化当作默认方案。不承诺尚未测量的提速倍数。
检测独立 FPS 与包含解码的端到端 FPS 分别报告；CPU 占用率不是验收目标。

## 范围与当前差距

不做 GUI、批处理框架、插件、视频编码、通用缩放库、持久缓存或准确率评估系统。
当前已有解码、固定版本 WWXD、golden 和有界预读；resize 是唯一的 swscale 正式路径（旧 Rust 缩放器已移除）；
输出为默认追加 `.kf.txt` 的文件（stdout 保持空，进度与 `--stats` 写 stderr），运行中有实时进度；
`--stats` 已按 producer/consumer 拆分，`producer_decode`、`producer_resize` 与 `queue_wait` 分列，等待时间不再冒充解码成本。

按 [ROADMAP.md](ROADMAP.md) 完成输出、进度、resize 迁移、WWXD 实测优化和收尾验收；
Aegisub 导入与 TTY 外观仍属人工验收项。
