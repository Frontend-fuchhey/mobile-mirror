use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use crate::thumbnail::VideoThumbnail;
use std::fs;
use std::io;
use std::path::Path;

/// Pre-filtered HighQualityBicubic BGRA/ARGB mipmap pyramid (`256`, `128`, `64`, `48`, `32`)
/// of the official "Glossy Laptop Mirroring App Icon".
const APP_ICON_MIPS: &[u8] = include_bytes!("../assets/app_icon_mips.bin");

/// Returns the `256x256` `0xAARRGGBB` pixel buffer of the official app icon for `SDL_SetWindowIcon`.
pub fn app_icon_argb_256() -> Vec<u32> {
    let mut out = Vec::with_capacity(256 * 256);
    for chunk in APP_ICON_MIPS[..256 * 256 * 4].chunks_exact(4) {
        out.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    out
}

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
// Design System Palette (Clean Windows 11-Inspired White & Subtle Light-Blue)
// ============================================================================
pub const BG_ALABASTER: Rgb = Rgb(255, 255, 255); // #FFFFFF Clean White Main Workspace Background
pub const BG_HEADER: Rgb = Rgb(248, 250, 253); // #F8FAFD Subtle Light-Blue Top Title Bar
pub const BG_SIDEBAR: Rgb = Rgb(244, 247, 252); // #F4F7FC Subtle Light-Blue Left Navigation Sidebar
pub const CARD_WHITE: Rgb = Rgb(255, 255, 255); // #FFFFFF Pure Surface
pub const BORDER_SUBTLE: Rgb = Rgb(228, 234, 243); // #E4EAF3 Crisp 1px Cool Hairline
pub const CARD_BORDER: Rgb = Rgb(226, 232, 242); // #E2E8F2 Subtle Surface Border
pub const SHADOW_SLATE: Rgb = Rgb(15, 23, 42); // #0F172A Slate Shadow Base

// Brand & Accent Blues (#246BFD Primary Accent)
pub const APPLE_BLUE: Rgb = Rgb(36, 107, 253); // #246BFD Primary Accent Blue
pub const BLUE_GRAD_TOP: Rgb = Rgb(36, 107, 253); // #246BFD Flat Primary Blue
pub const BLUE_GRAD_BOT: Rgb = Rgb(36, 107, 253); // #246BFD Flat Primary Blue
pub const BLUE_GRAD_HOVER_TOP: Rgb = Rgb(27, 94, 232); // #1B5EE8 Hover Primary Blue
pub const BLUE_GRAD_HOVER_BOT: Rgb = Rgb(27, 94, 232); // #1B5EE8 Hover Primary Blue
pub const APPLE_BLUE_SOFT: Rgb = Rgb(235, 242, 255); // #EBF2FF Subtle Light-Blue Active State
pub const APPLE_BLUE_HOVER: Rgb = Rgb(222, 234, 255); // #DEEAFF Subtle Light-Blue Hover

// Neutral Tinted Controls & Surfaces
pub const PILL_NEUTRAL: Rgb = Rgb(242, 245, 250); // #F2F5FA Subtle Neutral Control Surface
pub const PILL_NEUTRAL_HOVER: Rgb = Rgb(230, 236, 246); // #E6ECF6 Subtle Neutral Hover
pub const BANNER_USB_BG: Rgb = Rgb(245, 248, 254); // #F5F8FE Subtle Light-Blue Info Strip
pub const BANNER_USB_BORDER: Rgb = Rgb(224, 232, 246); // #E0E8F6 Info Strip Border
pub const BADGE_OFF_BG: Rgb = Rgb(237, 241, 248); // #EDF1F8 "Off" Badge Fill

// Status & Semantic Colors (Red reserved strictly for Recording controls/indicators)
pub const LIVE_GREEN: Rgb = Rgb(22, 163, 74); // #16A34A Connected / Ready Indicator
pub const LIVE_GREEN_SOFT: Rgb = Rgb(232, 249, 238); // #E8F9EE Soft Green Fill
pub const WARNING_AMBER: Rgb = Rgb(217, 119, 6); // #D97706 Warning / Auth Needed
pub const WARNING_AMBER_SOFT: Rgb = Rgb(254, 243, 226); // #FEF3E2 Soft Warning Fill
pub const ERROR_RED: Rgb = Rgb(220, 38, 38); // #DC2626 Recording Indicator & Active Timer
pub const ERROR_BG: Rgb = Rgb(254, 236, 238); // #FEECEE Recording Button Fill
pub const ERROR_BG_HOVER: Rgb = Rgb(252, 218, 222); // #FCDADE Recording Button Hover

// Typography Ink Hierarchy (Dark Navy Primary + Muted Grey Secondary)
pub const TEXT_PRIMARY: Rgb = Rgb(15, 23, 42); // #0F172A Dark Navy Heading & Primary Ink
pub const TEXT_BODY: Rgb = Rgb(30, 41, 59); // #1E293B Dark Navy Control Label Ink
pub const TEXT_SECONDARY: Rgb = Rgb(100, 116, 139); // #64748B Muted Grey Secondary Text

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
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        ]);
        let font_semibold = load_system_font(&[
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-B.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        ])
        .or_else(|| font_regular.clone());
        let font_bold = load_system_font(&[
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-B.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
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
        stroke_w: f32,
        color: Rgb,
        opacity: f32,
    ) {
        if opacity <= 0.01 || w <= 0.0 || h <= 0.0 || stroke_w <= 0.05 {
            return;
        }
        let s = self.scale;
        let x = x * s;
        let y = y * s;
        let w = w * s;
        let h = h * s;
        let radius = radius * s;
        let stroke_w = stroke_w * s;

        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let half_w = w * 0.5;
        let half_h = h * 0.5;
        let r = radius.min(half_w).min(half_h).max(0.0);
        let half_stroke = stroke_w * 0.5;

        let pad = (half_stroke + 3.0).ceil() as i32;
        let min_x = ((x.floor() as i32) - pad).max(0);
        let max_x = (((x + w).ceil() as i32) + pad).min(self.width as i32 - 1);
        let min_y = ((y.floor() as i32) - pad).max(0);
        let max_y = (((y + h).ceil() as i32) + pad).min(self.height as i32 - 1);

        let inner_w = (half_w - r).max(0.0);
        let inner_h = (half_h - r).max(0.0);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let qx = (fx - cx).abs() - inner_w;
                let qy = (fy - cy).abs() - inner_h;
                let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                let stroke_dist = dist.abs() - half_stroke;
                if stroke_dist < 0.75 {
                    let cov = (0.5 - stroke_dist).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
    }

    pub fn draw_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgb, opacity: f32) {
        if opacity <= 0.01 || radius <= 0.0 {
            return;
        }
        let s = self.scale;
        let cx = cx * s;
        let cy = cy * s;
        let radius = radius * s;

        let min_x = ((cx - radius - 2.0).floor() as i32).max(0);
        let max_x = ((cx + radius + 2.0).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((cy - radius - 2.0).floor() as i32).max(0);
        let max_y = ((cy + radius + 2.0).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let dy = (py as f32 + 0.5) - cy;
            for px in min_x..=max_x {
                let dx = (px as f32 + 0.5) - cx;
                let dist = (dx * dx + dy * dy).sqrt() - radius;
                if dist < 0.75 {
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
    }

    /// Draws an anti-aliased circular ring / arc stroke with rounded caps.
    /// Angles `start_rad` and `end_rad` are in radians (`0.0` = right/+X, `FRAC_PI_2` = down/+Y).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_arc_stroke(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        start_rad: f32,
        end_rad: f32,
        stroke_w: f32,
        color: Rgb,
        opacity: f32,
    ) {
        if opacity <= 0.01 || radius <= 0.0 || stroke_w <= 0.05 {
            return;
        }
        let s = self.scale;
        let cx = cx * s;
        let cy = cy * s;
        let r = radius * s;
        let half_w = stroke_w * s * 0.5;

        let mid_ang = 0.5 * (start_rad + end_rad);
        let half_span = 0.5 * (end_rad - start_rad).abs();
        let dir_x = mid_ang.cos();
        let dir_y = mid_ang.sin();
        let cos_span = half_span.cos();

        let p1x = cx + r * start_rad.cos();
        let p1y = cy + r * start_rad.sin();
        let p2x = cx + r * end_rad.cos();
        let p2y = cy + r * end_rad.sin();

        let pad = r + half_w + 3.0;
        let min_x = ((cx - pad).floor() as i32).max(0);
        let max_x = ((cx + pad).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((cy - pad).floor() as i32).max(0);
        let max_y = ((cy + pad).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            let dy = fy - cy;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let dx = fx - cx;
                let len = (dx * dx + dy * dy).sqrt();
                let in_sector = if len > 0.001 {
                    (dx * dir_x + dy * dir_y) / len >= cos_span
                } else {
                    false
                };
                let dist = if in_sector {
                    (len - r).abs() - half_w
                } else {
                    let d1 = (fx - p1x).hypot(fy - p1y);
                    let d2 = (fx - p2x).hypot(fy - p2y);
                    d1.min(d2) - half_w
                };
                if dist < 0.75 {
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
    }

    /// Draws a soft diffused elliptical glow / floor shadow centered at `(cx, cy)` that fades smoothly to 0 at `(rx, ry)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_soft_ellipse(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        _feather: f32,
        color: Rgb,
        opacity: f32,
    ) {
        if opacity <= 0.002 || rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let s = self.scale;
        let cx = cx * s;
        let cy = cy * s;
        let total_rx = rx * s;
        let total_ry = ry * s;

        let min_x = ((cx - total_rx - 1.0).floor() as i32).max(0);
        let max_x = ((cx + total_rx + 1.0).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((cy - total_ry - 1.0).floor() as i32).max(0);
        let max_y = ((cy + total_ry + 1.0).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let dy = ((py as f32 + 0.5) - cy) / total_ry;
            let dy2 = dy * dy;
            if dy2 >= 1.0 {
                continue;
            }
            for px in min_x..=max_x {
                let dx = ((px as f32 + 0.5) - cx) / total_rx;
                let r2 = dx * dx + dy2;
                if r2 < 1.0 {
                    let t = 1.0 - r2.sqrt();
                    let smooth = t * t * (3.0 - 2.0 * t);
                    self.blend_pixel(px, py, color, smooth * opacity);
                }
            }
        }
    }

    /// Draws an anti-aliased capsule/line segment in logical coordinates between `(x1, y1)` and `(x2, y2)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_capsule_line(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke_w: f32,
        color: Rgb,
        opacity: f32,
    ) {
        if opacity <= 0.01 || stroke_w <= 0.05 {
            return;
        }
        let s = self.scale;
        let x1 = x1 * s;
        let y1 = y1 * s;
        let x2 = x2 * s;
        let y2 = y2 * s;
        let r = stroke_w * s * 0.5;

        let min_x = ((x1.min(x2) - r - 2.0).floor() as i32).max(0);
        let max_x = ((x1.max(x2) + r + 2.0).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((y1.min(y2) - r - 2.0).floor() as i32).max(0);
        let max_y = ((y1.max(y2) + r + 2.0).ceil() as i32).min(self.height as i32 - 1);

        let vx = x2 - x1;
        let vy = y2 - y1;
        let len_sq = (vx * vx + vy * vy).max(0.0001);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let t = (((fx - x1) * vx + (fy - y1) * vy) / len_sq).clamp(0.0, 1.0);
                let proj_x = x1 + t * vx;
                let proj_y = y1 + t * vy;
                let dist = (fx - proj_x).hypot(fy - proj_y) - r;
                if dist < 0.75 {
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
    }

    /// Draws an anti-aliased rounded play triangle `▶` centered at `(cx, cy)`.
    pub fn draw_play_icon(&mut self, cx: f32, cy: f32, size: f32, color: Rgb, opacity: f32) {
        if opacity <= 0.01 {
            return;
        }
        let s = self.scale;
        let cx = cx * s;
        let cy = cy * s;
        let r_size = size * s;
        let round_r = 1.3 * s;

        // Triangle vertices (optically nudged +1px right for play icon balance)
        let ax = cx - r_size * 0.42;
        let ay = cy - r_size * 0.56;
        let bx = cx - r_size * 0.42;
        let by = cy + r_size * 0.56;
        let tx = cx + r_size * 0.58;
        let ty = cy;

        let min_x = ((ax.min(tx) - round_r - 2.0).floor() as i32).max(0);
        let max_x = ((ax.max(tx) + round_r + 2.0).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((ay.min(by) - round_r - 2.0).floor() as i32).max(0);
        let max_y = ((ay.max(by) + round_r + 2.0).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let d = sd_triangle(fx, fy, ax, ay, bx, by, tx, ty) - round_r;
                if d < 0.75 {
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
    }

    // ========================================================================
    // Custom Vector Icon Suite Matching Reference Screenshot
    // ========================================================================

    /// Renders the official "Glossy Laptop Mirroring App Icon" at `(x, y)` with width/height `size`
    /// using mipmap-selected premultiplied-alpha bilinear filtering.
    pub fn draw_app_brand_icon(&mut self, x: f32, y: f32, size: f32) {
        let s = self.scale;
        let px_x = x * s;
        let px_y = y * s;
        let px_size = (size * s).max(1.0);

        // Select optimal pre-filtered mipmap level:
        // Offsets in APP_ICON_MIPS:
        // 256x256: 0       (len 262144)
        // 128x128: 262144  (len 65536)
        //  64x64:  327680  (len 16384)
        //  48x48:  344064  (len 9216)
        //  32x32:  353280  (len 4096)
        let (mip_dim, mip_offset): (usize, usize) = if px_size <= 34.0 {
            (32, 353_280)
        } else if px_size <= 52.0 {
            (48, 344_064)
        } else if px_size <= 72.0 {
            (64, 327_680)
        } else if px_size <= 144.0 {
            (128, 262_144)
        } else {
            (256, 0)
        };

        let mip_bytes = &APP_ICON_MIPS[mip_offset..mip_offset + mip_dim * mip_dim * 4];
        let x0 = px_x.floor().max(0.0) as usize;
        let y0 = px_y.floor().max(0.0) as usize;
        let x1 = (px_x + px_size).ceil().min(self.width as f32) as usize;
        let y1 = (px_y + px_size).ceil().min(self.height as f32) as usize;

        let sample_premul = |tx: isize, ty: isize| -> (f32, f32, f32, f32) {
            if tx < 0 || ty < 0 || tx >= mip_dim as isize || ty >= mip_dim as isize {
                return (0.0, 0.0, 0.0, 0.0);
            }
            let idx = (ty as usize * mip_dim + tx as usize) * 4;
            let b = mip_bytes[idx] as f32;
            let g = mip_bytes[idx + 1] as f32;
            let r = mip_bytes[idx + 2] as f32;
            let a = (mip_bytes[idx + 3] as f32) * (1.0 / 255.0);
            (r * a, g * a, b * a, a)
        };

        let dim_f = mip_dim as f32;
        for iy in y0..y1 {
            let v = ((iy as f32 + 0.5 - px_y) / px_size) * dim_f - 0.5;
            let ty0 = v.floor() as isize;
            let fy = v - (ty0 as f32);
            let inv_fy = 1.0 - fy;
            let row_off = iy * self.width;

            for ix in x0..x1 {
                let u = ((ix as f32 + 0.5 - px_x) / px_size) * dim_f - 0.5;
                let tx0 = u.floor() as isize;
                let fx = u - (tx0 as f32);
                let inv_fx = 1.0 - fx;

                let w00 = inv_fx * inv_fy;
                let w10 = fx * inv_fy;
                let w01 = inv_fx * fy;
                let w11 = fx * fy;

                let p00 = sample_premul(tx0, ty0);
                let p10 = sample_premul(tx0 + 1, ty0);
                let p01 = sample_premul(tx0, ty0 + 1);
                let p11 = sample_premul(tx0 + 1, ty0 + 1);

                let af = (w00 * p00.3 + w10 * p10.3 + w01 * p01.3 + w11 * p11.3).clamp(0.0, 1.0);
                if af <= 0.002 {
                    continue;
                }
                let pr = w00 * p00.0 + w10 * p10.0 + w01 * p01.0 + w11 * p11.0;
                let pg = w00 * p00.1 + w10 * p10.1 + w01 * p01.1 + w11 * p11.1;
                let pb = w00 * p00.2 + w10 * p10.2 + w01 * p01.2 + w11 * p11.2;

                let dst = self.pixels[row_off + ix];
                let dr = ((dst >> 16) & 0xFF) as f32;
                let dg = ((dst >> 8) & 0xFF) as f32;
                let db = (dst & 0xFF) as f32;

                let inv_a = 1.0 - af;
                let out_r = (pr + dr * inv_a).round().clamp(0.0, 255.0) as u32;
                let out_g = (pg + dg * inv_a).round().clamp(0.0, 255.0) as u32;
                let out_b = (pb + db * inv_a).round().clamp(0.0, 255.0) as u32;
                self.pixels[row_off + ix] = 0xFF00_0000 | (out_r << 16) | (out_g << 8) | out_b;
            }
        }
    }

    /// Sidebar `Home` house icon centered at `(cx, cy)`.
    pub fn draw_home_icon(&mut self, cx: f32, cy: f32, inner_bg: Rgb, color: Rgb, opacity: f32) {
        // Roof triangle
        let s = self.scale;
        let scx = cx * s;
        let scy = cy * s;
        let ax = scx;
        let ay = (cy - 8.2) * s;
        let bx = (cx - 9.2) * s;
        let by = (cy - 0.5) * s;
        let tx = (cx + 9.2) * s;
        let ty = (cy - 0.5) * s;

        let min_x = ((bx - 2.0).floor() as i32).max(0);
        let max_x = ((tx + 2.0).ceil() as i32).min(self.width as i32 - 1);
        let min_y = ((ay - 2.0).floor() as i32).max(0);
        let max_y = ((by + 2.0).ceil() as i32).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let d = sd_triangle(fx, fy, ax, ay, bx, by, tx, ty) - 0.8 * s;
                if d < 0.75 {
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    self.blend_pixel(px, py, color, cov * opacity);
                }
            }
        }
        let _ = scx;
        let _ = scy;

        // Lower house body
        self.draw_rounded_card(
            cx - 6.6,
            cy - 1.5,
            13.2,
            9.5,
            1.8,
            color,
            color,
            0.0,
            color,
            0.0,
            0.0,
            0.0,
            opacity,
        );

        // Door cutout in inner_bg color
        self.draw_rounded_card(
            cx - 2.1,
            cy + 2.2,
            4.2,
            6.2,
            1.0,
            inner_bg,
            inner_bg,
            0.0,
            inner_bg,
            0.0,
            0.0,
            0.0,
            opacity,
        );
    }

    /// Sidebar `Recordings` video camera icon centered at `(cx, cy)`.
    pub fn draw_video_cam_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        // Camera body outline on left
        self.draw_rounded_stroke(cx - 9.0, cy - 5.8, 12.2, 11.6, 2.8, 1.8, color, opacity);
        // Right lens trapezoid/triangle
        self.draw_capsule_line(cx + 3.2, cy - 2.2, cx + 8.5, cy - 5.0, 1.8, color, opacity);
        self.draw_capsule_line(cx + 8.5, cy - 5.0, cx + 8.5, cy + 5.0, 1.8, color, opacity);
        self.draw_capsule_line(cx + 8.5, cy + 5.0, cx + 3.2, cy + 2.2, 1.8, color, opacity);
    }

    /// Precision 8-tooth `Settings` / `Mirror Quality` gear icon centered at `(cx, cy)`.
    pub fn draw_gear_icon(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        inner_bg: Rgb,
        color: Rgb,
        opacity: f32,
    ) {
        for i in 0..8 {
            let ang = (i as f32) * std::f32::consts::FRAC_PI_4;
            let x1 = cx + (radius * 0.55) * ang.cos();
            let y1 = cy + (radius * 0.55) * ang.sin();
            let x2 = cx + (radius * 0.98) * ang.cos();
            let y2 = cy + (radius * 0.98) * ang.sin();
            self.draw_capsule_line(x1, y1, x2, y2, radius * 0.34, color, opacity);
        }
        self.draw_circle(cx, cy, radius * 0.76, color, opacity);
        self.draw_circle(cx, cy, radius * 0.36, inner_bg, opacity);
    }

    /// Sidebar `About` circled-`i` icon centered at `(cx, cy)`.
    pub fn draw_info_circle_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        self.draw_arc_stroke(
            cx,
            cy,
            8.4,
            -std::f32::consts::PI,
            std::f32::consts::PI,
            1.8,
            color,
            opacity,
        );
        self.draw_circle(cx, cy - 3.6, 1.15, color, opacity);
        self.draw_capsule_line(cx, cy - 0.8, cx, cy + 4.2, 1.8, color, opacity);
    }

    /// `Quick Tools` crossed wrench & screwdriver icon centered at `(cx, cy)`.
    pub fn draw_wrench_screwdriver_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        // Diagonal 1: Wrench (bottom-left to top-right)
        self.draw_capsule_line(cx - 6.5, cy + 6.5, cx + 4.5, cy - 4.5, 2.3, color, opacity);
        self.draw_circle(cx + 5.2, cy - 5.2, 3.2, color, opacity);
        self.draw_circle(cx + 6.2, cy - 6.2, 1.5, CARD_WHITE, opacity);
        self.draw_circle(cx - 6.5, cy + 6.5, 1.8, color, opacity);

        // Diagonal 2: Screwdriver (top-left to bottom-right)
        self.draw_capsule_line(cx - 6.5, cy - 6.5, cx + 6.5, cy + 6.5, 2.0, color, opacity);
        // Screwdriver handle on bottom-right
        self.draw_capsule_line(cx + 2.5, cy + 2.5, cx + 6.8, cy + 6.8, 3.6, color, opacity);
        // Screwdriver tip on top-left
        self.draw_circle(cx - 6.8, cy - 6.8, 1.5, color, opacity);
    }

    /// `Quick Tools` folder icon centered at `(cx, cy)`.
    pub fn draw_folder_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        // Top-left folder tab
        self.draw_rounded_card(
            cx - 7.8,
            cy - 6.2,
            6.5,
            3.5,
            1.4,
            color,
            color,
            0.0,
            color,
            0.0,
            0.0,
            0.0,
            opacity,
        );
        // Folder main body outline
        self.draw_rounded_stroke(cx - 7.8, cy - 4.2, 15.6, 10.6, 2.2, 1.7, color, opacity);
    }

    /// `Quick Tools` filled dark circle with white `?` icon centered at `(cx, cy)`.
    pub fn draw_help_circle_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        self.draw_circle(cx, cy, 8.8, color, opacity);
        let q_w = self.measure_text_width("?", 11.0, FontWeight::Bold);
        let q_cap = self.cap_height(11.0, FontWeight::Bold);
        self.draw_text_baseline(
            "?",
            cx - q_w * 0.5,
            cy + q_cap * 0.5,
            11.0,
            FontWeight::Bold,
            CARD_WHITE,
            opacity,
        );
    }

    /// Right-pointing chevron `>` centered at `(cx, cy)`.
    pub fn draw_chevron_right(
        &mut self,
        cx: f32,
        cy: f32,
        size: f32,
        stroke_w: f32,
        color: Rgb,
        opacity: f32,
    ) {
        self.draw_capsule_line(
            cx - size * 0.45,
            cy - size,
            cx + size * 0.45,
            cy,
            stroke_w,
            color,
            opacity,
        );
        self.draw_capsule_line(
            cx + size * 0.45,
            cy,
            cx - size * 0.45,
            cy + size,
            stroke_w,
            color,
            opacity,
        );
    }

    /// Smartphone outline icon inside the bottom `"Connect your phone via USB"` banner.
    pub fn draw_phone_outline_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        let w = 18.0;
        let h = 30.0;
        let x = cx - w * 0.5;
        let y = cy - h * 0.5;
        self.draw_rounded_card(
            x + 1.0,
            y + 1.0,
            w - 2.0,
            h - 2.0,
            2.6,
            CARD_WHITE,
            color,
            0.0,
            color,
            0.0,
            0.0,
            0.0,
            opacity * 0.85,
        );
        self.draw_rounded_stroke(x, y, w, h, 3.4, 2.0, color, opacity);
        // Top earpiece slit
        self.draw_capsule_line(cx - 2.2, y + 3.2, cx + 2.2, y + 3.2, 1.5, color, opacity);
        // Bottom indicator bar
        self.draw_capsule_line(
            cx - 2.8,
            y + h - 3.2,
            cx + 2.8,
            y + h - 3.2,
            1.5,
            color,
            opacity,
        );
    }

    /// Clean Wi-Fi / AirPlay signal icon centered at `(cx, cy)`.
    pub fn draw_wifi_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        let dot_y = cy + 5.0;
        self.draw_circle(cx, dot_y, 1.8, color, opacity);
        self.draw_arc_stroke(cx, dot_y, 5.2, -2.35, -0.79, 1.7, color, opacity);
        self.draw_arc_stroke(cx, dot_y, 8.8, -2.35, -0.79, 1.7, color, opacity);
        self.draw_arc_stroke(cx, dot_y, 12.4, -2.35, -0.79, 1.7, color, opacity);
    }

    /// Clean desktop monitor icon centered at `(cx, cy)` for Mirror Quality.
    pub fn draw_monitor_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        let w = 16.0;
        let h = 11.5;
        let x = cx - w * 0.5;
        let y = cy - 6.8;
        self.draw_rounded_stroke(x, y, w, h, 2.2, 1.6, color, opacity);
        self.draw_capsule_line(cx, y + h, cx, y + h + 3.0, 1.6, color, opacity);
        self.draw_capsule_line(cx - 4.0, y + h + 3.2, cx + 4.0, y + h + 3.2, 1.6, color, opacity);
    }

    /// Interactive iOS/macOS style toggle switch at logical `(x, y, w, h)`.
    pub fn draw_toggle_switch(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        is_on: bool,
        hover: f32,
    ) {
        let track_col = if is_on {
            APPLE_BLUE.lerp(BLUE_GRAD_TOP, hover * 0.5)
        } else {
            Rgb(203, 213, 228).lerp(Rgb(184, 196, 216), hover)
        };
        let r = h * 0.5;
        self.draw_rounded_card(
            x, y, w, h, r, track_col, track_col, 0.0, SHADOW_SLATE, 0.0, 0.0, 0.0, 1.0,
        );
        let pad = 2.5;
        let thumb_r = (h - pad * 2.0) * 0.5;
        let thumb_cx = if is_on {
            x + w - pad - thumb_r
        } else {
            x + pad + thumb_r
        };
        let thumb_cy = y + h * 0.5;
        self.draw_rounded_card(
            thumb_cx - thumb_r,
            thumb_cy - thumb_r,
            thumb_r * 2.0,
            thumb_r * 2.0,
            thumb_r,
            CARD_WHITE,
            CARD_WHITE,
            0.0,
            SHADOW_SLATE,
            0.16,
            3.0,
            1.0,
            1.0,
        );
    }

    /// Renders a realistic phone screen with the multi-layered flowing blue wave wallpaper from the reference design.
    pub fn draw_phone_with_wave_wallpaper(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corner_r: f32,
        bezel_w: f32,
        shadow_alpha: f32,
    ) {
        // 1. Outer metallic chassis + drop shadow
        self.draw_rounded_card(
            x,
            y,
            w,
            h,
            corner_r,
            Rgb(14, 18, 30),
            Rgb(110, 118, 134),
            1.0,
            SHADOW_SLATE,
            shadow_alpha,
            14.0,
            6.0,
            1.0,
        );

        // 2. Inner OLED screen with multi-layered blue wave shader
        let sx = x + bezel_w;
        let sy = y + bezel_w;
        let sw = (w - bezel_w * 2.0).max(2.0);
        let sh = (h - bezel_w * 2.0).max(2.0);
        let sr = (corner_r - bezel_w * 0.85).max(2.0);

        let scale = self.scale;
        let psx = sx * scale;
        let psy = sy * scale;
        let psw = sw * scale;
        let psh = sh * scale;
        let psr = sr * scale;

        let cx = psx + psw * 0.5;
        let cy = psy + psh * 0.5;
        let half_w = psw * 0.5;
        let half_h = psh * 0.5;
        let inner_w = (half_w - psr).max(0.0);
        let inner_h = (half_h - psr).max(0.0);

        let min_x = (psx.floor() as i32 - 1).max(0);
        let max_x = ((psx + psw).ceil() as i32 + 1).min(self.width as i32 - 1);
        let min_y = (psy.floor() as i32 - 1).max(0);
        let max_y = ((psy + psh).ceil() as i32 + 1).min(self.height as i32 - 1);

        for py in min_y..=max_y {
            let fy = py as f32 + 0.5;
            let v = ((fy - psy) / psh).clamp(0.0, 1.0);
            for px in min_x..=max_x {
                let fx = px as f32 + 0.5;
                let qx = (fx - cx).abs() - inner_w;
                let qy = (fy - cy).abs() - inner_h;
                let dist = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - psr;
                if dist < 0.75 {
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    let u = ((fx - psx) / psw).clamp(0.0, 1.0);
                    let col = sample_blue_wave_wallpaper(u, v);
                    self.blend_pixel(px, py, col, cov);
                }
            }
        }

        // 3. Top notch / dynamic speaker cutout
        let notch_w = (w * 0.42).round();
        let notch_h = (bezel_w * 1.45).max(4.5);
        self.draw_rounded_card(
            x + (w - notch_w) * 0.5,
            y + bezel_w * 0.35,
            notch_w,
            notch_h,
            notch_h * 0.48,
            Rgb(14, 18, 30),
            Rgb(14, 18, 30),
            0.0,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
    }

    /// Renders the Hero App Logo Illustration ("Glossy Laptop Mirroring App Icon")
    /// centered horizontally around `center_x` with top anchor `top_y`.
    pub fn draw_hero_laptop_phone_illustration(
        &mut self,
        center_x: f32,
        top_y: f32,
        is_connecting: bool,
    ) {
        self.draw_hero_illustration_sized(center_x, top_y, 112.0, is_connecting);
    }

    pub fn draw_hero_illustration_sized(
        &mut self,
        center_x: f32,
        top_y: f32,
        hero_size: f32,
        is_connecting: bool,
    ) {
        let icon_x = (center_x - hero_size * 0.5).round();
        let icon_y = top_y.round();

        if is_connecting {
            let pulse = 0.55 + 0.45 * self.anim.glide_phase.sin();
            self.draw_circle(
                center_x,
                icon_y + hero_size * 0.5,
                hero_size * 0.62,
                Rgb(36, 107, 253),
                0.10 * pulse,
            );
        }

        self.draw_app_brand_icon(icon_x, icon_y, hero_size);
    }

    /// Legacy startup icon helper kept for unit tests.
    pub fn draw_smartphone_usb_icon(&mut self, cx: f32, cy: f32, color: Rgb, opacity: f32) {
        self.draw_phone_outline_icon(cx, cy, color, opacity);
    }

    // ========================================================================
    // TrueType Typography Engine
    // ========================================================================

    fn select_font(&self, weight: FontWeight) -> Option<&FontArc> {
        match weight {
            FontWeight::Regular => self.font_regular.as_ref(),
            FontWeight::Semibold => self.font_semibold.as_ref().or(self.font_regular.as_ref()),
            FontWeight::Bold => self
                .font_bold
                .as_ref()
                .or(self.font_semibold.as_ref())
                .or(self.font_regular.as_ref()),
        }
    }

    /// Converts a logical CSS/UI font size (`px_size`, where 1em = `px_size`) into `ab_glyph`'s
    /// total line-height `PxScale` (`ascent - descent`), preventing Segoe UI's 1.33x internal
    /// leading from shrinking rendered glyphs by 25%.
    #[inline]
    fn font_px_scale(font: &FontArc, px_size: f32, dpi_scale: f32) -> PxScale {
        let em_to_line_ratio = font
            .units_per_em()
            .map(|upm| (font.height_unscaled() / upm).clamp(1.0, 1.45))
            .unwrap_or(1.33);
        PxScale::from(px_size * dpi_scale * em_to_line_ratio)
    }

    /// Returns exact optical cap-height (height of uppercase `'H'`) in logical units for vertical pill centering.
    pub fn cap_height(&self, px_size: f32, weight: FontWeight) -> f32 {
        let Some(font) = self.select_font(weight) else {
            return px_size * 0.70;
        };
        let s = self.scale;
        let scale = Self::font_px_scale(font, px_size, s);
        let scaled = font.as_scaled(scale);
        let h_id = scaled.glyph_id('H');
        let glyph = h_id.with_scale_and_position(scale, point(0.0, 0.0));
        let phys_cap = scaled
            .outline_glyph(glyph)
            .map(|og| {
                let b = og.px_bounds();
                (b.max.y - b.min.y).max(1.0)
            })
            .unwrap_or_else(|| scaled.ascent() * 0.72);
        phys_cap / s
    }

    /// Renders anti-aliased TrueType text positioned at logical `(x, baseline_y)` with native HiDPI glyph rasterization
    /// and gamma-compensated optical stem darkening so small/medium body text is crisp and readable.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_baseline(
        &mut self,
        text: &str,
        x: f32,
        baseline_y: f32,
        px_size: f32,
        weight: FontWeight,
        color: Rgb,
        opacity: f32,
    ) {
        if opacity <= 0.01 || text.is_empty() {
            return;
        }
        let Some(font) = self.select_font(weight).cloned() else {
            return;
        };

        let s = self.scale;
        let scale = Self::font_px_scale(&font, px_size, s);
        let scaled_font = font.as_scaled(scale);
        let mut caret_x = x * s;
        let phys_baseline_y = baseline_y * s;
        let mut prev_glyph_id = None;

        for ch in text.chars() {
            let glyph_id = scaled_font.glyph_id(ch);
            if let Some(prev) = prev_glyph_id {
                caret_x += scaled_font.kern(prev, glyph_id);
            }
            prev_glyph_id = Some(glyph_id);

            let glyph =
                glyph_id.with_scale_and_position(scale, point(caret_x, phys_baseline_y));
            caret_x += scaled_font.h_advance(glyph_id);

            if let Some(outlined) = scaled_font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                let min_x = bounds.min.x.floor() as i32;
                let min_y = bounds.min.y.floor() as i32;
                outlined.draw(|gx, gy, cov| {
                    let px = min_x + gx as i32;
                    let py = min_y + gy as i32;
                    // Optical stem darkening / sRGB gamma compensation for crisp legibility
                    let boosted_cov = cov.clamp(0.0, 1.0).powf(0.74);
                    self.blend_pixel(px, py, color, boosted_cov * opacity);
                });
            }
        }
    }

    /// Renders anti-aliased TrueType text given logical top-of-text `y`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        px_size: f32,
        weight: FontWeight,
        color: Rgb,
        opacity: f32,
    ) {
        let cap = self.cap_height(px_size, weight);
        let baseline_offset = cap + px_size * 0.12;
        self.draw_text_baseline(
            text,
            x,
            y + baseline_offset,
            px_size,
            weight,
            color,
            opacity,
        );
    }

    /// Measures text advance width in logical coordinates.
    pub fn measure_text_width(&self, text: &str, px_size: f32, weight: FontWeight) -> f32 {
        let Some(font) = self.select_font(weight) else {
            return (text.chars().count() as f32) * px_size * 0.5;
        };

        let s = self.scale;
        let scale = Self::font_px_scale(font, px_size, s);
        let scaled_font = font.as_scaled(scale);
        let mut width = 0.0f32;
        let mut prev_id = None;
        for ch in text.chars() {
            let id = scaled_font.glyph_id(ch);
            if let Some(prev) = prev_id {
                width += scaled_font.kern(prev, id);
            }
            prev_id = Some(id);
            width += scaled_font.h_advance(id);
        }
        width / s
    }

    /// Truncates `text` cleanly with `"…"` if its logical width exceeds `max_width`.
    pub fn fit_text_to_width(
        &self,
        text: &str,
        max_width: f32,
        px_size: f32,
        weight: FontWeight,
    ) -> String {
        if self.measure_text_width(text, px_size, weight) <= max_width {
            return text.to_string();
        }
        let ellipsis = "…";
        let ell_w = self.measure_text_width(ellipsis, px_size, weight);
        let mut out = String::new();
        for ch in text.chars() {
            out.push(ch);
            if self.measure_text_width(&out, px_size, weight) + ell_w > max_width {
                out.pop();
                break;
            }
        }
        format!("{}{}", out.trim_end(), ellipsis)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_centered(
        &mut self,
        text: &str,
        center_x: f32,
        y: f32,
        px_size: f32,
        weight: FontWeight,
        color: Rgb,
        opacity: f32,
    ) {
        let w = self.measure_text_width(text, px_size, weight);
        let x = center_x - w * 0.5;
        self.draw_text(text, x, y, px_size, weight, color, opacity);
    }

    /// Scales and composites an ARGB pixel buffer with rounded-corner anti-aliased clipping.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image_rgba(
        &mut self,
        src_pixels: &[u32],
        src_w: usize,
        src_h: usize,
        dst_x: f32,
        dst_y: f32,
        dst_w: f32,
        dst_h: f32,
        corner_r: f32,
        opacity: f32,
    ) {
        if src_w == 0
            || src_h == 0
            || src_pixels.len() < src_w * src_h
            || dst_w <= 0.0
            || dst_h <= 0.0
        {
            return;
        }
        let s = self.scale;
        let phys_x0 = (dst_x * s).round() as i32;
        let phys_y0 = (dst_y * s).round() as i32;
        let phys_x1 = ((dst_x + dst_w) * s).round() as i32;
        let phys_y1 = ((dst_y + dst_h) * s).round() as i32;

        let min_x = phys_x0.max(0).min(self.width as i32) as usize;
        let max_x = phys_x1.max(0).min(self.width as i32) as usize;
        let min_y = phys_y0.max(0).min(self.height as i32) as usize;
        let max_y = phys_y1.max(0).min(self.height as i32) as usize;

        if min_x >= max_x || min_y >= max_y {
            return;
        }

        let pw = (phys_x1 - phys_x0).max(1) as f32;
        let ph = (phys_y1 - phys_y0).max(1) as f32;
        let cr = (corner_r * s).max(0.0);

        for py in min_y..max_y {
            let v = ((py as f32 - phys_y0 as f32) / ph).clamp(0.0, 1.0);
            let sy = ((v * (src_h - 1) as f32).round() as usize).min(src_h - 1);
            let row_offset = py * self.width;
            let src_row_offset = sy * src_w;

            for px in min_x..max_x {
                let mut cov = 1.0f32;
                if cr > 0.0 {
                    let dx = (px as f32 - phys_x0 as f32).min((phys_x1 - 1) as f32 - px as f32);
                    let dy = (py as f32 - phys_y0 as f32).min((phys_y1 - 1) as f32 - py as f32);
                    if dx < cr && dy < cr {
                        let d = ((cr - dx) * (cr - dx) + (cr - dy) * (cr - dy)).sqrt();
                        if d > cr + 0.5 {
                            continue;
                        } else if d > cr - 0.5 {
                            cov = (cr + 0.5 - d).clamp(0.0, 1.0);
                        }
                    }
                }

                let u = ((px as f32 - phys_x0 as f32) / pw).clamp(0.0, 1.0);
                let sx = ((u * (src_w - 1) as f32).round() as usize).min(src_w - 1);
                let src_pixel = src_pixels[src_row_offset + sx];

                let alpha = (((src_pixel >> 24) & 0xFF) as f32 / 255.0) * opacity * cov;
                if alpha >= 0.998 {
                    self.pixels[row_offset + px] = src_pixel;
                } else if alpha > 0.002 {
                    let sr = ((src_pixel >> 16) & 0xFF) as f32;
                    let sg = ((src_pixel >> 8) & 0xFF) as f32;
                    let sb = (src_pixel & 0xFF) as f32;

                    let dst_pixel = self.pixels[row_offset + px];
                    let dr = ((dst_pixel >> 16) & 0xFF) as f32;
                    let dg = ((dst_pixel >> 8) & 0xFF) as f32;
                    let db = (dst_pixel & 0xFF) as f32;

                    let out_r = (sr * alpha + dr * (1.0 - alpha)).round() as u32;
                    let out_g = (sg * alpha + dg * (1.0 - alpha)).round() as u32;
                    let out_b = (sb * alpha + db * (1.0 - alpha)).round() as u32;

                    self.pixels[row_offset + px] = 0xFF00_0000 | (out_r << 16) | (out_g << 8) | out_b;
                }
            }
        }
    }

    /// Renders the filmstrip frame preview card exactly styled after screenshot 2.
    /// Features:
    /// - Deep obsidian film frame container with rounded corners
    /// - 5 crisp white sprocket perforations on both left and right edges
    /// - Anti-aliased video snapshot or stylized fallback display
    /// - Interactive glowing Apple Blue play overlay on hover
    pub fn draw_filmstrip_frame(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        thumbnail: Option<&VideoThumbnail>,
        is_hovered: bool,
    ) {
        let film_bg = Rgb(18, 22, 28);
        let film_border = if is_hovered { APPLE_BLUE } else { Rgb(38, 46, 58) };
        self.draw_rounded_card(
            x,
            y,
            w,
            h,
            9.0,
            film_bg,
            film_border,
            if is_hovered { 1.5 } else { 1.0 },
            SHADOW_SLATE,
            if is_hovered { 0.12 } else { 0.06 },
            if is_hovered { 14.0 } else { 8.0 },
            if is_hovered { 4.0 } else { 2.5 },
            1.0,
        );

        // Film Sprocket Perforations (5 on left, 5 on right)
        let num_sprockets = 5;
        let sprocket_w = 5.5;
        let sprocket_h = 9.0;
        let sprocket_r = 1.6;
        let sprocket_col = Rgb(242, 246, 252);
        let left_x = x + 5.5;
        let right_x = x + w - 5.5 - sprocket_w;
        let pad_v = 8.0;
        let total_avail = (h - pad_v * 2.0).max(sprocket_h * num_sprockets as f32);
        let gap_v = (total_avail - (num_sprockets as f32 * sprocket_h)) / (num_sprockets as f32 - 1.0).max(1.0);

        for i in 0..num_sprockets {
            let sy = y + pad_v + i as f32 * (sprocket_h + gap_v);
            self.draw_rounded_card(
                left_x,
                sy,
                sprocket_w,
                sprocket_h,
                sprocket_r,
                sprocket_col,
                sprocket_col,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                0.92,
            );
            self.draw_rounded_card(
                right_x,
                sy,
                sprocket_w,
                sprocket_h,
                sprocket_r,
                sprocket_col,
                sprocket_col,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                0.92,
            );
        }

        // Inner Video Screen Frame
        let inner_x = x + 16.5;
        let inner_y = y + 6.0;
        let inner_w = w - 33.0;
        let inner_h = h - 12.0;
        let inner_r = 5.5;

        if let Some(thumb) = thumbnail {
            self.draw_image_rgba(
                &thumb.pixels,
                thumb.width,
                thumb.height,
                inner_x,
                inner_y,
                inner_w,
                inner_h,
                inner_r,
                1.0,
            );
        } else {
            self.draw_video_preview_fallback(inner_x, inner_y, inner_w, inner_h, inner_r);
        }

        // Inner subtle bezel border
        self.draw_rounded_stroke(inner_x, inner_y, inner_w, inner_h, inner_r, 1.0, Rgb(48, 56, 70), 0.6);

        // Center Play Button Disc Overlay
        let cx = inner_x + inner_w * 0.5;
        let cy = inner_y + inner_h * 0.5;
        let play_r = if is_hovered { 19.0 } else { 16.0 };
        let play_bg = if is_hovered { APPLE_BLUE } else { Rgb(14, 18, 24) };
        let play_alpha = if is_hovered { 0.95 } else { 0.76 };
        self.draw_circle(cx, cy, play_r, play_bg, play_alpha);
        if !is_hovered {
            self.draw_circle(cx, cy, play_r, CARD_WHITE, 0.25);
        }
        self.draw_play_icon(
            cx + 1.2,
            cy,
            if is_hovered { 9.0 } else { 7.5 },
            CARD_WHITE,
            1.0,
        );

        // Corner "MP4" badge inside video photo
        let badge_w = 34.0;
        let badge_h = 16.0;
        let badge_x = inner_x + inner_w - badge_w - 5.0;
        let badge_y = inner_y + inner_h - badge_h - 5.0;
        self.draw_rounded_card(
            badge_x,
            badge_y,
            badge_w,
            badge_h,
            3.5,
            Rgb(10, 14, 20),
            Rgb(40, 48, 60),
            0.5,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            0.75,
        );
        self.draw_text_centered(
            "MP4",
            badge_x + badge_w * 0.5,
            badge_y + 3.0,
            9.5,
            FontWeight::Bold,
            CARD_WHITE,
            0.92,
        );
    }

    /// Stylized wallpaper fallback when a video frame is not yet decoded or file is newly created.
    pub fn draw_video_preview_fallback(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        // Deep gradient backdrop
        self.draw_gradient_rounded_card(
            x,
            y,
            w,
            h,
            r,
            Rgb(15, 23, 42),
            Rgb(30, 41, 59),
            Rgb(40, 52, 72),
            0.8,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
        // Soft blue ambient wave in center
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        self.draw_soft_ellipse(cx, cy, w * 0.45, h * 0.35, 8.0, Rgb(21, 101, 240), 0.22);
        // Stylized camcorder icon
        self.draw_video_cam_icon(cx, cy - 2.0, Rgb(148, 163, 184), 0.75);
    }

    /// Writes the current physical pixel buffer (`0xAARRGGBB`) as a lossless 24-bit RGB PNG file
    /// using pure-Rust uncompressed zlib/DEFLATE stored blocks, CRC32, and Adler32.
    pub fn save_png(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let w = self.width;
        let h = self.height;
        let row_len = 1 + w * 3;
        let mut raw = Vec::with_capacity(h * row_len);
        for y in 0..h {
            raw.push(0u8); // Filter type 0 (None)
            let row_start = y * w;
            for x in 0..w {
                let px = self.pixels[row_start + x];
                raw.push(((px >> 16) & 0xFF) as u8);
                raw.push(((px >> 8) & 0xFF) as u8);
                raw.push((px & 0xFF) as u8);
            }
        }

        let mut zlib = Vec::with_capacity(raw.len() + (raw.len() / 65535 + 1) * 5 + 6);
        zlib.extend_from_slice(&[0x78, 0x01]);
        if raw.is_empty() {
            zlib.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        } else {
            let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
            let last_idx = chunks.len() - 1;
            for (i, block) in chunks.into_iter().enumerate() {
                zlib.push(if i == last_idx { 0x01 } else { 0x00 });
                let len = block.len() as u16;
                zlib.extend_from_slice(&len.to_le_bytes());
                zlib.extend_from_slice(&(!len).to_le_bytes());
                zlib.extend_from_slice(block);
            }
        }
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut png = Vec::with_capacity(zlib.len() + 64);
        png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

        let mut ihdr = [0u8; 13];
        ihdr[0..4].copy_from_slice(&(w as u32).to_be_bytes());
        ihdr[4..8].copy_from_slice(&(h as u32).to_be_bytes());
        ihdr[8] = 8; // 8 bits per channel
        ihdr[9] = 2; // Truecolor RGB
        write_png_chunk(&mut png, b"IHDR", &ihdr);
        write_png_chunk(&mut png, b"IDAT", &zlib);
        write_png_chunk(&mut png, b"IEND", &[]);

        fs::write(path, png)
    }
}

/// Multi-layered procedural shader matching the blue wave phone wallpaper in the reference screenshot.
#[inline]
fn sample_blue_wave_wallpaper(u: f32, v: f32) -> Rgb {
    // Base upper sky-ice gradient
    let sky = Rgb(224, 237, 255).lerp(Rgb(162, 198, 255), (v * 1.1 + u * 0.25).clamp(0.0, 1.0));

    // Wave 1: Soft upper-middle cornflower wave flowing from left-top to right-mid
    let w1_edge = 0.22 + 0.34 * u + 0.06 * (u * 4.2).sin();
    let d1 = v - w1_edge;
    let c1 = if d1 > -0.04 {
        let t = ((d1 + 0.04) / 0.045).clamp(0.0, 1.0);
        let wave1_col =
            Rgb(138, 180, 255).lerp(Rgb(68, 128, 248), ((v - w1_edge) * 2.2).clamp(0.0, 1.0));
        sky.lerp(wave1_col, t)
    } else {
        sky
    };

    // Wave 2: Sweeping cobalt-royal ribbon from mid-left down to bottom-right
    let w2_edge = 0.44 + 0.38 * u * u - 0.04 * (u * 3.5).cos();
    let d2 = v - w2_edge;
    let c2 = if d2 > -0.035 {
        let t = ((d2 + 0.035) / 0.04).clamp(0.0, 1.0);
        let depth = ((v - w2_edge) * 2.0 + (1.0 - u) * 0.3).clamp(0.0, 1.0);
        let wave2_col = Rgb(56, 118, 250).lerp(Rgb(12, 54, 196), depth);
        c1.lerp(wave2_col, t)
    } else {
        c1
    };

    // Wave 3: Deep indigo/royal bottom-left fold with bright crest highlight
    let w3_edge = 0.62 + 0.42 * u + 0.05 * (u * 3.0).sin();
    let d3 = v - w3_edge;
    if d3 > -0.03 {
        let t = ((d3 + 0.03) / 0.035).clamp(0.0, 1.0);
        let ridge = (1.0 - (d3.abs() * 14.0).clamp(0.0, 1.0)) * 0.45;
        let deep_col = Rgb(8, 30, 124)
            .lerp(Rgb(28, 86, 235), (u * 0.9 + (1.0 - v) * 0.4).clamp(0.0, 1.0))
            .lerp(Rgb(115, 168, 255), ridge);
        c2.lerp(deep_col, t)
    } else {
        c2
    }
}

/// Exact 2D signed distance to a triangle `(ax, ay) - (bx, by) - (cx, cy)`.
#[inline]
#[allow(clippy::too_many_arguments)]
fn sd_triangle(
    px: f32,
    py: f32,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    cx: f32,
    cy: f32,
) -> f32 {
    let e0x = bx - ax;
    let e0y = by - ay;
    let e1x = cx - bx;
    let e1y = cy - by;
    let e2x = ax - cx;
    let e2y = ay - cy;

    let v0x = px - ax;
    let v0y = py - ay;
    let v1x = px - bx;
    let v1y = py - by;
    let v2x = px - cx;
    let v2y = py - cy;

    let t0 = ((v0x * e0x + v0y * e0y) / (e0x * e0x + e0y * e0y)).clamp(0.0, 1.0);
    let t1 = ((v1x * e1x + v1y * e1y) / (e1x * e1x + e1y * e1y)).clamp(0.0, 1.0);
    let t2 = ((v2x * e2x + v2y * e2y) / (e2x * e2x + e2y * e2y)).clamp(0.0, 1.0);

    let pq0x = v0x - e0x * t0;
    let pq0y = v0y - e0y * t0;
    let pq1x = v1x - e1x * t1;
    let pq1y = v1y - e1y * t1;
    let pq2x = v2x - e2x * t2;
    let pq2y = v2y - e2y * t2;

    let s = (e0x * e2y - e0y * e2x).signum();
    let d0 = pq0x * pq0x + pq0y * pq0y;
    let d1 = pq1x * pq1x + pq1y * pq1y;
    let d2 = pq2x * pq2x + pq2y * pq2y;
    let min_d = d0.min(d1).min(d2).sqrt();

    let c0 = s * (v0x * e0y - v0y * e0x);
    let c1 = s * (v1x * e1y - v1y * e1x);
    let c2 = s * (v2x * e2y - v2y * e2x);
    let min_c = c0.min(c1).min(c2);

    -min_d * min_c.signum()
}

fn load_system_font(paths: &[&str]) -> Option<FontArc> {
    for p in paths {
        if let Ok(bytes) = fs::read(p) {
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Some(font);
            }
        }
    }
    None
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
