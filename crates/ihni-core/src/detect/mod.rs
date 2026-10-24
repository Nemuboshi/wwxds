//! Ordered WWXD scene detection.
//!
//! One [`Detector`] instance owns the previous padded luma frame, block motion
//! predictors, shot age, and the current frame index. The caller sends complete
//! prepared frames in increasing display order; the detector returns a decision
//! only after it validates the whole frame. The core is a faithful port of the
//! pinned `detection.c` decision function and has no decoder, FFmpeg, or CLI
//! dependency. See `docs/WwxdDesign.md` for the source mapping, equations,
//! constants, and bounds.

mod core;
mod state;
mod types;

pub use state::Detector;
pub use types::{
    Decision, DecisionReason, DetectError, DetectorFrame, DetectorTimings, Scores, StateDigests,
};
