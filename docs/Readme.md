# IHNI Technical Documentation

IHNI detects hard-cuts in video and generates Aegisub `.kf.txt` keyframe files.

- [Architecture.md](Architecture.md): System data pipeline, crate dependencies, FFI shim, memory safety, and build specifications.
- [WwxdDesign.md](WwxdDesign.md): Pinned WWXD scene detection algorithm, macroblock SAD mathematics, and golden reference rules.
- [Performance.md](Performance.md): Performance measurement methodology, `RunStats` stage metrics, profiling hotspots, and benchmark records.
