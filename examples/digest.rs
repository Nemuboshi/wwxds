//! Per-frame plane digests: the regression gate for decoder, scaler, and
//! pipeline changes.
//!
//! For each input the tool prints a header, one line per frame in display order,
//! and a footer:
//!
//! ```text
//! # input=cut8.mkv stream=0 source=96x64 pix_fmt=yuv420p analysis=96x64
//! 0 0123456789abcdef
//! 1 fedcba9876543210
//! # end frames=12
//! ```
//!
//! The digest covers exactly `width * height` bytes of the analysis plane — the
//! first `width` bytes of every row — so row padding never enters the hash. The
//! header identifies the input and the analysis size, and the footer carries the
//! frame count, so two runs can be compared with `diff` and a truncated run
//! cannot look like a valid one.
//!
//! `--native` sets the analysis height to the source height. That is not a
//! special decoder mode: swscale then takes its unscaled copy branch, which
//! promotes each sample and reduces it again (`v * 257 >> 8 == v` for 8-bit,
//! `((v << 6) | (v >> 4)) >> 8` for 10-bit), so the plane is the decoded luma
//! and the digest isolates decoding from the filtered path.
//!
//! Run with a release build:
//!
//! ```text
//! cargo run --release --example digest -- <INPUT>... [--analysis-height H]
//!     [--native] [--stream-index N]
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wwxds::decode::{FrameStream, ReaderOptions};

/// FNV-1a 64, the same construction the detector uses for its state digests.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct Args {
    inputs: Vec<PathBuf>,
    analysis_height: Option<u32>,
    native: bool,
    stream_index: Option<u32>,
}

fn parse_args() -> Result<Args, String> {
    let mut inputs = Vec::new();
    let mut analysis_height = None;
    let mut native = false;
    let mut stream_index = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--analysis-height" => {
                let value = args.next().ok_or("--analysis-height needs a value")?;
                let height = value
                    .parse()
                    .map_err(|_| format!("invalid --analysis-height '{value}'"))?;
                analysis_height = Some(height);
            }
            "--native" => native = true,
            "--stream-index" => {
                let value = args.next().ok_or("--stream-index needs a value")?;
                stream_index = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid --stream-index '{value}'"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            path => inputs.push(PathBuf::from(path)),
        }
    }

    if inputs.is_empty() {
        return Err("at least one input path is required".to_owned());
    }
    if native && analysis_height.is_some() {
        return Err("--native and --analysis-height are mutually exclusive".to_owned());
    }

    Ok(Args {
        inputs,
        analysis_height,
        native,
        stream_index,
    })
}

/// FNV-1a over the first `width` bytes of every row.
fn plane_digest(plane: &[u8], width: usize, height: usize) -> Result<u64, String> {
    if height == 0 || width == 0 {
        return Err("a plane needs a nonzero width and height".to_owned());
    }
    let needed = width
        .checked_mul(height)
        .ok_or("the plane dimensions overflow")?;
    if plane.len() < needed {
        return Err(format!(
            "the plane holds {} bytes, which is less than the {} bytes of {}x{} rows",
            plane.len(),
            needed,
            width,
            height
        ));
    }
    let stride = plane.len() / height;

    let mut hash = FNV_OFFSET_BASIS;
    for row in 0..height {
        let start = row * stride;
        for byte in &plane[start..start + width] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(hash)
}

fn digest_input(path: &Path, args: &Args) -> Result<(), String> {
    // `--native` needs the source height before the reader is opened. The probe
    // is one extra pass over the container header, not over the frames.
    let analysis_height = if args.native {
        Some(
            wwxds::decode::probe::probe(path, args.stream_index)
                .map_err(|error| error.to_string())?
                .height,
        )
    } else {
        args.analysis_height
    };

    let options = ReaderOptions {
        stream_index: args.stream_index,
        analysis_height,
        ..ReaderOptions::default()
    };
    let mut stream = FrameStream::open(path, options).map_err(|error| error.to_string())?;

    let source = stream.source().clone();
    let analysis = stream.analysis_size();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    println!(
        "# input={} stream={} source={}x{} pix_fmt={} analysis={}x{}",
        name,
        source.stream_index,
        source.width,
        source.height,
        source.pixel_format,
        analysis.width(),
        analysis.height(),
    );

    let width = analysis.width() as usize;
    let height = analysis.height() as usize;
    let mut count = 0u64;
    while let Some(frame) = stream.next_frame().map_err(|error| error.to_string())? {
        let digest = plane_digest(frame.luma(), width, height)?;
        println!("{} {:016x}", frame.index().get(), digest);
        count += 1;
        stream.recycle(frame);
    }

    println!("# end frames={count}");
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!(
                "usage: digest <INPUT>... [--analysis-height H] [--native] \
                 [--stream-index N]"
            );
            return ExitCode::from(2);
        }
    };

    for input in &args.inputs {
        if let Err(message) = digest_input(input, &args) {
            eprintln!("error: {}: {message}", input.display());
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}
