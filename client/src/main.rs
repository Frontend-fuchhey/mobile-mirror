#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod adb;
pub mod airplay;
pub mod decoder;
pub mod device;
pub mod ffi;
pub mod ios_usb;
pub mod muxer;
pub mod protocol;
pub mod renderer;
pub mod settings;
pub mod splitter;
pub mod thumbnail;
pub mod ui_canvas;

use crate::decoder::HwDecoder;
use crate::device::{DeviceStatus, UnifiedDeviceConnector};
use crate::muxer::NativeIsoMp4Muxer;
use crate::protocol::{
    ControlWriter, DevicePlatform, NalPacket, PacketSource, StreamHeader, StreamReader,
};
use crate::renderer::{
    DashboardViewState, Sdl3AudioPlayer, Sdl3Renderer, UiAction, DASHBOARD_HEIGHT, DASHBOARD_WIDTH,
};
use crate::settings::{
    open_folder_in_explorer, open_media_file, reveal_file_in_explorer, AppSettings, HwBackendPref,
    NavTab,
};
use crate::splitter::NalSplitter;

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    /// Default automatic USB detection & Bento Dashboard UI.
    AutoUsb,
    /// Direct socket connection (`--addr` / `--connect` / `--no-display`).
    DirectConnect,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub addr: String,
    pub port: u16,
    pub startup_mode: StartupMode,
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
            port: 12345,
            startup_mode: StartupMode::AutoUsb,
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
        Self::parse_from(env::args().skip(1))
    }

    pub fn parse_from<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cfg = Self::default();
        let mut args = iter.into_iter();
        while let Some(raw) = args.next() {
            let arg = raw.as_ref();
            match arg {
                "--wired" | "--usb" | "--auto" | "--ui" => {
                    cfg.startup_mode = StartupMode::AutoUsb;
                }
                "--mode" => {
                    if let Some(m) = args.next() {
                        match m.as_ref().to_ascii_lowercase().as_str() {
                            "direct" | "connect" => cfg.startup_mode = StartupMode::DirectConnect,
                            _ => cfg.startup_mode = StartupMode::AutoUsb,
                        }
                    }
                }
                "--addr" | "-a" | "--connect" => {
                    if let Some(v) = args.next() {
                        let v_str = v.as_ref();
                        if let Some(colon) = v_str.rfind(':') {
                            if let Ok(p) = v_str[colon + 1..].parse::<u16>() {
                                cfg.port = p;
                            }
                        }
                        cfg.addr = v_str.to_string();
                        cfg.startup_mode = StartupMode::DirectConnect;
                    }
                }
                "--port" | "-p" => {
                    if let Some(v) = args.next() {
                        let v_str = v.as_ref();
                        if let Ok(p) = v_str.parse::<u16>() {
                            cfg.port = p;
                        }
                        cfg.addr = format!("127.0.0.1:{}", cfg.port);
                    }
                }
                "--record-dir" => {
                    if let Some(v) = args.next() {
                        cfg.record_dir = PathBuf::from(v.as_ref());
                    }
                }
                "--record" | "-r" => {
                    if let Some(v) = args.next() {
                        let p = PathBuf::from(v.as_ref());
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
                    cfg.startup_mode = StartupMode::DirectConnect;
                }
                "--hw" | "--hw-backend" => {
                    if let Some(v) = args.next() {
                        cfg.preferred_hw = Some(v.as_ref().to_string());
                    }
                }
                "--max-packets" => {
                    if let Some(v) = args.next() {
                        cfg.max_packets = v.as_ref().parse().ok();
                    }
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("[Mobile Mirror] Ignoring unrecognized option: {}", arg);
                }
            }
        }
        cfg
    }
}

fn print_usage() {
    println!(
        "Mobile Mirror - Sub-50ms Android USB Screen Mirror & Lossless MP4 Recorder\n\n\
         USAGE:\n\
         \x20 mobile-mirror-client [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20     --usb                  Launch Automatic USB Mirroring Mode (default)\n\
         \x20 -a, --addr <HOST:PORT>     Connect directly to an active server socket\n\
         \x20 -p, --port <PORT>          Local ADB forward TCP port (default: 12345)\n\
         \x20     --record-dir <DIR>     Output directory for MP4 recordings (default: recordings/)\n\
         \x20 -r, --record <PATH>        Output directory or file path\n\
         \x20     --auto-record          Start recording immediately on connection\n\
         \x20     --mic                  Enable voice-isolated phone microphone on launch\n\
         \x20     --laptop-audio         Play phone system audio through PC speakers\n\
         \x20     --no-display           Headless recording mode (no SDL3 window)\n\
         \x20     --hw-backend <NAME>    Preferred HW decoder: nvdec, d3d11va, dxva2\n\
         \x20 -h, --help                 Print this help message\n\n\
         CONTROLS:\n\
         \x20 Click [Start Recording] or press [R]  Start / Stop lossless MP4 recording\n\
         \x20 Click [Mic]             or press [M]  Toggle phone voice-isolated microphone\n\
         \x20 Press [F11]                           Toggle Fullscreen Mirror View\n\
         \x20 Press [F]                             Toggle Compact Window / Dashboard Mirror View\n\
         \x20 Press [Q] or [Esc]                    Exit Fullscreen / Quit Mobile Mirror"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionOutcome {
    /// User explicitly closed the window or pressed `Q` / `Esc`.
    UserQuit,
    /// User clicked `[Stop]` to disconnect mirroring and return to the Home Hero view.
    UserStoppedStream,
    /// User changed `Mirror Quality` (`Auto` / `HD` / `FHD`) or encoder bitrate/FPS while live;
    /// immediately reconnects with the updated encoder configuration.
    RestartForQualityChange,
    /// Stream ended (`EOF` or USB cable unplugged) while window remained open.
    Disconnected,
}

fn main() {
    let config = ClientConfig::parse_args();
    let mut settings = AppSettings::load(&config.record_dir);
    if config.auto_record {
        settings.auto_record = true;
    }
    if config.enable_mic {
        settings.mic_enabled = true;
    }
    if config.laptop_audio {
        settings.pc_audio_enabled = true;
    }
    if let Some(ref hw) = config.preferred_hw {
        settings.hw_backend = HwBackendPref::from_opt_str(Some(hw));
    }

    // Headless or explicit direct-connect mode
    if !config.enable_display || config.startup_mode == StartupMode::DirectConnect {
        println!(
            "[Mobile Mirror] Connecting directly to Android stream at {}...",
            config.addr
        );
        let reader = match StreamReader::connect(&config.addr) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "[Mobile Mirror] Failed to connect to {}: {}\n\
                     Hint: Launch without `--addr` to start Mobile Mirror with automatic USB connection.",
                    config.addr, e
                );
                std::process::exit(1);
            }
        };
        let mut last_saved = None;
        let _ = run_streaming_session(
            Box::new(reader),
            None,
            &config,
            &mut settings,
            &mut last_saved,
        );
        return;
    }

    // Create the SDL3 application window ("Mobile Mirror") at 1024x698 Bento Dashboard geometry
    let mut renderer = match Sdl3Renderer::new("Mobile Mirror", DASHBOARD_WIDTH, DASHBOARD_HEIGHT) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[Mobile Mirror ERROR] Failed to create SDL3 window: {}\n\
                 Ensure SDL3.dll is present in client/lib/.",
                e
            );
            std::process::exit(1);
        }
    };
    renderer.set_always_on_top(settings.always_on_top);
    renderer.refresh_recordings_if_needed(&config.record_dir, true);

    let mut last_saved_recording: Option<String> = None;
    let mut force_immediate_connect = false;

    loop {
        let source = match run_startup_ui(
            &mut renderer,
            &config,
            &mut settings,
            &mut last_saved_recording,
            force_immediate_connect,
        ) {
            Some(src) => src,
            None => {
                println!("[Mobile Mirror] Closed.");
                return;
            }
        };
        force_immediate_connect = false;

        let outcome = run_streaming_session(
            source,
            Some(&mut renderer),
            &config,
            &mut settings,
            &mut last_saved_recording,
        );

        if outcome == SessionOutcome::UserQuit || config.max_packets.is_some() {
            break;
        }

        renderer.reset_to_startup_geometry();
        renderer.refresh_recordings_if_needed(&config.record_dir, true);

        match outcome {
            SessionOutcome::RestartForQualityChange => {
                println!(
                    "[Mobile Mirror] Reconfiguring Android stream for {} ({} Mbps @ {} FPS)...",
                    settings.quality.label(),
                    settings.bitrate_mbps,
                    settings.target_fps
                );
                force_immediate_connect = true;
            }
            SessionOutcome::UserStoppedStream => {
                // Pause automatic connection until user clicks "Start Mirroring"
                settings.auto_connect_usb = false;
                renderer.show_toast("Mirroring stopped — click 'Start Mirroring' to reconnect");
            }
            SessionOutcome::Disconnected => {
                println!("[Mobile Mirror] USB stream ended — waiting for USB reconnection...");
            }
            SessionOutcome::UserQuit => break,
        }
    }
}

/// Runs the 60 FPS Bento Dashboard UI while `UnifiedDeviceConnector` probes and initializes
/// Android USB, iOS USB, and iOS AirPlay sessions asynchronously in the background.
fn run_startup_ui(
    renderer: &mut Sdl3Renderer,
    config: &ClientConfig,
    settings: &mut AppSettings,
    last_saved_recording: &mut Option<String>,
    force_immediate_connect: bool,
) -> Option<Box<dyn PacketSource>> {
    let mut connector = UnifiedDeviceConnector::new(
        config.port,
        settings.to_stream_quality(),
        settings.auto_connect_usb || force_immediate_connect,
        settings.airplay_enabled,
        "Mobile Mirror",
    );
    if force_immediate_connect {
        connector.trigger_connect_now();
    }

    loop {
        let actions = renderer.poll_dashboard_actions();
        for action in actions {
            match action {
                UiAction::Quit => {
                    return None;
                }
                UiAction::MinimizeWindow => renderer.minimize_window(),
                UiAction::MaximizeRestoreWindow => renderer.toggle_maximize_window(),
                UiAction::StartMirroring => {
                    renderer.active_tab = NavTab::Home;
                    settings.fullscreen_mirror = true;
                    settings.auto_connect_usb = true;
                    settings.save(&config.record_dir);
                    connector.set_auto_connect_usb(true);
                    connector.set_quality(settings.to_stream_quality());
                    connector.trigger_connect_now();
                    if connector.latest_device_serial().is_none() {
                        renderer.show_toast(
                            "Scanning USB (Android/iOS)... or mirror via AirPlay",
                        );
                    } else {
                        renderer.show_toast("Starting USB screen mirroring...");
                    }
                }
                UiAction::StopMirroring => {
                    settings.auto_connect_usb = false;
                    connector.set_auto_connect_usb(false);
                }
                UiAction::ToggleRecord => {
                    settings.auto_record = !settings.auto_record;
                    settings.save(&config.record_dir);
                    if settings.auto_record {
                        settings.auto_connect_usb = true;
                        connector.set_auto_connect_usb(true);
                        connector.trigger_connect_now();
                        renderer.show_toast(
                            "Auto-Record ON: Recording will start automatically when mirroring begins",
                        );
                    } else {
                        renderer.show_toast("Auto-Record OFF");
                    }
                }
                UiAction::ToggleMic => {
                    settings.mic_enabled = !settings.mic_enabled;
                    settings.save(&config.record_dir);
                    renderer.show_toast(if settings.mic_enabled {
                        "Phone Microphone capture: ON (Voice Isolation DSP)"
                    } else {
                        "Phone Microphone capture: OFF"
                    });
                }
                UiAction::SelectQuality(q) => {
                    settings.apply_quality_preset(q);
                    settings.save(&config.record_dir);
                    connector.set_quality(settings.to_stream_quality());
                    renderer.show_toast(format!(
                        "Mirror Quality set to {} ({} Mbps @ {} FPS)",
                        q.label(),
                        settings.bitrate_mbps,
                        settings.target_fps
                    ));
                }
                UiAction::SwitchTab(tab) => {
                    renderer.active_tab = tab;
                    if tab == NavTab::Recordings {
                        renderer.refresh_recordings_if_needed(&config.record_dir, true);
                    }
                }
                UiAction::SwitchSettingsSection(_) | UiAction::OpenHelpInSettings => {}
                UiAction::OpenRecordingsFolder => {
                    open_folder_in_explorer(&config.record_dir);
                    renderer.active_tab = NavTab::Recordings;
                    renderer.refresh_recordings_if_needed(&config.record_dir, true);
                    renderer.show_toast("Opened recordings/ folder");
                }
                UiAction::PlayRecording(path) => {
                    open_media_file(&path);
                    renderer.show_toast("Opening recording in media player...");
                }
                UiAction::RevealRecording(path) => {
                    reveal_file_in_explorer(&path);
                }
                UiAction::DeleteRecording(path) => {
                    if std::fs::remove_file(&path).is_ok() {
                        renderer.refresh_recordings_if_needed(&config.record_dir, true);
                        renderer.show_toast("Deleted recording");
                    }
                }
                UiAction::PrevRecordingsPage => {
                    renderer.recordings_page = renderer.recordings_page.saturating_sub(1);
                }
                UiAction::NextRecordingsPage => {
                    renderer.recordings_page = renderer.recordings_page.saturating_add(1);
                }
                UiAction::CycleFps => {
                    settings.cycle_fps();
                    settings.save(&config.record_dir);
                    connector.set_quality(settings.to_stream_quality());
                    renderer.show_toast(format!("Target frame rate set to {} FPS", settings.target_fps));
                }
                UiAction::CycleBitrate => {
                    settings.cycle_bitrate();
                    settings.save(&config.record_dir);
                    connector.set_quality(settings.to_stream_quality());
                    renderer.show_toast(format!(
                        "Video encoder bitrate set to {} Mbps",
                        settings.bitrate_mbps
                    ));
                }
                UiAction::ToggleCbr => {
                    settings.cbr_mode = !settings.cbr_mode;
                    settings.save(&config.record_dir);
                    connector.set_quality(settings.to_stream_quality());
                    renderer.show_toast(if settings.cbr_mode {
                        "Bitrate mode: Constant (CBR)"
                    } else {
                        "Bitrate mode: Variable (VBR)"
                    });
                }
                UiAction::TogglePcAudio => {
                    settings.pc_audio_enabled = !settings.pc_audio_enabled;
                    settings.save(&config.record_dir);
                    renderer.show_toast(if settings.pc_audio_enabled {
                        "PC Speaker Audio Playback: ON"
                    } else {
                        "PC Speaker Audio Playback: OFF"
                    });
                }
                UiAction::ToggleAutoConnect => {
                    settings.auto_connect_usb = !settings.auto_connect_usb;
                    settings.save(&config.record_dir);
                    connector.set_auto_connect_usb(settings.auto_connect_usb);
                    renderer.show_toast(if settings.auto_connect_usb {
                        "Auto-Connect on USB Plug: ON"
                    } else {
                        "Auto-Connect on USB Plug: OFF"
                    });
                }
                UiAction::ToggleAirPlay => {
                    settings.airplay_enabled = !settings.airplay_enabled;
                    settings.save(&config.record_dir);
                    connector.set_airplay_enabled(settings.airplay_enabled);
                    renderer.show_toast(if settings.airplay_enabled {
                        "AirPlay Receiver: ON (Select 'Mobile Mirror' on iPhone)"
                    } else {
                        "AirPlay Receiver: OFF"
                    });
                }
                UiAction::ToggleAutoRecord => {
                    settings.auto_record = !settings.auto_record;
                    settings.save(&config.record_dir);
                    renderer.show_toast(if settings.auto_record {
                        "Auto-Record on Mirror Start: ON"
                    } else {
                        "Auto-Record on Mirror Start: OFF"
                    });
                }
                UiAction::ToggleAlwaysOnTop => {
                    settings.always_on_top = !settings.always_on_top;
                    settings.save(&config.record_dir);
                    renderer.set_always_on_top(settings.always_on_top);
                    renderer.show_toast(if settings.always_on_top {
                        "Window pinned Always on Top"
                    } else {
                        "Always on Top disabled"
                    });
                }
                UiAction::CycleHwBackend => {
                    settings.hw_backend = settings.hw_backend.next();
                    settings.save(&config.record_dir);
                    renderer.show_toast(format!(
                        "Hardware decoder set to {}",
                        settings.hw_backend.label()
                    ));
                }
                UiAction::ToggleFullscreenMirror => {
                    settings.fullscreen_mirror = !settings.fullscreen_mirror;
                    settings.save(&config.record_dir);
                }
                UiAction::ToggleWindowFullscreen => {
                    renderer.toggle_window_fullscreen();
                }
                UiAction::ResetDefaultSettings => {
                    *settings = AppSettings::default();
                    settings.save(&config.record_dir);
                    renderer.set_always_on_top(false);
                    connector.set_quality(settings.to_stream_quality());
                    connector.set_auto_connect_usb(settings.auto_connect_usb);
                    connector.set_airplay_enabled(settings.airplay_enabled);
                    renderer.show_toast("Settings reset to defaults");
                }
            }
        }

        if let Some(DeviceStatus::Connected(source)) = connector.poll_status() {
            renderer.active_tab = NavTab::Home;
            settings.fullscreen_mirror = true;
            return Some(source);
        }

        renderer.refresh_recordings_if_needed(&config.record_dir, false);
        let toast_msg = renderer.active_toast().map(|s| s.to_string());
        let recs_snapshot = if renderer.active_tab == NavTab::Recordings {
            renderer.cached_recordings.clone()
        } else {
            Vec::new()
        };

        let view_state = DashboardViewState {
            active_tab: renderer.active_tab,
            settings,
            headline: connector.latest_headline(),
            subtext: connector.latest_subtext(),
            is_connecting: connector.latest_is_connecting(),
            is_warning: connector.latest_is_warning(),
            is_mirroring: false,
            device_serial: connector.latest_device_serial(),
            platform_label: connector.latest_platform().map(|p| p.label()),
            airplay_listening: connector.is_airplay_listening(),
            is_recording: false,
            rec_elapsed_secs: 0,
            last_saved_recording: last_saved_recording.as_deref(),
            stream_width: 0,
            stream_height: 0,
            fps: settings.target_fps,
            recordings: &recs_snapshot,
            status_toast: toast_msg.as_deref(),
            thumbnails: None,
            recordings_page: renderer.recordings_page,
        };
        renderer.render_dashboard(&view_state);

        thread::sleep(Duration::from_millis(16));
    }
}

fn run_streaming_session(
    mut source: Box<dyn PacketSource>,
    existing_renderer: Option<&mut Sdl3Renderer>,
    config: &ClientConfig,
    settings: &mut AppSettings,
    last_saved_recording: &mut Option<String>,
) -> SessionOutcome {
    let header = source.header();
    let platform = source.platform();
    let device_name = source.device_name().to_string();
    let control_writer = source.control_writer();
    let control_writer_shutdown = control_writer.clone();
    println!(
        "[Mobile Mirror] Live {} stream active ({}): {}x{} (codec=0x{:08x})",
        platform.label(),
        device_name,
        header.width,
        header.height,
        header.codec_id
    );

    let running = Arc::new(AtomicBool::new(true));

    // Path A & B zero-copy NAL splitter
    let (mut splitter, _unused_mux_rx, decoder_rx, metrics) =
        NalSplitter::new(false, config.enable_display);

    // Spawn dedicated network ingestion thread
    let running_net = Arc::clone(&running);
    let max_packets = config.max_packets;
    let net_handle = thread::Builder::new()
        .name("nal-ingest".to_string())
        .spawn(move || {
            let mut count = 0u64;
            while running_net.load(Ordering::Relaxed) {
                match source.read_packet() {
                    Ok(Some(packet)) => {
                        splitter.dispatch(Arc::new(packet));
                        count += 1;
                        if let Some(limit) = max_packets {
                            if count >= limit {
                                break;
                            }
                        }
                    }
                    Ok(None) | Err(_) => {
                        println!("[Mobile Mirror] {} stream closed.", source.platform().label());
                        break;
                    }
                }
            }
            running_net.store(false, Ordering::SeqCst);
            source.shutdown();
            drop(source);
        })
        .expect("Failed to spawn network ingest thread");

    let outcome = if let Some(rx) = decoder_rx {
        run_playback_loop(
            header,
            platform,
            &device_name,
            existing_renderer,
            &config.record_dir,
            settings,
            last_saved_recording,
            control_writer,
            rx,
            Arc::clone(&running),
        )
    } else {
        SessionOutcome::UserQuit
    };

    running.store(false, Ordering::SeqCst);
    control_writer_shutdown.shutdown();
    let _ = net_handle.join();

    println!(
        "[Mobile Mirror] Session summary — Packets: {}, Bytes: {}, Dropped: {}",
        metrics.packets_dispatched.load(Ordering::Relaxed),
        metrics.bytes_dispatched.load(Ordering::Relaxed),
        metrics.decoder_drops.load(Ordering::Relaxed),
    );

    outcome
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

#[allow(clippy::too_many_arguments)]
fn run_playback_loop(
    header: StreamHeader,
    platform: DevicePlatform,
    device_name: &str,
    existing_renderer: Option<&mut Sdl3Renderer>,
    record_dir: &Path,
    settings: &mut AppSettings,
    last_saved_recording: &mut Option<String>,
    mut control_writer: ControlWriter,
    rx: Receiver<Arc<NalPacket>>,
    running: Arc<AtomicBool>,
) -> SessionOutcome {
    let mic_active = Arc::new(AtomicBool::new(settings.mic_enabled));
    let mut decoder = match HwDecoder::new(settings.hw_backend.cli_value()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[Mobile Mirror ERROR] Failed to initialize video decoder: {}\n\
                 Ensure FFmpeg DLLs (avcodec-63.dll, avutil-61.dll, avformat-63.dll, swresample-7.dll) are in client/lib/.",
                e
            );
            return SessionOutcome::UserQuit;
        }
    };

    let mut owned_renderer: Option<Sdl3Renderer> = None;
    let renderer: &mut Sdl3Renderer = match existing_renderer {
        Some(r) => {
            r.set_title("Mobile Mirror");
            r
        }
        None => match Sdl3Renderer::new("Mobile Mirror", DASHBOARD_WIDTH, DASHBOARD_HEIGHT) {
            Ok(r) => owned_renderer.insert(r),
            Err(e) => {
                eprintln!(
                    "[Mobile Mirror ERROR] Failed to create SDL3 window: {}\n\
                     Ensure SDL3.dll is in client/lib/.",
                    e
                );
                return SessionOutcome::UserQuit;
            }
        },
    };

    // Dedicated asynchronous MP4 recorder thread so disk I/O never blocks 60 FPS presentation
    let (record_tx, record_rx) = std::sync::mpsc::sync_channel::<RecorderCommand>(2048);
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
                                eprintln!(
                                    "[Mobile Mirror Recorder] Failed to create MP4 file: {}",
                                    e
                                );
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
                                "\n\x1b[92;1m[Mobile Mirror] Recording saved: {}\x1b[0m",
                                path.display()
                            );
                        }
                    }
                    RecorderCommand::Quit => {
                        if let Some((muxer, path)) = active_muxer.take() {
                            let _ = muxer.finalize();
                            println!(
                                "\n\x1b[92;1m[Mobile Mirror] Recording saved: {}\x1b[0m",
                                path.display()
                            );
                        }
                        break;
                    }
                }
            }
        })
        .expect("Failed to spawn recorder worker thread");

    let mut audio_player = if settings.pc_audio_enabled {
        Sdl3AudioPlayer::new().ok()
    } else {
        None
    };

    let mut is_recording = false;
    if settings.mic_enabled {
        control_writer.set_mic_enabled(true);
    }
    let mut recording_start_time: Option<Instant> = None;
    let mut active_recording_name: Option<String> = None;
    let mut cached_config_packet: Option<Arc<NalPacket>> = None;
    let mut cached_audio_config_packet: Option<Arc<NalPacket>> = None;

    let mut current_width = header.width;
    let mut current_height = header.height;
    let mut fps_counter = 0u32;
    let mut current_fps = settings.target_fps;
    let mut last_second = Instant::now();
    let mut last_title_update = Instant::now();
    let mut last_window_title = String::from("Mobile Mirror");
    let mut last_idle_render = Instant::now();
    let mut last_rec_elapsed_secs = 0u64;
    let mut session_outcome = SessionOutcome::Disconnected;

    // Default live mirroring sessions to the compact phone-shaped window.
    // Users can still click `Dash` or press `[F]` during mirroring to inspect the dashboard.
    settings.fullscreen_mirror = true;

    if settings.auto_record {
        let now = chrono_timestamp();
        let file_name = format!("recording_{}.mp4", now);
        let path = record_dir.join(&file_name);
        println!("[Mobile Mirror] Auto-recording started: {}", path.display());
        control_writer.request_keyframe();
        let _ = record_tx.send(RecorderCommand::Start {
            path,
            width: current_width,
            height: current_height,
            cached_config: cached_config_packet.clone(),
            cached_audio_config: cached_audio_config_packet.clone(),
        });
        recording_start_time = Some(Instant::now());
        active_recording_name = Some(file_name);
        is_recording = true;
    }

    while running.load(Ordering::Relaxed) {
        let actions = renderer.poll_dashboard_actions();
        let mut ui_dirty = !actions.is_empty();
        for action in actions {
            match action {
                UiAction::Quit => {
                    session_outcome = SessionOutcome::UserQuit;
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                UiAction::MinimizeWindow => renderer.minimize_window(),
                UiAction::MaximizeRestoreWindow => renderer.toggle_maximize_window(),
                UiAction::StopMirroring => {
                    session_outcome = SessionOutcome::UserStoppedStream;
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                UiAction::StartMirroring => {
                    renderer.active_tab = NavTab::Home;
                    settings.fullscreen_mirror = true;
                }
                UiAction::ToggleMic => {
                    settings.mic_enabled = !settings.mic_enabled;
                    mic_active.store(settings.mic_enabled, Ordering::Relaxed);
                    settings.save(record_dir);
                    control_writer.set_mic_enabled(settings.mic_enabled);
                    renderer.show_toast(if settings.mic_enabled {
                        "Phone Microphone: ON (Voice Isolation Active)"
                    } else {
                        "Phone Microphone: OFF"
                    });
                }
                UiAction::ToggleRecord => {
                    if is_recording {
                        let _ = record_tx.send(RecorderCommand::Stop);
                        is_recording = false;
                        recording_start_time = None;
                        if let Some(name) = active_recording_name.take() {
                            *last_saved_recording = Some(format!("Saved {}", name));
                            renderer.show_toast(format!("Recording saved: {}", name));
                        }
                        renderer.set_title("Mobile Mirror");
                        renderer.refresh_recordings_if_needed(record_dir, true);
                    } else {
                        let now = chrono_timestamp();
                        let file_name = format!("recording_{}.mp4", now);
                        let path = record_dir.join(&file_name);
                        println!("[Mobile Mirror] Recording started: {}", path.display());
                        control_writer.request_keyframe();
                        let _ = record_tx.send(RecorderCommand::Start {
                            path,
                            width: current_width,
                            height: current_height,
                            cached_config: cached_config_packet.clone(),
                            cached_audio_config: cached_audio_config_packet.clone(),
                        });
                        recording_start_time = Some(Instant::now());
                        active_recording_name = Some(file_name.clone());
                        is_recording = true;
                        renderer.show_toast(format!("Recording started: {}", file_name));
                    }
                }
                UiAction::SelectQuality(q) => {
                    if settings.quality != q {
                        settings.apply_quality_preset(q);
                        settings.save(record_dir);
                        renderer.show_toast(format!(
                            "Applying {} quality ({} Mbps)...",
                            q.label(),
                            settings.bitrate_mbps
                        ));
                        session_outcome = SessionOutcome::RestartForQualityChange;
                        running.store(false, Ordering::SeqCst);
                        break;
                    }
                }
                UiAction::SwitchTab(tab) => {
                    renderer.active_tab = tab;
                    if tab == NavTab::Recordings {
                        renderer.refresh_recordings_if_needed(record_dir, true);
                    }
                }
                UiAction::SwitchSettingsSection(_) | UiAction::OpenHelpInSettings => {}
                UiAction::OpenRecordingsFolder => {
                    open_folder_in_explorer(record_dir);
                    renderer.active_tab = NavTab::Recordings;
                    renderer.refresh_recordings_if_needed(record_dir, true);
                }
                UiAction::PlayRecording(path) => open_media_file(&path),
                UiAction::RevealRecording(path) => reveal_file_in_explorer(&path),
                UiAction::DeleteRecording(path) => {
                    if std::fs::remove_file(&path).is_ok() {
                        renderer.refresh_recordings_if_needed(record_dir, true);
                        renderer.show_toast("Deleted recording");
                    }
                }
                UiAction::PrevRecordingsPage => {
                    renderer.recordings_page = renderer.recordings_page.saturating_sub(1);
                }
                UiAction::NextRecordingsPage => {
                    renderer.recordings_page = renderer.recordings_page.saturating_add(1);
                }
                UiAction::CycleFps => {
                    settings.cycle_fps();
                    settings.save(record_dir);
                    session_outcome = SessionOutcome::RestartForQualityChange;
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                UiAction::CycleBitrate => {
                    settings.cycle_bitrate();
                    settings.save(record_dir);
                    session_outcome = SessionOutcome::RestartForQualityChange;
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                UiAction::ToggleCbr => {
                    settings.cbr_mode = !settings.cbr_mode;
                    settings.save(record_dir);
                    session_outcome = SessionOutcome::RestartForQualityChange;
                    running.store(false, Ordering::SeqCst);
                    break;
                }
                UiAction::TogglePcAudio => {
                    settings.pc_audio_enabled = !settings.pc_audio_enabled;
                    settings.save(record_dir);
                    if settings.pc_audio_enabled {
                        audio_player = Sdl3AudioPlayer::new().ok();
                        renderer.show_toast("PC Speaker Audio Playback: ON");
                    } else {
                        audio_player = None;
                        renderer.show_toast("PC Speaker Audio Playback: OFF");
                    }
                }
                UiAction::ToggleAutoConnect => {
                    settings.auto_connect_usb = !settings.auto_connect_usb;
                    settings.save(record_dir);
                }
                UiAction::ToggleAirPlay => {
                    settings.airplay_enabled = !settings.airplay_enabled;
                    settings.save(record_dir);
                    renderer.show_toast(if settings.airplay_enabled {
                        "AirPlay Receiver: ON (takes effect when idle)"
                    } else {
                        "AirPlay Receiver: OFF"
                    });
                }
                UiAction::ToggleAutoRecord => {
                    settings.auto_record = !settings.auto_record;
                    settings.save(record_dir);
                }
                UiAction::ToggleAlwaysOnTop => {
                    settings.always_on_top = !settings.always_on_top;
                    settings.save(record_dir);
                    renderer.set_always_on_top(settings.always_on_top);
                }
                UiAction::CycleHwBackend => {
                    settings.hw_backend = settings.hw_backend.next();
                    settings.save(record_dir);
                    renderer.show_toast(format!(
                        "Decoder preference: {}",
                        settings.hw_backend.label()
                    ));
                }
                UiAction::ToggleFullscreenMirror => {
                    if renderer.is_window_fullscreen() {
                        renderer.set_window_fullscreen(false);
                    }
                    settings.fullscreen_mirror = !settings.fullscreen_mirror;
                    settings.save(record_dir);
                }
                UiAction::ToggleWindowFullscreen => {
                    if renderer.is_window_fullscreen() {
                        renderer.set_window_fullscreen(false);
                    } else {
                        settings.fullscreen_mirror = true;
                        renderer.set_window_fullscreen(true);
                    }
                }
                UiAction::ResetDefaultSettings => {
                    *settings = AppSettings::default();
                    settings.save(record_dir);
                    renderer.set_always_on_top(false);
                    renderer.show_toast("Settings restored to defaults");
                }
            }
        }

        if !running.load(Ordering::Relaxed) {
            break;
        }

        let rec_elapsed_secs = recording_start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if is_recording && rec_elapsed_secs != last_rec_elapsed_secs {
            last_rec_elapsed_secs = rec_elapsed_secs;
            ui_dirty = true;
        }

        if last_second.elapsed() >= Duration::from_secs(1) {
            if current_fps != fps_counter {
                current_fps = fps_counter;
                ui_dirty = true;
            }
            fps_counter = 0;
            last_second = Instant::now();
        }

        if last_title_update.elapsed() >= Duration::from_millis(500) {
            let title = if is_recording {
                let mins = rec_elapsed_secs / 60;
                let secs = rec_elapsed_secs % 60;
                format!("Mobile Mirror — Recording {:02}:{:02}", mins, secs)
            } else {
                "Mobile Mirror".to_string()
            };
            if title != last_window_title {
                renderer.set_title(&title);
                last_window_title = title;
            }
            last_title_update = Instant::now();
        }

        let first_packet = match rx.recv_timeout(Duration::from_millis(4)) {
            Ok(pkt) => pkt,
            Err(RecvTimeoutError::Timeout) => {
                // Only redraw on timeout when UI state or hover zone changed, or at a gentle 80ms keepalive
                // so idle redraws never compete with incoming 60/90/120 FPS video frames.
                let (mx, my) = renderer.mouse_position();
                let hover_changed = renderer.is_hovered_zone_changed(mx, my);
                if ui_dirty || hover_changed || last_idle_render.elapsed() >= Duration::from_millis(80) {
                    let toast_msg = renderer.active_toast().map(|s| s.to_string());
                    let recs_snapshot = if renderer.active_tab == NavTab::Recordings {
                        renderer.cached_recordings.clone()
                    } else {
                        Vec::new()
                    };
                    let view_state = DashboardViewState {
                        active_tab: renderer.active_tab,
                        settings,
                        headline: match platform {
                            DevicePlatform::AndroidUsb => "Connected via Android USB",
                            DevicePlatform::IosUsb => "Connected via iOS USB",
                            DevicePlatform::IosAirPlay => "Connected via AirPlay",
                        },
                        subtext: "Live hardware video stream active",
                        is_connecting: false,
                        is_warning: false,
                        is_mirroring: true,
                        device_serial: Some(device_name),
                        platform_label: Some(platform.label()),
                        airplay_listening: settings.airplay_enabled,
                        is_recording,
                        rec_elapsed_secs,
                        last_saved_recording: last_saved_recording.as_deref(),
                        stream_width: current_width,
                        stream_height: current_height,
                        fps: current_fps,
                        recordings: &recs_snapshot,
                        status_toast: toast_msg.as_deref(),
                        thumbnails: None,
                        recordings_page: renderer.recordings_page,
                    };
                    renderer.render_dashboard(&view_state);
                    last_idle_render = Instant::now();
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };

        // Process queued packets while preserving smooth 1:1 frame presentation when on time (<22ms),
        // and draining up to 64 packets only when catching up from a real backlog (>=22ms).
        let mut decoded_any_frame = false;
        let mut batch_count = 0usize;
        let mut next_pkt = Some(first_packet);

        while let Some(packet) = next_pkt.take() {
            if packet.is_audio {
                if packet.is_config {
                    cached_audio_config_packet = Some(Arc::clone(&packet));
                    if is_recording {
                        let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
                    }
                } else if packet.is_keyframe {
                    if is_recording {
                        let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
                    }
                } else if let Some(ref mut player) = audio_player {
                    player.play(&packet.payload);
                }
            } else {
                if packet.is_config {
                    cached_config_packet = Some(Arc::clone(&packet));
                }
                if is_recording {
                    let _ = record_tx.try_send(RecorderCommand::Packet(Arc::clone(&packet)));
                }
                if let Ok(true) = decoder.decode_packet(&packet) {
                    fps_counter += 1;
                    decoded_any_frame = true;
                }
            }

            batch_count += 1;
            if batch_count >= 64 {
                break;
            }
            if decoded_any_frame && packet.received_at.elapsed() < Duration::from_millis(22) {
                break;
            }
            next_pkt = rx.try_recv().ok();
        }

        if decoded_any_frame {
            decoder.with_current_frame(|frame_view| {
                current_width = frame_view.width;
                current_height = frame_view.height;
                let _ = renderer.upload_video_frame(frame_view);
            });
        }

        if decoded_any_frame || ui_dirty {
            let toast_msg = renderer.active_toast().map(|s| s.to_string());
            let recs_snapshot = if renderer.active_tab == NavTab::Recordings {
                renderer.cached_recordings.clone()
            } else {
                Vec::new()
            };
            let view_state = DashboardViewState {
                active_tab: renderer.active_tab,
                settings,
                headline: match platform {
                    DevicePlatform::AndroidUsb => "Connected via Android USB",
                    DevicePlatform::IosUsb => "Connected via iOS USB",
                    DevicePlatform::IosAirPlay => "Connected via AirPlay",
                },
                subtext: "Live hardware video stream active",
                is_connecting: false,
                is_warning: false,
                is_mirroring: true,
                device_serial: Some(device_name),
                platform_label: Some(platform.label()),
                airplay_listening: settings.airplay_enabled,
                is_recording,
                rec_elapsed_secs,
                last_saved_recording: last_saved_recording.as_deref(),
                stream_width: current_width,
                stream_height: current_height,
                fps: current_fps,
                recordings: &recs_snapshot,
                status_toast: toast_msg.as_deref(),
                thumbnails: None,
                recordings_page: renderer.recordings_page,
            };
            renderer.render_dashboard(&view_state);
            last_idle_render = Instant::now();
        }
    }

    let _ = record_tx.send(RecorderCommand::Quit);
    let _ = recorder_handle.join();
    if is_recording {
        if let Some(name) = active_recording_name.take() {
            *last_saved_recording = Some(format!("Saved {}", name));
        }
    }

    session_outcome
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_auto_usb_mode() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.startup_mode, StartupMode::AutoUsb);
        assert_eq!(cfg.port, 12345);
        assert_eq!(cfg.addr, "127.0.0.1:12345");
        assert!(cfg.enable_display);
    }

    #[test]
    fn start_bat_flags_parse_cleanly() {
        let cfg = ClientConfig::parse_from([
            "--port",
            "12345",
            "--record-dir",
            "C:\\recordings",
            "--hw-backend",
            "nvdec",
        ]);
        assert_eq!(cfg.startup_mode, StartupMode::AutoUsb);
        assert_eq!(cfg.port, 12345);
        assert_eq!(cfg.record_dir, PathBuf::from("C:\\recordings"));
        assert_eq!(cfg.preferred_hw.as_deref(), Some("nvdec"));
    }

    #[test]
    fn direct_addr_switches_to_direct_connect_mode() {
        let cfg = ClientConfig::parse_from(["--addr", "127.0.0.1:23456"]);
        assert_eq!(cfg.startup_mode, StartupMode::DirectConnect);
        assert_eq!(cfg.port, 23456);
        assert_eq!(cfg.addr, "127.0.0.1:23456");
    }
}
