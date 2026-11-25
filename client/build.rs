use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_LIB_DIR");
    println!("cargo:rerun-if-env-changed=SDL3_LIB_DIR");

    if let Ok(ffmpeg_dir) = env::var("FFMPEG_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", ffmpeg_dir);
    }
    if let Ok(sdl3_dir) = env::var("SDL3_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", sdl3_dir);
    }

    if env::var("CARGO_FEATURE_LINK_SYSTEM_LIBS").is_ok() {
        println!("cargo:rustc-link-lib=avcodec");
        println!("cargo:rustc-link-lib=avformat");
        println!("cargo:rustc-link-lib=avutil");
        println!("cargo:rustc-link-lib=SDL3");
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
            let res_path = std::path::Path::new(&manifest_dir)
                .join("assets")
                .join("app_icon.res");
            if res_path.exists() {
                println!("cargo:rerun-if-changed={}", res_path.display());
                println!("cargo:rustc-link-arg-bins={}", res_path.display());
            }
        }
    }
}
