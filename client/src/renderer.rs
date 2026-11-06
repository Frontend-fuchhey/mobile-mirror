use crate::decoder::{DecodedFrameView, PixelLayout};
use crate::ffi::sdl3::{
    Sdl3Api, SDL_AudioSpec, SDL_AudioStream, SDL_Event, SDL_Renderer, SDL_Texture, SDL_Window,
    SDL_AUDIO_S16LE, SDL_EVENT_QUIT, SDL_EVENT_WINDOW_CLOSE_REQUESTED, SDL_INIT_AUDIO,
    SDL_INIT_VIDEO, SDL_PIXELFORMAT_IYUV, SDL_PIXELFORMAT_NV12, SDL_TEXTUREACCESS_STREAMING,
    SDL_WINDOW_HIGH_PIXEL_DENSITY, SDL_WINDOW_RESIZABLE,
};
use std::ffi::{CStr, CString};
use std::ptr;

/// Ultra-low-latency SDL3 window and GPU YUV/NV12 texture renderer.
pub struct Sdl3Renderer {
    api: Sdl3Api,
    window: *mut SDL_Window,
    renderer: *mut SDL_Renderer,
    texture: *mut SDL_Texture,
    tex_width: u32,
    tex_height: u32,
    tex_layout: Option<PixelLayout>,
    last_record_toggle: std::time::Instant,
    last_mic_toggle: std::time::Instant,
}

impl Sdl3Renderer {
    pub fn new(title: &str, initial_width: u32, initial_height: u32) -> Result<Self, String> {
        let api = Sdl3Api::load()?;

        unsafe {
            if !(api.sdl_init)(SDL_INIT_VIDEO | SDL_INIT_AUDIO) {
                return Err(format!("SDL_Init failed: {}", Self::last_error(&api)));
            }

            // Scale initial window dimensions to fit comfortable desktop viewport based on orientation
            let (win_w, win_h) = calculate_target_window_size(initial_width, initial_height);
            let c_title = CString::new(title).unwrap();
            let window = (api.sdl_create_window)(
                c_title.as_ptr(),
                win_w as i32,
                win_h as i32,
                SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY,
            );
            if window.is_null() {
                (api.sdl_quit)();
                return Err(format!(
                    "SDL_CreateWindow failed: {}",
                    Self::last_error(&api)
                ));
            }

            let renderer = (api.sdl_create_renderer)(window, ptr::null());
            if renderer.is_null() {
                (api.sdl_destroy_window)(window);
                (api.sdl_quit)();
                return Err(format!(
                    "SDL_CreateRenderer failed: {}",
                    Self::last_error(&api)
                ));
            }

            // Disable VSync for immediate sub-millisecond frame presentation
            let _ = (api.sdl_set_render_vsync)(renderer, 0);

            Ok(Self {
                api,
                window,
                renderer,
                texture: ptr::null_mut(),
                tex_width: 0,
                tex_height: 0,
                tex_layout: None,
                last_record_toggle: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(2))
                    .unwrap_or_else(std::time::Instant::now),
                last_mic_toggle: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(2))
                    .unwrap_or_else(std::time::Instant::now),
            })
        }
    }

    /// Updates the window title bar with dynamic state (FPS, recording status, hotkeys).
    pub fn set_title(&mut self, title: &str) {
        if let Ok(c_title) = CString::new(title) {
            unsafe {
                (self.api.sdl_set_window_title)(self.window, c_title.as_ptr());
            }
        }
    }

    /// Polls SDL3 events.
    /// Returns (should_continue, toggle_record_requested, toggle_mic_requested).
    pub fn poll_events(&mut self) -> (bool, bool, bool) {
        unsafe {
            let mut event = SDL_Event {
                event_type: 0,
                _padding: [0u8; 124],
            };
            let mut toggle_record = false;
            let mut toggle_mic = false;
            while (self.api.sdl_poll_event)(&mut event) {
                if event.event_type == SDL_EVENT_QUIT
                    || event.event_type == SDL_EVENT_WINDOW_CLOSE_REQUESTED
                {
                    return (false, false, false);
                }
                if event.event_type == crate::ffi::sdl3::SDL_EVENT_KEY_DOWN {
                    let key = event.keycode();
                    // Keycode 'r' = 0x72, 'R' = 0x52, or scancode 21 (SDL_SCANCODE_R)
                    if key == 0x72 || key == 0x52 || key == 21 {
                        if self.last_record_toggle.elapsed() >= std::time::Duration::from_millis(500) {
                            toggle_record = true;
                            self.last_record_toggle = std::time::Instant::now();
                        }
                    } else if key == 0x6D || key == 0x4D || key == 16 {
                        // Keycode 'm' = 0x6D, 'M' = 0x4D, or scancode 16 (SDL_SCANCODE_M)
                        if self.last_mic_toggle.elapsed() >= std::time::Duration::from_millis(400) {
                            toggle_mic = true;
                            self.last_mic_toggle = std::time::Instant::now();
                        }
                    } else if key == 0x71 || key == 0x51 || key == 0x1B {
                        // 'q' or ESC closes the window
                        return (false, false, false);
                    }
                }
            }
            (true, toggle_record, toggle_mic)
        }
    }

    /// Uploads a decoded NV12 or YUV420P frame to the GPU texture and presents it immediately.
    pub fn render_frame(
        &mut self,
        frame: &DecodedFrameView<'_>,
        is_recording: bool,
    ) -> Result<(), String> {
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

                // If dimensions changed (e.g. rotation between portrait and landscape), adapt window
                if self.tex_width != frame.width || self.tex_height != frame.height {
                    let (win_w, win_h) = calculate_target_window_size(frame.width, frame.height);
                    let _ = (self.api.sdl_set_window_size)(self.window, win_w as i32, win_h as i32);
                    let _ = (self.api.sdl_set_window_position)(self.window, 0x2FFF_0000, 0x2FFF_0000); // SDL_WINDOWPOS_CENTERED
                    println!(
                        "[Renderer] Display orientation changed: {}x{} -> window resized to {}x{}",
                        frame.width, frame.height, win_w, win_h
                    );
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

            (self.api.sdl_render_clear)(self.renderer);
            (self.api.sdl_render_texture)(self.renderer, self.texture, ptr::null(), ptr::null());

            if is_recording {
                let badge = crate::ffi::sdl3::SDL_FRect {
                    x: 18.0,
                    y: 18.0,
                    w: 16.0,
                    h: 16.0,
                };
                (self.api.sdl_set_render_draw_color)(self.renderer, 230, 20, 20, 255);
                (self.api.sdl_render_fill_rect)(self.renderer, &badge);
            }

            (self.api.sdl_render_present)(self.renderer);
            Ok(())
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

impl Drop for Sdl3Renderer {
    fn drop(&mut self) {
        unsafe {
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
            // 0xFFFFFFFF (SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK)
            let stream = (api.sdl_open_audio_device_stream)(0xFFFF_FFFF, &spec, None, ptr::null_mut());
            if stream.is_null() {
                return Err(format!("SDL_OpenAudioDeviceStream failed: {}", Sdl3Renderer::last_error(&api)));
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

fn calculate_target_window_size(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (384, 860);
    }
    if width >= height {
        // Landscape (e.g. Free Fire, PUBG, YouTube)
        // Aim for ~700px height so it fits comfortably on any 1080p laptop display with room for taskbar/titlebar
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
        // Portrait (phone home screen / portrait apps)
        // Aim for ~860px height
        let target_h = 860u32;
        let target_w = (width as u64 * target_h as u64 / height as u64) as u32;
        (target_w.max(100), target_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sdl_renderer_init() {
        match Sdl3Renderer::new("Test Window", 1080, 2400) {
            Ok(_r) => println!("Sdl3Renderer created successfully!"),
            Err(e) => panic!("Sdl3Renderer::new failed: {}", e),
        }
    }
}
