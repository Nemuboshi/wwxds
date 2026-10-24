//! The luma sample layout the decode and analysis-plane path accepts.
//!
//! This type is media's own. The variants name the *gray* equivalent of a
//! source depth, which is what the decoder's luma plane is reported as and
//! the analysis filter is fed. They are not decoded-frame formats: a decoded
//! `yuv420p10le` frame is handed to the filter as
//! [`LumaFormat::Gray10Le`], because only its luma plane is read.

/// The sample layout of a luma plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LumaFormat {
    /// One byte per sample, one sample per pixel.
    Gray8,
    /// Two bytes per sample, little endian, ten significant bits.
    Gray10Le,
    /// Two bytes per sample, big endian, ten significant bits.
    Gray10Be,
}

impl LumaFormat {
    /// Bytes one sample occupies in the source plane.
    pub const fn sample_bytes(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Gray10Le | Self::Gray10Be => 2,
        }
    }

    /// A name for diagnostics, matching the libav pixel format name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gray8 => "gray8",
            Self::Gray10Le => "gray10le",
            Self::Gray10Be => "gray10be",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LumaFormat;

    #[test]
    fn layouts_report_their_documented_properties() {
        assert_eq!(LumaFormat::Gray8.sample_bytes(), 1);
        assert_eq!(LumaFormat::Gray10Le.sample_bytes(), 2);
        assert_eq!(LumaFormat::Gray10Be.sample_bytes(), 2);
        assert_eq!(LumaFormat::Gray8.name(), "gray8");
        assert_eq!(LumaFormat::Gray10Le.name(), "gray10le");
        assert_eq!(LumaFormat::Gray10Be.name(), "gray10be");
    }
}
