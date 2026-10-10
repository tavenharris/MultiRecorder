# MultiRecording (Rust / macOS)

A native, high-performance macOS application built in Rust for dual-PC gaming, streaming, and content creation. It simultaneously captures multiple video and audio feeds (e.g., Elgato 4K capture card + webcam facecam + microphones), maintains a rolling **retroactive flashback buffer** with Apple Silicon hardware encoding (`VideoToolbox`), and exports synchronized clips instantly at the touch of a button.

---

## Key Features

- **Multi-Source Video Capture**: Record your 4K60 capture card (gameplay) and 1080p webcam (facecam) at the same time.
- **Up to 5 Discrete Audio Tracks**: Record up to 5 isolated audio channels simultaneously (e.g., Game Audio, Microphone, Discord/Chat, Desktop/Music, and Soundboard/Aux).
- **Embedded Track Labels**: Custom track titles are written directly into MP4 metadata so Premiere Pro & DaVinci Resolve timelines automatically name your audio tracks (`A1: Game Audio`, `A2: Microphone`, etc.)!
- **Hardware-Accelerated Encoding**: Zero CPU strain while gaming—uses Apple Silicon's hardware `h264_videotoolbox`, `hevc_videotoolbox`, or `prores_videotoolbox`.
- **Rolling Flashback Ring Buffer**: Automatically keeps the last $X$ minutes of footage (configurable, e.g., 5, 10, or 15 mins) on a fast temporary SSD ring buffer, evicting expired segments continuously.
- **Sub-Second Lossless Remuxing**: Hit `F` or `Space` at any time to immediately save the last $X$ minutes. Clips are stitched with direct stream copy (`-c copy`) in ~2 seconds without re-encoding!
- **Premiere Pro / NLE Ready**: Automatically outputs paired synchronized video files (`Flashback_[Timestamp]_gameplay.mp4` and `Flashback_[Timestamp]_facecam.mp4`) sharing matching start timecodes for one-click Multicam editing.

---

## Quick Start

### 1. Launch the Native GUI (Flat Design)
Simply run:

```bash
cargo run
# or
cargo run -- gui
```

#### What's in the Flat Design Interface:
- **Top Status Bar**: Live recording toggle (`Start Engine` / `Stop Engine`) with emerald `● BUFFERING LIVE` indicator.
- **Retroactive Flashback Card**:
  - Buffer Duration Chips: `[ 1m ]`, `[ 3m ]`, `[ 5m ]`, `[ 10m ]`, `[ 15m ]`.
  - Master Action Button: Large coral flat button: **`⚡ SAVE LAST 5 MINS [Space / F]`**.
- **Dual-Source Video Feeds**: Independent capture card and webcam device pickers.
- **🎚 Multi-Track Audio Mixer (Up to 5 Tracks)**:
  - Toggle and label each track individually (`Game Audio`, `Microphone`, `Discord`, `Music`, `Aux`).
  - Assign any hardware audio device to each track with individual active/muted status indicators.
- **Export History & Reveal**: List of all exported flashback sessions with file sizes and a 1-click **"Reveal in Finder"** button.

---

### 2. Discovered Connected Devices (CLI)
List all AVFoundation video capture devices (Elgato cards, webcams, screen capture) and audio inputs from the terminal:

```bash
cargo run -- devices
```

---

### 3. Configure via `config.toml` (Optional / Headless)
Generate or edit `config.toml`:

```bash
cargo run -- init
```

Example `config.toml` for a Dual-PC setup:
```toml
# Duration of rolling flashback buffer in seconds (300 = 5 minutes, 600 = 10 minutes)
buffer_duration_secs = 300
segment_duration_secs = 2

# Directory where your finalized flashback clips are exported
output_dir = "/Users/tavenharris/Movies/MultiRecording"

# Fast temporary SSD cache directory for rolling chunks
buffer_dir = "/tmp/multirecording_buffer"

# Stream 1: Gaming PC via Elgato Capture Card + HDMI Audio
[[sources]]
name = "gameplay"
video_device = "1"           # Elgato capture card video index from `cargo run -- devices`
audio_device = "1"           # Elgato HDMI audio index
resolution = "3840x2160"     # 4K resolution
fps = 60
video_bitrate = "50M"
encoder = "h264_videotoolbox"

# Stream 2: Facecam Webcam + Voice Microphone
[[sources]]
name = "facecam"
video_device = "2"           # Webcam device index
audio_device = "0"           # USB Microphone index
resolution = "1920x1080"     # 1080p resolution
fps = 60
video_bitrate = "15M"
encoder = "h264_videotoolbox"
```

---

### 4. Headless / Terminal Mode
If you prefer running without a GUI:

```bash
cargo run -- run
```

While running in terminal:
- **`F` or `Space`**: Instantly triggers Flashback! Exports the last $X$ minutes of all streams into your output folder.
- **`S`**: Displays live buffer statistics (active segments, size in MB, buffered duration).
- **`Q` or `Esc`**: Gracefully stops recording and flushes all pending segments.

---

### 4. Editing in Premiere Pro / DaVinci Resolve
1. Open Premiere Pro and drag the exported `Flashback_[Timestamp]_gameplay.mp4` and `Flashback_[Timestamp]_facecam.mp4` files into your project.
2. Select both clips -> Right-click -> **Create Multi-Camera Source Sequence**.
3. Choose **Timecode** or **Clip Start** for Synchronize Point.
4. Your gameplay, facecam, and isolated audio tracks are locked in sync with zero drift.

---

## Architecture Overview

```
[ Elgato 4K ]  ──> [ AVFoundation ] ──> [ VideoToolbox H.264 ] ──> [ 2s Segment Chunks ] ──┐
[ Webcam    ]  ──> [ AVFoundation ] ──> [ VideoToolbox H.264 ] ──> [ 2s Segment Chunks ] ──┼──> [ Ring Evictor ]
                                                                                           │
[ Hotkey: 'F' ] ───────────────────────────────────────────────────────────────────────────┴──> [ Concat Remuxer (-c copy) ]
                                                                                                        │
                                                                                                        ▼
                                                                                            Synced NLE MP4s (< 2s)
```
