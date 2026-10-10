# MultiRecording Architecture & Deep Dive

This document details the internal design, threading model, memory layout, and media pipeline of **MultiRecording**.

---

## 1. Design Philosophy

Content creators recording multi-source gameplay and camera feeds often encounter two problems with standard software like OBS:
1. **High Overhead & Resource Contention**: Traditional software canvas compositing requires continuous GPU/CPU rendering cycles, adding input latency to gaming.
2. **Buffer Volatility & Multitrack Desync**: Writing multiple feeds into a single interleaved file risks corrupting the entire recording if an unexpected system shutdown occurs.

**MultiRecording** solves this with a modular, decoupled architecture:
- **Subprocess Isolation**: Hardware media capture and encoding runs in isolated subprocesses powered by platform-native hardware encoders (Apple Silicon `VideoToolbox` on macOS, NVIDIA `NVENC` / Intel `QSV` / AMD `AMF` on Windows, or `libx264` CPU fallback). A failure in one webcam stream never crashes the main recording engine or corrupts the primary game feed.
- **Hardware Abstraction Layer (HAL)**: The `CapturePlatform` trait decouples platform-specific capture APIs (AVFoundation on macOS, DirectShow + GDI grab + WASAPI on Windows) from recording, ring buffer, and UI logic.
- **Micro-Segmented Ring Buffer**: Media is written in short, autonomous 2-second MP4 chunks. Each chunk contains keyframes (IDR) aligned at exact GOP boundaries.
- **Zero Re-encoding on Export**: Exporting a flashback clip operates via stream copy (`-c copy`), meaning saving a 10-minute 4K clip completes in approximately 1 to 2 seconds.

---

## 2. Process & Threading Model

```
+-----------------------------------------------------------------------------------+
| MultiRecording Main Process (UI / CLI)                                            |
|                                                                                   |
|  [ egui / eframe Render Thread (60 Hz) ]                                         |
|    |                                                                              |
|    +---> Updates preview textures from Arc<Mutex<Option<Vec<u8>>>>                |
|    +---> Polls audio level meters from AtomicU32 / Mutex                          |
|                                                                                   |
|  [ AudioTrackMonitor (Tracks 1..5) ]                                              |
|    +---> Subprocess:                                                             |
|    |       macOS:   ffmpeg -f avfoundation -i :<idx> -f f32le pipe:1              |
|    |       Windows: ffmpeg -f dshow -i audio="<name>" (or wasapi) -f f32le pipe:1 |
|    +---> Dedicated Thread: Reads 8kHz PCM chunks, calculates Peak/RMS -> Mutex    |
|                                                                                   |
|  [ PreviewStream (Standby Preview 1 & 2) ]                                        |
|    +---> Subprocess:                                                             |
|    |       macOS:   ffmpeg -f avfoundation -i <dev>:none -vf scale ... pipe:1     |
|    |       Windows: ffmpeg -f gdigrab -i desktop / -f dshow -i video="<name>"     |
|    +---> Stdout Thread: Reads RGBA 320x180 raw video frames -> Mutex              |
|    +---> Stderr Thread: Probes hardware resolution & framerate -> Mutex           |
|                                                                                   |
|  [ StreamRecorder (Active Engine) ]                                               |
|    +---> Subprocess: ffmpeg (HAL inputs) ... -filter_complex ...                  |
|    |       Output 0: <temp_dir>/multirecording_buffer/<stream>_%Y%m%d_%H%M%S.mp4  |
|    |       Output 1: pipe:1 (RGBA 320x180 live preview)                           |
|    +---> Stdout Thread: Drains preview frames for UI texture upload               |
|    +---> Stderr Thread: Drains logs to <temp_dir>/multirecording_logs/<stream>.log|
+-----------------------------------------------------------------------------------+
```

### Stdio Pipe Stall Prevention
A common issue when spawning media subprocesses in Unix is buffer deadlock: if an FFmpeg process outputs data to `stderr` or `stdout` and the parent process does not continuously drain the pipe, the OS buffer fills up (typically 64KB) and the child process blocks indefinitely.

MultiRecording guarantees deadlock-free streaming by spawning dedicated background threads immediately upon process creation:
1. **`stdout` reader thread**: Continuously drains preview rawvideo frames into a shared `Arc<Mutex<Option<Vec<u8>>>>`.
2. **`stderr` reader thread**: Continuously drains log lines into a ring buffer of strings and to `/tmp/multirecording_logs/<source>.log`, simultaneously checking regex patterns for dynamic stream resolution probing (`\d{3,5}x\d{3,5}`).

---

## 3. Media Pipeline & GOP Boundary Alignment

For lossless stream concatenation to succeed, **every segment must be independently decodable and begin with an IDR keyframe**.

MultiRecording achieves this by locking the Group of Pictures (GOP) size to an exact multiple of the segment duration:
$$\text{GOP Size} = \text{Framerate (fps)} \times \text{Segment Duration (seconds)}$$

For example, at 60 fps with 2-second segments:
- `-r 60`
- `-g 120`
- `-segment_time 2`
- `-reset_timestamps 1`

This ensures that every segment contains exactly 120 frames with an IDR keyframe at index 0, allowing FFmpeg's `concat` demuxer to concatenate arbitrary sequences of segments losslessly without introducing macroblocking or audio/video desync.

---

## 4. Multi-Track Discrete Audio Routing

MultiRecording maps up to 5 audio devices into a single MP4/MOV container.

### macOS Device Contention
AVFoundation does not allow multiple separate capture sessions to bind simultaneously to the same microphone hardware without throwing `Cannot open audio device`.

MultiRecording avoids this by:
1. **Consolidated Primary Ingestion**: All discrete audio inputs (Tracks 1 through 5) are fed exclusively into **Stream 1 (Gameplay)**:
   ```bash
   -i 0:0 -i none:1 -i none:2 -i none:3 ...
   -map [rec] -map 0:a -map 1:a -map 2:a ...
   ```
2. **Metadata Tagging**: Each track is injected with human-readable track titles:
   ```bash
   -metadata:s:a:0 title="Game Audio"
   -metadata:s:a:1 title="Microphone"
   -metadata:s:a:2 title="Discord / Chat"
   ```
3. **Optional Track 1 Mirroring for Video 2**: When `sync_audio_to_secondary` is enabled in Settings, Stream 2 (Facecam) attaches a copy of Track 1 (`-i <video_device>:<track_1_idx>`). This writes the primary audio waveform into both video files without claiming extra devices, allowing NLEs like Premiere Pro and DaVinci Resolve to synchronize the two video tracks via audio waveform alignment.

### Windows Audio Ingestion & Mapping Invariant
Unlike macOS AVFoundation (which bundles video and the first audio stream onto Input 0), Windows captures video independently (Input 0) from DirectShow or WASAPI audio inputs (Inputs 1..N):
```bash
# Windows Gameplay Stream:
-f gdigrab -framerate 60 -i desktop \
-f wasapi -i default \
-f dshow -i audio="Microphone (Realtek Audio)" \
...
-map [rec] -map 1:a -map 2:a ...
```
To guarantee cross-platform mapping safety without conditional spaghetti in `StreamRecorder`, the HAL provides `audio_input_index(track_idx)`:
- **macOS**: `track_idx` (0-indexed; Track 0 is embedded on Input 0).
- **Windows**: `track_idx + 1` (1-indexed; Input 0 is reserved exclusively for video).

### Desktop Audio & Loopback Detection
- **macOS**: AVFoundation cannot tap speaker outputs directly. MultiRecording checks for the `BlackHole 2ch` virtual loopback audio driver and warns the user if an unrouted output device is selected.
- **Windows**: Native desktop loopback is supported out of the box via FFmpeg's WASAPI backend (`-f wasapi -i default`) without needing external loopback drivers.

---

## 5. Ring Buffer Management & Flashback Extraction

The [`RingBufferManager`](file:///Users/tavenharris/Library/Mobile%20Documents/com~apple~CloudDocs/Apps/MultiRecording/src/ring_buffer.rs) struct manages the disk sliding window:

1. **Listing & Sorting**: Reads `<temp_dir>/multirecording_buffer` and filters for files matching `<stream>_%Y%m%d_%H%M%S.mp4`. Files are sorted chronologically by timestamp.
2. **Window Selection**: Given a desired duration $D$ (e.g., 300 seconds for 5 minutes), the manager calculates:
   $$N = \left\lceil \frac{D}{\text{segment\_duration}} \right\rceil$$
   and takes the last $N$ segments from the sorted list.
3. **Eviction**: Segments with a creation age greater than the configured buffer duration plus a 30-second safety cushion are removed from disk to prevent unbounded storage growth.

---

## 6. Remuxing, Timecode & Container Handling

When the user triggers a flashback:
1. MultiRecording extracts the session start timecode from the earliest segment in the selected sequence (or calculates Time-of-Day `HH:MM:SS:00`).
2. Creates a temporary concat manifest file (`multirec_concat_<uuid>.txt`):
   ```text
   file '/path/to/multirecording_buffer/gameplay_20261010_120000.mp4'
   file '/path/to/multirecording_buffer/gameplay_20261010_120002.mp4'
   ...
   ```
3. Invokes FFmpeg concat demuxer:
   ```bash
   ffmpeg -y -f concat -safe 0 -i <manifest> -c copy -timecode 14:32:15:00 -metadata timecode="14:32:15:00" [-movflags +faststart] <output_file>
   ```
4. **Timecode Track (`tmcd`)**: In `.mp4` and `.mov` containers, `-timecode` creates a dedicated SMPTE timecode track (`tmcd`) alongside metadata. Premiere Pro reads this track to enable frame-accurate multi-camera synchronization by Timecode.
5. **Streaming Atom Optimization**: For `.mp4` and `.mov`, `-movflags +faststart` shifts the `moov` atom to the beginning of the file, allowing instant playback and scrubbing in editing timelines. For `.mkv`, Matroska metadata tags are used.

---

## 7. Hardware Abstraction Layer (HAL)

Located in `src/platform/`, the HAL provides platform-specific implementations of the `CapturePlatform` trait:

```rust
pub trait CapturePlatform: Send + Sync {
    fn discover_devices(&self) -> anyhow::Result<Vec<Device>>;
    fn primary_display_resolution(&self) -> Option<String>;
    fn build_preview_input_args(&self, video_device: &str) -> Vec<String>;
    fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[crate::config::AudioTrackConfig],
    ) -> Vec<String>;
    fn audio_input_index(&self, track_idx: usize) -> usize;
    fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String>;
    fn default_video_encoder(&self) -> &'static str;
    fn available_encoders(&self) -> &'static [(&'static str, &'static str)];
    fn has_loopback_support(&self, devices: &[Device]) -> bool;
    fn validate_audio_tracks(
        &self,
        tracks: &[crate::config::AudioTrackConfig],
        devices: &[Device],
    ) -> Result<(), String>;
}
```

The active implementation is exposed as `CurrentPlatform` using conditional compilation (`#[cfg(target_os = "macos")]` and `#[cfg(target_os = "windows")]`), providing a clean, non-invasive integration across `devices.rs`, `recorder.rs`, `engine.rs`, and `ui.rs`. Cross-platform operating system actions (file manager reveal, text editor launch, folder picker dialog, and system audio control panel launch) are provided via platform helpers (`reveal_path`, `reveal_file`, `pick_folder`, `open_system_audio_control`).

