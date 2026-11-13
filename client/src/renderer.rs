// [Mobile Mirror] Progressive implementation phase 2/8
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
