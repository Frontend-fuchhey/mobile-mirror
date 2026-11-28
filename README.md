# Mobile Mirror

**Low-latency Android and iOS screen mirroring and zero-re-encoding MP4 recording for Windows, macOS, and Linux — built for developers, educators, and content creators.**

[![Release](https://img.shields.io/github/v/release/Frontend-fuchhey/mobile-mirror?style=flat-square)](https://github.com/Frontend-fuchhey/mobile-mirror/releases)
[![Build Status](https://img.shields.io/github/actions/workflow/status/Frontend-fuchhey/mobile-mirror/release.yml?style=flat-square&label=release%20build)](https://github.com/Frontend-fuchhey/mobile-mirror/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/client-Rust%20%2B%20SDL3%20%2B%20FFmpeg-orange.svg?style=flat-square)](client/Cargo.toml)
[![Java](https://img.shields.io/badge/android%20server-Java%20DEX-red.svg?style=flat-square)](server/)

[Download Page](https://mobile-mirror.vercel.app) · [GitHub Releases](https://github.com/Frontend-fuchhey/mobile-mirror/releases) · [Report a Bug](https://github.com/Frontend-fuchhey/mobile-mirror/issues)

---

## Visual Preview

![Demo](./assets/demo.gif)

*Mobile Mirror dashboard streaming a live 60 FPS Android/iOS device feed with hardware-accelerated decoding and one-click MP4 recording. (Replace `./assets/demo.gif` with a recorded capture or `./app.png`.)*

---

## Why Mobile Mirror?

Most phone mirroring workflows force a trade-off between platform lock-in, re-encoding overhead, or juggling separate tools for Android, iOS, audio, and recording.

- **One native client for Android and iOS:** Mirror Android over USB (ADB), iPhone/iPad over USB (CoreMedia/QuickTime protocol via `nusb`), or iOS wirelessly over local AirPlay 1 & 2 (`rust-airplay`) without switching apps.
- **Zero-re-encoding MP4 recording:** Incoming H.264 NAL units are split directly to the GPU decoder and a native ISO-BMFF / `libavformat` MP4 muxer simultaneously, avoiding double compression and CPU spikes.
- **Hardware-accelerated playback:** Uses FFmpeg `libavcodec` (`NVDEC`, `D3D11VA`, `DXVA2`, `VideoToolbox`, `VA-API`) with SDL3 rendering and slice-threaded software fallback.
- **Local-only and privacy-first:** All video, system audio, and microphone streams stay on USB or your local LAN. No accounts, telemetry, or cloud relays.

> **Honest Trade-offs & Current Limitations**
> - **View & record, not remote control:** Unlike `scrcpy`, Mobile Mirror focuses on clean display mirroring, audio capture, and recording rather than forwarding desktop mouse/keyboard input back to the phone.
> - **iOS USB on Windows:** Direct iOS USB mirroring uses `nusb` and requires an accessible WinUSB driver and accepting **Trust This Computer** on the device. Wireless AirPlay works without extra USB drivers.
> - **Unsigned macOS builds:** Release `.dmg` builds are ad-hoc signed; first launch requires running the bundled `First Time Open (Bypass Gatekeeper).command`.

---

## Quickstart

### Prerequisites

- **Android:** Enable **USB Debugging** in Developer Options and connect via USB. (Windows and macOS releases bundle `adb`; on Linux install `adb` and `ffmpeg`.)
- **iOS (AirPlay):** Connect your iPhone/iPad and computer to the same local Wi-Fi network, then pick **Mobile Mirror** from iOS Screen Mirroring.
- **iOS (USB):** Connect via Lightning/USB-C and tap **Trust This Computer**.

### Install

**Windows (recommended via winget):**

```powershell
winget install Frontend-fuchhey.MobileMirror
```

**macOS & Linux:**

Download the latest artifact from [GitHub Releases](https://github.com/Frontend-fuchhey/mobile-mirror/releases):

```bash
# Debian / Ubuntu (.deb)
sudo apt install ./MobileMirror-Linux-amd64.deb

# Portable Linux tarball
tar -xzf MobileMirror-Linux-x86_64.tar.gz
./MobileMirror-Linux-x86_64/install.sh
```

On macOS, open `MobileMirror-macOS.dmg`, drag **Mobile Mirror.app** to Applications, and run `First Time Open (Bypass Gatekeeper).command` once.

### Minimal Working Usage

Launch the desktop dashboard (automatically detects connected Android/iOS USB devices and advertises an AirPlay receiver):

```bash
mobile-mirror
```

Or run headless / CLI recording directly to a custom directory with phone system audio enabled:

```bash
mobile-mirror-client --auto-record --laptop-audio --record-dir ./recordings
```

---

## Features & Architecture

```text
Android (USB / ADB) ──► server.jar (SurfaceEncoder H.264 + AudioCapture) ──┐
iOS (USB / nusb)    ──► CoreMedia QuickTime Bulk Stream (H.264 + LPCM)   ──┼──► NalSplitter
iOS (LAN / AirPlay) ──► rust-airplay (RTSP + FairPlay + AES-128-CTR)     ──┘        │
                                                                    ┌───────────────┴───────────────┐
                                                                    ▼                               ▼
                                                      HwDecoder (FFmpeg FFI)              Mp4Muxer (Zero-Copy)
                                                  NVDEC / D3D11VA / VideoToolbox / VA-API   ISO-BMFF / libavformat
                                                                    │                               │
                                                                    ▼                               ▼
                                                        SDL3 Renderer & Audio               recordings/*.mp4
```

- **Android DEX Server (`server/`):** A lightweight Java server compiled with `d8` into `server.jar` and executed on-device via `app_process` over ADB (UID 2000). Captures the display at up to 60 FPS (default 24 Mbps CBR, 1-second IDR interval) and streams framed H.264 NAL packets plus system/microphone audio over a forwarded local TCP socket.
- **Rust Desktop Client (`client/`):** Native binary with an SDL3 UI dashboard (`Home`, `Recordings`, `Settings`), live quality presets (`Auto` 24 Mbps, `HD` 720p 16 Mbps, `FHD` 36 Mbps), thumbnail browser, and dynamic FFI loading for SDL3 and FFmpeg.
- **Dual-Path Pipeline (`splitter.rs`):** Duplicates incoming H.264 packets in memory—one path feeds the low-delay hardware decoder for live rendering, and the other writes Annex-B/AVCC samples directly into an `.mp4` container with zero re-encoding.
- **Live Control Channel:** Toggle the phone's voice-isolated microphone (`M`) or force an immediate IDR keyframe (`K`) over the active TCP control channel without restarting the stream.
- **Verifiable Releases:** Official installers for Windows, macOS, and Linux are built via GitHub Actions with published `SHA256SUMS.txt` checksums.

---

## Configuration / CLI Flags / API Reference

### Desktop Client CLI Flags (`mobile-mirror-client`)

| Flag | Alias | Default | Description |
| :--- | :--- | :--- | :--- |
| `--usb` | `--wired`, `--auto`, `--ui` | Enabled | Launch automatic USB/AirPlay detection with the SDL3 dashboard UI. |
| `--mode <MODE>` | — | `auto` | Startup mode: `auto` (dashboard) or `direct` (connect to an existing socket). |
| `--addr <HOST:PORT>` | `-a`, `--connect` | `127.0.0.1:12345` | Connect directly to an active Android server socket (switches to direct mode). |
| `--port <PORT>` | `-p` | `12345` | Local ADB forwarded TCP port. |
| `--record-dir <DIR>` | — | OS video dir / `recordings/` | Output directory for saved `.mp4` recordings. |
| `--record <PATH>` | `-r` | — | Output directory or target file path for recordings. |
| `--auto-record` | — | `false` | Start recording immediately upon device connection. |
| `--no-record` | — | `true` | Disable auto-recording on connection. |
| `--mic` / `--no-mic` | — | `false` | Enable or disable phone microphone capture on launch. |
| `--laptop-audio` | — | `false` | Play mirrored phone audio through desktop speakers. |
| `--no-display` | `--headless` | `false` | Run in headless mode without opening an SDL3 window. |
| `--hw-backend <NAME>` | `--hw` | `auto` | Preferred hardware decoder: `nvdec`, `d3d11va`, `dxva2`, `cpu`. |
| `--max-packets <N>` | — | Unlimited | Stop session after receiving `N` packets (useful for automated tests). |
| `-h`, `--help` | — | — | Print CLI usage and keyboard controls. |

### Interactive Keyboard Controls

| Key | Action |
| :--- | :--- |
| `R` | Start / stop lossless MP4 recording |
| `M` | Toggle phone voice-isolated microphone |
| `F11` | Toggle fullscreen mirror view |
| `F` | Toggle compact window vs. dashboard mirror view |
| `Q` / `Esc` | Exit fullscreen or quit Mobile Mirror |

### Android Server Options (`com.mirror.Server`)

When launching `server.jar` manually via `adb shell CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server [options]`:

| Option | Default | Description |
| :--- | :--- | :--- |
| `port=<INT>` | `12345` | TCP port the on-device server listens on. |
| `display_id=<INT>` | `0` | Android display ID to capture (`display=<INT>` alias). |
| `max_size=<INT>` | `0` | Max width/height in pixels (`0` = native resolution). |
| `max_fps=<INT>` | `60` | Target capture frame rate (`fps=<INT>` alias). |
| `bit_rate=<INT>` | `24000000` | Encoder bitrate in bps (`bitrate=<INT>` alias). |
| `i_frame_interval=<INT>` | `1` | Seconds between IDR keyframes. |
| `cbr=<BOOL>` | `true` | Use Constant Bitrate mode (pass `vbr` flag to disable). |
| `send_header=<BOOL>` | `true` | Prefix stream with 12-byte header and packet metadata (pass `raw` to disable). |
| `encoder=<NAME>` | Auto | Explicit Android `MediaCodec` encoder component name. |

---

## Roadmap

- [ ] **Wireless ADB mirroring** over local Wi-Fi (Android 11+ wireless debugging pairing)
- [ ] **H.265 (HEVC) encoding & muxing** for lower bandwidth at 1440p/4K resolutions
- [ ] **Apple Developer notarized macOS `.dmg`** to remove the first-run Gatekeeper workaround
- [ ] **Multi-device simultaneous mirroring** with side-by-side recording tabs
- [ ] **Optional mouse/keyboard control** toggle for Android devices over ADB

---

## Contributing & License

### Local Development Setup

**Prerequisites:** Rust (stable toolchain), JDK 17+, Android SDK (with `platforms` and `build-tools` for `d8`), and runtime libraries for SDL3 and FFmpeg (`libavcodec`, `libavformat`, `libavutil`, `libswresample`).

**Windows (PowerShell):**

```powershell
# Build Android server.jar, run client unit tests, and compile release binary
.\scripts\build_all.ps1

# Push server.jar, forward ADB port, and launch the client
.\scripts\run_mirror.ps1
```

**macOS / Linux:**

```bash
# 1. Build Android DEX server
./server/build.sh

# 2. Run Rust tests & build client
cargo test --manifest-path client/Cargo.toml
cargo build --release --manifest-path client/Cargo.toml

# 3. Launch via helper script
./scripts/run_mirror.sh
```

Pull requests, bug reports, and platform testing feedback are welcome. Open an issue first for major architectural changes.

### License

Distributed under the **[MIT License](LICENSE)**. Copyright (c) 2026 Mobile Mirror Contributors.
