//! IHNI detection core: ordered WWXD hard-cut detection.
//!
//! This crate is the detector alone. It does not decode video, link FFmpeg, read
//! files, or know about a command line: the caller sends complete prepared luma
//! frames in increasing display order, one at a time, and receives a decision.
//! Every dependency on the surrounding pipeline is deliberately absent so the
//! decision function can be tested against the pinned reference on its own.
//!
//! [`detect`] holds the ordered state and the decision; [`frame_index`] holds the
//! index types that name frames and boundaries. `docs/WwxdDesign.md` maps each
//! rule to the pinned WWXD `detection.c` and separates it from project
//! extensions.

pub mod detect;
pub mod frame_index;
