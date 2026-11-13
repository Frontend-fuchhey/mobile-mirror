// [Mobile Mirror] Progressive implementation phase 5/8
use crate::decoder::{DecodedFrameView, PixelLayout};
use crate::ffi::sdl3::{
    Sdl3Api, SDL_AudioSpec, SDL_AudioStream, SDL_Event, SDL_FRect, SDL_Point, SDL_Renderer,
    SDL_Texture, SDL_Window, SDLK_F, SDL_AUDIO_S16LE, SDL_EVENT_KEY_DOWN,
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
    BANNER_USB_BG, BANNER_USB_BORDER, BG_ALABASTER, BG_HEADER, BG_SIDEBAR, BLUE_GRAD_BOT,
    BLUE_GRAD_HOVER_BOT, BLUE_GRAD_HOVER_TOP, BLUE_GRAD_TOP, BORDER_SUBTLE, CARD_BORDER,
    CARD_WHITE, ERROR_BG, ERROR_BG_HOVER, ERROR_RED, LIVE_GREEN, PILL_NEUTRAL,
    PILL_NEUTRAL_HOVER, SHADOW_SLATE, TEXT_BODY, TEXT_PRIMARY, TEXT_SECONDARY, WARNING_AMBER,
    WARNING_AMBER_SOFT,
};
use std::ffi::{c_void, CStr, CString};
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

pub const DASHBOARD_WIDTH: u32 = 1120;
pub const DASHBOARD_HEIGHT: u32 = 740;
pub const TOOLBAR_HEIGHT: u32 = 44;
pub const HEADER_BAR_HEIGHT: f32 = 54.0;

/// Interactive UI actions emitted when the user clicks dashboard controls or presses hotkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    StartMirroring,
    StopMirroring,
    ToggleRecord,
    ToggleMic,
    SelectQuality(MirrorQuality),
    SwitchTab(NavTab),
    OpenRecordingsFolder,
    PlayRecording(PathBuf),
    RevealRecording(PathBuf),
    DeleteRecording(PathBuf),
    CycleFps,
    CycleBitrate,
    ToggleCbr,
    TogglePcAudio,
    ToggleAutoConnect,
    ToggleAutoRecord,
    ToggleAlwaysOnTop,
    CycleHwBackend,
    ToggleFullscreenMirror,
    ToggleWindowFullscreen,
    ResetDefaultSettings,
    MinimizeWindow,
    MaximizeRestoreWindow,
    Quit,
}

#[derive(Clone)]
struct HitZone {
    rect: SDL_FRect,
    action: UiAction,
}

/// Complete state snapshot passed to the Bento Dashboard renderer each frame.
pub struct DashboardViewState<'a> {
    pub active_tab: NavTab,
    pub settings: &'a AppSettings,
    pub headline: &'a str,
    pub subtext: &'a str,
    pub is_connecting: bool,
    pub is_warning: bool,
    pub is_mirroring: bool,
    pub device_serial: Option<&'a str>,
    pub is_recording: bool,
    pub rec_elapsed_secs: u64,
    pub last_saved_recording: Option<&'a str>,
    pub stream_width: u32,
    pub stream_height: u32,
    pub fps: u32,
    pub recordings: &'a [RecordingEntry],
    pub status_toast: Option<&'a str>,
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
    last_saved_recording: Option<String>,
    status_toast: Option<String>,
    recordings_len: usize,
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
    pub cached_recordings: Vec<RecordingEntry>,
    last_recordings_scan: std::time::Instant,
    pub status_toast: Option<(String, std::time::Instant)>,
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
                cached_recordings: Vec::new(),
                last_recordings_scan: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(10))
                    .unwrap_or_else(std::time::Instant::now),
                status_toast: None,
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
        if false && self.is_window_minimized() {
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
                    if key == SDLK_F || scancode == SDL_SCANCODE_F11 {
                        actions.push(UiAction::ToggleFullscreenMirror);
                    } else if key == 0x1B {
                        // Esc exits fullscreen if currently fullscreen; otherwise quits app
                        if self.is_window_fullscreen() {
                            actions.push(UiAction::ToggleFullscreenMirror);
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
        if false && self.is_window_minimized() {
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
            last_saved_recording: state.last_saved_recording.map(str::to_string),
            status_toast: state.status_toast.map(str::to_string),
            recordings_len: state.recordings.len(),
            anim_bucket,
        };

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
                    state,
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
            is_recording: false,
            rec_elapsed_secs: 0,
            last_saved_recording: None,
            stream_width: 0,
            stream_height: 0,
            fps: 60,
            recordings: &recs,
            status_toast: None,
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
            is_recording,
            rec_elapsed_secs,
            last_saved_recording: None,
            stream_width: frame.width,
            stream_height: frame.height,
            fps,
            recordings: &recs,
            status_toast: None,
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
            is_recording,
            rec_elapsed_secs,
            last_saved_recording: None,
            stream_width: self.tex_width,
            stream_height: self.tex_height,
            fps,
            recordings: &recs,
            status_toast: None,
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
) -> Option<SDL_FRect> {
    let win_w = canvas.logical_width;
    let win_h = canvas.logical_height;

    // 1. Main Workspace Cool-Tinted Alabaster Background (#F3F6FD)
    canvas.clear(BG_ALABASTER);

    // 2. Left Sidebar Surface (x = 0..192, y = 54..win_h)
    let sidebar_w = 192.0;
    canvas.fill_rect(
        0,
        HEADER_BAR_HEIGHT as i32,
        sidebar_w as i32,
        (win_h - HEADER_BAR_HEIGHT) as i32,
        BG_SIDEBAR,
    );
    // Crisp 1px vertical divider between sidebar and main workspace
    canvas.fill_rect(
        sidebar_w as i32,
        HEADER_BAR_HEIGHT as i32,
        1,
        (win_h - HEADER_BAR_HEIGHT) as i32,
        BORDER_SUBTLE,
    );

    // 3. Top Application Header Bar (y = 0..54)
    canvas.fill_rect(0, 0, win_w as i32, HEADER_BAR_HEIGHT as i32, BG_HEADER);
    canvas.fill_rect(0, HEADER_BAR_HEIGHT as i32 - 1, win_w as i32, 1, BORDER_SUBTLE);

    // Top-left Brand Icon + "Mobile Mirror" title
    canvas.draw_app_brand_icon(24.0, 12.0, 30.0);
    let hdr_cap = canvas.cap_height(16.0, FontWeight::Bold);
    canvas.draw_text_baseline(
        "Mobile Mirror",
        66.0,
        (HEADER_BAR_HEIGHT + hdr_cap) * 0.5,
        16.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    // Top-right Window Controls (—  □  ✕) matching reference screenshot
    let wc_y = 11.0;
    let wc_w = 36.0;
    let wc_h = 32.0;
    let close_x = win_w - 48.0;
    let max_x = close_x - 42.0;
    let min_x = max_x - 42.0;

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

    if point_in_rect(mx, my, &min_rect) {
        canvas.draw_rounded_card(
            min_rect.x,
            min_rect.y,
            min_rect.w,
            min_rect.h,
            8.0,
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
    if point_in_rect(mx, my, &max_rect) {
        canvas.draw_rounded_card(
            max_rect.x,
            max_rect.y,
            max_rect.w,
            max_rect.h,
            8.0,
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
    let close_hover = point_in_rect(mx, my, &close_rect);
    if close_hover {
        canvas.draw_rounded_card(
            close_rect.x,
            close_rect.y,
            close_rect.w,
            close_rect.h,
            8.0,
            ERROR_BG_HOVER,
            ERROR_BG_HOVER,
            0.0,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );
    }

    let wc_cy = HEADER_BAR_HEIGHT * 0.5;
    // Minimize `—`
    canvas.draw_capsule_line(
        min_x + 12.0,
        wc_cy,
        min_x + 24.0,
        wc_cy,
        1.8,
        TEXT_PRIMARY,
        0.88,
    );
    // Maximize `□`
    canvas.draw_rounded_stroke(
        max_x + 12.0,
        wc_cy - 5.5,
        11.0,
        11.0,
        2.0,
        1.7,
        TEXT_PRIMARY,
        0.88,
    );
    // Close `✕`
    let close_col = if close_hover { ERROR_RED } else { TEXT_PRIMARY };
    canvas.draw_capsule_line(
        close_x + 13.0,
        wc_cy - 5.0,
        close_x + 23.0,
        wc_cy + 5.0,
        1.8,
        close_col,
        0.9,
    );
    canvas.draw_capsule_line(
        close_x + 23.0,
        wc_cy - 5.0,
        close_x + 13.0,
        wc_cy + 5.0,
        1.8,
        close_col,
        0.9,
    );

    hit_zones.push(HitZone {
        rect: min_rect,
        action: UiAction::StopSession,
    });
    hit_zones.push(HitZone {
        rect: max_rect,
        action: UiAction::MaximizeRestoreWindow,
    });
    hit_zones.push(HitZone {
        rect: close_rect,
        action: UiAction::Quit,
    });

    // 4. Left Sidebar Navigation Items (Home, Recordings, Settings, About)
    let nav_items = [
        (NavTab::Home, "Home", 82.0, 50.0),
        (NavTab::Recordings, "Recordings", 142.0, 46.0),
        (NavTab::Settings, "Settings", 198.0, 46.0),
        (NavTab::About, "About", 254.0, 46.0),
    ];

    for (tab, label, ny, nh) in nav_items {
        let nav_rect = SDL_FRect {
            x: 12.0,
            y: ny,
            w: sidebar_w - 24.0,
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
                13.0,
                bg,
                bg,
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
                12.0,
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

        let icon_cx = 38.0;
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
                canvas.draw_gear_icon(icon_cx, icon_cy, 8.6, inner_bg, fg, 1.0);
            }
            NavTab::About => canvas.draw_info_circle_icon(icon_cx, icon_cy, fg, 1.0),
        }

        let cap = canvas.cap_height(14.5, weight);
        canvas.draw_text_baseline(
            label,
            64.0,
            icon_cy + cap * 0.5,
            14.5,
            weight,
            fg,
            1.0,
        );

        hit_zones.push(HitZone {
            rect: nav_rect,
            action: UiAction::SwitchTab(tab),
        });
    }

    // 5. Bottom-Left Status Card ("Ready" / "Waiting for device...")
    let status_card_w = sidebar_w - 24.0;
    let status_card_h = 76.0;
    let status_card_y = (win_h - status_card_h - 20.0).max(320.0);
    canvas.draw_rounded_card(
        12.0,
        status_card_y,
        status_card_w,
        status_card_h,
        14.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.04,
        12.0,
        3.0,
        1.0,
    );

    let (dot_col, status_title, status_sub) = if state.is_mirroring {
        let res = format!("{}×{} • {}fps", state.stream_width, state.stream_height, state.fps);
        (LIVE_GREEN, "Live", res)
    } else if state.is_connecting {
        (APPLE_BLUE, "Connecting", "Starting USB stream...".to_string())
    } else if state.is_warning {
        (WARNING_AMBER, "Action Needed", "Check phone screen".to_string())
    } else if let Some(serial) = state.device_serial {
        (LIVE_GREEN, "Ready", format!("USB: {}", serial))
    } else {
        (LIVE_GREEN, "Ready", "Waiting for device...".to_string())
    };

    let dot_cx = 30.0;
    let dot_cy = status_card_y + 27.0;
    canvas.draw_circle(dot_cx, dot_cy, 5.2, dot_col, 1.0);
    let st_cap = canvas.cap_height(14.2, FontWeight::Bold);
    canvas.draw_text_baseline(
        status_title,
        43.0,
        dot_cy + st_cap * 0.5,
        14.2,
        FontWeight::Bold,
        dot_col,
        1.0,
    );

    let fitted_sub = canvas.fit_text_to_width(
        &status_sub,
        status_card_w - 22.0,
        12.2,
        FontWeight::Regular,
    );
    canvas.draw_text(
        &fitted_sub,
        22.0,
        status_card_y + 48.0,
        12.2,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // 6. Right Column — 3 Stacked Functional Cards (Recording, Mirror Quality, Quick Tools)
    let right_col_w = 276.0;
    let right_col_x = (win_w - right_col_w - 22.0).max(540.0);
    draw_right_column_cards(canvas, hit_zones, mx, my, right_col_x, right_col_w, state);

    // 7. Center Main Card (x = sidebar_w + 18 .. right_col_x - 18, y = 72 .. win_h - 22)
    let center_x = sidebar_w + 18.0;
    let center_y = 72.0;
    let center_w = (right_col_x - 18.0 - center_x).max(340.0);
    let center_h = (win_h - 22.0 - center_y).max(380.0);

    canvas.draw_rounded_card(
        center_x,
        center_y,
        center_w,
        center_h,
        22.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.055,
        20.0,
        6.0,
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
                canvas, hit_zones, mx, my, center_x, center_y, center_w, center_h, state,
            );
            None
        }
        NavTab::About => {
            draw_center_about_help_view(
                canvas, hit_zones, mx, my, center_x, center_y, center_w, center_h, state,
            );
            None
        }
    };

    // Optional feedback toast pill at top-center of Center Card
    if let Some(toast) = state.status_toast {
        if !toast.is_empty() {
            let tw = canvas.measure_text_width(toast, 13.0, FontWeight::Semibold);
            let pill_w = (tw + 34.0).min(center_w - 32.0);
            let pill_x = center_x + (center_w - pill_w) * 0.5;
            let pill_y = center_y + 14.0;
            canvas.draw_rounded_card(
                pill_x,
                pill_y,
                pill_w,
                32.0,
                16.0,
                Rgb(19, 27, 46),
                APPLE_BLUE,
                1.0,
                SHADOW_SLATE,
                0.18,
                10.0,
                3.0,
                0.96,
            );
            let fitted = canvas.fit_text_to_width(toast, pill_w - 22.0, 13.0, FontWeight::Semibold);
            let cap = canvas.cap_height(13.0, FontWeight::Semibold);
            let ftw = canvas.measure_text_width(&fitted, 13.0, FontWeight::Semibold);
            canvas.draw_text_baseline(
                &fitted,
                pill_x + (pill_w - ftw) * 0.5,
                pill_y + (32.0 + cap) * 0.5,
                13.0,
                FontWeight::Semibold,
                CARD_WHITE,
                1.0,
            );
        }
    }

    video_stage
}

#[allow(clippy::too_many_arguments)]
fn draw_right_column_cards(
    canvas: &mut UiCanvas,
    hit_zones: &mut Vec<HitZone>,
    mx: f32,
    my: f32,
    col_x: f32,
    col_w: f32,
    state: &DashboardViewState<'_>,
) {
    let col_cx = col_x + col_w * 0.5;

    // ========================================================================
    // Card 1: Recording Card (y = 72, h = 236)
    // ========================================================================
    let rec_y = 72.0;
    let rec_h = 236.0;
    canvas.draw_rounded_card(
        col_x,
        rec_y,
        col_w,
        rec_h,
        20.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.05,
        18.0,
        5.0,
        1.0,
    );

    // Header: Red dot + "Recording" + Right status badge ("Off" / "ON" / "Auto")
    let rec_hdr_cy = rec_y + 34.0;
    canvas.draw_circle(col_x + 28.0, rec_hdr_cy, 7.5, ERROR_BG, 1.0);
    canvas.draw_circle(col_x + 28.0, rec_hdr_cy, 4.8, ERROR_RED, 1.0);

    let hdr_cap = canvas.cap_height(16.0, FontWeight::Bold);
    canvas.draw_text_baseline(
        "Recording",
        col_x + 44.0,
        rec_hdr_cy + hdr_cap * 0.5,
        16.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    let (badge_label, badge_bg, badge_fg) = if state.is_recording {
        ("ON", ERROR_BG, ERROR_RED)
    } else if state.settings.auto_record {
        ("Auto", WARNING_AMBER_SOFT, WARNING_AMBER)
    } else {
        ("Off", BADGE_OFF_BG, TEXT_BODY)
    };
    let badge_w = 50.0;
    let badge_h = 28.0;
    let badge_x = col_x + col_w - badge_w - 18.0;
    let badge_y = rec_hdr_cy - badge_h * 0.5;
    canvas.draw_rounded_card(
        badge_x,
        badge_y,
        badge_w,
        badge_h,
        14.0,
        badge_bg,
        badge_bg,
        0.0,
        SHADOW_SLATE,
        0.0,
        0.0,
        0.0,
        1.0,
    );
    let bw_txt = canvas.measure_text_width(badge_label, 12.2, FontWeight::Semibold);
    let b_cap = canvas.cap_height(12.2, FontWeight::Semibold);
    canvas.draw_text_baseline(
        badge_label,
        badge_x + (badge_w - bw_txt) * 0.5,
        badge_y + (badge_h + b_cap) * 0.5,
        12.2,
        FontWeight::Semibold,
        badge_fg,
        1.0,
    );

    // Large Tabular Timer: "00:00:00"
    let timer_str = format_hh_mm_ss(if state.is_recording {
        state.rec_elapsed_secs
    } else {
        0
    });
    canvas.draw_text_centered(
        &timer_str,
        col_cx,
        rec_y + 68.0,
        29.0,
        FontWeight::Bold,
        if state.is_recording {
            ERROR_RED
        } else {
            TEXT_PRIMARY
        },
        1.0,
    );

    // Status subtext below timer
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
        canvas.fit_text_to_width(rec_subtext, col_w - 28.0, 12.8, FontWeight::Regular);
    canvas.draw_text_centered(
        &fitted_rec_sub,
        col_cx,
        rec_y + 118.0,
        12.8,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // Action Button: "[ ● Start Recording ]"
    let rec_btn_rect = SDL_FRect {
        x: col_x + 18.0,
        y: rec_y + 160.0,
        w: col_w - 36.0,
        h: 50.0,
    };
    let rec_btn_hover = point_in_rect(mx, my, &rec_btn_rect);
    let rec_btn_fill = if rec_btn_hover {
        ERROR_BG_HOVER
    } else {
        ERROR_BG
    };
    canvas.draw_rounded_card(
        rec_btn_rect.x,
        rec_btn_rect.y,
        rec_btn_rect.w,
        rec_btn_rect.h,
        25.0,
        rec_btn_fill,
        Rgb(248, 212, 218),
        0.8,
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
    let rbl_w = canvas.measure_text_width(rec_btn_label, 14.0, FontWeight::Semibold);
    let rbl_cap = canvas.cap_height(14.0, FontWeight::Semibold);
    let rbl_total_w = 18.0 + rbl_w;
    let rbl_start_x = rec_btn_rect.x + (rec_btn_rect.w - rbl_total_w) * 0.5;
    let rbl_cy = rec_btn_rect.y + rec_btn_rect.h * 0.5;

    canvas.draw_circle(rbl_start_x + 5.0, rbl_cy, 7.2, Rgb(250, 198, 205), 1.0);
    canvas.draw_circle(rbl_start_x + 5.0, rbl_cy, 4.6, ERROR_RED, 1.0);
    canvas.draw_text_baseline(
        rec_btn_label,
        rbl_start_x + 18.0,
        rbl_cy + rbl_cap * 0.5,
        14.0,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

    hit_zones.push(HitZone {
        rect: rec_btn_rect,
        action: UiAction::ToggleRecord,
    });

    // ========================================================================
    // Card 2: Mirror Quality Card (y = 324, h = 150)
    // ========================================================================
    let qual_y = 324.0;
    let qual_h = 150.0;
    canvas.draw_rounded_card(
        col_x,
        qual_y,
        col_w,
        qual_h,
        20.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.05,
        18.0,
        5.0,
        1.0,
    );

    let qual_hdr_cy = qual_y + 30.0;
    canvas.draw_gear_icon(col_x + 28.0, qual_hdr_cy, 8.2, CARD_WHITE, TEXT_BODY, 1.0);
    canvas.draw_text_baseline(
        "Mirror Quality",
        col_x + 44.0,
        qual_hdr_cy + hdr_cap * 0.5,
        16.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    // 3-Way Segmented Control: [ Auto | HD | FHD ]
    let seg_x = col_x + 18.0;
    let seg_y = qual_y + 54.0;
    let seg_w = col_w - 36.0;
    let seg_h = 42.0;
    canvas.draw_rounded_card(
        seg_x,
        seg_y,
        seg_w,
        seg_h,
        21.0,
        PILL_NEUTRAL,
        BORDER_SUBTLE,
        0.8,
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
            canvas.draw_gradient_rounded_card(
                q_rect.x,
                q_rect.y,
                q_rect.w,
                q_rect.h,
                21.0,
                BLUE_GRAD_TOP,
                APPLE_BLUE,
                APPLE_BLUE,
                0.0,
                APPLE_BLUE,
                0.18,
                8.0,
                2.0,
                1.0,
            );
        } else if is_hov {
            canvas.draw_rounded_card(
                q_rect.x + 2.0,
                q_rect.y + 2.0,
                q_rect.w - 4.0,
                q_rect.h - 4.0,
                19.0,
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

        // Subtle divider between unselected segments
        if idx == 1 && state.settings.quality == MirrorQuality::Auto {
            canvas.fill_rect(
                (q_rect.x + q_rect.w) as i32,
                (seg_y + 11.0) as i32,
                1,
                20,
                BORDER_SUBTLE,
            );
        } else if idx == 0 && state.settings.quality == MirrorQuality::Fhd {
            canvas.fill_rect(
                (q_rect.x + q_rect.w) as i32,
                (seg_y + 11.0) as i32,
                1,
                20,
                BORDER_SUBTLE,
            );
        }

        let lbl = q.label();
        let weight = if is_sel {
            FontWeight::Semibold
        } else {
            FontWeight::Regular
        };
        let fg = if is_sel { CARD_WHITE } else { TEXT_PRIMARY };
        let lw = canvas.measure_text_width(lbl, 13.8, weight);
        let l_cap = canvas.cap_height(13.8, weight);
        canvas.draw_text_baseline(
            lbl,
            q_rect.x + (q_rect.w - lw) * 0.5,
            q_rect.y + (q_rect.h + l_cap) * 0.5,
            13.8,
            weight,
            fg,
            1.0,
        );

        hit_zones.push(HitZone {
            rect: q_rect,
            action: UiAction::SelectQuality(q),
        });
    }

    let q_caption = canvas.fit_text_to_width(
        state.settings.quality.caption(),
        col_w - 24.0,
        12.2,
        FontWeight::Regular,
    );
    canvas.draw_text_centered(
        &q_caption,
        col_cx,
        qual_y + 110.0,
        12.2,
        FontWeight::Regular,
        TEXT_SECONDARY,
        1.0,
    );

    // ========================================================================
    // Card 3: Quick Tools Card (y = 490, h = 216)
    // ========================================================================
    let qt_y = 490.0;
    let qt_h = 216.0;
    canvas.draw_rounded_card(
        col_x,
        qt_y,
        col_w,
        qt_h,
        20.0,
        CARD_WHITE,
        CARD_BORDER,
        1.0,
        SHADOW_SLATE,
        0.05,
        18.0,
        5.0,
        1.0,
    );

    let qt_hdr_cy = qt_y + 30.0;
    canvas.draw_wrench_screwdriver_icon(col_x + 28.0, qt_hdr_cy, TEXT_BODY, 1.0);
    canvas.draw_text_baseline(
        "Quick Tools",
        col_x + 44.0,
        qt_hdr_cy + hdr_cap * 0.5,
        16.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    let tool_rows = [
        (0, "Open Recordings", qt_y + 56.0, UiAction::OpenRecordingsFolder),
        (1, "Settings", qt_y + 106.0, UiAction::SwitchTab(NavTab::Settings)),
        (2, "Help & Support", qt_y + 156.0, UiAction::SwitchTab(NavTab::About)),
    ];

    for (icon_kind, label, ry, action) in tool_rows {
        let r_rect = SDL_FRect {
            x: col_x + 18.0,
            y: ry,
            w: col_w - 36.0,
            h: 42.0,
        };
        let is_hov = point_in_rect(mx, my, &r_rect);
        let row_bg = if is_hov {
            PILL_NEUTRAL_HOVER
        } else {
            PILL_NEUTRAL
        };
        canvas.draw_rounded_card(
            r_rect.x,
            r_rect.y,
            r_rect.w,
            r_rect.h,
            12.0,
            row_bg,
            row_bg,
            0.0,
            SHADOW_SLATE,
            0.0,
            0.0,
            0.0,
            1.0,
        );

        let r_cy = r_rect.y + r_rect.h * 0.5;
        let icon_cx = r_rect.x + 20.0;
        match icon_kind {
            0 => canvas.draw_folder_icon(icon_cx, r_cy, TEXT_BODY, 1.0),
            1 => canvas.draw_gear_icon(icon_cx, r_cy, 7.6, row_bg, TEXT_BODY, 1.0),
            _ => canvas.draw_help_circle_icon(icon_cx, r_cy, TEXT_BODY, 1.0),
        }

        let r_cap = canvas.cap_height(13.6, FontWeight::Semibold);
        canvas.draw_text_baseline(
            label,
            r_rect.x + 38.0,
            r_cy + r_cap * 0.5,
            13.6,
            FontWeight::Semibold,
            TEXT_PRIMARY,
            1.0,
        );

        canvas.draw_chevron_right(
            r_rect.x + r_rect.w - 16.0,
            r_cy,
            4.4,
            1.7,
            TEXT_SECONDARY,
            0.9,
        );

        hit_zones.push(HitZone {
            rect: r_rect,
            action,
        });
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

    // 1. Hero 3D Laptop + Signal Waves + Smartphone Illustration
    canvas.draw_hero_laptop_phone_illustration(cx, card_y + 30.0, state.is_connecting);

    // 2. Hero Title: "Mobile Mirror"
    canvas.draw_text_centered(
        "Mobile Mirror",
        cx,
        card_y + 292.0,
        29.0,
        FontWeight::Bold,
        TEXT_PRIMARY,
        1.0,
    );

    // 3. Hero Subtitle: "Mirror and record your phone screen on your laptop"
    let subtitle = "Mirror and record your phone screen on your laptop";
    let fitted_sub = canvas.fit_text_to_width(subtitle, card_w - 40.0, 15.0, FontWeight::Regular);
    canvas.draw_text_centered(
        &fitted_sub,
        cx,
        card_y + 344.0,
        15.0,
        FontWeight::Regular,
        TEXT_BODY,
        1.0,
    );

    // 4. Primary CTA Pill Button: "[ ▶  Start Mirroring ]"
    let btn_w = 256.0;
    let btn_h = 56.0;
    let btn_rect = SDL_FRect {
        x: (cx - btn_w * 0.5).round(),
        y: (card_y + 398.0).round(),
        w: btn_w,
        h: btn_h,
    };
    let btn_hover = point_in_rect(mx, my, &btn_rect);
    let (grad_top, grad_bot) = if btn_hover {
        (BLUE_GRAD_HOVER_TOP, BLUE_GRAD_HOVER_BOT)
    } else {
        (BLUE_GRAD_TOP, BLUE_GRAD_BOT)
    };

    canvas.draw_gradient_rounded_card(
        btn_rect.x,
        btn_rect.y,
        btn_rect.w,
        btn_rect.h,
        btn_h * 0.5,
        grad_top,
        grad_bot,
        APPLE_BLUE,
        0.0,
        APPLE_BLUE,
        0.28,
        18.0,
        6.0,
        1.0,
    );

    let btn_label = if state.is_connecting {
        "Connecting..."
    } else {
        "Start Mirroring"
    };
    let bl_w = canvas.measure_text_width(btn_label, 15.8, FontWeight::Semibold);
    let bl_cap = canvas.cap_height(15.8, FontWeight::Semibold);
    let total_w = 22.0 + bl_w;
    let start_x = btn_rect.x + (btn_rect.w - total_w) * 0.5;
    let btn_cy = btn_rect.y + btn_rect.h * 0.5;

    canvas.draw_play_icon(start_x + 6.0, btn_cy, 11.5, CARD_WHITE, 1.0);
    canvas.draw_text_baseline(
        btn_label,
        start_x + 22.0,
        btn_cy + bl_cap * 0.5,
        15.8,
        FontWeight::Semibold,
        CARD_WHITE,
        1.0,
    );

    hit_zones.push(HitZone {
        rect: btn_rect,
        action: UiAction::StartMirroring,
    });

    // 5. Bottom Info Banner ("Connect your phone via USB" + "How to Enable? >")
    let banner_h = 84.0;
    let banner_x = card_x + 18.0;
    let banner_w = (card_w - 36.0).max(260.0);
    let banner_y = (card_y + card_h - banner_h - 22.0).max(btn_rect.y + btn_rect.h + 22.0);

    canvas.draw_rounded_card(
        banner_x,
        banner_y,
        banner_w,
        banner_h,
        16.0,
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
    canvas.draw_phone_outline_icon(banner_x + 28.0, b_cy, TEXT_BODY, 1.0);

    // Right link button: "How to Enable? >"
    let link_label = "How to Enable?";
    let link_w = canvas.measure_text_width(link_label, 12.8, FontWeight::Semibold);
    let link_rect = SDL_FRect {
        x: banner_x + banner_w - link_w - 28.0,
        y: banner_y + 20.0,
        w: link_w + 22.0,
        h: 44.0,
    };
    let link_hover = point_in_rect(mx, my, &link_rect);
    let link_col = if link_hover {
        BLUE_GRAD_HOVER_BOT
    } else {
        APPLE_BLUE
    };
    let link_cap = canvas.cap_height(12.8, FontWeight::Semibold);
    canvas.draw_text_baseline(
        link_label,
        link_rect.x + 2.0,
        b_cy + link_cap * 0.5,
        12.8,
        FontWeight::Semibold,
        link_col,
        1.0,
    );
    canvas.draw_chevron_right(
        link_rect.x + link_w + 11.0,
        b_cy,
        4.2,
        1.7,
        link_col,
        1.0,
    );
    hit_zones.push(HitZone {
        rect: link_rect,
        action: UiAction::SwitchTab(NavTab::About),
    });

    // Middle text lines inside Banner
    let text_x = banner_x + 52.0;
    let max_text_w = (link_rect.x - text_x - 8.0).max(120.0);
    let title_color = if state.is_warning {
        WARNING_AMBER
    } else if state.is_connecting {
        APPLE_BLUE
    } else {
        TEXT_PRIMARY
    };
    let line1 = canvas.fit_text_to_width(state.headline, max_text_w, 14.2, FontWeight::Bold);
    let line2 = canvas.fit_text_to_width(state.subtext, max_text_w, 12.4, FontWeight::Regular);

    canvas.draw_text(
        &line1,
        text_x,
        banner_y + 20.0,
        14.2,
        FontWeight::Bold,
        title_color,
        1.0,
    );
    canvas.draw_text(
        &line2,
        text_x,
        banner_y + 46.0,
        12.4,
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
    let meta = format!(
        "Live Mirror  •  {}×{}  •  {} FPS",
        state.stream_width, state.stream_height, state.fps
    );
    let m_cap = canvas.cap_height(13.8, FontWeight::Semibold);
    canvas.draw_text_baseline(
        &meta,
        card_x + 38.0,
        bar_cy + m_cap * 0.5,
        13.8,
        FontWeight::Semibold,
        TEXT_PRIMARY,
        1.0,
    );

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
        action: UiAction::ToggleFullscreenMirror,
    });

    // Disconnect pill
    let stop_bg = if point_in_rect(mx, my, &stop_rect) {
        ERROR_BG_HOVER
    } else {
        ERROR_BG
