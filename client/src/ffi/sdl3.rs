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
pub const SDL_WINDOW_RESIZABLE: u64 = 0x0000_0000_0000_0020;
pub const SDL_WINDOW_HIGH_PIXEL_DENSITY: u64 = 0x0000_0000_0000_2000;

// SDL3 PixelFormat values for planar/semi-planar YUV
pub const SDL_PIXELFORMAT_IYUV: u32 = 0x5655_5949; // Planar Y + U + V (4:2:0)
pub const SDL_PIXELFORMAT_NV12: u32 = 0x3231_564E; // Planar Y + interleaved UV (4:2:0, NVDEC/DXVA2 native)

// SDL3 TextureAccess
pub const SDL_TEXTUREACCESS_STREAMING: c_int = 1;

// SDL3 Event types
pub const SDL_EVENT_QUIT: u32 = 0x100;
pub const SDL_EVENT_WINDOW_CLOSE_REQUESTED: u32 = 0x210;
pub const SDL_EVENT_KEY_DOWN: u32 = 0x300;

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
    pub sdl_set_window_position: unsafe extern "C" fn(*mut SDL_Window, c_int, c_int) -> bool,
    pub sdl_create_renderer:
        unsafe extern "C" fn(*mut SDL_Window, *const c_char) -> *mut SDL_Renderer,
    pub sdl_destroy_renderer: unsafe extern "C" fn(*mut SDL_Renderer),
    pub sdl_set_render_vsync: unsafe extern "C" fn(*mut SDL_Renderer, c_int) -> bool,
    pub sdl_create_texture: unsafe extern "C" fn(
        *mut SDL_Renderer,
        u32,
        c_int,
        c_int,
        c_int,
    ) -> *mut SDL_Texture,
    pub sdl_destroy_texture: unsafe extern "C" fn(*mut SDL_Texture),
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
    pub sdl_render_fill_rect: unsafe extern "C" fn(*mut SDL_Renderer, *const SDL_FRect) -> bool,
    pub sdl_render_clear: unsafe extern "C" fn(*mut SDL_Renderer) -> bool,
    pub sdl_render_texture: unsafe extern "C" fn(
        *mut SDL_Renderer,
        *mut SDL_Texture,
        *const SDL_FRect,
        *const SDL_FRect,
    ) -> bool,
    pub sdl_render_present: unsafe extern "C" fn(*mut SDL_Renderer) -> bool,
    pub sdl_poll_event: unsafe extern "C" fn(*mut SDL_Event) -> bool,
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
                sdl_set_window_position: lib.get_sym("SDL_SetWindowPosition")?,
                sdl_create_renderer: lib.get_sym("SDL_CreateRenderer")?,
                sdl_destroy_renderer: lib.get_sym("SDL_DestroyRenderer")?,
                sdl_set_render_vsync: lib.get_sym("SDL_SetRenderVSync")?,
                sdl_create_texture: lib.get_sym("SDL_CreateTexture")?,
                sdl_destroy_texture: lib.get_sym("SDL_DestroyTexture")?,
                sdl_update_nv_texture: lib.get_sym("SDL_UpdateNVTexture")?,
                sdl_update_yuv_texture: lib.get_sym("SDL_UpdateYUVTexture")?,
                sdl_set_render_draw_color: lib.get_sym("SDL_SetRenderDrawColor")?,
                sdl_render_fill_rect: lib.get_sym("SDL_RenderFillRect")?,
                sdl_render_clear: lib.get_sym("SDL_RenderClear")?,
                sdl_render_texture: lib.get_sym("SDL_RenderTexture")?,
                sdl_render_present: lib.get_sym("SDL_RenderPresent")?,
                sdl_poll_event: lib.get_sym("SDL_PollEvent")?,
                sdl_open_audio_device_stream: lib.get_sym("SDL_OpenAudioDeviceStream")?,
                sdl_put_audio_stream_data: lib.get_sym("SDL_PutAudioStreamData")?,
                sdl_resume_audio_stream_device: lib.get_sym("SDL_ResumeAudioStreamDevice")?,
                sdl_destroy_audio_stream: lib.get_sym("SDL_DestroyAudioStream")?,
                _lib: lib,
            })
        }
    }
}
