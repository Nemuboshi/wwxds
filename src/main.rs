use std::io::IsTerminal;
use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;

mod cli;

/// Exit code for a runtime failure (file, probe, decode, detection, output).
const EXIT_RUNTIME: u8 = 1;

fn main() -> ExitCode {
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap reports 0 for help/version and 2 for a usage error.
            let exit_code = error.exit_code();
            if exit_code == 0 {
                print!("{error}");
                return ExitCode::SUCCESS;
            }
            eprint!("{error}");
            return ExitCode::from(u8::try_from(exit_code).unwrap_or(2));
        }
    };

    if let Err(message) = cli.validate() {
        eprintln!("{message}");
        return ExitCode::from(2);
    }

    let output = cli.output_path();
    let options = ihni::RunOptions {
        stream_index: cli.stream_index,
        analysis_height: cli.analysis_height,
        decoder_threads: cli.decoder_threads,
        ..ihni::RunOptions::default()
    };

    // Progress goes to standard error on the same sink as the run's messages.
    // An interactive terminal gets in-place carriage-return redraws; a pipe
    // or file gets one log line per interval. `--no-progress` keeps the run
    // silent: no renderer is built and the callback does nothing.
    let stderr = std::io::stderr();
    let target = if stderr.is_terminal() {
        ihni::progress::ProgressTarget::Terminal
    } else {
        ihni::progress::ProgressTarget::Redirected
    };
    let mut renderer = if cli.no_progress {
        None
    } else {
        Some(ihni::progress::ProgressRenderer::new(&stderr, target))
    };

    // The keyframe file is written only after a complete decode and detection
    // run, so a failed run never publishes a partial result. Standard output
    // stays empty: every message about the run goes to standard error.
    let result = ihni::detect_video_with_progress(&cli.input, options, |progress| {
        if let Some(renderer) = &mut renderer {
            renderer.on_progress(progress);
        }
    });
    // End the progress line before the run's own message: a terminal line is
    // erased, and a redirected stream already ends on a line boundary. This
    // happens on success and on failure alike, so a failure never shows a
    // leftover progress line in place of its error.
    if let Some(renderer) = &mut renderer {
        renderer.finish();
    }
    let summary = match result {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(EXIT_RUNTIME);
        }
    };

    if let Err(error) = ihni::keyframes::write_kf_file(&output, &summary) {
        eprintln!("{error}");
        return ExitCode::from(EXIT_RUNTIME);
    }

    if cli.stats {
        let stats = &summary.stats;
        let fps = match stats.frames_per_second() {
            Some(fps) => format!("{fps:.3}"),
            None => "n/a".to_owned(),
        };
        eprintln!(
            "stats frames={} producer_decode_s={:.6} producer_resize_s={:.6} queue_wait_s={:.6} prepare_s={:.6} detect_s={:.6} total_s={:.6} fps={} analysis={}x{} mode=exact read_ahead={}",
            stats.frames,
            stats.producer_decode.as_secs_f64(),
            stats.producer_resize.as_secs_f64(),
            stats.queue_wait.as_secs_f64(),
            stats.prepare.as_secs_f64(),
            stats.detect.as_secs_f64(),
            stats.total.as_secs_f64(),
            fps,
            summary.decode.analysis_size.width(),
            summary.decode.analysis_size.height(),
            stats.read_ahead,
        );
    }

    // The success message is the last thing the run prints, after the output is
    // published and the optional statistics are written.
    if let Err(error) = writeln!(std::io::stderr(), "wrote {}", output.display()) {
        eprintln!("failed to report the result: {error}");
        return ExitCode::from(EXIT_RUNTIME);
    }

    ExitCode::SUCCESS
}
