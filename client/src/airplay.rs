//! Full-featured AirPlay 1 & AirPlay 2 Screen Mirroring + Audio receiver powered by `rust-airplay`.
//!
//! Provides:
//! - Real mDNS / DNS-SD advertisement (`_airplay._tcp.local.` and `_raop._tcp.local.`) via `mdns-sd`
//!   with proper `SO_REUSEADDR` and multi-interface Windows/macOS/Linux support.
//! - Complete AirPlay 1 / AirPlay 2 RTSP control server, Ed25519/X25519/HAP pairing, and real
//!   FairPlay SAP v3 (`fp-setup` + `playfair` key unwrapping).
//! - Real AES-128-CTR H.264 mirroring video decryption and NTP/RTCP timing synchronization.
//! - Real AirPlay audio reception (AAC-ELD / ALAC) decoded via bundled FFmpeg `libavcodec` and
//!   resampled to 48 kHz 16-bit stereo PCM for live PC speaker playback and MP4 recording.

use std::ffi::{c_void, CString};
use std::io;
use std::net::TcpListener;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rust_airplay::{
    AudioCodec, AudioConfig, AudioFrame, AudioStream, Config as AirPlayConfig, RenderError,
    Renderer, VideoConfig, VideoFrame, VideoStream, Volume,
};

use crate::ffi::ffmpeg::{
    FfmpegApi, AVMEDIA_TYPE_AUDIO, AV_CODEC_ID_AAC, AV_CODEC_ID_ALAC, AV_SAMPLE_FMT_FLT,
    AV_SAMPLE_FMT_FLTP, AV_SAMPLE_FMT_S16, AV_SAMPLE_FMT_S16P, AVERROR_EAGAIN, AVERROR_EOF,
};
use crate::protocol::{
    extract_sps_pps, ControlWriter, DevicePlatform, NalPacket, PacketSource, StreamHeader,
    CODEC_ID_H264,
};

const DEFAULT_AIRPLAY_PORT: u16 = 7000;
const TARGET_AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Derives a stable 32-byte AirPlay receiver identity seed from the receiver name and machine name
/// so iOS devices recognize the same AirPlay target across restarts without re-pairing issues.
fn derive_stable_seed(device_name: &str) -> [u8; 32] {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "desktop".to_string());
    let mut seed = [0u8; 32];
    let input = format!("mobile-mirror-airplay-v2:{}:{}", host, device_name);
    // Deterministic mixing (FNV-1a style expansion across 32 bytes)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (idx, b) in input.bytes().enumerate() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
        seed[idx % 32] ^= (h >> ((idx % 8) * 8)) as u8;
    }
    for i in 0..32 {
        h ^= (i as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
        h = h.wrapping_mul(0x0100_0000_01b3);
        seed[i] = seed[i].wrapping_add((h >> 24) as u8);
    }
    seed
}

/// Probes whether `DEFAULT_AIRPLAY_PORT` (7000) is available; falls back to an OS-assigned port.
fn pick_available_port() -> io::Result<u16> {
    if let Ok(l) = TcpListener::bind(("0.0.0.0", DEFAULT_AIRPLAY_PORT)) {
        let port = l.local_addr()?.port();
        drop(l);
        return Ok(port);
    }
    let l = TcpListener::bind(("0.0.0.0", 0))?;
    let port = l.local_addr()?.port();
    drop(l);
    Ok(port)
}

/// Shared lifetime guard for the background Tokio AirPlay runtime.
///
/// Held by both [`AirPlayServer`] and any active [`AirPlaySession`]. When the connector drops
/// `AirPlayServer` after accepting a session, the `Arc` kept inside `AirPlaySession` keeps the
/// AirPlay receiver runtime alive until the live mirroring session itself ends.
struct AirPlayRuntimeGuard {
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    runtime_thread: Mutex<Option<JoinHandle<()>>>,
}

impl AirPlayRuntimeGuard {
    fn shutdown_now(&self) {
        if let Ok(mut tx_slot) = self.shutdown_tx.lock() {
            if let Some(tx) = tx_slot.take() {
                let _ = tx.send(());
            }
        }
        if let Ok(mut th_slot) = self.runtime_thread.lock() {
            if let Some(handle) = th_slot.take() {
                if handle.thread().id() != thread::current().id() {
                    let _ = handle.join();
                }
            }
        }
    }
}

impl Drop for AirPlayRuntimeGuard {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

/// Pure-Rust AirPlay 1 & AirPlay 2 Screen Mirroring + Audio receiver.
pub struct AirPlayServer {
    port: u16,
    session_rx: Mutex<mpsc::Receiver<AirPlaySession>>,
    runtime_guard: Option<Arc<AirPlayRuntimeGuard>>,
}

impl AirPlayServer {
    /// Starts the AirPlay mDNS advertiser and RTSP/FairPlay/media receiver.
    pub fn start(device_name: &str) -> io::Result<Self> {
        let port = pick_available_port()?;
        let seed = derive_stable_seed(device_name);

        let (session_tx, session_rx) = mpsc::channel::<AirPlaySession>();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let runtime_guard = Arc::new(AirPlayRuntimeGuard {
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            runtime_thread: Mutex::new(None),
        });

        let bridge = Arc::new(AirPlayBridge {
            device_name: device_name.to_string(),
            session_tx: Mutex::new(session_tx),
            active_channel: Mutex::new(None),
            volume: Mutex::new(Volume::FULL),
            runtime_guard: Arc::clone(&runtime_guard),
        });

        let renderer: Arc<dyn Renderer> = Arc::new(MobileMirrorAirPlayRenderer {
            bridge: Arc::clone(&bridge),
        });

        let mut config = AirPlayConfig::new(device_name, seed, renderer);
        config.port = port;
        config.advertise = true;
        config.nohold = true;
        config.feedback_timeout = Some(Duration::from_secs(20));

        let (started_tx, started_rx) = mpsc::channel::<Result<(), String>>();

        let runtime_thread = thread::Builder::new()
            .name("airplay-runtime".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("Tokio runtime init failed: {e}")));
                        return;
                    }
                };
                let _ = started_tx.send(Ok(()));

                rt.block_on(async move {
                    if let Err(err) = rust_airplay::serve(config, async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    {
                        eprintln!("[AirPlay] Receiver exited with error: {err}");
                    }
                });
            })?;

        match started_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                let _ = runtime_thread.join();
                return Err(io::Error::new(io::ErrorKind::Other, msg));
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Timed out starting AirPlay runtime",
                ));
            }
        }

        if let Ok(mut slot) = runtime_guard.runtime_thread.lock() {
            *slot = Some(runtime_thread);
        }

        println!(
            "[AirPlay] Receiver '{}' listening on port {} (mDNS advertised)",
            device_name, port
        );

        Ok(Self {
            port,
            session_rx: Mutex::new(session_rx),
            runtime_guard: Some(runtime_guard),
        })
    }

    /// Non-blocking check for a newly connected AirPlay mirroring session.
    pub fn try_accept_session(&self) -> Option<AirPlaySession> {
        self.session_rx.lock().ok()?.try_recv().ok()
    }

    /// Returns the TCP port the AirPlay RTSP/control server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Releases the server's reference to the background AirPlay runtime.
    /// If no active `AirPlaySession` is running, the runtime shuts down immediately.
    pub fn stop(mut self) {
        self.runtime_guard.take();
    }
}

impl Drop for AirPlayServer {
    fn drop(&mut self) {
        self.runtime_guard.take();
    }
}

// ---------------------------------------------------------------------------
// Bridge between `rust-airplay::Renderer` and Mobile Mirror `PacketSource`
// ---------------------------------------------------------------------------

struct ActiveChannel {
    tx: mpsc::SyncSender<NalPacket>,
    started_at: Instant,
}

struct AirPlayBridge {
    device_name: String,
    session_tx: Mutex<mpsc::Sender<AirPlaySession>>,
    active_channel: Mutex<Option<ActiveChannel>>,
    volume: Mutex<Volume>,
    runtime_guard: Arc<AirPlayRuntimeGuard>,
}

impl AirPlayBridge {
    fn ensure_session(&self, width: u32, height: u32) -> (mpsc::SyncSender<NalPacket>, Instant) {
        let mut guard = self
            .active_channel
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(ref existing) = *guard {
            return (existing.tx.clone(), existing.started_at);
        }

        let (tx, rx) = mpsc::sync_channel::<NalPacket>(512);
        let started_at = Instant::now();
        *guard = Some(ActiveChannel {
            tx: tx.clone(),
            started_at,
        });
        drop(guard);

        let session = AirPlaySession {
            rx,
            header: StreamHeader {
                codec_id: CODEC_ID_H264,
                width: if width == 0 { 1920 } else { width },
                height: if height == 0 { 1080 } else { height },
                framed: true,
            },
            device_name: self.device_name.clone(),
            control: ControlWriter::noop(),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            runtime_guard: Some(Arc::clone(&self.runtime_guard)),
        };

        if let Ok(stx) = self.session_tx.lock() {
            let _ = stx.send(session);
        }

        (tx, started_at)
    }

    fn current_volume_gain(&self) -> f32 {
        self.volume
            .lock()
            .map(|v| v.level().clamp(0.0, 1.0) as f32)
            .unwrap_or(1.0)
    }

    fn close_session(&self) {
        if let Ok(mut guard) = self.active_channel.lock() {
            *guard = None;
        }
    }
}

struct MobileMirrorAirPlayRenderer {
    bridge: Arc<AirPlayBridge>,
}

impl std::fmt::Debug for MobileMirrorAirPlayRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileMirrorAirPlayRenderer")
            .field("device_name", &self.bridge.device_name)
            .finish()
    }
}

impl Renderer for MobileMirrorAirPlayRenderer {
    fn open_audio(&self, config: AudioConfig) -> Result<Box<dyn AudioStream>, RenderError> {
        let decoder = AirPlayAudioDecoder::new(config).ok();
        Ok(Box::new(AirPlayAudioSink {
            bridge: Arc::clone(&self.bridge),
            config,
            decoder: Mutex::new(decoder),
            resampler: Mutex::new(LinearStereoResampler::new(
                config.sample_rate,
                TARGET_AUDIO_SAMPLE_RATE,
            )),
        }))
    }

    fn open_video(&self, config: VideoConfig) -> Result<Box<dyn VideoStream>, RenderError> {
        let width = config.width.unwrap_or(1920);
        let height = config.height.unwrap_or(1080);
        let (tx, started_at) = self.bridge.ensure_session(width, height);
        Ok(Box::new(AirPlayVideoSink {
            bridge: Arc::clone(&self.bridge),
            tx,
            started_at,
            sent_config: AtomicBool::new(false),
        }))
    }

    fn set_volume(&self, volume: Volume) -> Result<(), RenderError> {
        if let Ok(mut v) = self.bridge.volume.lock() {
            *v = volume;
        }
        Ok(())
    }
}

struct AirPlayVideoSink {
    bridge: Arc<AirPlayBridge>,
    tx: mpsc::SyncSender<NalPacket>,
    started_at: Instant,
    sent_config: AtomicBool,
}

impl std::fmt::Debug for AirPlayVideoSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirPlayVideoSink").finish()
    }
}

impl VideoStream for AirPlayVideoSink {
    fn push(&self, frame: VideoFrame) -> Result<(), RenderError> {
        if frame.data.is_empty() {
            return Ok(());
        }

        let now = Instant::now();
        let pts_us = normalize_airplay_pts_us(frame.timestamp, self.started_at);

        // Extract and emit SPS/PPS config packet once (or whenever new parameter sets arrive)
        // so the MP4 muxer always has valid avcC extradata even if recording starts mid-stream.
        let (sps, pps) = extract_sps_pps(&frame.data);
        if let (Some(sps_nal), Some(pps_nal)) = (sps, pps) {
            if !self.sent_config.swap(true, Ordering::AcqRel) || frame.keyframe {
                let mut cfg = Vec::with_capacity(8 + sps_nal.len() + pps_nal.len());
                cfg.extend_from_slice(&[0, 0, 0, 1]);
                cfg.extend_from_slice(&sps_nal);
                cfg.extend_from_slice(&[0, 0, 0, 1]);
                cfg.extend_from_slice(&pps_nal);
                let _ = self.tx.try_send(NalPacket {
                    pts_us: 0,
                    is_config: true,
                    is_keyframe: false,
                    is_audio: false,
                    payload: Arc::from(cfg),
                    received_at: now,
                });
            }
        }

        let pkt = NalPacket {
            pts_us,
            is_config: false,
            is_keyframe: frame.keyframe,
            is_audio: false,
            payload: Arc::from(frame.data),
            received_at: now,
        };

        // Never block the AirPlay receive thread if the UI consumer briefly lags
        let _ = self.tx.try_send(pkt);
        Ok(())
    }

    fn flush(&self) -> Result<(), RenderError> {
        Ok(())
    }

    fn close(&self) -> Result<(), RenderError> {
        self.bridge.close_session();
        Ok(())
    }
}

struct AirPlayAudioSink {
    bridge: Arc<AirPlayBridge>,
    config: AudioConfig,
    decoder: Mutex<Option<AirPlayAudioDecoder>>,
    resampler: Mutex<LinearStereoResampler>,
}

impl std::fmt::Debug for AirPlayAudioSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirPlayAudioSink")
            .field("codec", &self.config.codec)
            .field("sample_rate", &self.config.sample_rate)
            .finish()
    }
}

impl AudioStream for AirPlayAudioSink {
    fn push(&self, frame: AudioFrame) -> Result<(), RenderError> {
        if frame.data.is_empty() {
            return Ok(());
        }

        // Only forward audio when an active mirroring session is running
        let (tx, started_at) = {
            let guard = match self.bridge.active_channel.lock() {
                Ok(g) => g,
                Err(_) => return Ok(()),
            };
            match *guard {
                Some(ref ch) => (ch.tx.clone(), ch.started_at),
                None => return Ok(()),
            }
        };

        let decoded_s16 = {
            let mut dec_guard = match self.decoder.lock() {
                Ok(g) => g,
                Err(_) => return Ok(()),
            };
            match dec_guard.as_mut() {
                Some(dec) => match dec.decode(&frame.data) {
                    Ok(pcm) if !pcm.is_empty() => pcm,
                    _ => return Ok(()),
                },
                None => return Ok(()),
            }
        };

        let gain = self.bridge.current_volume_gain();
        let resampled_bytes = {
            let mut rs = match self.resampler.lock() {
                Ok(g) => g,
                Err(_) => return Ok(()),
            };
            rs.resample_stereo_s16_to_bytes(&decoded_s16, gain)
        };

        if !resampled_bytes.is_empty() {
            let pts_us = started_at.elapsed().as_micros() as u64;
            let _ = tx.try_send(NalPacket {
                pts_us,
                is_config: false,
                is_keyframe: false,
                is_audio: true,
                payload: Arc::from(resampled_bytes),
                received_at: Instant::now(),
            });
        }

        Ok(())
    }

    fn flush(&self) -> Result<(), RenderError> {
        if let Ok(mut rs) = self.resampler.lock() {
            rs.reset();
        }
        Ok(())
    }

    fn close(&self) -> Result<(), RenderError> {
        Ok(())
    }
}

fn normalize_airplay_pts_us(raw_ts: u64, started_at: Instant) -> u64 {
    if raw_ts == 0 {
        return started_at.elapsed().as_micros() as u64;
    }
    // `rust-airplay` emits wall-clock timestamps in nanoseconds (or microseconds on some paths).
    if raw_ts > 1_000_000_000_000 {
        raw_ts / 1_000
    } else {
        raw_ts
    }
}

/// Active AirPlay Screen Mirroring session implementing [`PacketSource`].
pub struct AirPlaySession {
    rx: mpsc::Receiver<NalPacket>,
    header: StreamHeader,
    device_name: String,
    control: ControlWriter,
    shutdown_flag: Arc<AtomicBool>,
    runtime_guard: Option<Arc<AirPlayRuntimeGuard>>,
}

impl PacketSource for AirPlaySession {
    fn header(&self) -> StreamHeader {
        self.header
    }

    fn read_packet(&mut self) -> io::Result<Option<NalPacket>> {
        loop {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return Ok(None);
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(pkt) => return Ok(Some(pkt)),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }

    fn control_writer(&self) -> ControlWriter {
        self.control.clone()
    }

    fn platform(&self) -> DevicePlatform {
        DevicePlatform::IosAirPlay
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn shutdown(&mut self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(guard) = self.runtime_guard.take() {
            guard.shutdown_now();
        }
    }
}

impl Drop for AirPlaySession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// AirPlay Audio Decoder (AAC-ELD / ALAC via bundled FFmpeg libavcodec)
// + 44.1 kHz -> 48 kHz Stereo S16LE Linear Resampler
// ---------------------------------------------------------------------------

/// Builds the 4-byte MPEG-4 AudioSpecificConfig for AirPlay AAC-ELD (44.1 kHz, 480 samples/frame).
pub fn build_aac_eld_asc(sample_rate: u32, channels: u8, samples_per_frame: u16) -> [u8; 4] {
    // Standard AirPlay AAC-ELD 44.1 kHz stereo 480-sample AudioSpecificConfig: `F8 E8 50 00`
    // ObjectType = 31 (escape) + 7 (= 39, ELD), freqIdx = 4 (44100 Hz), chanCfg = 2, frameLengthFlag = 1 (480)
    if sample_rate == 44_100 && channels == 2 && samples_per_frame == 480 {
        return [0xF8, 0xE8, 0x50, 0x00];
    }
    if sample_rate == 44_100 && channels == 1 && samples_per_frame == 480 {
        return [0xF8, 0xE8, 0x48, 0x00];
    }
    if sample_rate == 48_000 && channels == 2 && samples_per_frame == 480 {
        return [0xF8, 0xE8, 0x30, 0x00];
    }
    [0xF8, 0xE8, 0x50, 0x00]
}

struct AirPlayAudioDecoder {
    api: FfmpegApi,
    codec_ctx: *mut c_void,
    pkt: *mut crate::ffi::ffmpeg::AVPacket,
    frame: *mut crate::ffi::ffmpeg::AVFrame,
    channels: usize,
}

unsafe impl Send for AirPlayAudioDecoder {}

impl AirPlayAudioDecoder {
    fn new(config: AudioConfig) -> Result<Self, String> {
        let api = FfmpegApi::load()?;
        let (codec_name, codec_id, extradata): (&str, i32, Vec<u8>) = match config.codec {
            AudioCodec::AacEld => (
                "aac",
                AV_CODEC_ID_AAC,
                build_aac_eld_asc(config.sample_rate, config.channels, config.samples_per_frame)
                    .to_vec(),
            ),
            AudioCodec::Alac => ("alac", AV_CODEC_ID_ALAC, Vec::new()),
            _ => return Err("Unsupported AirPlay audio codec".to_string()),
        };

        unsafe {
            let c_name = CString::new(codec_name).unwrap();
            let mut codec = (api.avcodec_find_decoder_by_name)(c_name.as_ptr());
            if codec.is_null() {
                codec = (api.avcodec_find_decoder)(codec_id);
            }
            if codec.is_null() {
                return Err(format!("FFmpeg decoder '{}' not available", codec_name));
            }

            let codec_ctx = (api.avcodec_alloc_context3)(codec);
            if codec_ctx.is_null() {
                return Err("avcodec_alloc_context3 failed".to_string());
            }

            if !extradata.is_empty() {
                let par = (api.avcodec_parameters_alloc)();
                if !par.is_null() {
                    (*par).codec_type = AVMEDIA_TYPE_AUDIO;
                    (*par).codec_id = codec_id;
                    let ed = (api.av_mallocz)(extradata.len() + 64) as *mut u8;
                    if !ed.is_null() {
                        ptr::copy_nonoverlapping(extradata.as_ptr(), ed, extradata.len());
                        (*par).extradata = ed;
                        (*par).extradata_size = extradata.len() as i32;
                    }
                    let _ = (api.avcodec_parameters_to_context)(codec_ctx, par);
                    let mut par_ptr = par;
                    (api.avcodec_parameters_free)(&mut par_ptr);
                }
            }

            let open_ret = (api.avcodec_open2)(codec_ctx, codec, ptr::null_mut());
            if open_ret < 0 {
                let mut ctx_ptr = codec_ctx;
                (api.avcodec_free_context)(&mut ctx_ptr);
                return Err(format!("avcodec_open2 failed for {} ({})", codec_name, open_ret));
            }

            let pkt = (api.av_packet_alloc)();
            let frame = (api.av_frame_alloc)();
            if pkt.is_null() || frame.is_null() {
                if !pkt.is_null() {
                    let mut p = pkt;
                    (api.av_packet_free)(&mut p);
                }
                if !frame.is_null() {
                    let mut f = frame;
                    (api.av_frame_free)(&mut f);
                }
                let mut ctx_ptr = codec_ctx;
                (api.avcodec_free_context)(&mut ctx_ptr);
                return Err("Failed to allocate audio AVPacket/AVFrame".to_string());
            }

            Ok(Self {
                api,
                codec_ctx,
                pkt,
                frame,
                channels: config.channels.clamp(1, 2) as usize,
            })
        }
    }

    /// Decodes a single coded AirPlay audio frame into interleaved stereo `i16` samples.
    fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, String> {
        unsafe {
            (self.api.av_packet_unref)(self.pkt);
            (*self.pkt).data = data.as_ptr() as *mut u8;
            (*self.pkt).size = data.len() as i32;

            let send_ret = (self.api.avcodec_send_packet)(self.codec_ctx, self.pkt);
            if send_ret < 0 && send_ret != AVERROR_EAGAIN {
                return Err(format!("audio avcodec_send_packet failed: {send_ret}"));
            }

            let mut out = Vec::new();
            loop {
                (self.api.av_frame_unref)(self.frame);
                let recv_ret = (self.api.avcodec_receive_frame)(self.codec_ctx, self.frame);
                if recv_ret == AVERROR_EAGAIN || recv_ret == AVERROR_EOF {
                    break;
                }
                if recv_ret < 0 {
                    return Err(format!("audio avcodec_receive_frame failed: {recv_ret}"));
                }

                let nb_samples = (*self.frame).nb_samples.max(0) as usize;
                let fmt = (*self.frame).format;
                if nb_samples == 0 || (*self.frame).data[0].is_null() {
                    continue;
                }

                out.reserve(nb_samples * 2);
                match fmt {
                    AV_SAMPLE_FMT_FLTP => {
                        let left = std::slice::from_raw_parts(
                            (*self.frame).data[0] as *const f32,
                            nb_samples,
                        );
                        let right = if self.channels >= 2 && !(*self.frame).data[1].is_null() {
                            std::slice::from_raw_parts(
                                (*self.frame).data[1] as *const f32,
                                nb_samples,
                            )
                        } else {
                            left
                        };
                        for i in 0..nb_samples {
                            out.push(f32_to_i16(left[i]));
                            out.push(f32_to_i16(right[i]));
                        }
                    }
                    AV_SAMPLE_FMT_FLT => {
                        let interleaved = std::slice::from_raw_parts(
                            (*self.frame).data[0] as *const f32,
                            nb_samples * self.channels,
                        );
                        if self.channels >= 2 {
                            for pair in interleaved.chunks_exact(2) {
                                out.push(f32_to_i16(pair[0]));
                                out.push(f32_to_i16(pair[1]));
                            }
                        } else {
                            for &s in interleaved {
                                let v = f32_to_i16(s);
                                out.push(v);
                                out.push(v);
                            }
                        }
                    }
                    AV_SAMPLE_FMT_S16P => {
                        let left = std::slice::from_raw_parts(
                            (*self.frame).data[0] as *const i16,
                            nb_samples,
                        );
                        let right = if self.channels >= 2 && !(*self.frame).data[1].is_null() {
                            std::slice::from_raw_parts(
                                (*self.frame).data[1] as *const i16,
                                nb_samples,
                            )
                        } else {
                            left
                        };
                        for i in 0..nb_samples {
                            out.push(left[i]);
                            out.push(right[i]);
                        }
                    }
                    AV_SAMPLE_FMT_S16 => {
                        let interleaved = std::slice::from_raw_parts(
                            (*self.frame).data[0] as *const i16,
                            nb_samples * self.channels,
                        );
                        if self.channels >= 2 {
                            out.extend_from_slice(interleaved);
                        } else {
                            for &s in interleaved {
                                out.push(s);
                                out.push(s);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(out)
        }
    }
}

impl Drop for AirPlayAudioDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.frame.is_null() {
                (self.api.av_frame_free)(&mut self.frame);
            }
            if !self.pkt.is_null() {
                (self.api.av_packet_free)(&mut self.pkt);
            }
            if !self.codec_ctx.is_null() {
                (self.api.avcodec_free_context)(&mut self.codec_ctx);
            }
        }
    }
}

#[inline]
fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Lightweight stateful linear resampler for converting 44.1 kHz AirPlay stereo PCM to 48 kHz S16LE.
pub struct LinearStereoResampler {
    in_rate: u32,
    out_rate: u32,
    phase: f64,
    prev_l: i16,
    prev_r: i16,
}

impl LinearStereoResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            in_rate: in_rate.max(8_000),
            out_rate: out_rate.max(8_000),
            phase: 0.0,
            prev_l: 0,
            prev_r: 0,
        }
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.prev_l = 0;
        self.prev_r = 0;
    }

    pub fn resample_stereo_s16_to_bytes(&mut self, stereo_in: &[i16], gain: f32) -> Vec<u8> {
        let in_frames = stereo_in.len() / 2;
        if in_frames == 0 {
            return Vec::new();
        }
        let g = gain.clamp(0.0, 1.0);

        if self.in_rate == self.out_rate {
            let mut out = Vec::with_capacity(in_frames * 4);
            for chunk in stereo_in.chunks_exact(2) {
                let l = (chunk[0] as f32 * g).clamp(-32768.0, 32767.0) as i16;
                let r = (chunk[1] as f32 * g).clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&l.to_le_bytes());
                out.extend_from_slice(&r.to_le_bytes());
            }
            self.prev_l = stereo_in[(in_frames - 1) * 2];
            self.prev_r = stereo_in[(in_frames - 1) * 2 + 1];
            return out;
        }

        let step = (self.in_rate as f64) / (self.out_rate as f64);
        let est_out = ((in_frames as f64) / step).ceil() as usize + 2;
        let mut out = Vec::with_capacity(est_out * 4);

        while self.phase < in_frames as f64 {
            let idx = self.phase.floor() as isize;
            let frac = (self.phase - idx as f64) as f32;

            let (l0, r0) = if idx <= 0 {
                (self.prev_l as f32, self.prev_r as f32)
            } else {
                let i = ((idx - 1) as usize).min(in_frames - 1);
                (stereo_in[i * 2] as f32, stereo_in[i * 2 + 1] as f32)
            };
            let i1 = (idx.max(0) as usize).min(in_frames - 1);
            let (l1, r1) = (stereo_in[i1 * 2] as f32, stereo_in[i1 * 2 + 1] as f32);

            let l = ((l0 + (l1 - l0) * frac) * g).clamp(-32768.0, 32767.0) as i16;
            let r = ((r0 + (r1 - r0) * frac) * g).clamp(-32768.0, 32767.0) as i16;
            out.extend_from_slice(&l.to_le_bytes());
            out.extend_from_slice(&r.to_le_bytes());

            self.phase += step;
        }

        self.phase -= in_frames as f64;
        self.prev_l = stereo_in[(in_frames - 1) * 2];
        self.prev_r = stereo_in[(in_frames - 1) * 2 + 1];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stable_seed_is_deterministic() {
        let s1 = derive_stable_seed("Mobile Mirror");
        let s2 = derive_stable_seed("Mobile Mirror");
        let s3 = derive_stable_seed("Other Device");
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_aac_eld_asc_and_linear_resampler() {
        assert_eq!(build_aac_eld_asc(44_100, 2, 480), [0xF8, 0xE8, 0x50, 0x00]);

        let mut rs = LinearStereoResampler::new(44_100, 48_000);
        let input = vec![1000i16; 441 * 2]; // 10ms at 44.1 kHz
        let bytes = rs.resample_stereo_s16_to_bytes(&input, 1.0);
        // 10ms at 48 kHz = ~480 stereo frames = ~1920 bytes
        let out_frames = bytes.len() / 4;
        assert!((478..=482).contains(&out_frames), "got {out_frames} frames");
    }

    #[test]
    fn test_airplay_server_starts_and_stops_cleanly() {
        let server = AirPlayServer::start("Mobile Mirror Test").expect("start AirPlayServer");
        assert!(server.port() > 0);
        assert!(server.try_accept_session().is_none());
        server.stop();
    }

    #[test]
    fn test_airplay_aac_eld_decoder_initializes() {
        let cfg = AudioConfig {
            codec: AudioCodec::AacEld,
            sample_rate: 44_100,
            channels: 2,
            samples_per_frame: 480,
        };
        let dec = AirPlayAudioDecoder::new(cfg);
        assert!(dec.is_ok(), "FFmpeg AAC-ELD decoder should initialize: {:?}", dec.err());
    }
}
