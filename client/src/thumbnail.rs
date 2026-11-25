use std::path::Path;

/// In-memory decoded video frame thumbnail ready for UiCanvas software blitting.
#[derive(Debug, Clone)]
pub struct VideoThumbnail {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>, // 0xAARRGGBB format
}

/// Converts YUV420P planar buffers into aspect-preserved ARGB pixels centered in `target_w x target_h`.
pub fn yuv420p_to_argb_centered(
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
    src_w: usize,
    src_h: usize,
    target_w: usize,
    target_h: usize,
) -> Vec<u32> {
    let mut out = vec![0xFF0E_1217u32; target_w * target_h]; // Dark cinema black background
    if src_w == 0 || src_h == 0 || target_w == 0 || target_h == 0 {
        return out;
    }

    // Preserve aspect ratio: scale to fit inside target_w x target_h
    let scale_x = target_w as f32 / src_w as f32;
    let scale_y = target_h as f32 / src_h as f32;
    let scale = scale_x.min(scale_y);

    let fit_w = (src_w as f32 * scale).round().max(1.0) as usize;
    let fit_h = (src_h as f32 * scale).round().max(1.0) as usize;

    let offset_x = (target_w.saturating_sub(fit_w)) / 2;
    let offset_y = (target_h.saturating_sub(fit_h)) / 2;

    for dy in 0..fit_h {
        let ty = offset_y + dy;
        if ty >= target_h {
            break;
        }
        let sy = ((dy as f32 / fit_h as f32) * src_h as f32) as usize;
        let sy = sy.min(src_h - 1);
        let y_row = sy * y_stride;
        let uv_row = (sy / 2) * u_stride;
        let v_row = (sy / 2) * v_stride;
        let out_row = ty * target_w;

        for dx in 0..fit_w {
            let tx = offset_x + dx;
            if tx >= target_w {
                break;
            }
            let sx = ((dx as f32 / fit_w as f32) * src_w as f32) as usize;
            let sx = sx.min(src_w - 1);

            let y_idx = y_row + sx;
            let uv_idx = uv_row + (sx / 2);
            let v_idx = v_row + (sx / 2);

            let y_val = y_plane.get(y_idx).copied().unwrap_or(16) as i32;
            let u_val = u_plane.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let v_val = v_plane.get(v_idx).copied().unwrap_or(128) as i32 - 128;

            let r = (y_val + ((1402 * v_val) >> 10)).clamp(0, 255) as u32;
            let g = (y_val - ((344 * u_val + 714 * v_val) >> 10)).clamp(0, 255) as u32;
            let b = (y_val + ((1772 * u_val) >> 10)).clamp(0, 255) as u32;

            out[out_row + tx] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        }
    }

    out
}

/// Converts NV12 semi-planar buffers into aspect-preserved ARGB pixels centered in `target_w x target_h`.
pub fn nv12_to_argb_centered(
    y_plane: &[u8],
    y_stride: usize,
    uv_plane: &[u8],
    uv_stride: usize,
    src_w: usize,
    src_h: usize,
    target_w: usize,
    target_h: usize,
) -> Vec<u32> {
    let mut out = vec![0xFF0E_1217u32; target_w * target_h];
    if src_w == 0 || src_h == 0 || target_w == 0 || target_h == 0 {
        return out;
    }

    let scale_x = target_w as f32 / src_w as f32;
    let scale_y = target_h as f32 / src_h as f32;
    let scale = scale_x.min(scale_y);

    let fit_w = (src_w as f32 * scale).round().max(1.0) as usize;
    let fit_h = (src_h as f32 * scale).round().max(1.0) as usize;

    let offset_x = (target_w.saturating_sub(fit_w)) / 2;
    let offset_y = (target_h.saturating_sub(fit_h)) / 2;

    for dy in 0..fit_h {
        let ty = offset_y + dy;
        if ty >= target_h {
            break;
        }
        let sy = ((dy as f32 / fit_h as f32) * src_h as f32) as usize;
        let sy = sy.min(src_h - 1);
        let y_row = sy * y_stride;
        let uv_row = (sy / 2) * uv_stride;
        let out_row = ty * target_w;

        for dx in 0..fit_w {
            let tx = offset_x + dx;
            if tx >= target_w {
                break;
            }
            let sx = ((dx as f32 / fit_w as f32) * src_w as f32) as usize;
            let sx = sx.min(src_w - 1);

            let y_idx = y_row + sx;
            let uv_idx = uv_row + (sx / 2) * 2;

            let y_val = y_plane.get(y_idx).copied().unwrap_or(16) as i32;
            let u_val = uv_plane.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
            let v_val = uv_plane.get(uv_idx + 1).copied().unwrap_or(128) as i32 - 128;

            let r = (y_val + ((1402 * v_val) >> 10)).clamp(0, 255) as u32;
            let g = (y_val - ((344 * u_val + 714 * v_val) >> 10)).clamp(0, 255) as u32;
            let b = (y_val + ((1772 * u_val) >> 10)).clamp(0, 255) as u32;

            out[out_row + tx] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        }
    }

    out
}

/// Scans and extracts the first H.264 keyframe (with SPS & PPS) from an MP4 container file.
/// Parses `moov` (`avcC`) for parameter sets and extracts the first IDR slice from `mdat`.
pub fn extract_first_keyframe_from_mp4(path: &Path) -> Option<Vec<u8>> {
    use std::fs::File;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    let mut file = BufReader::new(File::open(path).ok()?);
    let file_len = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;

    let mut moov_data: Option<Vec<u8>> = None;
    let mut mdat_offset: Option<u64> = None;
    let mut mdat_len: Option<u64> = None;

    let mut cur_pos = 0u64;
    while cur_pos + 8 <= file_len {
        if file.seek(SeekFrom::Start(cur_pos)).is_err() {
            break;
        }
        let mut header = [0u8; 8];
        if file.read_exact(&mut header).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let box_type = &header[4..8];

        let (box_len, payload_offset) = if size32 == 1 {
            let mut ext = [0u8; 8];
            if file.read_exact(&mut ext).is_err() {
                break;
            }
            let ext_size = u64::from_be_bytes(ext);
            (ext_size, cur_pos + 16)
        } else if size32 == 0 {
            (file_len - cur_pos, cur_pos + 8)
        } else {
            (size32, cur_pos + 8)
        };

        if box_type == b"moov" && box_len > 8 && box_len < 64 * 1024 * 1024 {
            let payload_size = (box_len.saturating_sub(payload_offset - cur_pos)) as usize;
            if file.seek(SeekFrom::Start(payload_offset)).is_ok() {
                let mut buf = vec![0u8; payload_size];
                if file.read_exact(&mut buf).is_ok() {
                    moov_data = Some(buf);
                }
            }
        } else if box_type == b"mdat" {
            mdat_offset = Some(payload_offset);
            mdat_len = Some(box_len.saturating_sub(payload_offset - cur_pos));
        }

        if box_len == 0 {
            break;
        }
        cur_pos = match cur_pos.checked_add(box_len) {
            Some(p) => p,
            None => break,
        };
    }

    // 1. Extract SPS & PPS from moov avcC atom
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;

    if let Some(ref moov) = moov_data {
        if let Some(pos) = moov.windows(4).position(|w| w == b"avcC") {
            let avcc = &moov[pos + 4..];
            if avcc.len() >= 8 {
                let num_sps = avcc[5] & 0x1F;
                let mut offset = 6;
                for _ in 0..num_sps {
                    if offset + 2 <= avcc.len() {
                        let len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
                        offset += 2;
                        if offset + len <= avcc.len() {
                            sps = Some(avcc[offset..offset + len].to_vec());
                            offset += len;
                        }
                    }
                }
                if offset < avcc.len() {
                    let num_pps = avcc[offset];
                    offset += 1;
                    for _ in 0..num_pps {
                        if offset + 2 <= avcc.len() {
                            let len = u16::from_be_bytes([avcc[offset], avcc[offset + 1]]) as usize;
                            offset += 2;
                            if offset + len <= avcc.len() {
                                pps = Some(avcc[offset..offset + len].to_vec());
                                offset += len;
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Read first keyframe sample from mdat
    let mdat_pos = mdat_offset.unwrap_or(32);
    if file.seek(SeekFrom::Start(mdat_pos)).is_err() {
        return None;
    }

    let read_limit = (3 * 1024 * 1024).min(mdat_len.unwrap_or(3 * 1024 * 1024)) as usize;
    let mut mdat_buf = vec![0u8; read_limit];
    let bytes_read = file.read(&mut mdat_buf).ok()?;
    mdat_buf.truncate(bytes_read);

    if mdat_buf.is_empty() {
        return None;
    }

    // A: Parse AVCC samples [4-byte big-endian length][NAL payload]
    let mut sample_offset = 0;
    let mut keyframe_nal: Option<Vec<u8>> = None;

    while sample_offset + 4 < mdat_buf.len() {
        let sample_len = u32::from_be_bytes([
            mdat_buf[sample_offset],
            mdat_buf[sample_offset + 1],
            mdat_buf[sample_offset + 2],
            mdat_buf[sample_offset + 3],
        ]) as usize;

        sample_offset += 4;
        if sample_len == 0 || sample_offset + sample_len > mdat_buf.len() {
            break;
        }

        let nal = &mdat_buf[sample_offset..sample_offset + sample_len];
        if !nal.is_empty() {
            let nal_type = nal[0] & 0x1F;
            if nal_type == 7 && sps.is_none() {
                sps = Some(nal.to_vec());
            } else if nal_type == 8 && pps.is_none() {
                pps = Some(nal.to_vec());
            } else if nal_type == 5 {
                keyframe_nal = Some(nal.to_vec());
                break;
            } else if nal_type == 1 && keyframe_nal.is_none() {
                keyframe_nal = Some(nal.to_vec());
            }
        }
        sample_offset += sample_len;
    }

    // B: Fallback - scan for Annex-B start codes [0, 0, 0, 1]
    if keyframe_nal.is_none() {
        let mut i = 0;
        while i + 4 < mdat_buf.len() {
            if mdat_buf[i] == 0 && mdat_buf[i + 1] == 0 && mdat_buf[i + 2] == 0 && mdat_buf[i + 3] == 1 {
                let nal_start = i + 4;
                let nal_type = mdat_buf[nal_start] & 0x1F;
                let mut next = nal_start;
                while next + 4 < mdat_buf.len() {
                    if mdat_buf[next] == 0 && mdat_buf[next + 1] == 0 && mdat_buf[next + 2] == 0 && mdat_buf[next + 3] == 1 {
                        break;
                    }
                    next += 1;
                }
                let nal = &mdat_buf[nal_start..next];
                if nal_type == 7 && sps.is_none() {
                    sps = Some(nal.to_vec());
                } else if nal_type == 8 && pps.is_none() {
                    pps = Some(nal.to_vec());
                } else if nal_type == 5 {
                    keyframe_nal = Some(nal.to_vec());
                    break;
                }
                i = next;
            } else {
                i += 1;
            }
        }
    }

    let keyframe = keyframe_nal?;
    let mut annexb = Vec::with_capacity(64 + keyframe.len());
    if let Some(ref s) = sps {
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(s);
    }
    if let Some(ref p) = pps {
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(p);
    }
    annexb.extend_from_slice(&[0, 0, 0, 1]);
    annexb.extend_from_slice(&keyframe);

    Some(annexb)
}

/// Extracts a decoded video frame thumbnail from an MP4 file using the low-delay H.264 decoder.
pub fn extract_video_thumbnail(
    path: &Path,
    target_w: usize,
    target_h: usize,
) -> Option<VideoThumbnail> {
    if !path.is_file() {
        return None;
    }

    let nal_payload = extract_first_keyframe_from_mp4(path)?;
    let mut decoder = crate::decoder::HwDecoder::new(Some("cpu")).ok()?;
    let pkt = crate::protocol::NalPacket {
        payload: nal_payload.into(),
        pts_us: 0,
        is_keyframe: true,
        is_config: false,
        is_audio: false,
        received_at: std::time::Instant::now(),
    };

    let _ = decoder.decode_packet(&pkt);

    let mut result_thumb = None;
    decoder.with_current_frame(|view| {
        let pixels = match view.layout {
            crate::decoder::PixelLayout::Nv12 => nv12_to_argb_centered(
                view.y_plane,
                view.y_pitch as usize,
                view.u_or_uv_plane,
                view.u_or_uv_pitch as usize,
                view.width as usize,
                view.height as usize,
                target_w,
                target_h,
            ),
            crate::decoder::PixelLayout::Yuv420p => yuv420p_to_argb_centered(
                view.y_plane,
                view.y_pitch as usize,
                view.u_or_uv_plane,
                view.u_or_uv_pitch as usize,
                view.v_plane.unwrap_or(&[]),
                view.v_pitch as usize,
                view.width as usize,
                view.height as usize,
                target_w,
                target_h,
            ),
        };
        result_thumb = Some(VideoThumbnail {
            width: target_w,
            height: target_h,
            pixels,
        });
    });

    result_thumb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sample_mp4() {
        let sample_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("recordings")
            .join("recording_20261118_174859.mp4");
        if sample_path.exists() {
            let thumb = extract_video_thumbnail(&sample_path, 240, 136);
            assert!(thumb.is_some(), "Should extract valid keyframe thumbnail");
            let t = thumb.unwrap();
            assert_eq!(t.width, 240);
            assert_eq!(t.height, 136);
            assert_eq!(t.pixels.len(), 240 * 136);
        }
    }

    #[test]
    fn test_yuv420p_conversion() {
        let w = 16;
        let h = 16;
        let y = vec![128u8; w * h];
        let u = vec![128u8; (w / 2) * (h / 2)];
        let v = vec![128u8; (w / 2) * (h / 2)];

        let thumb = yuv420p_to_argb_centered(&y, w, &u, w / 2, &v, w / 2, w, h, 32, 18);
        assert_eq!(thumb.len(), 32 * 18);
        // Middle pixels should be gray (y=128, u=128, v=128)
        let mid = thumb[9 * 32 + 16];
        assert_eq!(mid & 0xFF00_0000, 0xFF00_0000);
    }
}
