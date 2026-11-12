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
        self.set_logical_size(width as f32, height as f32, 1.0);
    }

    pub fn clear(&mut self, color: Rgb) {
        let packed =
            0xFF00_0000 | ((color.0 as u32) << 16) | ((color.1 as u32) << 8) | (color.2 as u32);
        self.pixels.fill(packed);
    }

    /// Fills a physical-pixel-aligned rectangle directly (for crisp HiDPI hairlines and strip fills).
    pub fn fill_physical_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
        if w <= 0 || h <= 0 {
            return;
        }
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = (x + w).min(self.width as i32).max(0) as usize;
        let y1 = (y + h).min(self.height as i32).max(0) as usize;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let packed =
            0xFF00_0000 | ((color.0 as u32) << 16) | ((color.1 as u32) << 8) | (color.2 as u32);
        for py in y0..y1 {
            let row = py * self.width;
            self.pixels[row + x0..row + x1].fill(packed);
        }
    }

    /// Fills a rectangle specified in logical coordinates, snapped cleanly to physical pixel bounds.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
        let s = self.scale;
        let px0 = (x as f32 * s).round() as i32;
        let py0 = (y as f32 * s).round() as i32;
        let px1 = ((x + w) as f32 * s).round() as i32;
        let py1 = ((y + h) as f32 * s).round() as i32;
        self.fill_physical_rect(px0, py0, px1 - px0, py1 - py0, color);
    }

    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Rgb, alpha: f32) {
        if x < 0 || y < 0 || (x as usize) >= self.width || (y as usize) >= self.height {
            return;
        }
        let a = alpha.clamp(0.0, 1.0);
        if a <= 0.002 {
            return;
        }
        let idx = (y as usize) * self.width + (x as usize);
        if a >= 0.998 {
            self.pixels[idx] = 0xFF00_0000
                | ((color.0 as u32) << 16)
                | ((color.1 as u32) << 8)
                | (color.2 as u32);
            return;
        }
        let dst = self.pixels[idx];
        let dr = ((dst >> 16) & 0xFF) as f32;
        let dg = ((dst >> 8) & 0xFF) as f32;
        let db = (dst & 0xFF) as f32;

        let inv = 1.0 - a;
        let r = (color.0 as f32 * a + dr * inv).round() as u32;
        let g = (color.1 as f32 * a + dg * inv).round() as u32;
        let b = (color.2 as f32 * a + db * inv).round() as u32;
        self.pixels[idx] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
    }

    /// Renders an anti-aliased rounded rectangle in logical coordinates using the exact 2D `sdRoundBox` SDF,
    /// rasterized at native physical HiDPI resolution with analytical Gaussian drop shadow and hairline border.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rounded_card(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: Rgb,
        border_color: Rgb,
        border_width: f32,
        shadow_color: Rgb,
        shadow_alpha: f32,
        shadow_blur: f32,
        shadow_offset_y: f32,
        opacity: f32,
    ) {
        self.draw_gradient_rounded_card(
            x,
            y,
            w,
            h,
            radius,
            fill,
            fill,
            border_color,
            border_width,
            shadow_color,
            shadow_alpha,
            shadow_blur,
            shadow_offset_y,
            opacity,
        );
    }

    /// Renders an anti-aliased vertical-gradient rounded rectangle (`fill_top` -> `fill_bottom`)
    /// with analytical Gaussian drop shadow and hairline border.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_gradient_rounded_card(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill_top: Rgb,
        fill_bottom: Rgb,
        border_color: Rgb,
        border_width: f32,
        shadow_color: Rgb,
        shadow_alpha: f32,
        shadow_blur: f32,
        shadow_offset_y: f32,
        opacity: f32,
    ) {
        if opacity <= 0.01 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let s = self.scale;
        let x = x * s;
        let y = y * s;
        let w = w * s;
        let h = h * s;
        let radius = radius * s;
        let border_width = border_width * s;
        let shadow_blur = shadow_blur * s;
        let shadow_offset_y = shadow_offset_y * s;

        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let half_w = w * 0.5;
        let half_h = h * 0.5;
        let r = radius.min(half_w).min(half_h).max(0.0);

        let pad = (shadow_blur * 1.6 + shadow_offset_y.abs() + border_width + 6.0).ceil() as i32;
        let min_x = ((x.floor() as i32) - pad).max(0);
        let max_x = (((x + w).ceil() as i32) + pad).min(self.width as i32 - 1);
        let min_y = ((y.floor() as i32) - pad).max(0);
        let max_y = (((y + h).ceil() as i32) + pad).min(self.height as i32 - 1);

        let inner_w = (half_w - r).max(0.0);
        let inner_h = (half_h - r).max(0.0);
        let max_shadow_dist = shadow_blur * 1.5;
        let sigma = (shadow_blur * 0.45).max(0.5);
        let inv_h = 1.0 / h.max(1.0);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            let grad_t = ((fy - y) * inv_h).clamp(0.0, 1.0);
            let fill = if fill_top == fill_bottom {
                fill_top
            } else {
                fill_top.lerp(fill_bottom, grad_t)
            };

            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;

                let qx = (fx - cx).abs() - inner_w;
                let qy = (fy - cy).abs() - inner_h;
                let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;

                // 1. Soft Gaussian drop shadow
                if shadow_alpha > 0.002 && shadow_blur > 0.5 && !(opacity >= 0.998 && dist < -1.0)
                {
                    let sqx = (fx - cx).abs() - inner_w;
                    let sqy = (fy - (cy + shadow_offset_y)).abs() - inner_h;
                    let sd = sqx.max(0.0).hypot(sqy.max(0.0)) + sqx.max(sqy).min(0.0) - r;
                    if sd < max_shadow_dist {
                        let u = sd / sigma;
                        let cdf = 1.0 / (1.0 + (1.702 * u).exp());
                        let window = if sd > 0.0 {
                            let t = (sd / max_shadow_dist).clamp(0.0, 1.0);
                            (1.0 - t) * (1.0 - t)
                        } else {
                            1.0
                        };
                        let falloff = cdf * window;
                        self.blend_pixel(px, py, shadow_color, shadow_alpha * falloff * opacity);
                    }
                }

                // 2. Paint card/pill body & anti-aliased border
                if dist < 0.75 {
                    let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
                    if border_width > 0.05 {
                        let inner_dist = dist + border_width;
                        let border_mix = (0.5 + inner_dist).clamp(0.0, 1.0);
                        let col = fill.lerp(border_color, border_mix);
                        self.blend_pixel(px, py, col, outer_cov * opacity);
                    } else {
                        self.blend_pixel(px, py, fill, outer_cov * opacity);
                    }
                }
            }
        }
    }

    /// Renders a hollow anti-aliased rounded rectangle outline in logical coordinates (centered on stroke path).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rounded_stroke(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
