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
