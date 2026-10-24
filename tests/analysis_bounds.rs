//! The dimension contract between the detector and the media layer.
//!
//! The detector must accept exactly the analysis sizes the decoder can produce.
//! Neither crate can check that from the inside: `wwxds-core` has no reference to
//! `wwxds-media` (AGENTS.md: keep the detection core independent of the decoder),
//! and a test inside `wwxds-core` would have to take a dev-dependency on the
//! media crate to see the decoder's bounds. So the check lives here, in the only
//! package that depends on both, and the two bound sets are still compared.

use wwxds::decode::analysis::{
    ANALYSIS_HEIGHT, MAX_ANALYSIS_DIMENSION, MAX_ANALYSIS_PIXELS, MIN_ANALYSIS_DIMENSION,
};
use wwxds::detect::Detector;

#[test]
fn the_detector_accepts_exactly_the_analysis_sizes_the_decoder_can_produce() {
    assert_eq!(
        (
            ANALYSIS_HEIGHT,
            MIN_ANALYSIS_DIMENSION,
            MAX_ANALYSIS_DIMENSION
        ),
        (360, 48, 4096)
    );
    assert!(Detector::new(MIN_ANALYSIS_DIMENSION, ANALYSIS_HEIGHT).is_ok());
    // A legal analysis rectangle at the exact area cap.
    assert!(Detector::new(MAX_ANALYSIS_DIMENSION, (MAX_ANALYSIS_PIXELS / 4096) as u32).is_ok());
    assert!(Detector::new(MIN_ANALYSIS_DIMENSION - 1, ANALYSIS_HEIGHT).is_err());
    assert!(Detector::new(MAX_ANALYSIS_DIMENSION + 1, MIN_ANALYSIS_DIMENSION).is_err());
    // One pixel over the area cap.
    assert!(
        Detector::new(
            MAX_ANALYSIS_DIMENSION,
            (MAX_ANALYSIS_PIXELS / 4096) as u32 + 1
        )
        .is_err()
    );
}
