# Mobile Mirror (`mobile-mirror`)

A real-time Android screen mirroring, audio capture, and zero-CPU-overhead MP4 recording system achieving **<50ms end-to-end latency at 60+ FPS**.

---

## Architecture Overview

```text
ANDROID DEVICE (UID 2000 / com.android.shell)
┌───────────────────────────────────────────────────────────────────────────┐
│ Video Pipeline:                                                           │
│   SurfaceControl / DisplayManager VirtualDisplay ("mirror")               │
│   └─► Hardware MediaCodec H.264 (COLOR_FormatSurface, 60 FPS, 12 Mbps)    │
│                                                                           │
│ Audio Pipeline:                                                           │
│   Internal Audio (AudioPlaybackCapture / REMOTE_SUBMIX) +                 │
│   Optional Voice-Isolated Mic (VOICE_COMMUNICATION + DSP Band-Pass/Gate)  │
│   └─► Hardware MediaCodec AAC-LC (48 kHz Stereo, 128 kbps) + Raw PCM      │
│                                                                           │
│ Network Transport:                                                        │
│   StreamServer (127.0.0.1:12345, Direct ByteBuffer writev, TCP_NODELAY)   │
└─────────────────────────┼─────────────────────────────────────────────────┘
                          │ ADB TCP Socket (127.0.0.1:12345)
                          ▼
PC DESKTOP CLIENT (Rust + FFmpeg + SDL3)
┌───────────────────────────────────────────────────────────────────────────┐
│ StreamReader (TCP_NODELAY) + Bidirectional ControlWriter (Mic Toggle)     │
│                         │                                                 │
│                    NalSplitter (Zero-Copy Arc::clone)                     │
│             ┌───────────┴───────────┐                                     │
│             ▼                       ▼                                     │
│   [Path A: Async MP4 Recorder]  [Path B: Real-Time Playback]              │
│   Native ISO-BMFF Muxer         libavcodec (NVDEC / DXVA2 / D3D11VA)      │
│   H.264 (avc1) + AAC-LC (mp4a)  Zero-delay decode -> NV12/YUV420P         │
│   Direct bitstream copy         SDL3 GPU Streaming Texture (VSync Off)    │
└───────────────────────────────────────────────────────────────────────────┘
```

---

## Standardized Folder Structure

```text
mobile-mirror/
├── start.bat                  # One-click Windows launcher
├── README.md                  # Project documentation
├── .gitignore                 # Git ignore rules
├── client/                    # Rust PC Desktop Client
│   ├── Cargo.toml
│   ├── build.rs
│   ├── lib/                   # Runtime native shared libraries (FFmpeg + SDL3)
│   │   ├── avcodec-63.dll
│   │   ├── avformat-63.dll
│   │   ├── avutil-61.dll
│   │   ├── swresample-7.dll
│   │   └── SDL3.dll
│   └── src/
│       ├── main.rs            # Entry point, UI loop, and async recorder worker
│       ├── protocol.rs        # Framed H.264/AAC/PCM stream reader & control channel
│       ├── splitter.rs        # Zero-copy Arc<NalPacket> packet dispatcher
│       ├── decoder.rs         # Hardware H.264 decoder (NVDEC / DXVA2 / D3D11VA)
│       ├── renderer.rs        # Low-latency SDL3 GPU renderer & audio player
│       ├── muxer.rs           # Zero-copy MP4 muxer (H.264 video + AAC-LC audio)
│       └── ffi/               # Dynamic library loader & FFmpeg/SDL3 bindings
├── server/                    # Android Java Server (compiled to DEX server.jar)
│   ├── build.ps1              # Windows javac + d8 DEX build script
│   ├── build.sh               # Linux/macOS javac + d8 DEX build script
│   ├── build.gradle           # Optional Gradle build definition
│   └── src/main/java/com/mirror/
│       ├── Server.java        # Main entry point (com.mirror.Server)
│       ├── Options.java       # CLI parameter parser
│       ├── Workarounds.java   # ActivityThread & AttributionSource spoofing
│       ├── FakeContext.java   # System Context wrapper for UID 2000 (shell)
│       ├── audio/             # Internal audio & voice-isolated mic capture + AAC encoder
│       ├── display/           # Display geometry models
│       ├── net/               # Zero-copy NIO socket server & packet streamer
│       ├── video/             # Virtual display & hardware H.264 MediaCodec encoder
│       └── wrappers/          # Reflective hidden API wrappers (Android 5 - 16+)
├── scripts/                   # Automation scripts
│   ├── build_all.ps1          # Compiles Android DEX server + Rust release client
│   ├── run_mirror.ps1         # PowerShell launcher script
│   └── run_mirror.sh          # Bash launcher script
└── recordings/                # Output directory for recorded MP4 videos
```

---

## Quick Start

### 1. One-Click Launch (Windows)
Double-click [`start.bat`](file:///c:/Users/asus/Desktop/mobile-mirror/start.bat) or run from terminal:
```cmd
.\start.bat
```

### 2. Window Hotkeys
While the mirror window is focused:
- **`[R]`** — Start / Stop lossless MP4 screen recording (saved to `recordings/`)
- **`[M]`** — Toggle Phone Microphone Voice Recording ON / OFF (with real-time voice-isolation DSP)
- **`[Q]`** or **`[ESC]`** — Disconnect and close the mirror window

### 3. Build All Components Manually
```powershell
powershell -ExecutionPolicy Bypass -File scripts\build_all.ps1
```
