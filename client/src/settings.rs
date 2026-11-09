use crate::adb::StreamQualityConfig;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Navigation tabs in the Left Sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Home,
    Recordings,
    Settings,
    About,
}

/// Mirror quality preset matching the right-hand `Mirror Quality` card (`Auto` | `HD` | `FHD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorQuality {
    Auto,
    Hd,
    Fhd,
}

impl MirrorQuality {
    pub fn label(self) -> &'static str {
        match self {
            MirrorQuality::Auto => "Auto",
            MirrorQuality::Hd => "HD",
            MirrorQuality::Fhd => "FHD",
        }
    }

    pub fn caption(self) -> &'static str {
        match self {
            MirrorQuality::Auto => "Native 60 FPS • 24 Mbps Lossless",
            MirrorQuality::Hd => "720p HD 60 FPS • 16 Mbps Low Latency",
            MirrorQuality::Fhd => "1080p+ FHD 60 FPS • 36 Mbps Studio",
        }
    }

    pub fn default_max_size(self) -> u32 {
        match self {
            MirrorQuality::Auto => 0,
            MirrorQuality::Hd => 1280,
            MirrorQuality::Fhd => 0,
        }
    }

    pub fn default_bitrate_mbps(self) -> u32 {
        match self {
            MirrorQuality::Auto => 24,
            MirrorQuality::Hd => 16,
            MirrorQuality::Fhd => 36,
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "hd" | "720p" => MirrorQuality::Hd,
            "fhd" | "1080p" => MirrorQuality::Fhd,
            _ => MirrorQuality::Auto,
        }
    }
}

/// Preferred hardware decoder backend in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwBackendPref {
    Auto,
    Nvdec,
    D3d11va,
    Dxva2,
}

impl HwBackendPref {
    pub fn label(self) -> &'static str {
        match self {
            HwBackendPref::Auto => "Auto (Best GPU)",
            HwBackendPref::Nvdec => "NVIDIA NVDEC",
            HwBackendPref::D3d11va => "Direct3D 11VA",
            HwBackendPref::Dxva2 => "DXVA2",
        }
    }

    pub fn cli_value(self) -> Option<&'static str> {
        match self {
            HwBackendPref::Auto => None,
            HwBackendPref::Nvdec => Some("nvdec"),
            HwBackendPref::D3d11va => Some("d3d11va"),
            HwBackendPref::Dxva2 => Some("dxva2"),
        }
    }

    pub fn next(self) -> Self {
        match self {
            HwBackendPref::Auto => HwBackendPref::Nvdec,
            HwBackendPref::Nvdec => HwBackendPref::D3d11va,
            HwBackendPref::D3d11va => HwBackendPref::Dxva2,
            HwBackendPref::Dxva2 => HwBackendPref::Auto,
        }
    }

    pub fn from_opt_str(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "nvdec" | "cuda" => HwBackendPref::Nvdec,
            "d3d11va" | "d3d11" => HwBackendPref::D3d11va,
            "dxva2" => HwBackendPref::Dxva2,
            _ => HwBackendPref::Auto,
        }
    }
}

/// Complete runtime application settings (every control in the UI & Settings tab is backed by this state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    pub quality: MirrorQuality,
    pub target_fps: u32,
    pub bitrate_mbps: u32,
    pub cbr_mode: bool,
    pub mic_enabled: bool,
    pub pc_audio_enabled: bool,
    pub auto_connect_usb: bool,
    pub auto_record: bool,
    pub always_on_top: bool,
    pub hw_backend: HwBackendPref,
    pub fullscreen_mirror: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            quality: MirrorQuality::Auto,
            target_fps: 60,
            bitrate_mbps: 24,
            cbr_mode: true,
            mic_enabled: false,
            pc_audio_enabled: false,
            auto_connect_usb: true,
            auto_record: false,
            always_on_top: false,
            hw_backend: HwBackendPref::Auto,
            fullscreen_mirror: true,
        }
    }
}

impl AppSettings {
    pub fn apply_quality_preset(&mut self, quality: MirrorQuality) {
        self.quality = quality;
        self.bitrate_mbps = quality.default_bitrate_mbps();
    }

    pub fn cycle_fps(&mut self) {
        self.target_fps = match self.target_fps {
            30 => 60,
            60 => 90,
            90 => 120,
            _ => 30,
        };
    }

    pub fn cycle_bitrate(&mut self) {
        self.bitrate_mbps = match self.bitrate_mbps {
            12 => 16,
            16 => 24,
            24 => 36,
            36 => 48,
            _ => 12,
        };
    }

    pub fn to_stream_quality(&self) -> StreamQualityConfig {
        StreamQualityConfig {
            max_size: self.quality.default_max_size(),
            bitrate: self.bitrate_mbps.clamp(2, 50) * 1_000_000,
            fps: self.target_fps.clamp(15, 144),
            cbr: self.cbr_mode,
        }
    }

    pub fn settings_file_path(record_dir: &Path) -> PathBuf {
        record_dir.join("settings.cfg")
    }

    pub fn load(record_dir: &Path) -> Self {
        let path = Self::settings_file_path(record_dir);
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut s = Self::default();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                match key {
