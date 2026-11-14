// [Mobile Mirror] Progressive implementation phase 2/6
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
