//! The D3D11 hardware decode and analysis-filter backend.
//!
//! In this mode the decoder runs as d3d11va and its frames stay in GPU memory
//! as NV12 or P010 textures; the analysis plane is produced on the device by
//! the compute shader in the C shim, which computes the same integer box
//! filter as [`super::resize::FrameResizer`] (`docs/Architecture.md` §4).
//! Only the tight 8-bit analysis plane — a few hundred kilobytes per frame —
//! crosses back to the host, so no full-size frame round trip exists.
//!
//! The backend covers whole-pixel downscales only. Every default analysis
//! size is one (1080p sources map 3:1); any other geometry is rejected here
//! and the caller falls back to the CPU filter. Hardware decode output is
//! byte-compared against the CPU pipeline in the integration tests, which is
//! what makes the two backends interchangeable.
//!
//! All Direct3D access lives in `csrc/ihni_ffmpeg.c`; this module holds the
//! selection mode and the filter parameters.

use crate::ffi;
use crate::luma::LumaFormat;

/// Which analysis-plane backend a run uses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GpuMode {
    /// Always use the CPU filter. No device is probed and nothing is printed.
    #[default]
    Off,
    /// Use the D3D11 backend when the machine supports it; fall back to the
    /// CPU filter with a reported reason otherwise.
    Auto,
    /// Require the D3D11 backend. A machine that cannot provide it fails the
    /// run instead of degrading silently.
    D3d11,
}

impl GpuMode {
    /// Parses the CLI spelling of the mode.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "d3d11" => Some(Self::D3d11),
            _ => None,
        }
    }

    /// The CLI spelling of the mode, for diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::D3d11 => "d3d11",
        }
    }
}

/// The GPU analysis filter for one run.
///
/// The device and the compiled shader live in the C shim's input handle; this
/// type holds the run's fixed geometry and, after the first frame, the exact
/// integer filter parameters the CPU implementation would compute.
pub(crate) struct D3d11Filter {
    destination_width: u32,
    destination_height: u32,
    /// The whole source pixels one output sample covers, per axis.
    block_w: u32,
    block_h: u32,
    /// Set by the first frame, whose luma depth the decoder reveals.
    format: Option<LumaFormat>,
    params: Option<ffi::D3d11Params>,
}

impl D3d11Filter {
    /// Plans a GPU filter for one source and analysis geometry.
    ///
    /// The GPU path only exists for whole-pixel downscales — the shape every
    /// default analysis size has — because that is where the shader and the
    /// CPU fast path compute the same exact block means. Any other geometry
    /// returns a reason the caller reports with its fallback.
    pub fn plan(
        source_width: u32,
        source_height: u32,
        destination_width: u32,
        destination_height: u32,
    ) -> Result<Self, String> {
        if destination_width == 0
            || destination_height == 0
            || source_width < destination_width
            || source_height < destination_height
            || !source_width.is_multiple_of(destination_width)
            || !source_height.is_multiple_of(destination_height)
        {
            return Err(format!(
                "the {source_width}x{source_height} source does not map to \
                 {destination_width}x{destination_height} in whole pixels"
            ));
        }
        Ok(Self {
            destination_width,
            destination_height,
            block_w: source_width / destination_width,
            block_h: source_height / destination_height,
            format: None,
            params: None,
        })
    }

    /// Filters one hardware frame into the tight 8-bit analysis plane.
    ///
    /// The first frame fixes the luma depth the parameters encode; a frame
    /// whose depth differs from that is rejected like a size change, because
    /// the compiled shader would otherwise misread its samples.
    pub fn filter(&mut self, frame: &ffi::FrameRef<'_>, target: &mut [u8]) -> Result<(), String> {
        if self.params.is_none() {
            let format = frame.luma_format().ok_or_else(|| {
                format!(
                    "the GPU filter does not support the frame's luma depth ({})",
                    frame.pixel_format_name()
                )
            })?;
            let sample_shift = match format {
                LumaFormat::Gray8 => 0,
                LumaFormat::Gray10Le | LumaFormat::Gray10Be => 6,
            };
            let out_shift = match format {
                LumaFormat::Gray8 => 0,
                LumaFormat::Gray10Le | LumaFormat::Gray10Be => 2,
            };
            let block = self.block_w * self.block_h;
            self.format = Some(format);
            self.params = Some(ffi::D3d11Params {
                dst_width: self.destination_width as i32,
                dst_height: self.destination_height as i32,
                block_w: self.block_w as i32,
                block_h: self.block_h as i32,
                half: (block / 2) as i32,
                block: block as i32,
                out_shift,
                sample_shift,
            });
        } else if self.format != Some(frame.luma_format().unwrap_or(LumaFormat::Gray8)) {
            return Err(format!(
                "the stream filtered as {} but the decoder produced a frame in {}",
                self.format.map(LumaFormat::name).unwrap_or_default(),
                frame
                    .luma_format()
                    .map(LumaFormat::name)
                    .unwrap_or("an unsupported depth"),
            ));
        }

        let pixels = self.destination_width as usize * self.destination_height as usize;
        if target.len() < pixels {
            return Err(format!(
                "the 8-bit destination needs {pixels} bytes but the slice holds {}",
                target.len()
            ));
        }
        // SAFETY (in `ffi::FrameRef::filter`): the frame borrows its input, so
        // the handle and the view are live for the call.
        frame.filter(self.params.as_ref().expect("params built above"), target)
    }
}

#[cfg(test)]
mod tests {
    use super::{D3d11Filter, GpuMode};

    #[test]
    fn gpu_modes_parse_their_cli_names() {
        assert_eq!(GpuMode::parse("off"), Some(GpuMode::Off));
        assert_eq!(GpuMode::parse("auto"), Some(GpuMode::Auto));
        assert_eq!(GpuMode::parse("d3d11"), Some(GpuMode::D3d11));
        assert_eq!(GpuMode::parse("cuda"), None);
    }

    #[test]
    fn a_1080p_source_plans_a_whole_pixel_block() {
        let filter = D3d11Filter::plan(1920, 1080, 640, 360).expect("a supported geometry");
        assert_eq!((filter.block_w, filter.block_h), (3, 3));
    }

    #[test]
    fn upsampling_and_fractional_scales_are_rejected() {
        // The m1 corpus sources upscale to the analysis height, so the GPU
        // path must refuse them rather than compute a different filter.
        assert!(D3d11Filter::plan(96, 64, 540, 360).is_err());
        // 960x432 maps to 800x360 at the default height: 1.2x, fractional.
        assert!(D3d11Filter::plan(960, 432, 800, 360).is_err());
        // Identity is a valid whole-pixel map.
        assert!(D3d11Filter::plan(640, 360, 640, 360).is_ok());
    }
}
