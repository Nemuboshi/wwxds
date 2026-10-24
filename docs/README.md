# IHNI 文档

项目只围绕一条流程：读取视频 → resize → WWXD → Aegisub 关键帧文件，CLI 显示进度。

- [PROJECT.md](PROJECT.md)：目标、输出格式和范围；产品要求以此为准。
- [ROADMAP.md](ROADMAP.md)：修正计划、执行顺序和验收条件；任务 1–5 的代码与自动化验收已完成，
  Aegisub 导入、TTY 外观和 Linux WWXD oracle 再生为人工/环境项，见 ROADMAP 状态行。
- [DECODE.md](DECODE.md)：现有解码、FFI、构建，以及 resize 迁移所需的兼容性证据。
- [WWXD_DESIGN.md](WWXD_DESIGN.md)：固定版本 WWXD 的算法、状态顺序和 golden 一致性依据。
- [PERFORMANCE.md](PERFORMANCE.md)：历史实测数据和测量方法，不代表本轮重新验证。

工具链、FFmpeg 接入和旧缩放器说明已合并进 DECODE.md，不再分别维护。
