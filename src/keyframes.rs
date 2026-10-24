//! Aegisub keyframe output.
//!
//! The output layer turns one completed [`crate::DetectSummary`] into the file
//! format Aegisub imports: a `# keyframe format v1` line, an `fps` metadata
//! line, then the zero-based frame indexes of the keyframes in display order.
//! The detector never reports frame 0 as a boundary, so a non-empty video gets
//! its `0` added here, once, and nothing else about the cut list changes.
//!
//! The `fps` line is format metadata only: it renders the container's declared
//! rate as a decimal and never takes part in numbering the frames. A variable
//! frame rate input keeps its true decoded frame indexes, and a stream that
//! reports no usable rate is written as `fps 0.000000` — unknown, not zero.
//!
//! Publishing is all-or-nothing. The document is written to a temporary file
//! in the output directory, flushed, and only then moved to the requested path
//! in a way that never replaces an existing file. A failed run therefore
//! publishes no partial keyframe file and leaves the old output and the input
//! untouched; the temporary is removed, and a removal that itself fails is
//! reported with the leftover path.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::{NamedTempFile, TempPath};

use crate::DetectSummary;
use crate::error::AppError;

/// First line of an Aegisub keyframe document.
const HEADER: &str = "# keyframe format v1";

/// Suffix appended to the input file name to get the default output path.
const OUTPUT_SUFFIX: &str = ".kf.txt";

/// The default keyframe path for one input: the full file name with
/// [`OUTPUT_SUFFIX`] appended, so `video.mkv` becomes `video.mkv.kf.txt`.
///
/// The extension is never replaced: a name without one still gets a usable
/// suffix, and the directory part is kept.
pub fn default_output_path(input: &Path) -> PathBuf {
    let mut path = OsString::from(input);
    path.push(OUTPUT_SUFFIX);
    PathBuf::from(path)
}

/// Publishes the keyframe document of one completed run at `path`.
///
/// The run must have decoded at least one frame: an empty video has no
/// keyframe list, so the call fails with [`AppError::EmptyVideo`] and creates
/// nothing at all. Otherwise the document goes into a temporary file in the
/// same directory as `path`, is flushed, and is then published without
/// clobbering: an existing file, including the case where the caller pointed
/// the output at the input, keeps its bytes. Any create, write, flush, or
/// publish failure is returned as [`AppError::OutputWrite`] naming the
/// requested path, the failed operation, and the I/O reason, and the temporary
/// file is removed; if even that removal fails, the leftover path is part of
/// the reported reason.
pub fn write_kf_file(path: &Path, summary: &DetectSummary) -> Result<(), AppError> {
    if summary.stats.frames == 0 {
        return Err(AppError::EmptyVideo {
            path: path.to_path_buf(),
        });
    }

    let directory = output_directory(path);
    let temp = NamedTempFile::new_in(directory).map_err(|source| AppError::OutputWrite {
        operation: "create a temporary file beside the output",
        path: path.to_path_buf(),
        source,
    })?;
    // The handle and the owning path are separated so the handle can be closed
    // before the publish: on Windows a temporary file is opened without share
    // access, and renaming a file with an open handle fails.
    let (mut handle, temp_path) = temp.into_parts();
    if let Err(source) = write_document(&mut handle, summary) {
        drop(handle);
        return Err(discard(temp_path, path, "write the keyframe file", source));
    }
    drop(handle);

    match temp_path.persist_noclobber(path) {
        Ok(()) => Ok(()),
        Err(persist) => {
            // The system wording of a refusal is localized, so the stable
            // phrase replaces it while the error kind is kept.
            let source = if persist.error.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the output file already exists",
                )
            } else {
                persist.error
            };
            Err(discard(
                persist.path,
                path,
                "publish the keyframe file",
                source,
            ))
        }
    }
}

/// Writes one keyframe document into `sink` and flushes it.
///
/// Frame 0 is written exactly once, before the boundaries, which the detector
/// guarantees are strictly increasing and never zero.
fn write_document<W: Write>(sink: &mut W, summary: &DetectSummary) -> io::Result<()> {
    let rate = summary
        .decode
        .source
        .average_frame_rate
        .or(summary.decode.source.frame_rate);
    // An unknown rate renders as 0.000000. The division of two u32 values
    // cannot overflow an f64.
    let fps = rate.map_or(0.0, |rate| {
        f64::from(rate.numerator()) / f64::from(rate.denominator())
    });
    writeln!(sink, "{HEADER}")?;
    writeln!(sink, "fps {fps:.6}")?;
    writeln!(sink, "0")?;
    for boundary in &summary.boundaries {
        writeln!(sink, "{}", boundary.get())?;
    }
    sink.flush()
}

/// The directory that holds the output, `.` when the path is bare.
fn output_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Removes the temporary after a failed write or publish and builds the error.
///
/// A removal that fails leaves a knowable path, so the leftover is reported
/// instead of silently discarded.
fn discard(temp: TempPath, path: &Path, operation: &'static str, source: io::Error) -> AppError {
    let leftover = temp.to_path_buf();
    let reason = match temp.close() {
        Ok(()) => source,
        // The file is already gone: there is nothing to report as leftover.
        Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => source,
        Err(cleanup) => io::Error::new(
            source.kind(),
            format!(
                "{source}; the temporary file could not be removed ({cleanup}) and is left at '{}'",
                leftover.display()
            ),
        ),
    };
    AppError::OutputWrite {
        operation,
        path: path.to_path_buf(),
        source: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::{default_output_path, write_document};
    use crate::decode::FrameRate;
    use crate::decode::analysis::AnalysisSize;
    use crate::decode::probe::{LumaDepth, SourceInfo};
    use crate::frame_index::{BoundaryFrameIndex, SourceFrameIndex};
    use crate::{DetectSummary, RunStats};
    use std::cell::Cell;
    use std::io::{self, Write};
    use std::path::Path;

    fn summary(frames: u64, boundaries: &[u64], average: Option<(u32, u32)>) -> DetectSummary {
        let kept = boundaries
            .iter()
            .map(|value| {
                BoundaryFrameIndex::new(SourceFrameIndex::new(*value))
                    .expect("test boundaries are above frame zero")
            })
            .collect();
        DetectSummary {
            decode: crate::decode::DecodeSummary {
                source: SourceInfo {
                    stream_index: 0,
                    width: 96,
                    height: 64,
                    pixel_format: "yuv420p".to_owned(),
                    depth: LumaDepth::Eight,
                    frame_rate: None,
                    average_frame_rate: average
                        .map(|(n, d)| FrameRate::new(n, d).expect("test rates are positive")),
                    start_time_seconds: None,
                },
                analysis_size: AnalysisSize::new(96, 64).expect("96x64 is inside the bounds"),
                decoded_frame_count: frames,
            },
            boundaries: kept,
            stats: RunStats {
                frames,
                ..RunStats::default()
            },
        }
    }

    /// Accepts a fixed byte budget, then fails every further write.
    struct BudgetWriter {
        budget: usize,
        written: Cell<usize>,
    }

    impl Write for BudgetWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let used = self.written.get();
            if used + buffer.len() > self.budget {
                return Err(io::Error::other("injected mid-write failure"));
            }
            self.written.set(used + buffer.len());
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Accepts every write and fails at flush.
    struct FlushFailWriter {
        received: usize,
    }

    impl Write for FlushFailWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.received += buffer.len();
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected flush failure"))
        }
    }

    #[test]
    fn document_is_headers_frame_zero_then_boundaries() {
        let mut sink: Vec<u8> = Vec::new();
        write_document(
            &mut sink,
            &summary(1000, &[1, 42, 125], Some((24000, 1001))),
        )
        .expect("a growing Vec never fails to write");
        assert_eq!(
            sink, b"# keyframe format v1\nfps 23.976024\n0\n1\n42\n125\n",
            "two header lines, frame 0 written once, then the cuts"
        );
    }

    #[test]
    fn nonempty_without_a_cut_writes_frame_zero() {
        let mut sink: Vec<u8> = Vec::new();
        write_document(&mut sink, &summary(12, &[], Some((24000, 1001))))
            .expect("a growing Vec never fails to write");
        assert_eq!(sink, b"# keyframe format v1\nfps 23.976024\n0\n");
    }

    #[test]
    fn mid_write_failure_is_reported() {
        let mut sink = BudgetWriter {
            budget: 30,
            written: Cell::new(0),
        };
        let error = write_document(&mut sink, &summary(9, &[1, 42, 125], Some((24000, 1001))))
            .expect_err("a sink that fails after 30 bytes must fail the document");
        assert!(error.to_string().contains("injected"), "{error}");
        assert!(
            sink.written.get() >= 21,
            "the failure lands inside the document, not before it: {}",
            sink.written.get()
        );
    }

    #[test]
    fn flush_failure_is_reported() {
        let mut sink = FlushFailWriter { received: 0 };
        let error = write_document(&mut sink, &summary(9, &[1], Some((24000, 1001))))
            .expect_err("the document writer must flush the sink and report the failure");
        assert!(
            error.to_string().contains("injected flush failure"),
            "{error}"
        );
        assert!(sink.received > 0, "the body was written first");
    }

    #[test]
    fn fps_line_follows_the_source_rates() {
        for (label, average, expected) in [
            (
                "24000/1001 renders six decimals",
                Some((24000u32, 1001u32)),
                "23.976024",
            ),
            ("an absent rate is an unknown rate", None, "0.000000"),
            (
                "an integer rate keeps its decimals",
                Some((12u32, 1u32)),
                "12.000000",
            ),
        ] {
            let mut sink: Vec<u8> = Vec::new();
            write_document(&mut sink, &summary(5, &[1], average))
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            let document =
                String::from_utf8(sink).unwrap_or_else(|error| panic!("{label}: {error}"));
            let fps_line = document
                .lines()
                .nth(1)
                .unwrap_or_else(|| panic!("{label}: the document has no second line"));
            assert_eq!(fps_line, format!("fps {expected}"), "{label}");
        }
    }

    #[test]
    fn default_path_appends_the_suffix_to_the_whole_name() {
        for (input, expected) in [
            ("video.mkv", "video.mkv.kf.txt"),
            ("dir/clip.mp4", "dir/clip.mp4.kf.txt"),
            ("dir/no-extension", "dir/no-extension.kf.txt"),
            ("dir/twice.mkv.mkv", "dir/twice.mkv.mkv.kf.txt"),
        ] {
            assert_eq!(
                default_output_path(Path::new(input)).to_string_lossy(),
                expected,
                "'{input}' must take the appended suffix"
            );
        }
    }
}
