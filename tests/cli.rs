use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run<I, S>(arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_ihni"))
        .args(arguments)
        .output()
        .expect("the CLI binary starts")
}

/// A generated fixture, or `None` (with a reported skip) when the decode fixture
/// generator has not run.
fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/m1-fixtures")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!(
            "skipping: fixture '{}' is absent; run bash scripts/make-m1-fixtures.sh",
            path.display()
        );
        None
    }
}

/// Copies a fixture into a temporary directory so a run cannot leave output
/// beside the shared fixture tree.
fn staged(name: &str) -> Option<(tempfile::TempDir, PathBuf)> {
    let source = fixture(name)?;
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join(name);
    std::fs::copy(&source, &input)
        .unwrap_or_else(|error| panic!("copying '{}' failed: {error}", source.display()));
    Some((temporary, input))
}

/// The byte content every CLI file-output test expects for `cut8.mkv`: two
/// header lines, frame 0 from the file layer, then the cut at frame 6.
const CUT8_DOCUMENT: &[u8] = b"# keyframe format v1\nfps 12.000000\n0\n6\n";

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path)
        .unwrap_or_else(|error| panic!("reading '{}' failed: {error}", path.display()))
}

/// The documented default output path: the full input file name plus `.kf.txt`.
fn kf_path(input: &Path) -> PathBuf {
    PathBuf::from(format!("{}.kf.txt", input.display()))
}

#[test]
fn help_and_version_succeed() {
    let help = run(["--help"]);
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("Usage:"));
    assert!(
        help_text.contains("--no-progress"),
        "the help lists the progress option: {help_text}"
    );

    let version = run(["--version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("ihni 0.1.0"));
}

#[test]
fn invalid_argument_shapes_exit_two() {
    for arguments in [
        Vec::<&str>::new(),
        vec!["input.mp4", "extra.mp4"],
        vec!["--unknown", "input.mp4"],
        vec!["--stream-index", "-1", "input.mp4"],
        vec!["--stream-index", "abc", "input.mp4"],
        vec!["input.mp4", "--output"],
    ] {
        let output = run(arguments);
        assert_eq!(
            output.status.code(),
            Some(2),
            "a usage error must exit 2, not 1"
        );
    }
}

#[test]
fn the_removed_options_are_rejected_as_usage_errors() {
    // The stdout JSON/text interface and the time-declaration options are gone;
    // a caller still passing them gets a clean clap rejection, exit 2.
    for (arguments, option) in [
        (vec!["--format", "json", "input.mp4"], "--format"),
        (vec!["--threads", "2", "input.mp4"], "--threads"),
        (vec!["--derived-cfr", "12/1", "input.mp4"], "--derived-cfr"),
        (vec!["--source-time", "input.mp4"], "--source-time"),
    ] {
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(option) && stderr.contains("unexpected argument"),
            "'{option}' must be rejected as unexpected: {stderr}"
        );
    }
}

#[test]
fn unsupported_configuration_exits_two_before_decoding() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("input.mp4");
    std::fs::write(&input, b"fixture bytes").expect("temporary input file is written");
    let input = input.to_str().expect("utf-8 path");

    for arguments in [
        vec!["--analysis-height", "16", input],
        vec!["--analysis-height", "9000", input],
        vec!["--decoder-threads", "0", input],
    ] {
        let output = run(arguments.clone());
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?} must be rejected as a configuration error"
        );
    }
}

#[test]
fn an_output_equal_to_the_input_is_rejected_before_decoding() {
    // `-o` pointing at the input itself would destroy the video. The run is
    // refused as an argument error before anything is decoded, and the input
    // bytes provably survive.
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("input.mp4");
    std::fs::write(&input, b"video bytes that must survive").expect("input is written");
    let input_text = input.to_str().expect("utf-8 path");

    let output = run(["--output".as_ref(), input_text.as_ref(), input.as_os_str()]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "an output equal to the input is an argument error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("output"), "{stderr}");
    assert_eq!(
        read(&input),
        b"video bytes that must survive",
        "the refusal must not touch the input"
    );
}

#[test]
fn missing_and_directory_paths_fail_at_run_time() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let missing = temporary.path().join("missing input.mp4");
    let missing_output = run([missing.as_os_str()]);
    assert_eq!(missing_output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("open input file"));

    let directory_output = run([temporary.path().as_os_str()]);
    assert_eq!(directory_output.status.code(), Some(1));
    let leftovers: Vec<_> = std::fs::read_dir(temporary.path())
        .expect("the directory is readable")
        .collect();
    assert!(leftovers.is_empty(), "a failed run creates no output");
}

#[test]
fn readable_non_video_path_fails_with_a_probe_message() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let input = temporary.path().join("readable input with spaces.mp4");
    std::fs::write(&input, b"fixture bytes").expect("temporary input file is written");

    let output = Command::new(env!("CARGO_BIN_EXE_ihni"))
        .args(["--stream-index", "3"])
        .arg(input.as_os_str())
        .output()
        .expect("the CLI binary starts");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The path is probed before a stream is selected, so a non-media file is
    // reported as unreadable media rather than as a missing stream.
    assert!(stderr.contains("readable input with spaces.mp4"));
    assert!(output.stdout.is_empty(), "a failed run writes no result");
    assert!(!kf_path(&input).exists(), "a failed run publishes nothing");
}

#[test]
fn a_successful_run_writes_the_keyframe_file_beside_the_input() {
    let Some((temporary, input)) = staged("cut8.mkv") else {
        return;
    };
    let output = run([input.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "the result lives in the file");
    let document = kf_path(&input);
    assert_eq!(
        read(&document),
        CUT8_DOCUMENT,
        "the default name is the input name plus .kf.txt"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cut8.mkv.kf.txt"),
        "the success line names the output path: {stderr}"
    );
    let entries: Vec<_> = std::fs::read_dir(temporary.path())
        .expect("the directory is readable")
        .collect();
    assert_eq!(entries.len(), 2, "input and published output, nothing else");
}

#[test]
fn the_output_option_selects_the_destination() {
    let Some((_temporary, input)) = staged("cut8.mkv") else {
        return;
    };
    let custom = input
        .parent()
        .expect("the copy has a directory")
        .join("my keys.kf.txt");
    let output = run([input.as_os_str(), "-o".as_ref(), custom.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert_eq!(
        read(&custom),
        CUT8_DOCUMENT,
        "the selected path receives the document"
    );
    assert!(
        !kf_path(&input).exists(),
        "a chosen output suppresses the default one"
    );
}

#[test]
fn a_spaced_input_path_writes_a_spaced_output() {
    let Some((temporary, _)) = staged("cut8.mkv") else {
        return;
    };
    let input = temporary.path().join("my clip.mkv");
    std::fs::copy(temporary.path().join("cut8.mkv"), &input).expect("the fixture is renamed");
    std::fs::remove_file(temporary.path().join("cut8.mkv")).expect("the original copy is removed");

    let output = run([input.as_os_str()]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        read(&temporary.path().join("my clip.mkv.kf.txt")),
        CUT8_DOCUMENT
    );
}

#[test]
fn an_existing_output_is_not_overwritten() {
    let Some((_temporary, input)) = staged("cut8.mkv") else {
        return;
    };
    let document = kf_path(&input);
    std::fs::write(&document, b"hand-written keyframes\n").expect("the existing output is written");

    let output = run([input.as_os_str()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "refusing to overwrite is a runtime error, not a usage error"
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert_eq!(
        read(&document),
        b"hand-written keyframes\n",
        "the existing file keeps its bytes"
    );
}

#[test]
fn stats_go_to_standard_error_not_standard_output() {
    let Some((_temporary, input)) = staged("long.mkv") else {
        return;
    };
    let output = run(["--stats".as_ref(), input.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "stdout stays empty");
    assert_eq!(
        read(&kf_path(&input)),
        b"# keyframe format v1\nfps 12.000000\n0\n18\n",
        "the cut at frame 18 is unchanged by the file layer"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for key in [
        "stats frames=36",
        "producer_decode_s=",
        "producer_resize_s=",
        "queue_wait_s=",
        "prepare_s=",
        "detect_s=",
        "total_s=",
    ] {
        assert!(
            stderr.contains(key),
            "stats line is missing '{key}': {stderr}"
        );
    }
    let completion = stderr
        .lines()
        .last()
        .expect("stderr has at least the completion line");
    assert!(
        completion.contains("long.mkv.kf.txt"),
        "the success message is the last thing printed: {stderr}"
    );
}

#[test]
fn a_truncated_container_exits_one_and_writes_no_file() {
    let Some((temporary, input)) = staged("truncated.mkv") else {
        return;
    };
    let output = run([input.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not complete"));
    assert!(
        !kf_path(&input).exists(),
        "an incomplete run publishes nothing"
    );
    let entries: Vec<_> = std::fs::read_dir(temporary.path())
        .expect("the directory is readable")
        .collect();
    assert_eq!(entries.len(), 1, "only the staged input remains");
}

#[test]
fn an_analysis_height_override_keeps_the_boundary() {
    let Some((_temporary, input)) = staged("cut8.mkv") else {
        return;
    };
    let output = run([
        "--analysis-height".as_ref(),
        "180".as_ref(),
        input.as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        read(&kf_path(&input)),
        CUT8_DOCUMENT,
        "the document is unchanged by the analysis height"
    );
}

#[test]
fn a_redirected_run_shows_progress_lines_before_the_completion_message() {
    // `Command::output` gives the child a pipe, so the CLI takes its
    // redirected path: newline log lines, readable later, ending with the
    // completion message.
    let Some((_temporary, input)) = staged("long.mkv") else {
        return;
    };
    let output = run([input.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "progress never goes to stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("progress frames="),
        "the run reports progress on stderr: {stderr}"
    );
    for key in ["keyframes=", "elapsed=", "fps="] {
        assert!(
            stderr.contains(key),
            "the progress line carries '{key}': {stderr}"
        );
    }
    assert!(
        !stderr.contains('%') && !stderr.to_lowercase().contains("eta"),
        "progress must not show a percentage or ETA: {stderr}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    // A note about a backend fallback is a diagnostic that may precede the
    // progress lines; the ordering claim is about progress and completion.
    let first_progress = lines
        .iter()
        .find(|line| !line.starts_with("note:"))
        .unwrap_or(&"");
    assert!(
        first_progress.starts_with("progress frames="),
        "progress precedes the completion message: {stderr}"
    );
    assert!(
        lines.last().is_some_and(|line| line.contains("wrote ")),
        "the completion message is last: {stderr}"
    );
}

#[test]
fn no_progress_keeps_the_run_silent_until_its_final_message() {
    // `--no-progress` closes only the periodic progress. The completion
    // message stays, and a failed run still reports its error.
    let Some((_temporary, input)) = staged("long.mkv") else {
        return;
    };
    let output = run(["--no-progress".as_ref(), input.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("progress"),
        "--no-progress must emit no periodic stderr output: {stderr}"
    );
    assert!(
        stderr.contains("wrote "),
        "the completion message survives --no-progress: {stderr}"
    );

    let Some((temporary, truncated)) = staged("truncated.mkv") else {
        return;
    };
    let output = run(["--no-progress".as_ref(), truncated.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("progress"),
        "a failing run under --no-progress stays silent too: {stderr}"
    );
    assert!(
        stderr.contains("did not complete"),
        "--no-progress must not swallow the error: {stderr}"
    );
    assert!(!kf_path(&truncated).exists());
    let entries: Vec<_> = std::fs::read_dir(temporary.path())
        .expect("the directory is readable")
        .collect();
    assert_eq!(entries.len(), 1, "only the staged input remains");
}

#[test]
fn a_failed_run_shows_progress_and_the_error_never_a_success_line() {
    // Progress may appear while a damaged input still completes frames, but
    // the run ends with its error and no `wrote` line.
    let Some((_temporary, input)) = staged("truncated.mkv") else {
        return;
    };
    let output = run([input.as_os_str()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not complete"),
        "the error is still the reported result: {stderr}"
    );
    assert!(
        stderr.contains("progress frames="),
        "a mid-run failure can still have shown progress: {stderr}"
    );
    assert!(
        !stderr.contains("wrote "),
        "a failed run must never show the completion path: {stderr}"
    );
}
