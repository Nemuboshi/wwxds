use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about = "Standalone WWXD hard-cut detector")]
pub(crate) struct Cli {
    /// Input video file.
    #[arg(value_name = "INPUT")]
    pub(crate) input: PathBuf,

    /// Keyframe output file. Omitted writes the input name with `.kf.txt`
    /// appended, so `video.mkv` becomes `video.mkv.kf.txt`. An existing file is
    /// never replaced.
    #[arg(short, long, value_name = "FILE")]
    pub(crate) output: Option<PathBuf>,

    /// Absolute input video stream index: the stream's index across all
    /// streams of the input, as the container numbers them.
    #[arg(long, value_name = "INDEX")]
    pub(crate) stream_index: Option<u32>,

    /// Target analysis height in pixels. The source is divided by the
    /// smallest integer factor landing within five sixths of the target,
    /// else by the factor closest to it from above, else analyzed natively.
    #[arg(long, value_name = "PIXELS")]
    pub(crate) analysis_height: Option<u32>,

    /// Decoder thread count passed to FFmpeg. Omitted lets FFmpeg choose.
    #[arg(long, value_name = "N")]
    pub(crate) decoder_threads: Option<u32>,

    /// Analysis-plane backend: `off` decodes and filters on the CPU, `auto`
    /// uses D3D11 hardware decode when the machine supports it and falls back
    /// to `off` otherwise, `d3d11` requires the D3D11 backend.
    #[arg(long, value_name = "MODE", default_value = "auto")]
    pub(crate) gpu: String,

    /// Write stage timings to standard error after a successful run.
    #[arg(long)]
    pub(crate) stats: bool,

    /// Suppress the periodic progress lines on standard error. Completion and
    /// error messages are still written.
    #[arg(long)]
    pub(crate) no_progress: bool,
}

impl Cli {
    pub(crate) fn output_path(&self) -> PathBuf {
        self.output
            .clone()
            .unwrap_or_else(|| wwxds::keyframes::default_output_path(&self.input))
    }

    /// Validates settings the argument parser cannot express.
    ///
    /// An output that names the input would ask the tool to overwrite the video
    /// it is reading, so it is rejected as an argument error before any decoding
    /// starts rather than left to the publish stage.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(height) = self.analysis_height
            && !(48..=4096).contains(&height)
        {
            return Err(format!(
                "--analysis-height {height} is outside the supported range 48..=4096"
            ));
        }
        if let Some(threads) = self.decoder_threads
            && threads == 0
        {
            return Err("--decoder-threads must be at least 1".to_owned());
        }
        if wwxds::decode::GpuMode::parse(&self.gpu).is_none() {
            return Err(format!(
                "--gpu '{}' is not one of off, auto, d3d11",
                self.gpu
            ));
        }
        let output = self.output_path();
        if output == self.input {
            return Err(format!(
                "the output path '{}' is the input file; choose a different file",
                output.display()
            ));
        }
        Ok(())
    }
}
