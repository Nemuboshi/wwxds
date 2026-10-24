# Performance Record

This file preserves historical measurements, including the old roadmap's phase
names. They are not acceptance results for the correction plan in
[ROADMAP.md](ROADMAP.md), and were not rerun during the documentation cleanup.
No new speedup or package-size reduction is claimed. Historical `decode_s`
measures consumer wait; the correction measured producer decode and resize
separately first — see *Honest stage split and profiling* — and the later
sections of this file record every accepted change against that split.

Measurements exist to decide whether an optimization is accepted. An optimization
is accepted only when it improves measured throughput or reduces measured cost
without an unexplained change in output.

## Method

- Release builds only (`cargo build --release`). The benchmark example warns when
  it is built without optimizations.
- The same input file, analysis size, thread settings, and hardware for a before/
  after pair. Never compare numbers measured under different conditions.
- Measurements on a shared machine drift. Processes the benchmark did not start,
  and their disk activity, move wall time by more than some of the effects
  measured here, so a single run is not evidence. Before/after results are
  measured as *interleaved pairs*: the two builds alternate inside one
  configuration, so ambient load lands on both sides. The spread between runs is
  reported with every result, and a difference smaller than that spread is
  recorded as unresolved rather than as a win or as a loss.
- `examples/bench.rs` reports three wall statistics for this reason:
  `wall_median_s` (the number to compare across builds), `wall_min_s` (the
  least contaminated estimate of what the build can do, because other processes
  can only add time), and `wall_max_s` (the spread over the measured runs).
- The input is read from the page cache after the warm-up run, and the tool
  writes nothing to disk, so disk activity from other processes affects a
  configuration's first run rather than its measured ones. Process start-up and
  probe are inside every measured run; the pre-migration rows also include the
  decoder spawn and the pipe.
- Report throughput in frames per second and say whether decoding is included.
  End-to-end numbers in this file include decoding; the detector-only replay rows
  (the *SSE2 `sad16`* and *SSE2 `dev16`* measurement tables) exclude it and are
  labelled as detector-only in their own headers and prose.
- Report stages separately: decode, prepare (padding), detect, and total wall
  time. Stage times are measured independently and are not derived from the
  total. Before the roadmap correction the stage list was `decode`, `prepare`,
  `detect`, and `total`, and `decode_s` was the time the consumer spent
  *waiting* for a prepared frame — the producer's real decode and resize cost
  was invisible. From the *Honest stage split* section below the stages are
  `producer_decode`, `producer_resize` (measured on the producer thread inside
  `ihni-media`), `queue_wait` (the consumer's wait, which the old `decode`
  actually measured), `prepare`, `detect`, and `total`. Producer stages and the
  queue wait overlap by design, so they must never be summed to reconstruct
  the wall time; the consumer-side stages (`prepare` + `detect` + `queue_wait`)
  are additive with each other because they run in the measured thread. The
  detector's own per-frame instrumentation (the `Instant::elapsed()` stamp in
  `detect_video_with_progress` added during the correction work, plus the
  per-frame stage pairs in `Detector::process` and the reader) costs one
  platform clock read per stage per frame, about 10^-6 of a frame's cost:
  historical rows measured without it are not a usable before-side for any
  difference smaller than the run-to-run spread.
- `total_s` includes process start-up, probe, decode, scaling, padding, and
  detection.

Commands:

```bash
cargo build --release
target/release/ihni --stats INPUT
cargo run --release --example bench -- INPUT --iterations 5
```

`--stats` writes one line to standard error after a successful run. Before the
honest stage split (all historical rows in this file) it read:

```text
stats frames=2879 decode_s=9.232679 prepare_s=0.165965 detect_s=1.839153 total_s=11.345074 fps=253.767 analysis=640x360 mode=exact threads=1 read_ahead=2
```

From the *Honest stage split* section it reads (this line is the real output
of the split-instrumented build on the recorded input):

```text
stats frames=2879 producer_decode_s=2.871883 producer_resize_s=7.286815 queue_wait_s=8.803634 prepare_s=0.088038 detect_s=1.300224 total_s=10.204193 fps=282.139 analysis=640x360 mode=exact read_ahead=2
```

## Recorded environment

| Item | Value |
| --- | --- |
| CPU | a quad-core x86-64 CPU, 4 cores, 8 logical processors |
| OS | Windows 11 x64 (10.0.26100) |
| Rust | rustc 1.98.1 (48a229cea 2026-09-01), cargo 1.98.1 |
| FFmpeg | N-121062-gfaba1501d for the child-process baselines; libav* 9.0.2 linked in-process for the Phase 5 rows |
| Build | `cargo build --release` (release profile, LTO settings from Cargo.toml) |

## Input

| Item | Value |
| --- | --- |
| Path | `target/bench/ramparts-120s.mkv` (ignored; derived from a local 1080p source) |
| Source | H.264 1920x1080, 8-bit, 24000/1001, first 120 s, stream copy |
| Frames | 2879 |
| Analysis size | 640x360 |
| Decoder threads | FFmpeg default (8 logical processors available) |

This clip is not committed. It is reproducible with `ffmpeg -c copy -t 120` from
the same source, but the encoded bytes depend on the source file, so the frame
count and analysis size are the stable identifiers.

## Baselines

### Before the inner-loop rewrite (commit `16733b5` behaviour)

Serial `detect_video`, three runs:

| Run | decode_s | prepare_s | detect_s | total_s | fps |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 2.899 | 0.150 | 10.247 | 13.406 | 214.8 |
| 2 | 2.915 | 0.150 | 10.532 | 13.704 | 210.1 |

### After the inner-loop rewrite

Serial `detect_video`, three runs:

| Run | decode_s | prepare_s | detect_s | total_s | fps |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 8.217 | 0.121 | 1.526 | 9.973 | 288.7 |
| 2 | 8.030 | 0.119 | 1.541 | 9.799 | 293.8 |
| 3 | 8.110 | 0.126 | 1.549 | 9.891 | 291.1 |

Detect time fell from about 10.3 s to about 1.5 s (about 6.8x). Total wall time
fell from about 13.5 s to about 9.9 s. The boundary list on the recorded input
was compared line by line against a build of the previous commit and is
identical (39 boundaries).

The decode stage appears larger after the rewrite (about 2.9 s to about 8.1 s)
because the two builds were measured against different background load, not
because decoding changed: the decoder arguments and the pipe format are
identical. Decode-only ffmpeg runs on the same input measured 7.7 s to 10.1 s
depending on run order, so this stage is dominated by the decoder and by
contention, not by this tool. Treat cross-build `decode_s` comparisons as
unreliable until both sides are re-measured back to back; the interleaved-pair
protocol in *Overlapped decoding* below is how the later phases do that, and how
this pair would have to be re-measured to be believed.

## Where the time went before the overlap

On the recorded input, after the inner-loop rewrite, decoding is the dominant
cost. A decode-only ffmpeg run with the same filter chain takes about 8 s of the
10 s total, and hardware decoding (d3d11va) reduced the decode step to about
7.4 s with much higher CPU time and memory. The detector is no longer the
bottleneck for this configuration, which is why the roadmap put decode/detect
overlap before further detector parallelism.

## Overlapped decoding (Phase 2)

Decoding now runs on a producer thread ahead of detection (`FrameStream`, with
`READ_AHEAD_FRAMES = 2` reusable planes). Frames still reach the detector one at
a time, in display order, with the bytes a serial read would produce, so no
decision can change. That was checked, not assumed: every fixture in
`target/m1-fixtures` and the recorded input at all three analysis sizes produce
identical standard output, identical standard error (modulo FFmpeg's per-run heap
addresses in its diagnostics) and the same exit code on the serial build
(`f8a8fcc`) and the overlap build — 39, 46 and 50 boundaries on the recorded
input, listed identically. What changed is where the time goes.

### How these numbers were measured

The serial build (`f8a8fcc`, in a separate worktree) and the overlap build
alternate inside each configuration — serial, overlap, serial, overlap — so
ambient load and disk activity land on both sides. One warm-up run plus three
measured runs per row, same input, one detection thread, FFmpeg automatic thread
counts. The rows are listed in the order they ran, so the drift is visible
instead of being averaged away.

The ambient load was sampled once every 4 s during the follow-up run below and
the idle period after it. The numbers are in *Ambient load*, and they matter
because on this machine they are the same order of magnitude as the effects being
compared.

| Analysis | Row | Build | wall_s | wall_median_s | decode_s | prepare_s | detect_s |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 640x360 | 1 | serial | 10.213 | 10.024 | 8.550 | 0.099 | 1.424 |
| 640x360 | 2 | overlap | 11.551 | 11.055 | 9.518 | 0.157 | 1.727 |
| 640x360 | 3 | serial | 11.748 | 11.653 | 9.947 | 0.137 | 1.512 |
| 640x360 | 4 | overlap | 11.594 | 11.531 | 9.456 | 0.185 | 1.812 |
| 1280x720 | 1 | serial | 14.688 | 14.466 | 7.949 | 0.727 | 5.890 |
| 1280x720 | 2 | overlap | 14.766 | 14.398 | 3.270 | 1.337 | 10.025 |
| 1280x720 | 3 | serial | 14.935 | 14.925 | 8.077 | 0.727 | 6.015 |
| 1280x720 | 4 | overlap | 14.610 | 14.567 | 2.827 | 1.353 | 10.290 |
| 1920x1080 | 1 | serial | 29.686 | 30.433 | 16.751 | 1.491 | 11.307 |
| 1920x1080 | 2 | overlap | 22.607 | 22.350 | 6.166 | 1.662 | 14.617 |
| 1920x1080 | 3 | serial | 25.371 | 25.455 | 14.681 | 1.222 | 9.350 |
| 1920x1080 | 4 | overlap | 22.660 | 22.837 | 6.168 | 1.580 | 14.772 |

### Verdicts

A pair is one adjacent serial/overlap pair, which is the comparison least exposed
to drift. The ratio is the overlap's mean wall time over the serial build's, so a
value below 1.00 is faster.

| Analysis | Pair 1 | Pair 2 | Verdict |
| --- | ---: | ---: | --- |
| 640x360 | 1.13 | 0.99 | **Unresolved.** The serial build's own two rows are 15% apart (10.21 s and 11.75 s), which is larger than the 4% being measured, and the two pairs disagree in sign. Decoding is the whole wall time here (8.6 s of 10.2 s) and is slower than detection, so there is nothing for the overlap to hide. |
| 1280x720 | 1.01 | 0.98 | **Unresolved.** All four rows fall between 14.61 s and 14.94 s — a 2.3% span — and the paired differences (1-2%) sit inside it. |
| 1920x1080 | 0.76 | 0.89 | **Real.** Both pairs agree and the margin survives the drift: the serial build's *better* row (25.37 s) against the overlap's *worse* row (22.66 s) still leaves the overlap 12% ahead, and the overlap's two rows agree to within 0.2% while the serial build's differ by 15%. |

Two effects are visible in every row, and both are real. Which one wins depends
on the analysis size:

- **The overlap works.** The consumer's wait (`decode_s`) falls by more than half
  wherever there is detection to hide: 7.95 s to 3.27 s at 720p and 16.75 s to
  6.17 s at 1080p. At 640x360 it does not fall — it *rises*, from 8.55 s to
  9.52 s — because the decoder is slower than the detector there, so the consumer
  is waiting in any case and the producer thread can only take cycles away from
  the decoder it is competing with.
- **The overlap costs.** `detect_s` grows at every size (+21% at 640x360, +70% at
  720p, +29% at 1080p) and `prepare_s` grows with it. The detector thread and
  FFmpeg's threads compete for four physical cores, and the decoder is slowed in
  return. This is not a measurement artefact: the same work is spread over the
  whole run instead of occupying the machine after decoding has finished.

So the overlap pays only where the work it hides exceeds the interference it
causes. At 1920x1080 it saves 6-7 s of a 25-30 s run. At the default 640x360 the
run is decode-bound and the overlap is at best neutral. The acceptance criterion
for this phase — "wall time improves" — therefore holds for large analysis sizes
and does not hold for the default one.

That historical crossover bounds the possible benefit. At 640x360, where
detection is about 14% of the wall time, making the detector arbitrarily fast
cannot improve throughput by more than that 14%; the lever for that configuration
is decoding. At larger analysis sizes detection occupied 35-45% of serial wall
time. These results do not establish a benefit from detector parallelism.

### Ambient load

`Get-Counter '\Processor(_Total)\% Processor Time'` was sampled every 4 s for 81
samples, covering the thread-budget follow-up run below and the idle period after
it. Total CPU is across the eight logical processors.

| Window | Total CPU | Interpretation |
| --- | --- | --- |
| During the follow-up run | 73-93% | the benchmark itself, which is expected to saturate the machine |
| After it, with nothing of ours running | 13-24% | about one to two logical processors of background work |
| Whole window | mean 80%, min 13% | the floor is not zero |

Roughly 300 processes are alive throughout, nearly all of them the operating
system's and the desktop's. **About one to two of the eight logical processors
are busy with work this tool did not start, at all times.** On four physical
cores that is 12-25% of the machine, which is the same order as the effects
measured above, and it is why the run-to-run drift at 640x360 (15%) is larger
than the difference being tested there (4%).

Two consequences for reading this file:

- A single run, or a pair of runs, cannot resolve a difference of a few percent
  on this machine. The verdicts above are stated on that basis: only the
  1920x1080 result is larger than the drift it sits in.
- The interleaved run in the table above predates the sampler, so its rows carry
  no load stamp. Its own drift is the evidence that the load was varying; the
  follow-up run is the one with a measured load beside it.

### Thread budget

**Pre-migration measurement.** This section was measured with the child-process
decoder, which ran a `scale` filter graph. `--filter-threads` no longer exists —
the in-process decoder has no filter graph — so the exact configuration below
cannot be reproduced on the current binary. The result is kept because the
conclusion (on four physical cores the decoder and the detector must share, and
reserving processors does not pay) is a property of the machine, and it is
recorded here as the baseline the in-process measurements will be compared
against.

FFmpeg chooses its own thread counts by default, and on four physical cores it
takes nearly all of them. Reserving logical processors for the detector was
measured directly: `--decoder-threads 4 --filter-threads 2` on both builds, same
interleaved protocol, one warm-up plus three measured runs per row.

| Analysis | Row | Build | wall_s | wall_median_s | decode_s | prepare_s | detect_s |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 640x360 | 1 | serial | 12.130 | 12.188 | 10.457 | 0.120 | 1.422 |
| 640x360 | 2 | overlap | 12.803 | 12.824 | 10.942 | 0.149 | 1.595 |
| 640x360 | 3 | serial | 13.250 | 13.043 | 11.533 | 0.132 | 1.458 |
| 640x360 | 4 | overlap | 12.927 | 13.065 | 10.961 | 0.151 | 1.668 |
| 1280x720 | 1 | serial | 15.990 | 16.446 | 8.532 | 0.703 | 6.633 |
| 1280x720 | 2 | overlap | 14.791 | 14.744 | 6.254 | 0.934 | 7.480 |
| 1280x720 | 3 | serial | 14.861 | 14.843 | 8.361 | 0.653 | 5.735 |
| 1280x720 | 4 | overlap | 14.926 | 14.875 | 6.398 | 0.905 | 7.498 |

The stage times are the stable part of this result; wall times drift. Read them
first:

- **The decoder gets slower, as intended.** Serial `decode_s` rises from 8.55 s
  to 10.96 s at 640x360 and from 7.95 s to 8.45 s at 720p, and the serial wall
  time rises with it (640x360: 10.98 s to 12.69 s; 720p: 14.81 s to 15.43 s).
- **The overlap's interference shrinks, which was the point.** `detect_s` growth
  falls from +70% to +21% at 720p and from +21% to +13% at 640x360. Holding
  logical processors back for the detector does reduce what the second thread
  costs the decoder.
- **No verdict changes.** 640x360 stays unresolved, and its pair ratios flip sign
  again (1.06 and 0.98). 1280x720 stays unresolved too: the pairs are 0.93 and
  1.00, while the serial build's own two rows are 7.6% apart (15.99 s and
  14.86 s).

So there is no thread-count policy to recommend from this evidence. The
interference is real and it is cappable, but paying for it with a slower decoder
produces no wall-time win that survives the noise on this machine. The useful
conclusion is a deployment one: with four physical cores, the decoder and the
detector have to share, and at this analysis size the machine does not have
enough cores for the overlap to pay. FFmpeg's automatic choice stays the default,
and `--decoder-threads` stays available for a machine where the balance is
different.

## In-process decoding and resampling (Phase 5)

The child-process decoder, its `scale` filter graph, and the pipe were replaced
by in-process libavformat/libavcodec and the project's own resampler. The output
is unchanged — every per-frame digest list and the 39/46/50 boundary counts are
identical — so this section is only about time.

### How these numbers were measured

Same protocol as the overlap measurement: the pre-migration build (`f8a8fcc`,
built in a separate worktree) and the current build alternate inside each
configuration — before, after, before, after — so ambient load lands on both
sides. One warm-up run plus one measured run per row, three rows per analysis
size, same input, one detection thread, FFmpeg automatic decoder thread count.
Rows are listed in the order they ran.

| Analysis | Row | Build | wall_s | decode_s | prepare_s | detect_s |
| --- | ---: | --- | ---: | ---: | ---: | ---: |
| 640x360 | 1 | before | 9.005 | 7.367 | 0.097 | 1.431 |
| 640x360 | 1 | after | 9.224 | 7.290 | 0.124 | 1.694 |
| 640x360 | 2 | before | 9.053 | 7.410 | 0.097 | 1.443 |
| 640x360 | 2 | after | 9.051 | 7.150 | 0.122 | 1.672 |
| 640x360 | 3 | before | 9.120 | 7.478 | 0.095 | 1.445 |
| 640x360 | 3 | after | 9.289 | 7.407 | 0.126 | 1.641 |
| 1280x720 | 1 | before | 12.058 | 6.655 | 0.573 | 4.728 |
| 1280x720 | 1 | after | 11.749 | 2.738 | 0.895 | 8.002 |
| 1280x720 | 2 | before | 12.159 | 6.665 | 0.562 | 4.818 |
| 1280x720 | 2 | after | 12.612 | 3.165 | 0.982 | 8.342 |
| 1280x720 | 3 | before | 14.538 | 8.071 | 0.547 | 5.782 |
| 1280x720 | 3 | after | 12.260 | 2.999 | 0.905 | 8.215 |
| 1920x1080 | 1 | before | 20.936 | 12.376 | 0.958 | 7.498 |
| 1920x1080 | 1 | after | 18.123 | 5.993 | 1.205 | 10.795 |
| 1920x1080 | 2 | before | 26.377 | 14.810 | 1.152 | 10.299 |
| 1920x1080 | 2 | after | 21.120 | 6.294 | 1.370 | 13.330 |
| 1920x1080 | 3 | before | 22.595 | 12.998 | 1.023 | 8.472 |
| 1920x1080 | 3 | after | 21.332 | 6.447 | 1.445 | 13.218 |

### Verdicts

Ratio is after over before within one adjacent pair; below 1.00 is faster.

| Analysis | Pair 1 | Pair 2 | Pair 3 | Median ratio | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| 640x360 | 1.024 | 1.000 | 1.018 | 1.018 | **Unresolved, and not a win.** The three pairs span 2.4% around no change, and decoding is 82% of the wall time. Nothing was hiding the pipe copy: at this size the cost was dominated by the H.264 decode itself, not by moving 460 KB per frame. |
| 1280x720 | 0.974 | 1.037 | 0.843 | 0.974 | **Unresolved.** The pairs disagree in sign and the before side's own rows span 21% (12.06 s to 14.54 s), which is larger than the effect. |
| 1920x1080 | 0.866 | 0.801 | 0.944 | 0.866 | **Real.** All three pairs favour the in-process build, by 5.6% to 19.9%, and the after side's three rows span 18% while the before side's span 26%. |

The stage times explain the 1080 result, and they are the more stable part of the
measurement:

- **The consumer's wait roughly halves wherever detection has work to hide.**
  `decode_s` falls from 6.66 s to 2.74-3.16 s at 720p and from 12.38-14.81 s to
  5.99-6.45 s at 1080p. At 640x360 it does not move (7.37 s to 7.29 s), which is
  the same decode-bound configuration the Phase 2 section describes.
- **`detect_s` rises, as it must when overlap actually happens.** At 1080p it
  goes from 7.50-10.30 s to 10.80-13.33 s. That is not a slower detector: the
  same work now runs concurrently with the decoder instead of after it, and the
  two compete for four physical cores. This is the Phase 2 pattern, unchanged.

### What this does and does not show

The migration's own claim was that removing the pipe and the process boundary
would save one full copy of every frame. It does remove that copy — the
pre-migration reader read `width * height * 2` bytes per frame through a pipe
(460,800 bytes at 640x360, 4.1 MB at 1920x1080) — but the copy was never the
bottleneck. At the default analysis size the wall time is unchanged, and the
honest reading is that the win is at large analysis sizes, where the decode that
overlap can hide is a larger share of the run.

What the migration does buy unconditionally is the things the measurement above
cannot show: no `ffmpeg` binary on `PATH`, no per-frame process boundary, exact
presentation time stamps (`--source-time`), one thread budget instead of two, and
a resampler that is verified against recorded vectors without FFmpeg installed.

## Honest stage split and profiling (correction plan task 4)

The `decode_s` column in every row above is the consumer's *wait* for a
prepared frame; the producer's real decode and resize cost was invisible. This
section instruments it and records what the pipeline's time actually goes to.

### Profiling attempts, and what was used instead

A sampling profiler was wanted before choosing what to optimize. Three tools
were tried on this machine:

- `samply 0.13.1` (installed with `cargo install samply`, then
  `samply record --save-only -r 1000 -o profile.json -- target/release/ihni.exe …`
  against a release build with a temporary, uncommitted `[profile.release]
  debug = 1`): the recorder never produced a profile file. On this session it
  hangs at start-up even for `sleep 2`, with no output even under `-v`.
- `wpr -start CPU` (Windows Performance Recorder): fails with
  `Failed to enable the policy to profile system performance`.
- `logman create trace … -ets`: access denied.

The cause is the session token: `whoami /groups` shows
`BUILTIN\Administrators … 只用于拒绝` (deny-only) at Medium Mandatory Level,
so this process has no ETW permission, and every kernel-trace profiler
(samply, WPR, logman) needs it. No user-space sampling profiler is installed,
and none was added (the plan keeps dependencies to a minimum).

Per the fallback agreed for this task, the profile is therefore **stage-split
timing plus scripted micro-harnesses**, not a sampling profile, and this file
never claims sampling-profiling that did not run:

1. The producer/consumer split below is the plan's own "measure decode, resize,
   detect, and total separately" requirement, and it localizes the cost.
2. `examples/bench.rs --detector-only` (added in this task, committed) replays
   a fixed 120-frame analysis sequence captured once from the recorded input
   through a freshly built `Detector` per iteration, and is the before/after
   instrument for detector changes.
3. Two throwaway harnesses (not committed) resolved where within a stage the
   time goes: a resize split (`swscale` call vs the `>> 8` reduction on the
   production geometry, fixed synthetic plane, 30 iterations, min/median over
   3 runs) and a detector kernel split (`sad16`/`sad32v`/`dev16` formulations
   on padded planes at 640x360 geometry, 4096 sampled origins, correctness
   checked against the committed formulation before timing, 9 interleaved
   rounds). Their section-by-section numbers are recorded below with the
   commands reproduced as closely as a removed harness allows.

### Where the time actually goes (measured with the new instrumentation)

Same input, configuration, and machine as the rows above: release build,
640x360 analysis, one detection thread, FFmpeg automatic decoder threads.
One warm-up plus five interleaved runs of the *unmodified previous binary*
(`4581874`, built in a separate target directory) alternating with the
timing-instrumented build, to show both the honest split and that the
instrumentation itself costs nothing measurable.

Previous binary (consumer-side view only; `decode_s` is the wait):

| Run | decode_s (wait) | prepare_s | detect_s | total_s |
| --- | ---: | ---: | ---: | ---: |
| A1 | 8.723 | 0.091 | 1.288 | 10.113 |
| A2 | 8.847 | 0.092 | 1.324 | 10.273 |
| A3 | 8.930 | 0.089 | 1.313 | 10.343 |
| A4 | 8.853 | 0.091 | 1.309 | 10.263 |
| A5 | 8.905 | 0.093 | 1.318 | 10.327 |
| A6 | 8.744 | 0.090 | 1.303 | 10.149 |
| median | **8.847** | **0.091** | **1.309** | **10.263** |

Instrumented build (the producer stages are new and overlap the wait — they
are *not* to be added to the wait or to each other):

| Run | producer_decode_s | producer_resize_s | queue_wait_s | prepare_s | detect_s | total_s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| B1 | 2.914 | 7.287 | 8.843 | 0.089 | 1.299 | 10.242 |
| B2 | 2.902 | 7.345 | 8.867 | 0.093 | 1.319 | 10.291 |
| B3 | 2.918 | 7.435 | 8.983 | 0.088 | 1.316 | 10.398 |
| B4 | 2.855 | 7.292 | 8.785 | 0.090 | 1.306 | 10.191 |
| B5 | 2.892 | 7.176 | 8.704 | 0.090 | 1.304 | 10.109 |
| B6 | 2.906 | 7.277 | 8.814 | 0.090 | 1.310 | 10.225 |
| median | **2.902** | **7.287** | **8.814** | **0.090** | **1.306** | **10.225** |

Reading:

- **Resize, not decode, is the pipeline's producer cost.** The producer spends
  a median 7.29 s in swscale-plus-reduction against 2.90 s in libavcodec on
  this input (the 2.9 s also sits beside the Phase 5 claim that the *decode*
  itself is not what the overlap was hiding). At 1920x1080 analysis the same
  split is roughly 15.1 s resize vs 0.77 s decode. The resize dominates
  because it reads the full 1920x1080 source plane for every frame
  (bicubic downscale by 3) — that is the contract, and it is native swscale.
- **The consumer's wait (8.81 s) and the producer stages overlap.** The wall
  time is not any sum of these columns; the old single `decode_s` column was
  the wait, and the producer's real cost was invisible.
- **The instrumentation costs nothing measurable.** Pairwise
  before/after ratios (A vs B within each adjacent pair) are 1.013, 1.002,
  1.005, 0.993, 0.979, 1.008 — the sign flips between pairs and the spread
  inside A alone (10.11–10.34, 2.3%) is larger than any of the differences.
  Verdict: **unresolved, i.e. no measurable cost**, which is the honest
  outcome for a per-frame `Instant` pair around three stage calls. The
  Task 2 per-frame elapsed stamp is inside *both* sides' builds here and is
  covered by the same verdict.
- **Boundaries unchanged:** the recorded input produced byte-identical
  `.kf.txt` (39 boundaries, md5 `46cbb08017aba4d5610e807f8d1c95cc`) on the
  previous binary and the instrumented one, at 640x360, 1280x720, and
  1920x1080; `cargo test -p ihni-media --all-features` (the committed digest
  lists) and the full workspace suite pass.

### Candidate resolution by micro-harness (no optimization accepted yet in this commit)

With the split measured, the two named candidate areas were tested on fixed
inputs before any production change:

- **The `>> 8` reduction loop** (`resize.rs`, the project-side half of the
  resize stage): the throwaway resize split measured the `swscale` call at a
  median 1.27–1.30 ms and the reduction at 0.112–0.115 ms per frame at the
  production geometry (30 iterations, 3 runs; the reduction is ~8% of the
  resize stage, ~1% of wall time). A reformulated reduction (`chunks_exact(2)
  .map` instead of index arithmetic) measured byte-identical and timed
  median 0.1091 ms against the committed formulation's 0.1091 ms (9 interleaved
  rounds of 40-rep bests; ratio 1.0000). **Rejected: inside noise**, and the
  possible gain is bounded by the 8% share anyway.
- **The detector kernels** (`sad16`, `sad32v`, `dev16` in `ihni-core`): the
  throwaway kernel harness sampled 4096 legal origins per family on
  production-geometry padded planes and compared the committed formulation
  against alternatives, verifying byte equality before timing. Per 120-frame
  detector-only replay the core spends ~1.16 M `sad16` calls (~73% of detect
  time), ~99 k `dev16` calls (~11%), and the rest in loop overhead. Timed
  medians per call: `sad16` 50.1 ns, `sad32v` (four `sad16` calls) 121.9 ns,
  `dev16` 40.3 ns; scalar row-unrolling variants were within noise of the
  committed code (ratio 1.011 — **rejected**). Only SSE2 `PSADBW` formulations
  moved anything (`sad16` ratio 0.172, `dev16` ratio 0.598; the four-quadrant
  32x32 `PSADBW` pass measured *slower* than four SSE2 `sad16` calls, so the
  minimal change wins: `sad32v` stays as four `sad16` calls). `PSADBW` is in
  the x86-64 instruction-set baseline, so no feature detection or new
  dependency is involved. The two SSE2 kernel changes are recorded, measured,
  and accepted in their own sections below.

## SSE2 `sad16` (accepted)

`sad16` on x86-64 runs one `PSADBW` per 16-byte row pair against the reference
plane and accumulates the two lane sums, replacing the byte-by-byte scalar
loop. `PSADBW` computes unsigned per-byte absolute differences and sums
eight of them into each 64-bit lane, so the row-pair sum is exactly the
scalar sum of `abs_diff` values, and every block result stays inside `u32`
(maximum 255 × 256 = 65 280) with room to spare in the `i64` accumulator. The
portable loop remains for non-x86-64 targets, and unit tests now pin the two
formulations against each other on pseudo-random planes at every legal
macroblock origin, at a shifted origin, and at the extreme fills (all-255 vs
all-0). The release-path bound checks (`assert!` on the 16×16 extent, the same
condition the `debug_assert!` pinned) replace the debug-only guard on the SIMD
path because the pointer loads would otherwise be unsound on a bad origin;
they never fire for the validated grids (docs/WWXD_DESIGN.md §6). No
threshold, predictor order, early-exit residual state, search-loop structure,
or `sad32v` composition changed.

### Measurement

`examples/bench.rs --detector-only` style replay (`detect_probe`, the same
capture-once replay-each-round scheme as the committed bench flag): warm-up
plus 5 interleaved rounds of 15 iterations each, baseline build `4581874` vs
this tree, same fixed 120-frame sequence captured once from the recorded
input, 640x360 analysis, one detector, fresh `Detector` per iteration.

| Pair | baseline median_ms | SSE2 median_ms | ratio |
| --- | ---: | ---: | ---: |
| 1 | 34.992 | 22.958 | 0.656 |
| 2 | 35.282 | 22.555 | 0.639 |
| 3 | 35.593 | 22.212 | 0.624 |
| 4 | 35.107 | 22.273 | 0.634 |
| 5 | 34.646 | 22.159 | 0.640 |
| medians | **35.107** | **22.273** | **0.634** |

Detector-only throughput 3 429 → 5 388 fps (median wall; min 33.4/21.6, max
42.3/26.6 — every pair's ratio sits far below the spread, and all five pairs
agree in sign). End-to-end on the same input is unchanged within noise, as
expected for a configuration the Phase 2/5 sections already show to be
resize-bound: the pipeline wall medians were 9.488 s before and 9.515 s after,
pair ratios 0.983–1.023 straddling 1.00 — **no end-to-end claim is made**. The
`--stats` `detect_s` column fell from a median 1.221 s to 0.798 s on the full
2 879-frame run (0.780–0.802 over the measured runs, never overlapping the
baseline's 1.174–1.235): the same ~1.5× on the consumer stage that the replay
attributes to `sad16`, which is about 73% of detect time, measured through the
pipeline rather than against a harness.

### Parity evidence

`cargo test -p ihni-core` (the committed `golden.json` replay: decisions, shot
age, predictor digests, padded-plane digests) passes;
`cargo test --all-targets --all-features` passes (148 tests, including the
committed analysis-digest media tests and the repeat-frame/motion/flash-style
regression fixtures `uniform_repeat`, `translation_motion`, `bright_ramp`,
`single_hard_cut`, `two_adjacent_cuts`, `single_tile_flip`, `odd_size_cut`);
the recorded input's boundary lists are byte-identical at 640x360, 1280x720,
and 1920x1080 (39/46/50 boundaries, same md5 `46cbb08017aba4d5610e807f8d1c95cc`
as before the change).

## SSE2 `dev16` (accepted)

The same `PSADBW` technique covers `dev16`'s two passes, on top of the
`sad16` change and measured against it (so the ratio attributes to `dev16`
alone, not to both). The sum pass is `PSADBW` against a zero vector: each
64-bit lane holds the sum of eight unsigned bytes, at most 8 × 255 = 2040,
so the lane extracts and the accumulator are exact and `sum / 256` is the
same integer the portable loop divides. The deviation pass is `PSADBW`
against the broadcast mean byte: the unsigned per-byte absolute difference
against a broadcast value is exactly `value.abs_diff(mean as u8)`, including
the same `mean as u8` truncation, and the row-pair accumulation matches the
portable loop's. The portable formulation stays for other targets and as the
comparison reference for the unit test that checks every legal macroblock
origin of a production-geometry padded plane. The `mean` truncation, the
`/ 256`, the thresholds around `dev` (`max(300)`, `> 1000`, `< 3000`), and
the call sites in the sampling loop are untouched.

### Measurement

The same detector-only replay protocol, this time against the accepted
`sad16` tree as the before side (warm-up plus 5 interleaved rounds of 15
iterations, 640x360 sequence captured once from the recorded input):

| Pair | sad16-only median_ms | + dev16 median_ms | ratio |
| --- | ---: | ---: | ---: |
| 1 | 22.541 | 18.744 | 0.832 |
| 2 | 22.328 | 18.631 | 0.834 |
| 3 | 22.097 | 18.821 | 0.852 |
| 4 | 22.229 | 18.812 | 0.846 |
| 5 | 22.479 | 18.673 | 0.831 |
| medians | **22.328** | **18.744** | **0.840** |

Detector-only throughput 5 374 → 6 402 fps on top of the `sad16` step;
cumulative from `4581874`: 34.63 → 18.74 ms median (1.85×), 3 429 → 6 402 fps.
Through the pipeline, `--stats` `detect_s` fell to a median 0.560 s on the
full 2 879-frame run (from 0.798 with `sad16` alone and 1.221 at the
baseline). End-to-end wall at 640x360 stays decode/resize-bound and inside
noise; at 1920x1080 analysis, where detection was 9.5–9.9 s of a 17 s run,
the two changes together moved the wall median 17.306 s → 16.166 s
(5 interleaved pairs, ratios 0.927–0.962, all favouring the change by more
than the spread — a real ~6–7% pipeline win at that analysis size).

### Parity evidence

`cargo test -p ihni-core` (golden `golden.json` replay) passes;
`cargo test --all-targets --all-features` passes (149 tests); the recorded
input's boundary lists stay byte-identical at 640x360, 1280x720, and
1920x1080 (39/46/50, same md5); fixtures `cut8.mkv`, `long.mkv`, `vfr.mp4`,
`odd-97x65.mkv` produce identical output against `4581874`, and
`mixed-depth.mp4` fails with the same recorded mid-stream format-change
rejection on both sides.

## swscale new frame API (accepted for 8-bit, regression on 10-bit)

The scaler moved from the stateful `sws_getContext`/`sws_scale` pair to
`sws_alloc_context` + `sws_scale_frame`. The reason is not speed on its own: the
legacy pair always implies `SWS_BACKEND_LEGACY`, which is single threaded and
silently ignores `SwsContext.threads`, so the scaler could not use more than one
core at any thread setting. The new API honours the setting.

The shim runs it in **dynamic mode**: `sws_alloc_context`, set `threads` and
`sws_flags`, and do **not** call `sws_init_context`. Calling it with no geometry
makes every frame fail with `EAGAIN` (-11). Explicit initialisation was also
measured and is *slower* — 1.889 ms/frame at `threads=1` against dynamic mode's
1.044 — so it is not used.

### Parity evidence

Byte identity is exact, not approximate. The standalone harness produced the
same FNV digest (`d170ba72a6f80fb5`) at `threads` = 1, 2, 3, 4, 5, 6, 8 and
auto; the two committed digest lists pass (`analysis-digests.txt`,
`native-digests.txt`); `resize_fixtures.rs` reproduces the recorded swscale
bytes; and `.kf.txt` is byte-identical between the two binaries on all five
benchmark inputs. The scaler's thread count is a scheduling setting, not a
quality one.

### Measurement

Release build, 1000-frame 1462x1080 clips, default 488x360 analysis, one
detection thread, FFmpeg automatic decoder threads. Strict alternation
(legacy, new, legacy, new, …), one pair per round, so both builds see the same
conditions. Numbers are `total_s`.

| Input | Pairs | Legacy median | New median | Result |
| --- | ---: | ---: | ---: | --- |
| H.264 8-bit | 8 | 2.110 | 1.891 | **~10% faster**, every pair |
| H.264 10-bit | 4 | 2.122 | 1.904 | **~10% faster**, every pair |
| HEVC 8-bit | 4 | 2.259 | 2.163 | **~4% faster**, every pair |
| HEVC 10-bit | 8 | 4.272 | 4.328 | **~1.4% slower**, 7 of 8 pairs |

The stage split shows why. On 8-bit H.264 the producer is resize-bound
(0.16 s decode, 1.91 s resize), so parallelising the scaler shortens the real
bottleneck. On 10-bit HEVC the producer is CPU-saturated: resize falls 2.27 s →
1.05 s, but `producer_decode` (the `avcodec_receive_frame` wait) rises 1.94 s →
3.23 s. The decoder's worker threads absorb the freed cores, so the wall time
does not move — the same effect that made swscale threading look useless on
this input when measured standalone.

**This is a regression on the input the tool was benchmarked against.** It is
recorded as such. The alternative is to keep the legacy path and lose the 10%
on 8-bit; the new path is at worst 1.4% slower and at best 10% faster.

Thread count is left at `0` (automatic). Sweeping it on the 10-bit HEVC input
did not recover the loss: medians at `threads` = 0/2/4/6 were 4.396/4.381/
4.360/4.334 s against the legacy 4.354 s — the best explicit count merely ties
it. An explicit count cannot know how much CPU the decoder is using, so the
automatic choice is the only defensible default.

## Accepted optimizations

| Change | Evidence | Output effect |
| --- | --- | --- |
| Row-slice SAD/dev, `usize` offsets, `fill` padding | detect 10.3 s → 1.5 s on the recorded input | None: boundaries identical to the previous commit; golden parity passes |
| `-hwaccel none`, `-threads`, `-filter_threads` passthrough (child-process decoder, since removed) | Thread counts verified byte-identical; `hwaccel none` does not change output | None |
| Per-frame stage timings | Enables the measurements above | None |
| In-process decoding and resampling (Phase 5) | Interleaved pairs against `f8a8fcc`; see *In-process decoding and resampling* above | None: every per-frame digest list identical; 39/46/50 boundaries unchanged |
| Producer-side stage split (`producer_decode`/`producer_resize`/`queue_wait` in `RunStats`) | One warm-up + five interleaved pairs against `4581874` on the recorded input; pair ratios flip sign inside the spread — instrumentation costs nothing measurable; see *Honest stage split and profiling* | None: `.kf.txt` byte-identical at 640x360/1280x720/1920x1080 |
| swscale new frame API (`sws_scale_frame`, dynamic mode) | Strict alternating pairs, 1000 frames each: 8-bit H.264 2.110 s → 1.891 s, 10-bit H.264 2.122 s → 1.904 s, 8-bit HEVC 2.259 s → 2.163 s; see *swscale new frame API* | None: digest identical at every thread count; both committed digest lists pass; `.kf.txt` identical on all five inputs |
| SSE2 `PSADBW` `sad16` on x86-64 (portable fallback kept) | Detector-only replay 35.107 ms → 22.273 ms median (5 interleaved pairs, ratios 0.624–0.656); pipeline `detect_s` 1.221 s → 0.798 s; see *SSE2 `sad16`* | None: golden parity, committed digests, and byte-identical boundaries at all three analysis heights |
| SSE2 `PSADBW` `dev16` on x86-64 (portable fallback kept) | Detector-only replay 22.328 ms → 18.744 ms median on top of the `sad16` change (5 interleaved pairs, ratios 0.831–0.852); pipeline `detect_s` to 0.560 s; see *SSE2 `dev16`* | None: golden parity, committed digests, and byte-identical boundaries at all three analysis heights |

## Rejected or deferred

| Change | Reason |
| --- | --- |
| swscale new frame API on 10-bit HEVC | The same change that wins ~10% on 8-bit loses ~1.4% here: the producer is CPU-saturated, so parallelising the scaler only moves work off the critical path that the decoder then re-occupies. Kept because the 8-bit gains are larger and the loss is small, but recorded as a known regression rather than claimed as a win. |
| 8-bit `gray` output from swscale instead of `gray16le >> 8` | Measured on real 8-bit H.264: about 6% of luma bytes differ, so the detector input would change. It is faster but not exact. The in-process resampler does not need it: it writes the 8-bit plane by reducing its own 16-bit result, so the 8-bit output is exact and the 16-bit plane is never materialised. A separate approximate path was not added. |
| Hardware decoding | Changes the pixel pipeline and memory use; not measured as a throughput win for this tool, and it is not portable across machines. |
| Exact wavefront parallelism | No measured benefit established; profile the exact sequential implementation before considering more concurrency. |
| Parallel approximate detector scheduling | Outside the correction scope; exact WWXD decisions and recorded state must be preserved. |

## Reporting rules

- Do not claim a speedup from CPU utilization alone.
- Do not add overlapping stage times as if they were sequential.
- Do not compare benchmark results from different inputs, hardware, thread
  settings, or build profiles.
- Record known failures and regressions alongside improvements.
