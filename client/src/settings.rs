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
                    "quality" => s.quality = MirrorQuality::from_str_lossy(val),
                    "target_fps" => {
                        if let Ok(n) = val.parse::<u32>() {
                            s.target_fps = n.clamp(15, 144);
                        }
                    }
                    "bitrate_mbps" => {
                        if let Ok(n) = val.parse::<u32>() {
                            s.bitrate_mbps = n.clamp(2, 50);
                        }
                    }
                    "cbr_mode" => s.cbr_mode = val.eq_ignore_ascii_case("true"),
                    "mic_enabled" => s.mic_enabled = val.eq_ignore_ascii_case("true"),
                    "pc_audio_enabled" => s.pc_audio_enabled = val.eq_ignore_ascii_case("true"),
                    "auto_connect_usb" => s.auto_connect_usb = val.eq_ignore_ascii_case("true"),
                    "auto_record" => s.auto_record = val.eq_ignore_ascii_case("true"),
                    "always_on_top" => s.always_on_top = val.eq_ignore_ascii_case("true"),
                    "hw_backend" => s.hw_backend = HwBackendPref::from_opt_str(Some(val)),
                    "fullscreen_mirror" => s.fullscreen_mirror = val.eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
        }
        s
    }

    pub fn save(&self, record_dir: &Path) {
        let _ = fs::create_dir_all(record_dir);
        let path = Self::settings_file_path(record_dir);
        let data = format!(
            "# Mobile Mirror Persistent Settings\n\
             quality={}\n\
             target_fps={}\n\
             bitrate_mbps={}\n\
             cbr_mode={}\n\
             mic_enabled={}\n\
             pc_audio_enabled={}\n\
             auto_connect_usb={}\n\
             auto_record={}\n\
             always_on_top={}\n\
             hw_backend={}\n\
             fullscreen_mirror={}\n",
            self.quality.label(),
            self.target_fps,
            self.bitrate_mbps,
            self.cbr_mode,
            self.mic_enabled,
            self.pc_audio_enabled,
            self.auto_connect_usb,
            self.auto_record,
            self.always_on_top,
            self.hw_backend.cli_value().unwrap_or("auto"),
            self.fullscreen_mirror,
        );
        let _ = fs::write(path, data);
    }
}

/// Metadata for a saved `.mp4` recording displayed in the `Recordings` tab.
#[derive(Debug, Clone)]
pub struct RecordingEntry {
    pub file_name: String,
    pub full_path: PathBuf,
    pub size_bytes: u64,
    pub modified_epoch_secs: u64,
}

impl RecordingEntry {
    pub fn formatted_size(&self) -> String {
        let mb = self.size_bytes as f64 / (1024.0 * 1024.0);
        if mb >= 1.0 {
            format!("{:.1} MB", mb)
        } else {
            let kb = (self.size_bytes as f64 / 1024.0).max(1.0);
            format!("{:.0} KB", kb)
        }
    }
}

/// Scans `record_dir` for `.mp4` files sorted newest-first.
pub fn scan_recordings(record_dir: &Path) -> Vec<RecordingEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(record_dir) else {
        return entries;
    };
    for item in read_dir.flatten() {
        let path = item.path();
        if !path.is_file() {
            continue;
        }
        let is_mp4 = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("mp4"))
            .unwrap_or(false);
        if !is_mp4 {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("recording.mp4")
            .to_string();
        let meta = fs::metadata(&path).ok();
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified_epoch_secs = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(RecordingEntry {
            file_name,
            full_path: path,
            size_bytes,
            modified_epoch_secs,
        });
    }
    entries.sort_by(|a, b| {
        b.modified_epoch_secs
            .cmp(&a.modified_epoch_secs)
            .then_with(|| b.file_name.cmp(&a.file_name))
    });
    entries
}

/// Opens the `recordings/` directory in the OS file manager (Windows Explorer).
pub fn open_folder_in_explorer(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    let abs = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // Strip Windows UNC `\\?\` prefix if present so explorer.exe handles it cleanly
    let clean_str = abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    #[cfg(windows)]
    {
        let _ = Command::new("explorer.exe").arg(&clean_str).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(&clean_str).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(&clean_str).spawn();
    }
}

/// Opens/plays a specific `.mp4` recording file in the system default media player.
pub fn open_media_file(path: &Path) {
    if !path.exists() {
        return;
    }
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let clean_str = abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("cmd")
            .args(["/C", "start", "", &clean_str])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(&clean_str).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(&clean_str).spawn();
    }
}

/// Reveals/highlights a specific `.mp4` recording file inside Windows Explorer.
pub fn reveal_file_in_explorer(path: &Path) {
    if !path.exists() {
        if let Some(p) = path.parent() {
            open_folder_in_explorer(p);
        }
        return;
    }
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let clean_str = abs
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    #[cfg(windows)]
    {
        let _ = Command::new("explorer.exe")
            .arg(format!("/select,{}", clean_str))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = path.parent() {
            open_folder_in_explorer(p);
        }
    }
}
