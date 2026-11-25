//! Pure-Rust AirPlay Screen Mirroring receiver.
//!
//! Implements:
//! - Raw UDP mDNS / Bonjour responder (`224.0.0.251:5353`) advertising `_airplay._tcp.local.`
//!   and `_raop._tcp.local.`.
//! - AirPlay HTTP/1.1 + RTSP/1.0 server (`0.0.0.0:7000`, fallback ephemeral port) handling
//!   `/info`, `/pair-setup`, `/pair-verify`, `/fp-setup`, `/stream`, and RTSP `OPTIONS`/`SETUP`/
//!   `RECORD`/`SET_PARAMETER`/`GET_PARAMETER`/`FLUSH`/`TEARDOWN`.
//! - AirPlay mirroring video stream parser (`AirPlaySession` implementing `PacketSource`)
//!   that reads 128-byte AirPlay packet headers, optionally decrypts AES-128-CTR payloads,
//!   and converts `avcC` + AVCC length-prefixed H.264 NALUs to Annex-B `NalPacket`s.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use plist::{Dictionary, Value};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::protocol::{
    ControlWriter, DevicePlatform, NalPacket, PacketSource, StreamHeader, CODEC_ID_H264,
};

const MDNS_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;
const DEFAULT_AIRPLAY_PORT: u16 = 7000;
const DEFAULT_VIDEO_PORT: u16 = 7100;
const MAX_VIDEO_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;
const AIRPLAY_FEATURES: u64 = 0x1E_5A7FFFF7;

const DNS_TYPE_A: u16 = 1;
const DNS_TYPE_PTR: u16 = 12;
const DNS_TYPE_TXT: u16 = 16;
const DNS_TYPE_SRV: u16 = 33;
const DNS_TYPE_ANY: u16 = 255;
const DNS_CLASS_IN: u16 = 1;
const DNS_CLASS_IN_FLUSH: u16 = 0x8001;

/// Device identity advertised via mDNS and `/info` and used for Ed25519 signing.
#[derive(Clone)]
struct DeviceIdentity {
    device_name: String,
    hostname: String,
    mac_colon: String,
    mac_hex: String,
    uuid: String,
    signing_key: SigningKey,
    verifying_key_bytes: [u8; 32],
    pk_hex: String,
    local_ip: Ipv4Addr,
    port: u16,
}

impl DeviceIdentity {
    fn generate(device_name: &str, port: u16) -> Self {
        let mut rng = OsRng;
        let mut mac = [0u8; 6];
        rng.fill_bytes(&mut mac);
        // Locally administered unicast MAC address
        mac[0] = (mac[0] & 0xFC) | 0x02;

        let mac_colon = format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
        let mac_hex = format!(
            "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        let mut uuid_bytes = [0u8; 16];
        rng.fill_bytes(&mut uuid_bytes);
        uuid_bytes[6] = (uuid_bytes[6] & 0x0F) | 0x40;
        uuid_bytes[8] = (uuid_bytes[8] & 0x3F) | 0x80;
        let uuid = format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3],
            uuid_bytes[4], uuid_bytes[5], uuid_bytes[6], uuid_bytes[7],
            uuid_bytes[8], uuid_bytes[9], uuid_bytes[10], uuid_bytes[11],
            uuid_bytes[12], uuid_bytes[13], uuid_bytes[14], uuid_bytes[15]
        );

        let signing_key = SigningKey::generate(&mut rng);
        let verifying_key_bytes = signing_key.verifying_key().to_bytes();
        let pk_hex = bytes_to_hex_lower(&verifying_key_bytes);

        let sanitized_host: String = device_name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        let host_label = if sanitized_host.is_empty() {
            "mobile-mirror".to_string()
        } else {
            sanitized_host
        };
        let hostname = format!("{}.local.", host_label);
        let local_ip = detect_local_ipv4();

        Self {
            device_name: device_name.to_string(),
            hostname,
            mac_colon,
            mac_hex,
            uuid,
            signing_key,
            verifying_key_bytes,
            pk_hex,
            local_ip,
            port,
        }
    }

    fn airplay_instance_name(&self) -> String {
        format!("{}._airplay._tcp.local.", self.device_name)
    }

    fn raop_instance_name(&self) -> String {
        format!("{}@{}._raop._tcp.local.", self.mac_hex, self.device_name)
    }

    fn airplay_txt_records(&self) -> Vec<String> {
        vec![
            format!("deviceid={}", self.mac_colon),
            "features=0x5A7FFFF7,0x1E".to_string(),
            "flags=0x4".to_string(),
            "model=AppleTV3,2".to_string(),
            format!("pi={}", self.uuid),
            format!("pk={}", self.pk_hex),
            "srcvers=220.68".to_string(),
            "vv=2".to_string(),
        ]
    }

    fn raop_txt_records(&self) -> Vec<String> {
        vec![
            "ch=2".to_string(),
            "cn=0,1,2,3".to_string(),
            "da=true".to_string(),
            "et=0,3,5".to_string(),
            "ft=0x5A7FFFF7,0x1E".to_string(),
            "md=0,1,2".to_string(),
            "sf=0x4".to_string(),
            "sr=44100".to_string(),
            "ss=16".to_string(),
            "tp=UDP".to_string(),
            "vn=65537".to_string(),
            "vs=220.68".to_string(),
            "vv=2".to_string(),
        ]
    }
}

fn bytes_to_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn detect_local_ipv4() -> Ipv4Addr {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(SocketAddr::V4(addr)) = sock.local_addr() {
                if !addr.ip().is_unspecified() && !addr.ip().is_loopback() {
                    return *addr.ip();
                }
            }
        }
    }
    Ipv4Addr::new(127, 0, 0, 1)
}

// ---------------------------------------------------------------------------
// AirPlay Server Public API
// ---------------------------------------------------------------------------

/// Pure-Rust AirPlay Screen Mirroring server (mDNS responder + HTTP/RTSP server).
pub struct AirPlayServer {
    port: u16,
    running: Arc<AtomicBool>,
    session_rx: Mutex<mpsc::Receiver<AirPlaySession>>,
    mdns_thread: Option<JoinHandle<()>>,
    http_thread: Option<JoinHandle<()>>,
}

impl AirPlayServer {
    /// Starts the AirPlay mDNS responder and HTTP/RTSP server.
    ///
    /// Binds to `0.0.0.0:7000` (falling back to an ephemeral port if 7000 is busy).
    pub fn start(device_name: &str) -> io::Result<Self> {
        let listener = match TcpListener::bind(("0.0.0.0", DEFAULT_AIRPLAY_PORT)) {
            Ok(l) => l,
            Err(_) => TcpListener::bind(("0.0.0.0", 0))?,
        };
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let identity = Arc::new(DeviceIdentity::generate(device_name, port));
        let running = Arc::new(AtomicBool::new(true));
        let (session_tx, session_rx) = mpsc::channel::<AirPlaySession>();

        let mdns_running = Arc::clone(&running);
        let mdns_identity = Arc::clone(&identity);
        let mdns_thread = thread::Builder::new()
            .name("airplay-mdns".to_string())
            .spawn(move || run_mdns_responder(mdns_identity, mdns_running))?;

        let http_running = Arc::clone(&running);
        let http_identity = Arc::clone(&identity);
        let http_thread = thread::Builder::new()
            .name("airplay-rtsp".to_string())
            .spawn(move || run_http_rtsp_server(listener, http_identity, http_running, session_tx))?;

        Ok(Self {
            port,
            running,
            session_rx: Mutex::new(session_rx),
            mdns_thread: Some(mdns_thread),
            http_thread: Some(http_thread),
        })
    }

    /// Non-blocking check for a newly connected AirPlay mirroring video session.
    pub fn try_accept_session(&self) -> Option<AirPlaySession> {
        self.session_rx.lock().ok()?.try_recv().ok()
    }

    /// Returns the TCP port the AirPlay HTTP/RTSP server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Gracefully stops the mDNS responder and HTTP/RTSP server threads.
    pub fn stop(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.mdns_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.http_thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AirPlayServer {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

// ---------------------------------------------------------------------------
// mDNS / DNS-SD Wire Encoding & Responder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct DnsQuestion {
    name: String,
    qtype: u16,
    qclass: u16,
}

#[derive(Debug, Clone)]
struct DnsRecord {
    name: String,
    rtype: u16,
    rclass: u16,
    ttl: u32,
    rdata: Vec<u8>,
}

fn encode_dns_name(name: &str, out: &mut Vec<u8>) {
    let trimmed = name.trim_end_matches('.');
    if !trimmed.is_empty() {
        for label in trimmed.split('.') {
            let bytes = label.as_bytes();
            let len = bytes.len().min(63);
            out.push(len as u8);
            out.extend_from_slice(&bytes[..len]);
        }
    }
    out.push(0);
}

fn decode_dns_name(packet: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut jumped = false;
    let mut end_offset = offset;
    let mut jumps = 0;

    loop {
        if offset >= packet.len() || jumps > 16 {
            return None;
        }
        let len = packet[offset];
        if len == 0 {
            if !jumped {
                end_offset = offset + 1;
            }
            break;
        }
        if (len & 0xC0) == 0xC0 {
            if offset + 1 >= packet.len() {
                return None;
            }
            let ptr = (((len & 0x3F) as usize) << 8) | (packet[offset + 1] as usize);
            if !jumped {
                end_offset = offset + 2;
            }
            offset = ptr;
            jumped = true;
            jumps += 1;
        } else {
            let label_len = len as usize;
            let start = offset + 1;
            let end = start + label_len;
            if end > packet.len() {
                return None;
            }
            let label = String::from_utf8_lossy(&packet[start..end]).into_owned();
            labels.push(label);
            offset = end;
            if !jumped {
                end_offset = end;
            }
        }
    }

    let mut full = labels.join(".");
    if !full.is_empty() {
        full.push('.');
    }
    Some((full, end_offset))
}

fn encode_ptr_rdata(target: &str) -> Vec<u8> {
    let mut rdata = Vec::new();
    encode_dns_name(target, &mut rdata);
    rdata
}

fn encode_txt_rdata(entries: &[String]) -> Vec<u8> {
    let mut rdata = Vec::new();
    if entries.is_empty() {
        rdata.push(0);
        return rdata;
    }
    for entry in entries {
        let bytes = entry.as_bytes();
        let len = bytes.len().min(255);
        rdata.push(len as u8);
        rdata.extend_from_slice(&bytes[..len]);
    }
    rdata
}

fn encode_srv_rdata(priority: u16, weight: u16, port: u16, target: &str) -> Vec<u8> {
    let mut rdata = Vec::with_capacity(6 + target.len() + 2);
    rdata.extend_from_slice(&priority.to_be_bytes());
    rdata.extend_from_slice(&weight.to_be_bytes());
    rdata.extend_from_slice(&port.to_be_bytes());
    encode_dns_name(target, &mut rdata);
    rdata
}

fn encode_a_rdata(ip: Ipv4Addr) -> Vec<u8> {
    ip.octets().to_vec()
}

fn encode_dns_record(rec: &DnsRecord, out: &mut Vec<u8>) {
    encode_dns_name(&rec.name, out);
    out.extend_from_slice(&rec.rtype.to_be_bytes());
    out.extend_from_slice(&rec.rclass.to_be_bytes());
    out.extend_from_slice(&rec.ttl.to_be_bytes());
    out.extend_from_slice(&(rec.rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(&rec.rdata);
}

fn build_mdns_response_packet(answers: &[DnsRecord], additionals: &[DnsRecord]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(512);
    pkt.extend_from_slice(&0u16.to_be_bytes()); // Transaction ID = 0
    pkt.extend_from_slice(&0x8400u16.to_be_bytes()); // Standard query response, authoritative
    pkt.extend_from_slice(&0u16.to_be_bytes()); // QDCOUNT
    pkt.extend_from_slice(&(answers.len() as u16).to_be_bytes()); // ANCOUNT
    pkt.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    pkt.extend_from_slice(&(additionals.len() as u16).to_be_bytes()); // ARCOUNT

    for rec in answers {
        encode_dns_record(rec, &mut pkt);
    }
    for rec in additionals {
        encode_dns_record(rec, &mut pkt);
    }
    pkt
}

fn parse_mdns_queries(packet: &[u8]) -> Option<Vec<DnsQuestion>> {
    if packet.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    // QR bit == 0 means query
    if (flags & 0x8000) != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    let mut offset = 12;
    let mut questions = Vec::with_capacity(qdcount.min(16));
    for _ in 0..qdcount {
        let (name, next) = decode_dns_name(packet, offset)?;
        if next + 4 > packet.len() {
            return None;
        }
        let qtype = u16::from_be_bytes([packet[next], packet[next + 1]]);
        let qclass = u16::from_be_bytes([packet[next + 2], packet[next + 3]]) & 0x7FFF;
        questions.push(DnsQuestion {
            name: name.to_ascii_lowercase(),
            qtype,
            qclass,
        });
        offset = next + 4;
    }
    Some(questions)
}

fn build_airplay_announcement_packet(id: &DeviceIdentity) -> Vec<u8> {
    let airplay_inst = id.airplay_instance_name();
    let raop_inst = id.raop_instance_name();

    let answers = vec![
        DnsRecord {
            name: "_services._dns-sd._udp.local.".to_string(),
            rtype: DNS_TYPE_PTR,
            rclass: DNS_CLASS_IN,
            ttl: 4500,
            rdata: encode_ptr_rdata("_airplay._tcp.local."),
        },
        DnsRecord {
            name: "_services._dns-sd._udp.local.".to_string(),
            rtype: DNS_TYPE_PTR,
            rclass: DNS_CLASS_IN,
            ttl: 4500,
            rdata: encode_ptr_rdata("_raop._tcp.local."),
        },
        DnsRecord {
            name: "_airplay._tcp.local.".to_string(),
            rtype: DNS_TYPE_PTR,
            rclass: DNS_CLASS_IN,
            ttl: 4500,
            rdata: encode_ptr_rdata(&airplay_inst),
        },
        DnsRecord {
            name: airplay_inst.clone(),
            rtype: DNS_TYPE_SRV,
            rclass: DNS_CLASS_IN_FLUSH,
            ttl: 120,
            rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
        },
        DnsRecord {
            name: airplay_inst,
            rtype: DNS_TYPE_TXT,
            rclass: DNS_CLASS_IN_FLUSH,
            ttl: 4500,
            rdata: encode_txt_rdata(&id.airplay_txt_records()),
        },
        DnsRecord {
            name: "_raop._tcp.local.".to_string(),
            rtype: DNS_TYPE_PTR,
            rclass: DNS_CLASS_IN,
            ttl: 4500,
            rdata: encode_ptr_rdata(&raop_inst),
        },
        DnsRecord {
            name: raop_inst.clone(),
            rtype: DNS_TYPE_SRV,
            rclass: DNS_CLASS_IN_FLUSH,
            ttl: 120,
            rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
        },
        DnsRecord {
            name: raop_inst,
            rtype: DNS_TYPE_TXT,
            rclass: DNS_CLASS_IN_FLUSH,
            ttl: 4500,
            rdata: encode_txt_rdata(&id.raop_txt_records()),
        },
        DnsRecord {
            name: id.hostname.clone(),
            rtype: DNS_TYPE_A,
            rclass: DNS_CLASS_IN_FLUSH,
            ttl: 120,
            rdata: encode_a_rdata(id.local_ip),
        },
    ];

    build_mdns_response_packet(&answers, &[])
}

fn build_mdns_query_response(id: &DeviceIdentity, questions: &[DnsQuestion]) -> Option<Vec<u8>> {
    let airplay_inst = id.airplay_instance_name();
    let raop_inst = id.raop_instance_name();
    let airplay_inst_lc = airplay_inst.to_ascii_lowercase();
    let raop_inst_lc = raop_inst.to_ascii_lowercase();
    let host_lc = id.hostname.to_ascii_lowercase();

    let mut answers = Vec::new();
    let mut additionals = Vec::new();

    for q in questions {
        if q.qclass != DNS_CLASS_IN && q.qclass != DNS_TYPE_ANY {
            continue;
        }
        let matches_type = |t: u16| q.qtype == t || q.qtype == DNS_TYPE_ANY;

        if q.name == "_services._dns-sd._udp.local." && matches_type(DNS_TYPE_PTR) {
            answers.push(DnsRecord {
                name: "_services._dns-sd._udp.local.".to_string(),
                rtype: DNS_TYPE_PTR,
                rclass: DNS_CLASS_IN,
                ttl: 4500,
                rdata: encode_ptr_rdata("_airplay._tcp.local."),
            });
            answers.push(DnsRecord {
                name: "_services._dns-sd._udp.local.".to_string(),
                rtype: DNS_TYPE_PTR,
                rclass: DNS_CLASS_IN,
                ttl: 4500,
                rdata: encode_ptr_rdata("_raop._tcp.local."),
            });
        } else if q.name == "_airplay._tcp.local." && matches_type(DNS_TYPE_PTR) {
            answers.push(DnsRecord {
                name: "_airplay._tcp.local.".to_string(),
                rtype: DNS_TYPE_PTR,
                rclass: DNS_CLASS_IN,
                ttl: 4500,
                rdata: encode_ptr_rdata(&airplay_inst),
            });
            additionals.push(DnsRecord {
                name: airplay_inst.clone(),
                rtype: DNS_TYPE_SRV,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 120,
                rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
            });
            additionals.push(DnsRecord {
                name: airplay_inst.clone(),
                rtype: DNS_TYPE_TXT,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 4500,
                rdata: encode_txt_rdata(&id.airplay_txt_records()),
            });
            additionals.push(DnsRecord {
                name: id.hostname.clone(),
                rtype: DNS_TYPE_A,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 120,
                rdata: encode_a_rdata(id.local_ip),
            });
        } else if q.name == "_raop._tcp.local." && matches_type(DNS_TYPE_PTR) {
            answers.push(DnsRecord {
                name: "_raop._tcp.local.".to_string(),
                rtype: DNS_TYPE_PTR,
                rclass: DNS_CLASS_IN,
                ttl: 4500,
                rdata: encode_ptr_rdata(&raop_inst),
            });
            additionals.push(DnsRecord {
                name: raop_inst.clone(),
                rtype: DNS_TYPE_SRV,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 120,
                rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
            });
            additionals.push(DnsRecord {
                name: raop_inst.clone(),
                rtype: DNS_TYPE_TXT,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 4500,
                rdata: encode_txt_rdata(&id.raop_txt_records()),
            });
            additionals.push(DnsRecord {
                name: id.hostname.clone(),
                rtype: DNS_TYPE_A,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 120,
                rdata: encode_a_rdata(id.local_ip),
            });
        } else if q.name == airplay_inst_lc {
            if matches_type(DNS_TYPE_SRV) {
                answers.push(DnsRecord {
                    name: airplay_inst.clone(),
                    rtype: DNS_TYPE_SRV,
                    rclass: DNS_CLASS_IN_FLUSH,
                    ttl: 120,
                    rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
                });
            }
            if matches_type(DNS_TYPE_TXT) {
                answers.push(DnsRecord {
                    name: airplay_inst.clone(),
                    rtype: DNS_TYPE_TXT,
                    rclass: DNS_CLASS_IN_FLUSH,
                    ttl: 4500,
                    rdata: encode_txt_rdata(&id.airplay_txt_records()),
                });
            }
        } else if q.name == raop_inst_lc {
            if matches_type(DNS_TYPE_SRV) {
                answers.push(DnsRecord {
                    name: raop_inst.clone(),
                    rtype: DNS_TYPE_SRV,
                    rclass: DNS_CLASS_IN_FLUSH,
                    ttl: 120,
                    rdata: encode_srv_rdata(0, 0, id.port, &id.hostname),
                });
            }
            if matches_type(DNS_TYPE_TXT) {
                answers.push(DnsRecord {
                    name: raop_inst.clone(),
                    rtype: DNS_TYPE_TXT,
                    rclass: DNS_CLASS_IN_FLUSH,
                    ttl: 4500,
                    rdata: encode_txt_rdata(&id.raop_txt_records()),
                });
            }
        } else if q.name == host_lc && matches_type(DNS_TYPE_A) {
            answers.push(DnsRecord {
                name: id.hostname.clone(),
                rtype: DNS_TYPE_A,
                rclass: DNS_CLASS_IN_FLUSH,
                ttl: 120,
                rdata: encode_a_rdata(id.local_ip),
            });
        }
    }

    if answers.is_empty() {
        None
    } else {
        Some(build_mdns_response_packet(&answers, &additionals))
    }
}

fn run_mdns_responder(identity: Arc<DeviceIdentity>, running: Arc<AtomicBool>) {
    let socket = match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT)) {
        Ok(s) => s,
        Err(_) => match UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) {
            Ok(s) => s,
            Err(_) => return,
        },
    };
    let _ = socket.join_multicast_v4(&MDNS_ADDR, &Ipv4Addr::UNSPECIFIED);
    let _ = socket.set_multicast_loop_v4(true);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));

    let mcast_dest = SocketAddr::V4(SocketAddrV4::new(MDNS_ADDR, MDNS_PORT));
    let announce_pkt = build_airplay_announcement_packet(&identity);

    // Send initial unsolicited announcements on startup
    for _ in 0..3 {
        if !running.load(Ordering::Relaxed) {
            return;
        }
        let _ = socket.send_to(&announce_pkt, mcast_dest);
        thread::sleep(Duration::from_millis(200));
    }

    let mut last_announce = Instant::now();
    let mut buf = [0u8; 4096];

    while running.load(Ordering::Relaxed) {
        if last_announce.elapsed() >= Duration::from_secs(30) {
            let _ = socket.send_to(&announce_pkt, mcast_dest);
            last_announce = Instant::now();
        }

        match socket.recv_from(&mut buf) {
            Ok((n, _src)) => {
                if let Some(questions) = parse_mdns_queries(&buf[..n]) {
                    if let Some(resp) = build_mdns_query_response(&identity, &questions) {
                        let _ = socket.send_to(&resp, mcast_dest);
                    }
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP / RTSP Request Parser & Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRtspRequest {
    method: String,
    target: String,
    version: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, pos + 4));
    }
    if let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
        return Some((pos, pos + 2));
    }
    None
}

fn parse_http_rtsp_request(buf: &[u8]) -> io::Result<Option<(HttpRtspRequest, usize)>> {
    let Some((header_end, body_start)) = find_header_end(buf) else {
        return Ok(None);
    };

    let header_str = std::str::from_utf8(&buf[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid UTF-8 in HTTP/RTSP header"))?;

    let mut lines = header_str.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Empty HTTP/RTSP request line"))?;

    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing HTTP/RTSP method"))?
        .to_ascii_uppercase();
    let target = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing HTTP/RTSP target"))?
        .to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    let total_len = body_start + content_length;
    if buf.len() < total_len {
        return Ok(None);
    }

    let body = buf[body_start..total_len].to_vec();
    Ok(Some((
        HttpRtspRequest {
            method,
            target,
            version,
            headers,
            body,
        },
        total_len,
    )))
}

fn format_response(
    req: &HttpRtspRequest,
    status_code: u16,
    reason: &str,
    content_type: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let proto = if req.version.starts_with("RTSP/") {
        "RTSP/1.0"
    } else {
        "HTTP/1.1"
    };

    let mut out = format!("{} {} {}\r\n", proto, status_code, reason);
    out.push_str("Server: AirTunes/220.68\r\n");
    if let Some(cseq) = req.headers.get("cseq") {
        out.push_str(&format!("CSeq: {}\r\n", cseq));
    }
    if let Some(ct) = content_type {
        out.push_str(&format!("Content-Type: {}\r\n", ct));
    }
    for (k, v) in extra_headers {
        out.push_str(&format!("{}: {}\r\n", k, v));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));

    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

// ---------------------------------------------------------------------------
// Pair-Verify & FairPlay State
// ---------------------------------------------------------------------------

struct PairVerifyState {
    cipher: Ctr128BE<Aes128>,
    server_curve_pk: [u8; 32],
    client_curve_pk: [u8; 32],
    client_ed_pk: [u8; 32],
}

fn derive_pair_verify_aes_key_iv(shared_secret: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut key_hasher = Sha512::new();
    key_hasher.update(b"Pair-Verify-AES-Key");
    key_hasher.update(shared_secret);
    let key_digest = key_hasher.finalize();

    let mut iv_hasher = Sha512::new();
    iv_hasher.update(b"Pair-Verify-AES-IV");
    iv_hasher.update(shared_secret);
    let iv_digest = iv_hasher.finalize();

    let mut aes_key = [0u8; 16];
    let mut aes_iv = [0u8; 16];
    aes_key.copy_from_slice(&key_digest[..16]);
    aes_iv.copy_from_slice(&iv_digest[..16]);
    (aes_key, aes_iv)
}

fn handle_pair_verify_stage1(
    signing_key: &SigningKey,
    body: &[u8],
) -> Result<(Vec<u8>, PairVerifyState), &'static str> {
    if body.len() < 68 {
        return Err("pair-verify stage 1 body too short");
    }
    let mut client_curve_pk = [0u8; 32];
    client_curve_pk.copy_from_slice(&body[4..36]);
    let mut client_ed_pk = [0u8; 32];
    client_ed_pk.copy_from_slice(&body[36..68]);

    let server_secret = StaticSecret::random_from_rng(OsRng);
    let server_curve_pk = X25519PublicKey::from(&server_secret).to_bytes();
    let shared_secret = server_secret.diffie_hellman(&X25519PublicKey::from(client_curve_pk));

    let (aes_key, aes_iv) = derive_pair_verify_aes_key_iv(shared_secret.as_bytes());
    let mut cipher = Ctr128BE::<Aes128>::new(&aes_key.into(), &aes_iv.into());

    let mut msg = [0u8; 64];
    msg[..32].copy_from_slice(&server_curve_pk);
    msg[32..].copy_from_slice(&client_curve_pk);
    let mut signature = signing_key.sign(&msg).to_bytes();

    cipher.apply_keystream(&mut signature);

    let mut resp = Vec::with_capacity(96);
    resp.extend_from_slice(&server_curve_pk);
    resp.extend_from_slice(&signature);

    let state = PairVerifyState {
        cipher,
        server_curve_pk,
        client_curve_pk,
        client_ed_pk,
    };
    Ok((resp, state))
}

fn handle_pair_verify_stage2(state: &mut PairVerifyState, body: &[u8]) -> Result<(), &'static str> {
    if body.len() < 68 {
        return Err("pair-verify stage 2 body too short");
    }
    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(&body[4..68]);
    state.cipher.apply_keystream(&mut sig_bytes);

    let mut msg = [0u8; 64];
    msg[..32].copy_from_slice(&state.client_curve_pk);
    msg[32..].copy_from_slice(&state.server_curve_pk);

    let verifying_key =
        VerifyingKey::from_bytes(&state.client_ed_pk).map_err(|_| "invalid client Ed25519 key")?;
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key
        .verify(&msg, &signature)
        .map_err(|_| "client signature verification failed")
}

fn handle_fp_setup(body: &[u8]) -> Option<Vec<u8>> {
    if body.len() == 16 {
        // FairPlay stage 1 (16-byte request -> 142-byte response)
        let mut resp = vec![0u8; 142];
        resp[0..4].copy_from_slice(b"FPLY");
        resp[4] = 0x03;
        resp[5] = 0x01;
        resp[6] = 0x02;
        resp[7] = 0x00;
        resp[8..12].copy_from_slice(&130u32.to_be_bytes());
        Some(resp)
    } else if body.len() == 164 {
        // FairPlay stage 2 (164-byte request -> 32-byte response)
        let mut resp = vec![0u8; 32];
        resp[0..4].copy_from_slice(b"FPLY");
        resp[4] = 0x03;
        resp[5] = 0x01;
        resp[6] = 0x04;
        resp[7] = 0x00;
        resp[8..12].copy_from_slice(&20u32.to_be_bytes());
        resp[12..32].copy_from_slice(&body[144..164]);
        Some(resp)
    } else {
        None
    }
}

fn build_info_plist(id: &DeviceIdentity) -> io::Result<Vec<u8>> {
    let mut dict = Dictionary::new();
    dict.insert("deviceid".to_string(), Value::String(id.mac_colon.clone()));
    dict.insert("features".to_string(), Value::Integer((AIRPLAY_FEATURES as i64).into()));
    dict.insert("model".to_string(), Value::String("AppleTV3,2".to_string()));
    dict.insert("name".to_string(), Value::String(id.device_name.clone()));
    dict.insert("pi".to_string(), Value::String(id.uuid.clone()));
    dict.insert("pk".to_string(), Value::Data(id.verifying_key_bytes.to_vec()));
    dict.insert("sourceVersion".to_string(), Value::String("220.68".to_string()));
    dict.insert("statusFlags".to_string(), Value::Integer(4.into()));
    dict.insert("vv".to_string(), Value::Integer(2.into()));

    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &Value::Dictionary(dict))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    Ok(buf)
}

fn build_setup_response_plist(video_port: u16) -> io::Result<Vec<u8>> {
    let mut stream_dict = Dictionary::new();
    stream_dict.insert("type".to_string(), Value::Integer(110.into()));
    stream_dict.insert("dataPort".to_string(), Value::Integer((video_port as i64).into()));

    let mut root = Dictionary::new();
    root.insert(
        "streams".to_string(),
        Value::Array(vec![Value::Dictionary(stream_dict)]),
    );
    root.insert("eventPort".to_string(), Value::Integer(0.into()));
    root.insert("timingPort".to_string(), Value::Integer(0.into()));

    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &Value::Dictionary(root))
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    Ok(buf)
}

fn run_http_rtsp_server(
    listener: TcpListener,
    identity: Arc<DeviceIdentity>,
    running: Arc<AtomicBool>,
    session_tx: mpsc::Sender<AirPlaySession>,
) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let id = Arc::clone(&identity);
                let run = Arc::clone(&running);
                let tx = session_tx.clone();
                let _ = thread::Builder::new()
                    .name("airplay-client".to_string())
                    .spawn(move || handle_client_connection(stream, id, run, tx));
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_client_connection(
    mut stream: TcpStream,
    identity: Arc<DeviceIdentity>,
    running: Arc<AtomicBool>,
    session_tx: mpsc::Sender<AirPlaySession>,
) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));

    let mut read_buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    let mut pair_verify_state: Option<PairVerifyState> = None;
    let mut active_video_shutdown: Option<Arc<AtomicBool>> = None;

    while running.load(Ordering::Relaxed) {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => read_buf.extend_from_slice(&tmp[..n]),
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }

        loop {
            match parse_http_rtsp_request(&read_buf) {
                Ok(Some((req, consumed))) => {
                    read_buf.drain(..consumed);
                    let resp = dispatch_request(
                        &req,
                        &identity,
                        &mut pair_verify_state,
                        &mut active_video_shutdown,
                        &running,
                        &session_tx,
                    );
                    if stream.write_all(&resp).is_err() || stream.flush().is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
}

fn dispatch_request(
    req: &HttpRtspRequest,
    identity: &Arc<DeviceIdentity>,
    pair_verify_state: &mut Option<PairVerifyState>,
    active_video_shutdown: &mut Option<Arc<AtomicBool>>,
    server_running: &Arc<AtomicBool>,
    session_tx: &mpsc::Sender<AirPlaySession>,
) -> Vec<u8> {
    let path = req.target.split('?').next().unwrap_or(&req.target);

    match (req.method.as_str(), path) {
        ("OPTIONS", _) => format_response(
            req,
            200,
            "OK",
            None,
            &[(
                "Public",
                "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, TEARDOWN, OPTIONS, GET_PARAMETER, SET_PARAMETER, POST, GET",
            )],
            &[],
        ),
        ("GET", "/info") => match build_info_plist(identity) {
            Ok(body) => format_response(
                req,
                200,
                "OK",
                Some("application/x-apple-binary-plist"),
                &[],
                &body,
            ),
            Err(_) => format_response(req, 500, "Internal Server Error", None, &[], &[]),
        },
        ("POST", "/pair-setup") => format_response(
            req,
            200,
            "OK",
            Some("application/octet-stream"),
            &[],
            &identity.verifying_key_bytes,
        ),
        ("POST", "/pair-verify") => {
            if req.body.first().copied() == Some(1) {
                match handle_pair_verify_stage1(&identity.signing_key, &req.body) {
                    Ok((resp, state)) => {
                        *pair_verify_state = Some(state);
                        format_response(
                            req,
                            200,
                            "OK",
                            Some("application/octet-stream"),
                            &[],
                            &resp,
                        )
                    }
                    Err(_) => format_response(req, 400, "Bad Request", None, &[], &[]),
                }
            } else if let Some(state) = pair_verify_state.as_mut() {
                match handle_pair_verify_stage2(state, &req.body) {
                    Ok(()) => format_response(req, 200, "OK", None, &[], &[]),
                    Err(_) => format_response(req, 403, "Forbidden", None, &[], &[]),
                }
            } else {
                format_response(req, 400, "Bad Request", None, &[], &[])
            }
        }
        ("POST", "/fp-setup") => match handle_fp_setup(&req.body) {
            Some(resp) => format_response(
                req,
                200,
                "OK",
                Some("application/octet-stream"),
                &[],
                &resp,
            ),
            None => format_response(req, 400, "Bad Request", None, &[], &[]),
        },
        ("SETUP", _) | ("POST", "/stream") => {
            // Parse binary plist body if present (best-effort)
            if !req.body.is_empty() {
                let _ = plist::from_bytes::<Value>(&req.body);
            }

            if let Ok((video_port, shutdown_flag)) = spawn_video_listener(
                identity.device_name.clone(),
                Arc::clone(server_running),
                session_tx.clone(),
            ) {
                *active_video_shutdown = Some(shutdown_flag);
                match build_setup_response_plist(video_port) {
                    Ok(body) => format_response(
                        req,
                        200,
                        "OK",
                        Some("application/x-apple-binary-plist"),
                        &[],
                        &body,
                    ),
                    Err(_) => format_response(req, 500, "Internal Server Error", None, &[], &[]),
                }
            } else {
                format_response(req, 500, "Internal Server Error", None, &[], &[])
            }
        }
        ("RECORD", _) | ("SET_PARAMETER", _) | ("GET_PARAMETER", _) | ("FLUSH", _) => {
            format_response(req, 200, "OK", None, &[], &[])
        }
        ("TEARDOWN", _) => {
            if let Some(flag) = active_video_shutdown.take() {
                flag.store(true, Ordering::SeqCst);
            }
            format_response(req, 200, "OK", None, &[], &[])
        }
        _ => format_response(req, 200, "OK", None, &[], &[]),
    }
}

fn spawn_video_listener(
    device_name: String,
    server_running: Arc<AtomicBool>,
    session_tx: mpsc::Sender<AirPlaySession>,
) -> io::Result<(u16, Arc<AtomicBool>)> {
    let listener = match TcpListener::bind(("0.0.0.0", DEFAULT_VIDEO_PORT)) {
        Ok(l) => l,
        Err(_) => TcpListener::bind(("0.0.0.0", 0))?,
    };
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let session_shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&session_shutdown);

    thread::Builder::new()
        .name("airplay-video-accept".to_string())
        .spawn(move || {
            while server_running.load(Ordering::Relaxed) && !shutdown_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let session = AirPlaySession::from_tcp_stream(
                            stream,
                            device_name,
                            None,
                            Arc::clone(&shutdown_clone),
                        );
                        let _ = session_tx.send(session);
                        break;
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        })?;

    Ok((port, session_shutdown))
}

// ---------------------------------------------------------------------------
// AirPlay Video Stream Parser & PacketSource Implementation
// ---------------------------------------------------------------------------

/// Parsed 128-byte AirPlay mirroring video packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AirPlayVideoHeader {
    pub payload_size: u32,
    pub payload_type: u16,
    pub pts_us: u64,
}

fn ntp_to_us(ntp: u64) -> u64 {
    let secs = ntp >> 32;
    let frac = ntp & 0xFFFF_FFFF;
    secs.saturating_mul(1_000_000) + ((frac.saturating_mul(1_000_000)) >> 32)
}

fn parse_airplay_video_header(hdr: &[u8; 128]) -> AirPlayVideoHeader {
    let payload_size = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let payload_type = u16::from_le_bytes([hdr[4], hdr[5]]);
    let ntp = u64::from_le_bytes([
        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
    ]);
    AirPlayVideoHeader {
        payload_size,
        payload_type,
        pts_us: ntp_to_us(ntp),
    }
}

/// Converts an ISO/IEC 14496-15 `avcC` decoder configuration record into Annex-B SPS/PPS NALs
/// and returns `(annex_b_bytes, nal_length_size)`.
fn avcc_config_to_annex_b(avcc: &[u8]) -> io::Result<(Vec<u8>, usize)> {
    if avcc.len() < 7 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "avcC record too short",
        ));
    }
    let nal_len_size = ((avcc[4] & 0x03) + 1) as usize;
    let num_sps = (avcc[5] & 0x1F) as usize;
    let mut offset = 6;
    let mut out = Vec::with_capacity(avcc.len() + 16);

    for _ in 0..num_sps {
        if offset + 2 > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated SPS length in avcC",
            ));
        }
        let sps_len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
        offset += 2;
        if offset + sps_len > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated SPS payload in avcC",
            ));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[offset..offset + sps_len]);
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
        if offset + pps_len > avcc.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated PPS payload in avcC",
            ));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&avcc[offset..offset + pps_len]);
        offset += pps_len;
    }

    Ok((out, nal_len_size))
}

/// Converts length-prefixed AVCC H.264 NAL units into Annex-B (`00 00 00 01`) format.
/// Returns `(annex_b_bytes, is_keyframe)`.
fn avcc_nals_to_annex_b(data: &[u8], nal_len_size: usize) -> io::Result<(Vec<u8>, bool)> {
    if !(1..=4).contains(&nal_len_size) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported AVCC NAL length size",
        ));
    }
    let mut offset = 0;
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut is_keyframe = false;

    while offset + nal_len_size <= data.len() {
        let mut nal_len: usize = 0;
        for i in 0..nal_len_size {
            nal_len = (nal_len << 8) | (data[offset + i] as usize);
        }
        offset += nal_len_size;

        if nal_len == 0 {
            continue;
        }
        if offset + nal_len > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated AVCC NAL unit",
            ));
        }

        let nal = &data[offset..offset + nal_len];
        if (nal[0] & 0x1F) == 5 {
            is_keyframe = true;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        offset += nal_len;
    }

    Ok((out, is_keyframe))
}

/// Active AirPlay Screen Mirroring video session implementing `PacketSource`.
pub struct AirPlaySession {
    rx: mpsc::Receiver<NalPacket>,
    header: StreamHeader,
    device_name: String,
    control: ControlWriter,
    shutdown_flag: Arc<AtomicBool>,
    stream_handle: Option<TcpStream>,
    reader_thread: Option<JoinHandle<()>>,
}

impl AirPlaySession {
    fn from_tcp_stream(
        stream: TcpStream,
        device_name: String,
        stream_cipher: Option<Ctr128BE<Aes128>>,
        shutdown_flag: Arc<AtomicBool>,
    ) -> Self {
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let stream_handle = stream.try_clone().ok();
        let (tx, rx) = mpsc::channel();

        let thread_shutdown = Arc::clone(&shutdown_flag);
        let reader_thread = thread::Builder::new()
            .name("airplay-video-rx".to_string())
            .spawn(move || {
                run_video_stream_loop(stream, stream_cipher, thread_shutdown, tx);
            })
            .ok();

        Self {
            rx,
            header: StreamHeader {
                codec_id: CODEC_ID_H264,
                width: 1920,
                height: 1080,
                framed: true,
            },
            device_name,
            control: ControlWriter::new(None),
            shutdown_flag,
            stream_handle,
            reader_thread,
        }
    }
}

fn read_exact_or_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    shutdown_flag: &AtomicBool,
) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        if shutdown_flag.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn run_video_stream_loop(
    mut stream: TcpStream,
    mut cipher: Option<Ctr128BE<Aes128>>,
    shutdown_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<NalPacket>,
) {
    let mut hdr_buf = [0u8; 128];
    let mut nal_len_size: usize = 4;

    while !shutdown_flag.load(Ordering::Relaxed) {
        match read_exact_or_timeout(&mut stream, &mut hdr_buf, &shutdown_flag) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }

        let hdr = parse_airplay_video_header(&hdr_buf);
        let size = hdr.payload_size as usize;
        if size > MAX_VIDEO_PAYLOAD_SIZE {
            break;
        }
        if size == 0 {
            continue;
        }

        let mut payload = vec![0u8; size];
        match read_exact_or_timeout(&mut stream, &mut payload, &shutdown_flag) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }

        match hdr.payload_type {
            0x01 => {
                // avcC SPS/PPS codec configuration
                if let Ok((annex_b, len_size)) = avcc_config_to_annex_b(&payload) {
                    nal_len_size = len_size;
                    let pkt = NalPacket {
                        pts_us: hdr.pts_us,
                        is_config: true,
                        is_keyframe: false,
                        is_audio: false,
                        payload: Arc::from(annex_b),
                        received_at: Instant::now(),
                    };
                    if tx.send(pkt).is_err() {
                        break;
                    }
                }
            }
            0x00 => {
                // H.264 video frame (decrypt if AES-128-CTR stream key is active)
                if let Some(ref mut c) = cipher {
                    c.apply_keystream(&mut payload);
                }
                if let Ok((annex_b, is_keyframe)) = avcc_nals_to_annex_b(&payload, nal_len_size) {
                    if !annex_b.is_empty() {
                        let pkt = NalPacket {
                            pts_us: hdr.pts_us,
                            is_config: false,
                            is_keyframe,
                            is_audio: false,
                            payload: Arc::from(annex_b),
                            received_at: Instant::now(),
                        };
                        if tx.send(pkt).is_err() {
                            break;
                        }
                    }
                }
            }
            0x02 | 0x05 | _ => {
                // Heartbeat / keepalive / non-video packet
            }
        }
    }
}

impl PacketSource for AirPlaySession {
    fn header(&self) -> StreamHeader {
        self.header
    }

    fn read_packet(&mut self) -> io::Result<Option<NalPacket>> {
        loop {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return Ok(None);
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(pkt) => return Ok(Some(pkt)),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }

    fn control_writer(&self) -> ControlWriter {
        self.control.clone()
    }

    fn platform(&self) -> DevicePlatform {
        DevicePlatform::IosAirPlay
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn shutdown(&mut self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(ref s) = self.stream_handle {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(h) = self.reader_thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AirPlaySession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdns_packet_encoding_and_query_response() {
        let mut id = DeviceIdentity::generate("Mobile Mirror", 7000);
        id.local_ip = Ipv4Addr::new(192, 168, 1, 42);

        let pkt = build_airplay_announcement_packet(&id);
        assert!(pkt.len() > 12);
        // Header: ID=0, Flags=0x8400, QD=0, AN=9
        assert_eq!(&pkt[0..4], &[0x00, 0x00, 0x84, 0x00]);
        assert_eq!(u16::from_be_bytes([pkt[4], pkt[5]]), 0);
        assert_eq!(u16::from_be_bytes([pkt[6], pkt[7]]), 9);

        // Build a synthetic mDNS PTR query for _airplay._tcp.local.
        let mut query = vec![
            0x00, 0x00, // ID
            0x00, 0x00, // Standard query
            0x00, 0x01, // QDCOUNT = 1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        encode_dns_name("_airplay._tcp.local.", &mut query);
        query.extend_from_slice(&DNS_TYPE_PTR.to_be_bytes());
        query.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());

        let parsed = parse_mdns_queries(&query).expect("valid mDNS query");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "_airplay._tcp.local.");
        assert_eq!(parsed[0].qtype, DNS_TYPE_PTR);

        let resp = build_mdns_query_response(&id, &parsed).expect("should respond to PTR query");
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1); // 1 Answer (PTR)
        assert_eq!(u16::from_be_bytes([resp[10], resp[11]]), 3); // 3 Additionals (SRV, TXT, A)

        // Verify A record rdata encoding
        assert_eq!(encode_a_rdata(Ipv4Addr::new(10, 0, 0, 5)), vec![10, 0, 0, 5]);

        // Verify SRV record encoding
        let srv = encode_srv_rdata(0, 0, 7000, "mirror.local.");
        assert_eq!(&srv[0..6], &[0, 0, 0, 0, 0x1B, 0x58]); // 7000 = 0x1B58

        // Verify TXT record encoding
        let txt = encode_txt_rdata(&["vv=2".to_string(), "flags=0x4".to_string()]);
        assert_eq!(txt[0], 4);
        assert_eq!(&txt[1..5], b"vv=2");
        assert_eq!(txt[5], 9);
        assert_eq!(&txt[6..15], b"flags=0x4");
    }

    #[test]
    fn test_http_and_rtsp_request_parsing() {
        let http_raw = b"POST /pair-verify HTTP/1.1\r\nHost: 192.168.1.10:7000\r\nContent-Length: 4\r\n\r\n\x01\x00\x00\x00";
        let (req, consumed) = parse_http_rtsp_request(http_raw)
            .expect("no parse error")
            .expect("complete request");
        assert_eq!(consumed, http_raw.len());
        assert_eq!(req.method, "POST");
        assert_eq!(req.target, "/pair-verify");
        assert_eq!(req.version, "HTTP/1.1");
        assert_eq!(req.body, vec![1, 0, 0, 0]);

        // Incomplete body should return Ok(None)
        let partial = b"POST /fp-setup HTTP/1.1\r\nContent-Length: 16\r\n\r\nFPLY";
        assert!(parse_http_rtsp_request(partial).unwrap().is_none());

        // RTSP request with CSeq
        let rtsp_raw = b"SETUP rtsp://192.168.1.10/123 RTSP/1.0\r\nCSeq: 3\r\nContent-Length: 2\r\n\r\nok";
        let (rtsp_req, _) = parse_http_rtsp_request(rtsp_raw)
            .unwrap()
            .expect("complete RTSP request");
        assert_eq!(rtsp_req.method, "SETUP");
        assert_eq!(rtsp_req.headers.get("cseq").map(String::as_str), Some("3"));

        let resp = format_response(&rtsp_req, 200, "OK", None, &[], b"");
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.starts_with("RTSP/1.0 200 OK\r\n"));
        assert!(resp_str.contains("CSeq: 3\r\n"));
    }

    #[test]
    fn test_pair_verify_key_derivation_and_roundtrip() {
        let server_ed = SigningKey::generate(&mut OsRng);
        let client_ed = SigningKey::generate(&mut OsRng);
        let client_curve_secret = StaticSecret::random_from_rng(OsRng);
        let client_curve_pk = X25519PublicKey::from(&client_curve_secret).to_bytes();

        // Stage 1 request: 4-byte header + client Curve25519 PK + client Ed25519 PK
        let mut stage1_req = Vec::with_capacity(68);
        stage1_req.extend_from_slice(&[1, 0, 0, 0]);
        stage1_req.extend_from_slice(&client_curve_pk);
        stage1_req.extend_from_slice(&client_ed.verifying_key().to_bytes());

        let (stage1_resp, mut pv_state) =
            handle_pair_verify_stage1(&server_ed, &stage1_req).expect("stage 1 succeeds");
        assert_eq!(stage1_resp.len(), 96);

        let mut server_curve_pk = [0u8; 32];
        server_curve_pk.copy_from_slice(&stage1_resp[..32]);
        let mut enc_server_sig = [0u8; 64];
        enc_server_sig.copy_from_slice(&stage1_resp[32..96]);

        // Client derives shared secret and AES-128-CTR key/IV independently
        let shared = client_curve_secret.diffie_hellman(&X25519PublicKey::from(server_curve_pk));
        let (aes_key, aes_iv) = derive_pair_verify_aes_key_iv(shared.as_bytes());
        let mut client_cipher = Ctr128BE::<Aes128>::new(&aes_key.into(), &aes_iv.into());

        // Client decrypts server signature and verifies it
        client_cipher.apply_keystream(&mut enc_server_sig);
        let mut server_signed_msg = [0u8; 64];
        server_signed_msg[..32].copy_from_slice(&server_curve_pk);
        server_signed_msg[32..].copy_from_slice(&client_curve_pk);
        server_ed
            .verifying_key()
            .verify(&server_signed_msg, &Signature::from_bytes(&enc_server_sig))
            .expect("server Ed25519 signature must verify");

        // Stage 2: Client signs (client_curve_pk || server_curve_pk) and encrypts with continued CTR stream
        let mut client_msg = [0u8; 64];
        client_msg[..32].copy_from_slice(&client_curve_pk);
        client_msg[32..].copy_from_slice(&server_curve_pk);
        let mut client_sig = client_ed.sign(&client_msg).to_bytes();
        client_cipher.apply_keystream(&mut client_sig);

        let mut stage2_req = Vec::with_capacity(68);
        stage2_req.extend_from_slice(&[0, 0, 0, 0]);
        stage2_req.extend_from_slice(&client_sig);

        handle_pair_verify_stage2(&mut pv_state, &stage2_req)
            .expect("stage 2 client signature verification must succeed");
    }

    #[test]
    fn test_airplay_video_header_and_avcc_to_annex_b() {
        // 128-byte AirPlay header: payload_size=512, type=0x00, ntp = 2 seconds + 0.5 seconds
        let mut hdr = [0u8; 128];
        hdr[0..4].copy_from_slice(&512u32.to_le_bytes());
        hdr[4..6].copy_from_slice(&0x00u16.to_le_bytes());
        let ntp: u64 = (2u64 << 32) | 0x8000_0000u64;
        hdr[8..16].copy_from_slice(&ntp.to_le_bytes());

        let parsed = parse_airplay_video_header(&hdr);
        assert_eq!(parsed.payload_size, 512);
        assert_eq!(parsed.payload_type, 0x00);
        assert_eq!(parsed.pts_us, 2_500_000);

        // avcC config with 1 SPS (4 bytes) and 1 PPS (3 bytes), nal_len_size=4
        let avcc = vec![
            0x01, 0x64, 0x00, 0x28, 0xFF, // version, profile, compat, level, lengthSizeMinusOne=3
            0xE1, // 1 SPS
            0x00, 0x04, 0x67, 0x64, 0x00, 0x28, // SPS
            0x01, // 1 PPS
            0x00, 0x03, 0x68, 0xEE, 0x3C, // PPS
        ];
        let (config_annex_b, nal_len_size) = avcc_config_to_annex_b(&avcc).expect("valid avcC");
        assert_eq!(nal_len_size, 4);
        assert_eq!(
            config_annex_b,
            vec![
                0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, // SPS
                0, 0, 0, 1, 0x68, 0xEE, 0x3C, // PPS
            ]
        );

        // AVCC video frame with 1 SEI (2 bytes) + 1 IDR slice (3 bytes)
        let avcc_frame = vec![
            0, 0, 0, 2, 0x06, 0x01, // SEI (non-IDR)
            0, 0, 0, 3, 0x65, 0x88, 0x84, // IDR slice (nal_type = 5)
        ];
        let (frame_annex_b, is_keyframe) =
            avcc_nals_to_annex_b(&avcc_frame, nal_len_size).expect("valid AVCC frame");
        assert!(is_keyframe);
        assert_eq!(
            frame_annex_b,
            vec![
                0, 0, 0, 1, 0x06, 0x01, // SEI
                0, 0, 0, 1, 0x65, 0x88, 0x84, // IDR
            ]
        );
    }
}
