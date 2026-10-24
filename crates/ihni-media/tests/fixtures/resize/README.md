# Resize regression fixtures

These files are recorded from the linked libswscale, once, through the same
fixed configuration the production resize path uses (`SWS_BICUBIC` defaults,
gray source, `gray16le` destination, 64-byte-aligned destination stride).
They are committed so CI can verify the production resize bytes without
FFmpeg installed or a video fixture generated.

Each case is a pair of files:

- `<name>.in` — the source plane, raw, with a stride larger than its width.
- `<name>.out` — the `gray16le` plane swscale produced.

`index.txt` describes each case, one line per case:

```text
<name> <format> <src_w> <src_h> <src_stride> <dst_w> <dst_h> <dst_stride>
```

The source stride is recorded because the resize path is required to honour
it; the destination stride is the 64-byte-aligned stride the library was
given, which is what a real libav caller passes.

The test that reads these is `crates/ihni-media/tests/resize_fixtures.rs`.
It drives the production `FrameResizer` and compares its 8-bit analysis plane
against the high byte of every recorded sample — the reduction the resizer
performs is exactly `>> 8`.

These are frozen regression data, like the committed digest lists. The
generator (`examples/scale_fixtures`) and the retired Rust resampler
(`ihni-scale`) were removed with the resize migration, so nothing regenerates
these files. A mismatch means the linked swscale or the resize path changed
the bytes the detector sees: a review event, not a routine update.
