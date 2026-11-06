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
    last_raw_pts_us: Option<u64>,
    accumulated_pts_90k: u64,
    samples: Vec<SampleMeta>,
    audio_samples: Vec<AudioSampleMeta>,
}

impl NativeIsoMp4Muxer {
    pub fn new(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::with_capacity(512 * 1024, file);

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
        let mdat_data_offset = mdat_header_offset + 16;

        Ok(Self {
            writer,
            width,
            height,
            sps: vec![0x67, 0x42, 0x00, 0x1f],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            latest_sps: vec![0x67, 0x42, 0x00, 0x1f],
            latest_pps: vec![0x68, 0xce, 0x3c, 0x80],
            aac_config: vec![0x11, 0x90], // AAC-LC, 48000 Hz, Stereo
            audio_is_aac: false,
            mdat_header_offset,
            current_offset: mdat_data_offset,
            last_raw_pts_us: None,
            accumulated_pts_90k: 0,
            samples: Vec::with_capacity(4096),
            audio_samples: Vec::with_capacity(4096),
        })
    }

    pub fn write_packet(&mut self, packet: &NalPacket) -> io::Result<()> {
        if packet.is_audio {
            if packet.is_config {
                if !packet.payload.is_empty() {
                    self.aac_config = packet.payload.to_vec();
                }
                return Ok(());
            }
            if packet.payload.is_empty() {
                return Ok(());
            }
            if packet.is_keyframe {
                // Hardware-encoded AAC-LC access unit (1024 samples at 48kHz)
                if !self.audio_is_aac {
                    self.audio_samples.clear();
                    self.audio_is_aac = true;
                }
                let size = packet.payload.len() as u32;
                self.writer.write_all(&packet.payload)?;
                self.audio_samples.push(AudioSampleMeta {
                    offset: self.current_offset,
                    size,
                    num_samples: 1024,
                });
                self.current_offset += size as u64;
            } else if !self.audio_is_aac {
                // Raw 16-bit stereo PCM fallback
                let size = packet.payload.len() as u32;
                let num_samples = size / 4;
                self.writer.write_all(&packet.payload)?;
                self.audio_samples.push(AudioSampleMeta {
                    offset: self.current_offset,
                    size,
                    num_samples,
                });
                self.current_offset += size as u64;
            }
            return Ok(());
        }

        let (maybe_sps, maybe_pps) = extract_sps_pps(&packet.payload);
        if let Some(sps) = maybe_sps {
            if self.samples.is_empty() {
                self.sps = sps.clone();
            }
            self.latest_sps = sps;
        }
        if let Some(pps) = maybe_pps {
            if self.samples.is_empty() {
                self.pps = pps.clone();
            }
            self.latest_pps = pps;
        }
        if packet.is_config {
            return Ok(());
        }

        let mut avcc_bytes = annexb_to_avcc_sample(&packet.payload);
        if avcc_bytes.is_empty() {
            return Ok(());
        }

        if (packet.is_keyframe || self.samples.is_empty())
            && (self.latest_sps != self.sps || self.latest_pps != self.pps)
        {
            let mut with_cfg = Vec::with_capacity(
                8 + self.latest_sps.len() + self.latest_pps.len() + avcc_bytes.len(),
            );
            with_cfg.extend_from_slice(&(self.latest_sps.len() as u32).to_be_bytes());
            with_cfg.extend_from_slice(&self.latest_sps);
            with_cfg.extend_from_slice(&(self.latest_pps.len() as u32).to_be_bytes());
            with_cfg.extend_from_slice(&self.latest_pps);
            with_cfg.extend_from_slice(&avcc_bytes);
            avcc_bytes = with_cfg;
        }

        // Maintain monotonically increasing 90kHz PTS even if encoder restarts on screen rotation
        let pts_90k = match self.last_raw_pts_us {
            None => {
                self.last_raw_pts_us = Some(packet.pts_us);
                0
            }
            Some(prev_us) => {
                let delta_us = if packet.pts_us > prev_us && (packet.pts_us - prev_us) < 2_000_000 {
                    packet.pts_us - prev_us
                } else {
                    16_667 // Smooth 60 FPS step across encoder restarts
                };
                self.last_raw_pts_us = Some(packet.pts_us);
                let step_90k = ((delta_us * 90) / 1_000).max(1);
                self.accumulated_pts_90k += step_90k;
                self.accumulated_pts_90k
            }
        };

        self.writer.write_all(&avcc_bytes)?;
        let size = avcc_bytes.len() as u32;
        self.samples.push(SampleMeta {
            offset: self.current_offset,
            size,
            pts_90k,
            is_sync: packet.is_keyframe || self.samples.is_empty(),
        });
        self.current_offset += size as u64;
        Ok(())
    }

    pub fn finalize(mut self) -> io::Result<()> {
        let mdat_total_size = self.current_offset - self.mdat_header_offset;
        let moov = self.build_moov_box();
        self.writer.write_all(&moov)?;
        self.writer.flush()?;

        // Patch 64-bit mdat size at mdat_header_offset + 8
        self.writer.seek(SeekFrom::Start(self.mdat_header_offset + 8))?;
        self.writer.write_all(&mdat_total_size.to_be_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    fn build_moov_box(&self) -> Vec<u8> {
        let timescale: u32 = 90_000;
        let default_delta: u32 = 1_500; // 60 FPS in 90kHz ticks

        let mut deltas = Vec::with_capacity(self.samples.len());
        let mut total_duration_90k: u64 = 0;
        for i in 0..self.samples.len() {
            let delta = if i + 1 < self.samples.len() {
                let d = self.samples[i + 1]
                    .pts_90k
                    .saturating_sub(self.samples[i].pts_90k);
                if d == 0 {
                    default_delta
                } else {
                    d.min(u32::MAX as u64) as u32
                }
            } else {
                default_delta
            };
            deltas.push(delta);
            total_duration_90k += delta as u64;
        }

        let audio_dur_90k = if !self.audio_samples.is_empty() {
            let total_audio_frames: u64 = self.audio_samples.iter().map(|s| s.num_samples as u64).sum();
            (total_audio_frames * 90_000) / 48_000
        } else {
            0
        };
        let final_movie_dur_90k = total_duration_90k.max(audio_dur_90k);

        let next_track_id = if self.audio_samples.is_empty() { 2 } else { 3 };
        let mvhd = build_mvhd_box(timescale, final_movie_dur_90k as u32, next_track_id);
        let tkhd = build_tkhd_box(self.width, self.height, total_duration_90k as u32);
        let mdhd = build_mdhd_box(timescale, total_duration_90k as u32);
        let hdlr = build_hdlr_box();
        let vmhd = build_vmhd_box();
        let dinf = build_dinf_box();
        let stbl = self.build_stbl_box(&deltas);

        let minf = wrap_box(b"minf", &[&vmhd, &dinf, &stbl]);
        let mdia = wrap_box(b"mdia", &[&mdhd, &hdlr, &minf]);
        let trak_video = wrap_box(b"trak", &[&tkhd, &mdia]);

        if self.audio_samples.is_empty() {
            wrap_box(b"moov", &[&mvhd, &trak_video])
        } else {
            let total_audio_frames: u64 = self.audio_samples.iter().map(|s| s.num_samples as u64).sum();
            let audio_tkhd = build_audio_tkhd_box(audio_dur_90k as u32);
            let audio_mdhd = build_mdhd_box(48_000, total_audio_frames as u32);
            let audio_hdlr = build_audio_hdlr_box();
            let smhd = build_smhd_box();
            let audio_dinf = build_dinf_box();
            let audio_stbl = self.build_audio_stbl_box();

            let audio_minf = wrap_box(b"minf", &[&smhd, &audio_dinf, &audio_stbl]);
            let audio_mdia = wrap_box(b"mdia", &[&audio_mdhd, &audio_hdlr, &audio_minf]);
            let trak_audio = wrap_box(b"trak", &[&audio_tkhd, &audio_mdia]);

            wrap_box(b"moov", &[&mvhd, &trak_video, &trak_audio])
        }
    }

    fn build_audio_stbl_box(&self) -> Vec<u8> {
        if self.audio_is_aac {
            // Standard ISO-BMFF mp4a (AAC-LC 48kHz Stereo) + esds box
            // Universally playable in Windows 11 Media Player, Movies & TV, VLC, and browsers
            let asc = if self.aac_config.is_empty() {
                &[0x11u8, 0x90u8][..]
            } else {
                &self.aac_config[..]
            };
            let esds_box = build_esds_box(asc);

            let mut mp4a_payload = Vec::new();
            mp4a_payload.extend_from_slice(&[0u8; 6]); // reserved
            mp4a_payload.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
            mp4a_payload.extend_from_slice(&0u16.to_be_bytes()); // version = 0
            mp4a_payload.extend_from_slice(&0u16.to_be_bytes()); // revision_level = 0
            mp4a_payload.extend_from_slice(&0u32.to_be_bytes()); // vendor = 0
            mp4a_payload.extend_from_slice(&2u16.to_be_bytes()); // channels = 2 (stereo)
            mp4a_payload.extend_from_slice(&16u16.to_be_bytes()); // samplesize = 16 bit
            mp4a_payload.extend_from_slice(&0u16.to_be_bytes()); // pre_defined = 0
            mp4a_payload.extend_from_slice(&0u16.to_be_bytes()); // reserved = 0
            mp4a_payload.extend_from_slice(&(48000u32 << 16).to_be_bytes()); // samplerate = 48000.0 (16.16 fp)
            mp4a_payload.extend_from_slice(&esds_box);
            let mp4a_box = wrap_box(b"mp4a", &[&mp4a_payload]);

            let mut stsd_payload = Vec::new();
            stsd_payload.extend_from_slice(&0u32.to_be_bytes()); // version + flags
            stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
            stsd_payload.extend_from_slice(&mp4a_box);
            let stsd = wrap_box(b"stsd", &[&stsd_payload]);

            // stts: each AAC-LC access unit is 1024 samples at 48kHz
            let mut stts_payload = Vec::new();
            stts_payload.extend_from_slice(&0u32.to_be_bytes());
            stts_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
            stts_payload.extend_from_slice(&(self.audio_samples.len() as u32).to_be_bytes());
            stts_payload.extend_from_slice(&1024u32.to_be_bytes());
            let stts = wrap_box(b"stts", &[&stts_payload]);

            // stsc: 1 AAC frame per chunk
            let mut stsc_payload = Vec::new();
            stsc_payload.extend_from_slice(&0u32.to_be_bytes());
            stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
            stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
            stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // samples_per_chunk = 1
            stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1
            let stsc = wrap_box(b"stsc", &[&stsc_payload]);

            // stsz: variable byte size per AAC frame
            let mut stsz_payload = Vec::new();
            stsz_payload.extend_from_slice(&0u32.to_be_bytes());
            stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 (variable)
            stsz_payload.extend_from_slice(&(self.audio_samples.len() as u32).to_be_bytes());
            for a in &self.audio_samples {
                stsz_payload.extend_from_slice(&a.size.to_be_bytes());
            }
            let stsz = wrap_box(b"stsz", &[&stsz_payload]);

            // co64: 64-bit chunk offsets
            let mut co64_payload = Vec::new();
            co64_payload.extend_from_slice(&0u32.to_be_bytes());
            co64_payload.extend_from_slice(&(self.audio_samples.len() as u32).to_be_bytes());
            for a in &self.audio_samples {
                co64_payload.extend_from_slice(&a.offset.to_be_bytes());
            }
            let co64 = wrap_box(b"co64", &[&co64_payload]);

            return wrap_box(b"stbl", &[&stsd, &stts, &stsc, &stsz, &co64]);
        }

        // Fallback: sowt AudioSampleEntry (16-bit little-endian stereo PCM)
        let total_pcm_frames: u32 = self.audio_samples.iter().map(|s| s.num_samples).sum();
        let mut sowt_payload = Vec::new();
        sowt_payload.extend_from_slice(&[0u8; 6]); // reserved
        sowt_payload.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
        sowt_payload.extend_from_slice(&0u16.to_be_bytes()); // version = 0
        sowt_payload.extend_from_slice(&0u16.to_be_bytes()); // revision_level = 0
        sowt_payload.extend_from_slice(&0u32.to_be_bytes()); // vendor = 0
        sowt_payload.extend_from_slice(&2u16.to_be_bytes()); // channels = 2 (stereo)
        sowt_payload.extend_from_slice(&16u16.to_be_bytes()); // samplesize = 16 bit
        sowt_payload.extend_from_slice(&0u16.to_be_bytes()); // compression_id = 0
        sowt_payload.extend_from_slice(&0u16.to_be_bytes()); // packet_size = 0
        sowt_payload.extend_from_slice(&(48000u32 << 16).to_be_bytes()); // samplerate = 48000.0 (16.16 fp)
        let sowt_box = wrap_box(b"sowt", &[&sowt_payload]);

        let mut stsd_payload = Vec::new();
        stsd_payload.extend_from_slice(&0u32.to_be_bytes()); // version + flags
        stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        stsd_payload.extend_from_slice(&sowt_box);
        let stsd = wrap_box(b"stsd", &[&stsd_payload]);

        // stts: each PCM sample frame has duration = 1 tick at 48kHz
        let mut stts_payload = Vec::new();
        stts_payload.extend_from_slice(&0u32.to_be_bytes());
        stts_payload.extend_from_slice(&1u32.to_be_bytes());
        stts_payload.extend_from_slice(&total_pcm_frames.to_be_bytes());
        stts_payload.extend_from_slice(&1u32.to_be_bytes());
        let stts = wrap_box(b"stts", &[&stts_payload]);

        // stsc: each chunk i has a.num_samples PCM frames
        let mut stsc_payload = Vec::new();
        stsc_payload.extend_from_slice(&0u32.to_be_bytes());
        stsc_payload.extend_from_slice(&(self.audio_samples.len() as u32).to_be_bytes());
        for (idx, a) in self.audio_samples.iter().enumerate() {
            stsc_payload.extend_from_slice(&((idx + 1) as u32).to_be_bytes());
            stsc_payload.extend_from_slice(&a.num_samples.to_be_bytes());
            stsc_payload.extend_from_slice(&1u32.to_be_bytes());
        }
        let stsc = wrap_box(b"stsc", &[&stsc_payload]);

        // stsz: constant 4 bytes per 16-bit stereo PCM sample frame
        let mut stsz_payload = Vec::new();
        stsz_payload.extend_from_slice(&0u32.to_be_bytes());
        stsz_payload.extend_from_slice(&4u32.to_be_bytes()); // sample_size = 4 bytes
        stsz_payload.extend_from_slice(&total_pcm_frames.to_be_bytes());
        let stsz = wrap_box(b"stsz", &[&stsz_payload]);

        // co64: 64-bit chunk offsets
        let mut co64_payload = Vec::new();
        co64_payload.extend_from_slice(&0u32.to_be_bytes());
        co64_payload.extend_from_slice(&(self.audio_samples.len() as u32).to_be_bytes());
        for a in &self.audio_samples {
            co64_payload.extend_from_slice(&a.offset.to_be_bytes());
        }
        let co64 = wrap_box(b"co64", &[&co64_payload]);

        wrap_box(b"stbl", &[&stsd, &stts, &stsc, &stsz, &co64])
    }

    fn build_stbl_box(&self, deltas: &[u32]) -> Vec<u8> {
        // 1. avcC (AVC Decoder Configuration Record)
        let profile_idc = *self.sps.get(1).unwrap_or(&0x42);
        let profile_compat = *self.sps.get(2).unwrap_or(&0x00);
        let level_idc = *self.sps.get(3).unwrap_or(&0x1f);

        let mut avcc_payload = Vec::new();
        avcc_payload.push(1); // configurationVersion = 1
        avcc_payload.push(profile_idc);
        avcc_payload.push(profile_compat);
        avcc_payload.push(level_idc);
        avcc_payload.push(0xFF); // 4-byte NAL length size minus 1
        avcc_payload.push(0xE1); // 1 SPS
        avcc_payload.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        avcc_payload.extend_from_slice(&self.sps);
        avcc_payload.push(1); // 1 PPS
        avcc_payload.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        avcc_payload.extend_from_slice(&self.pps);
        let avcc_box = wrap_box(b"avcC", &[&avcc_payload]);

        // 2. avc1 VisualSampleEntry
        let mut avc1_payload = Vec::new();
        avc1_payload.extend_from_slice(&[0u8; 6]); // reserved
        avc1_payload.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
        avc1_payload.extend_from_slice(&[0u8; 16]); // pre_defined + reserved
        avc1_payload.extend_from_slice(&(self.width as u16).to_be_bytes());
        avc1_payload.extend_from_slice(&(self.height as u16).to_be_bytes());
        avc1_payload.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution 72 dpi
        avc1_payload.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution 72 dpi
        avc1_payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
        avc1_payload.extend_from_slice(&1u16.to_be_bytes()); // frame_count = 1
        avc1_payload.extend_from_slice(&[0u8; 32]); // compressorname
        avc1_payload.extend_from_slice(&0x0018u16.to_be_bytes()); // depth = 24
        avc1_payload.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined = -1
        avc1_payload.extend_from_slice(&avcc_box);
        let avc1_box = wrap_box(b"avc1", &[&avc1_payload]);

        let mut stsd_payload = Vec::new();
        stsd_payload.extend_from_slice(&0u32.to_be_bytes()); // version + flags
        stsd_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        stsd_payload.extend_from_slice(&avc1_box);
        let stsd = wrap_box(b"stsd", &[&stsd_payload]);

        // 3. stts (Decoding Time to Sample)
        let mut stts_payload = Vec::new();
        stts_payload.extend_from_slice(&0u32.to_be_bytes());
        stts_payload.extend_from_slice(&(deltas.len() as u32).to_be_bytes());
        for &d in deltas {
            stts_payload.extend_from_slice(&1u32.to_be_bytes());
            stts_payload.extend_from_slice(&d.to_be_bytes());
        }
        let stts = wrap_box(b"stts", &[&stts_payload]);

        // 4. stss (Sync Sample Box - keyframes)
        let sync_indices: Vec<u32> = self
            .samples
            .iter()
            .enumerate()
            .filter_map(|(idx, s)| if s.is_sync { Some((idx + 1) as u32) } else { None })
            .collect();
        let mut stss_payload = Vec::new();
        stss_payload.extend_from_slice(&0u32.to_be_bytes());
        stss_payload.extend_from_slice(&(sync_indices.len() as u32).to_be_bytes());
        for idx in sync_indices {
            stss_payload.extend_from_slice(&idx.to_be_bytes());
        }
        let stss = wrap_box(b"stss", &[&stss_payload]);

        // 5. stsc (Sample to Chunk - 1 sample per chunk)
        let mut stsc_payload = Vec::new();
        stsc_payload.extend_from_slice(&0u32.to_be_bytes());
        stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
        stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // samples_per_chunk = 1
        stsc_payload.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1
        let stsc = wrap_box(b"stsc", &[&stsc_payload]);

        // 6. stsz (Sample Size Box)
        let mut stsz_payload = Vec::new();
        stsz_payload.extend_from_slice(&0u32.to_be_bytes());
        stsz_payload.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 (variable)
        stsz_payload.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
        for s in &self.samples {
            stsz_payload.extend_from_slice(&s.size.to_be_bytes());
        }
        let stsz = wrap_box(b"stsz", &[&stsz_payload]);

        // 7. co64 (64-bit Chunk Offset Box)
        let mut co64_payload = Vec::new();
        co64_payload.extend_from_slice(&0u32.to_be_bytes());
        co64_payload.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
        for s in &self.samples {
            co64_payload.extend_from_slice(&s.offset.to_be_bytes());
        }
        let co64 = wrap_box(b"co64", &[&co64_payload]);

        wrap_box(b"stbl", &[&stsd, &stts, &stss, &stsc, &stsz, &co64])
    }
}

fn build_esds_box(asc: &[u8]) -> Vec<u8> {
    let dsi_len = asc.len();
    let dec_cfg_len = 13 + 2 + dsi_len;
    let es_desc_len = 3 + 2 + dec_cfg_len + 3;

    let mut p = Vec::with_capacity(4 + 2 + es_desc_len);
    p.extend_from_slice(&0u32.to_be_bytes()); // FullBox version=0, flags=0

    // ES_Descriptor (tag 0x03)
    p.push(0x03);
    p.push(es_desc_len as u8);
    p.extend_from_slice(&2u16.to_be_bytes()); // ES_ID = 2
    p.push(0x00); // streamDependenceFlag=0, URL_Flag=0, OCRstreamFlag=0, streamPriority=0

    // DecoderConfigDescriptor (tag 0x04)
    p.push(0x04);
    p.push(dec_cfg_len as u8);
    p.push(0x40); // objectTypeIndication = 0x40 (Audio ISO/IEC 14496-3 / AAC)
    p.push(0x15); // streamType = 5 (AudioStream) << 2 | 1
    p.extend_from_slice(&[0x00, 0x20, 0x00]); // bufferSizeDB = 8192
    p.extend_from_slice(&128_000u32.to_be_bytes()); // maxBitrate
    p.extend_from_slice(&128_000u32.to_be_bytes()); // avgBitrate

    // DecoderSpecificInfo (tag 0x05)
    p.push(0x05);
    p.push(dsi_len as u8);
    p.extend_from_slice(asc);

    // SLConfigDescriptor (tag 0x06)
    p.push(0x06);
    p.push(0x01);
    p.push(0x02); // predefined = 2 (MP4)

    wrap_box(b"esds", &[&p])
}

fn wrap_box(fourcc: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let payload_len: usize = parts.iter().map(|p| p.len()).sum();
    let total_len = (8 + payload_len) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(fourcc);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

fn build_mvhd_box(timescale: u32, duration: u32, next_track_id: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(100);
    p.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    p.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    p.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate = 1.0
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume = 1.0
    p.extend_from_slice(&[0u8; 10]); // reserved
    // Identity matrix (36 bytes)
    for &v in &[
        0x0001_0000u32,
        0,
        0,
        0,
        0x0001_0000,
        0,
        0,
        0,
        0x4000_0000,
    ] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&[0u8; 24]); // pre_defined
    p.extend_from_slice(&next_track_id.to_be_bytes());
    wrap_box(b"mvhd", &[&p])
}

fn build_audio_tkhd_box(duration: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(84);
    p.extend_from_slice(&0x0000_0003u32.to_be_bytes()); // flags: track_enabled | track_in_movie
    p.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    p.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    p.extend_from_slice(&2u32.to_be_bytes()); // track_ID = 2
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&[0u8; 8]); // reserved
    p.extend_from_slice(&0u16.to_be_bytes()); // layer
    p.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume = 1.0 (audio)
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for &v in &[
        0x0001_0000u32,
        0,
        0,
        0,
        0x0001_0000,
        0,
        0,
        0,
        0x4000_0000,
    ] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&0u32.to_be_bytes()); // width = 0
    p.extend_from_slice(&0u32.to_be_bytes()); // height = 0
    wrap_box(b"tkhd", &[&p])
}

fn build_tkhd_box(width: u32, height: u32, duration: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(84);
    p.extend_from_slice(&0x0000_0003u32.to_be_bytes()); // flags: track_enabled | track_in_movie
    p.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    p.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    p.extend_from_slice(&1u32.to_be_bytes()); // track_ID = 1
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&[0u8; 8]); // reserved
    p.extend_from_slice(&0u16.to_be_bytes()); // layer
    p.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    p.extend_from_slice(&0u16.to_be_bytes()); // volume = 0 (video)
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for &v in &[
        0x0001_0000u32,
        0,
        0,
        0,
        0x0001_0000,
        0,
        0,
        0,
        0x4000_0000,
    ] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&(width << 16).to_be_bytes());
    p.extend_from_slice(&(height << 16).to_be_bytes());
    wrap_box(b"tkhd", &[&p])
}

fn build_mdhd_box(timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&0x55C4u16.to_be_bytes()); // language = "und"
    p.extend_from_slice(&0u16.to_be_bytes());
    wrap_box(b"mdhd", &[&p])
}

fn build_hdlr_box() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    p.extend_from_slice(b"vide"); // handler_type
    p.extend_from_slice(&[0u8; 12]); // reserved
    p.extend_from_slice(b"VideoHandler\0");
    wrap_box(b"hdlr", &[&p])
}

fn build_audio_hdlr_box() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // version + flags
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    p.extend_from_slice(b"soun"); // handler_type
    p.extend_from_slice(&[0u8; 12]); // reserved
    p.extend_from_slice(b"SoundHandler\0");
    wrap_box(b"hdlr", &[&p])
}

fn build_vmhd_box() -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version 0, flags 1
    p.extend_from_slice(&[0u8; 8]); // graphicsmode + opcolor
    wrap_box(b"vmhd", &[&p])
}

fn build_smhd_box() -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
    p.extend_from_slice(&0u16.to_be_bytes()); // balance = 0
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved = 0
    wrap_box(b"smhd", &[&p])
}

fn build_dinf_box() -> Vec<u8> {
    let url_box = wrap_box(b"url ", &[&0x0000_0001u32.to_be_bytes()]);
    let mut dref_payload = Vec::new();
    dref_payload.extend_from_slice(&0u32.to_be_bytes());
    dref_payload.extend_from_slice(&1u32.to_be_bytes());
    dref_payload.extend_from_slice(&url_box);
    let dref = wrap_box(b"dref", &[&dref_payload]);
    wrap_box(b"dinf", &[&dref])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn test_zero_copy_mp4_muxer_writes_valid_container() {
        let tmp_path = std::env::temp_dir().join("mobile_mirror_test_output.mp4");
        let _ = std::fs::remove_file(&tmp_path);

        let mut muxer = NativeIsoMp4Muxer::new(&tmp_path, 1920, 1080).unwrap();

        // 1. Config packet (SPS + PPS)
        let config_pkt = NalPacket {
            pts_us: 0,
            is_config: true,
            is_keyframe: false,
            is_audio: false,
            payload: Arc::from(
                &b"\x00\x00\x00\x01\x67\x42\x00\x1f\xe9\x00\x00\x00\x01\x68\xce\x3c\x80"[..],
            ),
            received_at: Instant::now(),
        };
        muxer.write_packet(&config_pkt).unwrap();

        // 2. IDR keyframe packet
        let idr_pkt = NalPacket {
            pts_us: 1000,
            is_config: false,
            is_keyframe: true,
            is_audio: false,
            payload: Arc::from(&b"\x00\x00\x00\x01\x65\x88\x84\x21\xa0"[..]),
            received_at: Instant::now(),
        };
        muxer.write_packet(&idr_pkt).unwrap();

        // 3. Non-IDR P-frame packet
        let p_pkt = NalPacket {
            pts_us: 17667,
            is_config: false,
            is_keyframe: false,
            is_audio: false,
            payload: Arc::from(&b"\x00\x00\x00\x01\x41\x9a\x24\x6c"[..]),
            received_at: Instant::now(),
        };
        muxer.write_packet(&p_pkt).unwrap();

        muxer.finalize().unwrap();

        let bytes = std::fs::read(&tmp_path).unwrap();
        assert!(bytes.len() > 200);
        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(&bytes[32..36], b"mdat");
        assert!(bytes.windows(4).any(|w| w == b"moov"));
        assert!(bytes.windows(4).any(|w| w == b"avcC"));

        let _ = std::fs::remove_file(&tmp_path);
    }
}
