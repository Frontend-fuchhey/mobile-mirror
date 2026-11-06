use crate::ffi::ffmpeg::{
    FfmpegApi, AVBufferRef, AVFrame, AVPacket, AVERROR_EAGAIN, AVERROR_EOF, AV_CODEC_ID_H264,
    AV_HWDEVICE_TYPE_CUDA, AV_HWDEVICE_TYPE_D3D11VA, AV_HWDEVICE_TYPE_DXVA2,
    AV_PIX_FMT_CUDA, AV_PIX_FMT_D3D11, AV_PIX_FMT_DXVA2_VLD, AV_PIX_FMT_NV12,
};
use crate::protocol::NalPacket;
use std::ffi::{c_void, CString};
use std::ptr;

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

/// Hardware-accelerated H.264 decoder (NVDEC / DXVA2 / D3D11VA) backed by FFmpeg `libavcodec`.
pub struct HwDecoder {
    api: FfmpegApi,
    codec_ctx: *mut c_void,
    hw_device_ctx: *mut AVBufferRef,
    pkt: *mut AVPacket,
    raw_frame: *mut AVFrame,
    sw_frame: *mut AVFrame,
    backend_name: String,
}

unsafe impl Send for HwDecoder {}

impl HwDecoder {
    pub fn new(preferred_hw: Option<&str>) -> Result<Self, String> {
        let api = FfmpegApi::load()?;

        unsafe {
            // 1. Try dedicated NVIDIA NVDEC (`h264_cuvid`) decoder first if requested or available
            let cuvid_name = CString::new("h264_cuvid").unwrap();
            let mut codec = if preferred_hw.map(|s| s.eq_ignore_ascii_case("nvdec") || s.eq_ignore_ascii_case("cuda")).unwrap_or(true) {
                (api.avcodec_find_decoder_by_name)(cuvid_name.as_ptr())
            } else {
                ptr::null()
            };

            let mut used_cuvid = !codec.is_null();
            if codec.is_null() {
                codec = (api.avcodec_find_decoder)(AV_CODEC_ID_H264);
                used_cuvid = false;
            }
            if codec.is_null() {
                return Err("Could not find H.264 decoder in libavcodec".to_string());
            }

            let codec_ctx = (api.avcodec_alloc_context3)(codec);
            if codec_ctx.is_null() {
                return Err("avcodec_alloc_context3 failed".to_string());
            }

            // Configure low-latency decoding options (zero frame reordering delay)
            let opt_threads = CString::new("threads").unwrap();
            (api.av_opt_set_int)(codec_ctx, opt_threads.as_ptr(), 1, 0);

            let mut hw_device_ctx: *mut AVBufferRef = ptr::null_mut();
            let mut backend_name = if used_cuvid {
                "NVDEC (h264_cuvid)".to_string()
            } else {
                "CPU Low-Delay H.264".to_string()
            };

            if !used_cuvid {
                let candidates: &[(i32, &str)] = match preferred_hw {
                    Some("dxva2") => &[(AV_HWDEVICE_TYPE_DXVA2, "DXVA2")],
                    Some("d3d11va") => &[(AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA")],
                    Some("nvdec") | Some("cuda") => &[(AV_HWDEVICE_TYPE_CUDA, "NVDEC (CUDA)")],
                    _ => &[
                        (AV_HWDEVICE_TYPE_CUDA, "NVDEC (CUDA)"),
                        (AV_HWDEVICE_TYPE_DXVA2, "DXVA2"),
                        (AV_HWDEVICE_TYPE_D3D11VA, "D3D11VA"),
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

            let open_ret = (api.avcodec_open2)(codec_ctx, codec, ptr::null_mut());
            if open_ret < 0 {
                let mut ctx_ptr = codec_ctx;
                (api.avcodec_free_context)(&mut ctx_ptr);
                return Err(format!("avcodec_open2 failed ({})", open_ret));
            }

            let pkt = (api.av_packet_alloc)();
            let raw_frame = (api.av_frame_alloc)();
            let sw_frame = (api.av_frame_alloc)();

            println!("[Decoder] Initialized hardware decoder backend: {}", backend_name);

            Ok(Self {
                api,
                codec_ctx,
                hw_device_ctx,
                pkt,
                raw_frame,
                sw_frame,
                backend_name,
            })
        }
    }

    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    /// Feeds a single H.264 NAL packet into the hardware decoder and invokes `on_frame`
    /// for each immediately decoded frame.
    pub fn decode_packet<F>(&mut self, packet: &NalPacket, mut on_frame: F) -> Result<(), String>
    where
        F: FnMut(DecodedFrameView<'_>),
    {
        unsafe {
            (self.api.av_packet_unref)(self.pkt);
            (*self.pkt).data = packet.payload.as_ptr() as *mut u8;
            (*self.pkt).size = packet.payload.len() as i32;
            (*self.pkt).pts = packet.pts_us as i64;
            (*self.pkt).dts = packet.pts_us as i64;

            let send_ret = (self.api.avcodec_send_packet)(self.codec_ctx, self.pkt);
            if send_ret < 0 && send_ret != AVERROR_EAGAIN {
                return Err(format!("avcodec_send_packet error: {}", send_ret));
            }

            loop {
                (self.api.av_frame_unref)(self.raw_frame);
                let recv_ret = (self.api.avcodec_receive_frame)(self.codec_ctx, self.raw_frame);
                if recv_ret == AVERROR_EAGAIN || recv_ret == AVERROR_EOF {
                    break;
                }
                if recv_ret < 0 {
                    return Err(format!("avcodec_receive_frame error: {}", recv_ret));
                }

                let fmt = (*self.raw_frame).format;
                let target_frame = if fmt == AV_PIX_FMT_CUDA
                    || fmt == AV_PIX_FMT_DXVA2_VLD
                    || fmt == AV_PIX_FMT_D3D11
                {
                    (self.api.av_frame_unref)(self.sw_frame);
                    let xfer =
                        (self.api.av_hwframe_transfer_data)(self.sw_frame, self.raw_frame, 0);
                    if xfer < 0 {
                        continue;
                    }
                    self.sw_frame
                } else {
                    self.raw_frame
                };

                let width = (*target_frame).width as u32;
                let height = (*target_frame).height as u32;
                if width == 0 || height == 0 || (*target_frame).data[0].is_null() {
                    continue;
                }

                let y_pitch = (*target_frame).linesize[0];
                let y_len = (y_pitch.unsigned_abs() as usize) * (height as usize);
                let y_plane = std::slice::from_raw_parts((*target_frame).data[0], y_len);

                if (*target_frame).format == AV_PIX_FMT_NV12
                    || (*target_frame).data[2].is_null()
                {
                    let uv_pitch = (*target_frame).linesize[1];
                    let uv_len = (uv_pitch.unsigned_abs() as usize) * ((height as usize) / 2);
                    let uv_plane = std::slice::from_raw_parts((*target_frame).data[1], uv_len);

                    on_frame(DecodedFrameView {
                        width,
                        height,
                        layout: PixelLayout::Nv12,
                        y_plane,
                        y_pitch,
                        u_or_uv_plane: uv_plane,
                        u_or_uv_pitch: uv_pitch,
                        v_plane: None,
                        v_pitch: 0,
                    });
                } else {
                    let u_pitch = (*target_frame).linesize[1];
                    let v_pitch = (*target_frame).linesize[2];
                    let chroma_h = (height as usize) / 2;
                    let u_len = (u_pitch.unsigned_abs() as usize) * chroma_h;
                    let v_len = (v_pitch.unsigned_abs() as usize) * chroma_h;
                    let u_plane = std::slice::from_raw_parts((*target_frame).data[1], u_len);
                    let v_plane = std::slice::from_raw_parts((*target_frame).data[2], v_len);

                    on_frame(DecodedFrameView {
                        width,
                        height,
                        layout: PixelLayout::Yuv420p,
                        y_plane,
                        y_pitch,
                        u_or_uv_plane: u_plane,
                        u_or_uv_pitch: u_pitch,
                        v_plane: Some(v_plane),
                        v_pitch,
                    });
                }
            }
            Ok(())
        }
    }
}

impl Drop for HwDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.sw_frame.is_null() {
                (self.api.av_frame_free)(&mut self.sw_frame);
            }
            if !self.raw_frame.is_null() {
                (self.api.av_frame_free)(&mut self.raw_frame);
            }
            if !self.pkt.is_null() {
                (self.api.av_packet_free)(&mut self.pkt);
            }
            if !self.codec_ctx.is_null() {
                (self.api.avcodec_free_context)(&mut self.codec_ctx);
            }
            if !self.hw_device_ctx.is_null() {
                (self.api.av_buffer_unref)(&mut self.hw_device_ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hw_decoder_init() {
        match HwDecoder::new(None) {
            Ok(d) => println!("HwDecoder created: {}", d.backend_name()),
            Err(e) => panic!("HwDecoder::new failed: {}", e),
        }
    }
}
