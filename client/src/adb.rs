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
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .expect("Failed to spawn usb-auto-connector thread");

        Self {
            rx,
            running,
            auto_connect,
            manual_trigger,
            quality,
            worker: Some(worker),
            last_headline: initial_headline,
            last_subtext: initial_subtext,
            last_is_connecting: false,
            last_is_warning: false,
            last_device_serial: None,
        }
    }

    /// Alias for [`UsbAutoConnector::new`].
    pub fn start(port: u16) -> Self {
        Self::new(port)
    }

    /// Updates the encoder quality configuration for the next USB connection.
    pub fn set_quality(&mut self, q: StreamQualityConfig) {
        if let Ok(mut guard) = self.quality.lock() {
            *guard = q;
        }
    }

    /// Enables or disables automatic USB connection when a phone is plugged in.
    pub fn set_auto_connect(&mut self, enabled: bool) {
        self.auto_connect.store(enabled, Ordering::SeqCst);
    }

    /// Immediately wakes the background worker and attempts to start mirroring right now.
    pub fn trigger_connect_now(&mut self) {
        self.manual_trigger.store(true, Ordering::SeqCst);
    }

    /// Polls for the latest [`UsbStatus`] in 0 microseconds without ever blocking the UI thread.
    /// Drains intermediate status updates while guaranteeing [`UsbStatus::Connected`] is never skipped.
    pub fn poll_status(&mut self) -> Option<UsbStatus> {
        let mut latest = None;
        while let Ok(status) = self.rx.try_recv() {
            match &status {
                UsbStatus::WaitingForUsb {
                    headline,
                    subtext,
                    is_warning,
                    device_serial,
                } => {
                    self.last_headline = headline.clone();
                    self.last_subtext = subtext.clone();
                    self.last_is_connecting = false;
                    self.last_is_warning = *is_warning;
                    self.last_device_serial = device_serial.clone();
                }
                UsbStatus::Connecting { headline, subtext } => {
                    self.last_headline = headline.clone();
                    self.last_subtext = subtext.clone();
                    self.last_is_connecting = true;
                    self.last_is_warning = false;
                }
                UsbStatus::Connected(_, _) => {
                    self.last_headline = "Connected".to_string();
                    self.last_subtext.clear();
                    self.last_is_connecting = false;
                    self.last_is_warning = false;
                    return Some(status);
                }
            }
            latest = Some(status);
        }
        latest
    }

    pub fn headline(&self) -> &str {
        &self.last_headline
    }

    pub fn subtext(&self) -> &str {
        &self.last_subtext
    }

    pub fn is_connecting(&self) -> bool {
        self.last_is_connecting
    }

    pub fn is_warning(&self) -> bool {
        self.last_is_warning
    }

    pub fn device_serial(&self) -> Option<&str> {
        self.last_device_serial.as_deref()
    }

    /// Signals the background auto-connect thread to stop.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for UsbAutoConnector {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let _ = self.worker.take();
    }
}

/// RAII guard for an active USB ADB mirroring session.
/// Terminates the local `adb shell` child immediately on drop and dispatches `adb forward --remove`
/// plus remote `app_process` cleanup on a detached background thread so the UI thread never stalls.
pub struct AdbSessionGuard {
    adb_bin: PathBuf,
    serial: String,
    port: u16,
    daemon_child: Option<Child>,
    cleanup_done: bool,
}

impl AdbSessionGuard {
    /// Executes the full USB ADB mirroring pipeline and returns the connected [`StreamReader`]
    /// alongside its RAII [`AdbSessionGuard`]:
    /// 1. Locate `adb` & verify an authorized physical USB Android device is attached.
    /// 2. Locate (or build) `server/server.jar` and push it to `/data/local/tmp/server.jar`.
    /// 3. Kill any lingering `com.mirror.Server` process FIRST, wait 150ms, clear old forward,
    ///    and establish `adb -s <serial> forward tcp:<port> tcp:<port>`.
    /// 4. Spawn `com.mirror.Server` via `app_process` (`12 Mbps CBR`, `60 FPS`).
    /// 5. Connect `StreamReader` to `127.0.0.1:<port>`, monitoring `daemon_child.try_wait()` on
    ///    every retry up to 8 seconds so early server failures surface immediately.
    pub fn start_wired_pipeline<F>(
        port: u16,
        on_status: F,
    ) -> Result<(StreamReader<TcpStream>, Self), String>
    where
        F: FnMut(&str),
    {
        Self::start_wired_pipeline_internal(port, None, on_status).map_err(|e| e.to_string())
    }

    /// Same as [`AdbSessionGuard::start_wired_pipeline`], preserving structured [`UsbCheckError`] variants.
    pub fn start_wired_pipeline_checked<F>(
        port: u16,
        on_status: F,
    ) -> Result<(StreamReader<TcpStream>, Self), UsbCheckError>
    where
        F: FnMut(&str),
    {
        Self::start_wired_pipeline_internal(port, None, on_status)
    }

    fn start_wired_pipeline_internal<F>(
        port: u16,
        running: Option<&AtomicBool>,
        on_status: F,
    ) -> Result<(StreamReader<TcpStream>, Self), UsbCheckError>
    where
        F: FnMut(&str),
    {
        Self::start_wired_pipeline_with_quality(
            port,
            StreamQualityConfig::default(),
            running,
            on_status,
        )
    }

    pub fn start_wired_pipeline_with_quality<F>(
        port: u16,
        quality: StreamQualityConfig,
        running: Option<&AtomicBool>,
        mut on_status: F,
    ) -> Result<(StreamReader<TcpStream>, Self), UsbCheckError>
    where
        F: FnMut(&str),
    {
        // Step 1: Locate `adb` & verify physical USB device
        on_status("Verifying USB connection\u{2026}");
        let adb_bin = locate_adb_binary()?;
        let serial = check_usb_device(&adb_bin)?;
        println!("[ADB] Authorized USB Android device ready: {}", serial);

        if is_cancelled(running) {
            return Err(UsbCheckError::Other("Connection cancelled".to_string()));
        }

        // Step 2: Locate (or build) `server/server.jar` and push to `/data/local/tmp/server.jar`
        on_status("Preparing screen capture engine\u{2026}");
        let server_jar = locate_or_build_server_jar().map_err(UsbCheckError::Other)?;
        println!(
            "[ADB] Pushing {} -> /data/local/tmp/server.jar on {}...",
            server_jar.display(),
            serial
        );
        let push_out = new_hidden_command(&adb_bin)
            .args([
                "-s",
                &serial,
                "push",
                &server_jar.to_string_lossy(),
                "/data/local/tmp/server.jar",
            ])
            .output()
            .map_err(|e| UsbCheckError::Other(format!("Failed to execute `adb push`: {}", e)))?;

        if !push_out.status.success() {
            let stderr = String::from_utf8_lossy(&push_out.stderr);
            let stdout = String::from_utf8_lossy(&push_out.stdout);
            let detail = if !stderr.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            };
            if let Err(dev_err) = check_usb_device(&adb_bin) {
                return Err(dev_err);
            }
            return Err(UsbCheckError::Other(format!(
                "`adb push server.jar` failed: {}",
                detail
            )));
        }

        if is_cancelled(running) {
            return Err(UsbCheckError::Other("Connection cancelled".to_string()));
        }

        // Step 3: Kill any lingering `com.mirror.Server` FIRST, wait 150ms, remove old port forward,
        // and establish a fresh `adb -s <serial> forward tcp:<port> tcp:<port>`.
        on_status("Opening high-speed USB video tunnel\u{2026}");
        kill_remote_server(&adb_bin, Some(&serial));
        thread::sleep(Duration::from_millis(150));

        let forward_spec = format!("tcp:{}", port);
        let _ = new_hidden_command(&adb_bin)
            .args(["-s", &serial, "forward", "--remove", &forward_spec])
            .output();

        let fwd_out = new_hidden_command(&adb_bin)
            .args(["-s", &serial, "forward", &forward_spec, &forward_spec])
            .output()
            .map_err(|e| UsbCheckError::Other(format!("Failed to execute `adb forward`: {}", e)))?;

        if !fwd_out.status.success() {
            let stderr = String::from_utf8_lossy(&fwd_out.stderr);
            if let Err(dev_err) = check_usb_device(&adb_bin) {
                return Err(dev_err);
            }
            return Err(UsbCheckError::Other(format!(
                "`adb forward tcp:{0} tcp:{0}` failed: {1}",
                port,
                stderr.trim()
            )));
        }

        // Step 4: Spawn `com.mirror.Server` via `app_process` with dynamic quality settings
        on_status("Starting low-latency hardware video stream over USB");
        let remote_cmd = format!(
            "CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server port={} max_size={} bitrate={} fps={} cbr={}",
            port, quality.max_size, quality.bitrate, quality.fps, quality.cbr
        );
        let mut child = new_hidden_command(&adb_bin)
            .args(["-s", &serial, "shell", &remote_cmd])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                UsbCheckError::Other(format!(
                    "Failed to spawn `app_process` server daemon: {}",
                    e
                ))
            })?;

        // Drain stdout & stderr continuously on lightweight background threads so OS pipes
        // never fill/block during multi-hour sessions while keeping recent lines for crash diagnostics.
        let recent_logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::with_capacity(16)));
        if let Some(stdout) = child.stdout.take() {
            let logs_ref = Arc::clone(&recent_logs);
            let _ = thread::Builder::new()
                .name("adb-server-stdout".to_string())
                .spawn(move || {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines().map_while(Result::ok) {
                        println!("{}", line);
                        push_log_line(&logs_ref, line);
                    }
                });
        }
        if let Some(stderr) = child.stderr.take() {
            let logs_ref = Arc::clone(&recent_logs);
            let _ = thread::Builder::new()
                .name("adb-server-stderr".to_string())
                .spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        eprintln!("{}", line);
                        push_log_line(&logs_ref, line);
                    }
                });
        }

        // Wrap immediately in `AdbSessionGuard` so any early return or cancellation automatically
