//! Periodic progress display for one detection run.
//!
//! The library reports progress as one [`Progress`] value per completed frame;
//! [`crate::detect_video_with_progress`] owns the numbers and this module owns
//! how they are shown. There is no event framework: the run calls one closure
//! and the CLI hands each value to a [`ProgressRenderer`].
//!
//! The renderer throttles on the progress value's own `elapsed` field rather
//! than on a private clock. The library fills `elapsed` from the run's real
//! start `Instant`, and a test can drive the throttle with fabricated values,
//! so no test ever sleeps.
//!
//! A line shows frames, keyframes, elapsed time, and the average processing
//! FPS. It never shows a percentage or an ETA: the run does not know the total
//! frame count before it ends, and no part of the pipeline guesses one.

use std::io::Write;
use std::time::Duration;

/// What one running detection has completed so far.
///
/// The values are reported after each frame's detection completes, strictly as
/// the run reaches them. `keyframes` counts the frame-0 line the output layer
/// will publish, so a non-empty run never reports zero keyframes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    /// Frames whose detection has completed. The first callback reports `1`.
    pub processed_frames: u64,
    /// Keyframes the run has so far: every boundary decided up to now, plus
    /// frame 0, which the output layer writes for every non-empty video.
    pub keyframes: u64,
    /// Wall time since the run started. The renderer's throttle reads this
    /// instead of its own clock, which is what lets tests pin the throttle
    /// without sleeping.
    pub elapsed: Duration,
}

/// Where the progress display goes. The caller decides it from
/// [`std::io::IsTerminal`] on the sink, so the renderer itself never probes a
/// terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressTarget {
    /// An interactive terminal: in-place carriage-return redraws at most every
    /// [`TERMINAL_REFRESH_INTERVAL`].
    Terminal,
    /// A pipe or a file: one newline-terminated log line at most every
    /// [`REDIRECTED_LINE_INTERVAL`].
    Redirected,
}

/// In-place refresh period for a terminal.
///
/// 200 ms is frequent enough to feel live and far slower than frame arrivals
/// on any input, so the console is never buried by redraws.
const TERMINAL_REFRESH_INTERVAL: Duration = Duration::from_millis(200);

/// Period of one log line for redirected output.
///
/// A redirected stream is a file someone reads later. One line every 2
/// seconds keeps a long run in the tens of lines per minute without dropping
/// information between them.
const REDIRECTED_LINE_INTERVAL: Duration = Duration::from_secs(2);

/// Renders the periodic progress line into a `Write` sink.
///
/// The sink is generic so a test can inject a failing writer and pin the
/// throttle with fabricated [`Progress`] values. The first callback renders
/// immediately; afterwards a value renders only once its `elapsed` is at least
/// one interval past the last render. A write failure marks the renderer
/// broken: every later callback and the final `finish` become no-ops.
/// Rendering is display only, so its failure never changes the run's result —
/// the pipeline reports the run's own success or error regardless.
pub struct ProgressRenderer<W> {
    sink: W,
    target: ProgressTarget,
    /// `elapsed` of the last value actually rendered, `None` before the first.
    last_rendered: Option<Duration>,
    /// Byte width of the last rendered line, so a terminal redraw can erase
    /// the tail of a longer previous line.
    last_width: usize,
    /// Set on the first sink failure; suppresses all further rendering.
    broken: bool,
}

impl<W: Write> ProgressRenderer<W> {
    /// A renderer that writes periodic progress into `sink` for `target`.
    pub fn new(sink: W, target: ProgressTarget) -> Self {
        Self {
            sink,
            target,
            last_rendered: None,
            last_width: 0,
            broken: false,
        }
    }

    /// Report one progress value, rendering it only when the throttle allows.
    pub fn on_progress(&mut self, progress: Progress) {
        if self.broken {
            return;
        }
        let interval = match self.target {
            ProgressTarget::Terminal => TERMINAL_REFRESH_INTERVAL,
            ProgressTarget::Redirected => REDIRECTED_LINE_INTERVAL,
        };
        if let Some(last) = self.last_rendered {
            // Saturating keeps a misordered value harmless instead of panicking.
            if progress.elapsed.saturating_sub(last) < interval {
                return;
            }
        }

        let line = render_line(progress);
        let chunk = match self.target {
            ProgressTarget::Terminal => {
                // Return to column 0, overwrite, then pad so the tail of a
                // longer previous line disappears.
                let pad = self.last_width.saturating_sub(line.len());
                format!("\r{line}{}", " ".repeat(pad))
            }
            ProgressTarget::Redirected => format!("{line}\n"),
        };
        if self.sink.write_all(chunk.as_bytes()).is_err() {
            self.broken = true;
            return;
        }
        self.last_rendered = Some(progress.elapsed);
        self.last_width = line.len();
    }

    /// End the progress display before the run's final message.
    ///
    /// A redirected stream needs nothing: its lines already end in newlines.
    /// A terminal line is erased so the completion or error message starts on
    /// a clean line. A renderer that never rendered writes nothing.
    pub fn finish(&mut self) {
        if self.broken {
            return;
        }
        if self.target == ProgressTarget::Terminal && self.last_rendered.is_some() {
            // Blank exactly the columns the last line occupied, then return
            // the cursor to column 0 so the final message starts clean.
            let erase = format!("\r{}\r", " ".repeat(self.last_width));
            if self.sink.write_all(erase.as_bytes()).is_err() {
                self.broken = true;
            }
        }
    }
}

/// The progress line: frames, keyframes, elapsed, average processing FPS.
///
/// The average FPS is frames over elapsed, the same reduction
/// [`crate::RunStats::frames_per_second`] documents: a zero elapsed is `n/a`,
/// never a fabricated rate.
fn render_line(progress: Progress) -> String {
    let seconds = progress.elapsed.as_secs_f64();
    let fps = if seconds > 0.0 {
        format!("{:.2}", progress.processed_frames as f64 / seconds)
    } else {
        "n/a".to_owned()
    };
    format!(
        "progress frames={} keyframes={} elapsed={seconds:.2}s fps={fps}",
        progress.processed_frames, progress.keyframes,
    )
}

#[cfg(test)]
mod tests {
    use super::{Progress, ProgressRenderer, ProgressTarget};
    use std::io::{self, Write};
    use std::time::Duration;

    fn progress(frames: u64, keyframes: u64, elapsed_ms: u64) -> Progress {
        Progress {
            processed_frames: frames,
            keyframes,
            elapsed: Duration::from_millis(elapsed_ms),
        }
    }

    /// Accepts the first `succeed` writes, then fails every further write and
    /// counts every attempt, so a test can tell "stopped" from "kept trying".
    struct FailAfterN {
        succeed: usize,
        attempts: usize,
        accepted: usize,
    }

    impl Write for FailAfterN {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.attempts += 1;
            if self.succeed == 0 {
                return Err(io::Error::other("injected stderr failure"));
            }
            self.succeed -= 1;
            self.accepted += buffer.len();
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn tty_throttles_at_200ms() {
        let mut sink: Vec<u8> = Vec::new();
        let mut renderer = ProgressRenderer::new(&mut sink, ProgressTarget::Terminal);
        // Frame arrivals far faster than the refresh period.
        renderer.on_progress(progress(1, 1, 0));
        renderer.on_progress(progress(2, 1, 50));
        renderer.on_progress(progress(3, 1, 199));
        renderer.on_progress(progress(4, 2, 200));
        renderer.on_progress(progress(5, 2, 399));
        renderer.on_progress(progress(6, 2, 400));
        renderer.finish();
        let text = String::from_utf8(sink).expect("the renderer writes UTF-8");
        let refreshes = text.matches("progress frames=").count();
        assert_eq!(
            refreshes, 3,
            "one refresh at 0 ms, 200 ms, 400 ms, none in between: {text:?}"
        );
        assert!(text.contains("frames=1 "), "the first refresh: {text:?}");
        assert!(text.contains("frames=4 "), "the 200 ms refresh: {text:?}");
        assert!(
            text.contains("frames=6 "),
            "the last refresh carries the newest values: {text:?}"
        );
        for gone in [
            "progress frames=2 ",
            "progress frames=3 ",
            "progress frames=5 ",
        ] {
            assert!(
                !text.contains(gone),
                "sub-200ms arrivals must not refresh: {text:?}"
            );
        }
        assert!(
            !text.contains('\n'),
            "a terminal update redraws in place, it never emits log lines: {text:?}"
        );
        assert!(
            text.starts_with('\r') && text.ends_with('\r'),
            "every update returns to column 0 and finish erases the line: {text:?}"
        );
    }

    #[test]
    fn redirected_output_throttles_at_2s() {
        let mut sink: Vec<u8> = Vec::new();
        let mut renderer = ProgressRenderer::new(&mut sink, ProgressTarget::Redirected);
        renderer.on_progress(progress(1, 1, 0));
        renderer.on_progress(progress(2, 1, 1_999));
        renderer.on_progress(progress(3, 2, 2_000));
        renderer.on_progress(progress(4, 2, 3_999));
        renderer.on_progress(progress(5, 2, 4_000));
        renderer.on_progress(progress(6, 2, 4_100));
        renderer.finish();
        let text = String::from_utf8(sink).expect("the renderer writes UTF-8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "one line at 0 s, 2 s, 4 s, none in between: {text:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with("progress frames=")),
            "{text:?}"
        );
        assert!(
            text.ends_with('\n'),
            "every redirected line is newline-terminated so the log stays readable"
        );
        for gone in [
            "progress frames=2 ",
            "progress frames=4 ",
            "progress frames=6 ",
        ] {
            assert!(
                !text.contains(gone),
                "sub-2s arrivals must not print: {text:?}"
            );
        }
    }

    #[test]
    fn unknown_total_has_no_percent() {
        for target in [ProgressTarget::Terminal, ProgressTarget::Redirected] {
            let mut sink: Vec<u8> = Vec::new();
            ProgressRenderer::new(&mut sink, target).on_progress(progress(100, 3, 5_000));
            let text = String::from_utf8(sink).expect("the renderer writes UTF-8");
            let lower = text.to_lowercase();
            assert!(text.contains("frames=100"), "frame count: {text:?}");
            assert!(text.contains("keyframes=3"), "keyframe count: {text:?}");
            assert!(text.contains("elapsed=5.00"), "elapsed time: {text:?}");
            assert!(
                text.contains("fps=20.00"),
                "average processing FPS: {text:?}"
            );
            assert!(
                !lower.contains('%'),
                "no percentage without a total: {text:?}"
            );
            assert!(!lower.contains("eta"), "no ETA without a total: {text:?}");
            assert!(!lower.contains("total"), "no fabricated total: {text:?}");
        }
    }

    #[test]
    fn progress_write_failure_is_nonfatal() {
        // The redirected line at 0 ms succeeds, the line at 2 s hits the
        // injected failure, and everything after must stop touching the sink.
        let mut sink = FailAfterN {
            succeed: 1,
            attempts: 0,
            accepted: 0,
        };
        let mut renderer = ProgressRenderer::new(&mut sink, ProgressTarget::Redirected);
        renderer.on_progress(progress(1, 1, 0));
        renderer.on_progress(progress(2, 1, 2_000));
        renderer.on_progress(progress(3, 1, 4_000));
        renderer.on_progress(progress(4, 2, 6_000));
        renderer.finish();
        assert_eq!(
            sink.attempts, 2,
            "after a failed write the renderer must stop rendering entirely"
        );
        assert!(
            sink.accepted > 0,
            "the line before the failure must have been written"
        );
    }
}
