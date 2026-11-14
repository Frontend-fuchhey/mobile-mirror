#![allow(non_camel_case_types, dead_code)]

use crate::ffi::DynLib;
use std::ffi::{c_char, c_int, c_void};

pub const SDL_INIT_VIDEO: u32 = 0x0000_0020;
pub const SDL_INIT_AUDIO: u32 = 0x0000_0010;
pub const SDL_AUDIO_S16LE: u32 = 0x8010;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SDL_AudioSpec {
    pub format: u32,
    pub channels: c_int,
    pub freq: c_int,
}

#[repr(C)]
pub struct SDL_AudioStream {
    _opaque: [u8; 0],
}
pub const SDL_WINDOW_FULLSCREEN: u64 = 0x0000_0000_0000_0001;
pub const SDL_WINDOW_BORDERLESS: u64 = 0x0000_0000_0000_0010;
pub const SDL_WINDOW_RESIZABLE: u64 = 0x0000_0000_0000_0020;
pub const SDL_WINDOW_MINIMIZED: u64 = 0x0000_0000_0000_0040;
pub const SDL_WINDOW_MAXIMIZED: u64 = 0x0000_0000_0000_0080;
pub const SDL_WINDOW_HIGH_PIXEL_DENSITY: u64 = 0x0000_0000_0000_2000;
pub const SDL_WINDOW_ALWAYS_ON_TOP: u64 = 0x0000_0000_0001_0000;

// SDL3 Keycodes & Scancodes
pub const SDL_SCANCODE_F11: u32 = 68;
pub const SDLK_F11: u32 = 0x4000_0044;

// SDL3 HitTestResult constants for borderless custom titlebar dragging & edge resizing
pub const SDL_HITTEST_NORMAL: c_int = 0;
pub const SDL_HITTEST_DRAGGABLE: c_int = 1;
pub const SDL_HITTEST_RESIZE_TOPLEFT: c_int = 2;
pub const SDL_HITTEST_RESIZE_TOP: c_int = 3;
pub const SDL_HITTEST_RESIZE_TOPRIGHT: c_int = 4;
pub const SDL_HITTEST_RESIZE_RIGHT: c_int = 5;
pub const SDL_HITTEST_RESIZE_BOTTOMRIGHT: c_int = 6;
pub const SDL_HITTEST_RESIZE_BOTTOM: c_int = 7;
pub const SDL_HITTEST_RESIZE_BOTTOMLEFT: c_int = 8;
pub const SDL_HITTEST_RESIZE_LEFT: c_int = 9;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SDL_Point {
    pub x: c_int,
    pub y: c_int,
}

pub type SDL_HitTest =
    unsafe extern "C" fn(*mut SDL_Window, *const SDL_Point, *mut c_void) -> c_int;

// SDL3 PixelFormat values for planar/semi-planar YUV and 32-bit ARGB UI canvas
pub const SDL_PIXELFORMAT_ARGB8888: u32 = 0x1636_2004; // 0xAARRGGBB u32 in native endian
pub const SDL_PIXELFORMAT_IYUV: u32 = 0x5655_5949; // Planar Y + U + V (4:2:0)
pub const SDL_PIXELFORMAT_NV12: u32 = 0x3231_564E; // Planar Y + interleaved UV (4:2:0, NVDEC/DXVA2 native)

// SDL3 TextureAccess
pub const SDL_TEXTUREACCESS_STREAMING: c_int = 1;

// SDL3 Event types
pub const SDL_EVENT_QUIT: u32 = 0x100;
pub const SDL_EVENT_WINDOW_CLOSE_REQUESTED: u32 = 0x210;
pub const SDL_EVENT_KEY_DOWN: u32 = 0x300;
pub const SDL_EVENT_MOUSE_MOTION: u32 = 0x400;
pub const SDL_EVENT_MOUSE_BUTTON_DOWN: u32 = 0x401;
pub const SDL_EVENT_MOUSE_BUTTON_UP: u32 = 0x402;
pub const SDL_EVENT_MOUSE_WHEEL: u32 = 0x403;
pub const SDL_BUTTON_LMASK: u32 = 1 << 0;

#[repr(C)]
pub struct SDL_Window {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct SDL_Renderer {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct SDL_Texture {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct SDL_FRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[repr(C)]
pub struct SDL_Event {
    pub event_type: u32,
    pub _padding: [u8; 124],
}

impl SDL_Event {
    pub fn scancode(&self) -> u32 {
        if self.event_type == SDL_EVENT_KEY_DOWN && self._padding.len() >= 24 {
            let s0 = self._padding[20];
            let s1 = self._padding[21];
            let s2 = self._padding[22];
            let s3 = self._padding[23];
            return u32::from_ne_bytes([s0, s1, s2, s3]);
        }
        0
    }

    pub fn keycode(&self) -> u32 {
        if self.event_type == SDL_EVENT_KEY_DOWN {
            // In SDL3 SDL_KeyboardEvent:
            // offset 0: type (u32)
            // offset 4: reserved (u32)
            // offset 8: timestamp (u64)
            // offset 16: windowID (u32)
            // offset 20: which (u32)
            // offset 24: scancode (u32)
            // offset 28: key (u32)
            if self._padding.len() >= 28 {
                let k0 = self._padding[24];
                let k1 = self._padding[25];
                let k2 = self._padding[26];
                let k3 = self._padding[27];
                return u32::from_ne_bytes([k0, k1, k2, k3]);
            }
        }
        0
    }

    pub fn is_key_repeat(&self) -> bool {
        if self.event_type == SDL_EVENT_KEY_DOWN && self._padding.len() >= 34 {
            return self._padding[33] != 0;
        }
        false
    }
}

/// Function pointer table for SDL3 (`SDL3.dll` / `libSDL3.so` / `libSDL3.dylib`).
pub struct Sdl3Api {
    _lib: DynLib,
    pub sdl_init: unsafe extern "C" fn(u32) -> bool,
    pub sdl_quit: unsafe extern "C" fn(),
    pub sdl_get_error: unsafe extern "C" fn() -> *const c_char,
    pub sdl_create_window: unsafe extern "C" fn(*const c_char, c_int, c_int, u64) -> *mut SDL_Window,
    pub sdl_destroy_window: unsafe extern "C" fn(*mut SDL_Window),
    pub sdl_set_window_title: unsafe extern "C" fn(*mut SDL_Window, *const c_char) -> bool,
    pub sdl_set_window_size: unsafe extern "C" fn(*mut SDL_Window, c_int, c_int) -> bool,
    pub sdl_get_window_size: unsafe extern "C" fn(*mut SDL_Window, *mut c_int, *mut c_int) -> bool,
    pub sdl_get_window_size_in_pixels:
        Option<unsafe extern "C" fn(*mut SDL_Window, *mut c_int, *mut c_int) -> bool>,
    pub sdl_get_window_display_scale: Option<unsafe extern "C" fn(*mut SDL_Window) -> f32>,
    pub sdl_set_window_position: unsafe extern "C" fn(*mut SDL_Window, c_int, c_int) -> bool,
    pub sdl_minimize_window: Option<unsafe extern "C" fn(*mut SDL_Window) -> bool>,
    pub sdl_maximize_window: Option<unsafe extern "C" fn(*mut SDL_Window) -> bool>,
    pub sdl_restore_window: Option<unsafe extern "C" fn(*mut SDL_Window) -> bool>,
    pub sdl_set_window_fullscreen: Option<unsafe extern "C" fn(*mut SDL_Window, bool) -> bool>,
    pub sdl_sync_window: Option<unsafe extern "C" fn(*mut SDL_Window) -> bool>,
    pub sdl_get_window_flags: Option<unsafe extern "C" fn(*mut SDL_Window) -> u64>,
    pub sdl_set_window_always_on_top: Option<unsafe extern "C" fn(*mut SDL_Window, bool) -> bool>,
    pub sdl_set_window_bordered: Option<unsafe extern "C" fn(*mut SDL_Window, bool) -> bool>,
    pub sdl_set_window_hit_test:
        Option<unsafe extern "C" fn(*mut SDL_Window, Option<SDL_HitTest>, *mut c_void) -> bool>,
    pub sdl_create_renderer:
        unsafe extern "C" fn(*mut SDL_Window, *const c_char) -> *mut SDL_Renderer,
    pub sdl_destroy_renderer: unsafe extern "C" fn(*mut SDL_Renderer),
    pub sdl_set_render_vsync: unsafe extern "C" fn(*mut SDL_Renderer, c_int) -> bool,
    pub sdl_set_render_scale: unsafe extern "C" fn(*mut SDL_Renderer, f32, f32) -> bool,
    pub sdl_create_texture: unsafe extern "C" fn(
        *mut SDL_Renderer,
        u32,
        c_int,
        c_int,
        c_int,
    ) -> *mut SDL_Texture,
    pub sdl_destroy_texture: unsafe extern "C" fn(*mut SDL_Texture),
    pub sdl_update_texture: unsafe extern "C" fn(
        *mut SDL_Texture,
        *const c_void,
        *const c_void,
        c_int,
    ) -> bool,
    pub sdl_update_nv_texture: unsafe extern "C" fn(
        *mut SDL_Texture,
        *const c_void,
        *const u8,
        c_int,
        *const u8,
        c_int,
    ) -> bool,
    pub sdl_update_yuv_texture: unsafe extern "C" fn(
        *mut SDL_Texture,
        *const c_void,
        *const u8,
        c_int,
        *const u8,
        c_int,
        *const u8,
        c_int,
    ) -> bool,
    pub sdl_set_render_draw_color: unsafe extern "C" fn(*mut SDL_Renderer, u8, u8, u8, u8) -> bool,
    pub sdl_render_rect: unsafe extern "C" fn(*mut SDL_Renderer, *const SDL_FRect) -> bool,
    pub sdl_render_fill_rect: unsafe extern "C" fn(*mut SDL_Renderer, *const SDL_FRect) -> bool,
    pub sdl_render_fill_rects:
        unsafe extern "C" fn(*mut SDL_Renderer, *const SDL_FRect, c_int) -> bool,
    pub sdl_render_debug_text:
        unsafe extern "C" fn(*mut SDL_Renderer, f32, f32, *const c_char) -> bool,
    pub sdl_render_clear: unsafe extern "C" fn(*mut SDL_Renderer) -> bool,
    pub sdl_render_texture: unsafe extern "C" fn(
        *mut SDL_Renderer,
        *mut SDL_Texture,
        *const SDL_FRect,
        *const SDL_FRect,
    ) -> bool,
    pub sdl_render_present: unsafe extern "C" fn(*mut SDL_Renderer) -> bool,
    pub sdl_poll_event: unsafe extern "C" fn(*mut SDL_Event) -> bool,
    pub sdl_get_mouse_state: unsafe extern "C" fn(*mut f32, *mut f32) -> u32,
    pub sdl_open_audio_device_stream: unsafe extern "C" fn(
        u32,
        *const SDL_AudioSpec,
        Option<unsafe extern "C" fn(*mut c_void, *mut SDL_AudioStream, c_int, c_int)>,
        *mut c_void,
    ) -> *mut SDL_AudioStream,
    pub sdl_put_audio_stream_data: unsafe extern "C" fn(*mut SDL_AudioStream, *const c_void, c_int) -> bool,
    pub sdl_resume_audio_stream_device: unsafe extern "C" fn(*mut SDL_AudioStream) -> bool,
    pub sdl_destroy_audio_stream: unsafe extern "C" fn(*mut SDL_AudioStream),
}

impl Sdl3Api {
    pub fn load() -> Result<Self, String> {
        let lib = DynLib::open_any(&[
            "SDL3.dll",
            "libSDL3.so.0",
            "libSDL3.so",
            "libSDL3.0.dylib",
            "libSDL3.dylib",
        ])?;

        unsafe {
            Ok(Self {
                sdl_init: lib.get_sym("SDL_Init")?,
                sdl_quit: lib.get_sym("SDL_Quit")?,
                sdl_get_error: lib.get_sym("SDL_GetError")?,
                sdl_create_window: lib.get_sym("SDL_CreateWindow")?,
                sdl_destroy_window: lib.get_sym("SDL_DestroyWindow")?,
                sdl_set_window_title: lib.get_sym("SDL_SetWindowTitle")?,
                sdl_set_window_size: lib.get_sym("SDL_SetWindowSize")?,
                sdl_get_window_size: lib.get_sym("SDL_GetWindowSize")?,
                sdl_get_window_size_in_pixels: lib.get_sym("SDL_GetWindowSizeInPixels").ok(),
                sdl_get_window_display_scale: lib.get_sym("SDL_GetWindowDisplayScale").ok(),
                sdl_set_window_position: lib.get_sym("SDL_SetWindowPosition")?,
                sdl_minimize_window: lib.get_sym("SDL_MinimizeWindow").ok(),
                sdl_maximize_window: lib.get_sym("SDL_MaximizeWindow").ok(),
                sdl_restore_window: lib.get_sym("SDL_RestoreWindow").ok(),
                sdl_set_window_fullscreen: lib.get_sym("SDL_SetWindowFullscreen").ok(),
                sdl_sync_window: lib.get_sym("SDL_SyncWindow").ok(),
                sdl_get_window_flags: lib.get_sym("SDL_GetWindowFlags").ok(),
                sdl_set_window_always_on_top: lib.get_sym("SDL_SetWindowAlwaysOnTop").ok(),
                sdl_set_window_bordered: lib.get_sym("SDL_SetWindowBordered").ok(),
                sdl_set_window_hit_test: lib.get_sym("SDL_SetWindowHitTest").ok(),
                sdl_create_renderer: lib.get_sym("SDL_CreateRenderer")?,
                sdl_destroy_renderer: lib.get_sym("SDL_DestroyRenderer")?,
                sdl_set_render_vsync: lib.get_sym("SDL_SetRenderVSync")?,
                sdl_set_render_scale: lib.get_sym("SDL_SetRenderScale")?,
                sdl_create_texture: lib.get_sym("SDL_CreateTexture")?,
                sdl_destroy_texture: lib.get_sym("SDL_DestroyTexture")?,
                sdl_update_texture: lib.get_sym("SDL_UpdateTexture")?,
                sdl_update_nv_texture: lib.get_sym("SDL_UpdateNVTexture")?,
                sdl_update_yuv_texture: lib.get_sym("SDL_UpdateYUVTexture")?,
                sdl_set_render_draw_color: lib.get_sym("SDL_SetRenderDrawColor")?,
                sdl_render_rect: lib.get_sym("SDL_RenderRect")?,
                sdl_render_fill_rect: lib.get_sym("SDL_RenderFillRect")?,
                sdl_render_fill_rects: lib.get_sym("SDL_RenderFillRects")?,
                sdl_render_debug_text: lib.get_sym("SDL_RenderDebugText")?,
                sdl_render_clear: lib.get_sym("SDL_RenderClear")?,
                sdl_render_texture: lib.get_sym("SDL_RenderTexture")?,
                sdl_render_present: lib.get_sym("SDL_RenderPresent")?,
                sdl_poll_event: lib.get_sym("SDL_PollEvent")?,
                sdl_get_mouse_state: lib.get_sym("SDL_GetMouseState")?,
                sdl_open_audio_device_stream: lib.get_sym("SDL_OpenAudioDeviceStream")?,
                sdl_put_audio_stream_data: lib.get_sym("SDL_PutAudioStreamData")?,
                sdl_resume_audio_stream_device: lib.get_sym("SDL_ResumeAudioStreamDevice")?,
                sdl_destroy_audio_stream: lib.get_sym("SDL_DestroyAudioStream")?,
                _lib: lib,
            })
        }
    }
}
