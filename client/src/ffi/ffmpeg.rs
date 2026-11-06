#![allow(non_camel_case_types, dead_code)]

use crate::ffi::DynLib;
use std::ffi::{c_char, c_int, c_void};

pub const AV_CODEC_ID_H264: c_int = 27;
pub const AVMEDIA_TYPE_VIDEO: c_int = 0;

pub const AV_PKT_FLAG_KEY: c_int = 0x0001;
pub const AVIO_FLAG_WRITE: c_int = 2;
pub const AVFMT_NOFILE: c_int = 0x0001;

pub const AV_CODEC_FLAG_LOW_DELAY: c_int = 1 << 19;
pub const AV_CODEC_FLAG_GLOBAL_HEADER: c_int = 1 << 22;
pub const AV_CODEC_FLAG2_FAST: c_int = 1 << 0;

// Hardware Acceleration Device Types (AVHWDeviceType)
pub const AV_HWDEVICE_TYPE_NONE: c_int = 0;
pub const AV_HWDEVICE_TYPE_VDPAU: c_int = 1;
pub const AV_HWDEVICE_TYPE_CUDA: c_int = 2;       // NVIDIA NVDEC
pub const AV_HWDEVICE_TYPE_VAAPI: c_int = 3;
pub const AV_HWDEVICE_TYPE_DXVA2: c_int = 4;      // DirectX Video Acceleration 2
pub const AV_HWDEVICE_TYPE_QSV: c_int = 5;
pub const AV_HWDEVICE_TYPE_VIDEOTOOLBOX: c_int = 6;
pub const AV_HWDEVICE_TYPE_D3D11VA: c_int = 7;    // Direct3D 11 Video Acceleration

// Common Pixel Formats (AVPixelFormat)
pub const AV_PIX_FMT_YUV420P: c_int = 0;
pub const AV_PIX_FMT_NV12: c_int = 23;
pub const AV_PIX_FMT_DXVA2_VLD: c_int = 53;
pub const AV_PIX_FMT_CUDA: c_int = 119;
pub const AV_PIX_FMT_D3D11: c_int = 174;

pub const AVERROR_EAGAIN: c_int = -11;
pub const AVERROR_EOF: c_int = -((b'E' as c_int)
    | ((b'O' as c_int) << 8)
    | ((b'F' as c_int) << 16)
    | ((b' ' as c_int) << 24));

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct AVRational {
    pub num: c_int,
    pub den: c_int,
}

#[repr(C)]
pub struct AVCodec {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct AVBufferRef {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct AVIOContext {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct AVOutputFormat {
    pub name: *const c_char,
    pub long_name: *const c_char,
    pub mime_type: *const c_char,
    pub extensions: *const c_char,
    pub audio_codec: c_int,
    pub video_codec: c_int,
    pub subtitle_codec: c_int,
    pub flags: c_int,
}

#[repr(C)]
pub struct AVCodecParameters {
    pub codec_type: c_int,
    pub codec_id: c_int,
    pub codec_tag: u32,
    pub extradata: *mut u8,
    pub extradata_size: c_int,
    pub format: c_int,
    pub bit_rate: i64,
    pub bits_per_coded_sample: c_int,
    pub bits_per_raw_sample: c_int,
    pub profile: c_int,
    pub level: c_int,
    pub width: c_int,
    pub height: c_int,
}

#[repr(C)]
pub struct AVStream {
    pub index: c_int,
    pub id: c_int,
    pub codecpar: *mut AVCodecParameters,
    pub time_base: AVRational,
}

#[repr(C)]
pub struct AVFormatContext {
    pub av_class: *const c_void,
    pub oformat: *const AVOutputFormat,
    pub priv_data: *mut c_void,
    pub pb: *mut AVIOContext,
    pub ctx_flags: c_int,
    pub nb_streams: u32,
    pub streams: *mut *mut AVStream,
}

#[repr(C)]
pub struct AVPacket {
    pub buf: *mut AVBufferRef,
    pub pts: i64,
    pub dts: i64,
    pub data: *mut u8,
    pub size: c_int,
    pub stream_index: c_int,
    pub flags: c_int,
    pub side_data: *mut c_void,
    pub side_data_elems: c_int,
    pub duration: i64,
    pub pos: i64,
    pub opaque: *mut c_void,
    pub opaque_ref: *mut AVBufferRef,
    pub time_base: AVRational,
}

#[repr(C)]
pub struct AVFrame {
    pub data: [*mut u8; 8],
    pub linesize: [c_int; 8],
    pub extended_data: *mut *mut u8,
    pub width: c_int,
    pub height: c_int,
    pub nb_samples: c_int,
    pub format: c_int,
    pub key_frame: c_int,
    pub pict_type: c_int,
    pub sample_aspect_ratio: AVRational,
    pub pts: i64,
    pub pkt_dts: i64,
    pub time_base: AVRational,
}

/// Function pointer table for FFmpeg (`libavformat`, `libavcodec`, `libavutil`).
pub struct FfmpegApi {
    _avutil: DynLib,
    _avcodec: DynLib,
    _avformat: DynLib,

    // libavformat
    pub avformat_alloc_output_context2: unsafe extern "C" fn(
        *mut *mut AVFormatContext,
        *const AVOutputFormat,
        *const c_char,
        *const c_char,
    ) -> c_int,
    pub avformat_new_stream:
        unsafe extern "C" fn(*mut AVFormatContext, *const AVCodec) -> *mut AVStream,
    pub avio_open:
        unsafe extern "C" fn(*mut *mut AVIOContext, *const c_char, c_int) -> c_int,
    pub avio_closep: unsafe extern "C" fn(*mut *mut AVIOContext) -> c_int,
    pub avformat_write_header:
        unsafe extern "C" fn(*mut AVFormatContext, *mut *mut c_void) -> c_int,
    pub av_interleaved_write_frame:
        unsafe extern "C" fn(*mut AVFormatContext, *mut AVPacket) -> c_int,
    pub av_write_trailer: unsafe extern "C" fn(*mut AVFormatContext) -> c_int,
    pub avformat_free_context: unsafe extern "C" fn(*mut AVFormatContext),

    // libavcodec
    pub avcodec_find_decoder: unsafe extern "C" fn(c_int) -> *const AVCodec,
    pub avcodec_find_decoder_by_name: unsafe extern "C" fn(*const c_char) -> *const AVCodec,
    pub avcodec_alloc_context3: unsafe extern "C" fn(*const AVCodec) -> *mut c_void,
    pub avcodec_free_context: unsafe extern "C" fn(*mut *mut c_void),
    pub avcodec_open2:
        unsafe extern "C" fn(*mut c_void, *const AVCodec, *mut *mut c_void) -> c_int,
    pub avcodec_send_packet: unsafe extern "C" fn(*mut c_void, *const AVPacket) -> c_int,
    pub avcodec_receive_frame: unsafe extern "C" fn(*mut c_void, *mut AVFrame) -> c_int,
    pub av_packet_alloc: unsafe extern "C" fn() -> *mut AVPacket,
    pub av_packet_free: unsafe extern "C" fn(*mut *mut AVPacket),
    pub av_packet_unref: unsafe extern "C" fn(*mut AVPacket),

    // libavutil
    pub av_frame_alloc: unsafe extern "C" fn() -> *mut AVFrame,
    pub av_frame_free: unsafe extern "C" fn(*mut *mut AVFrame),
    pub av_frame_unref: unsafe extern "C" fn(*mut AVFrame),
    pub av_hwdevice_ctx_create: unsafe extern "C" fn(
        *mut *mut AVBufferRef,
        c_int,
        *const c_char,
        *mut c_void,
        c_int,
    ) -> c_int,
    pub av_hwframe_transfer_data:
        unsafe extern "C" fn(*mut AVFrame, *const AVFrame, c_int) -> c_int,
    pub av_buffer_ref: unsafe extern "C" fn(*const AVBufferRef) -> *mut AVBufferRef,
    pub av_buffer_unref: unsafe extern "C" fn(*mut *mut AVBufferRef),
    pub av_mallocz: unsafe extern "C" fn(usize) -> *mut c_void,
    pub av_opt_set_int:
        unsafe extern "C" fn(*mut c_void, *const c_char, i64, c_int) -> c_int,
}

impl FfmpegApi {
    pub fn load() -> Result<Self, String> {
        let avutil = DynLib::open_any(&[
            "avutil-61.dll",
            "avutil-60.dll",
            "avutil-59.dll",
            "avutil-58.dll",
            "avutil.dll",
            "libavutil.so.61",
            "libavutil.so.60",
            "libavutil.so.59",
            "libavutil.so.58",
            "libavutil.so",
            "libavutil.dylib",
        ])?;
        let avcodec = DynLib::open_any(&[
            "avcodec-63.dll",
            "avcodec-62.dll",
            "avcodec-61.dll",
            "avcodec-60.dll",
            "avcodec.dll",
            "libavcodec.so.63",
            "libavcodec.so.62",
            "libavcodec.so.61",
            "libavcodec.so.60",
            "libavcodec.so",
            "libavcodec.dylib",
        ])?;
        let avformat = DynLib::open_any(&[
            "avformat-63.dll",
            "avformat-62.dll",
            "avformat-61.dll",
            "avformat-60.dll",
            "avformat.dll",
            "libavformat.so.63",
            "libavformat.so.62",
            "libavformat.so.61",
            "libavformat.so.60",
            "libavformat.so",
            "libavformat.dylib",
        ])?;

        unsafe {
            Ok(Self {
                avformat_alloc_output_context2: avformat
                    .get_sym("avformat_alloc_output_context2")?,
                avformat_new_stream: avformat.get_sym("avformat_new_stream")?,
                avio_open: avformat.get_sym("avio_open")?,
                avio_closep: avformat.get_sym("avio_closep")?,
                avformat_write_header: avformat.get_sym("avformat_write_header")?,
                av_interleaved_write_frame: avformat.get_sym("av_interleaved_write_frame")?,
                av_write_trailer: avformat.get_sym("av_write_trailer")?,
                avformat_free_context: avformat.get_sym("avformat_free_context")?,

                avcodec_find_decoder: avcodec.get_sym("avcodec_find_decoder")?,
                avcodec_find_decoder_by_name: avcodec.get_sym("avcodec_find_decoder_by_name")?,
                avcodec_alloc_context3: avcodec.get_sym("avcodec_alloc_context3")?,
                avcodec_free_context: avcodec.get_sym("avcodec_free_context")?,
                avcodec_open2: avcodec.get_sym("avcodec_open2")?,
                avcodec_send_packet: avcodec.get_sym("avcodec_send_packet")?,
                avcodec_receive_frame: avcodec.get_sym("avcodec_receive_frame")?,
                av_packet_alloc: avcodec.get_sym("av_packet_alloc")?,
                av_packet_free: avcodec.get_sym("av_packet_free")?,
                av_packet_unref: avcodec.get_sym("av_packet_unref")?,

                av_frame_alloc: avutil.get_sym("av_frame_alloc")?,
                av_frame_free: avutil.get_sym("av_frame_free")?,
                av_frame_unref: avutil.get_sym("av_frame_unref")?,
                av_hwdevice_ctx_create: avutil.get_sym("av_hwdevice_ctx_create")?,
                av_hwframe_transfer_data: avutil.get_sym("av_hwframe_transfer_data")?,
                av_buffer_ref: avutil.get_sym("av_buffer_ref")?,
                av_buffer_unref: avutil.get_sym("av_buffer_unref")?,
                av_mallocz: avutil.get_sym("av_mallocz")?,
                av_opt_set_int: avutil.get_sym("av_opt_set_int")?,

                _avutil: avutil,
                _avcodec: avcodec,
                _avformat: avformat,
            })
        }
    }
}
