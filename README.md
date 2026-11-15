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
