use crate::decoder::{DecodedFrameView, PixelLayout};
use crate::ffi::sdl3::{
    Sdl3Api, SDL_AudioSpec, SDL_AudioStream, SDL_Event, SDL_FRect, SDL_Point, SDL_Renderer,
    SDL_Texture, SDL_Window, SDLK_F11, SDL_AUDIO_S16LE, SDL_EVENT_KEY_DOWN,
    SDL_EVENT_MOUSE_BUTTON_DOWN, SDL_EVENT_QUIT, SDL_EVENT_WINDOW_CLOSE_REQUESTED,
    SDL_HITTEST_DRAGGABLE, SDL_HITTEST_NORMAL, SDL_HITTEST_RESIZE_BOTTOM,
    SDL_HITTEST_RESIZE_BOTTOMLEFT, SDL_HITTEST_RESIZE_BOTTOMRIGHT, SDL_HITTEST_RESIZE_LEFT,
    SDL_HITTEST_RESIZE_RIGHT, SDL_HITTEST_RESIZE_TOP, SDL_HITTEST_RESIZE_TOPLEFT,
    SDL_HITTEST_RESIZE_TOPRIGHT, SDL_INIT_AUDIO, SDL_INIT_VIDEO, SDL_PIXELFORMAT_ARGB8888,
    SDL_PIXELFORMAT_IYUV, SDL_PIXELFORMAT_NV12, SDL_SCANCODE_F11, SDL_TEXTUREACCESS_STREAMING,
    SDL_WINDOW_BORDERLESS, SDL_WINDOW_FULLSCREEN, SDL_WINDOW_HIGH_PIXEL_DENSITY,
    SDL_WINDOW_MAXIMIZED, SDL_WINDOW_MINIMIZED, SDL_WINDOW_RESIZABLE,
};
use crate::settings::{
    scan_recordings, AppSettings, MirrorQuality, NavTab, RecordingEntry,
};
use crate::ui_canvas::{
    FontWeight, Rgb, UiCanvas, APPLE_BLUE, APPLE_BLUE_HOVER, APPLE_BLUE_SOFT, BADGE_OFF_BG,
    BANNER_USB_BG, BANNER_USB_BORDER, BG_HEADER, BG_SIDEBAR, BLUE_GRAD_HOVER_TOP,
    BORDER_SUBTLE, CARD_BORDER, CARD_WHITE, ERROR_BG, ERROR_BG_HOVER, ERROR_RED, LIVE_GREEN,
    PILL_NEUTRAL, PILL_NEUTRAL_HOVER, SHADOW_SLATE, TEXT_BODY, TEXT_PRIMARY, TEXT_SECONDARY,
    WARNING_AMBER,
};
use std::ffi::{c_void, CStr, CString};
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

pub const DASHBOARD_WIDTH: u32 = 1120;
pub const DASHBOARD_HEIGHT: u32 = 740;
pub const TOOLBAR_HEIGHT: u32 = 44;
pub const HEADER_BAR_HEIGHT: f32 = 48.0;

/// Sub-sections inside the Settings view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Preferences,
    HelpAbout,
}

/// Interactive UI actions emitted when the user clicks dashboard controls or presses hotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    StartMirroring,
    StopMirroring,
    ToggleRecord,
    ToggleMic,
    SelectQuality(MirrorQuality),
    SwitchTab(NavTab),
    SwitchSettingsSection(SettingsSection),
    OpenHelpInSettings,
    OpenRecordingsFolder,
    PlayRecording(PathBuf),
    RevealRecording(PathBuf),
    DeleteRecording(PathBuf),
    CycleFps,
    CycleBitrate,
    ToggleCbr,
    TogglePcAudio,
    ToggleAutoConnect,
    ToggleAirPlay,
    ToggleAutoRecord,
    ToggleAlwaysOnTop,
    CycleHwBackend,
    ToggleFullscreenMirror,
    ToggleWindowFullscreen,
    ResetDefaultSettings,
    MinimizeWindow,
    MaximizeRestoreWindow,
    PrevRecordingsPage,
    NextRecordingsPage,
    Quit,
}

#[derive(Clone)]
struct HitZone {
    rect: SDL_FRect,
    action: UiAction,
}

/// Complete state snapshot passed to the Bento Dashboard renderer each frame.
#[derive(Debug, Clone, Copy)]
pub struct DashboardViewState<'a> {
    pub active_tab: NavTab,
    pub settings: &'a AppSettings,
    pub headline: &'a str,
    pub subtext: &'a str,
    pub is_connecting: bool,
    pub is_warning: bool,
    pub is_mirroring: bool,
    pub device_serial: Option<&'a str>,
    pub platform_label: Option<&'a str>,
    pub airplay_listening: bool,
    pub is_recording: bool,
    pub rec_elapsed_secs: u64,
    pub last_saved_recording: Option<&'a str>,
    pub stream_width: u32,
    pub stream_height: u32,
    pub fps: u32,
    pub recordings: &'a [RecordingEntry],
    pub status_toast: Option<&'a str>,
    pub thumbnails: Option<&'a std::collections::HashMap<PathBuf, crate::thumbnail::VideoThumbnail>>,
    pub recordings_page: usize,
}

impl<'a> DashboardViewState<'a> {
    pub fn thumbnail_for(&self, path: &Path) -> Option<&crate::thumbnail::VideoThumbnail> {
        self.thumbnails.and_then(|map| map.get(path))
    }
}

struct HitTestContext {
    get_window_size: unsafe extern "C" fn(
        *mut SDL_Window,
        *mut std::ffi::c_int,
        *mut std::ffi::c_int,
    ) -> bool,
    get_window_flags: Option<unsafe extern "C" fn(*mut SDL_Window) -> u64>,
    fullscreen_mirror_active: std::sync::atomic::AtomicBool,
    toolbar_drag_max_x: std::sync::atomic::AtomicI32,
}

#[derive(Clone, PartialEq, Eq)]
struct UiCacheKey {
    win_w_px: u32,
    win_h_px: u32,
    dpi_bits: u32,
    is_full_video: bool,
    is_window_fullscreen: bool,
    active_tab: NavTab,
    settings_section: SettingsSection,
    is_mirroring: bool,
    is_connecting: bool,
    is_warning: bool,
    is_recording: bool,
    rec_elapsed_secs: u64,
    stream_width: u32,
    stream_height: u32,
    fps: u32,
    settings: AppSettings,
    hovered_zone_idx: Option<usize>,
    headline: String,
    subtext: String,
    device_serial: Option<String>,
    platform_label: Option<String>,
    airplay_listening: bool,
    last_saved_recording: Option<String>,
    status_toast: Option<String>,
    recordings_len: usize,
    recordings_page: usize,
    anim_bucket: u32,
}

fn find_hovered_zone_index(hit_zones: &[HitZone], mx: f32, my: f32) -> Option<usize> {
    hit_zones
        .iter()
        .enumerate()
        .rev()
        .find(|(_, z)| point_in_rect(mx, my, &z.rect))
        .map(|(idx, _)| idx)
}

unsafe extern "C" fn window_hit_test_callback(
    win: *mut SDL_Window,
    area: *const SDL_Point,
    data: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if win.is_null() || area.is_null() || data.is_null() {
        return SDL_HITTEST_NORMAL;
    }
    let ctx = &*(data as *const HitTestContext);
    let pt = *area;

    let mut w: std::ffi::c_int = DASHBOARD_WIDTH as std::ffi::c_int;
    let mut h: std::ffi::c_int = DASHBOARD_HEIGHT as std::ffi::c_int;
    let _ = (ctx.get_window_size)(win, &mut w, &mut h);
    if w <= 0 || h <= 0 {
        return SDL_HITTEST_NORMAL;
    }

    let flags = ctx.get_window_flags.map(|f| f(win)).unwrap_or(0);
    let is_maximized = (flags & SDL_WINDOW_MAXIMIZED) != 0;
    let is_fullscreen = (flags & SDL_WINDOW_FULLSCREEN) != 0;

    if is_fullscreen {
        return SDL_HITTEST_NORMAL;
    }

    // 1. Resize edges and corners (5px perimeter when window is not maximized or fullscreen)
    if !is_maximized {
        let border = 5;
        let on_left = pt.x < border;
        let on_right = pt.x >= w - border;
        let on_top = pt.y < border;
        let on_bottom = pt.y >= h - border;

        if on_top && on_left {
            return SDL_HITTEST_RESIZE_TOPLEFT;
        }
        if on_top && on_right {
            return SDL_HITTEST_RESIZE_TOPRIGHT;
        }
        if on_bottom && on_left {
            return SDL_HITTEST_RESIZE_BOTTOMLEFT;
        }
        if on_bottom && on_right {
            return SDL_HITTEST_RESIZE_BOTTOMRIGHT;
        }
        if on_top {
            return SDL_HITTEST_RESIZE_TOP;
        }
        if on_bottom {
            return SDL_HITTEST_RESIZE_BOTTOM;
        }
        if on_left {
            return SDL_HITTEST_RESIZE_LEFT;
        }
        if on_right {
            return SDL_HITTEST_RESIZE_RIGHT;
        }
    }

    // 2. Custom Header Bar Draggable Region
    let is_full_video = ctx
        .fullscreen_mirror_active
        .load(std::sync::atomic::Ordering::Relaxed);
    if !is_full_video {
        // In dashboard mode, the custom header bar is y = 0..54, and the right-side window
        // control buttons (Minimize, Maximize, Close) occupy x = (w - 138)..w.
        if pt.y >= 0 && pt.y < (HEADER_BAR_HEIGHT as std::ffi::c_int) && pt.x < (w - 138) {
            return SDL_HITTEST_DRAGGABLE;
        }
    } else if pt.y >= 0
        && pt.y < (TOOLBAR_HEIGHT as std::ffi::c_int)
        && pt.x < ctx.toolbar_drag_max_x.load(std::sync::atomic::Ordering::Relaxed)
    {
        // In compact phone / full-window video mode, allow dragging along the left section of the top bar
        return SDL_HITTEST_DRAGGABLE;
    }

    SDL_HITTEST_NORMAL
}

/// Ultra-low-latency SDL3 window, Bento Dashboard "Mobile Mirror" HiDPI UI presenter, and GPU YUV/NV12 video renderer.
pub struct Sdl3Renderer {
    api: Sdl3Api,
    window: *mut SDL_Window,
    renderer: *mut SDL_Renderer,
    hit_test_ctx: *mut HitTestContext,
    texture: *mut SDL_Texture,
    tex_width: u32,
    tex_height: u32,
    tex_layout: Option<PixelLayout>,
    ui_texture: *mut SDL_Texture,
    ui_tex_w: usize,
    ui_tex_h: usize,
    pub ui_canvas: UiCanvas,
    hit_zones: Vec<HitZone>,
    last_record_toggle: std::time::Instant,
    last_mic_toggle: std::time::Instant,
    pub active_tab: NavTab,
    pub settings_section: SettingsSection,
    pub cached_recordings: Vec<RecordingEntry>,
    last_recordings_scan: std::time::Instant,
    pub status_toast: Option<(String, std::time::Instant)>,
    pub recordings_page: usize,
    pub thumbnail_cache: std::collections::HashMap<PathBuf, crate::thumbnail::VideoThumbnail>,
    in_compact_phone_mode: bool,
    is_window_fullscreen: bool,
    applied_video_w: u32,
    applied_video_h: u32,
    last_ui_cache_key: Option<UiCacheKey>,
    cached_center_video_viewport: Option<SDL_FRect>,
}

impl Sdl3Renderer {
    pub fn new(title: &str, initial_width: u32, initial_height: u32) -> Result<Self, String> {
        let api = Sdl3Api::load()?;

        unsafe {
            if !(api.sdl_init)(SDL_INIT_VIDEO | SDL_INIT_AUDIO) {
                return Err(format!("SDL_Init failed: {}", Self::last_error(&api)));
            }

            let (win_w, win_h) = (initial_width.max(820), initial_height.max(560));

            let c_title =
                CString::new(title).unwrap_or_else(|_| CString::new("Mobile Mirror").unwrap());
            let window = (api.sdl_create_window)(
                c_title.as_ptr(),
                win_w as i32,
                win_h as i32,
                SDL_WINDOW_BORDERLESS | SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY,
            );
            if window.is_null() {
                (api.sdl_quit)();
                return Err(format!(
                    "SDL_CreateWindow failed: {}",
                    Self::last_error(&api)
                ));
            }

            if let Some(set_bordered) = api.sdl_set_window_bordered {
                let _ = set_bordered(window, false);
            }

            // Set native window & taskbar icon ("Glossy Laptop Mirroring App Icon")
            if let (Some(create_surf), Some(set_icon), Some(destroy_surf)) = (
                api.sdl_create_surface_from,
                api.sdl_set_window_icon,
                api.sdl_destroy_surface,
            ) {
                let mut icon_argb = crate::ui_canvas::app_icon_argb_256();
                let surf = create_surf(
                    256,
                    256,
                    SDL_PIXELFORMAT_ARGB8888,
                    icon_argb.as_mut_ptr() as *mut std::ffi::c_void,
                    256 * 4,
                );
                if !surf.is_null() {
                    let _ = set_icon(window, surf);
                    destroy_surf(surf);
                }
            }

            let hit_test_ctx = Box::into_raw(Box::new(HitTestContext {
                get_window_size: api.sdl_get_window_size,
                get_window_flags: api.sdl_get_window_flags,
                fullscreen_mirror_active: std::sync::atomic::AtomicBool::new(false),
                toolbar_drag_max_x: std::sync::atomic::AtomicI32::new(170),
            }));
            if let Some(set_hit_test) = api.sdl_set_window_hit_test {
                let _ = set_hit_test(
                    window,
                    Some(window_hit_test_callback),
                    hit_test_ctx as *mut std::ffi::c_void,
                );
            }

            let renderer = (api.sdl_create_renderer)(window, ptr::null());
            if renderer.is_null() {
                if let Some(set_hit_test) = api.sdl_set_window_hit_test {
                    let _ = set_hit_test(window, None, ptr::null_mut());
                }
                let _ = Box::from_raw(hit_test_ctx);
                (api.sdl_destroy_window)(window);
                (api.sdl_quit)();
                return Err(format!(
                    "SDL_CreateRenderer failed: {}",
                    Self::last_error(&api)
                ));
            }

            // Disable VSync for immediate sub-millisecond frame presentation
            let _ = (api.sdl_set_render_vsync)(renderer, 0);

            let ui_canvas = UiCanvas::new(win_w as usize, win_h as usize);

            let inst = Self {
                api,
                window,
                renderer,
                hit_test_ctx,
                texture: ptr::null_mut(),
                tex_width: 0,
                tex_height: 0,
                tex_layout: None,
                ui_texture: ptr::null_mut(),
                ui_tex_w: 0,
                ui_tex_h: 0,
                ui_canvas,
                hit_zones: Vec::with_capacity(32),
                last_record_toggle: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(2))
                    .unwrap_or_else(std::time::Instant::now),
                last_mic_toggle: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(2))
                    .unwrap_or_else(std::time::Instant::now),
                active_tab: NavTab::Home,
                settings_section: SettingsSection::Preferences,
                cached_recordings: Vec::new(),
                last_recordings_scan: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or_else(std::time::Instant::now),
                status_toast: None,
                recordings_page: 0,
                thumbnail_cache: std::collections::HashMap::new(),
                in_compact_phone_mode: false,
                is_window_fullscreen: false,
                applied_video_w: 0,
                applied_video_h: 0,
                last_ui_cache_key: None,
                cached_center_video_viewport: None,
            };
            let _ = inst.sync_and_query_metrics();
            Ok(inst)
        }
    }

    pub fn set_title(&mut self, title: &str) {
        if let Ok(c_title) = CString::new(title) {
            unsafe {
                (self.api.sdl_set_window_title)(self.window, c_title.as_ptr());
            }
        }
    }

    pub fn set_always_on_top(&mut self, on_top: bool) {
        unsafe {
            if let Some(func) = self.api.sdl_set_window_always_on_top {
                let _ = func(self.window, on_top);
            }
        }
    }

    pub fn is_window_minimized(&self) -> bool {
        unsafe {
            self.api
                .sdl_get_window_flags
                .map(|f| (f(self.window) & SDL_WINDOW_MINIMIZED) != 0)
                .unwrap_or(false)
        }
    }

    pub fn is_window_fullscreen(&self) -> bool {
        unsafe {
            let flag_fs = self
                .api
                .sdl_get_window_flags
                .map(|f| (f(self.window) & SDL_WINDOW_FULLSCREEN) != 0)
                .unwrap_or(false);
            self.is_window_fullscreen || flag_fs
        }
    }

    pub fn set_window_fullscreen(&mut self, fullscreen: bool) {
        unsafe {
            if let Some(set_fs) = self.api.sdl_set_window_fullscreen {
                let _ = set_fs(self.window, fullscreen);
            } else if fullscreen {
                if let Some(maximize) = self.api.sdl_maximize_window {
                    let _ = maximize(self.window);
                }
            } else if let Some(restore) = self.api.sdl_restore_window {
                let _ = restore(self.window);
            }
            if let Some(sync) = self.api.sdl_sync_window {
                let _ = sync(self.window);
            }
            self.is_window_fullscreen = fullscreen;
            if !fullscreen {
                if self.in_compact_phone_mode
                    && self.applied_video_w > 0
                    && self.applied_video_h > 0
                {
                    let (win_w, win_h) =
                        calculate_target_window_size(self.applied_video_w, self.applied_video_h);
                    let _ =
                        (self.api.sdl_set_window_size)(self.window, win_w as i32, win_h as i32);
                    let _ = (self.api.sdl_set_window_position)(
                        self.window,
                        0x2FFF_0000,
                        0x2FFF_0000,
                    );
                } else {
                    let _ = (self.api.sdl_set_window_size)(
                        self.window,
                        DASHBOARD_WIDTH as i32,
                        DASHBOARD_HEIGHT as i32,
                    );
                    let _ = (self.api.sdl_set_window_position)(
                        self.window,
                        0x2FFF_0000,
                        0x2FFF_0000,
                    );
                }
            }
            self.last_ui_cache_key = None;
        }
    }

    pub fn toggle_window_fullscreen(&mut self) -> bool {
        let next = !self.is_window_fullscreen();
        self.set_window_fullscreen(next);
        next
    }

    pub fn minimize_window(&mut self) {
        unsafe {
            // If currently in fullscreen, exit fullscreen first so Windows cleanly minimizes
            // the window to the taskbar and uncovers the desktop.
            if self.is_window_fullscreen() {
                if let Some(set_fs) = self.api.sdl_set_window_fullscreen {
                    let _ = set_fs(self.window, false);
                }
                if let Some(sync) = self.api.sdl_sync_window {
                    let _ = sync(self.window);
                }
                self.is_window_fullscreen = false;
                if self.in_compact_phone_mode
                    && self.applied_video_w > 0
                    && self.applied_video_h > 0
                {
                    let (win_w, win_h) =
                        calculate_target_window_size(self.applied_video_w, self.applied_video_h);
                    let _ =
                        (self.api.sdl_set_window_size)(self.window, win_w as i32, win_h as i32);
                }
            }
            if let Some(func) = self.api.sdl_minimize_window {
                let _ = func(self.window);
            }
            self.last_ui_cache_key = None;
        }
    }

    pub fn toggle_maximize_window(&mut self) {
        unsafe {
            if self.is_window_fullscreen() {
                self.set_window_fullscreen(false);
                return;
            }
            let is_max = self
                .api
                .sdl_get_window_flags
                .map(|f| (f(self.window) & SDL_WINDOW_MAXIMIZED) != 0)
                .unwrap_or(false);
            if is_max {
                if let Some(restore) = self.api.sdl_restore_window {
                    let _ = restore(self.window);
                }
            } else if let Some(maximize) = self.api.sdl_maximize_window {
                let _ = maximize(self.window);
            }
            self.last_ui_cache_key = None;
        }
    }

    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.status_toast = Some((msg.into(), std::time::Instant::now()));
    }

    pub fn active_toast(&mut self) -> Option<&str> {
        if let Some((_, ts)) = &self.status_toast {
            if ts.elapsed() > std::time::Duration::from_millis(3800) {
                self.status_toast = None;
                return None;
            }
        }
        self.status_toast.as_ref().map(|(s, _)| s.as_str())
    }

    pub fn refresh_recordings_if_needed(&mut self, record_dir: &Path, force: bool) {
        if force || self.last_recordings_scan.elapsed() >= std::time::Duration::from_secs(2) {
            self.cached_recordings = scan_recordings(record_dir);
            self.last_recordings_scan = std::time::Instant::now();
            self.thumbnail_cache.retain(|p, _| p.exists());
            if self.active_tab == NavTab::Recordings {
                for rec in &self.cached_recordings {
                    if !self.thumbnail_cache.contains_key(&rec.full_path) {
                        if let Some(thumb) =
                            crate::thumbnail::extract_video_thumbnail(&rec.full_path, 240, 136)
                        {
                            self.thumbnail_cache.insert(rec.full_path.clone(), thumb);
                        }
                    }
                }
            }
        }
    }

    pub fn window_size(&self) -> (f32, f32) {
        unsafe {
            let mut w: i32 = DASHBOARD_WIDTH as i32;
            let mut h: i32 = DASHBOARD_HEIGHT as i32;
            let _ = (self.api.sdl_get_window_size)(self.window, &mut w, &mut h);
            (w.max(240) as f32, h.max(240) as f32)
        }
    }

    pub fn mouse_position(&self) -> (f32, f32) {
        unsafe {
            let mut mx: f32 = -1000.0;
            let mut my: f32 = -1000.0;
            let _ = (self.api.sdl_get_mouse_state)(&mut mx, &mut my);
            (mx, my)
        }
    }

    pub fn is_hovered_zone_changed(&self, mx: f32, my: f32) -> bool {
        let current_hover = find_hovered_zone_index(&self.hit_zones, mx, my);
        let cached_hover = self
            .last_ui_cache_key
            .as_ref()
            .and_then(|k| k.hovered_zone_idx);
        current_hover != cached_hover
    }

    pub fn sync_window_geometry(&mut self, want_compact_phone: bool, video_w: u32, video_h: u32) {
        if self.is_window_minimized() {
            return;
        }
        unsafe {
            if want_compact_phone && video_w > 0 && video_h > 0 {
                if self.is_window_fullscreen() {
                    self.in_compact_phone_mode = true;
                    self.applied_video_w = video_w;
                    self.applied_video_h = video_h;
                    return;
                }
                if !self.in_compact_phone_mode
                    || self.applied_video_w != video_w
                    || self.applied_video_h != video_h
                {
                    let is_max = self
                        .api
                        .sdl_get_window_flags
                        .map(|f| (f(self.window) & SDL_WINDOW_MAXIMIZED) != 0)
                        .unwrap_or(false);
                    if is_max {
                        if let Some(restore) = self.api.sdl_restore_window {
                            let _ = restore(self.window);
                        }
                    }
                    let (win_w, win_h) = calculate_target_window_size(video_w, video_h);
                    let _ =
                        (self.api.sdl_set_window_size)(self.window, win_w as i32, win_h as i32);
                    let _ = (self.api.sdl_set_window_position)(
                        self.window,
                        0x2FFF_0000,
                        0x2FFF_0000,
                    );
                    self.in_compact_phone_mode = true;
                    self.applied_video_w = video_w;
                    self.applied_video_h = video_h;
                    self.last_ui_cache_key = None;
                }
            } else if !want_compact_phone && self.in_compact_phone_mode {
                if self.is_window_fullscreen() {
                    if let Some(set_fs) = self.api.sdl_set_window_fullscreen {
                        let _ = set_fs(self.window, false);
                    }
                    if let Some(sync) = self.api.sdl_sync_window {
                        let _ = sync(self.window);
                    }
                    self.is_window_fullscreen = false;
                }
                let is_max = self
                    .api
                    .sdl_get_window_flags
                    .map(|f| (f(self.window) & SDL_WINDOW_MAXIMIZED) != 0)
                    .unwrap_or(false);
                if is_max {
                    if let Some(restore) = self.api.sdl_restore_window {
                        let _ = restore(self.window);
                    }
                }
                let _ = (self.api.sdl_set_window_size)(
                    self.window,
                    DASHBOARD_WIDTH as i32,
                    DASHBOARD_HEIGHT as i32,
                );
                let _ = (self.api.sdl_set_window_position)(
                    self.window,
                    0x2FFF_0000,
                    0x2FFF_0000,
                );
                self.in_compact_phone_mode = false;
                self.applied_video_w = 0;
                self.applied_video_h = 0;
                self.last_ui_cache_key = None;
            }
        }
    }

    fn sync_and_query_metrics(&self) -> (f32, f32, f32, u32, u32) {
        unsafe {
            let mut lw: i32 = DASHBOARD_WIDTH as i32;
            let mut lh: i32 = DASHBOARD_HEIGHT as i32;
            let _ = (self.api.sdl_get_window_size)(self.window, &mut lw, &mut lh);
            let logical_w = lw.max(240) as f32;
            let logical_h = lh.max(240) as f32;

            if let Some(get_px) = self.api.sdl_get_window_size_in_pixels {
                let mut pw: i32 = 0;
                let mut ph: i32 = 0;
                if get_px(self.window, &mut pw, &mut ph) && lw > 0 && lh > 0 && pw > 0 && ph > 0 {
                    let sx = (pw as f32 / lw as f32).clamp(1.0, 4.0);
                    let sy = (ph as f32 / lh as f32).clamp(1.0, 4.0);
                    let _ = (self.api.sdl_set_render_scale)(self.renderer, sx, sy);
                    return (logical_w, logical_h, sx, pw as u32, ph as u32);
                }
            }

            (
                logical_w,
                logical_h,
                1.0,
                logical_w as u32,
                logical_h as u32,
            )
        }
    }

    /// Resets the video texture when a USB stream disconnects and restores the dashboard window geometry.
    pub fn reset_to_startup_geometry(&mut self) {
        unsafe {
            if !self.texture.is_null() {
                (self.api.sdl_destroy_texture)(self.texture);
                self.texture = ptr::null_mut();
            }
            self.tex_width = 0;
            self.tex_height = 0;
            self.tex_layout = None;
        }
        if self.is_window_fullscreen() {
            self.set_window_fullscreen(false);
        }
        self.sync_window_geometry(false, 0, 0);
        self.last_ui_cache_key = None;
        self.set_title("Mobile Mirror");
        self.sync_and_query_metrics();
    }

    /// Polls all SDL3 mouse and keyboard events and maps clicks against active dashboard `HitZone`s.
    pub fn poll_dashboard_actions(&mut self) -> Vec<UiAction> {
        let mut actions = Vec::new();
        unsafe {
            let mut event = SDL_Event {
                event_type: 0,
                _padding: [0u8; 124],
            };
            while (self.api.sdl_poll_event)(&mut event) {
                if event.event_type == SDL_EVENT_QUIT
                    || event.event_type == SDL_EVENT_WINDOW_CLOSE_REQUESTED
                {
                    actions.push(UiAction::Quit);
                    return actions;
                }
                if event.event_type == SDL_EVENT_KEY_DOWN && !event.is_key_repeat() {
                    let key = event.keycode();
                    let scancode = event.scancode();
                    if key == SDLK_F11 || scancode == SDL_SCANCODE_F11 {
                        actions.push(UiAction::ToggleWindowFullscreen);
                    } else if key == 0x1B {
                        // Esc exits fullscreen if currently fullscreen; otherwise quits app
                        if self.is_window_fullscreen() {
                            actions.push(UiAction::ToggleWindowFullscreen);
                        } else {
                            actions.push(UiAction::Quit);
                            return actions;
                        }
                    } else if key == 0x71 || key == 0x51 {
                        actions.push(UiAction::Quit);
                        return actions;
                    } else if key == 0x72 || key == 0x52 || key == 21 {
                        if self.last_record_toggle.elapsed()
                            >= std::time::Duration::from_millis(280)
                        {
                            self.last_record_toggle = std::time::Instant::now();
                            actions.push(UiAction::ToggleRecord);
                        }
                    } else if key == 0x6D || key == 0x4D || key == 16 {
                        if self.last_mic_toggle.elapsed() >= std::time::Duration::from_millis(280) {
                            self.last_mic_toggle = std::time::Instant::now();
                            actions.push(UiAction::ToggleMic);
                        }
                    } else if key == 0x66 || key == 0x46 {
                        actions.push(UiAction::ToggleFullscreenMirror);
                    }
                }
                if event.event_type == SDL_EVENT_MOUSE_BUTTON_DOWN {
                    let (mx, my) = self.mouse_position();
                    // Check hit zones in reverse draw order (top-most control wins)
                    for zone in self.hit_zones.iter().rev() {
                        if point_in_rect(mx, my, &zone.rect) {
                            match &zone.action {
                                UiAction::PrevRecordingsPage => {
                                    self.recordings_page = self.recordings_page.saturating_sub(1);
                                }
                                UiAction::NextRecordingsPage => {
                                    self.recordings_page = self.recordings_page.saturating_add(1);
                                }
                                UiAction::SwitchSettingsSection(sec) => {
                                    self.settings_section = *sec;
                                }
                                UiAction::OpenHelpInSettings => {
                                    self.active_tab = NavTab::Settings;
                                    self.settings_section = SettingsSection::HelpAbout;
                                }
                                _ => {}
                            }
                            actions.push(zone.action.clone());
                            break;
                        }
                    }
                }
            }
        }
        actions
    }

    /// Legacy helper kept for compatibility with existing callers/tests.
    pub fn poll_startup_quit(&mut self) -> bool {
        !self
            .poll_dashboard_actions()
            .into_iter()
            .any(|a| a == UiAction::Quit)
    }

    /// Renders the full Bento Dashboard (or the Compact Phone Window when `fullscreen_mirror` is active)
    /// using strict `UiCacheKey` dirty-checking so 99% of video frames skip CPU UI rasterization & PCIe uploads.
    pub fn render_dashboard(&mut self, state: &DashboardViewState<'_>) {
        if self.is_window_minimized() {
            return;
        }
        let vid_w = if self.tex_width > 0 {
            self.tex_width
        } else {
            state.stream_width
        };
        let vid_h = if self.tex_height > 0 {
            self.tex_height
        } else {
            state.stream_height
        };
        let is_full_video = state.is_mirroring
            && state.settings.fullscreen_mirror
            && (!self.texture.is_null() || (vid_w > 0 && vid_h > 0));

        self.sync_window_geometry(is_full_video, vid_w, vid_h);

        let is_win_fs = self.is_window_fullscreen();
        let (win_w, win_h, dpi_scale, pw, ph) = self.sync_and_query_metrics();
        let (mx, my) = self.mouse_position();

        if !self.hit_test_ctx.is_null() {
            unsafe {
                (*self.hit_test_ctx)
                    .fullscreen_mirror_active
                    .store(is_full_video, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let anim_bucket = if !state.is_mirroring && state.is_connecting {
            self.ui_canvas.anim.step_startup();
            (self.ui_canvas.anim.pulse_phase * 20.0) as u32
        } else {
            0
        };

        let hovered_zone_idx = find_hovered_zone_index(&self.hit_zones, mx, my);
        let mut current_key = UiCacheKey {
            win_w_px: pw,
            win_h_px: ph,
            dpi_bits: dpi_scale.to_bits(),
            is_full_video,
            is_window_fullscreen: is_win_fs,
            active_tab: state.active_tab,
            settings_section: self.settings_section,
            is_mirroring: state.is_mirroring,
            is_connecting: state.is_connecting,
            is_warning: state.is_warning,
            is_recording: state.is_recording,
            rec_elapsed_secs: if state.is_recording {
                state.rec_elapsed_secs
            } else {
                0
            },
            stream_width: vid_w,
            stream_height: vid_h,
            fps: state.fps,
            settings: state.settings.clone(),
            hovered_zone_idx,
            headline: state.headline.to_string(),
            subtext: state.subtext.to_string(),
            device_serial: state.device_serial.map(str::to_string),
            platform_label: state.platform_label.map(str::to_string),
            airplay_listening: state.airplay_listening,
            last_saved_recording: state.last_saved_recording.map(str::to_string),
            status_toast: state.status_toast.map(str::to_string),
            recordings_len: state.recordings.len(),
            recordings_page: self.recordings_page,
            anim_bucket,
        };

        let mut enriched_state = *state;
        if enriched_state.thumbnails.is_none() {
            enriched_state.thumbnails = Some(&self.thumbnail_cache);
        }
        enriched_state.recordings_page = self.recordings_page;

        let needs_ui_redraw =
            self.ui_texture.is_null() || self.last_ui_cache_key.as_ref() != Some(&current_key);

        if needs_ui_redraw {
            self.hit_zones.clear();
            if is_full_video {
                self.ui_canvas
                    .set_logical_size(win_w, TOOLBAR_HEIGHT as f32, dpi_scale);
                let first_btn_x = draw_live_toolbar_strip_with_Zones(
                    &mut self.ui_canvas,
                    &mut self.hit_zones,
                    mx,
                    my,
                    0.0,
                    win_w,
                    state.is_recording,
                    state.rec_elapsed_secs,
                    state.settings.mic_enabled,
                    state.fps,
                    is_win_fs,
                );
                if !self.hit_test_ctx.is_null() {
                    unsafe {
                        (*self.hit_test_ctx).toolbar_drag_max_x.store(
                            (first_btn_x as i32).max(60),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
                self.cached_center_video_viewport = None;
            } else {
                self.ui_canvas.set_logical_size(win_w, win_h, dpi_scale);
                self.cached_center_video_viewport = draw_bento_dashboard_canvas(
                    &mut self.ui_canvas,
                    &mut self.hit_zones,
                    mx,
                    my,
                    &enriched_state,
                    self.settings_section,
                );
            }
            self.upload_ui_canvas_texture();
            current_key.hovered_zone_idx = find_hovered_zone_index(&self.hit_zones, mx, my);
            self.last_ui_cache_key = Some(current_key);
        }

        if is_full_video {
            let avail_w = win_w.max(1.0);
            let avail_h = (win_h - TOOLBAR_HEIGHT as f32).max(1.0);
            let video_dst = compute_aspect_rect(
                0.0,
                TOOLBAR_HEIGHT as f32,
                avail_w,
                avail_h,
                vid_w,
                vid_h,
            );
            let toolbar_dst = SDL_FRect {
                x: 0.0,
                y: 0.0,
                w: win_w,
                h: TOOLBAR_HEIGHT as f32,
            };
            let border_rect = SDL_FRect {
                x: 0.0,
                y: 0.0,
                w: win_w,
                h: win_h,
            };

            unsafe {
                (self.api.sdl_set_render_draw_color)(self.renderer, 12, 16, 26, 255);
                (self.api.sdl_render_clear)(self.renderer);
                if !self.texture.is_null() {
                    (self.api.sdl_render_texture)(
                        self.renderer,
                        self.texture,
                        ptr::null(),
                        &video_dst,
                    );
                }
                if !self.ui_texture.is_null() {
                    (self.api.sdl_render_texture)(
                        self.renderer,
                        self.ui_texture,
                        ptr::null(),
                        &toolbar_dst,
                    );
                }
                if !is_win_fs {
                    (self.api.sdl_set_render_draw_color)(self.renderer, 208, 216, 232, 255);
                    (self.api.sdl_render_rect)(self.renderer, &border_rect);
                }
                (self.api.sdl_render_present)(self.renderer);
            }
            return;
        }

        unsafe {
            let full_dst = SDL_FRect {
                x: 0.0,
                y: 0.0,
                w: win_w,
                h: win_h,
            };
            (self.api.sdl_set_render_draw_color)(self.renderer, 243, 246, 253, 255);
            (self.api.sdl_render_clear)(self.renderer);
            if !self.ui_texture.is_null() {
                (self.api.sdl_render_texture)(self.renderer, self.ui_texture, ptr::null(), &full_dst);
            }

            // If live mirroring is active and we are on the Home tab, composite the GPU NV12/YUV texture
            // directly into the Center Card's live stage!
            if state.is_mirroring
                && state.active_tab == NavTab::Home
                && !self.texture.is_null()
                && self.tex_width > 0
                && self.tex_height > 0
            {
                if let Some(stage) = self.cached_center_video_viewport {
                    let video_dst = compute_aspect_rect(
                        stage.x + 8.0,
                        stage.y + 8.0,
                        (stage.w - 16.0).max(10.0),
                        (stage.h - 16.0).max(10.0),
                        self.tex_width,
                        self.tex_height,
                    );
                    (self.api.sdl_render_texture)(
                        self.renderer,
                        self.texture,
                        ptr::null(),
                        &video_dst,
                    );
                }
            }

            (self.api.sdl_render_present)(self.renderer);
        }
    }

    /// Uploads a decoded NV12 or YUV420P frame to the GPU texture without resizing away from the dashboard.
    pub fn upload_video_frame(&mut self, frame: &DecodedFrameView<'_>) -> Result<(), String> {
        unsafe {
            if self.texture.is_null()
                || self.tex_width != frame.width
                || self.tex_height != frame.height
                || self.tex_layout != Some(frame.layout)
            {
                if !self.texture.is_null() {
                    (self.api.sdl_destroy_texture)(self.texture);
                    self.texture = ptr::null_mut();
                }

                let sdl_format = match frame.layout {
                    PixelLayout::Nv12 => SDL_PIXELFORMAT_NV12,
                    PixelLayout::Yuv420p => SDL_PIXELFORMAT_IYUV,
                };

                self.texture = (self.api.sdl_create_texture)(
                    self.renderer,
                    sdl_format,
                    SDL_TEXTUREACCESS_STREAMING,
                    frame.width as i32,
                    frame.height as i32,
                );
                if self.texture.is_null() {
                    return Err(format!(
                        "SDL_CreateTexture failed: {}",
                        Self::last_error(&self.api)
                    ));
                }
                self.tex_width = frame.width;
                self.tex_height = frame.height;
                self.tex_layout = Some(frame.layout);
            }

            let updated = match frame.layout {
                PixelLayout::Nv12 => (self.api.sdl_update_nv_texture)(
                    self.texture,
                    ptr::null(),
                    frame.y_plane.as_ptr(),
                    frame.y_pitch,
                    frame.u_or_uv_plane.as_ptr(),
                    frame.u_or_uv_pitch,
                ),
                PixelLayout::Yuv420p => {
                    let v_plane = frame.v_plane.ok_or("Missing V plane for YUV420P")?;
                    (self.api.sdl_update_yuv_texture)(
                        self.texture,
                        ptr::null(),
                        frame.y_plane.as_ptr(),
                        frame.y_pitch,
                        frame.u_or_uv_plane.as_ptr(),
                        frame.u_or_uv_pitch,
                        v_plane.as_ptr(),
                        frame.v_pitch,
                    )
                }
            };

            if !updated {
                return Err(format!(
                    "SDL texture upload failed: {}",
                    Self::last_error(&self.api)
                ));
            }
            Ok(())
        }
    }

    /// Legacy startup screen wrapper that renders the full Bento Dashboard in its default state.
    pub fn render_startup_screen(
        &mut self,
        headline: &str,
        subtext: &str,
        is_connecting: bool,
        is_warning: bool,
    ) {
        let default_settings = AppSettings::default();
        let recs = self.cached_recordings.clone();
        let state = DashboardViewState {
            active_tab: self.active_tab,
            settings: &default_settings,
            headline,
            subtext,
            is_connecting,
            is_warning,
            is_mirroring: false,
            device_serial: None,
            platform_label: None,
            airplay_listening: false,
            is_recording: false,
            rec_elapsed_secs: 0,
            last_saved_recording: None,
            stream_width: 0,
            stream_height: 0,
            fps: 60,
            recordings: &recs,
            status_toast: None,
            thumbnails: None,
            recordings_page: 0,
        };
        self.render_dashboard(&state);
    }

    pub fn poll_events(&mut self) -> (bool, bool, bool) {
        let actions = self.poll_dashboard_actions();
        let mut should_run = true;
        let mut toggle_rec = false;
        let mut toggle_mic = false;
        for a in actions {
            match a {
                UiAction::Quit => should_run = false,
                UiAction::ToggleRecord => toggle_rec = true,
                UiAction::ToggleMic => toggle_mic = true,
                _ => {}
            }
        }
        (should_run, toggle_rec, toggle_mic)
    }

    pub fn render_frame(
        &mut self,
        frame: &DecodedFrameView<'_>,
        is_recording: bool,
        rec_elapsed_secs: u64,
        is_mic_enabled: bool,
        fps: u32,
    ) -> Result<(), String> {
        self.upload_video_frame(frame)?;
        let mut settings = AppSettings::default();
        settings.mic_enabled = is_mic_enabled;
        let recs = self.cached_recordings.clone();
        let state = DashboardViewState {
            active_tab: self.active_tab,
            settings: &settings,
            headline: "Connected",
            subtext: "Live USB stream active",
            is_connecting: false,
            is_warning: false,
            is_mirroring: true,
            device_serial: None,
            platform_label: None,
            airplay_listening: false,
            is_recording,
            rec_elapsed_secs,
            last_saved_recording: None,
            stream_width: frame.width,
            stream_height: frame.height,
            fps,
            recordings: &recs,
            status_toast: None,
            thumbnails: None,
            recordings_page: 0,
        };
        self.render_dashboard(&state);
        Ok(())
    }

    pub fn present_cached_frame(
        &mut self,
        is_recording: bool,
        rec_elapsed_secs: u64,
        is_mic_enabled: bool,
        fps: u32,
    ) -> Result<(), String> {
        let mut settings = AppSettings::default();
        settings.mic_enabled = is_mic_enabled;
        let recs = self.cached_recordings.clone();
        let state = DashboardViewState {
            active_tab: self.active_tab,
            settings: &settings,
            headline: "Connected",
            subtext: "Live USB stream active",
            is_connecting: false,
            is_warning: false,
            is_mirroring: !self.texture.is_null(),
            device_serial: None,
            platform_label: None,
            airplay_listening: false,
            is_recording,
            rec_elapsed_secs,
            last_saved_recording: None,
            stream_width: self.tex_width,
            stream_height: self.tex_height,
            fps,
            recordings: &recs,
            status_toast: None,
            thumbnails: None,
            recordings_page: 0,
        };
        self.render_dashboard(&state);
        Ok(())
    }

    fn upload_ui_canvas_texture(&mut self) {
        unsafe {
            let w = self.ui_canvas.width;
            let h = self.ui_canvas.height;
            if w == 0 || h == 0 {
                return;
            }
            if self.ui_texture.is_null() || self.ui_tex_w != w || self.ui_tex_h != h {
                if !self.ui_texture.is_null() {
                    (self.api.sdl_destroy_texture)(self.ui_texture);
                    self.ui_texture = ptr::null_mut();
                }
                self.ui_texture = (self.api.sdl_create_texture)(
                    self.renderer,
                    SDL_PIXELFORMAT_ARGB8888,
                    SDL_TEXTUREACCESS_STREAMING,
                    w as i32,
                    h as i32,
                );
                self.ui_tex_w = w;
                self.ui_tex_h = h;
            }

            if !self.ui_texture.is_null() {
                let _ = (self.api.sdl_update_texture)(
                    self.ui_texture,
                    ptr::null(),
                    self.ui_canvas.pixels.as_ptr() as *const c_void,
                    (w * 4) as i32,
                );
            }
        }
    }

    unsafe fn last_error(api: &Sdl3Api) -> String {
        let err_ptr = (api.sdl_get_error)();
        if err_ptr.is_null() {
            "unknown SDL3 error".to_string()
        } else {
            CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
        }
    }
}

fn compute_aspect_rect(
    avail_x: f32,
    avail_y: f32,
    avail_w: f32,
    avail_h: f32,
    tex_w: u32,
    tex_h: u32,
) -> SDL_FRect {
    if tex_w > 0 && tex_h > 0 {
        let scale = (avail_w / tex_w as f32).min(avail_h / tex_h as f32);
        let vw = (tex_w as f32 * scale).round().min(avail_w);
        let vh = (tex_h as f32 * scale).round().min(avail_h);
        SDL_FRect {
            x: avail_x + ((avail_w - vw) * 0.5).floor(),
            y: avail_y + ((avail_h - vh) * 0.5).floor(),
            w: vw,
            h: vh,
        }
    } else {
        SDL_FRect {
            x: avail_x,
            y: avail_y,
            w: avail_w,
            h: avail_h,
        }
    }
}

// ============================================================================
// Full Bento Dashboard Canvas Renderer (Picture-Similar to Reference Screenshot)
// ============================================================================

/// Renders the complete Bento Dashboard onto `canvas` and populates interactive `hit_zones`.
/// Returns `Some(SDL_FRect)` for the video viewport if `Home` tab is showing a live mirror stream.
fn draw_bento_dashboard_canvas(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    state: &DashboardViewState<'_>,
    settings_section: SettingsSection,
) -> Option<SDL_FRect> {
    let win_w = canvas.logical_width;
    let win_h = canvas.logical_height;

    // 1. Subtle Light-Blue Outer Workspace Surface (#F4F7FC)
    canvas.clear(BG_SIDEBAR);

    // 2. Responsive Left Sidebar
    let compact_sidebar = win_w < 740.0;
    let sidebar_w = if compact_sidebar {
        58.0
    } else if win_w < 960.0 {
        156.0
    } else {
        184.0
    };
    canvas.fill_rect(
        0,
        HEADER_BAR_HEIGHT as i32,
        sidebar_w as i32,
        (win_h - HEADER_BAR_HEIGHT) as i32,
        BG_SIDEBAR,
    );

    // 3. Top Application Header Bar (y = 0..HEADER_BAR_HEIGHT)
    canvas.fill_rect(0, 0, win_w as i32, HEADER_BAR_HEIGHT as i32, BG_HEADER);
    canvas.fill_rect(0, HEADER_BAR_HEIGHT as i32 - 1, win_w as i32, 1, BORDER_SUBTLE);

    // Top-left Brand Icon + "Mobile Mirror" title
    canvas.draw_app_brand_icon(16.0, 11.0, 26.0);
    let hdr_cap = canvas.cap_height(14.5, FontWeight::Semibold);
    canvas.draw_text_baseline(
        "Mobile Mirror",
        50.0,
        (HEADER_BAR_HEIGHT + hdr_cap) * 0.5,
        14.5,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

    if let Some(plat) = state.platform_label {
        if win_w >= 600.0 {
            let pw = (canvas.measure_text_width(plat, 11.5, FontWeight::Semibold) + 18.0).max(64.0);
            let pill_rect = SDL_FRect {
                x: 158.0,
                y: (HEADER_BAR_HEIGHT - 22.0) * 0.5,
                w: pw,
                h: 22.0,
            };
            let (p_bg, p_fg) = if state.is_mirroring {
                (Rgb(232, 249, 238), LIVE_GREEN)
            } else {
                (APPLE_BLUE_SOFT, APPLE_BLUE)
            };
            draw_small_pill(canvas, &pill_rect, plat, p_bg, p_fg);
        }
    }

    // Top-right Windows 11 Window Controls (—  □  ✕)
    let wc_y = 8.0;
    let wc_w = 36.0;
    let wc_h = 32.0;
    let close_x = win_w - 44.0;
    let max_x = close_x - 40.0;
    let min_x = max_x - 40.0;

    let min_rect = SDL_FRect {
        x: min_x,
        y: wc_y,
        w: wc_w,
        h: wc_h,
    };
    let max_rect = SDL_FRect {
        x: max_x,
        y: wc_y,
        w: wc_w,
        h: wc_h,
    };
    let close_rect = SDL_FRect {
        x: close_x,
        y: wc_y,
        w: wc_w,
        h: wc_h,
    };

    for r in [&min_rect, &max_rect, &close_rect] {
        if point_in_rect(mx, my, r) {
            canvas.draw_rounded_card(
                r.x,
                r.y,
                r.w,
                r.h,
                6.0,
                PILL_NEUTRAL_HOVER,
                PILL_NEUTRAL_HOVER,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        }
    }

    let wc_cy = HEADER_BAR_HEIGHT * 0.5;
    canvas.draw_capsule_line(
        min_x + 12.0,
        wc_cy,
        min_x + 24.0,
        wc_cy,
        1.6,
        TEXT_PRIMARY,
        0.88,
    );
    canvas.draw_rounded_stroke(
        max_x + 12.5,
        wc_cy - 5.0,
        10.0,
        10.0,
        1.5,
        1.5,
        TEXT_PRIMARY,
        0.88,
    );
    canvas.draw_capsule_line(
        close_x + 13.0,
        wc_cy - 4.5,
        close_x + 22.0,
        wc_cy + 4.5,
        1.6,
        TEXT_PRIMARY,
        0.9,
    );
    canvas.draw_capsule_line(
        close_x + 22.0,
        wc_cy - 4.5,
        close_x + 13.0,
        wc_cy + 4.5,
        1.6,
        TEXT_PRIMARY,
        0.9,
    );

    hit_zones.push(HitZone {
        rect: min_rect,
        action: UiAction::MinimizeWindow,
    });
    hit_zones.push(HitZone {
        rect: max_rect,
        action: UiAction::MaximizeRestoreWindow,
    });
    hit_zones.push(HitZone {
        rect: close_rect,
        action: UiAction::Quit,
    });

    // 4. Left Sidebar Navigation Items (Home, Recordings, Settings)
    let nav_items = [
        (NavTab::Home, "Home", 64.0, 40.0),
        (NavTab::Recordings, "Recordings", 110.0, 40.0),
        (NavTab::Settings, "Settings", 156.0, 40.0),
    ];

    for (tab, label, ny, nh) in nav_items {
        let nav_rect = SDL_FRect {
            x: 8.0,
            y: ny,
            w: sidebar_w - 16.0,
            h: nh,
        };
        let is_active = state.active_tab == tab;
        let is_hover = point_in_rect(mx, my, &nav_rect);

        if is_active {
            let bg = if is_hover {
                APPLE_BLUE_HOVER
            } else {
                APPLE_BLUE_SOFT
            };
            canvas.draw_rounded_card(
                nav_rect.x,
                nav_rect.y,
                nav_rect.w,
                nav_rect.h,
                8.0,
                bg,
                bg,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
            canvas.draw_rounded_card(
                nav_rect.x + 2.0,
                nav_rect.y + 10.0,
                3.0,
                nav_rect.h - 20.0,
                1.5,
                APPLE_BLUE,
                APPLE_BLUE,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        } else if is_hover {
            canvas.draw_rounded_card(
                nav_rect.x,
                nav_rect.y,
                nav_rect.w,
                nav_rect.h,
                8.0,
                PILL_NEUTRAL_HOVER,
                PILL_NEUTRAL_HOVER,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                0.75,
            );
        }

        let icon_cx = if compact_sidebar {
            sidebar_w * 0.5
        } else {
            28.0
        };
        let icon_cy = ny + nh * 0.5;
        let fg = if is_active { APPLE_BLUE } else { TEXT_BODY };
        let weight = if is_active {
            FontWeight::Semibold
        } else {
            FontWeight::Regular
        };

        let inner_bg = if is_active {
            APPLE_BLUE_SOFT
        } else if is_hover {
            PILL_NEUTRAL_HOVER
        } else {
            BG_SIDEBAR
        };

        match tab {
            NavTab::Home => canvas.draw_home_icon(icon_cx, icon_cy, inner_bg, fg, 1.0),
            NavTab::Recordings => canvas.draw_video_cam_icon(icon_cx, icon_cy, fg, 1.0),
            NavTab::Settings => {
                canvas.draw_gear_icon(icon_cx, icon_cy, 8.0, inner_bg, fg, 1.0);
            }
        }

        if !compact_sidebar {
            let cap = canvas.cap_height(13.8, weight);
            canvas.draw_text_baseline(
                label,
                48.0,
                icon_cy + cap * 0.5,
                13.8,
                weight,
                fg,
                1.0,
            );
        }

        hit_zones.push(HitZone {
            rect: nav_rect,
            action: UiAction::SwitchTab(tab),
        });
    }

    // 5. Fully Responsive Center + Right Column Layout
    let pad = if win_w < 800.0 { 10.0 } else { 16.0 };
    let right_col_w = (win_w * 0.24).clamp(200.0, 272.0);
    let avail_three_col_center = win_w - sidebar_w - right_col_w - pad * 3.0;
    let top_y = HEADER_BAR_HEIGHT + pad;
    let total_h = (win_h - top_y - pad).max(200.0);

    let (center_x, center_y, center_w, center_h) = if avail_three_col_center >= 280.0 {
        let right_col_x = win_w - right_col_w - pad;
        draw_right_column_cards(
            canvas,
            hit_zones,
            mx,
            my,
            right_col_x,
            right_col_w,
            top_y,
            total_h,
            state,
        );
        (
            sidebar_w + pad,
            top_y,
            (right_col_x - pad - (sidebar_w + pad)).max(240.0),
            total_h,
        )
    } else {
        // Ultra-narrow window fallback: stack right cards below center card so nothing overlaps
        let right_h = 156.0_f32.min(total_h * 0.36);
        let c_w = (win_w - sidebar_w - pad * 2.0).max(220.0);
        let c_h = (total_h - right_h - pad).max(160.0);
        let stack_y = top_y + c_h + pad;
        draw_right_column_cards(
            canvas,
            hit_zones,
            mx,
            my,
            sidebar_w + pad,
            c_w,
            stack_y,
            right_h,
            state,
        );
        (sidebar_w + pad, top_y, c_w, c_h)
    };

    // Clean White Center Card Surface
    canvas.draw_rounded_card(
        center_x,
        center_y,
        center_w,
        center_h,
        14.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let video_stage = match state.active_tab {
        NavTab::Home => {
            if state.is_mirroring {
                Some(draw_center_live_mirror_view(
                    canvas, hit_zones, mx, my, center_x, center_y, center_w, center_h, state,
                ))
            } else {
                draw_center_home_hero_view(
                    canvas, hit_zones, mx, my, center_x, center_y, center_w, center_h, state,
                );
                None
            }
        }
        NavTab::Recordings => {
            draw_center_recordings_view(
                canvas, hit_zones, mx, my, center_x, center_y, center_w, center_h, state,
            );
            None
        }
        NavTab::Settings => {
            draw_center_settings_view(
                canvas,
                hit_zones,
                mx,
                my,
                center_x,
                center_y,
                center_w,
                center_h,
                state,
                settings_section,
            );
            None
        }
    };

    // Optional feedback toast pill at top-center of Center Card
    if let Some(toast) = state.status_toast {
        if !toast.is_empty() {
            let tw = canvas.measure_text_width(toast, 12.5, FontWeight::Semibold);
            let pill_w = (tw + 30.0).min(center_w - 32.0);
            let pill_x = center_x + (center_w - pill_w) * 0.5;
            let pill_y = center_y + 10.0;
            canvas.draw_rounded_card(
                pill_x,
                pill_y,
                pill_w,
                30.0,
                8.0,
                TEXT_PRIMARY,
                TEXT_PRIMARY,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                0.95,
            );
            let fitted = canvas.fit_text_to_width(toast, pill_w - 20.0, 12.5, FontWeight::Semibold);
            let cap = canvas.cap_height(12.5, FontWeight::Semibold);
            let ftw = canvas.measure_text_width(&fitted, 12.5, FontWeight::Semibold);
            canvas.draw_text_baseline(
                &fitted,
                pill_x + (pill_w - ftw) * 0.5,
                pill_y + (30.0 + cap) * 0.5,
                12.5,
                FontWeight::Semibold,
                CARD_WHITE,
                1.0,
            );
        }
    }

    video_stage
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn draw_right_column_cards(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    col_x: f32,
    col_w: f32,
    top_y: f32,
    avail_h: f32,
    state: &DashboardViewState<'_>,
) {
    // If ultra-narrow stacked mode gives us a wide horizontal strip (avail_h < 220 && col_w > 380),
    // place Recording and Mirror Quality side-by-side so nothing clips!
    let side_by_side = avail_h < 230.0 && col_w >= 380.0;
    let gap = 12.0;

    let (rec_x, rec_y, rec_w, rec_h, qual_x, qual_y, qual_w, qual_h) = if side_by_side {
        let half_w = (col_w - gap) * 0.5;
        (col_x, top_y, half_w, avail_h, col_x + half_w + gap, top_y, half_w, avail_h)
    } else {
        let scale = (avail_h / 396.0).min(1.0);
        let r_h = (206.0 * scale).max(114.0);
        let q_h = (114.0 * scale).max(80.0);
        (col_x, top_y, col_w, r_h, col_x, top_y + r_h + gap, col_w, q_h)
    };

    let rec_cx = rec_x + rec_w * 0.5;
    let qual_cx = qual_x + qual_w * 0.5;

    // ========================================================================
    // Card 1: Dedicated Recording Controls
    // ========================================================================
    canvas.draw_rounded_card(
        rec_x,
        rec_y,
        rec_w,
        rec_h,
        14.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let compact_rec = rec_h < 170.0;
    let rec_hdr_cy = rec_y + if compact_rec { 22.0 } else { 28.0 };
    canvas.draw_circle(rec_x + 20.0, rec_hdr_cy, 4.5, ERROR_RED, 1.0);

    let hdr_cap = canvas.cap_height(14.5, FontWeight::Semibold);
    canvas.draw_text_baseline(
        "Recording",
        rec_x + 32.0,
        rec_hdr_cy + hdr_cap * 0.5,
        14.5,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

    let (badge_label, badge_bg, badge_fg) = if state.is_recording {
        ("REC", ERROR_BG, ERROR_RED)
    } else if state.settings.auto_record {
        ("Auto", APPLE_BLUE_SOFT, APPLE_BLUE)
    } else {
        ("Off", BADGE_OFF_BG, TEXT_SECONDARY)
    };
    let badge_w = 46.0;
    let badge_h = 24.0;
    let badge_x = rec_x + rec_w - badge_w - 14.0;
    let badge_y = rec_hdr_cy - badge_h * 0.5;
    canvas.draw_rounded_card(
        badge_x,
        badge_y,
        badge_w,
        badge_h,
        12.0,
        badge_bg,
        badge_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    let bw_txt = canvas.measure_text_width(badge_label, 11.5, FontWeight::Semibold);
    let b_cap = canvas.cap_height(11.5, FontWeight::Semibold);
    canvas.draw_text_baseline(
        badge_label,
        badge_x + (badge_w - bw_txt) * 0.5,
        badge_y + (badge_h + b_cap) * 0.5,
        11.5,
        FontWeight::Semibold,
        badge_fg,
        1.0,
    );

    // Tabular Timer: "00:00:00"
    let timer_str = format_hh_mm_ss(if state.is_recording {
        state.rec_elapsed_secs
    } else {
        0
    });
    let timer_sz = if compact_rec { 22.0 } else { 27.0 };
    let timer_y = rec_y + (rec_h * 0.28);
    canvas.draw_text_centered(
        &timer_str,
        rec_cx,
        timer_y,
        timer_sz,
        FontWeight::Bold,
        if state.is_recording {
            ERROR_RED
        } else {
            TEXT_PRIMARY
        },
        1.0,
    );

    // Status subtext below timer
    if !compact_rec {
        let rec_subtext = if state.is_recording {
            "Recording MP4 in progress..."
        } else if let Some(saved) = state.last_saved_recording {
            saved
        } else if state.settings.auto_record {
            "Auto-record armed for mirror"
        } else {
            "No recording in progress"
        };
        let fitted_rec_sub =
            canvas.fit_text_to_width(rec_subtext, rec_w - 28.0, 12.0, FontWeight::Regular);
        canvas.draw_text_centered(
            &fitted_rec_sub,
            rec_cx,
            timer_y + 38.0,
            12.0,
            FontWeight::Regular,
            TEXT_SECONDARY,
            1.0,
        );
    }

    // Action Button: Start / Stop Recording (Red used strictly for recording control)
    let btn_h = if compact_rec { 36.0 } else { 42.0 };
    let rec_btn_rect = SDL_FRect {
        x: rec_x + 14.0,
        y: rec_y + rec_h - btn_h - 16.0,
        w: rec_w - 28.0,
        h: btn_h,
    };
    let rec_btn_hover = point_in_rect(mx, my, &rec_btn_rect);
    let (rec_btn_fill, rec_btn_border, rec_btn_fg) = if state.is_recording {
        let fill = if rec_btn_hover {
            Rgb(195, 30, 30)
        } else {
            ERROR_RED
        };
        (fill, fill, CARD_WHITE)
    } else {
        let fill = if rec_btn_hover {
            ERROR_BG_HOVER
        } else {
            ERROR_BG
        };
        (fill, Rgb(245, 198, 204), ERROR_RED)
    };
    canvas.draw_rounded_card(
        rec_btn_rect.x,
        rec_btn_rect.y,
        rec_btn_rect.w,
        rec_btn_rect.h,
        btn_h * 0.5,
        rec_btn_fill,
        rec_btn_border,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let rec_btn_label = if state.is_recording {
        "Stop Recording"
    } else if !state.is_mirroring && state.settings.auto_record {
        "Auto-Record: On"
    } else {
        "Start Recording"
    };
    let rbl_w = canvas.measure_text_width(rec_btn_label, 13.5, FontWeight::Semibold);
    let rbl_cap = canvas.cap_height(13.5, FontWeight::Semibold);
    let rbl_total_w = 16.0 + rbl_w;
    let rbl_start_x = rec_btn_rect.x + (rec_btn_rect.w - rbl_total_w) * 0.5;
    let rbl_cy = rec_btn_rect.y + rec_btn_rect.h * 0.5;

    if state.is_recording {
        canvas.draw_rounded_card(
            rbl_start_x,
            rbl_cy - 4.5,
            9.0,
            9.0,
            2.0,
            CARD_WHITE,
            CARD_WHITE,
            0.0,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
    } else {
        canvas.draw_circle(rbl_start_x + 4.5, rbl_cy, 4.5, ERROR_RED, 1.0);
    }
    canvas.draw_text_baseline(
        rec_btn_label,
        rbl_start_x + 16.0,
        rbl_cy + rbl_cap * 0.5,
        13.5,
        FontWeight::Semibold,
        rec_btn_fg,
        1.0,
    );

    hit_zones.push(HitZone {
        rect: rec_btn_rect,
        action: UiAction::ToggleRecord,
    });

    // ========================================================================
    // Card 2: Compact Mirror Quality Control (Auto | HD | FHD)
    // ========================================================================
    canvas.draw_rounded_card(
        qual_x,
        qual_y,
        qual_w,
        qual_h,
        14.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let qual_hdr_cy = qual_y + 22.0;
    canvas.draw_monitor_icon(qual_x + 22.0, qual_hdr_cy, TEXT_BODY, 1.0);
    let qual_hdr_cap = canvas.cap_height(13.8, FontWeight::Semibold);
    canvas.draw_text_baseline(
        "Mirror Quality",
        qual_x + 36.0,
        qual_hdr_cy + qual_hdr_cap * 0.5,
        13.8,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

    // Compact 3-Way Segmented Control: [ Auto | HD | FHD ]
    let seg_x = qual_x + 14.0;
    let seg_h = if qual_h < 96.0 { 30.0 } else { 36.0 };
    let seg_y = qual_y + (qual_h - seg_h) * 0.52;
    let seg_w = qual_w - 28.0;
    canvas.draw_rounded_card(
        seg_x,
        seg_y,
        seg_w,
        seg_h,
        seg_h * 0.5,
        PILL_NEUTRAL,
        BORDER_SUBTLE,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let item_w = seg_w / 3.0;
    let qualities = [MirrorQuality::Auto, MirrorQuality::Hd, MirrorQuality::Fhd];
    for (idx, q) in qualities.iter().copied().enumerate() {
        let q_rect = SDL_FRect {
            x: seg_x + idx as f32 * item_w,
            y: seg_y,
            w: item_w,
            h: seg_h,
        };
        let is_sel = state.settings.quality == q;
        let is_hov = point_in_rect(mx, my, &q_rect);

        if is_sel {
            canvas.draw_rounded_card(
                q_rect.x + 2.0,
                q_rect.y + 2.0,
                q_rect.w - 4.0,
                q_rect.h - 4.0,
                (seg_h - 4.0) * 0.5,
                APPLE_BLUE,
                APPLE_BLUE,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        } else if is_hov {
            canvas.draw_rounded_card(
                q_rect.x + 2.0,
                q_rect.y + 2.0,
                q_rect.w - 4.0,
                q_rect.h - 4.0,
                (seg_h - 4.0) * 0.5,
                PILL_NEUTRAL_HOVER,
                PILL_NEUTRAL_HOVER,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        }

        let lbl = q.label();
        let weight = if is_sel {
            FontWeight::Semibold
        } else {
            FontWeight::Regular
        };
        let fg = if is_sel { CARD_WHITE } else { TEXT_BODY };
        let lw = canvas.measure_text_width(lbl, 12.8, weight);
        let l_cap = canvas.cap_height(12.8, weight);
        canvas.draw_text_baseline(
            lbl,
            q_rect.x + (q_rect.w - lw) * 0.5,
            q_rect.y + (q_rect.h + l_cap) * 0.5,
            12.8,
            weight,
            fg,
            1.0,
        );

        hit_zones.push(HitZone {
            rect: q_rect,
            action: UiAction::SelectQuality(q),
        });
    }

    // Small secondary text displaying resolution and FPS info
    let res_label = match state.settings.quality {
        MirrorQuality::Auto => {
            if state.is_mirroring && state.stream_width > 0 && state.stream_height > 0 {
                format!(
                    "{}×{} • {} FPS • {} Mbps",
                    state.stream_width, state.stream_height, state.fps, state.settings.bitrate_mbps
                )
            } else {
                format!(
                    "Native • {} FPS • {} Mbps",
                    state.settings.target_fps, state.settings.bitrate_mbps
                )
            }
        }
        MirrorQuality::Hd => format!(
            "720p • {} FPS • Low Latency",
            state.settings.target_fps
        ),
        MirrorQuality::Fhd => format!(
            "1080p+ • {} FPS • {} Mbps",
            state.settings.target_fps, state.settings.bitrate_mbps
        ),
    };
    let q_caption = canvas.fit_text_to_width(&res_label, qual_w - 24.0, 11.8, FontWeight::Regular);
    canvas.draw_text_centered(
        &q_caption,
        qual_cx,
        qual_y + qual_h - 24.0,
        11.8,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // ========================================================================
    // Card 3: Slim 1-Line AirPlay Toggle Card (Option 1)
    // ========================================================================
    if !side_by_side {
        let ap_y = qual_y + qual_h + gap;
        let ap_h = (top_y + avail_h - ap_y).min(48.0);
        if ap_h >= 34.0 {
            let ap_rect = SDL_FRect {
                x: col_x,
                y: ap_y,
                w: col_w,
                h: ap_h,
            };
            let ap_hov = point_in_rect(mx, my, &ap_rect);
            let ap_bg = if ap_hov {
                Rgb(248, 250, 254)
            } else {
                CARD_WHITE
            };
            canvas.draw_rounded_card(
                ap_rect.x,
                ap_rect.y,
                ap_rect.w,
                ap_rect.h,
                14.0,
                ap_bg,
                CARD_BORDER,
                1.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );

            let ap_cy = ap_y + ap_h * 0.5;
            let ap_on = state.settings.airplay_enabled;
            let wifi_col = if ap_on { APPLE_BLUE } else { TEXT_SECONDARY };
            canvas.draw_wifi_icon(col_x + 22.0, ap_cy, wifi_col, 1.0);

            let ap_cap = canvas.cap_height(13.5, FontWeight::Semibold);
            canvas.draw_text_baseline(
                "AirPlay",
                col_x + 38.0,
                ap_cy + ap_cap * 0.5,
                13.5,
                FontWeight::Semibold,
                TEXT_PRIMARY,
                1.0,
            );

            canvas.draw_toggle_switch(
                col_x + col_w - 54.0,
                ap_cy - 11.0,
                40.0,
                22.0,
                ap_on,
                if ap_hov { 1.0 } else { 0.0 },
            );

            hit_zones.push(HitZone {
                rect: ap_rect,
                action: UiAction::ToggleAirPlay,
            });
        }
    }
}

/// Renders the Center Card when on `NavTab::Home` (Hero Illustration + Title + "Start Mirroring" + Bottom USB Banner).
#[allow(clippy::too_many_arguments)]
fn draw_center_home_hero_view(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    card_x: f32,
    card_y: f32,
    card_w: f32,
    card_h: f32,
    state: &DashboardViewState<'_>,
) {
    let cx = card_x + card_w * 0.5;
    let pad_x = (card_w * 0.05).clamp(14.0, 28.0);
    let pad_y = (card_h * 0.03).clamp(10.0, 20.0);

    // 1. Bottom Connection & AirPlay Bar (anchored cleanly at bottom of center card)
    let banner_h = (card_h * 0.13).clamp(54.0, 72.0);
    let banner_w = (card_w - pad_x * 2.0).max(200.0);
    let banner_x = (cx - banner_w * 0.5).round();
    let banner_y = (card_y + card_h - banner_h - pad_y).round();

    // 2. Dynamically size and vertically center the hero stack above the bottom bar
    let avail_hero_h = (banner_y - card_y - 12.0).max(160.0);
    let hero_size = (avail_hero_h * 0.30).clamp(52.0, 120.0);
    let title_sz = if avail_hero_h < 250.0 { 20.0 } else { 26.0 };
    let sub_sz = if avail_hero_h < 250.0 { 12.5 } else { 14.0 };
    let btn_h = (avail_hero_h * 0.12).clamp(36.0, 48.0);
    let btn_w = (card_w * 0.44).clamp(176.0, 232.0);

    let gap1 = (avail_hero_h * 0.045).clamp(6.0, 16.0);
    let gap2 = (avail_hero_h * 0.025).clamp(4.0, 10.0);
    let gap3 = (avail_hero_h * 0.065).clamp(10.0, 24.0);

    let total_stack_h = hero_size + gap1 + title_sz + gap2 + sub_sz + gap3 + btn_h;
    let top_y = (card_y + (avail_hero_h - total_stack_h) * 0.48).max(card_y + 10.0);

    // Hero Logo
    canvas.draw_hero_illustration_sized(cx, top_y, hero_size, state.is_connecting);

    // Application Title
    let title_y = top_y + hero_size + gap1;
    canvas.draw_text_centered(
        "Mobile Mirror",
        cx,
        title_y,
        title_sz,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    // Short Description
    let sub_y = title_y + title_sz + gap2;
    let subtitle = "Mirror and record your phone screen on your laptop";
    let fitted_sub = canvas.fit_text_to_width(subtitle, card_w - 36.0, sub_sz, FontWeight::Regular);
    canvas.draw_text_centered(
        &fitted_sub,
        cx,
        sub_y,
        sub_sz,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // Primary Blue Pill Button: "[ ▶  Start Mirroring ]"
    let btn_y = (sub_y + sub_sz + gap3).round();
    let btn_rect = SDL_FRect {
        x: (cx - btn_w * 0.5).round(),
        y: btn_y,
        w: btn_w,
        h: btn_h,
    };
    let btn_hover = point_in_rect(mx, my, &btn_rect);
    let btn_fill = if btn_hover {
        BLUE_GRAD_HOVER_TOP
    } else {
        APPLE_BLUE
    };

    canvas.draw_rounded_card(
        btn_rect.x,
        btn_rect.y,
        btn_rect.w,
        btn_rect.h,
        btn_h * 0.5,
        btn_fill,
        btn_fill,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let btn_label = if state.is_connecting {
        "Connecting..."
    } else {
        "Start Mirroring"
    };
    let btn_font_sz = if btn_h < 42.0 { 13.5 } else { 14.8 };
    let bl_w = canvas.measure_text_width(btn_label, btn_font_sz, FontWeight::Semibold);
    let bl_cap = canvas.cap_height(btn_font_sz, FontWeight::Semibold);
    let total_w = 20.0 + bl_w;
    let start_x = btn_rect.x + (btn_rect.w - total_w) * 0.5;
    let btn_cy = btn_rect.y + btn_rect.h * 0.5;

    canvas.draw_play_icon(start_x + 5.0, btn_cy, 10.5, CARD_WHITE, 1.0);
    canvas.draw_text_baseline(
        btn_label,
        start_x + 20.0,
        btn_cy + bl_cap * 0.5,
        btn_font_sz,
        FontWeight::Semibold,
        CARD_WHITE,
        1.0,
    );

    hit_zones.push(HitZone {
        rect: btn_rect,
        action: UiAction::StartMirroring,
    });

    // 3. Draw Bottom Connection Banner
    canvas.draw_rounded_card(
        banner_x,
        banner_y,
        banner_w,
        banner_h,
        12.0,
        BANNER_USB_BG,
        BANNER_USB_BORDER,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let b_cy = banner_y + banner_h * 0.5;
    canvas.draw_phone_outline_icon(banner_x + 24.0, b_cy, TEXT_BODY, 1.0);

    // Right "Setup guide" link if banner is wide enough
    let mut right_limit = banner_x + banner_w - 14.0;
    if banner_w >= 340.0 {
        let link_label = "Setup guide";
        let link_w = canvas.measure_text_width(link_label, 12.0, FontWeight::Semibold);
        let link_rect = SDL_FRect {
            x: banner_x + banner_w - link_w - 22.0,
            y: b_cy - 14.0,
            w: link_w + 16.0,
            h: 28.0,
        };
        let link_hover = point_in_rect(mx, my, &link_rect);
        let link_col = if link_hover {
            BLUE_GRAD_HOVER_TOP
        } else {
            APPLE_BLUE
        };
        let link_cap = canvas.cap_height(12.0, FontWeight::Semibold);
        canvas.draw_text_baseline(
            link_label,
            link_rect.x + 2.0,
            b_cy + link_cap * 0.5,
            12.0,
            FontWeight::Semibold,
            link_col,
            1.0,
        );
        hit_zones.push(HitZone {
            rect: link_rect,
            action: UiAction::OpenHelpInSettings,
        });
        right_limit = link_rect.x - 8.0;
    }

    // Middle connection state & instructions
    let text_x = banner_x + 48.0;
    let max_text_w = (right_limit - text_x).max(90.0);

    let (title_text, title_color, sub_text) = if state.is_connecting {
        (
            state.headline.to_string(),
            APPLE_BLUE,
            state.subtext.to_string(),
        )
    } else if state.is_warning {
        (
            state.headline.to_string(),
            WARNING_AMBER,
            state.subtext.to_string(),
        )
    } else if let Some(serial) = state.device_serial {
        let plat = state.platform_label.unwrap_or("USB");
        (
            format!("Ready • {} ({})", plat, serial),
            LIVE_GREEN,
            "Click Start Mirroring above to begin".to_string(),
        )
    } else {
        (
            "Connect your Android or iPhone".to_string(),
            TEXT_PRIMARY,
            if state.settings.airplay_enabled {
                "Use USB cable or connect via Wi-Fi (AirPlay Ready)".to_string()
            } else {
                "Use USB cable or enable AirPlay for Wi-Fi".to_string()
            },
        )
    };

    let line1 = canvas.fit_text_to_width(&title_text, max_text_w, 13.5, FontWeight::Semibold);
    let line2 = canvas.fit_text_to_width(&sub_text, max_text_w, 11.8, FontWeight::Regular);

    let line1_y = banner_y + (banner_h - 34.0) * 0.5;
    canvas.draw_text(
        &line1,
        text_x,
        line1_y,
        13.5,
        FontWeight::Semibold,
        title_color,
        1.0,
    );
    canvas.draw_text(
        &line2,
        text_x,
        line1_y + 20.0,
        11.8,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );
}

/// Renders the Live Mirroring stage inside the Center Card when an Android stream is active.
#[allow(clippy::too_many_arguments)]
fn draw_center_live_mirror_view(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    card_x: f32,
    card_y: f32,
    card_w: f32,
    card_h: f32,
    state: &DashboardViewState<'_>,
) -> SDL_FRect {
    // Top control strip inside Center Card
    let bar_h = 52.0;
    let bar_cy = card_y + bar_h * 0.5;

    canvas.draw_circle(card_x + 24.0, bar_cy, 5.0, LIVE_GREEN, 1.0);
    let meta = if let Some(plat) = state.platform_label {
        format!(
            "Live ({})  •  {}×{}  •  {} FPS",
            plat, state.stream_width, state.stream_height, state.fps
        )
    } else {
        format!(
            "Live Mirror  •  {}×{}  •  {} FPS",
            state.stream_width, state.stream_height, state.fps
        )
    };

    // Right control pills inside Center Card header: [Mic] [Compact] [F11 Full] [Stop]
    let stop_rect = SDL_FRect {
        x: card_x + card_w - 86.0,
        y: card_y + 11.0,
        w: 70.0,
        h: 30.0,
    };
    let f11_rect = SDL_FRect {
        x: stop_rect.x - 94.0,
        y: card_y + 11.0,
        w: 88.0,
        h: 30.0,
    };
    let full_rect = SDL_FRect {
        x: f11_rect.x - 86.0,
        y: card_y + 11.0,
        w: 80.0,
        h: 30.0,
    };
    let mic_rect = SDL_FRect {
        x: full_rect.x - 86.0,
        y: card_y + 11.0,
        w: 80.0,
        h: 30.0,
    };

    let meta_fit = canvas.fit_text_to_width(
        &meta,
        (mic_rect.x - (card_x + 38.0) - 10.0).max(80.0),
        13.8,
        FontWeight::Semibold,
    );
    let m_cap = canvas.cap_height(13.8, FontWeight::Semibold);
    canvas.draw_text_baseline(
        &meta_fit,
        card_x + 38.0,
        bar_cy + m_cap * 0.5,
        13.8,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

    // Mic pill
    let mic_bg = if state.settings.mic_enabled {
        APPLE_BLUE_SOFT
    } else if point_in_rect(mx, my, &mic_rect) {
        PILL_NEUTRAL_HOVER
    } else {
        PILL_NEUTRAL
    };
    let mic_fg = if state.settings.mic_enabled {
        APPLE_BLUE
    } else {
        TEXT_PRIMARY
    };
    let mic_lbl = if state.settings.mic_enabled {
        "Mic: On"
    } else {
        "Mic: Off"
    };
    draw_small_pill(canvas, &mic_rect, mic_lbl, mic_bg, mic_fg);
    hit_zones.push(HitZone {
        rect: mic_rect,
        action: UiAction::ToggleMic,
    });

    // Compact Phone Window pill
    let full_bg = if point_in_rect(mx, my, &full_rect) {
        PILL_NEUTRAL_HOVER
    } else {
        PILL_NEUTRAL
    };
    draw_small_pill(canvas, &full_rect, "Expand", full_bg, TEXT_PRIMARY);
    hit_zones.push(HitZone {
        rect: full_rect,
        action: UiAction::ToggleFullscreenMirror,
    });

    // True Fullscreen (F11) pill
    let f11_bg = if point_in_rect(mx, my, &f11_rect) {
        APPLE_BLUE_HOVER
    } else {
        APPLE_BLUE_SOFT
    };
    draw_small_pill(canvas, &f11_rect, "F11 Full", f11_bg, APPLE_BLUE);
    hit_zones.push(HitZone {
        rect: f11_rect,
        action: UiAction::ToggleWindowFullscreen,
    });

    // Disconnect pill
    let stop_bg = if point_in_rect(mx, my, &stop_rect) {
        PILL_NEUTRAL_HOVER
    } else {
        PILL_NEUTRAL
    };
    draw_small_pill(canvas, &stop_rect, "Stop", stop_bg, TEXT_PRIMARY);
    hit_zones.push(HitZone {
        rect: stop_rect,
        action: UiAction::StopMirroring,
    });

    // Dark obsidian stage inside Center Workspace for the live GPU video frame
    let stage = SDL_FRect {
        x: card_x + 12.0,
        y: card_y + bar_h,
        w: card_w - 24.0,
        h: card_h - bar_h - 12.0,
    };
    canvas.draw_rounded_card(
        stage.x,
        stage.y,
        stage.w,
        stage.h,
        10.0,
        Rgb(12, 16, 26),
        BORDER_SUBTLE,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    stage
}

/// Renders the functional `Recordings` Gallery inside the Center Card.
/// Displays recorded videos in an orderly, responsive Bento grid with filmstrip
/// thumbnail previews (Image 2 style), video metadata, and Play/Show/Delete actions.
#[allow(clippy::too_many_arguments)]
fn draw_center_recordings_view(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    card_x: f32,
    card_y: f32,
    card_w: f32,
    card_h: f32,
    state: &DashboardViewState<'_>,
) {
    // Header
    canvas.draw_text(
        "Saved Recordings",
        card_x + 28.0,
        card_y + 24.0,
        20.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );
    let sub = format!(
        "{} MP4 video file(s) in recordings/ folder (Zero-Copy Lossless H.264 + AAC)",
        state.recordings.len()
    );
    let fitted_sub = canvas.fit_text_to_width(&sub, card_w - 320.0, 12.8, FontWeight::Regular);
    canvas.draw_text(
        &fitted_sub,
        card_x + 28.0,
        card_y + 54.0,
        12.8,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // Top-right "[ Open Folder ]" button
    let open_rect = SDL_FRect {
        x: card_x + card_w - 146.0,
        y: card_y + 24.0,
        w: 122.0,
        h: 36.0,
    };
    let open_bg = if point_in_rect(mx, my, &open_rect) {
        APPLE_BLUE_HOVER
    } else {
        APPLE_BLUE_SOFT
    };
    draw_small_pill(canvas, &open_rect, "Open Folder", open_bg, APPLE_BLUE);
    hit_zones.push(HitZone {
        rect: open_rect,
        action: UiAction::OpenRecordingsFolder,
    });

    // Divider line below header
    canvas.fill_rect(
        (card_x + 24.0) as i32,
        (card_y + 86.0) as i32,
        (card_w - 48.0) as i32,
        1,
        BORDER_SUBTLE,
    );

    if state.recordings.is_empty() {
        let cx = card_x + card_w * 0.5;
        let cy = card_y + card_h * 0.5;
        canvas.draw_circle(cx, cy - 34.0, 32.0, PILL_NEUTRAL, 1.0);
        canvas.draw_video_cam_icon(cx, cy - 34.0, APPLE_BLUE, 1.0);
        canvas.draw_text_centered(
            "No recordings saved yet",
            cx,
            cy + 12.0,
            16.5,
            FontWeight::Semibold,
            TEXT_PRIMARY,
            1.0,
        );
        let empty_hint = canvas.fit_text_to_width(
            "Click 'Start Recording' on the right panel while mirroring to record lossless MP4 video.",
            card_w - 48.0,
            13.2,
            FontWeight::Regular,
        );
        canvas.draw_text_centered(
            &empty_hint,
            cx,
            cy + 42.0,
            13.2,
            FontWeight::Regular,
            TEXT_SECONDARY,
            1.0,
        );
        return;
    }

    // Responsive Orderly Grid measurements
    let grid_x = card_x + 20.0;
    let grid_y = card_y + 96.0;
    let grid_w = (card_w - 40.0).max(160.0);
    let grid_h = (card_h - 108.0).max(160.0);

    let cols = if grid_w >= 800.0 {
        3
    } else if grid_w >= 430.0 {
        2
    } else {
        1
    };
    let gap_x = 14.0;
    let gap_y = 14.0;
    let col_w = (grid_w - (cols as f32 - 1.0) * gap_x) / cols as f32;
    let card_item_h = 246.0_f32.min(grid_h);

    let rows = ((grid_h + gap_y) / (card_item_h + gap_y)).floor().max(1.0) as usize;
    let page_size = cols * rows;
    let total_pages = (state.recordings.len() + page_size - 1) / page_size;
    let current_page = state.recordings_page.min(total_pages.saturating_sub(1));

    // Pagination controls in top-right header if recordings span multiple pages
    if total_pages > 1 {
        let prev_rect = SDL_FRect {
            x: open_rect.x - 146.0,
            y: card_y + 24.0,
            w: 32.0,
            h: 36.0,
        };
        let next_rect = SDL_FRect {
            x: open_rect.x - 42.0,
            y: card_y + 24.0,
            w: 32.0,
            h: 36.0,
        };
        let page_label = format!("{}/{}", current_page + 1, total_pages);
        let pl_cx = prev_rect.x + 32.0 + (next_rect.x - (prev_rect.x + 32.0)) * 0.5;

        let prev_bg = if point_in_rect(mx, my, &prev_rect) {
            PILL_NEUTRAL_HOVER
        } else {
            PILL_NEUTRAL
        };
        draw_small_pill(canvas, &prev_rect, "<", prev_bg, TEXT_PRIMARY);
        hit_zones.push(HitZone {
            rect: prev_rect,
            action: UiAction::PrevRecordingsPage,
        });

        canvas.draw_text_centered(
            &page_label,
            pl_cx,
            card_y + 35.0,
            12.5,
            FontWeight::Semibold,
            TEXT_PRIMARY,
            1.0,
        );

        let next_bg = if point_in_rect(mx, my, &next_rect) {
            PILL_NEUTRAL_HOVER
        } else {
            PILL_NEUTRAL
        };
        draw_small_pill(canvas, &next_rect, ">", next_bg, TEXT_PRIMARY);
        hit_zones.push(HitZone {
            rect: next_rect,
            action: UiAction::NextRecordingsPage,
        });
    }

    let start_idx = current_page * page_size;
    let visible_entries = state.recordings.iter().skip(start_idx).take(page_size);

    for (idx, entry) in visible_entries.enumerate() {
        let row = idx / cols;
        let col = idx % cols;
        let rx = grid_x + col as f32 * (col_w + gap_x);
        let ry = grid_y + row as f32 * (card_item_h + gap_y);
        let rw = col_w;
        let rh = card_item_h;

        let card_rect = SDL_FRect { x: rx, y: ry, w: rw, h: rh };
        let is_card_hovered = point_in_rect(mx, my, &card_rect);

        // Elevated Bento card container
        let card_border = if is_card_hovered {
            APPLE_BLUE_HOVER
        } else {
            CARD_BORDER
        };
        canvas.draw_rounded_card(
            rx,
            ry,
            rw,
            rh,
            10.0,
            CARD_WHITE,
            card_border,
            1.0,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );

        // 1. Filmstrip Video Frame Preview (Screenshot 2 style!)
        let thumb_w = rw - 20.0;
        let thumb_h = 136.0;
        let thumb_x = rx + 10.0;
        let thumb_y = ry + 10.0;
        let thumb_rect = SDL_FRect {
            x: thumb_x,
            y: thumb_y,
            w: thumb_w,
            h: thumb_h,
        };
        let is_thumb_hovered = point_in_rect(mx, my, &thumb_rect);

        canvas.draw_filmstrip_frame(
            thumb_x,
            thumb_y,
            thumb_w,
            thumb_h,
            state.thumbnail_for(&entry.full_path),
            is_thumb_hovered,
        );

        // Clicking preview directly plays the video
        hit_zones.push(HitZone {
            rect: thumb_rect,
            action: UiAction::PlayRecording(entry.full_path.clone()),
        });

        // 2. Video Title below preview
        let title_fit =
            canvas.fit_text_to_width(&entry.file_name, rw - 24.0, 13.5, FontWeight::Semibold);
        canvas.draw_text(
            &title_fit,
            rx + 12.0,
            ry + 158.0,
            13.5,
            FontWeight::Semibold,
            TEXT_PRIMARY,
            1.0,
        );

        // 3. Metadata (Size & Format)
        let meta_txt = format!("{}  •  MP4 (H.264 + AAC)", entry.formatted_size());
        let meta_fit = canvas.fit_text_to_width(&meta_txt, rw - 24.0, 11.8, FontWeight::Regular);
        canvas.draw_text(
            &meta_fit,
            rx + 12.0,
            ry + 179.0,
            11.8,
            FontWeight::Regular,
            TEXT_SECONDARY,
            1.0,
        );

        // 4. Action buttons: [Play] [Show] [Delete]
        let btn_y = ry + 204.0;
        let btn_h = 30.0;
        let total_btn_w = rw - 24.0;
        let btn_gap = 6.0;
        let play_w = ((total_btn_w - btn_gap * 2.0) * 0.40).round();
        let show_w = ((total_btn_w - btn_gap * 2.0) * 0.30).round();
        let del_w = total_btn_w - play_w - show_w - btn_gap * 2.0;

        let play_rect = SDL_FRect {
            x: rx + 12.0,
            y: btn_y,
            w: play_w,
            h: btn_h,
        };
        let show_rect = SDL_FRect {
            x: play_rect.x + play_w + btn_gap,
            y: btn_y,
            w: show_w,
            h: btn_h,
        };
        let del_rect = SDL_FRect {
            x: show_rect.x + show_w + btn_gap,
            y: btn_y,
            w: del_w,
            h: btn_h,
        };

        let play_bg = if point_in_rect(mx, my, &play_rect) {
            BLUE_GRAD_HOVER_TOP
        } else {
            APPLE_BLUE
        };
        draw_small_pill(canvas, &play_rect, "Play", play_bg, CARD_WHITE);
        hit_zones.push(HitZone {
            rect: play_rect,
            action: UiAction::PlayRecording(entry.full_path.clone()),
        });

        let show_bg = if point_in_rect(mx, my, &show_rect) {
            PILL_NEUTRAL_HOVER
        } else {
            PILL_NEUTRAL
        };
        draw_small_pill(canvas, &show_rect, "Show", show_bg, TEXT_PRIMARY);
        hit_zones.push(HitZone {
            rect: show_rect,
            action: UiAction::RevealRecording(entry.full_path.clone()),
        });

        let del_bg = if point_in_rect(mx, my, &del_rect) {
            PILL_NEUTRAL_HOVER
        } else {
            PILL_NEUTRAL
        };
        draw_small_pill(canvas, &del_rect, "Delete", del_bg, TEXT_BODY);
        hit_zones.push(HitZone {
            rect: del_rect,
            action: UiAction::DeleteRecording(entry.full_path.clone()),
        });
    }
}

/// Renders the interactive `Settings` panel inside the Center Card where all settings are functional.
#[allow(clippy::too_many_arguments)]
fn draw_center_settings_view(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    card_x: f32,
    card_y: f32,
    card_w: f32,
    card_h: f32,
    state: &DashboardViewState<'_>,
    section: SettingsSection,
) {
    let rx = card_x + 20.0;
    let rw = card_w - 40.0;

    canvas.draw_text(
        "Settings",
        rx,
        card_y + 16.0,
        20.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    // Compact 2-Tab Switcher: [ Preferences | Help & About ]
    let tab_w = 224.0;
    let tab_h = 32.0;
    let tab_x = rx;
    let tab_y = card_y + 48.0;
    canvas.draw_rounded_card(
        tab_x,
        tab_y,
        tab_w,
        tab_h,
        8.0,
        PILL_NEUTRAL,
        BORDER_SUBTLE,
        1.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );

    let sections = [
        (SettingsSection::Preferences, "Preferences"),
        (SettingsSection::HelpAbout, "Help & About"),
    ];
    let half_w = tab_w * 0.5;
    for (idx, (sec, lbl)) in sections.into_iter().enumerate() {
        let s_rect = SDL_FRect {
            x: tab_x + idx as f32 * half_w,
            y: tab_y,
            w: half_w,
            h: tab_h,
        };
        let is_sel = section == sec;
        let is_hov = point_in_rect(mx, my, &s_rect);
        if is_sel {
            canvas.draw_rounded_card(
                s_rect.x + 2.0,
                s_rect.y + 2.0,
                s_rect.w - 4.0,
                s_rect.h - 4.0,
                6.0,
                APPLE_BLUE,
                APPLE_BLUE,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        } else if is_hov {
            canvas.draw_rounded_card(
                s_rect.x + 2.0,
                s_rect.y + 2.0,
                s_rect.w - 4.0,
                s_rect.h - 4.0,
                6.0,
                PILL_NEUTRAL_HOVER,
                PILL_NEUTRAL_HOVER,
                0.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
        }
        let weight = if is_sel {
            FontWeight::Semibold
        } else {
            FontWeight::Regular
        };
        let fg = if is_sel { CARD_WHITE } else { TEXT_BODY };
        let lw = canvas.measure_text_width(lbl, 12.4, weight);
        let l_cap = canvas.cap_height(12.4, weight);
        canvas.draw_text_baseline(
            lbl,
            s_rect.x + (s_rect.w - lw) * 0.5,
            s_rect.y + (s_rect.h + l_cap) * 0.5,
            12.4,
            weight,
            fg,
            1.0,
        );
        hit_zones.push(HitZone {
            rect: s_rect,
            action: UiAction::SwitchSettingsSection(sec),
        });
    }

    match section {
        SettingsSection::Preferences => {
            let reset_rect = SDL_FRect {
                x: rx + rw - 122.0,
                y: tab_y,
                w: 122.0,
                h: 32.0,
            };
            let reset_bg = if point_in_rect(mx, my, &reset_rect) {
                PILL_NEUTRAL_HOVER
            } else {
                PILL_NEUTRAL
            };
            draw_small_pill(canvas, &reset_rect, "Reset Defaults", reset_bg, TEXT_BODY);
            hit_zones.push(HitZone {
                rect: reset_rect,
                action: UiAction::ResetDefaultSettings,
            });

            let s = state.settings;
            let fps_val = format!("{} FPS", s.target_fps);
            let br_val = format!(
                "{} Mbps ({})",
                s.bitrate_mbps,
                if s.cbr_mode { "CBR" } else { "VBR" }
            );
            let cbr_val = if s.cbr_mode {
                "Constant (CBR)"
            } else {
                "Variable (VBR)"
            };

            #[allow(clippy::type_complexity)]
            let rows: [(&str, &str, Option<bool>, String, UiAction); 10] = [
                (
                    "Target Frame Rate",
                    "Hardware encoder capture frame rate (30 / 60 / 90 / 120 FPS)",
                    None,
                    fps_val,
                    UiAction::CycleFps,
                ),
                (
                    "Video Encoder Bitrate",
                    "H.264 stream bitrate over USB (12 / 16 / 24 / 36 / 48 Mbps)",
                    None,
                    br_val,
                    UiAction::CycleBitrate,
                ),
                (
                    "Bitrate Control Mode",
                    "Constant Bitrate (CBR) for lowest latency vs Variable (VBR)",
                    None,
                    cbr_val.to_string(),
                    UiAction::ToggleCbr,
                ),
                (
                    "Phone Microphone (Voice Isolation DSP)",
                    "Capture voice-isolated phone mic audio alongside game sound",
                    Some(s.mic_enabled),
                    String::new(),
                    UiAction::ToggleMic,
                ),
                (
                    "PC Speaker Audio Playback",
                    "Play phone & game audio live through your laptop speakers",
                    Some(s.pc_audio_enabled),
                    String::new(),
                    UiAction::TogglePcAudio,
                ),
                (
                    "Auto-Connect on USB Plug",
                    "Automatically start mirroring when an Android or iOS device is plugged in",
                    Some(s.auto_connect_usb),
                    String::new(),
                    UiAction::ToggleAutoConnect,
                ),
                (
                    "AirPlay Receiver (iOS Wireless)",
                    "Listen for iPhone/iPad Screen Mirroring over Wi-Fi ('Mobile Mirror')",
                    Some(s.airplay_enabled),
                    String::new(),
                    UiAction::ToggleAirPlay,
                ),
                (
                    "Auto-Record on Mirror Start",
                    "Automatically start lossless MP4 recording when mirroring begins",
                    Some(s.auto_record),
                    String::new(),
                    UiAction::ToggleAutoRecord,
                ),
                (
                    "Always on Top Window",
                    "Keep Mobile Mirror window pinned above other desktop apps",
                    Some(s.always_on_top),
                    String::new(),
                    UiAction::ToggleAlwaysOnTop,
                ),
                (
                    "Hardware Video Decoder",
                    "GPU decoding acceleration backend (NVDEC / D3D11VA / DXVA2)",
                    None,
                    s.hw_backend.label().to_string(),
                    UiAction::CycleHwBackend,
                ),
            ];

            let start_y = tab_y + tab_h + 12.0;
            let avail_h = (card_y + card_h - start_y - 10.0).max(260.0);
            let gap = if avail_h < 420.0 { 3.0 } else { 5.0 };
            let row_h = ((avail_h - gap * 9.0) / 10.0).clamp(24.0, 50.0);
            let compact_rows = row_h < 38.0;

            for (idx, (title, desc, toggle_state, badge_text, action)) in
                rows.into_iter().enumerate()
            {
                let ry = start_y + idx as f32 * (row_h + gap);
                let row_rect = SDL_FRect {
                    x: rx,
                    y: ry,
                    w: rw,
                    h: row_h,
                };
                let is_hov = point_in_rect(mx, my, &row_rect);
                let bg = if is_hov {
                    PILL_NEUTRAL_HOVER
                } else {
                    BANNER_USB_BG
                };
                canvas.draw_rounded_card(
                    rx,
                    ry,
                    rw,
                    row_h,
                    8.0,
                    bg,
                    CARD_BORDER,
                    1.0,
                    SHADOW_SLATE,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                );

                let right_reserve = if toggle_state.is_some() { 72.0 } else { 148.0 };
                let fit_title = canvas.fit_text_to_width(
                    title,
                    (rw - right_reserve).max(80.0),
                    13.0,
                    FontWeight::Semibold,
                );
                if compact_rows {
                    let t_cap = canvas.cap_height(12.5, FontWeight::Semibold);
                    canvas.draw_text_baseline(
                        &fit_title,
                        rx + 12.0,
                        ry + (row_h + t_cap) * 0.5,
                        12.5,
                        FontWeight::Semibold,
                        TEXT_PRIMARY,
                        1.0,
                    );
                } else {
                    canvas.draw_text(
                        &fit_title,
                        rx + 14.0,
                        ry + (row_h - 34.0) * 0.5,
                        13.0,
                        FontWeight::Semibold,
                        TEXT_PRIMARY,
                        1.0,
                    );
                    let fit_desc = canvas.fit_text_to_width(
                        desc,
                        (rw - right_reserve).max(80.0),
                        11.4,
                        FontWeight::Regular,
                    );
                    canvas.draw_text(
                        &fit_desc,
                        rx + 14.0,
                        ry + (row_h - 34.0) * 0.5 + 18.0,
                        11.4,
                        FontWeight::Regular,
                        TEXT_SECONDARY,
                        1.0,
                    );
                }

                if let Some(is_on) = toggle_state {
                    let th = if compact_rows { 18.0 } else { 22.0 };
                    let tw = if compact_rows { 34.0 } else { 42.0 };
                    canvas.draw_toggle_switch(
                        rx + rw - tw - 14.0,
                        ry + (row_h - th) * 0.5,
                        tw,
                        th,
                        is_on,
                        if is_hov { 1.0 } else { 0.0 },
                    );
                } else {
                    let bw = (canvas.measure_text_width(&badge_text, 11.8, FontWeight::Semibold)
                        + 20.0)
                        .clamp(76.0, (rw * 0.42).max(76.0));
                    let bh = if compact_rows { 22.0 } else { 26.0 };
                    let pill_r = SDL_FRect {
                        x: rx + rw - bw - 12.0,
                        y: ry + (row_h - bh) * 0.5,
                        w: bw,
                        h: bh,
                    };
                    draw_small_pill(canvas, &pill_r, &badge_text, APPLE_BLUE_SOFT, APPLE_BLUE);
                }

                hit_zones.push(HitZone {
                    rect: row_rect,
                    action,
                });
            }
        }
        SettingsSection::HelpAbout => {
            let content_y = tab_y + tab_h + 16.0;

            // About Header Card
            canvas.draw_rounded_card(
                rx,
                content_y,
                rw,
                68.0,
                8.0,
                BANNER_USB_BG,
                CARD_BORDER,
                1.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
            canvas.draw_app_brand_icon(rx + 14.0, content_y + 14.0, 40.0);
            canvas.draw_text(
                "Mobile Mirror  •  v0.1.0",
                rx + 66.0,
                content_y + 14.0,
                14.5,
                FontWeight::Bold,
                TEXT_PRIMARY,
                1.0,
            );
            let about_sub = canvas.fit_text_to_width(
                "Ultra-low-latency Android (USB) & iOS (USB / AirPlay) screen mirroring & lossless MP4 recording",
                rw - 82.0,
                11.8,
                FontWeight::Regular,
            );
            canvas.draw_text(
                &about_sub,
                rx + 66.0,
                content_y + 37.0,
                11.8,
                FontWeight::Regular,
                TEXT_SECONDARY,
                1.0,
            );

            // Connection Guide Steps
            let steps = [
                (
                    "Android (USB Debugging)",
                    "Enable Developer Options -> USB Debugging, plug in via USB, and tap 'Allow'.",
                ),
                (
                    "iPhone / iPad (USB Cable)",
                    "Plug in your iPhone or iPad via USB and tap 'Trust This Computer' on the device.",
                ),
                (
                    "iPhone / iPad (Wireless AirPlay)",
                    "Open Control Center -> Screen Mirroring -> select 'Mobile Mirror' (keep AirPlay ON).",
                ),
                (
                    "Keyboard Shortcuts",
                    "F11: Toggle Fullscreen  •  F: Expand/Compact  •  R: Start/Stop Recording  •  M: Toggle Mic",
                ),
            ];

            let steps_y = content_y + 82.0;
            for (idx, (st_title, st_desc)) in steps.iter().enumerate() {
                let sy = steps_y + idx as f32 * 64.0;
                canvas.draw_rounded_card(
                    rx,
                    sy,
                    rw,
                    56.0,
                    8.0,
                    CARD_WHITE,
                    CARD_BORDER,
                    1.0,
                    SHADOW_SLATE,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                );
                canvas.draw_circle(rx + 22.0, sy + 28.0, 12.0, APPLE_BLUE_SOFT, 1.0);
                let num = format!("{}", idx + 1);
                let nw = canvas.measure_text_width(&num, 12.5, FontWeight::Bold);
                let n_cap = canvas.cap_height(12.5, FontWeight::Bold);
                canvas.draw_text_baseline(
                    &num,
                    rx + 22.0 - nw * 0.5,
                    sy + 28.0 + n_cap * 0.5,
                    12.5,
                    FontWeight::Bold,
                    APPLE_BLUE,
                    1.0,
                );

                canvas.draw_text(
                    st_title,
                    rx + 44.0,
                    sy + 9.0,
                    13.2,
                    FontWeight::Semibold,
                    TEXT_PRIMARY,
                    1.0,
                );
                let fit_d =
                    canvas.fit_text_to_width(st_desc, rw - 56.0, 11.8, FontWeight::Regular);
                canvas.draw_text(
                    &fit_d,
                    rx + 44.0,
                    sy + 30.0,
                    11.8,
                    FontWeight::Regular,
                    TEXT_SECONDARY,
                    1.0,
                );
            }

            // Engine Diagnostics Box
            let diag_y = steps_y + 4.0 * 64.0 + 10.0;
            canvas.draw_rounded_card(
                rx,
                diag_y,
                rw,
                114.0,
                8.0,
                BANNER_USB_BG,
                BANNER_USB_BORDER,
                1.0,
                SHADOW_SLATE,
                0.0,
                0.0,
                0.0,
                1.0,
            );
            canvas.draw_text(
                "Engine Diagnostics",
                rx + 16.0,
                diag_y + 12.0,
                13.5,
                FontWeight::Semibold,
                TEXT_PRIMARY,
                1.0,
            );
            let diag1 = format!(
                "• Connection: {} ({})  •  AirPlay: {}",
                state.headline,
                state
                    .platform_label
                    .or(state.device_serial)
                    .unwrap_or("Waiting"),
                if state.airplay_listening {
                    "Listening"
                } else if state.settings.airplay_enabled {
                    "Enabled"
                } else {
                    "Off"
                }
            );
            let diag2 = format!(
                "• Stream Profile: {}  •  {} Mbps {}  •  {} FPS",
                state.settings.quality.label(),
                state.settings.bitrate_mbps,
                if state.settings.cbr_mode { "CBR" } else { "VBR" },
                state.settings.target_fps
            );
            let diag3 = format!(
                "• Hardware Decoder: {}",
                state.settings.hw_backend.label(),
            );
            canvas.draw_text(
                &canvas.fit_text_to_width(&diag1, rw - 32.0, 12.0, FontWeight::Regular),
                rx + 16.0,
                diag_y + 38.0,
                12.0,
                FontWeight::Regular,
                TEXT_BODY,
                1.0,
            );
            canvas.draw_text(
                &canvas.fit_text_to_width(&diag2, rw - 32.0, 12.0, FontWeight::Regular),
                rx + 16.0,
                diag_y + 60.0,
                12.0,
                FontWeight::Regular,
                TEXT_BODY,
                1.0,
            );
            canvas.draw_text(
                &canvas.fit_text_to_width(&diag3, rw - 32.0, 12.0, FontWeight::Regular),
                rx + 16.0,
                diag_y + 82.0,
                12.0,
                FontWeight::Regular,
                TEXT_BODY,
                1.0,
            );

            // Check USB Device Now button
            let btn_y = (diag_y + 126.0).min(card_y + card_h - 46.0);
            let test_rect = SDL_FRect {
                x: rx,
                y: btn_y,
                w: 196.0,
                h: 38.0,
            };
            let test_bg = if point_in_rect(mx, my, &test_rect) {
                BLUE_GRAD_HOVER_TOP
            } else {
                APPLE_BLUE
            };
            draw_small_pill(canvas, &test_rect, "Check USB Device Now", test_bg, CARD_WHITE);
            hit_zones.push(HitZone {
                rect: test_rect,
                action: UiAction::StartMirroring,
            });
        }
    }
}

fn draw_small_pill(canvas: &mut UiCanvas, rect: &SDL_FRect, label: &str, bg: Rgb, fg: Rgb) {
    canvas.draw_rounded_card(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        rect.h * 0.5,
        bg,
        bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    let tw = canvas.measure_text_width(label, 12.4, FontWeight::Semibold);
    let cap = canvas.cap_height(12.4, FontWeight::Semibold);
    canvas.draw_text_baseline(
        label,
        rect.x + (rect.w - tw) * 0.5,
        rect.y + (rect.h + cap) * 0.5,
        12.4,
        FontWeight::Semibold,
        fg,
        1.0,
    );
}

fn format_hh_mm_ss(total_secs: u64) -> String {
    let hrs = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hrs, mins, secs)
}

fn format_rec_timer(rec_elapsed_secs: u64) -> String {
    let mins = rec_elapsed_secs / 60;
    let secs = rec_elapsed_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

const TOOLBAR_TITLE_SIZE: f32 = 14.0;
const TOOLBAR_META_SIZE: f32 = 12.2;
const TOOLBAR_PILL_FONT_SIZE: f32 = 12.0;

#[allow(clippy::too_many_arguments, non_snake_case)]
fn draw_live_toolbar_strip_with_Zones(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    y_top: f32,
    bar_w: f32,
    is_recording: bool,
    rec_elapsed_secs: u64,
    is_mic_enabled: bool,
    fps: u32,
    is_window_fullscreen: bool,
) -> f32 {
    let s = canvas.scale;
    let phys_y0 = (y_top * s).round() as i32;
    let phys_bw = (bar_w * s).round() as i32;
    let phys_bh = (TOOLBAR_HEIGHT as f32 * s).round() as i32;
    let div_px = s.floor().max(1.0) as i32;

    canvas.fill_physical_rect(0, phys_y0, phys_bw, phys_bh - div_px, CARD_WHITE);
    canvas.fill_physical_rect(0, phys_y0 + phys_bh - div_px, phys_bw, div_px, BORDER_SUBTLE);

    let pill_y = y_top + 8.0;
    let pill_h = 28.0;
    let cy = y_top + (TOOLBAR_HEIGHT as f32) * 0.5;
    let is_wide = bar_w >= 540.0;

    // 1. Right-to-Left Compact Controls: [Dash] [Mic] [REC] [—] [□ (F11)] [✕]
    let close_rect = SDL_FRect {
        x: (bar_w - 6.0 - 28.0).round(),
        y: pill_y,
        w: 28.0,
        h: pill_h,
    };
    let full_rect = SDL_FRect {
        x: (close_rect.x - 4.0 - 28.0).round(),
        y: pill_y,
        w: 28.0,
        h: pill_h,
    };
    let min_rect = SDL_FRect {
        x: (full_rect.x - 4.0 - 28.0).round(),
        y: pill_y,
        w: 28.0,
        h: pill_h,
    };

    let rec_label = if is_recording {
        format_rec_timer(rec_elapsed_secs)
    } else if is_wide {
        "Record".to_string()
    } else {
        "REC".to_string()
    };
    let rec_w = if is_recording {
        64.0
    } else if is_wide {
        76.0
    } else {
        56.0
    };
    let rec_rect = SDL_FRect {
        x: (min_rect.x - 5.0 - rec_w).round(),
        y: pill_y,
        w: rec_w,
        h: pill_h,
    };

    let mic_label = if is_mic_enabled && is_wide {
        "Mic On"
    } else {
        "Mic"
    };
    let mic_w = if is_mic_enabled && is_wide { 68.0 } else { 46.0 };
    let mic_rect = SDL_FRect {
        x: (rec_rect.x - 5.0 - mic_w).round(),
        y: pill_y,
        w: mic_w,
        h: pill_h,
    };

    let dash_label = if is_wide { "Dashboard" } else { "Dash" };
    let dash_w = if is_wide { 92.0 } else { 50.0 };
    let dash_rect = SDL_FRect {
        x: (mic_rect.x - 5.0 - dash_w).round(),
        y: pill_y,
        w: dash_w,
        h: pill_h,
    };

    let first_btn_x = dash_rect.x;

    // Draw Dash pill
    let dash_bg = if point_in_rect(mx, my, &dash_rect) {
        PILL_NEUTRAL_HOVER
    } else {
        PILL_NEUTRAL
    };
    draw_small_pill(canvas, &dash_rect, dash_label, dash_bg, TEXT_PRIMARY);
    hit_zones.push(HitZone {
        rect: dash_rect,
        action: UiAction::ToggleFullscreenMirror,
    });

    // Draw Mic pill
    let mic_hover = point_in_rect(mx, my, &mic_rect);
    let (mic_bg, mic_fg) = if is_mic_enabled {
        (
            if mic_hover {
                APPLE_BLUE_HOVER
            } else {
                APPLE_BLUE_SOFT
            },
            APPLE_BLUE,
        )
    } else {
        (
            if mic_hover {
                PILL_NEUTRAL_HOVER
            } else {
                PILL_NEUTRAL
            },
            TEXT_PRIMARY,
        )
    };
    draw_small_pill(canvas, &mic_rect, mic_label, mic_bg, mic_fg);
    hit_zones.push(HitZone {
        rect: mic_rect,
        action: UiAction::ToggleMic,
    });

    // Draw Record pill with status dot
    let rec_hover = point_in_rect(mx, my, &rec_rect);
    let (rec_bg, rec_fg) = if is_recording {
        (
            if rec_hover {
                ERROR_BG_HOVER
            } else {
                ERROR_BG
            },
            ERROR_RED,
        )
    } else {
        (
            if rec_hover {
                PILL_NEUTRAL_HOVER
            } else {
                PILL_NEUTRAL
            },
            TEXT_PRIMARY,
        )
    };
    canvas.draw_rounded_card(
        rec_rect.x,
        rec_rect.y,
        rec_rect.w,
        rec_rect.h,
        14.0,
        rec_bg,
        rec_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    let rtw = canvas.measure_text_width(&rec_label, TOOLBAR_PILL_FONT_SIZE, FontWeight::Semibold);
    let rcap = canvas.cap_height(TOOLBAR_PILL_FONT_SIZE, FontWeight::Semibold);
    let total_rw = 11.0 + rtw;
    let rx0 = rec_rect.x + (rec_rect.w - total_rw) * 0.5;
    canvas.draw_circle(rx0 + 3.5, cy, 3.6, ERROR_RED, 1.0);
    canvas.draw_text_baseline(
        &rec_label,
        rx0 + 11.0,
        cy + rcap * 0.5,
        TOOLBAR_PILL_FONT_SIZE,
        FontWeight::Semibold,
        rec_fg,
        1.0,
    );
    hit_zones.push(HitZone {
        rect: rec_rect,
        action: UiAction::ToggleRecord,
    });

    // Draw Minimize button `—`
    let min_bg = if point_in_rect(mx, my, &min_rect) {
        PILL_NEUTRAL_HOVER
    } else {
        PILL_NEUTRAL
    };
    canvas.draw_rounded_card(
        min_rect.x,
        min_rect.y,
        min_rect.w,
        min_rect.h,
        8.0,
        min_bg,
        min_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    canvas.draw_capsule_line(
        min_rect.x + 9.0,
        cy,
        min_rect.x + 19.0,
        cy,
        1.8,
        TEXT_PRIMARY,
        0.88,
    );
    hit_zones.push(HitZone {
        rect: min_rect,
        action: UiAction::MinimizeWindow,
    });

    // Draw Fullscreen / Restore (F11) button `□`
    let full_hover = point_in_rect(mx, my, &full_rect);
    let (full_bg, full_fg) = if is_window_fullscreen {
        (
            if full_hover {
                APPLE_BLUE_HOVER
            } else {
                APPLE_BLUE_SOFT
            },
            APPLE_BLUE,
        )
    } else {
        (
            if full_hover {
                PILL_NEUTRAL_HOVER
            } else {
                PILL_NEUTRAL
            },
            TEXT_PRIMARY,
        )
    };
    canvas.draw_rounded_card(
        full_rect.x,
        full_rect.y,
        full_rect.w,
        full_rect.h,
        8.0,
        full_bg,
        full_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    if is_window_fullscreen {
        // Restore / Exit-Fullscreen double-frame icon
        canvas.draw_rounded_stroke(
            full_rect.x + 10.5,
            cy - 5.5,
            8.5,
            8.5,
            1.6,
            1.5,
            full_fg,
            0.82,
        );
        canvas.draw_rounded_card(
            full_rect.x + 8.5,
            cy - 3.0,
            8.5,
            8.5,
            1.6,
            full_bg,
            full_fg,
            1.5,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
    } else {
        canvas.draw_rounded_stroke(
            full_rect.x + 9.0,
            cy - 5.0,
            10.0,
            10.0,
            2.0,
            1.7,
            full_fg,
            0.88,
        );
    }
    hit_zones.push(HitZone {
        rect: full_rect,
        action: UiAction::ToggleWindowFullscreen,
    });

    // Draw Close / Disconnect button `✕`
    let close_hover = point_in_rect(mx, my, &close_rect);
    let close_bg = if close_hover {
        ERROR_BG_HOVER
    } else {
        PILL_NEUTRAL
    };
    let close_fg = if close_hover { ERROR_RED } else { TEXT_PRIMARY };
    canvas.draw_rounded_card(
        close_rect.x,
        close_rect.y,
        close_rect.w,
        close_rect.h,
        8.0,
        close_bg,
        close_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    canvas.draw_capsule_line(
        close_rect.x + 9.5,
        cy - 4.5,
        close_rect.x + 18.5,
        cy + 4.5,
        1.8,
        close_fg,
        0.92,
    );
    canvas.draw_capsule_line(
        close_rect.x + 18.5,
        cy - 4.5,
        close_rect.x + 9.5,
        cy + 4.5,
        1.8,
        close_fg,
        0.92,
    );
    hit_zones.push(HitZone {
        rect: close_rect,
        action: UiAction::StopMirroring,
    });

    // 2. Left Draggable Status Section: Brand Icon + Green Live Dot + Title (if wide) + FPS badge
    canvas.draw_app_brand_icon(8.0, cy - 11.0, 22.0);
    let dot_cx = 37.0;
    canvas.draw_circle(dot_cx, cy, 4.4, LIVE_GREEN, 1.0);

    let mut cur_x = 46.0;
    if bar_w >= 540.0 {
        let title_cap = canvas.cap_height(TOOLBAR_TITLE_SIZE, FontWeight::Semibold);
        canvas.draw_text_baseline(
            "Mobile Mirror",
            cur_x,
            cy + title_cap * 0.5,
            TOOLBAR_TITLE_SIZE,
            FontWeight::Semibold,
            TEXT_PRIMARY,
            1.0,
        );
        cur_x +=
            canvas.measure_text_width("Mobile Mirror", TOOLBAR_TITLE_SIZE, FontWeight::Semibold)
                + 10.0;
    }

    let fps_val = if fps > 0 { fps } else { 60 };
    let fps_label = format!("{} FPS", fps_val);
    let fps_w = canvas.measure_text_width(&fps_label, TOOLBAR_META_SIZE, FontWeight::Semibold);
    if cur_x + fps_w + 6.0 <= first_btn_x {
        let fps_cap = canvas.cap_height(TOOLBAR_META_SIZE, FontWeight::Semibold);
        canvas.draw_text_baseline(
            &fps_label,
            cur_x,
            cy + fps_cap * 0.5,
            TOOLBAR_META_SIZE,
            FontWeight::Semibold,
            if bar_w >= 540.0 {
                TEXT_SECONDARY
            } else {
                TEXT_PRIMARY
            },
            1.0,
        );
        cur_x += fps_w + 12.0;
    }

    if bar_w >= 680.0 {
        let f11_hint = if is_window_fullscreen {
            "Press F11 or Esc to exit Fullscreen"
        } else {
            "Press F11 for Fullscreen"
        };
        let hw = canvas.measure_text_width(f11_hint, 11.8, FontWeight::Regular);
        if cur_x + hw + 10.0 <= first_btn_x {
            let hcap = canvas.cap_height(11.8, FontWeight::Regular);
            canvas.draw_text_baseline(
                f11_hint,
                cur_x,
                cy + hcap * 0.5,
                11.8,
                FontWeight::Regular,
                TEXT_SECONDARY,
                1.0,
            );
        }
    }

    first_btn_x
}

/// Renders the `44px` logical Live Mirroring Top Control Bar at vertical offset `y_top`.
pub fn draw_live_toolbar_strip(
    canvas: &mut UiCanvas,
    y_top: f32,
    bar_w: f32,
    is_recording: bool,
    rec_elapsed_secs: u64,
    is_mic_enabled: bool,
    fps: u32,
) {
    let mut zones = Vec::new();
    let _ = draw_live_toolbar_strip_with_Zones(
        canvas,
        &mut zones,
        -1000.0,
        -1000.0,
        y_top,
        bar_w,
        is_recording,
        rec_elapsed_secs,
        is_mic_enabled,
        fps,
        false,
    );
}

/// Legacy compatibility entry point that renders the Bento Dashboard onto `canvas`.
pub fn draw_startup_screen_canvas(
    canvas: &mut UiCanvas,
    headline: &str,
    subtext: &str,
    is_connecting: bool,
    is_warning: bool,
) {
    let default_settings = AppSettings::default();
    let state = DashboardViewState {
        active_tab: NavTab::Home,
        settings: &default_settings,
        headline,
        subtext,
        is_connecting,
        is_warning,
        is_mirroring: false,
        device_serial: None,
        platform_label: None,
        airplay_listening: false,
        is_recording: false,
        rec_elapsed_secs: 0,
        last_saved_recording: None,
        stream_width: 0,
        stream_height: 0,
        fps: 60,
        recordings: &[],
        status_toast: None,
        thumbnails: None,
        recordings_page: 0,
    };
    let mut zones = Vec::new();
    let _ = draw_bento_dashboard_canvas(
        canvas,
        &mut zones,
        -1000.0,
        -1000.0,
        &state,
        SettingsSection::Preferences,
    );
}

/// Renders and saves lossless PNG preview snapshots of the "Mobile Mirror" Bento Dashboard states
/// (`ui_dashboard_home.png`, `ui_dashboard_settings.png`, `ui_startup_waiting.png`).
pub fn export_preview_snapshots(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;

    let default_settings = AppSettings::default();
    let mut zones = Vec::new();

    // 1. Main Home Dashboard (1024 x 698 — exact match to reference screenshot)
    let mut canvas_home = UiCanvas::new(DASHBOARD_WIDTH as usize, DASHBOARD_HEIGHT as usize);
    let state_home = DashboardViewState {
        active_tab: NavTab::Home,
        settings: &default_settings,
        headline: "Connect your phone via USB",
        subtext: "Make sure USB debugging is enabled on your Android device.",
        is_connecting: false,
        is_warning: false,
        is_mirroring: false,
        device_serial: None,
        platform_label: None,
        airplay_listening: false,
        is_recording: false,
        rec_elapsed_secs: 0,
        last_saved_recording: None,
        stream_width: 0,
        stream_height: 0,
        fps: 60,
        recordings: &[],
        status_toast: None,
        thumbnails: None,
        recordings_page: 0,
    };
    let _ = draw_bento_dashboard_canvas(
        &mut canvas_home,
        &mut zones,
        -1000.0,
        -1000.0,
        &state_home,
        SettingsSection::Preferences,
    );
    canvas_home.save_png(&dir.join("ui_dashboard_home.png"))?;
    canvas_home.save_png(&dir.join("ui_startup_waiting.png"))?;

    // 2. Settings Tab Preview (Preferences)
    let mut canvas_settings = UiCanvas::new(DASHBOARD_WIDTH as usize, DASHBOARD_HEIGHT as usize);
    let state_settings = DashboardViewState {
        active_tab: NavTab::Settings,
        settings: &default_settings,
        headline: "Connect your phone via USB",
        subtext: "Make sure USB debugging is enabled on your Android device.",
        is_connecting: false,
        is_warning: false,
        is_mirroring: false,
        device_serial: None,
        platform_label: None,
        airplay_listening: false,
        is_recording: false,
        rec_elapsed_secs: 0,
        last_saved_recording: None,
        stream_width: 0,
        stream_height: 0,
        fps: 60,
        recordings: &[],
        status_toast: None,
        thumbnails: None,
        recordings_page: 0,
    };
    let _ = draw_bento_dashboard_canvas(
        &mut canvas_settings,
        &mut zones,
        -1000.0,
        -1000.0,
        &state_settings,
        SettingsSection::Preferences,
    );
    canvas_settings.save_png(&dir.join("ui_dashboard_settings.png"))?;

    // 3. Settings -> Help & About Preview
    let mut canvas_about = UiCanvas::new(DASHBOARD_WIDTH as usize, DASHBOARD_HEIGHT as usize);
    let state_about = DashboardViewState {
        active_tab: NavTab::Settings,
        settings: &default_settings,
        headline: "Connect your phone via USB",
        subtext: "Make sure USB debugging is enabled on your Android device.",
        is_connecting: false,
        is_warning: false,
        is_mirroring: false,
        device_serial: None,
        platform_label: None,
        airplay_listening: false,
        is_recording: false,
        rec_elapsed_secs: 0,
        last_saved_recording: None,
        stream_width: 0,
        stream_height: 0,
        fps: 60,
        recordings: &[],
        status_toast: None,
        thumbnails: None,
        recordings_page: 0,
    };
    let _ = draw_bento_dashboard_canvas(
        &mut canvas_about,
        &mut zones,
        -1000.0,
        -1000.0,
        &state_about,
        SettingsSection::HelpAbout,
    );
    canvas_about.save_png(&dir.join("ui_dashboard_about.png"))?;

    // 4. Compact Portrait Phone Toolbar Preview (387 x 44)
    let mut canvas_toolbar = UiCanvas::new(387, TOOLBAR_HEIGHT as usize);
    let _ = draw_live_toolbar_strip_with_Zones(
        &mut canvas_toolbar,
        &mut zones,
        -1000.0,
        -1000.0,
        0.0,
        387.0,
        false,
        0,
        false,
        60,
        false,
    );
    canvas_toolbar.save_png(&dir.join("ui_compact_phone_toolbar.png"))?;

    // 5. Recordings Tab Orderly Grid Preview
    let mut canvas_rec = UiCanvas::new(DASHBOARD_WIDTH as usize, DASHBOARD_HEIGHT as usize);
    let rec_path1 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("recordings")
        .join("recording_20261118_174859.mp4");
    let sample_rec1 = RecordingEntry {
        file_name: "recording_20261118_174859.mp4".to_string(),
        full_path: rec_path1.clone(),
        size_bytes: 163390359,
        modified_epoch_secs: 1731930539,
    };
    let rec_path2 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("recordings")
        .join("part2.mp4");
    let sample_rec2 = RecordingEntry {
        file_name: "part2.mp4".to_string(),
        full_path: rec_path2.clone(),
        size_bytes: 84200000,
        modified_epoch_secs: 1731920000,
    };
    let rec_path3 = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("recordings")
        .join("part3.mp4");
    let sample_rec3 = RecordingEntry {
        file_name: "part3.mp4".to_string(),
        full_path: rec_path3.clone(),
        size_bytes: 65100000,
        modified_epoch_secs: 1731910000,
    };
    let recs_list = vec![sample_rec1, sample_rec2, sample_rec3];

    let mut thumb_map = std::collections::HashMap::new();
    if let Some(real_thumb) = crate::thumbnail::extract_video_thumbnail(&rec_path1, 240, 136) {
        thumb_map.insert(rec_path1, real_thumb.clone());
        thumb_map.insert(rec_path2, real_thumb.clone());
        thumb_map.insert(rec_path3, real_thumb);
    }

    let state_rec = DashboardViewState {
        active_tab: NavTab::Recordings,
        settings: &default_settings,
        headline: "Saved Recordings",
        subtext: "Live USB screen recordings",
        is_connecting: false,
        is_warning: false,
        is_mirroring: false,
        device_serial: None,
        platform_label: None,
        airplay_listening: false,
        is_recording: false,
        rec_elapsed_secs: 0,
        last_saved_recording: None,
        stream_width: 0,
        stream_height: 0,
        fps: 60,
        recordings: &recs_list,
        status_toast: None,
        thumbnails: Some(&thumb_map),
        recordings_page: 0,
    };
    let _ = draw_bento_dashboard_canvas(
        &mut canvas_rec,
        &mut zones,
        -1000.0,
        -1000.0,
        &state_rec,
        SettingsSection::Preferences,
    );
    canvas_rec.save_png(&dir.join("ui_dashboard_recordings_gallery.png"))?;

    Ok(())
}

fn point_in_rect(px: f32, py: f32, r: &SDL_FRect) -> bool {
    px >= r.x && px <= (r.x + r.w) && py >= r.y && py <= (r.y + r.h)
}

impl Drop for Sdl3Renderer {
    fn drop(&mut self) {
        unsafe {
            if !self.window.is_null() {
                if let Some(set_hit_test) = self.api.sdl_set_window_hit_test {
                    let _ = set_hit_test(self.window, None, ptr::null_mut());
                }
            }
            if !self.hit_test_ctx.is_null() {
                let _ = Box::from_raw(self.hit_test_ctx);
                self.hit_test_ctx = ptr::null_mut();
            }
            if !self.ui_texture.is_null() {
                (self.api.sdl_destroy_texture)(self.ui_texture);
            }
            if !self.texture.is_null() {
                (self.api.sdl_destroy_texture)(self.texture);
            }
            if !self.renderer.is_null() {
                (self.api.sdl_destroy_renderer)(self.renderer);
            }
            if !self.window.is_null() {
                (self.api.sdl_destroy_window)(self.window);
            }
            (self.api.sdl_quit)();
        }
    }
}

/// Real-time ultra-low-latency SDL3 audio output player for phone system & game sound.
pub struct Sdl3AudioPlayer {
    api: Sdl3Api,
    stream: *mut SDL_AudioStream,
}

unsafe impl Send for Sdl3AudioPlayer {}

impl Sdl3AudioPlayer {
    pub fn new() -> Result<Self, String> {
        let api = Sdl3Api::load()?;
        unsafe {
            let spec = SDL_AudioSpec {
                format: SDL_AUDIO_S16LE,
                channels: 2,
                freq: 48000,
            };
            let stream =
                (api.sdl_open_audio_device_stream)(0xFFFF_FFFF, &spec, None, ptr::null_mut());
            if stream.is_null() {
                return Err(format!(
                    "SDL_OpenAudioDeviceStream failed: {}",
                    Sdl3Renderer::last_error(&api)
                ));
            }
            let _ = (api.sdl_resume_audio_stream_device)(stream);
            Ok(Self { api, stream })
        }
    }

    pub fn play(&mut self, pcm_bytes: &[u8]) {
        if !self.stream.is_null() && !pcm_bytes.is_empty() {
            unsafe {
                (self.api.sdl_put_audio_stream_data)(
                    self.stream,
                    pcm_bytes.as_ptr() as *const std::ffi::c_void,
                    pcm_bytes.len() as std::ffi::c_int,
                );
            }
        }
    }
}

impl Drop for Sdl3AudioPlayer {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            unsafe {
                (self.api.sdl_destroy_audio_stream)(self.stream);
            }
            self.stream = ptr::null_mut();
        }
    }
}

pub fn calculate_target_window_size(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (DASHBOARD_WIDTH, DASHBOARD_HEIGHT);
    }
    let (video_w, video_h) = if width >= height {
        let target_h = 700u32;
        let target_w = (width as u64 * target_h as u64 / height as u64) as u32;
        if target_w > 1560 {
            let max_w = 1560u32;
            let h = (height as u64 * max_w as u64 / width as u64) as u32;
            (max_w, h.max(100))
        } else {
            (target_w.max(100), target_h)
        }
    } else {
        let target_h = 860u32;
        let target_w = (width as u64 * target_h as u64 / height as u64) as u32;
        (target_w.max(100), target_h)
    };
    (video_w, video_h + TOOLBAR_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_ui_snapshots() {
        let out_dir = Path::new(
            r"C:\Users\asus\.gemini\antigravity\brain\b2d7cb42-2db3-4536-ab53-6aadb7033689\scratch",
        );
        export_preview_snapshots(out_dir).expect("Failed to export UI preview snapshots");
    }

    #[test]
    fn test_compact_phone_window_sizing_and_toolbar_layout() {
        let (pw, ph) = calculate_target_window_size(1080, 2400);
        assert_eq!((pw, ph), (387, 904));

        let (lw, lh) = calculate_target_window_size(2400, 1080);
        assert_eq!((lw, lh), (1555, 744));

        let mut canvas = UiCanvas::new(pw as usize, TOOLBAR_HEIGHT as usize);
        let mut zones = Vec::new();
        let first_btn_x = draw_live_toolbar_strip_with_Zones(
            &mut canvas,
            &mut zones,
            -1000.0,
            -1000.0,
            0.0,
            pw as f32,
            true,
            12,
            true,
            60,
            false,
        );
        assert!(
            first_btn_x >= 96.0,
            "Expected generous draggable left area on 387px portrait toolbar, got {}",
            first_btn_x
        );
        assert_eq!(zones.len(), 6);
        assert!(zones.iter().any(|z| z.action == UiAction::MinimizeWindow));
        assert!(zones
            .iter()
            .any(|z| z.action == UiAction::ToggleWindowFullscreen));
    }
}
