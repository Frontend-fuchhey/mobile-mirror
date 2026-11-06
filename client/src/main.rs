pub mod decoder;
pub mod ffi;
pub mod muxer;
pub mod protocol;
pub mod renderer;
pub mod splitter;

use crate::decoder::HwDecoder;
use crate::muxer::NativeIsoMp4Muxer;
use crate::protocol::{ControlWriter, NalPacket, StreamHeader, StreamReader};
use crate::renderer::{Sdl3AudioPlayer, Sdl3Renderer};
use crate::splitter::NalSplitter;

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub addr: String,
    pub record_dir: PathBuf,
    pub auto_record: bool,
    pub enable_display: bool,
    pub enable_mic: bool,
    pub laptop_audio: bool,
    pub preferred_hw: Option<String>,
    pub max_packets: Option<u64>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:12345".to_string(),
            record_dir: PathBuf::from("recordings"),
            auto_record: false,
            enable_display: true,
            enable_mic: false,
            laptop_audio: false,
            preferred_hw: None,
            max_packets: None,
        }
    }
}

impl ClientConfig {
    pub fn parse_args() -> Self {
        let mut cfg = Self::default();
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--addr" | "-a" => {
                    if let Some(v) = args.next() {
                        cfg.addr = v;
                    }
                }
                "--port" | "-p" => {
                    if let Some(v) = args.next() {
                        cfg.addr = format!("127.0.0.1:{}", v);
                    }
                }
                "--record-dir" => {
                    if let Some(v) = args.next() {
                        cfg.record_dir = PathBuf::from(v);
                    }
                }
                "--record" | "-r" => {
                    if let Some(v) = args.next() {
                        let p = PathBuf::from(v);
                        if p.extension().is_some() {
                            if let Some(parent) = p.parent() {
                                if !parent.as_os_str().is_empty() {
                                    cfg.record_dir = parent.to_path_buf();
                                }
                            }
                        } else {
                            cfg.record_dir = p;
                        }
                    }
                    cfg.auto_record = false;
                }
                "--auto-record" => {
                    cfg.auto_record = true;
                }
                "--no-record" => {
                    cfg.auto_record = false;
                }
                "--mic" => {
                    cfg.enable_mic = true;
                }
                "--no-mic" => {
                    cfg.enable_mic = false;
                }
                "--laptop-audio" => {
                    cfg.laptop_audio = true;
                }
                "--no-display" | "--headless" => {
                    cfg.enable_display = false;
                }
                "--hw" | "--hw-backend" => {
                    if let Some(v) = args.next() {
                        cfg.preferred_hw = Some(v);
                    }
                }
                "--max-packets" => {
                    if let Some(v) = args.next() {
                        cfg.max_packets = v.parse().ok();
                    }
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("[Client] Ignoring unrecognized option: {}", arg);
                }
            }
        }
        cfg
    }
}

fn print_usage() {
    println!(
        "mobile-mirror-client - Sub-50ms Android Screen Mirror & Zero-Copy MP4 Recorder\n\n\
         USAGE:\n\
         \x20 mobile-mirror-client [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20 -a, --addr <HOST:PORT>     Forwarded ADB TCP address (default: 127.0.0.1:12345)\n\
         \x20 -p, --port <PORT>          Convenience option for 127.0.0.1:<PORT>\n\
         \x20 -r, --record <PATH>        Output directory or file (default: recordings/)\n\
         \x20     --auto-record          Start recording immediately on launch\n\
         \x20     --no-display           Headless recording mode (no SDL3 window)\n\
         \x20     --hw <BACKEND>         Preferred HW decoder: nvdec, dxva2, d3d11va\n\
         \x20 -h, --help                 Print this help message\n\n\
         CONTROLS INSIDE WINDOW:\n\
         \x20 [R]   Toggle Screen Recording ON / OFF\n\
         \x20 [M]   Toggle Phone Mic Voice Recording ON / OFF (Noise-Suppressed)\n\
         \x20 [Q]   Quit & Disconnect"
    );
}

fn main() {
    let config = ClientConfig::parse_args();
    println!(
        "[Client] Connecting to Android server at {}...",
        config.addr
    );

    let mut reader = match StreamReader::connect(&config.addr) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[Client] Failed to connect to {}: {}\n\
                 Hint: Ensure ADB port forwarding is active (`adb forward tcp:12345 tcp:12345`) \
                 and `com.mirror.Server` is running on the device.",
                config.addr, e
            );
            std::process::exit(1);
        }
    };

    let header = reader.header();
    let control_writer = reader.take_control_writer();
    println!(
        "[Client] Connected! Stream format: codec=0x{:08x}, resolution={}x{}, framed={}",
        header.codec_id, header.width, header.height, header.framed
    );
    println!(
        "[Client] Controls: Press [R] to Start/Stop Recording | Press [M] to Toggle Phone Mic (Voice-Only) | Press [Q] to Quit."
    );

    let running = Arc::new(AtomicBool::new(true));

    // Path A & B split
    let (mut splitter, _unused_mux_rx, decoder_rx, metrics) =
        NalSplitter::new(false, config.enable_display);

    // Spawn network ingestion thread
    let running_net = Arc::clone(&running);
    let max_packets = config.max_packets;
    let net_handle = thread::Builder::new()
        .name("nal-ingest".to_string())
        .spawn(move || {
            let mut count = 0u64;
            while running_net.load(Ordering::Relaxed) {
                match reader.next_packet() {
                    Ok(Some(packet)) => {
                        splitter.dispatch(packet);
                        count += 1;
                        if let Some(limit) = max_packets {
                            if count >= limit {
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        println!("[Client] Android server closed stream (EOF).");
                        break;
                    }
                    Err(e) => {
                        if running_net.load(Ordering::Relaxed) {
                            eprintln!("[Client] Network read error: {}", e);
                        }
                        break;
                    }
                }
            }
            running_net.store(false, Ordering::SeqCst);
        })
        .expect("Failed to spawn network ingest thread");

    if let Some(rx) = decoder_rx {
        run_playback_loop(
            header,
            config.preferred_hw.as_deref(),
            &config.record_dir,
            config.auto_record,
            config.enable_mic,
            config.laptop_audio,
            control_writer,
            rx,
            Arc::clone(&running),
        );
    } else {
        // Headless mode
        let _ = net_handle.join();
    }

    running.store(false, Ordering::SeqCst);

    println!(
        "[Client] Session ended. Packets: {}, Bytes: {}, Dropped: {}",
        metrics.packets_dispatched.load(Ordering::Relaxed),
        metrics.bytes_dispatched.load(Ordering::Relaxed),
        metrics.decoder_drops.load(Ordering::Relaxed),
    );
}

enum RecorderCommand {
    Start {
        path: PathBuf,
        width: u32,
        height: u32,
        cached_config: Option<Arc<NalPacket>>,
        cached_audio_config: Option<Arc<NalPacket>>,
    },
    Packet(Arc<NalPacket>),
    Stop,
    Quit,
}

fn run_playback_loop(
    header: StreamHeader,
    preferred_hw: Option<&str>,
    record_dir: &Path,
    auto_record: bool,
    initial_mic: bool,
    laptop_audio: bool,
    mut control_writer: ControlWriter,
    rx: Receiver<Arc<NalPacket>>,
    running: Arc<AtomicBool>,
) {
    let mut decoder = match HwDecoder::new(preferred_hw) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[Playback ERROR] Failed to initialize hardware video decoder: {}\n\
                 Ensure FFmpeg DLLs (avcodec-63.dll, avutil-61.dll, avformat-63.dll, swresample-7.dll) are in client/lib/.",
                e
            );
            return;
        }
    };

    let title = format!(
        "Mobile Mirror ({}x{}) | [R] Record | [Q] Quit",
        header.width, header.height
    );
    let mut renderer = match Sdl3Renderer::new(&title, header.width, header.height) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[Playback ERROR] Failed to create SDL3 mirror window: {}\n\
                 Ensure SDL3.dll is in client/lib/.",
                e
            );
            return;
        }
    };

    // Dedicated asynchronous background MP4 recorder thread
    // Isolates file I/O completely so mirroring presentation never drops a frame or freezes
    let (record_tx, record_rx) = std::sync::mpsc::sync_channel::<RecorderCommand>(512);
    let recorder_handle = thread::Builder::new()
        .name("mp4-recorder".to_string())
        .spawn(move || {
            let mut active_muxer: Option<(NativeIsoMp4Muxer, PathBuf)> = None;
            while let Ok(cmd) = record_rx.recv() {
                match cmd {
                    RecorderCommand::Start {
                        path,
                        width,
                        height,
                        cached_config,
                        cached_audio_config,
                    } => {
                        if let Some((m, _)) = active_muxer.take() {
                            let _ = m.finalize();
                        }
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match NativeIsoMp4Muxer::new(&path, width, height) {
                            Ok(mut muxer) => {
                                if let Some(cfg) = cached_config {
                                    let _ = muxer.write_packet(&cfg);
                                }
                                if let Some(acfg) = cached_audio_config {
                                    let _ = muxer.write_packet(&acfg);
                                }
                                active_muxer = Some((muxer, path));
                            }
                            Err(e) => {
                                eprintln!("[Recorder ERROR] Failed to create MP4 file: {}", e);
                            }
                        }
                    }
                    RecorderCommand::Packet(pkt) => {
                        if let Some((ref mut muxer, _)) = active_muxer {
                            let _ = muxer.write_packet(&pkt);
                        }
                    }
                    RecorderCommand::Stop => {
                        if let Some((muxer, path)) = active_muxer.take() {
                            let _ = muxer.finalize();
                            println!(
                                "\n\x1b[92;1m[RECORDING SAVED] Complete video saved to: {}\x1b[0m",
                                path.display()
                            );
                        }
                    }
                    RecorderCommand::Quit => {
                        if let Some((muxer, path)) = active_muxer.take() {
                            let _ = muxer.finalize();
                            println!(
                                "\n\x1b[92;1m[RECORDING SAVED] Complete video saved to: {}\x1b[0m",
                                path.display()
                            );
                        }
                        break;
                    }
                }
            }
        })
        .expect("Failed to spawn recorder worker thread");

    // Keep game/system sound playing on the phone by default (do not redirect to laptop speakers unless requested)
    let mut audio_player = if laptop_audio {
        match Sdl3AudioPlayer::new() {
            Ok(p) => {
                println!("\x1b[92;1m[Audio] Laptop speaker playback enabled\x1b[0m");
                Some(p)
            }
            Err(e) => {
                eprintln!("[Audio WARNING] Could not initialize laptop audio: {}", e);
                None
            }
        }
    } else {
        println!(
            "\x1b[92;1m[Audio] Phone speaker active (sound plays on phone & saves to recording | Press [M] to toggle Mic)\x1b[0m"
        );
        None
    };

    let mut is_recording = false;
    let mut is_mic_enabled = initial_mic;
    if is_mic_enabled {
        control_writer.set_mic_enabled(true);
        println!(
            "\x1b[96;1m[Mic] Phone Microphone Recording: ON (Voice-Isolation Active, Background Noise Suppressed)\x1b[0m"
        );
    }
    let mut recording_start_time: Option<Instant> = None;
    let mut cached_config_packet: Option<Arc<NalPacket>> = None;
    let mut cached_audio_config_packet: Option<Arc<NalPacket>> = None;

    let mut current_width = header.width;
    let mut current_height = header.height;
    let mut rendered_frames = 0u64;
    let mut fps_counter = 0u32;
    let mut current_fps = 60u32;
    let mut last_second = Instant::now();
    let mut last_title_update = Instant::now();

    // Start auto-recording if requested
    if auto_record {
        let now = chrono_timestamp();
        let filename = format!("recording_{}.mp4", now);
        let path = record_dir.join(filename);
        println!(
            "\n\x1b[91;1m[● RECORDING STARTED] Saving to: {} (Press 'R' to stop | Press 'M' to toggle Mic)\x1b[0m",
            path.display()
        );
        let _ = record_tx.send(RecorderCommand::Start {
            path,
            width: current_width,
            height: current_height,
            cached_config: None,
            cached_audio_config: None,
        });
        recording_start_time = Some(Instant::now());
        is_recording = true;
    }

    while running.load(Ordering::Relaxed) {
        // Poll user events (window close, hotkey 'R' to toggle record, 'M' to toggle mic, 'Q' to quit)
        let (should_run, toggle_record, toggle_mic) = renderer.poll_events();
        if !should_run {
            println!("[Playback] Window closed by user.");
            running.store(false, Ordering::SeqCst);
            break;
        }

        // Handle user pressing [M] to toggle voice-isolated phone microphone recording
        if toggle_mic {
            is_mic_enabled = !is_mic_enabled;
            control_writer.set_mic_enabled(is_mic_enabled);
            if is_mic_enabled {
                println!(
                    "\n\x1b[96;1m[🎤 MIC ON] Phone Voice Recording ENABLED (Voice-Only Isolation Active — Background Noise Suppressed)\x1b[0m"
                );
            } else {
                println!(
                    "\n\x1b[93;1m[🔇 MIC OFF] Phone Voice Recording DISABLED (Game/System Audio Only)\x1b[0m"
                );
            }
        }

        // Handle user pressing [R] to toggle recording
        if toggle_record {
            if is_recording {
                // Stop recording asynchronously
                let _ = record_tx.send(RecorderCommand::Stop);
                is_recording = false;
                recording_start_time = None;
            } else {
                // Start recording asynchronously
                let now = chrono_timestamp();
                let filename = format!("recording_{}.mp4", now);
                let path = record_dir.join(filename);
                println!(
                    "\n\x1b[91;1m[● RECORDING STARTED] Saving to: {} (Mic: {} | Press 'M' to toggle Mic | Press 'R' to stop)\x1b[0m",
                    path.display(),
                    if is_mic_enabled { "ON (Voice-Only)" } else { "OFF" }
                );
                let _ = record_tx.send(RecorderCommand::Start {
                    path,
                    width: current_width,
                    height: current_height,
                    cached_config: cached_config_packet.clone(),
                    cached_audio_config: cached_audio_config_packet.clone(),
                });
                recording_start_time = Some(Instant::now());
                is_recording = true;
            }
        }

        // Receive next NAL packet from stream
        let packet = match rx.recv_timeout(Duration::from_millis(8)) {
            Ok(pkt) => pkt,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        // Handle Audio packets (raw 48kHz Stereo PCM for live playback + AAC-LC for MP4 recording)
        if packet.is_audio {
            if packet.is_config {
                cached_audio_config_packet = Some(Arc::clone(&packet));
                if is_recording {
                    let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
                }
            } else if packet.is_keyframe {
                // Hardware-encoded AAC-LC packet for MP4 recording
                if is_recording {
                    let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
                }
            } else {
                // Raw 48kHz Stereo 16-bit PCM packet for zero-latency laptop speakers
                if let Some(ref mut player) = audio_player {
                    player.play(&packet.payload);
                }
            }
            continue; // Skip video decoder and renderer for audio packets
        }

        // Cache the latest SPS/PPS config packet
        if packet.is_config {
            cached_config_packet = Some(Arc::clone(&packet));
        }

        // Forward packet to asynchronous recorder thread (zero UI lag)
        if is_recording {
            let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
        }

        // Decode packet and present onto SDL3 mirror window
        let _ = decoder.decode_packet(&packet, |frame_view| {
            current_width = frame_view.width;
            current_height = frame_view.height;
            let _ = renderer.render_frame(&frame_view, is_recording);
            rendered_frames += 1;
            fps_counter += 1;
        });

        // Compute FPS
        if last_second.elapsed() >= Duration::from_secs(1) {
            current_fps = fps_counter;
            fps_counter = 0;
            last_second = Instant::now();
        }

        // Update Window Title with dynamic state
        if last_title_update.elapsed() >= Duration::from_millis(250) {
            let mic_badge = if is_mic_enabled { "Mic: ON (Voice)" } else { "Mic: OFF" };
            let title = if is_recording {
                let elapsed_secs = recording_start_time
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                format!(
                    "[● REC {:02}:{:02} | {}] Mobile Mirror ({}x{}) | {} FPS | [M] Mic | [R] Stop | [Q] Quit",
                    mins, secs, mic_badge, current_width, current_height, current_fps
                )
            } else {
                format!(
                    "[LIVE | {}] Mobile Mirror ({}x{}) | {} FPS | [M] Mic | [R] Record | [Q] Quit",
                    mic_badge, current_width, current_height, current_fps
                )
            };
            renderer.set_title(&title);
            last_title_update = Instant::now();
        }
    }

    // Cleanly signal the recorder thread to finalize and flush on exit
    let _ = record_tx.send(RecorderCommand::Quit);
    let _ = recorder_handle.join();
}

#[cfg(windows)]
#[allow(non_snake_case)]
#[repr(C)]
struct SYSTEMTIME {
    wYear: u16,
    wMonth: u16,
    wDayOfWeek: u16,
    wDay: u16,
    wHour: u16,
    wMinute: u16,
    wSecond: u16,
    wMilliseconds: u16,
}

#[cfg(windows)]
extern "system" {
    fn GetLocalTime(lpSystemTime: *mut SYSTEMTIME);
}

fn chrono_timestamp() -> String {
    #[cfg(windows)]
    {
        let mut st = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        unsafe {
            GetLocalTime(&mut st);
        }
        format!(
            "{:04}{:02}{:02}_{:02}{:02}{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        use std::time::SystemTime;
        let dur = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = dur.as_secs();
        let s = secs % 60;
        let m = (secs / 60) % 60;
        let h = (secs / 3600) % 24;
        let days = secs / 86400;
        format!("D{:05}_{:02}-{:02}-{:02}", days, h, m, s)
    }
}
