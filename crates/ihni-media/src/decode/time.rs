//! Time modes for decoded frames.
//!
//! The default mode reports no time at all, so the output contract does not
//! depend on whether the container carried time stamps. `derived_cfr` is
//! available only when the caller declares constant-frame-rate input and
//! supplies an exact rational rate; the derived value is elapsed time from frame
//! 0, never a source time stamp. `decoder_pts` reports the presentation time the
//! decoder produced, converted with the stream time base.
//!
//! The two kinds of time are reported through two separate fields, so a caller
//! can never mistake a derived value for a source value.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// An exact rational frame rate with a positive numerator and denominator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRate {
    numerator: u32,
    denominator: u32,
}

impl FrameRate {
    /// Builds a frame rate from a positive rational value.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, FrameRateError> {
        if numerator == 0 || denominator == 0 {
            return Err(FrameRateError);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Numerator of the rational rate.
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// Denominator of the rational rate.
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// Elapsed derived time for a source frame index, in seconds.
    ///
    /// Frame 0 starts at elapsed time zero. The value is exact rational
    /// arithmetic expressed as `f64`; it is not a source time stamp.
    pub fn elapsed_seconds(self, index: u64) -> f64 {
        index as f64 * f64::from(self.denominator) / f64::from(self.numerator)
    }
}

impl Display for FrameRate {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.numerator, self.denominator)
    }
}

/// A frame rate that is not a positive rational value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRateError;

impl Display for FrameRateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a frame rate must be a positive NUM/DEN rational value")
    }
}

impl Error for FrameRateError {}

/// How the run reports frame time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeMode {
    /// Report no time. Accepts constant and variable frame rate input.
    Unavailable,
    /// Report elapsed time derived from the declared rational frame rate.
    DerivedCfr { rate: FrameRate },
    /// Report the decoder presentation time stamp, converted with the stream
    /// time base.
    DecoderPts,
}

impl TimeMode {
    /// Elapsed derived time for one frame, when the mode derives it.
    pub fn elapsed_seconds(self, index: u64) -> Option<f64> {
        match self {
            Self::DerivedCfr { rate } => Some(rate.elapsed_seconds(index)),
            Self::Unavailable | Self::DecoderPts => None,
        }
    }

    /// Source presentation time for one frame, when the mode reports it.
    ///
    /// `time_base` is the stream time base as a `(numerator, denominator)`
    /// rational. The stamp is converted with that rational rather than with an
    /// assumed constant frame rate, so a variable-rate input keeps its own
    /// times. A frame without a usable stamp reports `None`; no value is
    /// invented for it. A mode other than `decoder_pts` reports `None` even when
    /// a stamp is available, so this field never carries a derived value.
    pub fn source_seconds(self, pts: Option<i64>, time_base: (i32, i32)) -> Option<f64> {
        match self {
            Self::DecoderPts => {
                let pts = pts?;
                let (numerator, denominator) = time_base;
                if numerator == 0 || denominator == 0 {
                    return None;
                }
                Some(pts as f64 * f64::from(numerator) / f64::from(denominator))
            }
            Self::Unavailable | Self::DerivedCfr { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameRate, TimeMode};

    #[test]
    fn rejects_zero_rates() {
        assert!(FrameRate::new(0, 1).is_err());
        assert!(FrameRate::new(1, 0).is_err());
    }

    #[test]
    fn fractional_rate_derives_exact_elapsed_time() {
        let rate = FrameRate::new(30000, 1001).expect("30000/1001 is a valid rate");
        assert_eq!(rate.elapsed_seconds(0), 0.0);
        assert!((rate.elapsed_seconds(30000) - 1001.0).abs() < 1e-9);
    }

    #[test]
    fn unavailable_mode_never_invents_time() {
        assert_eq!(TimeMode::Unavailable.elapsed_seconds(5), None);
    }

    #[test]
    fn derived_mode_reports_elapsed_time() {
        let rate = FrameRate::new(12, 1).expect("12/1 is a valid rate");
        let mode = TimeMode::DerivedCfr { rate };
        assert_eq!(mode.elapsed_seconds(6), Some(0.5));
    }

    #[test]
    fn only_the_pts_mode_reports_a_source_time() {
        // The stream time base is 1/1000: a stamp of 1500 is 1.5 seconds.
        let time_base = (1, 1000);
        assert_eq!(
            TimeMode::DecoderPts.source_seconds(Some(1500), time_base),
            Some(1.5)
        );
        // A frame without a stamp reports nothing rather than zero.
        assert_eq!(TimeMode::DecoderPts.source_seconds(None, time_base), None);
        // The other modes never report a source time, even when one exists.
        assert_eq!(
            TimeMode::Unavailable.source_seconds(Some(1500), time_base),
            None
        );
        let rate = FrameRate::new(12, 1).expect("12/1 is a valid rate");
        assert_eq!(
            TimeMode::DerivedCfr { rate }.source_seconds(Some(1500), time_base),
            None
        );
    }

    #[test]
    fn an_unusable_time_base_reports_no_source_time() {
        // A stream that declares no usable time base cannot convert a stamp, so
        // the stamp is reported as absent instead of being read as seconds.
        assert_eq!(
            TimeMode::DecoderPts.source_seconds(Some(1500), (0, 0)),
            None
        );
        assert_eq!(
            TimeMode::DecoderPts.source_seconds(Some(1500), (1, 0)),
            None
        );
    }
}
