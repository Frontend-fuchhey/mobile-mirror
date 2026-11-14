// [Mobile Mirror] Progressive implementation phase 1/6
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
