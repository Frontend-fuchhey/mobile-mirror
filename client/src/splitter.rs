use crate::protocol::NalPacket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

/// Telemetry counters for the zero-copy NAL splitter.
#[derive(Default)]
pub struct SplitterMetrics {
    pub packets_dispatched: AtomicU64,
    pub bytes_dispatched: AtomicU64,
    pub decoder_drops: AtomicU64,
}

/// Zero-copy packet splitter that fans out reference-counted `Arc<NalPacket>`
/// instances to both the MP4 muxer and the real-time hardware decoder without
/// duplicating underlying NAL payload bytes.
pub struct NalSplitter {
    muxer_tx: Option<SyncSender<Arc<NalPacket>>>,
    decoder_tx: Option<SyncSender<Arc<NalPacket>>>,
    metrics: Arc<SplitterMetrics>,
}

impl NalSplitter {
    pub fn new(
        enable_muxer: bool,
        enable_decoder: bool,
    ) -> (
        Self,
        Option<Receiver<Arc<NalPacket>>>,
        Option<Receiver<Arc<NalPacket>>>,
        Arc<SplitterMetrics>,
    ) {
        let (muxer_tx, muxer_rx) = if enable_muxer {
            // Deep queue for lossless disk recording
            let (tx, rx) = mpsc::sync_channel(512);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let (decoder_tx, decoder_rx) = if enable_decoder {
            let (tx, rx) = mpsc::sync_channel(128);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let metrics = Arc::new(SplitterMetrics::default());

        (
            Self {
                muxer_tx,
                decoder_tx,
                metrics: Arc::clone(&metrics),
            },
            muxer_rx,
            decoder_rx,
            metrics,
        )
    }

    /// Dispatches a packet to both pipelines using `Arc::clone` (atomic pointer refcount increment only).
    pub fn dispatch(&mut self, packet: Arc<NalPacket>) {
        self.metrics
            .packets_dispatched
            .fetch_add(1, Ordering::Relaxed);
        self.metrics
            .bytes_dispatched
            .fetch_add(packet.payload.len() as u64, Ordering::Relaxed);

        // Path A: Lossless MP4 recording (never drop config or video packets)
        if let Some(ref tx) = self.muxer_tx {
            if tx.send(Arc::clone(&packet)).is_err() {
                self.muxer_tx = None;
            }
        }

        // Path B: Real-time low-latency hardware decoding
        if let Some(ref tx) = self.decoder_tx {
            if tx.send(packet).is_err() {
                self.decoder_tx = None;
            }
        }
    }
}
