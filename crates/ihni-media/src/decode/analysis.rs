//! The fixed analysis size for one run.
//!
//! The detector compares blocks at one analysis size. The default analysis
//! height is 360; the caller can override it. The width keeps the
//! source aspect ratio and stays a multiple of 4, which the reference harness's
//! padding expects. These bounds are reference safety limits, not accuracy
//! parameters.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Analysis height used for the initial comparison configuration.
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
    ///
    /// The width is `4 * round(source_width * height / (4 * source_height))`.
    /// A half-way value rounds upward. All arithmetic is checked.
    pub fn for_source(source_width: u32, source_height: u32) -> Result<Self, AnalysisSizeError> {
        Self::for_source_height(source_width, source_height, ANALYSIS_HEIGHT)
    }

    /// Derives the analysis size for a source size at a chosen analysis height.
    pub fn for_source_height(
        source_width: u32,
        source_height: u32,
        analysis_height: u32,
    ) -> Result<Self, AnalysisSizeError> {
        if source_width == 0 || source_height == 0 {
            return Err(AnalysisSizeError::ZeroSourceDimension {
                source_width,
                source_height,
            });
        }
        if analysis_height == 0 {
            return Err(AnalysisSizeError::ZeroAnalysisHeight);
        }

        let numerator = u64::from(source_width)
            .checked_mul(u64::from(analysis_height))
            .ok_or(AnalysisSizeError::Overflow)?;
        let denominator = u64::from(source_height)
            .checked_mul(4)
            .ok_or(AnalysisSizeError::Overflow)?;

        // Round the quotient of the four-pixel-column count upward on a tie.
        let columns = numerator
            .checked_add(denominator / 2)
            .ok_or(AnalysisSizeError::Overflow)?
            / denominator;
        let width = columns.checked_mul(4).ok_or(AnalysisSizeError::Overflow)?;

        let size = Self {
            width: u32::try_from(width).map_err(|_| AnalysisSizeError::Overflow)?,
            height: analysis_height,
        };
        size.check_bounds()?;
        Ok(size)
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

#[cfg(test)]
mod tests {
    use super::{ANALYSIS_HEIGHT, AnalysisSize, AnalysisSizeError};

    #[test]
    fn derives_the_documented_benchmark_sizes() {
        // The measured benchmark used 800x360 for a 960x432 source.
        let size = AnalysisSize::for_source(960, 432).expect("960x432 is a supported source");
        assert_eq!((size.width(), size.height()), (800, ANALYSIS_HEIGHT));

        // A 16:9 source maps to 640x360.
        let wide = AnalysisSize::for_source(1920, 1080).expect("1920x1080 is a supported source");
        assert_eq!((wide.width(), wide.height()), (640, 360));

        // A portrait source keeps its orientation.
        let tall = AnalysisSize::for_source(720, 1280).expect("720x1280 is a supported source");
        assert_eq!((tall.width(), tall.height()), (204, 360));
    }

    #[test]
    fn width_is_always_a_multiple_of_four() {
        for source_width in [145u32, 146, 147, 149, 641, 959, 960, 961, 1919, 1920] {
            let size = AnalysisSize::for_source(source_width, 1080)
                .expect("a 1080-high source is supported");
            assert_eq!(size.width() % 4, 0, "source width {source_width}");
        }
    }

    #[test]
    fn odd_source_dimensions_follow_the_rule() {
        // 97x65 at height 360: round(97 * 360 / (4 * 65)) = round(134.31) = 134.
        let size = AnalysisSize::for_source(97, 65).expect("97x65 is a supported source");
        assert_eq!((size.width(), size.height()), (536, 360));
    }

    #[test]
    fn ties_round_upward() {
        // 50x100 at height 360 gives exactly 45 four-pixel columns.
        let exact = AnalysisSize::for_source(50, 100).expect("50x100 is a supported source");
        assert_eq!(exact.width(), 180);

        // 51x100 gives 45.9 columns, which rounds to 46.
        let rounded = AnalysisSize::for_source(51, 100).expect("51x100 is a supported source");
        assert_eq!(rounded.width(), 184);
    }

    #[test]
    fn rejects_zero_dimensions_and_tiny_sizes() {
        assert_eq!(
            AnalysisSize::for_source(0, 64),
            Err(AnalysisSizeError::ZeroSourceDimension {
                source_width: 0,
                source_height: 64,
            })
        );
        assert!(AnalysisSize::for_source(1, 1000).is_err());
    }

    #[test]
    fn rejects_an_extreme_aspect_ratio_instead_of_stretching_it() {
        // 10000x1 at height 360 would be 1,440,000 pixels wide.
        assert!(AnalysisSize::for_source(10000, 1).is_err());
        // 1x10000 at height 360 is 4 pixels wide, below the minimum.
        assert!(AnalysisSize::for_source(1, 10000).is_err());
    }

    #[test]
    fn luma_buffer_sizes_are_consistent() {
        let size = AnalysisSize::new(800, 360).expect("800x360 is a supported size");
        assert_eq!(size.luma_bytes(), 288_000);
    }
}
