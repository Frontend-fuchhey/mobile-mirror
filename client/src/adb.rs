use crate::protocol::StreamReader;

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

static ADB_PIPELINE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static SESSION_GENERATION: AtomicU64 = AtomicU64::new(0);

fn pipeline_lock() -> &'static Mutex<()> {
    ADB_PIPELINE_LOCK.get_or_init(|| Mutex::new(()))
}

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
    quality: StreamQualityConfig,
    generation: u64,
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
        let generation = SESSION_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let _pipeline_guard = pipeline_lock().lock().unwrap_or_else(|e| e.into_inner());

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
        thread::sleep(Duration::from_millis(120));

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
        let (child, recent_logs, server_listening) =
            spawn_remote_server_process(&adb_bin, &serial, port, quality)?;

        // Wrap immediately in `AdbSessionGuard` so any early return or cancellation automatically
        // kills `daemon_child`, removes `adb forward`, and cleans up the remote `app_process`.
        let mut guard = Self {
            adb_bin: adb_bin.clone(),
            serial: serial.clone(),
            port,
            quality,
            generation,
            daemon_child: Some(child),
            cleanup_done: false,
        };

        // Step 5: Wait for `com.mirror.Server` to bind its local socket ("Listening on 127.0.0.1:")
        // so we never spam `adbd` with premature failed forward connections during ART VM startup.
        on_status("Synchronizing 60 FPS video stream\u{2026}");
        let listen_wait_deadline = Instant::now() + Duration::from_millis(2400);
        while Instant::now() < listen_wait_deadline && !server_listening.load(Ordering::Acquire) {
            if is_cancelled(running) {
                return Err(UsbCheckError::Other("Connection cancelled".to_string()));
            }
            if let Some(ref mut proc) = guard.daemon_child {
                if let Ok(Some(status)) = proc.try_wait() {
                    thread::sleep(Duration::from_millis(50));
                    if let Err(dev_err) = check_usb_device(&adb_bin) {
                        return Err(dev_err);
                    }
                    let tail = format_recent_logs(&recent_logs);
                    let msg = if tail.is_empty() {
                        format!("Android capture server exited early ({})", status)
                    } else {
                        format!("Android capture server exited early ({}): {}", status, tail)
                    };
                    return Err(UsbCheckError::Other(msg));
                }
            }
            thread::sleep(Duration::from_millis(25));
        }

        let sock_addr = SocketAddr::from(([127, 0, 0, 1], port));
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut last_conn_err = String::from("Timed out waiting for stream header");

        while Instant::now() < deadline {
            if is_cancelled(running) {
                return Err(UsbCheckError::Other("Connection cancelled".to_string()));
            }

            if let Some(ref mut proc) = guard.daemon_child {
                match proc.try_wait() {
                    Ok(Some(status)) => {
                        thread::sleep(Duration::from_millis(60));
                        if let Err(dev_err) = check_usb_device(&adb_bin) {
                            return Err(dev_err);
                        }
                        let tail = format_recent_logs(&recent_logs);
                        let msg = if tail.is_empty() {
                            format!("Android capture server exited early ({})", status)
                        } else {
                            format!("Android capture server exited early ({}): {}", status, tail)
                        };
                        return Err(UsbCheckError::Other(msg));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return Err(UsbCheckError::Other(format!(
                            "Failed monitoring `app_process` daemon: {}",
                            e
                        )));
                    }
                }
            }

            match TcpStream::connect_timeout(&sock_addr, Duration::from_millis(500)) {
                Ok(stream) => match StreamReader::from_tcp_stream(stream) {
                    Ok(reader) => {
                        println!(
                            "[ADB] Stream connected ({}x{}, framed={}) on 127.0.0.1:{}",
                            reader.header().width,
                            reader.header().height,
                            reader.header().framed,
                            port
                        );
                        return Ok((reader, guard));
                    }
                    Err(e) => {
                        last_conn_err = e.to_string();
                    }
                },
                Err(e) => {
                    last_conn_err = e.to_string();
                    if e.kind() == ErrorKind::ConnectionRefused {
                        let _ = new_hidden_command(&adb_bin)
                            .args(["-s", &serial, "forward", &forward_spec, &forward_spec])
                            .output();
                    }
                }
            }

            thread::sleep(Duration::from_millis(120));
        }

        if let Err(dev_err) = check_usb_device(&adb_bin) {
            return Err(dev_err);
        }

        let tail = format_recent_logs(&recent_logs);
        let detail = if tail.is_empty() {
            format!(
                "Timed out connecting to 127.0.0.1:{} ({})",
                port, last_conn_err
            )
        } else {
            format!(
                "Timed out connecting to 127.0.0.1:{} ({}) — {}",
                port, last_conn_err, tail
            )
        };
        Err(UsbCheckError::Other(detail))
    }

    /// Seamlessly reconnects the active USB mirroring stream in-place without closing the mirror
    /// window or interrupting an active recording, provided the physical USB device is still attached.
    pub fn try_reconnect_stream(
        &mut self,
        running: &AtomicBool,
    ) -> Result<StreamReader<TcpStream>, UsbCheckError> {
        let _pipeline_guard = pipeline_lock().lock().unwrap_or_else(|e| e.into_inner());

        // 1. Verify the physical USB device is still attached and authorized
        let current_serial = check_usb_device(&self.adb_bin)?;
        self.serial = current_serial;

        let forward_spec = format!("tcp:{}", self.port);
        let sock_addr = SocketAddr::from(([127, 0, 0, 1], self.port));

        // 2. Fast path: if `com.mirror.Server` is still alive on the phone (waiting in `acceptClient()`),
        // re-ensure `adb forward` and reconnect the TCP stream in <100ms without restarting `app_process`.
        if self.is_server_alive() {
            let _ = new_hidden_command(&self.adb_bin)
                .args(["-s", &self.serial, "forward", &forward_spec, &forward_spec])
                .output();

            for _ in 0..4 {
                if !running.load(Ordering::Relaxed) {
                    return Err(UsbCheckError::Other("Session stopped".to_string()));
                }
                if !self.is_server_alive() {
                    break;
                }
                if let Ok(stream) = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(400))
                {
                    if let Ok(reader) = StreamReader::from_tcp_stream(stream) {
                        println!(
                            "[ADB] Fast in-place USB stream recovery succeeded ({}x{})",
                            reader.header().width,
                            reader.header().height
                        );
                        return Ok(reader);
                    }
                }
                thread::sleep(Duration::from_millis(80));
            }
        }

        // 3. Full in-place server restart if `app_process` exited while the phone is still plugged in
        check_usb_device(&self.adb_bin)?;
        if let Some(ref mut child) = self.daemon_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.daemon_child = None;

        kill_remote_server(&self.adb_bin, Some(&self.serial));
        thread::sleep(Duration::from_millis(100));

        let _ = new_hidden_command(&self.adb_bin)
            .args(["-s", &self.serial, "forward", "--remove", &forward_spec])
            .output();
        let fwd_out = new_hidden_command(&self.adb_bin)
            .args(["-s", &self.serial, "forward", &forward_spec, &forward_spec])
            .output()
            .map_err(|e| UsbCheckError::Other(format!("Failed to execute `adb forward`: {}", e)))?;

        if !fwd_out.status.success() {
            check_usb_device(&self.adb_bin)?;
            return Err(UsbCheckError::Other("Failed to re-establish adb forward".to_string()));
        }

        let (child, _recent_logs, server_listening) =
            spawn_remote_server_process(&self.adb_bin, &self.serial, self.port, self.quality)?;
        self.daemon_child = Some(child);

        let listen_deadline = Instant::now() + Duration::from_millis(2200);
        while Instant::now() < listen_deadline && !server_listening.load(Ordering::Acquire) {
            if !running.load(Ordering::Relaxed) {
                return Err(UsbCheckError::Other("Session stopped".to_string()));
            }
            if !self.is_server_alive() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !running.load(Ordering::Relaxed) {
                return Err(UsbCheckError::Other("Session stopped".to_string()));
            }
            if !self.is_server_alive() {
                check_usb_device(&self.adb_bin)?;
                return Err(UsbCheckError::Other(
                    "Android server exited during in-place restart".to_string(),
                ));
            }
            if let Ok(stream) = TcpStream::connect_timeout(&sock_addr, Duration::from_millis(500)) {
                if let Ok(reader) = StreamReader::from_tcp_stream(stream) {
                    println!(
                        "[ADB] In-place server restart succeeded ({}x{})",
                        reader.header().width,
                        reader.header().height
                    );
                    return Ok(reader);
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        check_usb_device(&self.adb_bin)?;
        Err(UsbCheckError::Other(
            "Timed out reconnecting in-place USB stream".to_string(),
        ))
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_server_alive(&mut self) -> bool {
        match self.daemon_child {
            Some(ref mut child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Drop for AdbSessionGuard {
    fn drop(&mut self) {
        if self.cleanup_done {
            return;
        }
        self.cleanup_done = true;

        println!(
            "[ADB] Cleaning up USB ADB session (device {}, port {})...",
            self.serial, self.port
        );

        // 1. Terminate local `adb shell` process immediately (<1ms local OS operation)
        if let Some(ref mut child) = self.daemon_child {
            let _ = child.kill();
            let _ = child.wait();
        }

        // 2. Offload `adb forward --remove` and remote `app_process` cleanup onto a background
        // thread synchronized via `pipeline_lock()` + `SESSION_GENERATION` so dropping
        // `AdbSessionGuard` on the SDL3 UI thread never blocks AND never races with or kills
        // a newly started session!
        let adb_path = self.adb_bin.clone();
        let serial = self.serial.clone();
        let port = self.port;
        let generation = self.generation;

        let _ = thread::Builder::new()
            .name("adb-cleanup".to_string())
            .spawn(move || {
                let _lock = pipeline_lock().lock().unwrap_or_else(|e| e.into_inner());
                if SESSION_GENERATION.load(Ordering::SeqCst) != generation {
                    // A newer session has already started (and handles its own cleanup under `pipeline_lock`).
                    return;
                }
                let _ = new_hidden_command(&adb_path)
                    .args([
                        "-s",
                        &serial,
                        "forward",
                        "--remove",
                        &format!("tcp:{}", port),
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                kill_remote_server(&adb_path, Some(&serial));
            });
    }
}

fn spawn_remote_server_process(
    adb_bin: &Path,
    serial: &str,
    port: u16,
    quality: StreamQualityConfig,
) -> Result<(Child, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>), UsbCheckError> {
    let remote_cmd = format!(
        "CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server port={} max_size={} bitrate={} fps={} cbr={}",
        port, quality.max_size, quality.bitrate, quality.fps, quality.cbr
    );
    let mut child = new_hidden_command(adb_bin)
        .args(["-s", serial, "shell", &remote_cmd])
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

    let recent_logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::with_capacity(16)));
    let server_listening = Arc::new(AtomicBool::new(false));

    if let Some(stdout) = child.stdout.take() {
        let logs_ref = Arc::clone(&recent_logs);
        let listen_ref = Arc::clone(&server_listening);
        let _ = thread::Builder::new()
            .name("adb-server-stdout".to_string())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if line.contains("Listening on 127.0.0.1:") {
                        listen_ref.store(true, Ordering::Release);
                    }
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

    Ok((child, recent_logs, server_listening))
}

fn is_cancelled(running: Option<&AtomicBool>) -> bool {
    running.map(|r| !r.load(Ordering::Relaxed)).unwrap_or(false)
}

fn push_log_line(logs: &Arc<Mutex<Vec<String>>>, line: String) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Ok(mut guard) = logs.lock() {
        if guard.len() >= 12 {
            guard.remove(0);
        }
        guard.push(trimmed.to_string());
    }
}

fn format_recent_logs(logs: &Arc<Mutex<Vec<String>>>) -> String {
    if let Ok(guard) = logs.lock() {
        for line in guard.iter().rev() {
            if line.contains("Error")
                || line.contains("Exception")
                || line.contains("terminated")
                || line.contains("Fatal")
            {
                return line.clone();
            }
        }
        if let Some(last) = guard.last() {
            return last.clone();
        }
    }
    String::new()
}

fn kill_remote_server(adb_bin: &Path, serial: Option<&str>) {
    let kill_script = "pkill -9 -f 'com.mirror.Server' 2>/dev/null; \
        for pid in $(pidof app_process64 app_process32 app_process 2>/dev/null); do \
        if tr '\\0' ' ' < /proc/$pid/cmdline 2>/dev/null | grep -q 'com.mirror.Server'; then kill -9 $pid 2>/dev/null; fi; \
        done";
    let mut cmd = new_hidden_command(adb_bin);
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    let _ = cmd
        .args(["shell", kill_script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Locates the `adb` executable bundled alongside the app, on system `PATH`, or inside standard Android SDK directories.
pub fn locate_adb_binary() -> Result<PathBuf, UsbCheckError> {
    let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };

    // 0. Prefer bundled `adb` next to the executable (Windows installer, macOS .app, Linux bundle)
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let bundled_candidates = [
                exe_dir.join(exe_name),
                exe_dir.join("platform-tools").join(exe_name),
                exe_dir.join("adb").join(exe_name),
                exe_dir.join("..").join("platform-tools").join(exe_name),
                exe_dir.join("..").join("Resources").join(exe_name),
                exe_dir
                    .join("..")
                    .join("Resources")
                    .join("platform-tools")
                    .join(exe_name),
            ];
            for candidate in bundled_candidates {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    // 1. Check `adb` on system PATH (and common Homebrew/Linux system bins for GUI launches)
    if let Ok(out) = new_hidden_command("adb").arg("version").output() {
        if out.status.success() {
            return Ok(PathBuf::from("adb"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        for p in ["/opt/homebrew/bin/adb", "/usr/local/bin/adb"] {
            let cand = PathBuf::from(p);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for p in ["/usr/bin/adb", "/usr/local/bin/adb"] {
            let cand = PathBuf::from(p);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }

    // 2. Check ANDROID_HOME / ANDROID_SDK_ROOT / LOCALAPPDATA / HOME
    let mut sdk_roots = Vec::new();
    if let Ok(h) = env::var("ANDROID_HOME") {
        sdk_roots.push(PathBuf::from(h));
    }
    if let Ok(r) = env::var("ANDROID_SDK_ROOT") {
        sdk_roots.push(PathBuf::from(r));
    }
    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        sdk_roots.push(PathBuf::from(local_app_data).join("Android").join("Sdk"));
    }
    if let Ok(home) = env::var("HOME") {
        sdk_roots.push(PathBuf::from(&home).join("Android").join("Sdk"));
        sdk_roots.push(
            PathBuf::from(&home)
                .join("Library")
                .join("Android")
                .join("sdk"),
        );
    }

    for root in sdk_roots {
        let candidate = root.join("platform-tools").join(exe_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(UsbCheckError::AdbMissing(
        "Install Android platform-tools and add adb to your system PATH".to_string(),
    ))
}

/// Inspects `adb devices` output and returns the authorized physical USB device serial, or a specific
/// [`UsbCheckError`] distinguishing `NoDevice`, `Unauthorized`, and `Offline`.
pub fn check_usb_device(adb_bin: &Path) -> Result<String, UsbCheckError> {
    let out = new_hidden_command(adb_bin)
        .arg("devices")
        .output()
        .map_err(|e| UsbCheckError::Other(format!("Failed to run `adb devices`: {}", e)))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(UsbCheckError::Other(format!(
            "`adb devices` exited with error: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_adb_devices_output(&stdout)
}

/// Parses `adb devices` output strictly for physical USB transports.
/// Any network/TCP transport (`serial.contains(':')` or mDNS TLS service) is completely ignored
/// so stale network ADB entries can never hijack USB auto-connect or mask USB authorization prompts.
fn parse_adb_devices_output(stdout: &str) -> Result<String, UsbCheckError> {
    let mut usb_ready: Option<String> = None;
    let mut usb_unauthorized: Option<String> = None;
    let mut usb_offline: Option<String> = None;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('*')
            || trimmed.starts_with("List of devices")
        {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let serial = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let state = match parts.next() {
            Some(st) => st,
            None => continue,
        };

        // Strictly ignore network/TCP transports (`ip:port` or mDNS `._adb-tls-`)
        if serial.contains(':') || serial.contains("._adb-tls-") {
            continue;
        }

        match state {
            "device" => {
                if usb_ready.is_none() {
                    usb_ready = Some(serial.to_string());
                }
            }
            "unauthorized" => {
                if usb_unauthorized.is_none() {
                    usb_unauthorized = Some(serial.to_string());
                }
            }
            "offline" => {
                if usb_offline.is_none() {
                    usb_offline = Some(serial.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(serial) = usb_ready {
        return Ok(serial);
    }
    if let Some(serial) = usb_unauthorized {
        return Err(UsbCheckError::Unauthorized(serial));
    }
    if let Some(serial) = usb_offline {
        return Err(UsbCheckError::Offline(serial));
    }
    Err(UsbCheckError::NoDevice)
}

pub fn locate_artifact_file(relative_path: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join(relative_path));
        candidates.push(cwd.join("..").join(relative_path));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(relative_path));
            candidates.push(dir.join("..").join("Resources").join(relative_path));
            candidates.push(dir.join("..").join("share").join("mobile-mirror").join(relative_path));
            candidates.push(dir.join("..").join(relative_path));
            candidates.push(dir.join("..").join("..").join(relative_path));
            candidates.push(dir.join("..").join("..").join("..").join(relative_path));
        }
    }
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(parent) = manifest_root.parent() {
        candidates.push(parent.join(relative_path));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn locate_or_build_server_jar() -> Result<PathBuf, String> {
    if let Some(jar) = locate_artifact_file("server.jar")
        .or_else(|| locate_artifact_file("server/server.jar"))
    {
        return Ok(jar);
    }

    // Attempt to build server.jar automatically if missing
    if cfg!(windows) {
        if let Some(build_script) = locate_artifact_file("server/build.ps1") {
            let status = new_hidden_command("powershell")
                .args([
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    &build_script.to_string_lossy(),
                ])
                .status()
                .map_err(|e| format!("Failed to invoke server/build.ps1: {}", e))?;
            if status.success() {
                if let Some(jar) = locate_artifact_file("server/server.jar") {
                    return Ok(jar);
                }
            }
        }
    } else if let Some(build_script) = locate_artifact_file("server/build.sh") {
        let status = Command::new("sh")
            .arg(&build_script)
            .status()
            .map_err(|e| format!("Failed to invoke server/build.sh: {}", e))?;
        if status.success() {
            if let Some(jar) = locate_artifact_file("server/server.jar") {
                return Ok(jar);
            }
        }
    }

    Err(
        "Could not find `server/server.jar`. Run `server/build.ps1` to compile the Android server."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_adb_devices_strict_usb_only() {
        let empty = "* daemon not running; starting now at tcp:5037\n\
                     * daemon started successfully\n\
                     List of devices attached\n\n";
        assert_eq!(parse_adb_devices_output(empty), Err(UsbCheckError::NoDevice));

        let unauth = "List of devices attached\nR58N123456A\tunauthorized\n";
        assert_eq!(
            parse_adb_devices_output(unauth),
            Err(UsbCheckError::Unauthorized("R58N123456A".to_string()))
        );

        let offline = "List of devices attached\nR58N123456A\toffline\n";
        assert_eq!(
            parse_adb_devices_output(offline),
            Err(UsbCheckError::Offline("R58N123456A".to_string()))
        );

        // Stale network `ip:port` entry in `device` state alone must return NoDevice
        let tcp_only_ready = "List of devices attached\n192.168.1.50:5555\tdevice\n";
        assert_eq!(
            parse_adb_devices_output(tcp_only_ready),
            Err(UsbCheckError::NoDevice)
        );

        // Stale network `ip:port` entry in `device` state alongside physical USB `unauthorized`
        // must NOT hijack auto-connect and must surface the USB `Unauthorized` prompt!
        let tcp_and_usb_unauth =
            "List of devices attached\n192.168.1.50:5555\tdevice\nR58M123456\tunauthorized\n";
        assert_eq!(
            parse_adb_devices_output(tcp_and_usb_unauth),
            Err(UsbCheckError::Unauthorized("R58M123456".to_string()))
        );

        // Stale network `ip:port` offline entry must NOT mask NoDevice when unplugged
        let stale_tcp_offline = "List of devices attached\n192.168.1.50:5555\toffline\n";
        assert_eq!(
            parse_adb_devices_output(stale_tcp_offline),
            Err(UsbCheckError::NoDevice)
        );

        // Physical USB serial connects cleanly even if a TCP entry is also listed
        let both_ready =
            "List of devices attached\n192.168.1.50:5555\tdevice\nR58N123456A\tdevice\n";
        assert_eq!(
            parse_adb_devices_output(both_ready),
            Ok("R58N123456A".to_string())
        );
    }
}
