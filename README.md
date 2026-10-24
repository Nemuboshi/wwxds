# wwxds

wwxds is a standalone hard-cut detector for video. It is a standalone version of
[WWXD](https://github.com/dubhatervapoursynth/vapoursynth-wwxd). It runs as one
binary. It does not need VapourSynth.

wwxds decodes a video, scales each frame to an analysis plane, and detects hard
cuts. It writes the keyframes to an Aegisub `.kf.txt` file.

## Features

- Decoding and scaling in one pipeline. The scaler is an exact integer box
  filter.
- Keyframe output in the Aegisub `.kf.txt` format.
- An experimental D3D11 backend for Windows. It decodes on the GPU and filters
  on the GPU. The CPU path is the default and the fallback.

The project is inspired by
[vapoursynth-wwxd](https://github.com/dubhatervapoursynth/vapoursynth-wwxd).

## Build

You need a stable Rust toolchain. Run:

```sh
cargo build --release
```

## License

wwxds uses the [GPL-2.0](LICENSE) license. It inherits this license from WWXD.
