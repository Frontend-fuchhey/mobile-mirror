// [Mobile Mirror] Progressive implementation phase 2/8
use crate::ffi::ffmpeg::{
    FfmpegApi, AVFormatContext, AVPacket, AVRational, AVFMT_NOFILE, AVIO_FLAG_WRITE,
    AVMEDIA_TYPE_VIDEO, AV_CODEC_ID_H264, AV_PKT_FLAG_KEY,
};
use crate::protocol::{annexb_to_avcc_sample, extract_sps_pps, NalPacket, StreamHeader};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::ptr;

/// Zero-re-encoding MP4 stream muxer.
/// Uses FFmpeg `libavformat` (`avformat_alloc_output_context2` + `av_interleaved_write_frame`)
/// when available at runtime, or falls back to a built-in zero-copy ISO-BMFF (`ftyp`/`mdat`/`moov`)
/// MP4 container writer.
pub enum Mp4Muxer {
    LibAvformat(LibAvformatMp4Muxer),
    NativeIsoBmff(NativeIsoMp4Muxer),
}

impl Mp4Muxer {
    pub fn create<P: AsRef<Path>>(path: P, header: StreamHeader) -> io::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let native = NativeIsoMp4Muxer::new(path.as_ref(), header.width, header.height)?;
        Ok(Self::NativeIsoBmff(native))
    }

    pub fn write_packet(&mut self, packet: &NalPacket) -> io::Result<()> {
        match self {
            Self::LibAvformat(m) => m.write_packet(packet),
            Self::NativeIsoBmff(m) => m.write_packet(packet),
        }
    }

    pub fn finalize(self) -> io::Result<()> {
        match self {
            Self::LibAvformat(m) => m.finalize(),
            Self::NativeIsoBmff(m) => m.finalize(),
        }
    }
}

/// FFmpeg `libavformat` direct bitstream muxer (zero re-encoding).
pub struct LibAvformatMp4Muxer {
    api: FfmpegApi,
    fmt_ctx: *mut AVFormatContext,
    pkt: *mut AVPacket,
    header_written: bool,
    base_pts_us: Option<u64>,
    last_dts_90k: i64,
    stream_time_base: AVRational,
}

unsafe impl Send for LibAvformatMp4Muxer {}

impl LibAvformatMp4Muxer {
    pub fn new(api: FfmpegApi, filename: &str, header: StreamHeader) -> Result<Self, String> {
        let c_filename = CString::new(filename).map_err(|e| e.to_string())?;
        let c_format = CString::new("mp4").unwrap();
        let mut fmt_ctx: *mut AVFormatContext = ptr::null_mut();

        unsafe {
            let ret = (api.avformat_alloc_output_context2)(
                &mut fmt_ctx,
                ptr::null(),
                c_format.as_ptr(),
                c_filename.as_ptr(),
            );
            if ret < 0 || fmt_ctx.is_null() {
                return Err(format!("avformat_alloc_output_context2 failed ({})", ret));
            }

            let stream = (api.avformat_new_stream)(fmt_ctx, ptr::null());
            if stream.is_null() {
                (api.avformat_free_context)(fmt_ctx);
                return Err("avformat_new_stream returned null".to_string());
            }

            (*stream).id = 0;
            (*stream).time_base = AVRational { num: 1, den: 90_000 };
            let codecpar = (*stream).codecpar;
            (*codecpar).codec_type = AVMEDIA_TYPE_VIDEO;
            (*codecpar).codec_id = AV_CODEC_ID_H264;
            (*codecpar).width = header.width as i32;
            (*codecpar).height = header.height as i32;

            let oformat = (*fmt_ctx).oformat;
            if !oformat.is_null() && ((*oformat).flags & AVFMT_NOFILE) == 0 {
                let rio = (api.avio_open)(
                    &mut (*fmt_ctx).pb,
                    c_filename.as_ptr(),
                    AVIO_FLAG_WRITE,
                );
                if rio < 0 {
                    (api.avformat_free_context)(fmt_ctx);
                    return Err(format!("avio_open failed ({})", rio));
                }
            }

            let pkt = (api.av_packet_alloc)();
            if pkt.is_null() {
                (api.avformat_free_context)(fmt_ctx);
                return Err("av_packet_alloc failed".to_string());
            }

            Ok(Self {
                api,
                fmt_ctx,
                pkt,
                header_written: false,
                base_pts_us: None,
                last_dts_90k: -1,
                stream_time_base: AVRational { num: 1, den: 90_000 },
            })
        }
    }

    pub fn write_packet(&mut self, packet: &NalPacket) -> io::Result<()> {
        unsafe {
            let stream = *(*self.fmt_ctx).streams;
            if packet.is_config {
                if !self.header_written {
                    let codecpar = (*stream).codecpar;
                    let size = packet.payload.len();
                    let extradata = (self.api.av_mallocz)(size + 64) as *mut u8;
                    if !extradata.is_null() {
                        ptr::copy_nonoverlapping(packet.payload.as_ptr(), extradata, size);
                        (*codecpar).extradata = extradata;
                        (*codecpar).extradata_size = size as i32;
                    }
                    let ret = (self.api.avformat_write_header)(self.fmt_ctx, ptr::null_mut());
                    if ret < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("avformat_write_header failed: {}", ret),
                        ));
                    }
                    self.stream_time_base = (*stream).time_base;
                    self.header_written = true;
                }
                return Ok(());
            }

            if !self.header_written {
                let ret = (self.api.avformat_write_header)(self.fmt_ctx, ptr::null_mut());
                if ret < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("avformat_write_header failed: {}", ret),
                    ));
                }
                self.stream_time_base = (*stream).time_base;
                self.header_written = true;
            }

            let base = *self.base_pts_us.get_or_insert(packet.pts_us);
            let rel_us = packet.pts_us.saturating_sub(base);
            let tb_den = self.stream_time_base.den.max(1) as u128;
            let tb_num = self.stream_time_base.num.max(1) as u128;
            let mut pts = ((rel_us as u128 * tb_den) / (1_000_000u128 * tb_num)) as i64;
            if pts <= self.last_dts_90k {
                pts = self.last_dts_90k + 1;
            }
            self.last_dts_90k = pts;

            (self.api.av_packet_unref)(self.pkt);
            (*self.pkt).data = packet.payload.as_ptr() as *mut u8;
            (*self.pkt).size = packet.payload.len() as i32;
            (*self.pkt).stream_index = 0;
            (*self.pkt).pts = pts;
            (*self.pkt).dts = pts;
            (*self.pkt).flags = if packet.is_keyframe { AV_PKT_FLAG_KEY } else { 0 };

            let ret = (self.api.av_interleaved_write_frame)(self.fmt_ctx, self.pkt);
            if ret < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("av_interleaved_write_frame failed: {}", ret),
                ));
            }
            Ok(())
        }
    }

    pub fn finalize(mut self) -> io::Result<()> {
        unsafe {
            if self.header_written {
                (self.api.av_write_trailer)(self.fmt_ctx);
                self.header_written = false;
            }
        }
        Ok(())
    }
}

impl Drop for LibAvformatMp4Muxer {
    fn drop(&mut self) {
        unsafe {
            if self.header_written && !self.fmt_ctx.is_null() {
                (self.api.av_write_trailer)(self.fmt_ctx);
            }
            if !self.pkt.is_null() {
                (self.api.av_packet_free)(&mut self.pkt);
            }
            if !self.fmt_ctx.is_null() {
                let oformat = (*self.fmt_ctx).oformat;
                if !oformat.is_null()
                    && ((*oformat).flags & AVFMT_NOFILE) == 0
                    && !(*self.fmt_ctx).pb.is_null()
                {
                    (self.api.avio_closep)(&mut (*self.fmt_ctx).pb);
                }
                (self.api.avformat_free_context)(self.fmt_ctx);
                self.fmt_ctx = ptr::null_mut();
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SampleMeta {
    offset: u64,
    size: u32,
    pts_90k: u64,
    is_sync: bool,
}

#[derive(Clone, Copy)]
struct AudioSampleMeta {
    offset: u64,
    size: u32,
    num_samples: u32,
}

/// Built-in zero-re-encoding ISO-BMFF MP4 muxer (`ftyp` + `mdat` + `moov`).
/// Converts Annex-B start codes to 4-byte length-prefixed AVCC samples on the fly
/// and writes the complete `moov` index table upon finalization.
pub struct NativeIsoMp4Muxer {
    writer: BufWriter<File>,
    width: u32,
    height: u32,
    sps: Vec<u8>,
    pps: Vec<u8>,
    latest_sps: Vec<u8>,
    latest_pps: Vec<u8>,
    aac_config: Vec<u8>,
    audio_is_aac: bool,
    mdat_header_offset: u64,
    current_offset: u64,
    first_raw_pts_us: Option<u64>,
    last_raw_pts_us: Option<u64>,
    accumulated_us: u64,
    accumulated_pts_90k: u64,
    leading_audio_dropped: u32,
    samples: Vec<SampleMeta>,
    audio_samples: Vec<AudioSampleMeta>,
}

impl NativeIsoMp4Muxer {
    pub fn new(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::with_capacity(2 * 1024 * 1024, file);

        // Write `ftyp` box (28 bytes)
        let ftyp: [u8; 28] = [
            0, 0, 0, 28, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 2, 0, b'i', b's',
            b'o', b'm', b'i', b's', b'o', b'2', b'a', b'v', b'c', b'1',
        ];
        writer.write_all(&ftyp)?;

        // Write 64-bit `mdat` header placeholder (16 bytes: size=1 -> 64-bit largesize)
        let mdat_header_offset = 28u64;
        writer.write_all(&1u32.to_be_bytes())?;
        writer.write_all(b"mdat")?;
        writer.write_all(&0u64.to_be_bytes())?;
        writer.flush()?;
