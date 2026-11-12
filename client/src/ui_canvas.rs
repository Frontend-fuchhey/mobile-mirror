use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use std::fs;
use std::io;
use std::path::Path;

/// Color in sRGB `(R, G, B)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    #[inline]
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let r = (self.0 as f32 + (other.0 as f32 - self.0 as f32) * t).round() as u8;
        let g = (self.1 as f32 + (other.1 as f32 - self.1 as f32) * t).round() as u8;
        let b = (self.2 as f32 + (other.2 as f32 - self.2 as f32) * t).round() as u8;
        Rgb(r, g, b)
    }
}

// ============================================================================
// Design System Palette (Impeccable + UI/UX Pro Max — Cool Tinted Light Mode)
// ============================================================================
pub const BG_ALABASTER: Rgb = Rgb(243, 246, 253); // #F3F6FD Main Dashboard Workspace Background
pub const BG_HEADER: Rgb = Rgb(246, 248, 255); // #F6F8FF Top Title Bar Surface
pub const BG_SIDEBAR: Rgb = Rgb(246, 249, 255); // #F6F9FF Left Navigation Sidebar Surface
pub const CARD_WHITE: Rgb = Rgb(255, 255, 255); // #FFFFFF Pure Elevated Card Surface
pub const BORDER_SUBTLE: Rgb = Rgb(226, 232, 244); // #E2E8F4 Crisp 1px Cool-Tinted Hairline
pub const CARD_BORDER: Rgb = Rgb(234, 239, 249); // #EAEFF9 Subtle Elevated Card Border
pub const SHADOW_SLATE: Rgb = Rgb(15, 23, 42); // #1E293B Tinted Deep Slate Drop Shadow

// Brand & Accent Blues
pub const APPLE_BLUE: Rgb = Rgb(21, 101, 240); // #1565F0 Primary Brand Blue (Active Nav, Auto Pill, Links)
pub const BLUE_GRAD_TOP: Rgb = Rgb(38, 114, 246); // #2672F6 Start Mirroring Button Top Gradient
pub const BLUE_GRAD_BOT: Rgb = Rgb(12, 85, 226); // #0C55E2 Start Mirroring Button Bottom Gradient
pub const BLUE_GRAD_HOVER_TOP: Rgb = Rgb(28, 102, 236); // #1C66EC Hover Top
pub const BLUE_GRAD_HOVER_BOT: Rgb = Rgb(8, 72, 206); // #0848CE Hover Bottom
pub const APPLE_BLUE_SOFT: Rgb = Rgb(224, 235, 255); // #E0EBFF Active Sidebar Pill Fill
pub const APPLE_BLUE_HOVER: Rgb = Rgb(210, 226, 255); // #D2E2FF Active Pill Hover

// Neutral Tinted Controls & Banners
pub const PILL_NEUTRAL: Rgb = Rgb(240, 244, 253); // #F0F4FD Quick Tools Row & Quality Track Fill
pub const PILL_NEUTRAL_HOVER: Rgb = Rgb(228, 235, 250); // #E4EBFA Quick Tools Row Hover Fill
pub const BANNER_USB_BG: Rgb = Rgb(236, 240, 253); // #ECF0FD Bottom "Connect via USB" Banner Fill
pub const BANNER_USB_BORDER: Rgb = Rgb(222, 229, 248); // #DEE5F8 Bottom Banner Border
pub const BADGE_OFF_BG: Rgb = Rgb(236, 240, 249); // #ECF0F9 "Off" Badge Fill

// Status & Semantic Colors
pub const LIVE_GREEN: Rgb = Rgb(28, 184, 84); // #1CB854 "Ready" Green Status Dot & Label
pub const LIVE_GREEN_SOFT: Rgb = Rgb(232, 249, 238); // #E8F9EE Soft Green Badge Fill
pub const WARNING_AMBER: Rgb = Rgb(230, 126, 12); // #E67E0C Warning / Auth Needed
pub const WARNING_AMBER_SOFT: Rgb = Rgb(254, 243, 226); // #FEF3E2 Soft Warning Fill
pub const ERROR_RED: Rgb = Rgb(232, 48, 56); // #E83038 Recording Red Dot & Active Timer
pub const ERROR_BG: Rgb = Rgb(253, 232, 236); // #FDE8EC "Start Recording" Soft Rose Pill Fill
pub const ERROR_BG_HOVER: Rgb = Rgb(250, 215, 222); // #FAD7DE "Start Recording" Hover Fill

// Typography Ink Hierarchy (WCAG AAA/AA Compliant — High-Contrast Readable Slate)
pub const TEXT_PRIMARY: Rgb = Rgb(15, 23, 42); // #1E293B Primary Display & Heading Ink
pub const TEXT_BODY: Rgb = Rgb(30, 41, 59); // #1E293B Navigation & Control Label Ink
pub const TEXT_SECONDARY: Rgb = Rgb(64, 78, 102); // #404E66 Subtitles, Captions & Muted Meta

/// Typographic weight matching Windows Segoe UI / Apple SF font families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontWeight {
    Regular,
    Semibold,
    Bold,
}

/// Smooth 60 FPS animation state for dashboard micro-interactions and status indicators.
pub struct UiAnimationState {
    pub pulse_phase: f32,
    pub glide_phase: f32,
    pub hover_mic: f32,
    pub hover_record: f32,
    pub hover_start_mirror: f32,
    pub hover_rec_btn: f32,
    last_tick: std::time::Instant,
}

impl Default for UiAnimationState {
    fn default() -> Self {
        Self {
            pulse_phase: 0.0,
            glide_phase: 0.0,
            hover_mic: 0.0,
            hover_record: 0.0,
            hover_start_mirror: 0.0,
            hover_rec_btn: 0.0,
            last_tick: std::time::Instant::now(),
        }
    }
}

impl UiAnimationState {
    pub fn step_startup(&mut self) {
        let now = std::time::Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_tick = now;

        self.pulse_phase = (self.pulse_phase + dt * 2.4) % std::f32::consts::TAU;
        self.glide_phase = (self.glide_phase + dt * 3.2) % std::f32::consts::TAU;
    }

    pub fn step_toolbar(&mut self, is_hover_mic: bool, is_hover_record: bool) {
        let now = std::time::Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_tick = now;

        self.pulse_phase = (self.pulse_phase + dt * 2.4) % std::f32::consts::TAU;
        self.glide_phase = (self.glide_phase + dt * 3.2) % std::f32::consts::TAU;

        let hover_speed = 1.0 - (-18.0 * dt).exp();
        let target_mic = if is_hover_mic { 1.0 } else { 0.0 };
        let target_rec = if is_hover_record { 1.0 } else { 0.0 };

        self.hover_mic += (target_mic - self.hover_mic) * hover_speed;
        if (self.hover_mic - target_mic).abs() < 0.005 {
            self.hover_mic = target_mic;
        }

        self.hover_record += (target_rec - self.hover_record) * hover_speed;
        if (self.hover_record - target_rec).abs() < 0.005 {
            self.hover_record = target_rec;
        }
    }
}

/// Native HiDPI anti-aliased 2D SDF + TrueType UI surface renderer (`0xAARRGGBB` physical pixel buffer).
///
/// All drawing and measurement methods accept **logical coordinates** and automatically rasterize
/// at native physical pixel resolution (`width = round(logical_width * scale)`, `height = round(logical_height * scale)`).
pub struct UiCanvas {
    pub width: usize,
    pub height: usize,
    pub logical_width: f32,
    pub logical_height: f32,
    pub scale: f32,
    pub pixels: Vec<u32>,
    font_regular: Option<FontArc>,
    font_semibold: Option<FontArc>,
    font_bold: Option<FontArc>,
    pub anim: UiAnimationState,
}

impl UiCanvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self::new_scaled(width as f32, height as f32, 1.0)
    }

    pub fn new_scaled(logical_width: f32, logical_height: f32, scale: f32) -> Self {
        let s = scale.clamp(1.0, 4.0);
        let phys_w = (logical_width.max(1.0) * s).round().max(1.0) as usize;
        let phys_h = (logical_height.max(1.0) * s).round().max(1.0) as usize;

        let font_regular = load_system_font(&[
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\arial.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]);
        let font_semibold = load_system_font(&[
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ])
        .or_else(|| font_regular.clone());
        let font_bold = load_system_font(&[
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ])
        .or_else(|| font_semibold.clone());

        Self {
            width: phys_w,
            height: phys_h,
            logical_width: logical_width.max(1.0),
            logical_height: logical_height.max(1.0),
            scale: s,
            pixels: vec![0xFF_F3F6FD; phys_w * phys_h],
            font_regular,
            font_semibold,
            font_bold,
            anim: UiAnimationState::default(),
        }
    }

    /// Configures logical dimensions and HiDPI scale factor, resizing the backing physical pixel buffer if needed.
    pub fn set_logical_size(&mut self, logical_width: f32, logical_height: f32, scale: f32) {
        let lw = logical_width.max(1.0);
        let lh = logical_height.max(1.0);
        let s = scale.clamp(1.0, 4.0);
        let phys_w = (lw * s).round().max(1.0) as usize;
        let phys_h = (lh * s).round().max(1.0) as usize;

        self.logical_width = lw;
        self.logical_height = lh;
        self.scale = s;
        if self.width != phys_w || self.height != phys_h {
            self.width = phys_w;
            self.height = phys_h;
            self.pixels.resize(phys_w * phys_h, 0xFF_F3F6FD);
        }
    }

    pub fn resize_if_needed(&mut self, width: usize, height: usize) {
