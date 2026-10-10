# AGENTS.md — MultiRecording System Architecture & AI Agent Guide

> **Target Audience:** Future AI coding agents (Antigravity, Claude, Copilot, Cursor, etc.) working on the MultiRecording codebase.  
> **Repository:** `MultiRecording` (macOS Apple Silicon / AVFoundation / FFmpeg / Rust / egui)  
> **Last Updated:** October 2026

---

## 1. Executive Summary & Core Mission

**MultiRecording** is an ultra-low-overhead, multi-source video capture and rolling ring-buffer flashback recording engine built in Rust specifically for macOS Apple Silicon.

It solves two critical problems content creators experience with OBS Studio:
1. **High Latency & GPU Overhead:** OBS continuously composites scenes onto an internal canvas on the main thread, introducing frame drops and input lag into high-refresh gaming. MultiRecording offloads encoding directly to isolated FFmpeg subprocesses leveraging Apple Silicon hardware (`VideoToolbox`).
2. **Buffer Volatility & Multitrack Desync:** Recording long multi-source sessions into a single continuous file risks corrupting the entire container if the system crashes or a single webcam disconnects. MultiRecording writes micro-segmented 2-second chunks (`.mp4`) into an in-memory/disk rolling buffer and performs **instant, lossless flashback clips via stream copy (`-c copy`) in 1 to 2 seconds without re-encoding**.

---

## 2. Codebase Map & Module Responsibilities

The codebase follows a modular, decoupled architecture:

```
src/
├── main.rs         # CLI entrypoint (clap) & eframe native GUI bootstrap
├── config.rs       # TOML configuration serialization (Config, SourceConfig, AudioTrackConfig)
├── devices.rs      # AVFoundation device discovery, classification, and BlackHole detection
├── engine.rs       # Terminal-mode continuous recording engine coordinator
├── recorder.rs     # Subprocess managers: PreviewStream, StreamRecorder, AudioTrackMonitor
├── remuxer.rs      # Concat demuxer (-c copy), SMPTE timecode injection & NLE export
├── ring_buffer.rs  # Sliding-window segment management, disk eviction & metrics
└── ui.rs           # Flat dark-mode native GUI (egui 0.29), VU meters, 16:9 previews, Settings popup
tests/
└── integration_tests.rs # Complete headless test suite (15 tests: ring buffer, sync, config, devices)
docs/
└── ARCHITECTURE.md # In-depth media pipeline and threading model documentation
```

### Module Breakdown
- **`src/main.rs`**: Handles CLI commands (`gui`, `devices`, `init`, `run`, `status`, `save`) and launches `ui::MultiRecordingApp` with `eframe::run_native`.
- **`src/config.rs`**: Manages `config.toml`. Enforces default 2 sources (Gameplay, Facecam), 5 discrete audio tracks, segment times, bitrates, buffer duration, NLE sync flags (`sync_audio_to_secondary`, `embed_timecode`), and output directory.
- **`src/devices.rs`**: Executes `ffmpeg -f avfoundation -list_devices true -i ""` to enumerate macOS hardware. Classifies devices into `Video`, `AudioInput`, and `AudioOutput`. Detects `BlackHole 2ch` loopback driver for speaker recording.
- **`src/recorder.rs`**:
  - `PreviewStream`: Spawns standby preview ffmpeg subprocess (`-vf scale=320:180... -f rawvideo -pix_fmt rgba pipe:1`). Continuously drains stdout for UI textures and parses stderr for real hardware canvas resolution and framerate.
  - `StreamRecorder`: Spawns the active recording subprocess. Uses `-filter_complex "[0:v]split=2[rec_raw][prev_raw];..."` to simultaneously produce micro-segmented MP4s to buffer and stream 15fps 320x180 RGBA preview frames to `pipe:1`.
  - `AudioTrackMonitor`: Spawns continuous audio probe reading raw `f32le` PCM samples from AVFoundation audio devices. Computes real-time RMS/peak levels for animated segmented VU meters.
- **`src/ring_buffer.rs`**: Tracks segment timestamps in `/tmp/multirecording_buffer/`, computes buffer disk usage, and evicts segments older than the configured sliding window (1m up to 2h).
- **`src/remuxer.rs`**: Assembles flashback clips using FFmpeg's `concat` demuxer without decoding/re-encoding (`-c copy`). Injects SMPTE timecode (`-timecode HH:MM:SS:00`) for multicam synchronization in Premiere Pro / DaVinci Resolve.
- **`src/ui.rs`**: Pure flat dark-theme interface with custom color tokens. Live 16:9 viewports with letterbox/pillarbox padding, discrete VU meters with fast attack and smooth broadcast decay, and an independent floating Settings popup window (`egui::Window`).

---

## 3. Critical Invariants & Gotchas (MUST READ BEFORE MODIFYING)

When modifying or refactoring this project, future agents **must respect these hard system constraints**:

### A. AVFoundation Device Contention on macOS
- **The Quirk:** macOS AVFoundation **does not permit** two separate processes or capture sessions to bind to the same physical microphone or audio input at the same time. If attempted, FFmpeg errors with `Cannot open audio device`.
- **The Solution:**
  1. All 5 discrete audio tracks are fed exclusively into **Stream 1 (Gameplay)** (`-i <dev>:0 -i none:1 -i none:2 ...`).
  2. Stream 2 (Facecam) captures video only, unless `sync_audio_to_secondary` is enabled, where it attaches a copy of Track 1 (`-i <video_dev>:<track_1_idx>`) for NLE waveform synchronization.
  3. Standby `PreviewStream` **must always** use `-i <video_dev>:none` so it never claims audio devices while idling.
  4. All `AudioTrackMonitor` instances **must be stopped** before launching `StreamRecorder`, and vice versa.

### B. macOS Audio Output Capture (Speakers / AirPods)
- **The Quirk:** AVFoundation only records *input* devices (microphones, capture cards). It cannot directly intercept macOS system audio output (Speakers, AirPods).
- **The Solution:** Capturing desktop/speaker audio requires the virtual loopback driver **BlackHole 2ch** (or an Apple Multi-Output Device). The app checks `DeviceManager::has_loopback_device()` and warns users if they map an `AudioOutput` without BlackHole.

### C. Stdio Pipe Stall & Process Deadlock
- **The Quirk:** When spawning FFmpeg subprocesses with piped `stdout` or `stderr`, Unix pipes have a default buffer size of 64KB. If FFmpeg writes logs or frames to a pipe and the parent process does not continuously read from it, the pipe buffer fills up and FFmpeg freezes indefinitely after 2–3 seconds.
- **The Solution:** Every subprocess in `recorder.rs` spawns dedicated background threads immediately upon startup to continuously drain `stdout` (preview frames) and `stderr` (logs & resolution probing). **Never spawn FFmpeg with `Stdio::piped()` without an active draining thread.**

### D. GOP Boundary Alignment for Concat Demuxer
- **The Quirk:** Lossless concatenation (`-c copy`) requires that every single segment begins with an IDR keyframe and has matching timestamps. Mismatched GOP sizes cause macroblocking, freeze frames, or audio desync in Premiere Pro.
- **The Invariant:** The GOP size (`-g`) must be strictly locked to:
  $$\text{GOP Size} = \text{Framerate (fps)} \times \text{Segment Duration (secs)}$$
  *(e.g., 60 fps with 2-second segments $\rightarrow$ `-g 120`)*.

### E. Resolution & Frame Rate Constraints
- **Separation:** Resolution and Frame Rate are decoupled into separate dropdowns under each video preview card.
- **Hardware Clamping:** Output frame rate is strictly bounded by the input hardware frame rate. If an input device reports 30fps (e.g. built-in FaceTime camera or USB webcam), the output dropdown **must only allow 30fps**, and any previously saved higher value must be auto-clamped to 30fps.

### F. Dark Mode Enforcement
- **The Quirk:** `eframe` defaults to `ThemePreference::System`. When the user's macOS host OS is set to Light Mode, `egui` automatically overrides visuals back to `Visuals::light()`, turning backgrounds and widgets stark white.
- **The Solution:** In `src/ui.rs`:
  1. `ctx.set_theme(egui::ThemePreference::Dark);`
  2. `ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());`
  3. `ctx.set_visuals_of(egui::Theme::Light, visuals.clone());`
  4. Top of `update()` verifies `!ctx.style().visuals.dark_mode` and re-applies if overridden.
  5. `CentralPanel` uses an explicit frame fill of `#0E0E12` (`Color32::from_rgb(14, 14, 18)`).

### G. Settings UI is a Popup Window
- The settings drawer is not rendered inline inside `CentralPanel` (which shifts the canvas and squashes preview cards).
- It is rendered via `egui::Window::new("⚙ Preferences & Recording Settings")` as an independent floating popup modal, toggled by the top bar `[⚙ settings]` button.

---

## 4. Hardware Encoding Configuration

MultiRecording utilizes Apple Silicon hardware encoders via FFmpeg:

| Encoder Name | FFmpeg Codec | Description |
|---|---|---|
| **Apple H.264 (Hardware)** | `h264_videotoolbox` | Default hardware encoder. Zero CPU impact, highest compatibility. |
| **Apple HEVC / H.265** | `hevc_videotoolbox` | Apple Silicon hardware HEVC. Best compression-to-bitrate ratio. |
| **libx264 (CPU)** | `libx264` | Software fallback for custom crf profiles. |

---

## 5. Build, Test, and Run Commands

### Running Tests
Always run the test suite to verify changes across the ring buffer, timecode, and configuration:
```bash
cargo test
```

### Running the Application
Launch GUI mode:
```bash
cargo run -- gui
```
List discovered AVFoundation video devices and audio inputs/outputs:
```bash
cargo run -- devices
```
Check disk usage of current recording buffer:
```bash
cargo run -- status
```
Trigger a flashback clip export from terminal mode:
```bash
cargo run -- save --duration 300
```

### Production Release Build
```bash
cargo build --release
# Output binary: target/release/multirecording
```

---

## 6. Common Debugging Workflows

1. **Recording stops after a few seconds:**
   - Check if an audio monitor thread was left running while starting the engine (AVFoundation device lock).
   - Check if a new piped FFmpeg command was added without draining its `stderr`/`stdout`.
   - Inspect `/tmp/multirecording_logs/gameplay.log` or `/tmp/multirecording_logs/facecam.log`.
2. **Preview aspect ratio looks stretched:**
   - Ensure the canvas uses letterboxing with `target_aspect = 16.0 / 9.0` (or input aspect ratio) rather than stretching to the full rectangle.
3. **Audio VU meters stay at zero:**
   - Ensure `AudioTrackMonitor` is bound to the device's AVFoundation index (`av_index`), not a synthetic loopback ID without audio routing.
   - Run `cargo run -- devices` to inspect hardware index mapping.
