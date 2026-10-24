//! Ordered detector state: the WWXD frame harness in Rust.
//!
//! One [`Detector`] owns the two padded planes, the reused macroblock array, the
//! reused search scratch, the next expected frame index, and the shot age. It
//! mirrors the pinned plugin state in `wwxd.c` and the reference harness's
//! `create` / `process` behaviour. Frames must arrive in display order. The
//! detector is used from one thread; it starts no work of its own.

use std::time::{Duration, Instant};

use super::core::{AnalysisResult, Geometry, MacroBlock, SearchData, me_analysis};
use super::types::{
    Decision, DecisionReason, DetectError, DetectorFrame, DetectorTimings, Fnv, StateDigests,
    boundary_at, fnv_predictor, scores_unavailable,
};

/// `fcode` the reference harness always passes to `MEanalysis` (golden.c:87).
const FCODE: i32 = 4;

/// `MBParam.edged_height` mirror; the padded plane has `mb_height * 16 + 128`
/// rows including the two 64-row borders (golden.c:50-56).
fn edged_height(mb_height: u32) -> u32 {
    mb_height * 16 + 128
}

/// A configured, ordered WWXD detector over frames of one fixed analysis size.
pub struct Detector {
    geometry: Geometry,
    /// Previous padded plane (`golden.c` `previous`).
    previous: Vec<u8>,
    /// Current padded plane, overwritten each frame (`golden.c` `current`).
    current: Vec<u8>,
    /// Reused macroblock predictors (`golden.c` `blocks`); never re-zeroed.
    mbs: Vec<MacroBlock>,
    /// Reused search scratch, zeroed once (see docs/WWXDDesign.md §5, §7).
    search: SearchData,
    /// Next expected display-order index.
    next_index: u64,
    /// Shot age, equal to the harness `intra_count` (minimum 1, saturating).
    shot_age: i32,
    /// `golden.c` `first`: the first frame returns a marker without analysis.
    first: bool,
    /// Set by [`Detector::finish`]; no further frames are accepted.
    finalized: bool,
    /// Wall time of the most recent accepted frame's two stages.
    last_timings: DetectorTimings,
}

impl Detector {
    /// Create a detector for the given analysis size.
    ///
    /// Dimensions must be inside the reference harness's accepted range
    /// (`48..=4096` each, area at most `2_097_152`), the same bounds `wwxds`
    /// records in `decode::analysis`. The detector runs the exact serial
    /// schedule.
    pub fn new(width: u32, height: u32) -> Result<Self, DetectError> {
        if !(48..=4096).contains(&width)
            || !(48..=4096).contains(&height)
            || u64::from(width) * u64::from(height) > 2_097_152
        {
            return Err(DetectError::Dimensions { width, height });
        }

        let mb_width = width.div_ceil(16);
        let mb_height = height.div_ceil(16);
        let edged_width = usize::try_from(mb_width * 16 + 128)
            .expect("edged width fits usize within the validated analysis range");
        let inner_base = 64 * edged_width + 64;
        let geometry = Geometry {
            width,
            height,
            edged_width,
            mb_width,
            mb_height,
            inner_base,
        };

        // Plane allocation identical to golden.c:57-61.
        let plane_len = edged_width
            * (usize::try_from(edged_height(mb_height)).expect("edged height fits usize") + 1)
            + 64;
        let mbs_len = usize::try_from(mb_width * mb_height).expect("mb count fits usize");

        Ok(Self {
            geometry,
            previous: vec![0; plane_len],
            current: vec![0; plane_len],
            mbs: vec![MacroBlock::default(); mbs_len],
            search: SearchData::default(),
            next_index: 0,
            shot_age: 1,
            first: true,
            finalized: false,
            last_timings: DetectorTimings::default(),
        })
    }

    /// Wall time of the two stages of the most recent accepted frame.
    pub fn last_timings(&self) -> DetectorTimings {
        self.last_timings
    }

    /// The next display-order index this detector will accept.
    pub fn expected_index(&self) -> u64 {
        self.next_index
    }

    /// Current shot age (`intra_count`).
    pub fn shot_age(&self) -> i32 {
        self.shot_age
    }

    /// Process one frame and return its decision.
    ///
    /// Validation of the index, size, stride, and buffer length happens before
    /// any state is changed: a rejected frame leaves the detector identical
    /// (`AGENTS.md` stop condition "a detector state change after invalid
    /// input").
    pub fn process(&mut self, frame: DetectorFrame<'_>) -> Result<Decision, DetectError> {
        if self.finalized {
            return Err(DetectError::StateFinalized);
        }

        let index = frame.index.get();
        if index != self.next_index {
            let expected = if self.first {
                None
            } else {
                Some(self.next_index)
            };
            return Err(DetectError::IndexOrder {
                expected,
                got: index,
            });
        }
        if frame.width != self.geometry.width || frame.height != self.geometry.height {
            return Err(DetectError::SizeMismatch {
                expected_width: self.geometry.width,
                expected_height: self.geometry.height,
                got_width: frame.width,
                got_height: frame.height,
            });
        }
        let width = frame.width as usize;
        if frame.stride < width {
            return Err(DetectError::StrideTooSmall {
                stride: frame.stride,
                width,
            });
        }
        // The last copied byte of the plane is `(height - 1) * stride + width`
        // (golden.c copies `width` bytes per row, so it reads through the final
        // row's width-1 column).
        let required = (frame.height as usize - 1)
            .checked_mul(frame.stride)
            .and_then(|n| n.checked_add(width))
            .ok_or(DetectError::BufferTooShort {
                length: frame.luma.len(),
                required: usize::MAX,
            })?;
        if frame.luma.len() < required {
            return Err(DetectError::BufferTooShort {
                length: frame.luma.len(),
                required,
            });
        }

        // All checks passed; the mutation below is committed only for valid
        // frames. Pad the borrowed plane into the reused `current` buffer.
        let prepare_start = Instant::now();
        self.pad(frame);
        let prepare = prepare_start.elapsed();

        if self.first {
            // golden.c:86: the first frame is a marker; MEanalysis is not run,
            // so no block diagnostics exist. The marker is not an output
            // boundary (frame 0 is never a boundary).
            let decision = Decision {
                boundary: None,
                reason: DecisionReason::FirstFrame,
                scores: scores_unavailable(),
            };
            self.shot_age = 1; // golden.c:89 cut -> intra_count = 1
            self.swap_planes();
            self.first = false;
            self.next_index = 1;
            self.last_timings = DetectorTimings {
                prepare,
                analyze: Duration::ZERO,
            };
            return Ok(decision);
        }

        let analyze_start = Instant::now();
        let result = self.run_analysis();
        let analyze = analyze_start.elapsed();
        self.last_timings = DetectorTimings { prepare, analyze };

        // golden.c:89-90: update shot age from the cut BEFORE the plane swap.
        if result.cut {
            self.shot_age = 1;
        } else {
            // golden.c:90 caps at INT32_MAX, so a saturating add is exact.
            self.shot_age = self.shot_age.saturating_add(1);
        }
        self.swap_planes();
        self.next_index += 1;

        // index >= 1 here, so a cut boundary always passes the >0 check.
        let boundary = if result.cut {
            boundary_at(frame.index)
        } else {
            None
        };
        Ok(Decision {
            boundary,
            reason: result.reason,
            scores: result.scores,
        })
    }

    /// Finalise the stream. Emits no decision and cannot invent a frame after
    /// the last. Idempotent; a later [`Detector::process`] returns
    /// `StateFinalized`.
    pub fn finish(&mut self) -> Result<(), DetectError> {
        self.finalized = true;
        Ok(())
    }

    /// The reference `MEanalysis` call for the current padded planes.
    fn run_analysis(&mut self) -> AnalysisResult {
        let g = self.geometry;
        let shot_age = self.shot_age;
        // Borrow the two planes without aliasing `mbs`/`search`.
        let previous = &self.previous;
        let current = &self.current;
        me_analysis(
            previous,
            current,
            &g,
            &mut self.mbs,
            &mut self.search,
            shot_age,
            FCODE,
        )
    }

    /// Replicate `golden.c:78-85`: copy the input rows by stride, right-pad
    /// each to `mb_width * 16` with the last source pixel, and bottom-pad the
    /// remaining macroblock rows by repeating the last source row.
    fn pad(&mut self, frame: DetectorFrame<'_>) {
        let g = &self.geometry;
        let inner = g.inner_base;
        let ew = g.edged_width;
        let width = g.width as usize;
        let height = g.height as usize;
        let padded_width = (g.mb_width * 16) as usize;
        let current = &mut self.current;

        for y in 0..height {
            let row_dst = inner + y * ew;
            let row_src = y * frame.stride;
            current[row_dst..row_dst + width]
                .copy_from_slice(&frame.luma[row_src..row_src + width]);
            let last = current[row_dst + width - 1];
            current[row_dst + width..row_dst + padded_width].fill(last);
        }

        let last_content_row = inner + (height - 1) * ew;
        let bottom_end = (g.mb_height as usize) * 16;
        for y in height..bottom_end {
            let row_dst = inner + y * ew;
            let (head, tail) = current.split_at_mut(row_dst);
            let src = &head[last_content_row..last_content_row + ew];
            tail[..ew].copy_from_slice(src);
        }
    }

    /// Swap `previous` and `current` (golden.c:91-93).
    fn swap_planes(&mut self) {
        std::mem::swap(&mut self.previous, &mut self.current);
    }

    /// Diagnostic digests of the reused macroblock array and the padded previous
    /// plane. Used by the pinned-C parity test; never affects a decision.
    pub fn state_digests(&self) -> StateDigests {
        let mut predictors = Fnv::new();
        for block in &self.mbs {
            fnv_predictor(&mut predictors, block);
        }
        let mut previous_plane = Fnv::new();
        previous_plane.write(&self.previous);
        StateDigests {
            predictors: predictors.finish(),
            previous_plane: previous_plane.finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_index::SourceFrameIndex;

    fn uniform(width: u32, height: u32, value: u8) -> Vec<u8> {
        vec![value; (width * height) as usize]
    }

    fn tight_frame(index: u64, plane: &[u8], width: u32, height: u32) -> DetectorFrame<'_> {
        DetectorFrame {
            index: SourceFrameIndex::new(index),
            width,
            height,
            stride: width as usize,
            luma: plane,
        }
    }

    #[test]
    fn first_frame_is_a_marker_and_not_a_boundary() {
        let mut detector = Detector::new(48, 48).expect("valid size");
        let plane = uniform(48, 48, 32);
        let decision = detector
            .process(tight_frame(0, &plane, 48, 48))
            .expect("first frame accepted");
        assert_eq!(decision.reason, DecisionReason::FirstFrame);
        assert!(
            decision.boundary.is_none(),
            "frame 0 must never be a boundary"
        );
        assert!(decision.is_cut(), "the reference marks the first frame");
        assert!(!decision.scores.complete);
        assert_eq!(detector.expected_index(), 1);
    }

    #[test]
    fn repeated_uniform_frames_add_no_boundary() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        // Frame 0 marker, then identical frames: no cut, then steady NoCut.
        let _ = detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        for index in 1..8u64 {
            let decision = detector
                .process(tight_frame(index, &plane, 96, 64))
                .expect("in-order frame");
            assert!(decision.boundary.is_none(), "uniform repeat at {index} cut");
            assert!(!decision.is_cut());
        }
    }

    #[test]
    fn an_isolated_hard_cut_returns_the_new_shot_first_frame() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let dark = uniform(96, 64, 32);
        let light = uniform(96, 64, 220);
        detector.process(tight_frame(0, &dark, 96, 64)).unwrap();
        detector.process(tight_frame(1, &dark, 96, 64)).unwrap();
        // A global dark->light step at frame 2 is a synthetic hard cut.
        let decision = detector.process(tight_frame(2, &light, 96, 64)).unwrap();
        assert!(decision.boundary.is_some(), "hard cut at frame 2 missed");
        assert_eq!(decision.boundary.unwrap().get(), 2);
        assert!(decision.is_cut());
    }

    #[test]
    fn a_rejected_gap_leaves_state_unchanged() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        let before_index = detector.expected_index();
        let before_age = detector.shot_age();
        // Skipping index 1 must fail and change nothing.
        let error = detector
            .process(tight_frame(2, &plane, 96, 64))
            .expect_err("gap rejected");
        assert!(matches!(error, DetectError::IndexOrder { .. }));
        assert_eq!(detector.expected_index(), before_index);
        assert_eq!(detector.shot_age(), before_age);
        // The correct frame still processes afterward.
        let after = detector.process(tight_frame(1, &plane, 96, 64));
        assert!(after.is_ok(), "state was corrupted by the rejected frame");
    }

    #[test]
    fn invalid_stride_and_buffer_do_not_advance_state() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        let before = detector.expected_index();

        // stride < width rejected
        let bad = DetectorFrame {
            index: SourceFrameIndex::new(1),
            width: 96,
            height: 64,
            stride: 95,
            luma: &plane,
        };
        assert!(matches!(
            detector.process(bad),
            Err(DetectError::StrideTooSmall { .. })
        ));
        assert_eq!(detector.expected_index(), before);

        // short buffer rejected
        let mut short = plane.clone();
        short.truncate(short.len() - 1);
        let bad = DetectorFrame {
            index: SourceFrameIndex::new(1),
            width: 96,
            height: 64,
            stride: 96,
            luma: &short,
        };
        assert!(matches!(
            detector.process(bad),
            Err(DetectError::BufferTooShort { .. })
        ));
        assert_eq!(detector.expected_index(), before);
    }

    #[test]
    fn finish_stops_the_stream_and_is_idempotent() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        detector.finish().expect("finish");
        detector.finish().expect("finish is idempotent");
        let error = detector.process(tight_frame(1, &plane, 96, 64));
        assert!(matches!(error, Err(DetectError::StateFinalized)));
    }

    #[test]
    fn padded_stride_matches_tight_stride() {
        // Same content, two strides: identical decisions and shot age.
        let dark = uniform(96, 64, 32);
        let light = uniform(96, 64, 220);

        let mut tight = Detector::new(96, 64).unwrap();
        let mut padded_detector = Detector::new(96, 64).unwrap();
        let stride = 96 + 16;
        let mut padded_plane = vec![255u8; 64 * stride];
        for row in 0..64 {
            padded_plane[row * stride..row * stride + 96]
                .copy_from_slice(&dark[row * 96..row * 96 + 96]);
        }

        tight.process(tight_frame(0, &dark, 96, 64)).unwrap();
        padded_detector
            .process(DetectorFrame {
                index: SourceFrameIndex::new(0),
                width: 96,
                height: 64,
                stride,
                luma: &padded_plane,
            })
            .unwrap();

        let a = tight.process(tight_frame(1, &dark, 96, 64)).unwrap();
        let b = padded_detector
            .process(DetectorFrame {
                index: SourceFrameIndex::new(1),
                width: 96,
                height: 64,
                stride,
                luma: &padded_plane,
            })
            .unwrap();
        // Now flip both to light at frame 2 (the hard cut).
        for row in 0..64 {
            padded_plane[row * stride..row * stride + 96]
                .copy_from_slice(&light[row * 96..row * 96 + 96]);
        }
        let c = tight.process(tight_frame(2, &light, 96, 64)).unwrap();
        let d = padded_detector
            .process(DetectorFrame {
                index: SourceFrameIndex::new(2),
                width: 96,
                height: 64,
                stride,
                luma: &padded_plane,
            })
            .unwrap();
        assert_eq!(a.boundary, b.boundary);
        assert_eq!(c.boundary, d.boundary);
        assert_eq!(c.reason, d.reason);
        assert_eq!(c.scores, d.scores);
    }

    #[test]
    fn analysis_size_change_is_rejected() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        let wrong = DetectorFrame {
            index: SourceFrameIndex::new(1),
            width: 96,
            height: 80,
            stride: 96,
            luma: &vec![32u8; 96 * 80],
        };
        assert!(matches!(
            detector.process(wrong),
            Err(DetectError::SizeMismatch { .. })
        ));
        assert_eq!(detector.expected_index(), 1, "size change must not advance");
    }

    #[test]
    fn repeated_runs_are_identical() {
        let dark = uniform(96, 64, 32);
        let light = uniform(96, 64, 220);
        let run = || {
            let mut detector = Detector::new(96, 64).unwrap();
            let mut out = Vec::new();
            let frames = [dark.as_slice(), dark.as_slice(), light.as_slice()];
            for (index, frame_plane) in frames.iter().enumerate() {
                out.push(
                    detector
                        .process(tight_frame(index as u64, frame_plane, 96, 64))
                        .unwrap(),
                );
            }
            out
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn dimension_bounds_are_the_documented_range() {
        // 48..=4096 each, area at most 2_097_152 — the same range the reference
        // harness validates (golden.c:46-47).
        assert!(Detector::new(48, 48).is_ok());
        assert!(Detector::new(4096, 512).is_ok(), "exact area cap accepted");
        assert!(matches!(
            Detector::new(47, 48),
            Err(DetectError::Dimensions { .. })
        ));
        assert!(matches!(
            Detector::new(48, 4097),
            Err(DetectError::Dimensions { .. })
        ));
        assert!(
            matches!(
                Detector::new(4096, 513),
                Err(DetectError::Dimensions { .. })
            ),
            "area one over the cap rejected"
        );
    }

    #[test]
    fn duplicate_and_downward_indexes_are_rejected_without_state_change() {
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        detector.process(tight_frame(0, &plane, 96, 64)).unwrap();
        detector.process(tight_frame(1, &plane, 96, 64)).unwrap();
        let index = detector.expected_index();
        let age = detector.shot_age();

        // A duplicate (index 1 again) is rejected.
        assert!(matches!(
            detector.process(tight_frame(1, &plane, 96, 64)),
            Err(DetectError::IndexOrder { .. })
        ));
        // A downward index is rejected.
        assert!(matches!(
            detector.process(tight_frame(0, &plane, 96, 64)),
            Err(DetectError::IndexOrder { .. })
        ));
        assert_eq!(detector.expected_index(), index);
        assert_eq!(detector.shot_age(), age);

        // The correct next frame still processes, proving state is intact.
        assert!(detector.process(tight_frame(2, &plane, 96, 64)).is_ok());
    }

    #[test]
    fn first_frame_must_be_index_zero() {
        // No resume API: a stream cannot start mid-way, because shot age
        // and predictors have no prior frame to build on.
        let mut detector = Detector::new(96, 64).expect("valid size");
        let plane = uniform(96, 64, 32);
        assert!(matches!(
            detector.process(tight_frame(5, &plane, 96, 64)),
            Err(DetectError::IndexOrder {
                expected: None,
                got: 5
            })
        ));
    }

    #[test]
    fn empty_stream_finalizes_cleanly() {
        // A stream that processes no frame can still finish; the flush must not
        // invent a boundary.
        let mut detector = Detector::new(96, 64).expect("valid size");
        assert_eq!(detector.expected_index(), 0);
        detector.finish().expect("empty finish");
    }
}
