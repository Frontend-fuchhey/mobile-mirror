//! Apple iOS USB QuickTime / CoreMedia screen & audio mirroring over `nusb`.
//!
//! This module implements:
//! - USB device discovery and driver/trust probing (`probe_ios_usb`, `IosUsbProbe`)
//! - Apple QuickTime mode-switch vendor control transfer (`0x40 / 0x52 / 0x0000 / 0x0002`)
//! - CoreMedia USB bulk framing (`PING`, `SYNC`, `ASYN`, `FEED`, `RPLY`, `EAT!`, `HPD1`, `HPA1`, `CWPA`, `OG`, `AFMT`, `CFMT`, `TIME`)
//! - `CMSampleBuffer` (`sbuf` / `stia` / `sdat` / `avcC`) parsing to Annex-B H.264 `NalPacket`s and 48 kHz LPCM audio packets.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, In, Out, Recipient, TransferError};
use nusb::{Device, DeviceInfo, Endpoint, Interface, MaybeFuture};

use crate::protocol::{
    ControlWriter, DevicePlatform, NalPacket, PacketSource, StreamHeader, CODEC_ID_H264,
};

/// Apple USB Vendor ID.
pub const APPLE_VENDOR_ID: u16 = 0x05AC;

/// QuickTime / CoreMedia USB interface class & subclass.
const QT_INTERFACE_CLASS: u8 = 0xFF;
const QT_INTERFACE_SUBCLASS: u8 = 0x2A;
const APPLE_MOBILE_SUBCLASS: u8 = 0xFE;

/// Apple vendor control transfer to activate the QuickTime USB bulk configuration.
const QT_SWITCH_REQUEST: u8 = 0x52;
const QT_SWITCH_VALUE: u16 = 0x0000;
const QT_SWITCH_INDEX: u16 = 0x0002;

/// Maximum CoreMedia frame size accepted (16 MiB) to guard against corrupt length prefixes.
const MAX_COREMEDIA_FRAME_LEN: usize = 16 * 1024 * 1024;

/// CoreMedia 4CC magic constants (stored in wire little-endian order).
pub const MAGIC_PING: u32 = u32::from_be_bytes(*b"ping");
pub const MAGIC_SYNC: u32 = u32::from_be_bytes(*b"sync");
pub const MAGIC_ASYN: u32 = u32::from_be_bytes(*b"asyn");
pub const MAGIC_FEED: u32 = u32::from_be_bytes(*b"feed");
pub const MAGIC_RPLY: u32 = u32::from_be_bytes(*b"rply");
pub const MAGIC_CWPA: u32 = u32::from_be_bytes(*b"cwpa");
pub const MAGIC_AFMT: u32 = u32::from_be_bytes(*b"afmt");
pub const MAGIC_CFMT: u32 = u32::from_be_bytes(*b"cfmt");
pub const MAGIC_HPD1: u32 = u32::from_be_bytes(*b"hpd1");
pub const MAGIC_HPA1: u32 = u32::from_be_bytes(*b"hpa1");
pub const MAGIC_EAT: u32 = u32::from_be_bytes(*b"eat!");
pub const MAGIC_TIME: u32 = u32::from_be_bytes(*b"time");
pub const MAGIC_GOOG: u32 = u32::from_be_bytes(*b"goog");
pub const MAGIC_OG: u32 = u32::from_be_bytes(*b"og\0\0");
pub const MAGIC_NEED: u32 = u32::from_be_bytes(*b"need");
pub const MAGIC_SBUF: u32 = u32::from_be_bytes(*b"sbuf");
pub const MAGIC_STIA: u32 = u32::from_be_bytes(*b"stia");
pub const MAGIC_SDAT: u32 = u32::from_be_bytes(*b"sdat");
pub const MAGIC_FDSC: u32 = u32::from_be_bytes(*b"fdsc");
pub const MAGIC_AVCC: u32 = u32::from_be_bytes(*b"avcC");
pub const MAGIC_VIDE: u32 = u32::from_be_bytes(*b"vide");
pub const MAGIC_SOUN: u32 = u32::from_be_bytes(*b"soun");

const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Probe status for an Apple iOS device connected over USB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IosUsbProbe {
    /// No Apple iOS USB device is currently attached.
    NoDevice,
    /// An Apple iOS USB device is attached and accessible via WinUSB/libusb.
    Ready {
        serial: String,
        product_name: String,
    },
    /// An Apple iOS device was detected, but opening or claiming it failed
    /// (typically missing WinUSB driver on Windows or "Trust This Computer" not accepted).
    NeedsDriverOrTrust {
        serial: String,
        reason: String,
    },
}

/// Returns `true` if the USB product ID or interface descriptors look like an Apple iOS device.
fn is_apple_ios_device(info: &DeviceInfo) -> bool {
    if info.vendor_id() != APPLE_VENDOR_ID {
        return false;
    }
    let pid = info.product_id();
    let ios_pid = (0x1290..=0x12FF).contains(&pid);
    let ios_iface = info.interfaces().any(|iface| {
        iface.class() == QT_INTERFACE_CLASS
            && (iface.subclass() == QT_INTERFACE_SUBCLASS
                || iface.subclass() == APPLE_MOBILE_SUBCLASS)
    });
    ios_pid || ios_iface
}

/// Probes connected USB buses for an Apple iOS device and checks accessibility.
pub fn probe_ios_usb() -> IosUsbProbe {
    let devices = match nusb::list_devices().wait() {
        Ok(iter) => iter,
        Err(e) => {
            return IosUsbProbe::NeedsDriverOrTrust {
                serial: String::new(),
                reason: format!("Failed to enumerate USB devices: {e}"),
            };
        }
    };

    for info in devices {
        if !is_apple_ios_device(&info) {
            continue;
        }
        let serial = info.serial_number().unwrap_or_default().to_string();
        let product_name = info
            .product_string()
            .unwrap_or("Apple iOS Device")
            .to_string();

        match info.open().wait() {
            Ok(dev) => {
                // Verify we can read the active configuration or claim an interface.
                let cfg_ok = dev.active_configuration().is_ok();
                if cfg_ok {
                    return IosUsbProbe::Ready {
                        serial,
                        product_name,
                    };
                }
                return IosUsbProbe::NeedsDriverOrTrust {
                    serial,
                    reason: "Device opened, but no active USB configuration was found. \
                             Unlock the device and tap 'Trust This Computer'."
                        .to_string(),
                };
            }
            Err(e) => {
                return IosUsbProbe::NeedsDriverOrTrust {
                    serial,
                    reason: format!(
                        "Cannot open Apple iOS USB device ({e}). Ensure WinUSB driver is \
                         installed (e.g. via Zadig) and 'Trust This Computer' is accepted."
                    ),
                };
            }
        }
    }

    IosUsbProbe::NoDevice
}

/// A parsed CoreMedia USB frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreMediaFrame<'a> {
    pub total_len: usize,
    pub magic: u32,
    pub payload: &'a [u8],
}

/// Parses a single CoreMedia USB frame (`[total_len: u32 LE][magic: 4CC][payload...]`).
///
/// Returns `Ok(None)` if `buf` does not yet contain a complete frame, or `Err(InvalidData)`
/// if the declared length is smaller than 8 bytes or exceeds `MAX_COREMEDIA_FRAME_LEN`.
pub fn parse_coremedia_frame(buf: &[u8]) -> io::Result<Option<(CoreMediaFrame<'_>, usize)>> {
    if buf.len() < 8 {
        return Ok(None);
    }

    let total_len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if total_len < 8 || total_len > MAX_COREMEDIA_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid CoreMedia frame length: {total_len}"),
        ));
    }

    if buf.len() < total_len {
        return Ok(None);
    }

    let magic = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Ok(Some((
        CoreMediaFrame {
            total_len,
            magic,
            payload: &buf[8..total_len],
        },
        total_len,
    )))
}

/// Serializes a CoreMedia USB frame with `[total_len: u32 LE][magic: u32 LE][payload]`.
pub fn build_coremedia_frame(magic: u32, payload: &[u8]) -> Vec<u8> {
    let total_len = (8 + payload.len()) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&magic.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Parses an ISO/IEC 14496-15 `AVCDecoderConfigurationRecord` (`avcC`) into an Annex-B
/// byte stream containing all SPS and PPS NAL units, along with the AVCC NAL length size
/// (`1`, `2`, or `4` bytes).
pub fn parse_avcc_config(avcc: &[u8]) -> io::Result<(Vec<u8>, usize)> {
    if avcc.len() < 7 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "avcC descriptor too short",
        ));
    }
    if avcc[0] != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unsupported avcC configurationVersion: {}", avcc[0]),
        ));
    }

    let nal_length_size = ((avcc[4] & 0x03) + 1) as usize;
    let num_sps = (avcc[5] & 0x1F) as usize;

    let mut offset = 6usize;
    let mut annex_b = Vec::with_capacity(avcc.len() + 16);

    for _ in 0..num_sps {
        if offset + 2 > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated SPS length in avcC",
            ));
        }
        let sps_len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
        offset += 2;
        if sps_len == 0 || offset + sps_len > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid SPS payload bounds in avcC",
            ));
        }
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(&avcc[offset..offset + sps_len]);
        offset += sps_len;
    }

    if offset >= avcc.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Missing PPS count in avcC",
        ));
    }
    let num_pps = avcc[offset] as usize;
    offset += 1;

    for _ in 0..num_pps {
        if offset + 2 > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated PPS length in avcC",
            ));
        }
        let pps_len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
        offset += 2;
        if pps_len == 0 || offset + pps_len > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid PPS payload bounds in avcC",
            ));
        }
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(&avcc[offset..offset + pps_len]);
        offset += pps_len;
    }

    if annex_b.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "avcC contained no SPS or PPS NAL units",
        ));
    }

    Ok((annex_b, nal_length_size))
}

/// Converts an AVCC length-prefixed H.264 access unit into Annex-B (`00 00 00 01`) format.
///
/// Returns `(annex_b_bytes, is_keyframe)` where `is_keyframe` is `true` if any NAL unit
/// in the sample has NAL unit type `5` (IDR slice).
pub fn avcc_to_annexb(avcc_sample: &[u8], nal_length_size: usize) -> io::Result<(Vec<u8>, bool)> {
    if !matches!(nal_length_size, 1 | 2 | 4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Unsupported AVCC NAL length size: {nal_length_size}"),
        ));
    }

    let mut offset = 0usize;
    let mut out = Vec::with_capacity(avcc_sample.len() + 16);
    let mut is_keyframe = false;

    while offset + nal_length_size <= avcc_sample.len() {
        let nal_len = match nal_length_size {
            1 => avcc_sample[offset] as usize,
            2 => u16::from_be_bytes([avcc_sample[offset], avcc_sample[offset + 1]]) as usize,
            4 => u32::from_be_bytes([
                avcc_sample[offset],
                avcc_sample[offset + 1],
                avcc_sample[offset + 2],
                avcc_sample[offset + 3],
            ]) as usize,
            _ => unreachable!(),
        };
        offset += nal_length_size;

        if nal_len == 0 || offset + nal_len > avcc_sample.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid or truncated AVCC NAL unit length",
            ));
        }

        let nal = &avcc_sample[offset..offset + nal_len];
        let nal_type = nal[0] & 0x1F;
        if nal_type == 5 {
            is_keyframe = true;
        }

        out.extend_from_slice(&ANNEX_B_START_CODE);
        out.extend_from_slice(nal);
        offset += nal_len;
    }

    if out.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "AVCC sample contained no NAL units",
        ));
    }

    Ok((out, is_keyframe))
}

/// Validates and aligns a 48 kHz stereo 16-bit LE LPCM audio payload to 4-byte frame boundaries.
pub fn parse_lpcm_48khz_audio(sdat: &[u8]) -> Option<Vec<u8>> {
    let aligned_len = sdat.len() & !3;
    if aligned_len == 0 {
        return None;
    }
    Some(sdat[..aligned_len].to_vec())
}

/// Parses a CoreMedia `stia` (Sample Timing Info Array) atom to extract PTS in microseconds.
fn parse_stia_pts_us(stia: &[u8]) -> Option<u64> {
    // CMTime: value (i64 LE), timescale (i32 LE), flags (u32 LE), epoch (i64 LE).
    // In `stia`, PTS typically starts at offset 24 (after duration CMTime) or offset 0.
    let try_cmtime = |slice: &[u8]| -> Option<u64> {
        if slice.len() < 24 {
            return None;
        }
        let value = i64::from_le_bytes(slice[0..8].try_into().ok()?);
        let timescale = i32::from_le_bytes(slice[8..12].try_into().ok()?);
        if value >= 0 && timescale > 0 {
            let us = (value as u128).saturating_mul(1_000_000) / (timescale as u128);
            u64::try_from(us).ok()
        } else {
            None
        }
    };

    if stia.len() >= 48 {
        if let Some(pts) = try_cmtime(&stia[24..48]) {
            return Some(pts);
        }
    }
    try_cmtime(stia)
}

#[derive(Default)]
struct ParsedSampleBuffer<'a> {
    is_audio: bool,
    is_video: bool,
    pts_us: Option<u64>,
    avcc: Option<&'a [u8]>,
    sdat: Option<&'a [u8]>,
}

/// Recursively walks CoreMedia LE atoms (`[len: u32 LE][tag: u32 LE][body...]`) inside `sbuf`.
fn walk_cm_atoms<'a>(data: &'a [u8], out: &mut ParsedSampleBuffer<'a>, depth: u8) {
    if depth > 8 {
        return;
    }
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let atom_len = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if atom_len < 8 || offset + atom_len > data.len() {
            break;
        }
        let tag = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]);
        let body = &data[offset + 8..offset + atom_len];

        match tag {
            MAGIC_SBUF | MAGIC_FDSC => walk_cm_atoms(body, out, depth + 1),
            MAGIC_VIDE => out.is_video = true,
            MAGIC_SOUN => out.is_audio = true,
            MAGIC_STIA => {
                if out.pts_us.is_none() {
                    out.pts_us = parse_stia_pts_us(body);
                }
            }
            MAGIC_SDAT => {
                out.sdat = Some(body);
            }
            MAGIC_AVCC => {
                out.avcc = Some(body);
                out.is_video = true;
            }
            _ => {
                // Also scan for nested ISO BMFF `avcC` box (`[be_len][b"avcC"]`) inside format descriptors.
                if body.len() >= 12 {
                    if let Some(pos) = body.windows(4).position(|w| w == b"avcC") {
                        if pos >= 4 {
                            let box_len = u32::from_be_bytes([
                                body[pos - 4],
                                body[pos - 3],
                                body[pos - 2],
                                body[pos - 1],
                            ]) as usize;
                            let start = pos + 4;
                            if box_len >= 8 && (pos - 4) + box_len <= body.len() {
                                out.avcc = Some(&body[start..(pos - 4) + box_len]);
                                out.is_video = true;
                            }
                        }
                    }
                }
            }
        }

        offset += atom_len;
    }
}

/// Finds the `sbuf` atom inside a `FEED` payload (skipping an optional 8-byte `clock_ref` prefix).
fn extract_sample_buffer<'a>(feed_payload: &'a [u8]) -> ParsedSampleBuffer<'a> {
    let mut parsed = ParsedSampleBuffer::default();
    // Try direct atom walk first, then offset +8 (clock_ref prefix).
    walk_cm_atoms(feed_payload, &mut parsed, 0);
    if parsed.sdat.is_none() && parsed.avcc.is_none() && feed_payload.len() > 16 {
        walk_cm_atoms(&feed_payload[8..], &mut parsed, 0);
    }
    parsed
}

struct SharedQueue {
    queue: Mutex<VecDeque<NalPacket>>,
    cv: Condvar,
    closed: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl SharedQueue {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(64)),
            cv: Condvar::new(),
            closed: AtomicBool::new(false),
            last_error: Mutex::new(None),
        }
    }

    fn push(&self, pkt: NalPacket) {
        if let Ok(mut q) = self.queue.lock() {
            // Bound queue to prevent unbounded memory growth if consumer stalls.
            if q.len() >= 512 {
                q.pop_front();
            }
            q.push_back(pkt);
            self.cv.notify_one();
        }
    }

    fn close(&self, err: Option<String>) {
        if let Some(msg) = err {
            if let Ok(mut guard) = self.last_error.lock() {
                if guard.is_none() {
                    *guard = Some(msg);
                }
            }
        }
        self.closed.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    fn pop_blocking(&self) -> io::Result<Option<NalPacket>> {
        let mut q = self
            .queue
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "iOS USB packet queue poisoned"))?;

        loop {
            if let Some(pkt) = q.pop_front() {
                return Ok(Some(pkt));
            }
            if self.closed.load(Ordering::Acquire) {
                if let Ok(err) = self.last_error.lock() {
                    if let Some(ref msg) = *err {
                        return Err(io::Error::new(io::ErrorKind::BrokenPipe, msg.clone()));
                    }
                }
                return Ok(None);
            }
            q = self.cv.wait(q).map_err(|_| {
                io::Error::new(io::ErrorKind::Other, "iOS USB packet queue poisoned")
            })?;
        }
    }
}

/// Active iOS USB QuickTime / CoreMedia mirroring session.
pub struct IosUsbSession {
    header: StreamHeader,
    device_name: String,
    control: ControlWriter,
    shared: Arc<SharedQueue>,
    shutdown_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// Convenience wrapper for [`IosUsbSession::connect`].
pub fn connect_ios_usb(target_fps: u32) -> io::Result<IosUsbSession> {
    IosUsbSession::connect(target_fps)
}

impl IosUsbSession {
    /// Connects to an attached Apple iOS USB device, switches it into QuickTime mode if needed,
    /// claims the CoreMedia bulk endpoints, and starts the background protocol/demux thread.
    pub fn connect(target_fps: u32) -> io::Result<Self> {
        let (device_name, _device, interface, ep_in_addr, ep_out_addr) =
            open_and_switch_quicktime_interface()?;

        let ep_in = interface
            .endpoint::<Bulk, In>(ep_in_addr)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Bulk IN endpoint: {e}")))?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Bulk OUT endpoint: {e}")))?;

        let shared = Arc::new(SharedQueue::new());
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        let thread_shared = Arc::clone(&shared);
        let thread_shutdown = Arc::clone(&shutdown_flag);
        let fps = target_fps.clamp(15, 120);

        let worker = thread::Builder::new()
            .name("ios-usb-coremedia".to_string())
            .spawn(move || {
                run_coremedia_loop(ep_in, ep_out, fps, thread_shared, thread_shutdown);
            })?;

        Ok(Self {
            header: StreamHeader {
                codec_id: CODEC_ID_H264,
                width: 0,
                height: 0,
                framed: true,
            },
            device_name,
            control: ControlWriter::new(None),
            shared,
            shutdown_flag,
            worker: Some(worker),
        })
    }
}

impl PacketSource for IosUsbSession {
    fn header(&self) -> StreamHeader {
        self.header
    }

    fn read_packet(&mut self) -> io::Result<Option<NalPacket>> {
        self.shared.pop_blocking()
    }

    fn control_writer(&self) -> ControlWriter {
        self.control.clone()
    }

    fn platform(&self) -> DevicePlatform {
        DevicePlatform::IosUsb
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn shutdown(&mut self) {
        self.shutdown_flag.store(true, Ordering::Release);
        self.shared.close(None);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IosUsbSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Finds the QuickTime bulk interface (`class 0xFF, subclass 0x2A`) and its IN/OUT bulk endpoints.
fn find_quicktime_endpoints(device: &Device) -> Option<(u8, u8, u8)> {
    let cfg = device.active_configuration().ok()?;
    for iface in cfg.interfaces() {
        for alt in iface.alt_settings() {
            if alt.class() == QT_INTERFACE_CLASS && alt.subclass() == QT_INTERFACE_SUBCLASS {
                let mut ep_in = None;
                let mut ep_out = None;
                for ep in alt.endpoints() {
                    if ep.transfer_type() == nusb::descriptors::TransferType::Bulk {
                        match ep.direction() {
                            nusb::transfer::Direction::In if ep_in.is_none() => {
                                ep_in = Some(ep.address());
                            }
                            nusb::transfer::Direction::Out if ep_out.is_none() => {
                                ep_out = Some(ep.address());
                            }
                            _ => {}
                        }
                    }
                }
                if let (Some(in_addr), Some(out_addr)) = (ep_in, ep_out) {
                    return Some((alt.interface_number(), in_addr, out_addr));
                }
            }
        }
    }
    None
}

fn open_and_switch_quicktime_interface() -> io::Result<(String, Device, Interface, u8, u8)> {
    let find_apple = || -> io::Result<(DeviceInfo, String)> {
        let info = nusb::list_devices()
            .wait()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("USB list failed: {e}")))?
            .find(is_apple_ios_device)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "No Apple iOS USB device found")
            })?;
        let name = info
            .product_string()
            .unwrap_or("Apple iOS Device")
            .to_string();
        Ok((info, name))
    };

    let (info, device_name) = find_apple()?;
    let device = info
        .open()
        .wait()
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, format!("Open failed: {e}")))?;

    if let Some((iface_num, ep_in, ep_out)) = find_quicktime_endpoints(&device) {
        let interface = device
            .detach_and_claim_interface(iface_num)
            .wait()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Claim QT iface: {e}")))?;
        return Ok((device_name, device, interface, ep_in, ep_out));
    }

    // QuickTime interface not yet active: send Apple vendor control transfer
    // (bmRequestType = 0x40, bRequest = 0x52, wValue = 0x0000, wIndex = 0x0002).
    let ctrl_iface_num = device
        .active_configuration()
        .ok()
        .and_then(|c| c.interfaces().next().map(|i| i.interface_number()))
        .unwrap_or(0);

    let ctrl_iface = device
        .detach_and_claim_interface(ctrl_iface_num)
        .wait()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("Claim interface {ctrl_iface_num} for QuickTime mode switch failed: {e}"),
            )
        })?;

    ctrl_iface
        .control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: QT_SWITCH_REQUEST,
                value: QT_SWITCH_VALUE,
                index: QT_SWITCH_INDEX,
                data: &[],
            },
            Duration::from_secs(2),
        )
        .wait()
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("QuickTime mode-switch control transfer failed: {e}"),
            )
        })?;

    drop(ctrl_iface);
    drop(device);

    // Wait for USB re-enumeration with the QuickTime bulk interface active.
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(250));
        if let Ok((re_info, re_name)) = find_apple() {
            if let Ok(re_dev) = re_info.open().wait() {
                if let Some((iface_num, ep_in, ep_out)) = find_quicktime_endpoints(&re_dev) {
                    let interface = re_dev
                        .detach_and_claim_interface(iface_num)
                        .wait()
                        .map_err(|e| {
                            io::Error::new(io::ErrorKind::Other, format!("Claim QT iface: {e}"))
                        })?;
                    return Ok((re_name, re_dev, interface, ep_in, ep_out));
                }
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Apple iOS device did not expose QuickTime bulk interface after mode switch",
    ))
}

fn send_bulk_frame(
    ep_out: &mut Endpoint<Bulk, Out>,
    magic: u32,
    payload: &[u8],
) -> io::Result<()> {
    let frame = build_coremedia_frame(magic, payload);
    let completion = ep_out.transfer_blocking(frame.into(), Duration::from_secs(2));
    completion
        .into_result()
        .map(|_| ())
        .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, format!("Bulk OUT error: {e}")))
}

fn send_sync_reply(
    ep_out: &mut Endpoint<Bulk, Out>,
    correlation_id: &[u8],
    extra: &[u8],
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(correlation_id.len() + 4 + extra.len());
    payload.extend_from_slice(correlation_id);
    payload.extend_from_slice(&0u32.to_le_bytes()); // status = 0 (OK)
    payload.extend_from_slice(extra);
    send_bulk_frame(ep_out, MAGIC_RPLY, &payload)
}

fn send_asyn_packet(
    ep_out: &mut Endpoint<Bulk, Out>,
    sub_magic: u32,
    body: &[u8],
) -> io::Result<()> {
    let mut payload = Vec::with_capacity(12 + body.len());
    payload.extend_from_slice(&0u64.to_le_bytes()); // clock/object ref
    payload.extend_from_slice(&sub_magic.to_le_bytes());
    payload.extend_from_slice(body);
    send_bulk_frame(ep_out, MAGIC_ASYN, &payload)
}

fn run_coremedia_loop(
    mut ep_in: Endpoint<Bulk, In>,
    mut ep_out: Endpoint<Bulk, Out>,
    target_fps: u32,
    shared: Arc<SharedQueue>,
    shutdown: Arc<AtomicBool>,
) {
    let max_pkt = ep_in.max_packet_size().max(512);
    let read_chunk = ((16 * 1024) / max_pkt).max(1) * max_pkt;

    let mut rx_buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut nal_length_size: usize = 4;
    let mut synthetic_pts_us: u64 = 0;
    let frame_step_us: u64 = 1_000_000 / (target_fps.max(1) as u64);
    let mut plugged_virtual_devices = false;

    while !shutdown.load(Ordering::Acquire) {
        let completion = ep_in.transfer_blocking(Buffer::new(read_chunk), Duration::from_millis(250));
        match completion.into_result() {
            Ok(data) => {
                if !data.is_empty() {
                    rx_buf.extend_from_slice(&data);
                }
            }
            Err(TransferError::Cancelled) => continue,
            Err(e) => {
                shared.close(Some(format!("iOS USB bulk read error: {e}")));
                return;
            }
        }

        loop {
            match parse_coremedia_frame(&rx_buf) {
                Ok(Some((frame, consumed))) => {
                    let magic = frame.magic;
                    let payload = frame.payload.to_vec();
                    rx_buf.drain(..consumed);

                    match magic {
                        MAGIC_PING => {
                            if let Err(e) = send_bulk_frame(&mut ep_out, MAGIC_PING, &payload) {
                                shared.close(Some(e.to_string()));
                                return;
                            }
                        }
                        MAGIC_SYNC => {
                            let corr = if payload.len() >= 8 {
                                &payload[..8]
                            } else {
                                &[0u8; 8]
                            };
                            let sub_magic = if payload.len() >= 12 {
                                u32::from_le_bytes([
                                    payload[8],
                                    payload[9],
                                    payload[10],
                                    payload[11],
                                ])
                            } else {
                                0
                            };

                            let extra: &[u8] = match sub_magic {
                                MAGIC_CWPA => &1u64.to_le_bytes(),
                                MAGIC_OG | MAGIC_GOOG => &MAGIC_GOOG.to_le_bytes(),
                                MAGIC_AFMT | MAGIC_CFMT => &[],
                                _ => &[],
                            };
                            if let Err(e) = send_sync_reply(&mut ep_out, corr, extra) {
                                shared.close(Some(e.to_string()));
                                return;
                            }

                            if !plugged_virtual_devices && sub_magic == MAGIC_CWPA {
                                plugged_virtual_devices = true;
                                let _ = send_asyn_packet(
                                    &mut ep_out,
                                    MAGIC_HPD1,
                                    &target_fps.to_le_bytes(),
                                );
                                let _ = send_asyn_packet(&mut ep_out, MAGIC_HPA1, &[]);
                                let _ = send_asyn_packet(&mut ep_out, MAGIC_NEED, &[]);
                            }
                        }
                        MAGIC_ASYN => {
                            let sub_magic = if payload.len() >= 12 {
                                u32::from_le_bytes([
                                    payload[8],
                                    payload[9],
                                    payload[10],
                                    payload[11],
                                ])
                            } else if payload.len() >= 4 {
                                u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
                            } else {
                                0
                            };

                            if sub_magic == MAGIC_TIME {
                                let _ = send_asyn_packet(&mut ep_out, MAGIC_TIME, &[]);
                            } else if sub_magic == MAGIC_FEED && payload.len() > 12 {
                                process_feed_payload(
                                    &payload[12..],
                                    &mut nal_length_size,
                                    &mut synthetic_pts_us,
                                    frame_step_us,
                                    &shared,
                                );
                                let _ = send_asyn_packet(&mut ep_out, MAGIC_EAT, &[]);
                            }
                        }
                        MAGIC_FEED => {
                            process_feed_payload(
                                &payload,
                                &mut nal_length_size,
                                &mut synthetic_pts_us,
                                frame_step_us,
                                &shared,
                            );
                            let _ = send_asyn_packet(&mut ep_out, MAGIC_EAT, &[]);
                        }
                        _ => {}
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    shared.close(Some(format!("CoreMedia framing error: {e}")));
                    return;
                }
            }
        }
    }

    shared.close(None);
}

fn process_feed_payload(
    payload: &[u8],
    nal_length_size: &mut usize,
    synthetic_pts_us: &mut u64,
    frame_step_us: u64,
    shared: &SharedQueue,
) {
    let parsed = extract_sample_buffer(payload);
    let now = Instant::now();
    let pts_us = parsed.pts_us.unwrap_or_else(|| {
        let p = *synthetic_pts_us;
        *synthetic_pts_us = synthetic_pts_us.saturating_add(frame_step_us);
        p
    });

    if let Some(avcc) = parsed.avcc {
        if let Ok((config_annexb, nls)) = parse_avcc_config(avcc) {
            *nal_length_size = nls;
            shared.push(NalPacket {
                pts_us,
                is_config: true,
                is_keyframe: false,
                is_audio: false,
                payload: Arc::from(config_annexb),
                received_at: now,
            });
        }
    }

    if let Some(sdat) = parsed.sdat {
        if parsed.is_audio && !parsed.is_video {
            if let Some(pcm) = parse_lpcm_48khz_audio(sdat) {
                shared.push(NalPacket {
                    pts_us,
                    is_config: false,
                    is_keyframe: false,
                    is_audio: true,
                    payload: Arc::from(pcm),
                    received_at: now,
                });
            }
        } else if let Ok((annex_b, is_keyframe)) = avcc_to_annexb(sdat, *nal_length_size) {
            shared.push(NalPacket {
                pts_us,
                is_config: false,
                is_keyframe,
                is_audio: false,
                payload: Arc::from(annex_b),
                received_at: now,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coremedia_frame_parsing_ping_sync_feed() {
        let ping_bytes = build_coremedia_frame(MAGIC_PING, &[1, 2, 3, 4]);
        let (frame, consumed) = parse_coremedia_frame(&ping_bytes)
            .expect("valid ping frame")
            .expect("complete ping frame");
        assert_eq!(consumed, 12);
        assert_eq!(frame.total_len, 12);
        assert_eq!(frame.magic, MAGIC_PING);
        assert_eq!(frame.payload, &[1, 2, 3, 4]);

        let sync_bytes = build_coremedia_frame(MAGIC_SYNC, b"cwpa_payload");
        let (frame, consumed) = parse_coremedia_frame(&sync_bytes)
            .expect("valid sync frame")
            .expect("complete sync frame");
        assert_eq!(consumed, 20);
        assert_eq!(frame.magic, MAGIC_SYNC);
        assert_eq!(frame.payload, b"cwpa_payload");

        let feed_bytes = build_coremedia_frame(MAGIC_FEED, &[0xAA, 0xBB]);
        // Partial buffer returns Ok(None)
        assert!(parse_coremedia_frame(&feed_bytes[..7]).unwrap().is_none());
        assert!(parse_coremedia_frame(&feed_bytes[..9]).unwrap().is_none());

        let (frame, consumed) = parse_coremedia_frame(&feed_bytes)
            .expect("valid feed frame")
            .expect("complete feed frame");
        assert_eq!(consumed, 10);
        assert_eq!(frame.magic, MAGIC_FEED);
        assert_eq!(frame.payload, &[0xAA, 0xBB]);
    }

    #[test]
    fn test_coremedia_frame_rejects_invalid_lengths() {
        // Length < 8
        let too_small = [4, 0, 0, 0, b'g', b'n', b'i', b'p'];
        assert!(parse_coremedia_frame(&too_small).is_err());

        // Length > MAX_COREMEDIA_FRAME_LEN
        let mut too_large = Vec::new();
        too_large.extend_from_slice(&((MAX_COREMEDIA_FRAME_LEN as u32) + 1).to_le_bytes());
        too_large.extend_from_slice(&MAGIC_FEED.to_le_bytes());
        assert!(parse_coremedia_frame(&too_large).is_err());
    }

    #[test]
    fn test_avcc_config_to_annexb_sps_pps() {
        let sps = [0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9];
        let pps = [0x68, 0xEE, 0x3C, 0x80];

        let mut avcc = vec![
            1,    // configurationVersion
            0x64, // AVCProfileIndication
            0x00, // profile_compatibility
            0x1F, // AVCLevelIndication
            0xFF, // lengthSizeMinusOne = 3 -> 4-byte NAL length
            0xE1, // numOfSequenceParameterSets = 1
        ];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1); // numOfPictureParameterSets = 1
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);

        let (annexb, nls) = parse_avcc_config(&avcc).expect("valid avcC");
        assert_eq!(nls, 4);

        let mut expected = Vec::new();
        expected.extend_from_slice(&ANNEX_B_START_CODE);
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&ANNEX_B_START_CODE);
        expected.extend_from_slice(&pps);
        assert_eq!(annexb, expected);
    }

    #[test]
    fn test_avcc_to_annexb_and_idr_detection() {
        // Non-IDR slice (NAL type 1: 0x41 & 0x1F == 1)
        let non_idr = [0x41, 0x9A, 0x01];
        let mut sample_non_idr = Vec::new();
        sample_non_idr.extend_from_slice(&(non_idr.len() as u32).to_be_bytes());
        sample_non_idr.extend_from_slice(&non_idr);

        let (out, is_keyframe) = avcc_to_annexb(&sample_non_idr, 4).expect("valid non-IDR");
        assert!(!is_keyframe);
        assert_eq!(out, [0, 0, 0, 1, 0x41, 0x9A, 0x01]);

        // IDR slice (NAL type 5: 0x65 & 0x1F == 5) preceded by SEI (NAL type 6)
        let sei = [0x06, 0x05, 0x01];
        let idr = [0x65, 0x88, 0x84, 0x21];
        let mut sample_idr = Vec::new();
        sample_idr.extend_from_slice(&(sei.len() as u32).to_be_bytes());
        sample_idr.extend_from_slice(&sei);
        sample_idr.extend_from_slice(&(idr.len() as u32).to_be_bytes());
        sample_idr.extend_from_slice(&idr);

        let (out_idr, is_keyframe_idr) = avcc_to_annexb(&sample_idr, 4).expect("valid IDR");
        assert!(is_keyframe_idr);
        assert_eq!(
            out_idr,
            [0, 0, 0, 1, 0x06, 0x05, 0x01, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]
        );
    }

    #[test]
    fn test_lpcm_48khz_alignment_and_probe_defaults() {
        assert_eq!(parse_lpcm_48khz_audio(&[1, 2, 3]), None);
        assert_eq!(
            parse_lpcm_48khz_audio(&[1, 2, 3, 4, 5, 6]),
            Some(vec![1, 2, 3, 4])
        );

        let probe_none = IosUsbProbe::NoDevice;
        let probe_ready = IosUsbProbe::Ready {
            serial: "00008110".to_string(),
            product_name: "iPhone".to_string(),
        };
        assert_ne!(probe_none, probe_ready);

        let header = StreamHeader {
            codec_id: CODEC_ID_H264,
            width: 0,
            height: 0,
            framed: true,
        };
        assert_eq!(header.codec_id, CODEC_ID_H264);
        assert!(header.framed);
    }
}
