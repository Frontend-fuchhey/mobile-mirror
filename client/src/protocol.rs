// [Mobile Mirror] Progressive implementation phase 1/4
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
