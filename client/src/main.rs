// [Mobile Mirror] Progressive implementation phase 3/6
pub mod adb;
pub mod decoder;
pub mod ffi;
pub mod muxer;
pub mod protocol;
pub mod renderer;
pub mod settings;
pub mod splitter;
pub mod ui_canvas;

use crate::adb::{AdbSessionGuard, UsbAutoConnector, UsbStatus};
use crate::decoder::HwDecoder;
use crate::muxer::NativeIsoMp4Muxer;
use crate::protocol::{ControlWriter, NalPacket, StreamHeader, StreamReader};
use crate::renderer::{
    DashboardViewState, Sdl3AudioPlayer, Sdl3Renderer, UiAction, DASHBOARD_HEIGHT, DASHBOARD_WIDTH,
};
use crate::settings::{
    open_folder_in_explorer, open_media_file, reveal_file_in_explorer, AppSettings, HwBackendPref,
    NavTab,
};
use crate::splitter::NalSplitter;

use std::env;
use std::net::TcpStream;
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
            reader,
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
        let (reader, _adb_guard) = match run_startup_ui(
            &mut renderer,
            &config,
            &mut settings,
            &mut last_saved_recording,
            force_immediate_connect,
        ) {
            Some(pair) => pair,
            None => {
                println!("[Mobile Mirror] Closed.");
                return;
            }
        };
        force_immediate_connect = false;

        let outcome = run_streaming_session(
            reader,
            Some(&mut renderer),
            &config,
            &mut settings,
            &mut last_saved_recording,
        );
        // Dropping `_adb_guard` deterministically cleans up `adb forward` and `app_process`.
        drop(_adb_guard);

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

/// Runs the 60 FPS Bento Dashboard UI while `UsbAutoConnector` probes and initializes
/// the Android USB session asynchronously in the background.
fn run_startup_ui(
    renderer: &mut Sdl3Renderer,
    config: &ClientConfig,
    settings: &mut AppSettings,
    last_saved_recording: &mut Option<String>,
    force_immediate_connect: bool,
) -> Option<(StreamReader<TcpStream>, AdbSessionGuard)> {
    let mut connector = UsbAutoConnector::new_with_config(
        config.port,
        settings.to_stream_quality(),
        settings.auto_connect_usb || force_immediate_connect,
    );
    if force_immediate_connect {
        connector.trigger_connect_now();
    }

    loop {
        let actions = renderer.poll_dashboard_actions();
        for action in actions {
            match action {
                UiAction::Quit => {
                    connector.stop();
                    return None;
                }
                UiAction::MinimizeWindow => renderer.minimize_window(),
                UiAction::MaximizeRestoreWindow => renderer.toggle_maximize_window(),
                UiAction::StartMirroring => {
                    renderer.active_tab = NavTab::Home;
                    settings.fullscreen_mirror = true;
                    settings.auto_connect_usb = true;
                    settings.save(&config.record_dir);
                    connector.set_auto_connect(true);
                    connector.set_quality(settings.to_stream_quality());
                    connector.trigger_connect_now();
                    if connector.device_serial().is_none() {
                        renderer.show_toast(
                            "Scanning USB... Connect your Android phone with USB debugging enabled",
                        );
                    } else {
                        renderer.show_toast("Starting USB screen mirroring...");
                    }
                }
                UiAction::StopMirroring => {
                    settings.auto_connect_usb = false;
                    connector.set_auto_connect(false);
                }
                UiAction::ToggleRecord => {
                    settings.auto_record = !settings.auto_record;
                    settings.save(&config.record_dir);
                    if settings.auto_record {
                        settings.auto_connect_usb = true;
                        connector.set_auto_connect(true);
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
                    if tab == NavTab::Recordings || tab == NavTab::About {
                        renderer.refresh_recordings_if_needed(&config.record_dir, true);
                    }
                }
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
                    connector.set_auto_connect(settings.auto_connect_usb);
                    renderer.show_toast(if settings.auto_connect_usb {
                        "Auto-Connect on USB Plug: ON"
                    } else {
                        "Auto-Connect on USB Plug: OFF"
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
                    connector.set_auto_connect(true);
                    renderer.show_toast("Settings reset to defaults");
                }
            }
        }

        if let Some(UsbStatus::Connected(reader, adb_guard)) = connector.poll_status() {
            renderer.active_tab = NavTab::Home;
            settings.fullscreen_mirror = true;
            return Some((reader, adb_guard));
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
            headline: connector.headline(),
            subtext: connector.subtext(),
            is_connecting: connector.is_connecting(),
            is_warning: connector.is_warning(),
            is_mirroring: false,
            device_serial: connector.device_serial(),
            is_recording: false,
            rec_elapsed_secs: 0,
            last_saved_recording: last_saved_recording.as_deref(),
            stream_width: 0,
            stream_height: 0,
            fps: settings.target_fps,
            recordings: &recs_snapshot,
            status_toast: toast_msg.as_deref(),
        };
        renderer.render_dashboard(&view_state);

        thread::sleep(Duration::from_millis(16));
    }
}

fn run_streaming_session(
    mut reader: StreamReader<TcpStream>,
    existing_renderer: Option<&mut Sdl3Renderer>,
    config: &ClientConfig,
    settings: &mut AppSettings,
    last_saved_recording: &mut Option<String>,
) -> SessionOutcome {
    let header = reader.header();
    let control_writer = reader.take_control_writer();
    println!(
        "[Mobile Mirror] Live USB stream active: {}x{} (codec=0x{:08x})",
        header.width, header.height, header.codec_id
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
                        println!("[Mobile Mirror] Android USB stream closed.");
                        break;
                    }
                    Err(e) => {
                        if running_net.load(Ordering::Relaxed) {
                            eprintln!("[Mobile Mirror] USB stream read ended: {}", e);
                        }
                        break;
                    }
                }
            }
            running_net.store(false, Ordering::SeqCst);
        })
