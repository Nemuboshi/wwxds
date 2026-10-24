//! WWXDS media layer: probing, display-order decoding, and frame preparation.
//!
//! This crate owns every contact with media files. It is the only crate that
//! knows how frames are obtained, and the only one that touches FFmpeg.
//!
//! The stages are:
//!
//! 1. [`decode::probe`] selects one absolute stream index and records source
//!    metadata.
//! 2. [`decode::analysis::AnalysisSize`] derives the fixed analysis size for the
//!    run.
//! 3. [`decode::reader::FrameReader`] decodes complete frames in display order
//!    and resamples each luma plane to the 8-bit analysis plane through the
//!    linked libswscale ([`decode::resize::FrameResizer`]).
//! 4. [`decode::overlap::FrameStream`] runs the reader on a producer thread so
//!    decoding overlaps detection without changing the frame sequence.
//!
//! Decoding happens in this process, through the C shim in `csrc/` and the
//! declarations in `ffi`. The binary starts no child process and reads no pipe.
//! FFmpeg libraries are linked statically; see `docs/Architecture.md`.
//!
//! The detector is not here: this crate depends on `wwxds-core` for the frame
//! index types, never the other way round. The production resize is the
//! linked libswscale; the retired Rust resampler and its parity oracle were
//! removed once the migration was verified.
//!
//! # Unsafe code
//!
//! `unsafe` appears only in `ffi`, where each block documents the condition that
//! makes it sound. It is denied everywhere else, together with any `unsafe`
//! operation inside an `unsafe fn` and any `unsafe` block without a safety
//! comment.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

pub mod decode;
pub mod error;
mod ffi;
mod luma;

pub use error::MediaError;

/// The luma sample layout the decode and resize path accepts.
///
/// Defined here, in the crate that feeds it to the resizer.
pub use luma::LumaFormat;

/// The frame index types that appear in this crate's public signatures.
///
/// They are defined by `wwxds-core`; re-exporting them lets a caller that depends
/// only on this crate still name the indexes it receives.
pub use wwxds_core::frame_index;
