use crate::adb::{AdbSessionGuard, StreamQualityConfig, UsbAutoConnector, UsbStatus};
use crate::airplay::AirPlayServer;
use crate::ios_usb::{self, IosUsbProbe};
use crate::protocol::{ControlWriter, DevicePlatform, NalPacket, PacketSource, StreamHeader, StreamReader};

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// [`PacketSource`] implementation for a wired Android ADB USB stream,
/// owning the [`AdbSessionGuard`] so cleanup happens automatically on shutdown or drop.
pub struct AdbPacketSource {
    reader: StreamReader<TcpStream>,
    guard: Option<AdbSessionGuard>,
    device_name: String,
}

impl AdbPacketSource {
    pub fn new(
        reader: StreamReader<TcpStream>,
        guard: AdbSessionGuard,
        device_name: impl Into<String>,
    ) -> Self {
        let name = device_name.into();
        let resolved_name = if name.trim().is_empty() {
            format!("Android ({})", guard.serial())
        } else {
            name
        };
        Self {
            reader,
            guard: Some(guard),
            device_name: resolved_name,
        }
    }
}

impl PacketSource for AdbPacketSource {
    fn header(&self) -> StreamHeader {
        self.reader.header()
    }

    fn read_packet(&mut self) -> io::Result<Option<NalPacket>> {
        self.reader.read_packet()
    }

    fn control_writer(&self) -> ControlWriter {
        self.reader.control_writer()
    }

    fn platform(&self) -> DevicePlatform {
        DevicePlatform::AndroidUsb
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn shutdown(&mut self) {
        self.reader.shutdown();
        self.guard.take();
    }
}

/// Unified connection status published by [`UnifiedDeviceConnector`] to the UI thread.
pub enum DeviceStatus {
    Waiting {
        headline: String,
        subtext: String,
        is_warning: bool,
        device_serial: Option<String>,
        platform: Option<DevicePlatform>,
        airplay_listening: bool,
    },
    Connecting {
        headline: String,
        subtext: String,
        platform: DevicePlatform,
    },
    Connected(Box<dyn PacketSource>),
}

enum IosWorkerStatus {
    NotConnected,
    Waiting {
        headline: String,
        subtext: String,
        is_warning: bool,
        device_serial: Option<String>,
    },
    Connecting {
        headline: String,
        subtext: String,
    },
    Connected(Box<dyn PacketSource>),
}

#[derive(Clone, PartialEq, Eq)]
struct WaitingSnapshot {
    headline: String,
    subtext: String,
    is_warning: bool,
    device_serial: Option<String>,
    platform: Option<DevicePlatform>,
    airplay_listening: bool,
}

/// Coordinates Android USB (ADB), iOS USB (`usbmuxd`), and wireless iOS AirPlay discovery
/// without blocking the 60 FPS UI thread.
pub struct UnifiedDeviceConnector {
    adb: UsbAutoConnector,
    ios_rx: Receiver<IosWorkerStatus>,
    ios_running: Arc<AtomicBool>,
    ios_auto_connect: Arc<AtomicBool>,
    ios_manual_trigger: Arc<AtomicBool>,
    ios_quality: Arc<Mutex<StreamQualityConfig>>,
    ios_worker: Option<JoinHandle<()>>,
    airplay_server: Option<AirPlayServer>,
    airplay_enabled: bool,
    device_display_name: String,

    // Cached per-transport state for synthesizing unified waiting/connecting messages
    adb_connecting: Option<(String, String)>,
    adb_waiting: (String, String, bool, Option<String>),
    ios_connecting: Option<(String, String)>,
    ios_waiting: Option<(String, String, bool, Option<String>)>,

    // Latest published unified status fields
    last_headline: String,
    last_subtext: String,
    last_is_connecting: bool,
    last_is_warning: bool,
    last_device_serial: Option<String>,
    last_platform: Option<DevicePlatform>,
    last_published_waiting: Option<WaitingSnapshot>,
    last_published_connecting: Option<(String, String, DevicePlatform)>,
}

impl UnifiedDeviceConnector {
    pub fn new(
        port: u16,
        quality: StreamQualityConfig,
        auto_connect_usb: bool,
        airplay_enabled: bool,
        device_display_name: &str,
    ) -> Self {
        let adb = UsbAutoConnector::new_with_config(port, quality, auto_connect_usb);

        let (ios_tx, ios_rx) = mpsc::channel();
        let ios_running = Arc::new(AtomicBool::new(true));
        let ios_auto_connect = Arc::new(AtomicBool::new(auto_connect_usb));
        let ios_manual_trigger = Arc::new(AtomicBool::new(false));
        let ios_quality = Arc::new(Mutex::new(quality));

        let ios_worker = spawn_ios_worker(
            ios_tx,
            Arc::clone(&ios_running),
            Arc::clone(&ios_auto_connect),
            Arc::clone(&ios_manual_trigger),
            Arc::clone(&ios_quality),
        );

        let airplay_server = if airplay_enabled {
            start_airplay_server(device_display_name)
        } else {
            None
        };
        let airplay_listening = airplay_server.is_some();

        let (initial_headline, initial_subtext) =
            default_waiting_text(airplay_listening, device_display_name);

        Self {
            adb,
            ios_rx,
            ios_running,
            ios_auto_connect,
            ios_manual_trigger,
            ios_quality,
            ios_worker: Some(ios_worker),
            airplay_server,
            airplay_enabled,
            device_display_name: device_display_name.to_string(),
            adb_connecting: None,
            adb_waiting: (
                "Connect your phone via USB".to_string(),
                "Make sure USB debugging is enabled on your Android device.".to_string(),
                false,
                None,
            ),
            ios_connecting: None,
            ios_waiting: None,
            last_headline: initial_headline,
            last_subtext: initial_subtext,
            last_is_connecting: false,
            last_is_warning: false,
            last_device_serial: None,
            last_platform: None,
            last_published_waiting: None,
            last_published_connecting: None,
        }
    }

    pub fn set_auto_connect_usb(&mut self, enabled: bool) {
        self.adb.set_auto_connect(enabled);
        self.ios_auto_connect.store(enabled, Ordering::SeqCst);
    }

    pub fn set_airplay_enabled(&mut self, enabled: bool) {
        if self.airplay_enabled == enabled {
            return;
        }
        self.airplay_enabled = enabled;
        if enabled {
            if self.airplay_server.is_none() {
                self.airplay_server = start_airplay_server(&self.device_display_name);
            }
        } else if let Some(server) = self.airplay_server.take() {
            server.stop();
        }
    }

    pub fn set_quality(&mut self, quality: StreamQualityConfig) {
        self.adb.set_quality(quality);
        if let Ok(mut guard) = self.ios_quality.lock() {
            *guard = quality;
        }
    }

    pub fn trigger_connect_now(&mut self) {
        self.adb.trigger_connect_now();
        self.ios_manual_trigger.store(true, Ordering::SeqCst);
    }

    pub fn poll_status(&mut self) -> Option<DeviceStatus> {
        // 1. AirPlay wireless connections are accepted immediately
        if let Some(ref server) = self.airplay_server {
            if let Some(session) = server.try_accept_session() {
                return Some(self.finish_connected(Box::new(session), DevicePlatform::IosAirPlay));
            }
        }

        // 2. Drain Android USB (ADB) updates without skipping Connected
        if let Some(usb_status) = self.adb.poll_status() {
            match usb_status {
                UsbStatus::Connected(reader, guard) => {
                    let name = format!("Android ({})", guard.serial());
                    let source = AdbPacketSource::new(reader, guard, name);
                    return Some(self.finish_connected(Box::new(source), DevicePlatform::AndroidUsb));
                }
                UsbStatus::Connecting { headline, subtext } => {
                    self.adb_connecting = Some((headline, subtext));
                }
                UsbStatus::WaitingForUsb {
                    headline,
                    subtext,
                    is_warning,
                    device_serial,
                } => {
                    self.adb_connecting = None;
                    self.adb_waiting = (headline, subtext, is_warning, device_serial);
                }
            }
        }

        // 3. Drain iOS USB updates without skipping Connected
        while let Ok(ios_status) = self.ios_rx.try_recv() {
            match ios_status {
                IosWorkerStatus::Connected(source) => {
                    return Some(self.finish_connected(source, DevicePlatform::IosUsb));
                }
                IosWorkerStatus::Connecting { headline, subtext } => {
                    self.ios_connecting = Some((headline, subtext));
                }
                IosWorkerStatus::Waiting {
                    headline,
                    subtext,
                    is_warning,
                    device_serial,
                } => {
                    self.ios_connecting = None;
                    self.ios_waiting = Some((headline, subtext, is_warning, device_serial));
                }
                IosWorkerStatus::NotConnected => {
                    self.ios_connecting = None;
                    self.ios_waiting = None;
                }
            }
        }

        // 4. Synthesize unified Connecting or Waiting status when state changes
        if let Some((ref headline, ref subtext)) = self.adb_connecting {
            return self.publish_connecting(headline.clone(), subtext.clone(), DevicePlatform::AndroidUsb);
        }
        if let Some((ref headline, ref subtext)) = self.ios_connecting {
            return self.publish_connecting(headline.clone(), subtext.clone(), DevicePlatform::IosUsb);
        }

        let airplay_listening = self.is_airplay_listening();
        let snapshot = self.build_waiting_snapshot(airplay_listening);
        if self.last_published_waiting.as_ref() != Some(&snapshot) {
            self.last_headline = snapshot.headline.clone();
            self.last_subtext = snapshot.subtext.clone();
            self.last_is_connecting = false;
            self.last_is_warning = snapshot.is_warning;
            self.last_device_serial = snapshot.device_serial.clone();
            self.last_platform = snapshot.platform;
            self.last_published_connecting = None;
            self.last_published_waiting = Some(snapshot.clone());

            return Some(DeviceStatus::Waiting {
                headline: snapshot.headline,
                subtext: snapshot.subtext,
                is_warning: snapshot.is_warning,
                device_serial: snapshot.device_serial,
                platform: snapshot.platform,
                airplay_listening: snapshot.airplay_listening,
            });
        }

        None
    }

    pub fn latest_headline(&self) -> &str {
        &self.last_headline
    }

    pub fn latest_subtext(&self) -> &str {
        &self.last_subtext
    }

    pub fn latest_is_connecting(&self) -> bool {
        self.last_is_connecting
    }

    pub fn latest_is_warning(&self) -> bool {
        self.last_is_warning
    }

    pub fn latest_device_serial(&self) -> Option<&str> {
        self.last_device_serial.as_deref()
    }

    pub fn latest_platform(&self) -> Option<DevicePlatform> {
        self.last_platform
    }

    pub fn is_airplay_listening(&self) -> bool {
        self.airplay_server.is_some()
    }

    fn finish_connected(
        &mut self,
        source: Box<dyn PacketSource>,
        platform: DevicePlatform,
    ) -> DeviceStatus {
        self.last_headline = "Connected".to_string();
        self.last_subtext.clear();
        self.last_is_connecting = false;
        self.last_is_warning = false;
        self.last_platform = Some(platform);
        self.stop_background_workers();
        DeviceStatus::Connected(source)
    }

    fn publish_connecting(
        &mut self,
        headline: String,
        subtext: String,
        platform: DevicePlatform,
    ) -> Option<DeviceStatus> {
        let key = (headline.clone(), subtext.clone(), platform);
        if self.last_published_connecting.as_ref() == Some(&key) {
            return None;
        }
        self.last_headline = headline.clone();
        self.last_subtext = subtext.clone();
        self.last_is_connecting = true;
        self.last_is_warning = false;
        self.last_platform = Some(platform);
        self.last_published_waiting = None;
        self.last_published_connecting = Some(key);

        Some(DeviceStatus::Connecting {
            headline,
            subtext,
            platform,
        })
    }

    fn build_waiting_snapshot(&self, airplay_listening: bool) -> WaitingSnapshot {
        let (ref adb_h, ref adb_s, adb_warn, ref adb_serial) = self.adb_waiting;

        // If an Android USB device is physically detected (or has an actionable USB warning like Unauthorized/Offline),
        // surface it unless ADB itself is simply not installed while AirPlay/iOS is ready.
        let adb_missing_only = adb_h == "Android Debug Bridge not found";
        if adb_serial.is_some() || (adb_warn && (!adb_missing_only || !airplay_listening)) {
            return WaitingSnapshot {
                headline: adb_h.clone(),
                subtext: adb_s.clone(),
                is_warning: adb_warn,
                device_serial: adb_serial.clone(),
                platform: Some(DevicePlatform::AndroidUsb),
                airplay_listening,
            };
        }

        if let Some((ref ios_h, ref ios_s, ios_warn, ref ios_serial)) = self.ios_waiting {
            return WaitingSnapshot {
                headline: ios_h.clone(),
                subtext: ios_s.clone(),
                is_warning: ios_warn,
                device_serial: ios_serial.clone(),
                platform: Some(DevicePlatform::IosUsb),
                airplay_listening,
            };
        }

        let (headline, subtext) =
            default_waiting_text(airplay_listening, &self.device_display_name);
        WaitingSnapshot {
            headline,
            subtext,
            is_warning: false,
            device_serial: None,
            platform: None,
            airplay_listening,
        }
    }

    fn stop_background_workers(&mut self) {
        self.adb.stop();
        self.ios_running.store(false, Ordering::SeqCst);
        if let Some(server) = self.airplay_server.take() {
            server.stop();
        }
    }
}

impl Drop for UnifiedDeviceConnector {
    fn drop(&mut self) {
        self.stop_background_workers();
        let _ = self.ios_worker.take();
    }
}

fn default_waiting_text(airplay_listening: bool, device_display_name: &str) -> (String, String) {
    if airplay_listening {
        (
            "Ready — USB (Android/iOS) or AirPlay".to_string(),
            format!(
                "Plug in Android/iPhone via USB, or AirPlay to \"{}\"",
                device_display_name
            ),
        )
    } else {
        (
            "Connect Android or iPhone via USB".to_string(),
            "Plug in your Android (USB debugging) or iPhone (Trust this PC)".to_string(),
        )
    }
}

fn start_airplay_server(device_display_name: &str) -> Option<AirPlayServer> {
    match AirPlayServer::start(device_display_name) {
        Ok(server) => Some(server),
        Err(err) => {
            eprintln!("[AirPlay] Failed to start receiver '{}': {}", device_display_name, err);
            None
        }
    }
}

fn spawn_ios_worker(
    tx: mpsc::Sender<IosWorkerStatus>,
    running: Arc<AtomicBool>,
    auto_connect: Arc<AtomicBool>,
    manual_trigger: Arc<AtomicBool>,
    quality: Arc<Mutex<StreamQualityConfig>>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ios-usb-connector".to_string())
        .spawn(move || {
            let mut last_not_connected = false;
            while running.load(Ordering::Relaxed) {
                let triggered = manual_trigger.swap(false, Ordering::SeqCst);
                let should_connect = triggered || auto_connect.load(Ordering::Relaxed);

                match ios_usb::probe_ios_usb() {
                    IosUsbProbe::NoDevice => {
                        if !last_not_connected {
                            let _ = tx.send(IosWorkerStatus::NotConnected);
                            last_not_connected = true;
                        }
                    }
                    IosUsbProbe::NeedsDriverOrTrust { serial, reason } => {
                        last_not_connected = false;
                        let _ = tx.send(IosWorkerStatus::Waiting {
                            headline: format!("iOS USB attention needed ({})", serial),
                            subtext: reason,
                            is_warning: true,
                            device_serial: Some(serial),
                        });
                    }
                    IosUsbProbe::Ready { serial, product_name } => {
                        last_not_connected = false;
                        if !should_connect {
                            let _ = tx.send(IosWorkerStatus::Waiting {
                                headline: format!("iPhone Ready ({})", product_name),
                                subtext: "iOS USB device detected — click 'Start Mirroring' to begin."
                                    .to_string(),
                                is_warning: false,
                                device_serial: Some(serial),
                            });
                        } else {
                            let max_fps = quality.lock().map(|g| g.fps).unwrap_or(60);
                            let _ = tx.send(IosWorkerStatus::Connecting {
                                headline: format!("Connecting to {}…", product_name),
                                subtext: "Starting low-latency iOS USB screen capture".to_string(),
                            });

                            match ios_usb::connect_ios_usb(max_fps) {
                                Ok(session) => {
                                    let _ = tx.send(IosWorkerStatus::Connected(Box::new(session)));
                                    break;
                                }
                                Err(err) => {
                                    eprintln!("[iOS USB] Connect retry needed: {}", err);
                                    let _ = tx.send(IosWorkerStatus::Waiting {
                                        headline: "Reconnecting to iPhone…".to_string(),
                                        subtext: err.to_string(),
                                        is_warning: true,
                                        device_serial: Some(serial),
                                    });
                                }
                            }
                        }
                    }
                }

                for _ in 0..8 {
                    if !running.load(Ordering::Relaxed) {
                        return;
                    }
                    if manual_trigger.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .expect("Failed to spawn ios-usb-connector thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_waiting_text_variants() {
        let (h1, s1) = default_waiting_text(true, "Desktop Mirror");
        assert_eq!(h1, "Ready — USB (Android/iOS) or AirPlay");
        assert!(s1.contains("Desktop Mirror"));

        let (h2, s2) = default_waiting_text(false, "Desktop Mirror");
        assert_eq!(h2, "Connect Android or iPhone via USB");
        assert!(s2.contains("Android"));
        assert!(s2.contains("iPhone"));
    }
}
