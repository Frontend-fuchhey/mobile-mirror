use crate::protocol::StreamReader;

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Creates a `Command` configured with `CREATE_NO_WINDOW` on Windows so background
/// `adb` probes, pushes, forwards, and shell processes never flash console windows.
fn new_hidden_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Structured diagnostics for physical USB device discovery via `adb devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbCheckError {
    /// No physical USB Android device is attached (`"Connect your phone via USB"`).
    NoDevice,
    /// Phone is plugged in via USB, but the RSA debugging prompt has not been accepted yet
    /// (`"Tap 'Allow USB debugging' on your phone"`).
    Unauthorized(String),
    /// Phone USB transport reported `offline` (`"USB connection is offline"`).
    Offline(String),
    /// `adb` executable was not found in `PATH` or standard Android SDK locations.
    AdbMissing(String),
    /// Any other transient ADB/push/forward/handshake error.
    Other(String),
}

impl UsbCheckError {
    pub fn headline(&self) -> String {
        match self {
            UsbCheckError::NoDevice => "Connect your phone via USB".to_string(),
            UsbCheckError::Unauthorized(_) => {
                "Tap 'Allow USB debugging' on your phone".to_string()
            }
            UsbCheckError::Offline(_) => "USB connection is offline".to_string(),
            UsbCheckError::AdbMissing(_) => "Android Debug Bridge not found".to_string(),
            UsbCheckError::Other(_) => "Reconnecting to your phone\u{2026}".to_string(),
        }
    }

    pub fn subtext(&self) -> String {
        match self {
            UsbCheckError::NoDevice => {
                "Plug in your Android phone with USB debugging enabled".to_string()
            }
            UsbCheckError::Unauthorized(_) => {
                "Unlock your phone and tap Allow on the USB prompt".to_string()
            }
            UsbCheckError::Offline(_) => {
                "Unplug and reconnect your phone's USB cable".to_string()
            }
            UsbCheckError::AdbMissing(msg) => msg.clone(),
            UsbCheckError::Other(_) => {
                "Check your USB cable — retrying automatically".to_string()
            }
        }
    }

    pub fn is_warning(&self) -> bool {
        !matches!(self, UsbCheckError::NoDevice)
    }
}

impl fmt::Display for UsbCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsbCheckError::NoDevice => write!(
                f,
                "Connect your phone via USB (Plug in your Android phone with USB debugging enabled)."
            ),
            UsbCheckError::Unauthorized(serial) => write!(
                f,
                "Tap 'Allow USB debugging' on your phone (device {} is unauthorized).",
                serial
            ),
            UsbCheckError::Offline(serial) => write!(
                f,
                "USB connection is offline — unplug and reconnect your phone's USB cable (device {}).",
                serial
            ),
            UsbCheckError::AdbMissing(msg) => write!(f, "{}", msg),
            UsbCheckError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for UsbCheckError {}

/// Real-time encoder quality parameters passed to `com.mirror.Server` on Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamQualityConfig {
    pub max_size: u32,
    pub bitrate: u32,
    pub fps: u32,
    pub cbr: bool,
}

impl Default for StreamQualityConfig {
    fn default() -> Self {
        Self {
            max_size: 0,
            bitrate: 24_000_000,
            fps: 60,
            cbr: true,
        }
    }
}

/// Real-time non-blocking state updates published by [`UsbAutoConnector`] to the 60 FPS SDL3 UI thread.
pub enum UsbStatus {
    WaitingForUsb {
        headline: String,
        subtext: String,
        is_warning: bool,
        device_serial: Option<String>,
    },
    Connecting {
        headline: String,
        subtext: String,
    },
    Connected(StreamReader<TcpStream>, AdbSessionGuard),
}

/// Non-blocking background worker (`"usb-auto-connector"`) that continuously watches for a
/// USB-connected Android device, automatically deploys `server.jar`, establishes the `tcp:12345`
/// tunnel, starts `com.mirror.Server`, and hands the live [`StreamReader`] to the SDL3 UI thread
/// with zero main-thread blocking.
pub struct UsbAutoConnector {
    rx: Receiver<UsbStatus>,
    running: Arc<AtomicBool>,
    auto_connect: Arc<AtomicBool>,
    manual_trigger: Arc<AtomicBool>,
    quality: Arc<Mutex<StreamQualityConfig>>,
    worker: Option<JoinHandle<()>>,
    last_headline: String,
    last_subtext: String,
    last_is_connecting: bool,
    last_is_warning: bool,
    last_device_serial: Option<String>,
}

impl UsbAutoConnector {
    /// Spawns the `"usb-auto-connector"` background thread for the specified TCP stream port.
    pub fn new(port: u16) -> Self {
        Self::new_with_config(port, StreamQualityConfig::default(), true)
    }

    /// Spawns `"usb-auto-connector"` with custom initial quality and auto-connect preference.
    pub fn new_with_config(
        port: u16,
        initial_quality: StreamQualityConfig,
        initial_auto_connect: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_bg = Arc::clone(&running);
        let auto_connect = Arc::new(AtomicBool::new(initial_auto_connect));
        let auto_connect_bg = Arc::clone(&auto_connect);
        let manual_trigger = Arc::new(AtomicBool::new(false));
        let manual_trigger_bg = Arc::clone(&manual_trigger);
        let quality = Arc::new(Mutex::new(initial_quality));
        let quality_bg = Arc::clone(&quality);

        let initial_headline = "Connect your phone via USB".to_string();
        let initial_subtext =
            "Make sure USB debugging is enabled on your Android device.".to_string();

        let _ = tx.send(UsbStatus::WaitingForUsb {
            headline: initial_headline.clone(),
            subtext: initial_subtext.clone(),
            is_warning: false,
            device_serial: None,
        });

        let worker = thread::Builder::new()
            .name("usb-auto-connector".to_string())
            .spawn(move || {
                let mut last_waiting: Option<(String, String, bool, Option<String>)> = None;

                while running_bg.load(Ordering::Relaxed) {
                    let triggered = manual_trigger_bg.swap(false, Ordering::SeqCst);
                    let should_connect = triggered || auto_connect_bg.load(Ordering::Relaxed);

                    // 1. Fast non-intrusive probe for `adb` + physical USB device
                    let probe = locate_adb_binary().and_then(|adb| check_usb_device(&adb));

                    match probe {
                        Ok(serial) => {
                            if !should_connect {
                                let headline = format!("Device Ready ({})", serial);
                                let subtext =
                                    "USB device detected — click 'Start Mirroring' to begin."
                                        .to_string();
                                let current =
                                    (headline.clone(), subtext.clone(), false, Some(serial.clone()));
                                if last_waiting.as_ref() != Some(&current) {
                                    let _ = tx.send(UsbStatus::WaitingForUsb {
                                        headline,
                                        subtext,
                                        is_warning: false,
                                        device_serial: Some(serial),
                                    });
                                    last_waiting = Some(current);
                                }
                            } else {
                                let q = quality_bg
                                    .lock()
                                    .map(|g| *g)
                                    .unwrap_or_default();

                                let _ = tx.send(UsbStatus::Connecting {
                                    headline: "Connecting to Android phone\u{2026}".to_string(),
                                    subtext: "Starting low-latency hardware video stream over USB"
                                        .to_string(),
                                });

                                let pipeline_res =
                                    AdbSessionGuard::start_wired_pipeline_with_quality(
                                        port,
                                        q,
                                        Some(&running_bg),
                                        |step_msg| {
                                            let _ = tx.send(UsbStatus::Connecting {
                                                headline: "Connecting to Android phone\u{2026}"
                                                    .to_string(),
                                                subtext: step_msg.to_string(),
                                            });
                                        },
                                    );

                                if !running_bg.load(Ordering::Relaxed) {
                                    break;
                                }

                                match pipeline_res {
                                    Ok((reader, guard)) => {
                                        let _ = tx.send(UsbStatus::Connected(reader, guard));
                                        // Mission accomplished: exit auto-connect worker cleanly.
                                        break;
                                    }
                                    Err(err) => {
                                        eprintln!(
                                            "[USB Auto-Connect] Pipeline retry needed: {}",
                                            err
                                        );
                                        let headline = err.headline();
                                        let subtext = err.subtext();
                                        let is_warning = err.is_warning();
                                        let _ = tx.send(UsbStatus::WaitingForUsb {
                                            headline: headline.clone(),
                                            subtext: subtext.clone(),
                                            is_warning,
                                            device_serial: None,
                                        });
                                        last_waiting = Some((headline, subtext, is_warning, None));
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            let headline = err.headline();
                            let subtext = if matches!(err, UsbCheckError::NoDevice) {
                                "Make sure USB debugging is enabled on your Android device."
                                    .to_string()
                            } else {
                                err.subtext()
                            };
                            let is_warning = err.is_warning();
                            let current = (headline.clone(), subtext.clone(), is_warning, None);

                            if triggered || last_waiting.as_ref() != Some(&current) {
                                let _ = tx.send(UsbStatus::WaitingForUsb {
                                    headline,
                                    subtext,
                                    is_warning,
                                    device_serial: None,
                                });
                                last_waiting = Some(current);
                            }
                        }
                    }

                    // Sleep in responsive 100ms slices (~800ms total) so manual triggers and
                    // quitting respond within <100ms.
                    for _ in 0..8 {
                        if !running_bg.load(Ordering::Relaxed) {
                            return;
                        }
                        if manual_trigger_bg.load(Ordering::Relaxed) {
