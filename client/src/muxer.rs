// [Mobile Mirror] Progressive implementation phase 1/8
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
