// [Mobile Mirror] Progressive implementation phase 1/8
use crate::ffi::ffmpeg::{
    FfmpegApi, AVBufferRef, AVCodec, AVFrame, AVPacket, AVERROR_EAGAIN, AVERROR_EOF,
    AV_CODEC_FLAG2_FAST, AV_CODEC_FLAG_LOW_DELAY, AV_CODEC_ID_H264, AV_HWDEVICE_TYPE_CUDA,
    AV_HWDEVICE_TYPE_D3D11VA, AV_HWDEVICE_TYPE_DXVA2, AV_PIX_FMT_CUDA, AV_PIX_FMT_D3D11,
    AV_PIX_FMT_DXVA2_VLD, AV_PIX_FMT_NV12, AV_PIX_FMT_YUV420P,
};
use crate::protocol::NalPacket;
use std::ffi::{c_void, CString};
use std::ptr;

const FF_THREAD_SLICE: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    Nv12,
    Yuv420p,
}

/// Borrowed zero-copy view of a decoded video frame ready for SDL3 GPU upload.
pub struct DecodedFrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub y_plane: &'a [u8],
    pub y_pitch: i32,
    pub u_or_uv_plane: &'a [u8],
    pub u_or_uv_pitch: i32,
    pub v_plane: Option<&'a [u8]>,
    pub v_pitch: i32,
}

/// Hardware-accelerated low-latency H.264 decoder (`NVDEC` / `D3D11VA` / `DXVA2` / `CUDA`)
/// backed by FFmpeg `libavcodec`, with automatic fallback to slice-threaded low-delay H.264 decoding.
pub struct HwDecoder {
    api: FfmpegApi,
    codec_ctx: *mut c_void,
    hw_device_ctx: *mut AVBufferRef,
    pkt: *mut AVPacket,
    recv_frame: *mut AVFrame,
    raw_frame: *mut AVFrame,
    sw_frame: *mut AVFrame,
    has_frame: bool,
    sw_frame_valid: bool,
    backend_name: String,
}

unsafe impl Send for HwDecoder {}

impl HwDecoder {
