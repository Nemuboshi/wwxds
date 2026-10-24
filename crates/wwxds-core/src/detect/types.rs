//! Detector input and output vocabulary.
//!
//! These types depend only on the frame-index contract. The detector does not
//! import the decoder, an FFmpeg type, or a CLI type.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use crate::frame_index::{BoundaryFrameIndex, SourceFrameIndex};

/// Wall time spent inside one [`super::Detector::process`] call.
///
/// `prepare` covers the padding copy into the detector's plane; `analyze` covers
/// the macroblock decision. The two are measured separately and are never
/// derived from the total.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DetectorTimings {
    /// Time spent padding the borrowed frame into the detector's plane.
    pub prepare: Duration,
    /// Time spent in the block decision.
    pub analyze: Duration,
}

/// One borrowed prepared frame handed to the detector.
///
/// `luma` is read through `stride`, never assumed to be `stride == width`. The
/// detector copies it into its own padded buffers; the caller's plane is not
/// retained after [`super::Detector::process`] returns.
#[derive(Clone, Copy, Debug)]
pub struct DetectorFrame<'a> {
    /// Zero-based index in display order. Must be exactly the index the
    /// detector expects next.
    pub index: SourceFrameIndex,
    pub width: u32,
    pub height: u32,
    /// Bytes between the start of consecutive rows.
    pub stride: usize,
    /// Borrowed 8-bit luma plane. Length at least `(height - 1) * stride + width`.
    pub luma: &'a [u8],
}

/// Why a detector call did not return a decision, or why input was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetectError {
    /// Construction dimensions are outside the analysis bounds the reference
    /// harness accepts: not `48 <= width,height <= 4096` or `width*height`
    /// exceeds `2_097_152`.
    Dimensions { width: u32, height: u32 },
    /// The frame dimensions do not match the detector's construction size.
    ///
    /// The reference never changes analysis size within one instance.
    SizeMismatch {
        expected_width: u32,
        expected_height: u32,
        got_width: u32,
        got_height: u32,
    },
    /// `stride < width`; rows cannot be read without overlapping.
    StrideTooSmall { stride: usize, width: usize },
    /// `luma.len() < (height - 1) * stride + width`.
    BufferTooShort { length: usize, required: usize },
    /// The frame index is not the next expected display-order index.
    ///
    /// A `None` expected means the first frame must be index 0 and no start
    /// point has been established.
    IndexOrder { expected: Option<u64>, got: u64 },
    /// The stream was finalised; further frames are not accepted.
    StateFinalized,
}

impl Display for DetectError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dimensions { width, height } => write!(
                formatter,
                "analysis dimensions {width}x{height} are outside the range 48..=4096 with area at most 2097152"
            ),
            Self::SizeMismatch {
                expected_width,
                expected_height,
                got_width,
                got_height,
            } => write!(
                formatter,
                "frame size {got_width}x{got_height} does not match the detector size {expected_width}x{expected_height}"
            ),
            Self::StrideTooSmall { stride, width } => {
                write!(
                    formatter,
                    "luma stride {stride} is smaller than width {width}"
                )
            }
            Self::BufferTooShort { length, required } => write!(
                formatter,
                "luma buffer has {length} bytes but the frame needs at least {required}"
            ),
            Self::IndexOrder { expected, got } => match expected {
                Some(expected) => write!(
                    formatter,
                    "frame index {got} is out of order; the detector expects {expected}"
                ),
                None => write!(
                    formatter,
                    "frame index {got} cannot start a stream; the first frame must be index 0"
                ),
            },
            Self::StateFinalized => write!(
                formatter,
                "the detector stream was finalised and accepts no further frames"
            ),
        }
    }
}

impl Error for DetectError {}

/// Which rule in the ported reference produced a decision.
///
/// Each variant names the `detection.c` / `reference.c` return site it maps to.
/// Reason values are diagnostics; the decision itself is the same boolean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionReason {
    /// `reference.c:86`: the first frame returns a cut in the reference. The
    /// production path reports the marker without turning it into a boundary.
    FirstFrame,
    /// `detection.c:454-455`: intra sub-block count exceeded the majority
    /// threshold and the loop returned early.
    IntraMajority,
    /// `detection.c:470`: the mean block SAD exceeded the adaptive threshold.
    MeanSadHigh,
    /// `detection.c:470`: the mean block SAD did not exceed the threshold.
    NoCut,
}

/// Raw per-frame block evidence from the reference.
///
/// Every field is `None` when the reference did not compute it: the first-frame
/// marker never runs `MEanalysis`, and the intra-majority early return exits the
/// block loop before normalisation. A missing value is never represented as
/// zero. `Scores` is library-internal and no production JSON schema exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scores {
    /// Accumulated block motion sum before normalisation (`sSAD`). A partial
    /// tally when `complete` is false.
    pub motion_sum: Option<i64>,
    /// Block energy term after the `>>= 7` shift (`complexity`).
    pub activity: Option<i64>,
    /// Sampled block counter (`blocks`).
    pub sampled_blocks: Option<i64>,
    /// Final normalised score compared against `threshold` (`sSAD / ...`).
    pub mean_sad: Option<i64>,
    /// Adaptive threshold `sSAD` is compared against (`IntraThresh2`).
    pub threshold: Option<i64>,
    /// Sub-blocks marked intra at exit (`intra`).
    pub intra_blocks: Option<i64>,
    /// Whether the reference ran the full block loop to its normal end.
    pub complete: bool,
}

/// No diagnostics available (the first-frame marker ran no analysis).
pub(crate) const fn scores_unavailable() -> Scores {
    Scores {
        motion_sum: None,
        activity: None,
        sampled_blocks: None,
        mean_sad: None,
        threshold: None,
        intra_blocks: None,
        complete: false,
    }
}

/// One frame's decision and evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Decision {
    /// Boundary at the first frame of a new shot. `Some` only when a real cut
    /// was detected and `index >= 1`. Frame 0 is a valid source index but never
    /// a boundary.
    pub boundary: Option<BoundaryFrameIndex>,
    /// The rule that produced this decision.
    pub reason: DecisionReason,
    /// Block evidence recorded during the decision.
    pub scores: Scores,
}

impl Decision {
    /// Whether the reference reported a cut for this frame.
    pub fn is_cut(&self) -> bool {
        self.reason == DecisionReason::FirstFrame || self.boundary.is_some()
    }
}

/// Boundary helper shared by the state path.
///
/// Frame 0 is a valid source index but never a boundary, so `BoundaryFrameIndex`
/// rejects it; the caller only offers an index that already passed the >0 rule.
pub(super) fn boundary_at(index: SourceFrameIndex) -> Option<BoundaryFrameIndex> {
    BoundaryFrameIndex::new(index).ok()
}

/// Content digests of the detector's internal state, for the pinned-C parity
/// test. These are diagnostics only; they never affect a decision.
///
/// The reference and this port both keep a reused macroblock predictor array and
/// the padded previous plane. Digesting them pins every predictor vector, `sad16`,
/// and `mode`, and the padding replication, so parity is more than one bit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateDigests {
    /// FNV-1a over the macroblock array in raster order.
    pub predictors: u64,
    /// FNV-1a over the padded previous plane (full buffer, including borders).
    pub previous_plane: u64,
}

/// FNV-1a 64-bit offset basis (the standard constant; not a detection value).
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Streaming FNV-1a accumulator shared by the parity digests.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Self(FNV_OFFSET)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    pub(crate) fn write_i32(&mut self, value: i32) {
        self.write(&value.to_le_bytes());
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

/// Canonical FNV-1a digest of one macroblock predictor, in the same field order
/// the C oracle serializes: eight motion-vector components, then `sad16`, then
/// `mode`, each as a little-endian `i32`.
pub(crate) fn fnv_predictor(hash: &mut Fnv, block: &super::core::MacroBlock) {
    for vector in block.mvs {
        hash.write_i32(vector.x);
        hash.write_i32(vector.y);
    }
    hash.write_i32(block.sad16);
    hash.write_i32(block.mode);
}
