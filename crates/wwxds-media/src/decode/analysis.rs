//! The fixed analysis size for one run.
//!
//! The detector compares blocks at one analysis size. The size is the source
//! divided by an integer scale factor, so every analysis geometry is an exact
//! whole-pixel block map of the source — the shape the CPU filter and the
//! D3D11 shader both compute. The default target height is 360; the caller
//! can override it. The bounds are reference safety limits, not accuracy
//! parameters.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// The default target analysis height. The width follows the source through
/// the same scale factor; see [`AnalysisSize::for_source`].
pub const ANALYSIS_HEIGHT: u32 = 360;

/// Smallest accepted analysis width or height.
pub const MIN_ANALYSIS_DIMENSION: u32 = 48;

/// Largest accepted analysis width or height.
pub const MAX_ANALYSIS_DIMENSION: u32 = 4096;

/// Largest accepted analysis pixel count.
pub const MAX_ANALYSIS_PIXELS: u64 = 2_097_152;

/// The width and height passed to the decoder and the detector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnalysisSize {
    width: u32,
    height: u32,
}

impl AnalysisSize {
    /// Derives the analysis size for a source size at [`ANALYSIS_HEIGHT`].
    pub fn for_source(source_width: u32, source_height: u32) -> Result<Self, AnalysisSizeError> {
        Self::for_source_height(source_width, source_height, ANALYSIS_HEIGHT)
    }

    /// Derives the analysis size for a source size at a chosen target height.
    ///
    /// The scale factor is the smallest integer that divides both source
    /// dimensions and brings the height to `target_height` or below; the
    /// analysis size is the source divided by it. A source already at or
    /// below the target height is analyzed natively. A source no divisor
    /// brings to the target is still divided by its smallest common factor
    /// above one — the analysis plane is never larger than the source and
    /// never fractional — and only a source whose dimensions share no factor
    /// above one is analyzed natively. Every result is checked against the
    /// detector bounds; a source no valid factor exists for is an error.
    pub fn for_source_height(
        source_width: u32,
        source_height: u32,
        target_height: u32,
    ) -> Result<Self, AnalysisSizeError> {
        if source_width == 0 || source_height == 0 {
            return Err(AnalysisSizeError::ZeroSourceDimension {
                source_width,
                source_height,
            });
        }
        if target_height == 0 {
            return Err(AnalysisSizeError::ZeroAnalysisHeight);
        }

        // The factor must divide both dimensions, so it divides their gcd.
        // Ascending order makes the first hit the largest result that
        // reaches the target. `native` holds the k = 1 result and `partial`
        // the smallest factor above one: when nothing reaches the target,
        // the partial downscale still divides the work, so it wins over the
        // native plane.
        let common = gcd(source_width, source_height);
        let mut native = None;
        let mut partial = None;
        for factor in 1..=common {
            if !common.is_multiple_of(factor) {
                continue;
            }
            let size = Self {
                width: source_width / factor,
                height: source_height / factor,
            };
            if size.check_bounds().is_err() {
                continue;
            }
            if size.height <= target_height {
                return Ok(size);
            }
            if factor > 1 {
                partial.get_or_insert(size);
            }
            native.get_or_insert(size);
        }
        partial
            .or(native)
            .ok_or(AnalysisSizeError::NoSupportedScale {
                source_width,
                source_height,
            })
    }

    /// Builds an analysis size directly, for tests and explicit configurations.
    pub fn new(width: u32, height: u32) -> Result<Self, AnalysisSizeError> {
        let size = Self { width, height };
        size.check_bounds()?;
        Ok(size)
    }

    fn check_bounds(self) -> Result<(), AnalysisSizeError> {
        if self.width < MIN_ANALYSIS_DIMENSION || self.height < MIN_ANALYSIS_DIMENSION {
            return Err(AnalysisSizeError::BelowMinimum {
                width: self.width,
                height: self.height,
            });
        }
        if self.width > MAX_ANALYSIS_DIMENSION || self.height > MAX_ANALYSIS_DIMENSION {
            return Err(AnalysisSizeError::AboveMaximum {
                width: self.width,
                height: self.height,
            });
        }
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels > MAX_ANALYSIS_PIXELS {
            return Err(AnalysisSizeError::TooManyPixels {
                width: self.width,
                height: self.height,
                pixels,
            });
        }
        Ok(())
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn height(self) -> u32 {
        self.height
    }

    /// Bytes in one 8-bit luma plane at this size.
    pub const fn luma_bytes(self) -> usize {
        // Both dimensions are at most 4096, so the product cannot overflow a
        // 64-bit usize.
        (self.width as usize) * (self.height as usize)
    }
}

/// Why an analysis size could not be derived or accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalysisSizeError {
    ZeroSourceDimension {
        source_width: u32,
        source_height: u32,
    },
    ZeroAnalysisHeight,
    NoSupportedScale {
        source_width: u32,
        source_height: u32,
    },
    Overflow,
    BelowMinimum {
        width: u32,
        height: u32,
    },
    AboveMaximum {
        width: u32,
        height: u32,
    },
    TooManyPixels {
        width: u32,
        height: u32,
        pixels: u64,
    },
}

impl Display for AnalysisSizeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSourceDimension {
                source_width,
                source_height,
            } => write!(
                formatter,
                "the source size {source_width}x{source_height} has a zero dimension"
            ),
            Self::ZeroAnalysisHeight => {
                formatter.write_str("the analysis height must be greater than zero")
            }
            Self::NoSupportedScale {
                source_width,
                source_height,
            } => write!(
                formatter,
                "no integer scale of {source_width}x{source_height} fits the analysis bounds"
            ),
            Self::Overflow => formatter.write_str("the analysis size calculation overflowed"),
            Self::BelowMinimum { width, height } => write!(
                formatter,
                "the analysis size {width}x{height} is below the {MIN_ANALYSIS_DIMENSION}-pixel minimum"
            ),
            Self::AboveMaximum { width, height } => write!(
                formatter,
                "the analysis size {width}x{height} exceeds the {MAX_ANALYSIS_DIMENSION}-pixel maximum"
            ),
            Self::TooManyPixels {
                width,
                height,
                pixels,
            } => write!(
                formatter,
                "the analysis size {width}x{height} has {pixels} pixels, above the {MAX_ANALYSIS_PIXELS}-pixel limit"
            ),
        }
    }
}

impl Error for AnalysisSizeError {}

/// The largest integer dividing both values.
fn gcd(left: u32, right: u32) -> u32 {
    let (mut left, mut right) = (left, right);
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::{AnalysisSize, AnalysisSizeError};

    #[test]
    fn standard_sources_reach_the_target_exactly() {
        // 16:9 and 16:10 families divide down to the target height.
        let cases = [
            ((1920u32, 1080u32), (640u32, 360u32)),
            ((1280, 720), (640, 360)),
            ((2560, 1440), (640, 360)),
            ((3840, 2160), (640, 360)),
        ];
        for ((source_width, source_height), expected) in cases {
            let size = AnalysisSize::for_source(source_width, source_height)
                .expect("the source derives an analysis size");
            assert_eq!(
                (size.width(), size.height()),
                expected,
                "source {source_width}x{source_height}"
            );
        }
    }

    #[test]
    fn non_standard_sources_divide_by_their_smallest_reaching_factor() {
        // The user-recorded examples.
        let size = AnalysisSize::for_source(720, 576).expect("720x576 derives");
        assert_eq!((size.width(), size.height()), (360, 288));

        let size = AnalysisSize::for_source(1024, 768).expect("1024x768 derives");
        // 768/2 = 384 is above the target, 3 does not divide 1024, so k = 4.
        assert_eq!((size.width(), size.height()), (256, 192));

        // 960x432: k = 2 reaches 216.
        let size = AnalysisSize::for_source(960, 432).expect("960x432 derives");
        assert_eq!((size.width(), size.height()), (480, 216));
    }

    #[test]
    fn sources_at_or_below_the_target_are_analyzed_natively() {
        for (source_width, source_height) in [(96u32, 64u32), (97, 65), (48, 48), (640, 360)] {
            let size = AnalysisSize::for_source(source_width, source_height)
                .expect("the source derives an analysis size");
            assert_eq!(
                (size.width(), size.height()),
                (source_width, source_height),
                "source {source_width}x{source_height}"
            );
        }
    }

    #[test]
    fn a_source_no_divisor_reaches_the_target_is_still_divided() {
        // The only common factors of 1366x768 are 1 and 2, and 768/2 = 384 is
        // above the target, but halving still divides the work four ways.
        let size = AnalysisSize::for_source(1366, 768).expect("1366x768 derives");
        assert_eq!((size.width(), size.height()), (683, 384));

        // 854x480: k = 2 reaches the target outright (240 <= 360).
        let size = AnalysisSize::for_source(854, 480).expect("854x480 derives");
        assert_eq!((size.width(), size.height()), (427, 240));

        // Two prime dimensions share no factor above one, so the source is
        // analyzed natively.
        let size = AnalysisSize::for_source(1367, 769).expect("1367x769 derives");
        assert_eq!((size.width(), size.height()), (1367, 769));
    }

    #[test]
    fn the_scale_factor_keeps_every_dimension_inside_the_bounds() {
        // 4096x2160 natively exceeds the pixel cap; the derivation walks the
        // divisors until the area fits, at k = 4 first (1024x540), which is
        // above the target, then k = 8 reaches 270.
        let size = AnalysisSize::for_source(4096, 2160).expect("4096x2160 derives");
        assert_eq!((size.width(), size.height()), (512, 270));

        // A source below the 48-pixel minimum has no valid scale at all.
        assert_eq!(
            AnalysisSize::for_source(100, 32),
            Err(AnalysisSizeError::NoSupportedScale {
                source_width: 100,
                source_height: 32,
            })
        );
    }

    #[test]
    fn rejects_zero_dimensions() {
        assert_eq!(
            AnalysisSize::for_source(0, 64),
            Err(AnalysisSizeError::ZeroSourceDimension {
                source_width: 0,
                source_height: 64,
            })
        );
    }

    #[test]
    fn luma_buffer_sizes_are_consistent() {
        let size = AnalysisSize::new(800, 360).expect("800x360 is a supported size");
        assert_eq!(size.luma_bytes(), 288_000);
    }
}
