// [Mobile Mirror] Progressive implementation phase 3/8
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
    pub fn new(preferred_hw: Option<&str>) -> Result<Self, String> {
        let api = FfmpegApi::load()?;

        let pref = preferred_hw
            .unwrap_or("auto")
            .trim()
            .to_ascii_lowercase();
        let force_software = matches!(pref.as_str(), "cpu" | "sw" | "software");

        unsafe {
            let mut codec_ctx: *mut c_void = ptr::null_mut();
            let mut hw_device_ctx: *mut AVBufferRef = ptr::null_mut();
            let mut backend_name = String::new();

            // 1. Try dedicated NVIDIA NVDEC (`h264_cuvid`) when requested or in `auto` mode.
            if !force_software && matches!(pref.as_str(), "" | "auto" | "nvdec" | "cuda") {
                let cuvid_name = CString::new("h264_cuvid").unwrap();
                let cuvid_codec = (api.avcodec_find_decoder_by_name)(cuvid_name.as_ptr());
                if !cuvid_codec.is_null() {
                    if let Some(ctx) = Self::try_open_codec(&api, cuvid_codec, 1, false) {
                        codec_ctx = ctx;
                        backend_name = "NVDEC (h264_cuvid)".to_string();
                    }
                }
            }

            // 2. Standard `AV_CODEC_ID_H264` with `D3D11VA` / `DXVA2` / `CUDA` hardware context
            //    and automatic fallback to multi-threaded low-delay software H.264 decoding.
            if codec_ctx.is_null() {
                let sw_codec = (api.avcodec_find_decoder)(AV_CODEC_ID_H264);
                if sw_codec.is_null() {
                    return Err("Could not find H.264 decoder in libavcodec".to_string());
                }

                if !force_software {
                    let candidates: &[(i32, &str)] = match pref.as_str() {
                        "d3d11va" | "d3d11" => &[
                            (AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA"),
                            (AV_HWDEVICE_TYPE_DXVA2, "DXVA2"),
                        ],
                        "dxva2" => &[
                            (AV_HWDEVICE_TYPE_DXVA2, "DXVA2"),
                            (AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA"),
                        ],
                        "nvdec" | "cuda" => &[
                            (AV_HWDEVICE_TYPE_CUDA, "NVDEC (CUDA)"),
                            (AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA"),
                            (AV_HWDEVICE_TYPE_DXVA2, "DXVA2"),
                        ],
                        _ => &[
                            (AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA"),
                            (AV_HWDEVICE_TYPE_DXVA2, "DXVA2"),
                            (AV_HWDEVICE_TYPE_CUDA, "NVDEC (CUDA)"),
                        ],
                    };

                    for &(hw_type, label) in candidates {
                        let ret = (api.av_hwdevice_ctx_create)(
                            &mut hw_device_ctx,
                            hw_type,
                            ptr::null(),
                            ptr::null_mut(),
                            0,
                        );
                        if ret == 0 && !hw_device_ctx.is_null() {
                            backend_name = label.to_string();
                            break;
                        }
                    }
                }

                let slice_threads = std::thread::available_parallelism()
                    .map(|n| n.get().clamp(2, 4) as i64)
                    .unwrap_or(2);

                match Self::try_open_codec(&api, sw_codec, slice_threads, true) {
                    Some(ctx) => {
                        codec_ctx = ctx;
                        if backend_name.is_empty() {
                            backend_name = "FFmpeg H.264 Low-Delay".to_string();
                        }
                    }
                    None => {
                        if !hw_device_ctx.is_null() {
                            (api.av_buffer_unref)(&mut hw_device_ctx);
                        }
                        return Err(
                            "Failed to open FFmpeg H.264 Low-Delay decoder (avcodec_open2 failed)"
                                .to_string(),
                        );
                    }
                }
            }

            let pkt = (api.av_packet_alloc)();
            let recv_frame = (api.av_frame_alloc)();
            let raw_frame = (api.av_frame_alloc)();
            let sw_frame = (api.av_frame_alloc)();
