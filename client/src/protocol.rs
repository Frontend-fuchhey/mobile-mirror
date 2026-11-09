// [Mobile Mirror] Progressive implementation phase 3/4
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

/// Bidirectional control channel back to the Android server (e.g. toggling phone microphone).
pub struct ControlWriter {
    stream: Option<TcpStream>,
}

impl ControlWriter {
    pub fn set_mic_enabled(&mut self, enabled: bool) {
        if let Some(ref mut s) = self.stream {
            let cmd: [u8; 1] = [if enabled { 1 } else { 0 }];
            let _ = s.write_all(&cmd);
            let _ = s.flush();
        }
    }

    pub fn request_keyframe(&mut self) {
        if let Some(ref mut s) = self.stream {
            let cmd: [u8; 1] = [b'K'];
            let _ = s.write_all(&cmd);
            let _ = s.flush();
        }
    }
}

pub const CODEC_ID_H264: u32 = 0x6832_3634; // "h264"
pub const PACKET_FLAG_CONFIG: u64 = 1 << 63;
pub const PACKET_FLAG_KEY_FRAME: u64 = 1 << 62;
pub const PACKET_FLAG_AUDIO: u64 = 1 << 61;
pub const PTS_MASK: u64 = !(PACKET_FLAG_CONFIG | PACKET_FLAG_KEY_FRAME | PACKET_FLAG_AUDIO);

/// Initial stream geometry negotiated during handshake.
#[derive(Debug, Clone, Copy)]
pub struct StreamHeader {
    pub codec_id: u32,
    pub width: u32,
    pub height: u32,
    pub framed: bool,
}

/// Reference-counted H.264 NAL or PCM audio packet shared zero-copy between the MP4 muxer,
/// the real-time audio playback device, and the hardware video decoder pipeline.
#[derive(Debug, Clone)]
pub struct NalPacket {
    pub pts_us: u64,
    pub is_config: bool,
    pub is_keyframe: bool,
    pub is_audio: bool,
    pub payload: Arc<[u8]>,
    pub received_at: Instant,
}

/// Connects to the forwarded ADB server socket and reads framed or raw Annex-B NAL packets.
pub struct StreamReader<R: Read> {
    reader: R,
    header: StreamHeader,
    raw_buffer: Vec<u8>,
    synthetic_pts_us: u64,
    control_stream: Option<TcpStream>,
}

impl StreamReader<TcpStream> {
    pub fn connect(addr: &str) -> io::Result<Self> {
        let max_attempts = 35;
        let mut last_err = io::Error::new(io::ErrorKind::ConnectionRefused, "Failed to connect");
        for attempt in 1..=max_attempts {
            match TcpStream::connect(addr) {
                Ok(stream) => match Self::from_tcp_stream(stream) {
                    Ok(reader) => return Ok(reader),
                    Err(e) => {
                        last_err = e;
                        if attempt < max_attempts {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                        }
                    }
                },
                Err(e) => {
                    last_err = e;
                    if attempt < max_attempts {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                }
            }
        }
        Err(last_err)
    }

    /// Initializes a `StreamReader` from an already-connected `TcpStream`
    /// over the local ADB USB port-forwarded tunnel.
    pub fn from_tcp_stream(stream: TcpStream) -> io::Result<Self> {
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(3000)));
        let control_clone = stream.try_clone().ok();
        let mut reader = Self::new(stream)?;
        // Clear read timeout so the live stream never disconnects on idle frames
        let _ = reader.reader.set_read_timeout(None);
        reader.control_stream = control_clone;
        Ok(reader)
    }

    pub fn take_control_writer(&mut self) -> ControlWriter {
        ControlWriter {
            stream: self.control_stream.take(),
        }
    }
}

impl<R: Read> StreamReader<R> {
    pub fn new(mut reader: R) -> io::Result<Self> {
        let mut first4 = [0u8; 4];
        reader.read_exact(&mut first4)?;
        let magic = u32::from_be_bytes(first4);

        if magic == CODEC_ID_H264 {
            let mut dims = [0u8; 8];
            reader.read_exact(&mut dims)?;
            let width = u32::from_be_bytes([dims[0], dims[1], dims[2], dims[3]]);
            let height = u32::from_be_bytes([dims[4], dims[5], dims[6], dims[7]]);
            Ok(Self {
                reader,
                header: StreamHeader {
                    codec_id: magic,
                    width,
                    height,
                    framed: true,
                },
                raw_buffer: Vec::new(),
                synthetic_pts_us: 0,
                control_stream: None,
            })
        } else {
            // Raw Annex-B stream mode: seed the internal buffer with the first 4 bytes
            let mut raw_buffer = Vec::with_capacity(256 * 1024);
            raw_buffer.extend_from_slice(&first4);
            Ok(Self {
                reader,
                header: StreamHeader {
                    codec_id: CODEC_ID_H264,
                    width: 1080,
                    height: 2400,
                    framed: false,
                },
                raw_buffer,
                synthetic_pts_us: 0,
                control_stream: None,
            })
        }
    }

    pub fn header(&self) -> StreamHeader {
        self.header
    }

    /// Reads the next H.264 NAL packet from the socket.
    /// Returns `Ok(None)` on clean EOF.
    pub fn next_packet(&mut self) -> io::Result<Option<Arc<NalPacket>>> {
        if self.header.framed {
            self.read_framed_packet()
        } else {
            self.read_raw_annexb_packet()
        }
    }

    fn read_framed_packet(&mut self) -> io::Result<Option<Arc<NalPacket>>> {
        let mut meta = [0u8; 12];
        match read_exact_or_eof(&mut self.reader, &mut meta)? {
            false => return Ok(None),
            true => {}
        }

        let pts_and_flags = u64::from_be_bytes([
            meta[0], meta[1], meta[2], meta[3], meta[4], meta[5], meta[6], meta[7],
        ]);
        let size = u32::from_be_bytes([meta[8], meta[9], meta[10], meta[11]]) as usize;

        if size == 0 || size > 64 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid NAL packet size: {} bytes", size),
            ));
        }

        let mut buf = vec![0u8; size];
        self.reader.read_exact(&mut buf)?;

        let is_config = (pts_and_flags & PACKET_FLAG_CONFIG) != 0;
        let is_keyframe = (pts_and_flags & PACKET_FLAG_KEY_FRAME) != 0;
        let is_audio = (pts_and_flags & PACKET_FLAG_AUDIO) != 0;
        let pts_us = if is_config {
            0
        } else {
            pts_and_flags & PTS_MASK
        };

        Ok(Some(Arc::new(NalPacket {
            pts_us,
            is_config,
            is_keyframe,
            is_audio,
            payload: Arc::from(buf),
            received_at: Instant::now(),
        })))
    }

    fn read_raw_annexb_packet(&mut self) -> io::Result<Option<Arc<NalPacket>>> {
        let mut temp = [0u8; 32 * 1024];
        loop {
            if let Some(split_idx) = find_next_annexb_unit_boundary(&self.raw_buffer) {
                let unit: Vec<u8> = self.raw_buffer.drain(..split_idx).collect();
                let (is_config, is_keyframe) = classify_annexb_payload(&unit);
                let pts_us = if is_config {
                    0
                } else {
                    let pts = self.synthetic_pts_us;
                    self.synthetic_pts_us += 16_667; // 60 FPS (16.667 ms)
                    pts
                };
                return Ok(Some(Arc::new(NalPacket {
                    pts_us,
                    is_config,
                    is_keyframe,
                    is_audio: false,
                    payload: Arc::from(unit),
                    received_at: Instant::now(),
                })));
            }

            let n = self.reader.read(&mut temp)?;
            if n == 0 {
                if self.raw_buffer.is_empty() {
                    return Ok(None);
                }
                let unit: Vec<u8> = self.raw_buffer.drain(..).collect();
                let (is_config, is_keyframe) = classify_annexb_payload(&unit);
                let pts_us = if is_config { 0 } else { self.synthetic_pts_us };
                return Ok(Some(Arc::new(NalPacket {
                    pts_us,
                    is_config,
                    is_keyframe,
                    is_audio: false,
                    payload: Arc::from(unit),
                    received_at: Instant::now(),
                })));
            }
            self.raw_buffer.extend_from_slice(&temp[..n]);
        }
    }
}

fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => {
                if read == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF in packet header",
                ));
            }
            Ok(n) => read += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Scans an Annex-B byte buffer for individual NAL units (without start codes).
pub fn split_annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut starts = Vec::new();

    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, i + 3));
                i += 3;
                continue;
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }

    for idx in 0..starts.len() {
        let payload_start = starts[idx].1;
        let payload_end = if idx + 1 < starts.len() {
            starts[idx + 1].0
        } else {
            data.len()
        };
        if payload_start < payload_end {
            nals.push(&data[payload_start..payload_end]);
        }
    }
    nals
}

/// Extracts SPS (NAL type 7) and PPS (NAL type 8) from an Annex-B buffer.
pub fn extract_sps_pps(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for nal in split_annexb_nals(data) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        if nal_type == 7 && sps.is_none() {
            sps = Some(nal.to_vec());
        } else if nal_type == 8 && pps.is_none() {
            pps = Some(nal.to_vec());
        }
    }
    (sps, pps)
}

/// Converts Annex-B start-code prefixed NAL units into 4-byte big-endian length-prefixed AVCC format
/// suitable for MP4 `mdat` sample payloads (filtering out SPS/PPS config NALs if desired).
pub fn annexb_to_avcc_sample(data: &[u8]) -> Vec<u8> {
    let nals = split_annexb_nals(data);
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nals {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        // Skip SPS (7) and PPS (8) in sample data when stored in avcC box
        if nal_type == 7 || nal_type == 8 {
            continue;
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn first_nal_type(buf: &[u8]) -> Option<u8> {
    if buf.len() >= 4 && buf[0] == 0 && buf[1] == 0 && buf[2] == 1 {
        Some(buf[3] & 0x1F)
    } else if buf.len() >= 5 && buf[0] == 0 && buf[1] == 0 && buf[2] == 0 && buf[3] == 1 {
        Some(buf[4] & 0x1F)
    } else {
        None
    }
}

fn find_next_annexb_unit_boundary(buf: &[u8]) -> Option<usize> {
    if buf.len() < 8 {
        return None;
    }
    let first_is_config = matches!(first_nal_type(buf), Some(7 | 8));
    let mut i = 4;
    while i + 4 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            let next_hdr_idx = if buf[i + 2] == 1 {
                Some(i + 3)
            } else if buf[i + 2] == 0 && buf[i + 3] == 1 {
                Some(i + 4)
            } else {
                None
            };
            if let Some(hdr_idx) = next_hdr_idx {
