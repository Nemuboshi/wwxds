//! Overlapped decoding.
//!
//! A producer thread owns the [`FrameReader`] and fills reusable frame buffers
//! while the caller runs detection on the frame it already has. Frames cross the
//! channel in display order, one at a time, and the detector still consumes them
//! in a single thread. The detector therefore sees exactly the sequence a serial
//! read would produce, so the overlap cannot change a decision.
//!
//! The producer holds at most `read_ahead` frame buffers, so peak memory is
//! bounded by that count rather than by the length of the video. A caller that
//! stops early drops the stream; the producer then observes a closed channel and
//! stops, closing the decoder with it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::error::MediaError;
use ihni_core::frame_index::SourceFrameIndex;

use super::ProducerTimings;
use super::analysis::AnalysisSize;
use super::probe::SourceInfo;
use super::reader::{FrameMeta, FrameReader, ReadOutcome, ReaderOptions};

/// Producer-side timings shared between the producer thread and the stream.
///
/// The producer publishes a snapshot of its reader's accumulated decode and
/// resize time once per frame, never inside a pixel or block loop. The
/// consumer reads it as nanosecond counters; the frame channel's send/receive
/// pair orders the last snapshot before the end of stream, so a consumer that
/// received the end always sees the producer's final numbers.
#[derive(Debug)]
struct SharedProducerTimings {
    decode_ns: AtomicU64,
    resize_ns: AtomicU64,
}

impl SharedProducerTimings {
    fn publish(&self, timings: ProducerTimings) {
        self.decode_ns.store(
            Duration::as_nanos(&timings.decode)
                .try_into()
                .unwrap_or(u64::MAX),
            Ordering::Release,
        );
        self.resize_ns.store(
            Duration::as_nanos(&timings.resize)
                .try_into()
                .unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    fn snapshot(&self) -> ProducerTimings {
        let nanos_to_duration =
            |value: u64| Duration::new(value / 1_000_000_000, (value % 1_000_000_000) as u32);
        ProducerTimings {
            decode: nanos_to_duration(self.decode_ns.load(Ordering::Acquire)),
            resize: nanos_to_duration(self.resize_ns.load(Ordering::Acquire)),
        }
    }
}

/// One complete prepared frame in a reusable buffer.
pub struct Frame {
    meta: FrameMeta,
    luma: Vec<u8>,
}

impl Frame {
    /// Zero-based index of this frame in display order.
    pub fn index(&self) -> SourceFrameIndex {
        self.meta.index
    }

    /// Elapsed derived seconds, present only in `derived_cfr` mode.
    pub fn derived_seconds(&self) -> Option<f64> {
        self.meta.derived_seconds
    }

    /// The 8-bit analysis luma plane, `height * stride` bytes.
    pub fn luma(&self) -> &[u8] {
        &self.luma
    }
}

/// What the producer sends to the consumer.
///
/// `Ok(None)` is the end of the stream; it is distinct from a failure so an
/// incomplete decode is never mistaken for a clean end.
type Produced = Result<Option<Frame>, MediaError>;

/// A display-order frame stream that decodes ahead of the consumer.
pub struct FrameStream {
    ready: Option<Receiver<Produced>>,
    /// Buffers returned by the consumer, reused by the producer.
    free: Option<SyncSender<Vec<u8>>>,
    producer: Option<JoinHandle<()>>,
    /// The producer thread's timing snapshots, published once per frame.
    producer_timings: Arc<SharedProducerTimings>,
    path: PathBuf,
    source: SourceInfo,
    size: AnalysisSize,
    ended: bool,
}

impl FrameStream {
    /// Starts the decoder and its producer thread.
    ///
    /// The input is opened here, on the calling thread, so an unusable input
    /// unusable input fails before a thread is created.
    pub fn open(path: &Path, options: ReaderOptions) -> Result<Self, MediaError> {
        let reader = FrameReader::open(path, options)?;
        let source = reader.source().clone();
        let size = reader.analysis_size();
        let slots = options.slot_count();

        let (ready_tx, ready_rx) = sync_channel::<Produced>(slots);
        let (free_tx, free_rx) = sync_channel::<Vec<u8>>(slots);
        // Seed the pool so the producer does not allocate in steady state. The
        // channel was created with exactly this capacity and has no consumer
        // yet, so every send succeeds.
        for _ in 0..slots {
            let _ = free_tx.try_send(Vec::new());
        }

        let producer_path = path.to_path_buf();
        let timings = Arc::new(SharedProducerTimings {
            decode_ns: AtomicU64::new(0),
            resize_ns: AtomicU64::new(0),
        });
        let producer_timings = Arc::clone(&timings);
        let producer = thread::Builder::new()
            .name("ihni-decode".to_owned())
            .spawn(move || produce(reader, producer_path, ready_tx, free_rx, producer_timings))
            .map_err(|source| MediaError::ThreadSpawn {
                path: path.to_path_buf(),
                source,
            })?;

        Ok(Self {
            ready: Some(ready_rx),
            free: Some(free_tx),
            producer: Some(producer),
            producer_timings: timings,
            path: path.to_path_buf(),
            source,
            size,
            ended: false,
        })
    }

    /// Metadata for the selected source stream.
    pub fn source(&self) -> &SourceInfo {
        &self.source
    }

    /// The fixed analysis size of this run.
    pub fn analysis_size(&self) -> AnalysisSize {
        self.size
    }

    /// Waits for the next frame in display order.
    ///
    /// Returns `None` at the end of the stream. A decode failure, a truncated
    /// frame, or an incomplete decoder run is returned as an error, never as an
    /// early end of stream.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, MediaError> {
        if self.ended {
            return Err(MediaError::Decode {
                operation: "read a frame after the end of the stream",
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::UnexpectedEof, "stream already ended"),
            });
        }
        let ready = self.ready.as_ref().ok_or_else(|| MediaError::Decode {
            operation: "read a frame after the end of the stream",
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::UnexpectedEof, "stream already ended"),
        })?;
        match ready.recv() {
            Ok(Ok(frame)) => {
                if frame.is_none() {
                    self.ended = true;
                }
                Ok(frame)
            }
            Ok(Err(error)) => {
                self.ended = true;
                Err(error)
            }
            Err(_) => {
                // The producer only stops without reporting an outcome when its
                // channel closed, which cannot happen while this receiver is
                // alive. Report it rather than reporting a clean end.
                self.ended = true;
                Err(MediaError::Decode {
                    operation: "receive a prepared frame",
                    path: self.path.clone(),
                    source: io::Error::other("the decode producer stopped unexpectedly"),
                })
            }
        }
    }

    /// Returns a consumed frame's buffer to the producer for reuse.
    ///
    /// The buffer is reused by a later frame, so a caller must not keep any
    /// reference to it after recycling. A frame that is dropped instead is
    /// simply not reused; the producer allocates a replacement buffer.
    pub fn recycle(&self, frame: Frame) {
        if let Some(free) = self.free.as_ref() {
            // A full pool means the producer is ahead of the consumer; the
            // extra buffer is dropped rather than blocking the caller.
            let _ = free.try_send(frame.luma);
        }
    }

    /// The producer thread's decode and resize cost, once the stream has
    /// ended.
    ///
    /// `None` while frames are still flowing: the producer publishes its
    /// accumulated cost once per frame before sending it, so a complete
    /// figure only exists after the end-of-stream (or error) item has been
    /// received. The snapshot of the end item covers every frame the
    /// producer read, because the final drain decode happens inside the
    /// same call that produced that item.
    pub fn producer_timings(&self) -> Option<ProducerTimings> {
        if self.ended {
            Some(self.producer_timings.snapshot())
        } else {
            None
        }
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        // Closing both ends makes the producer's next send or receive fail, so
        // it stops after at most one frame read. The reader's `Drop` then closes
        // the decoder.
        self.ready.take();
        self.free.take();
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

/// Fills frame buffers until the stream ends or the consumer goes away.
fn produce(
    mut reader: FrameReader,
    path: PathBuf,
    ready: SyncSender<Produced>,
    free: Receiver<Vec<u8>>,
    timings: Arc<SharedProducerTimings>,
) {
    loop {
        // Reuse a returned buffer when one is available. A consumer that keeps
        // a frame causes a fresh allocation instead of a deadlock.
        let mut luma = free.try_recv().unwrap_or_default();

        let outcome = reader.next_frame_into(&mut luma);
        // Publish the reader's accumulated producer cost once per frame,
        // before the frame goes on the channel, so the consumer's receive on
        // that channel happens after this write.
        timings.publish(reader.producer_timings());
        let produced = match outcome {
            Ok(ReadOutcome::Frame) => match reader.current_meta() {
                Some(meta) => Ok(Some(Frame { meta, luma })),
                None => Err(MediaError::Decode {
                    operation: "read the committed frame metadata",
                    path: path.clone(),
                    source: io::Error::other("frame metadata was not committed"),
                }),
            },
            // `next_frame_into` waits for the decoder and rejects diagnostics
            // before reporting a clean end, so this is a complete stream.
            Ok(ReadOutcome::EndOfStream) => Ok(None),
            Err(error) => Err(error),
        };

        let is_end = matches!(produced, Ok(None) | Err(_));
        if ready.send(produced).is_err() || is_end {
            return;
        }
    }
}
