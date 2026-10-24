//! The production resize path: one swscale context plus the 16-to-8-bit
//! reduction.
//!
//! [`FrameResizer`] drives the scaler entry points in `csrc/ihni_ffmpeg.c`:
//! one `SwsContext` for the fixed single configuration this project uses —
//! gray source, `gray16le` destination, `SWS_BICUBIC` defaults — reused for a
//! whole run, with one reusable intermediate plane.
//!
//! The shim drives swscale's new frame API (`sws_alloc_context` +
//! `sws_scale_frame`) rather than the stateful `sws_getContext`/`sws_scale`
//! pair. The legacy pair always implies `SWS_BACKEND_LEGACY`, which is single
//! threaded and silently ignores `SwsContext.threads`; the new API honours it
//! and produces byte-identical output at every thread count.
//!
//! The destination plane never hands swscale a tight buffer. swscale's SIMD
//! output writes whole vectors and overruns a row stride no real libav caller
//! would produce, so the plane uses the same 64-byte-aligned row stride the
//! parity harness measured with (`docs/Architecture.md`, "Measurement conditions").
//! The 8-bit analysis plane the detector consumes is that 16-bit result
//! reduced per sample by `>> 8`, which for little-endian `gray16le` is the
//! second byte of each pair. One path, one reduction: the two forms cannot
//! disagree because the 8-bit plane is made from the bytes swscale actually
//! wrote.

use crate::ffi;
use crate::luma::LumaFormat;

/// Row alignment the destination plane uses, matching what a real libav
/// caller's `linesize` provides and what the parity fixtures recorded with.
const DESTINATION_ALIGNMENT: usize = 64;

/// Worker threads the scaler is allowed to use; zero lets swscale choose.
///
/// This is the new swscale API's own slice count, not a project thread pool.
/// The stateful legacy `sws_scale` path ignores the setting entirely, so the
/// shim drives `sws_scale_frame` — the two produce byte-identical output at
/// every thread count, which is what keeps the recorded digest fixtures valid.
///
/// Zero rather than a fixed count: the useful number depends on how much CPU
/// the decoder is already using on the same frames, and swscale derives it
/// from the machine.
const RESIZE_THREADS: u32 = 0;

/// Resizes one source geometry and format to one fixed analysis size.
///
/// One context is built per run and reused; the intermediate 16-bit plane is
/// reused per frame. A resizer is bound to the geometry and format it was
/// opened with — feeding it a plane of any other size or depth is rejected
/// rather than silently rescaled.
pub struct FrameResizer {
    scaler: ffi::Scaler,
    source_width: usize,
    source_height: usize,
    source_format: LumaFormat,
    destination_width: usize,
    destination_height: usize,
    /// Reused `gray16le` plane, `stride * destination_height` bytes.
    plane: Vec<u8>,
    /// Bytes between the start of consecutive rows in `plane`.
    stride: usize,
}

/// Rounds `value` up to a multiple of `align`.
fn align_up(value: usize, align: usize) -> Result<usize, String> {
    value
        .checked_add(align - 1)
        .map(|padded| padded & !(align - 1))
        .ok_or_else(|| "the resize destination size overflowed".to_owned())
}

impl FrameResizer {
    /// Opens the swscale context for one source and one destination.
    ///
    /// The returned message names the failed conversion so the reader can add
    /// the input path: a scaling context knows nothing about the file it was
    /// built for.
    pub fn open(
        source_width: u32,
        source_height: u32,
        source_format: LumaFormat,
        destination_width: u32,
        destination_height: u32,
    ) -> Result<Self, String> {
        if source_width == 0 || source_height == 0 {
            return Err(format!(
                "the source size {source_width}x{source_height} has a zero dimension"
            ));
        }
        if destination_width == 0 || destination_height == 0 {
            return Err(format!(
                "the destination size {destination_width}x{destination_height} has a zero dimension"
            ));
        }
        let scaler = ffi::Scaler::open(
            source_width,
            source_height,
            source_format,
            destination_width,
            destination_height,
            RESIZE_THREADS,
        )?;
        let destination_width = destination_width as usize;
        let destination_height = destination_height as usize;
        let row = destination_width
            .checked_mul(2)
            .ok_or_else(|| "the resize destination row overflowed".to_owned())?;
        let stride = align_up(row, DESTINATION_ALIGNMENT)?;
        let length = stride
            .checked_mul(destination_height)
            .ok_or_else(|| "the resize destination plane overflowed".to_owned())?;
        Ok(Self {
            scaler,
            source_width: source_width as usize,
            source_height: source_height as usize,
            source_format,
            destination_width,
            destination_height,
            plane: vec![0u8; length],
            stride,
        })
    }

    /// The luma format the context was built for.
    pub fn source_format(&self) -> LumaFormat {
        self.source_format
    }

    /// Resizes one source plane into `target`, reduced to 8-bit samples.
    ///
    /// `source` is the decoder's plane addressed by its real `source_stride`;
    /// `target` must hold exactly the tight `width * height` analysis plane.
    /// Every length is checked before anything is handed to swscale, so a
    /// short buffer is a rejection, not an overrun.
    pub fn resize(
        &mut self,
        source: &[u8],
        source_stride: usize,
        target: &mut [u8],
    ) -> Result<(), String> {
        let source_row = self
            .source_width
            .checked_mul(self.source_format.sample_bytes())
            .ok_or_else(|| "the source row length overflowed".to_owned())?;
        if source_stride < source_row {
            return Err(format!(
                "the source stride {source_stride} is smaller than one row of {source_row} bytes"
            ));
        }
        let source_need = source_stride
            .checked_mul(self.source_height)
            .ok_or_else(|| "the source plane size overflowed".to_owned())?;
        if source.len() < source_need {
            return Err(format!(
                "the source plane needs {source_need} bytes but the slice holds {}",
                source.len()
            ));
        }
        let target_need = self
            .destination_width
            .checked_mul(self.destination_height)
            .ok_or_else(|| "the destination plane size overflowed".to_owned())?;
        if target.len() < target_need {
            return Err(format!(
                "the 8-bit destination needs {target_need} bytes but the slice holds {}",
                target.len()
            ));
        }

        self.scaler
            .scale(source, source_stride, &mut self.plane, self.stride)
            .map_err(|error| format!("resizing a frame failed: {error}"))?;

        // `gray16le` puts the value's high byte second, so `>> 8` is exactly
        // taking the second byte of every sample pair. The SIMD padding
        // beyond `destination_width * 2` stays inside `plane`.
        for row in 0..self.destination_height {
            let wide_start = row * self.stride;
            let wide_end = wide_start + self.destination_width * 2;
            let tight_start = row * self.destination_width;
            let tight_end = tight_start + self.destination_width;
            let (samples, _) = self.plane[wide_start..wide_end].as_chunks::<2>();
            let line = &mut target[tight_start..tight_end];
            for (out, sample) in line.iter_mut().zip(samples) {
                *out = sample[1];
            }
        }
        Ok(())
    }
}
