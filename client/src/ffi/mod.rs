pub mod ffmpeg;
pub mod sdl3;

use std::ffi::{c_char, c_void, CString};

/// Cross-platform dynamic shared library loader (Win32 LoadLibraryA / POSIX dlopen).
pub struct DynLib {
    handle: *mut c_void,
}

unsafe impl Send for DynLib {}
unsafe impl Sync for DynLib {}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(lpLibFileName: *const c_char) -> *mut c_void;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const c_char) -> *mut c_void;
    fn FreeLibrary(hLibModule: *mut c_void) -> i32;
    fn SetDllDirectoryA(lpPathName: *const c_char) -> i32;
}

#[cfg(not(target_os = "windows"))]
#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}

fn candidate_lib_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            dirs.push(exe_dir.join("lib"));
            dirs.push(exe_dir.join("..").join("..").join("lib"));
            dirs.push(exe_dir.join("..").join("..").join("..").join("lib"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("client").join("lib"));
        dirs.push(cwd.join("lib"));
    }
    dirs.push(std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/lib"
    )));
    dirs
}

fn configure_dll_search_path() {
    #[cfg(target_os = "windows")]
    {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            for dir in candidate_lib_dirs() {
                if dir.is_dir() {
                    let canonical = std::fs::canonicalize(&dir).unwrap_or(dir);
                    let path_str = canonical.to_string_lossy();
                    let clean_path = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
                    if let Ok(c_dir) = CString::new(clean_path) {
                        unsafe {
                            SetDllDirectoryA(c_dir.as_ptr());
                        }
                        break;
                    }
                }
            }
        });
    }
}

impl DynLib {
    pub fn open_any(candidates: &[&str]) -> Result<Self, String> {
        configure_dll_search_path();
        for &name in candidates {
            if let Ok(lib) = Self::open(name) {
                return Ok(lib);
            }
            for dir in candidate_lib_dirs() {
                let full_path = dir.join(name);
                if full_path.is_file() {
                    if let Some(s) = full_path.to_str() {
                        if let Ok(lib) = Self::open(s) {
                            return Ok(lib);
                        }
                    }
                }
            }
        }
        Err(format!(
            "Could not load any shared library from candidates: {:?}",
            candidates
        ))
    }

    pub fn open(name: &str) -> Result<Self, String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        #[cfg(target_os = "windows")]
        let handle = unsafe { LoadLibraryA(c_name.as_ptr()) };

        #[cfg(not(target_os = "windows"))]
        let handle = unsafe { dlopen(c_name.as_ptr(), 0x00002 /* RTLD_NOW */) };

        if handle.is_null() {
            Err(format!("Failed to open library '{}'", name))
        } else {
            Ok(Self { handle })
        }
    }

    pub unsafe fn get_sym<T: Copy>(&self, symbol: &str) -> Result<T, String> {
        let c_sym = CString::new(symbol).map_err(|e| e.to_string())?;
        #[cfg(target_os = "windows")]
        let addr = GetProcAddress(self.handle, c_sym.as_ptr());

        #[cfg(not(target_os = "windows"))]
        let addr = dlsym(self.handle, c_sym.as_ptr());

        if addr.is_null() {
            Err(format!("Missing symbol '{}'", symbol))
        } else {
            Ok(std::mem::transmute_copy(&addr))
        }
    }
}

impl Drop for DynLib {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                #[cfg(target_os = "windows")]
                FreeLibrary(self.handle);

                #[cfg(not(target_os = "windows"))]
                dlclose(self.handle);
            }
        }
    }
}
