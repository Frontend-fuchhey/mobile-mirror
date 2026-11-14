// [Mobile Mirror] Progressive implementation phase 1/2
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
