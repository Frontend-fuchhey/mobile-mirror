# Mobile Mirror (`mobile-mirror`)

A real-time Android USB screen mirroring, voice-isolated audio capture, and zero-CPU-overhead MP4 recording desktop application achieving **<50ms end-to-end latency at 60–120 FPS** using hardware H.264 decoding (NVDEC / DXVA2 / D3D11VA) and a native HiDPI SDL3 Bento Dashboard UI.

---

## Bento Dashboard UI & Interactive Features

- **Left Navigation Sidebar**:
  - **`Home`**: Displays the hero device illustration when idle and hosts the live 60 FPS GPU-accelerated phone screen while mirroring (with instant `[Mic: On/Off]`, `[Expand]` full-window toggle, and `[Stop]` controls).
  - **`Recordings`**: Built-in recordings manager listing all saved `.mp4` videos in `recordings/` with file sizes and one-click **`[Play]`**, **`[Show]`** (reveal in Explorer), **`[Delete]`**, and **`[Open Folder]`** actions.
  - **`Settings`**: Live interactive settings panel (persisted automatically to `recordings/settings.cfg`) controlling:
    1. **Target Frame Rate** (`30 / 60 / 90 / 120 FPS`)
    2. **Video Encoder Bitrate** (`4 / 8 / 12 / 16 / 24 Mbps`)
    3. **Bitrate Control Mode** (`Constant CBR` vs `Variable VBR`)
    4. **Phone Microphone (Voice Isolation DSP)** (`ON / OFF`)
    5. **PC Speaker Audio Playback** (`ON / OFF`)
    6. **Auto-Connect on USB Plug** (`ON / OFF`)
    7. **Auto-Record on Mirror Start** (`ON / OFF`)
    8. **Always on Top Window** (`ON / OFF`)
    9. **Hardware Video Decoder Backend** (`Auto / NVDEC / D3D11VA / DXVA2`)
  - **`About` / `Help & Support`**: Interactive 4-step **USB Debugging Setup Guide**, live engine diagnostics, and a **`[Check USB Device Now]`** button.
  - **Live Status Card (`Ready`)**: Displays real-time USB ADB status (`Ready — Waiting for device...`, `Connecting`, `Action Needed`, or `Live — <res> • <fps>fps`).
- **Right Control Column**:
  - **`Recording` Card**: Live tabular `00:00:00` (`HH:MM:SS`) recording timer, dynamic status pill (`Off` / `Auto` / `ON`), and **`[Start Recording]` / `[Stop Recording]`** button.
  - **`Mirror Quality` Card**: 3-way segmented quality switcher (**`Auto`** native 12 Mbps, **`HD`** 720p 8 Mbps, **`FHD`** 1080p 16 Mbps) that dynamically configures (and live-reconfigures) the Android hardware H.264 encoder.
  - **`Quick Tools` Card**: One-click shortcuts for **`Open Recordings`**, **`Settings`**, and **`Help & Support`**.

---

## Folder Structure

```text
mobile-mirror/
├── start.bat                  # One-click Windows launcher (auto-builds & launches Mobile Mirror)
├── README.md                  # Project documentation
├── .gitignore                 # Git ignore rules
├── client/                    # Rust PC Desktop Client (SDL3 + FFmpeg NVDEC/DXVA2)
│   ├── Cargo.toml
│   ├── build.rs
│   ├── lib/                   # Runtime native shared libraries (FFmpeg + SDL3)
│   └── src/
│       ├── main.rs            # Entry point, Bento Dashboard event loop & live streaming session
│       ├── settings.rs        # Persistent AppSettings, MirrorQuality, Recordings scanner & OS actions
│       ├── adb.rs             # Non-blocking UsbAutoConnector, dynamic quality ADB pipeline & RAII guard
│       ├── ui_canvas.rs       # High-DPI anti-aliased SDF UI canvas, 3D illustration & vector icons
│       ├── protocol.rs        # Framed & raw Annex-B H.264/AAC/PCM stream reader
│       ├── splitter.rs        # Zero-copy Arc<NalPacket> packet dispatcher
│       ├── decoder.rs         # Hardware H.264 decoder (NVDEC / DXVA2 / D3D11VA)
│       ├── renderer.rs        # SDL3 Bento Dashboard presenter, GPU YUV renderer & audio playback
│       ├── muxer.rs           # Zero-copy MP4 muxer (H.264 video + AAC-LC audio)
│       └── ffi/               # Dynamic library loader & FFmpeg/SDL3 bindings
├── server/                    # Android DEX Capture Server (UID 2000 / app_process)
│   ├── build.ps1              # Compiles Java sources & packages server.jar via d8
│   ├── build.sh               # Linux/macOS DEX build script
│   ├── build.gradle           # Gradle build definition
│   ├── server.jar             # Compiled DEX archive pushed to /data/local/tmp/server.jar
│   └── src/main/java/com/mirror/
│       ├── Server.java        # CLI entry point (com.mirror.Server)
│       ├── Options.java       # CLI parameter parser (port, max_size, bitrate, fps, cbr)
│       ├── Workarounds.java   # Standalone app_process ActivityThread initialization
│       ├── FakeContext.java   # Shell UID 2000 Context wrapper
│       ├── audio/             # Internal audio & voice-isolated mic capture + AAC encoder
│       ├── display/           # Display geometry models
│       ├── net/               # Local NIO TCP server (127.0.0.1:12345) & packet streamer
│       ├── video/             # Virtual display & hardware H.264 MediaCodec encoder
│       └── wrappers/          # Reflective hidden API wrappers (Android 5 - 16+)
├── scripts/                   # Automation scripts
│   ├── build_all.ps1          # Compiles Android DEX server + Rust release client
│   ├── run_mirror.ps1         # PowerShell standalone launcher script
│   └── run_mirror.sh          # Bash standalone launcher script
└── recordings/                # Output directory for recorded MP4 videos & persistent settings.cfg
```

---

## Quick Start

### 1. Launch Mobile Mirror (Windows)
Double-click [`start.bat`](file:///c:/Users/asus/Desktop/mobile-mirror/start.bat) or run from terminal:
```cmd
.\start.bat
```
Plug in your Android phone over USB (with **Developer Options -> USB Debugging** enabled) or click **`Start Mirroring`**.

### 2. Keyboard Shortcuts
- **`[R]`** — Start / Stop lossless MP4 screen recording (saved to `recordings/`)
- **`[M]`** — Toggle Phone Microphone Voice Recording ON / OFF (with real-time voice-isolation DSP)
- **`[F]`** — Toggle between Embedded Dashboard Mirror View and Full-Window Mirror View
- **`[Q]`** or **`[ESC]`** — Disconnect and close Mobile Mirror
