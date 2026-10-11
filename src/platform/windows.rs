use super::CapturePlatform;
use crate::config::AudioTrackConfig;
use crate::devices::{Device, DeviceType};
use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Check if ffmpeg is accessible in the current process's PATH.
/// If not, searches the user's persistent Windows registry PATH (HKCU\Environment\Path)
/// and common installation directories (WinGet Gyan.FFmpeg, Chocolatey, Scoop, etc.)
/// and prepends any found bin directory to `std::env::set_var("PATH", ...)`.
pub fn ensure_ffmpeg_in_path() {
    // 1. Check if ffmpeg is already runnable
    if Command::new("ffmpeg").arg("-version").output().is_ok() {
        return;
    }

    let mut candidate_dirs: Vec<PathBuf> = Vec::new();

    // 2. Check HKCU\Environment\Path (User PATH in registry)
    if let Ok(output) = Command::new("reg")
        .args(["query", "HKCU\\Environment", "/v", "Path"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.trim().starts_with("Path") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let full_val = parts[2..].join(" ");
                    for dir_str in full_val.split(';') {
                        let trimmed = dir_str.trim();
                        if !trimmed.is_empty() {
                            let p = PathBuf::from(trimmed);
                            if p.join("ffmpeg.exe").is_file() {
                                candidate_dirs.push(p);
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Check WinGet Packages directory
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let winget_pkg_dir = Path::new(&local_app_data).join("Microsoft").join("WinGet").join("Packages");
        if winget_pkg_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&winget_pkg_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.to_lowercase().contains("ffmpeg") {
                        if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                            for sub in sub_entries.flatten() {
                                let sub_path = sub.path();
                                if sub_path.join("bin").join("ffmpeg.exe").is_file() {
                                    candidate_dirs.push(sub_path.join("bin"));
                                } else if sub_path.join("ffmpeg.exe").is_file() {
                                    candidate_dirs.push(sub_path);
                                }
                            }
                        }
                    }
                }
            }
        }
        let winget_links = Path::new(&local_app_data).join("Microsoft").join("WinGet").join("Links");
        if winget_links.join("ffmpeg.exe").is_file() {
            candidate_dirs.push(winget_links);
        }
    }

    // 4. Check Scoop
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let scoop_shims = Path::new(&user_profile).join("scoop").join("shims");
        if scoop_shims.join("ffmpeg.exe").is_file() {
            candidate_dirs.push(scoop_shims);
        }
    }

    // 5. Check Chocolatey and standard drive locations
    let standard_paths = [
        "C:\\ProgramData\\chocolatey\\bin",
        "C:\\ffmpeg\\bin",
        "C:\\Program Files\\ffmpeg\\bin",
        "C:\\Program Files (x86)\\ffmpeg\\bin",
    ];
    for p_str in standard_paths {
        let p = PathBuf::from(p_str);
        if p.join("ffmpeg.exe").is_file() {
            candidate_dirs.push(p);
        }
    }

    // Prepend the first valid directory to PATH
    for dir in candidate_dirs {
        if let Ok(current_path) = std::env::var("PATH") {
            let new_path = format!("{};{}", dir.display(), current_path);
            std::env::set_var("PATH", &new_path);
            if Command::new("ffmpeg").arg("-version").output().is_ok() {
                eprintln!("[MultiRecording] Auto-located FFmpeg at: {}", dir.display());
                break;
            }
        }
    }

    // Also ensure cargo is restored to PATH if found in .cargo/bin
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let cargo_bin = Path::new(&user_profile).join(".cargo").join("bin");
        if cargo_bin.join("cargo.exe").is_file() {
            if let Ok(current_path) = std::env::var("PATH") {
                if !current_path.to_lowercase().contains(".cargo\\bin") {
                    std::env::set_var("PATH", format!("{};{}", cargo_bin.display(), current_path));
                }
            }
        }
    }
}

pub fn query_dshow_audio_devices() -> Vec<String> {
    ensure_ffmpeg_in_path();
    if let Ok(output) = Command::new("ffmpeg")
        .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
        .output()
    {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let (_, audios) = parse_dshow_devices(&stderr);
        audios
    } else {
        Vec::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn discover_devices(&self) -> Result<Vec<Device>> {
        ensure_ffmpeg_in_path();

        let mut devices = Vec::new();

        // 1. Enumerate Screen/Desktop as a synthetic video device
        let display_res = self
            .primary_display_resolution()
            .unwrap_or_else(|| "1920x1080".to_string());
        devices.push(Device {
            id: "desktop".to_string(),
            index: 0,
            name: "Primary Display (Screen)".to_string(),
            device_type: DeviceType::Video,
            av_index: None,
            resolution: Some(display_res),
            fps: Some(60),
        });

        // 2. Discover DirectShow video and audio devices via FFmpeg
        let (dshow_videos, dshow_audios) = match Command::new("ffmpeg")
            .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
            .output()
        {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                parse_dshow_devices(&stderr)
            }
            Err(e) => {
                eprintln!("[MultiRecording] Warning: DirectShow discovery via FFmpeg failed: {e}. Ensure FFmpeg is installed.");
                (Vec::new(), Vec::new())
            }
        };

        // Add DirectShow video devices (webcams, capture cards)
        for (i, v_name) in dshow_videos.into_iter().enumerate() {
            let lower = v_name.to_lowercase();
            let is_cam = lower.contains("cam") || lower.contains("face");
            let fps = if is_cam { 30 } else { 60 };
            devices.push(Device {
                id: v_name.clone(),
                index: i + 1,
                name: v_name,
                device_type: DeviceType::Video,
                av_index: None,
                resolution: Some("1920x1080".to_string()),
                fps: Some(fps),
            });
        }

        let host = cpal::default_host();
        let default_out_name = host
            .default_output_device()
            .and_then(|d| d.description().ok())
            .map(|desc| desc.name().to_string());

        // 3. Audio Inputs: DirectShow microphone / input devices + CPAL inputs
        let mut in_idx = 0;
        let mut added_inputs = Vec::new();

        for a_name in dshow_audios {
            if !added_inputs.contains(&a_name) {
                added_inputs.push(a_name.clone());
                devices.push(Device {
                    id: a_name.clone(),
                    index: in_idx,
                    name: a_name,
                    device_type: DeviceType::AudioInput,
                    av_index: None,
                    resolution: None,
                    fps: None,
                });
                in_idx += 1;
            }
        }

        if let Ok(inputs) = host.input_devices() {
            for dev in inputs {
                if let Ok(desc) = dev.description() {
                    let name = desc.name().to_string();
                    if !added_inputs.contains(&name) {
                        added_inputs.push(name.clone());
                        devices.push(Device {
                            id: name.clone(),
                            index: in_idx,
                            name,
                            device_type: DeviceType::AudioInput,
                            av_index: None,
                            resolution: None,
                            fps: None,
                        });
                        in_idx += 1;
                    }
                }
            }
        }

        // 4. Audio Outputs: Query CPAL for real endpoints (Monitors, Speakers, Headphones)
        let mut out_idx = in_idx;
        let mut added_outputs = Vec::new();

        if let Ok(outputs) = host.output_devices() {
            for dev in outputs {
                if let Ok(desc) = dev.description() {
                    let name = desc.name().to_string();
                    if !added_outputs.contains(&name) {
                        added_outputs.push(name.clone());
                        let is_default = default_out_name.as_deref() == Some(&name);
                        let display_name = if is_default {
                            format!("{} (Default Output)", name)
                        } else {
                            name.clone()
                        };
                        devices.push(Device {
                            id: name.clone(),
                            index: out_idx,
                            name: display_name,
                            device_type: DeviceType::AudioOutput,
                            av_index: None,
                            resolution: None,
                            fps: None,
                        });
                        out_idx += 1;
                    }
                }
            }
        }

        // Always ensure a fallback WASAPI loopback device exists if none were enumerated or as generic alias
        devices.push(Device {
            id: "wasapi".to_string(),
            index: out_idx,
            name: match &default_out_name {
                Some(name) => format!("Desktop Audio (Default Output: {})", name),
                None => "Desktop Audio (WASAPI Loopback)".to_string(),
            },
            device_type: DeviceType::AudioOutput,
            av_index: None,
            resolution: None,
            fps: None,
        });

        Ok(devices)
    }

    pub fn primary_display_resolution(&self) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            // SAFETY: Safe Rust cannot directly query Win32 User32 APIs. `GetSystemMetrics`
            // is a read-only query of primary display width (SM_CXSCREEN) and height (SM_CYSCREEN).
            // It takes plain integral constants, performs no raw pointer dereferencing, and produces no side-effects.
            unsafe {
                let w = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
                );
                let h = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows_sys::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
                );
                if w > 0 && h > 0 {
                    return Some(format!("{}x{}", w, h));
                }
            }
        }

        // Fallback: query via PowerShell
        let ps_cmd = "[System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Width.ToString() + 'x' + [System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Height.ToString()";
        let mut ps = Command::new("powershell");
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            ps.creation_flags(0x08000000);
        }
        if let Ok(output) = ps
            .args(["-NoProfile", "-Command", ps_cmd])
            .output()
        {
            let res = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if res.contains('x') {
                return Some(res);
            }
        }

        Some("1920x1080".to_string())
    }

    pub fn build_preview_input_args(&self, video_device: &str) -> Vec<String> {
        let is_desktop = video_device == "desktop"
            || video_device == "0"
            || video_device.to_lowercase().contains("screen")
            || video_device.to_lowercase().contains("desktop");

        if is_desktop {
            vec![
                "-f".to_string(),
                "gdigrab".to_string(),
                "-framerate".to_string(),
                "15".to_string(),
                "-i".to_string(),
                "desktop".to_string(),
                "-an".to_string(),
            ]
        } else {
            let dev_arg = if video_device.starts_with("video=") {
                video_device.to_string()
            } else {
                format!("video={}", video_device)
            };
            vec![
                "-rtbufsize".to_string(),
                "128M".to_string(),
                "-f".to_string(),
                "dshow".to_string(),
                "-i".to_string(),
                dev_arg,
                "-an".to_string(),
            ]
        }
    }

    pub fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[AudioTrackConfig],
    ) -> Vec<String> {
        let mut args = Vec::new();

        // Input 0: Video stream (Desktop via GDI or Webcam/Capture Card via DirectShow)
        let is_desktop = video_device == "desktop"
            || video_device == "0"
            || video_device.to_lowercase().contains("screen")
            || video_device.to_lowercase().contains("desktop");

        if is_desktop {
            args.extend([
                "-f".to_string(),
                "gdigrab".to_string(),
                "-framerate".to_string(),
                in_fps.to_string(),
                "-i".to_string(),
                "desktop".to_string(),
            ]);
        } else {
            let dev_arg = if video_device.starts_with("video=") {
                video_device.to_string()
            } else {
                format!("video={}", video_device)
            };
            args.extend([
                "-rtbufsize".to_string(),
                "256M".to_string(),
                "-f".to_string(),
                "dshow".to_string(),
                "-i".to_string(),
                dev_arg,
            ]);
        }

        // Query DirectShow audio hardware devices once upfront for all tracks
        let dshow_audios = query_dshow_audio_devices();

        // Inputs 1..N: Enabled audio tracks
        for track in enabled_tracks {
            let id = &track.device_id;
            let is_wasapi = id == "wasapi"
                || id == "default"
                || id.starts_with("out:")
                || id.to_lowercase().contains("wasapi");

            let clean_name = id
                .strip_prefix("out:")
                .or_else(|| id.strip_prefix("audio="))
                .or_else(|| id.strip_prefix("in:"))
                .unwrap_or(id)
                .trim();

            if is_wasapi {
                if let Some((port, sr)) = start_wasapi_loopback_bridge(clean_name) {
                    args.extend([
                        "-thread_queue_size".to_string(),
                        "1024".to_string(),
                        "-f".to_string(),
                        "f32le".to_string(),
                        "-sample_rate".to_string(),
                        sr.to_string(),
                        "-ch_layout".to_string(),
                        "stereo".to_string(),
                        "-i".to_string(),
                        format!("tcp://127.0.0.1:{}", port),
                    ]);
                } else {
                    args.extend([
                        "-f".to_string(),
                        "lavfi".to_string(),
                        "-i".to_string(),
                        "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
                    ]);
                }
            } else if clean_name.is_empty() || clean_name == "none" {
                args.extend([
                    "-f".to_string(),
                    "lavfi".to_string(),
                    "-i".to_string(),
                    "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
                ]);
            } else if let Ok(num) = clean_name.parse::<usize>() {
                if let Some(real_name) = dshow_audios.get(num) {
                    args.extend([
                        "-f".to_string(),
                        "dshow".to_string(),
                        "-i".to_string(),
                        format!("audio={}", real_name),
                    ]);
                } else if let Some(first) = dshow_audios.first() {
                    args.extend([
                        "-f".to_string(),
                        "dshow".to_string(),
                        "-i".to_string(),
                        format!("audio={}", first),
                    ]);
                } else {
                    args.extend([
                        "-f".to_string(),
                        "lavfi".to_string(),
                        "-i".to_string(),
                        "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
                    ]);
                }
            } else {
                let is_output_name = clean_name.to_lowercase().contains("speaker")
                    || clean_name.to_lowercase().contains("output")
                    || clean_name.to_lowercase().contains("headphones")
                    || clean_name.to_lowercase().contains("monitor")
                    || clean_name.to_lowercase().contains("desktop");

                // 1. Exact match in DirectShow audio inputs (e.g. Microphone / Capture Card)
                let exact_dshow = dshow_audios.iter().find(|d| {
                    d.eq_ignore_ascii_case(clean_name)
                        || clean_name.eq_ignore_ascii_case(d.as_str())
                });

                if let Some(real_name) = exact_dshow {
                    args.extend([
                        "-f".to_string(),
                        "dshow".to_string(),
                        "-i".to_string(),
                        format!("audio={}", real_name),
                    ]);
                } else if is_output_name || is_cpal_output_device(clean_name) {
                    // 2. Audio Output endpoint (e.g. Speakers (Razer BlackShark V2 X USB), ED270R M):
                    // Capture natively via our in-process WASAPI Loopback -> TCP PCM bridge!
                    if let Some((port, sr)) = start_wasapi_loopback_bridge(clean_name) {
                        args.extend([
                            "-thread_queue_size".to_string(),
                            "1024".to_string(),
                            "-f".to_string(),
                            "f32le".to_string(),
                            "-sample_rate".to_string(),
                            sr.to_string(),
                            "-ch_layout".to_string(),
                            "stereo".to_string(),
                            "-i".to_string(),
                            format!("tcp://127.0.0.1:{}", port),
                        ]);
                    } else {
                        args.extend([
                            "-f".to_string(),
                            "lavfi".to_string(),
                            "-i".to_string(),
                            "anullsrc=channel_layout=stereo:sample_rate=48000".to_string(),
                        ]);
                    }
                } else if let Some(real_name) = dshow_audios.iter().find(|d| {
                    d.to_lowercase().contains(&clean_name.to_lowercase())
                        || clean_name.to_lowercase().contains(&d.to_lowercase())
                }) {
                    args.extend([
                        "-f".to_string(),
                        "dshow".to_string(),
                        "-i".to_string(),
                        format!("audio={}", real_name),
                    ]);
                } else {
                    let audio_arg = if clean_name.starts_with("audio=") {
                        clean_name.to_string()
                    } else {
                        format!("audio={}", clean_name)
                    };
                    args.extend([
                        "-f".to_string(),
                        "dshow".to_string(),
                        "-i".to_string(),
                        audio_arg,
                    ]);
                }
            }
        }

        args
    }

    pub fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String> {
        let is_wasapi = audio_device_identifier == "wasapi"
            || audio_device_identifier == "default"
            || audio_device_identifier.starts_with("out:")
            || audio_device_identifier.to_lowercase().contains("wasapi");

        if is_wasapi {
            vec![
                "-f".to_string(),
                "wasapi".to_string(),
                "-i".to_string(),
                "default".to_string(),
                "-vn".to_string(),
                "-ac".to_string(),
                "2".to_string(),
                "-ar".to_string(),
                "8000".to_string(),
                "-f".to_string(),
                "f32le".to_string(),
                "pipe:1".to_string(),
            ]
        } else {
            let clean = audio_device_identifier
                .strip_prefix("audio=")
                .or_else(|| audio_device_identifier.strip_prefix("in:"))
                .unwrap_or(audio_device_identifier);
            let input_arg = if clean.starts_with("audio=") {
                clean.to_string()
            } else {
                format!("audio={}", clean)
            };
            vec![
                "-f".to_string(),
                "dshow".to_string(),
                "-i".to_string(),
                input_arg,
                "-vn".to_string(),
                "-ac".to_string(),
                "2".to_string(),
                "-ar".to_string(),
                "8000".to_string(),
                "-f".to_string(),
                "f32le".to_string(),
                "pipe:1".to_string(),
            ]
        }
    }

    pub fn default_video_encoder(&self) -> &'static str {
        detect_best_windows_encoder()
    }

    pub fn has_loopback_support(&self, _devices: &[Device]) -> bool {
        // Windows natively supports capturing any AudioOutput endpoint via WASAPI Loopback
        true
    }

    pub fn audio_input_index(&self, track_idx: usize) -> usize {
        // Input 0 is reserved for video (gdigrab/dshow), audio starts at input 1
        track_idx + 1
    }

    pub fn available_encoders(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("h264_nvenc", "NVIDIA NVENC (Hardware)"),
            ("h264_qsv", "Intel QuickSync (Hardware)"),
            ("h264_amf", "AMD AMF (Hardware)"),
            ("libx264", "libx264 (CPU)"),
        ]
    }

    pub fn validate_audio_tracks(
        &self,
        _tracks: &[AudioTrackConfig],
        _devices: &[Device],
    ) -> Result<(), String> {
        // Windows supports both DirectShow AudioInput and WASAPI Loopback AudioOutput natively
        Ok(())
    }
}

impl CapturePlatform for WindowsPlatform {
    fn discover_devices(&self) -> Result<Vec<Device>> {
        self.discover_devices()
    }

    fn primary_display_resolution(&self) -> Option<String> {
        self.primary_display_resolution()
    }

    fn build_preview_input_args(&self, video_device: &str) -> Vec<String> {
        self.build_preview_input_args(video_device)
    }

    fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[AudioTrackConfig],
    ) -> Vec<String> {
        self.build_recording_input_args(video_device, in_fps, enabled_tracks)
    }

    fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String> {
        self.build_audio_monitor_args(audio_device_identifier)
    }

    fn default_video_encoder(&self) -> &'static str {
        self.default_video_encoder()
    }

    fn has_loopback_support(&self, devices: &[Device]) -> bool {
        self.has_loopback_support(devices)
    }

    fn audio_input_index(&self, track_idx: usize) -> usize {
        self.audio_input_index(track_idx)
    }

    fn available_encoders(&self) -> &'static [(&'static str, &'static str)] {
        self.available_encoders()
    }

    fn validate_audio_tracks(
        &self,
        tracks: &[AudioTrackConfig],
        devices: &[Device],
    ) -> Result<(), String> {
        self.validate_audio_tracks(tracks, devices)
    }
}

/// Checks whether a given device identifier matches any Windows Audio Output endpoint via CPAL.
fn is_cpal_output_device(identifier: &str) -> bool {
    let clean = identifier
        .replace(" (Default Output)", "")
        .replace(" (WASAPI Loopback)", "")
        .trim()
        .to_lowercase();
    if clean.is_empty() {
        return false;
    }
    let host = cpal::default_host();
    if let Ok(outputs) = host.output_devices() {
        for dev in outputs {
            if let Ok(desc) = dev.description() {
                let d_name = desc.name().to_lowercase();
                if d_name == clean || d_name.contains(&clean) || clean.contains(&d_name) {
                    return true;
                }
            }
        }
    }
    false
}

/// Starts a local in-process WASAPI Loopback -> TCP PCM (`f32le` stereo) bridge for FFmpeg.
/// Because standard Windows FFmpeg builds lack a native `wasapi` input demuxer and DirectShow
/// only exposes microphones/inputs, this bridge captures any Windows Audio Output endpoint
/// (e.g. `Speakers (Razer BlackShark V2 X USB)`, `ED270R M`, or default desktop output)
/// in shared WASAPI Loopback mode via `cpal` and streams continuous stereo `f32le` PCM over
/// `127.0.0.1:<port>` to FFmpeg.
///
/// Includes a wall-clock-synchronized silence generator when Windows pauses WASAPI loopback
/// callbacks during desktop silence so FFmpeg never stalls or drops segments.
pub fn start_wasapi_loopback_bridge(identifier: &str) -> Option<(u16, u32)> {
    let host = cpal::default_host();
    let clean_id = identifier
        .strip_prefix("out:")
        .or_else(|| identifier.strip_prefix("audio="))
        .or_else(|| identifier.strip_prefix("in:"))
        .unwrap_or(identifier)
        .replace(" (Default Output)", "")
        .replace(" (WASAPI Loopback)", "")
        .trim()
        .to_string();

    let is_default_alias = clean_id.is_empty()
        || clean_id.eq_ignore_ascii_case("wasapi")
        || clean_id.eq_ignore_ascii_case("default")
        || clean_id.to_lowercase().contains("desktop");

    let mut target_device = None;
    if !is_default_alias {
        let search = clean_id.to_lowercase();
        if let Ok(outputs) = host.output_devices() {
            for dev in outputs {
                if let Ok(desc) = dev.description() {
                    let d_name = desc.name().to_lowercase();
                    if d_name == search || d_name.contains(&search) || search.contains(&d_name) {
                        target_device = Some(dev);
                        break;
                    }
                }
            }
        }
    }
    if target_device.is_none() {
        target_device = host.default_output_device();
    }

    let dev = target_device?;
    let supported_config = dev.default_output_config().ok()?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let sample_rate = stream_config.sample_rate;
    let channels = (stream_config.channels as usize).max(1);

    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let _ = listener.set_nonblocking(true);

    let clean_id_for_thread = clean_id.clone();
    std::thread::spawn(move || {
        // Wait up to 15 seconds for FFmpeg subprocess to connect to our local TCP socket
        let mut tcp_stream = None;
        for _ in 0..1500 {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    tcp_stream = Some(stream);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }

        let mut tcp_stream = match tcp_stream {
            Some(s) => s,
            None => return,
        };

        // Re-acquire output device inside the worker thread to own the CPAL stream handle
        let host = cpal::default_host();
        let mut thread_dev = None;
        if !is_default_alias {
            let search = clean_id_for_thread.to_lowercase();
            if let Ok(outputs) = host.output_devices() {
                for d in outputs {
                    if let Ok(desc) = d.description() {
                        let d_name = desc.name().to_lowercase();
                        if d_name == search || d_name.contains(&search) || search.contains(&d_name) {
                            thread_dev = Some(d);
                            break;
                        }
                    }
                }
            }
        }
        if thread_dev.is_none() {
            thread_dev = host.default_output_device();
        }

        let audio_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(65536)));
        let buf_cb = Arc::clone(&audio_buf);

        let push_stereo_f32 = move |samples_f32: &[f32]| {
            if let Ok(mut guard) = buf_cb.lock() {
                if guard.len() < (sample_rate as usize * 8) {
                    for frame in samples_f32.chunks(channels) {
                        let l = frame[0];
                        let r = if channels > 1 { frame[1] } else { l };
                        guard.extend_from_slice(&l.to_le_bytes());
                        guard.extend_from_slice(&r.to_le_bytes());
                    }
                }
            }
        };

        let _cpal_stream = thread_dev.and_then(|d| {
            let stream_res = match sample_format {
                cpal::SampleFormat::F32 => {
                    let push = push_stereo_f32;
                    d.build_input_stream(
                        stream_config,
                        move |data: &[f32], _| {
                            push(data);
                        },
                        |_| {},
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let buf_i16 = Arc::clone(&audio_buf);
                    d.build_input_stream(
                        stream_config,
                        move |data: &[i16], _| {
                            if let Ok(mut guard) = buf_i16.lock() {
                                if guard.len() < (sample_rate as usize * 8) {
                                    for frame in data.chunks(channels) {
                                        let l = frame[0] as f32 / 32768.0;
                                        let r = if channels > 1 { frame[1] as f32 / 32768.0 } else { l };
                                        guard.extend_from_slice(&l.to_le_bytes());
                                        guard.extend_from_slice(&r.to_le_bytes());
                                    }
                                }
                            }
                        },
                        |_| {},
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let buf_u16 = Arc::clone(&audio_buf);
                    d.build_input_stream(
                        stream_config,
                        move |data: &[u16], _| {
                            if let Ok(mut guard) = buf_u16.lock() {
                                if guard.len() < (sample_rate as usize * 8) {
                                    for frame in data.chunks(channels) {
                                        let l = (frame[0] as f32 - 32768.0) / 32768.0;
                                        let r = if channels > 1 { (frame[1] as f32 - 32768.0) / 32768.0 } else { l };
                                        guard.extend_from_slice(&l.to_le_bytes());
                                        guard.extend_from_slice(&r.to_le_bytes());
                                    }
                                }
                            }
                        },
                        |_| {},
                        None,
                    )
                }
                _ => return None,
            };
            let s = stream_res.ok()?;
            s.play().ok()?;
            Some(s)
        });

        // Immediately send 50ms of stereo f32le silence so FFmpeg stream probing completes right away
        let primer_frames = (sample_rate as usize) / 20;
        let primer_bytes = vec![0u8; primer_frames * 8];
        if tcp_stream.write_all(&primer_bytes).is_err() {
            return;
        }
        let _ = tcp_stream.flush();

        let mut last_audio_instant = Instant::now();
        let silence_threshold = Duration::from_millis(25);
        let zero_chunk = [0u8; 4096];

        loop {
            std::thread::sleep(Duration::from_millis(5));
            let now = Instant::now();

            let chunk = {
                if let Ok(mut guard) = audio_buf.lock() {
                    if !guard.is_empty() {
                        let data = std::mem::take(&mut *guard);
                        Some(data)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(data) = chunk {
                last_audio_instant = now;
                if tcp_stream.write_all(&data).is_err() {
                    break;
                }
            } else {
                // WASAPI Loopback pauses callbacks when no application is outputting sound.
                // Generate wall-clock-accurate silent stereo frames so FFmpeg continues recording smoothly.
                let elapsed = now.duration_since(last_audio_instant);
                if elapsed >= silence_threshold {
                    let silent_frames = (elapsed.as_secs_f64() * sample_rate as f64) as usize;
                    if silent_frames > 0 {
                        last_audio_instant +=
                            Duration::from_secs_f64(silent_frames as f64 / sample_rate as f64);
                        let mut remaining_bytes = silent_frames * 8;
                        let mut write_err = false;
                        while remaining_bytes > 0 {
                            let to_write = remaining_bytes.min(zero_chunk.len());
                            if tcp_stream.write_all(&zero_chunk[..to_write]).is_err() {
                                write_err = true;
                                break;
                            }
                            remaining_bytes -= to_write;
                        }
                        if write_err {
                            break;
                        }
                    }
                }
            }
        }
    });

    Some((port, sample_rate))
}

/// Spawns a real-time audio monitor stream using native Windows CoreAudio WASAPI loopback or capture via CPAL.
/// Monitors peak levels directly without spawning external subprocesses.
#[allow(dead_code)]
pub fn start_cpal_monitor(
    identifier: &str,
    peak_left: Arc<AtomicU32>,
    peak_right: Arc<AtomicU32>,
) -> Option<cpal::Stream> {
    let host = cpal::default_host();
    let clean_id = identifier
        .strip_prefix("out:")
        .or_else(|| identifier.strip_prefix("audio="))
        .or_else(|| identifier.strip_prefix("in:"))
        .unwrap_or(identifier)
        .trim();

    let clean_id_normalized = clean_id
        .replace(" (Default Output)", "")
        .replace(" (WASAPI Loopback)", "");

    let is_explicit_output = clean_id.eq_ignore_ascii_case("wasapi")
        || clean_id.eq_ignore_ascii_case("default")
        || clean_id.to_lowercase().contains("desktop")
        || identifier.starts_with("out:");

    let mut target_device = None;
    let mut is_loopback = false;

    let matches_name = |desc_name: &str, search: &str| -> bool {
        let dn = desc_name.to_lowercase();
        let s = search.to_lowercase();
        dn == s || dn.contains(&s) || s.contains(&dn)
    };

    if is_explicit_output {
        // Explicit loopback output request
        if let Ok(outputs) = host.output_devices() {
            for dev in outputs {
                if let Ok(desc) = dev.description() {
                    let d_name = desc.name();
                    if matches_name(d_name, &clean_id_normalized) {
                        target_device = Some(dev);
                        is_loopback = true;
                        break;
                    }
                }
            }
        }
        if target_device.is_none() {
            target_device = host.default_output_device();
            is_loopback = true;
        }
    } else {
        // Check output devices first (e.g. Speakers, Monitors like ED270R M)
        if let Ok(outputs) = host.output_devices() {
            for dev in outputs {
                if let Ok(desc) = dev.description() {
                    let d_name = desc.name();
                    if matches_name(d_name, &clean_id_normalized) {
                        target_device = Some(dev);
                        is_loopback = true;
                        break;
                    }
                }
            }
        }

        // If not matched in outputs, check input devices (Microphones, capture cards)
        if target_device.is_none() {
            if let Ok(inputs) = host.input_devices() {
                for dev in inputs {
                    if let Ok(desc) = dev.description() {
                        let d_name = desc.name();
                        if matches_name(d_name, &clean_id_normalized) {
                            target_device = Some(dev);
                            is_loopback = false;
                            break;
                        }
                    }
                }
            }
        }

        // Fallback to default output device if still not found
        if target_device.is_none() {
            target_device = host.default_output_device();
            is_loopback = true;
        }
    }

    let dev = target_device?;

    let supported_config = if is_loopback {
        dev.default_output_config().ok()?
    } else {
        dev.default_input_config().ok()?
    };

    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let channels = (stream_config.channels as usize).max(1);

    let peak_l = Arc::clone(&peak_left);
    let peak_r = Arc::clone(&peak_right);
    let stream = match sample_format {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            stream_config,
            move |data: &[f32], _| {
                let mut max_l: f32 = 0.0;
                let mut max_r: f32 = 0.0;
                for frame in data.chunks(channels) {
                    let l = frame[0].abs();
                    let r = if channels > 1 { frame[1].abs() } else { l };
                    if l > max_l {
                        max_l = l;
                    }
                    if r > max_r {
                        max_r = r;
                    }
                }
                let boost_l = (max_l * 2.5).clamp(0.0, 1.0);
                let boost_r = (max_r * 2.5).clamp(0.0, 1.0);
                peak_l.store((boost_l * 10000.0) as u32, Ordering::Relaxed);
                peak_r.store((boost_r * 10000.0) as u32, Ordering::Relaxed);
            },
            |err| {
                let err_str = format!("{:?}", err);
                if !err_str.contains("Xrun") {
                    eprintln!("[MultiRecording] Audio monitor stream error: {}", err_str);
                }
            },
            None,
        ),
        cpal::SampleFormat::I16 => dev.build_input_stream(
            stream_config,
            move |data: &[i16], _| {
                let mut max_l: f32 = 0.0;
                let mut max_r: f32 = 0.0;
                for frame in data.chunks(channels) {
                    let l = (frame[0] as f32 / 32768.0).abs();
                    let r = if channels > 1 {
                        (frame[1] as f32 / 32768.0).abs()
                    } else {
                        l
                    };
                    if l > max_l {
                        max_l = l;
                    }
                    if r > max_r {
                        max_r = r;
                    }
                }
                let boost_l = (max_l * 2.5).clamp(0.0, 1.0);
                let boost_r = (max_r * 2.5).clamp(0.0, 1.0);
                peak_l.store((boost_l * 10000.0) as u32, Ordering::Relaxed);
                peak_r.store((boost_r * 10000.0) as u32, Ordering::Relaxed);
            },
            |err| {
                let err_str = format!("{:?}", err);
                if !err_str.contains("Xrun") {
                    eprintln!("[MultiRecording] Audio monitor stream error: {}", err_str);
                }
            },
            None,
        ),
        cpal::SampleFormat::U16 => dev.build_input_stream(
            stream_config,
            move |data: &[u16], _| {
                let mut max_l: f32 = 0.0;
                let mut max_r: f32 = 0.0;
                for frame in data.chunks(channels) {
                    let l = ((frame[0] as f32 - 32768.0) / 32768.0).abs();
                    let r = if channels > 1 {
                        ((frame[1] as f32 - 32768.0) / 32768.0).abs()
                    } else {
                        l
                    };
                    if l > max_l {
                        max_l = l;
                    }
                    if r > max_r {
                        max_r = r;
                    }
                }
                let boost_l = (max_l * 2.5).clamp(0.0, 1.0);
                let boost_r = (max_r * 2.5).clamp(0.0, 1.0);
                peak_l.store((boost_l * 10000.0) as u32, Ordering::Relaxed);
                peak_r.store((boost_r * 10000.0) as u32, Ordering::Relaxed);
            },
            |err| {
                let err_str = format!("{:?}", err);
                if !err_str.contains("Xrun") {
                    eprintln!("[MultiRecording] Audio monitor stream error: {}", err_str);
                }
            },
            None,
        ),
        _ => return None,
    }.ok()?;

    if stream.play().is_ok() {
        Some(stream)
    } else {
        None
    }
}

pub fn detect_best_windows_encoder() -> &'static str {
    static ENCODER: OnceLock<&'static str> = OnceLock::new();
    ENCODER.get_or_init(|| {
        ensure_ffmpeg_in_path();
        if let Ok(output) = Command::new("ffmpeg").args(["-encoders"]).output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("h264_nvenc") {
                return "h264_nvenc";
            }
            if stdout.contains("h264_qsv") {
                return "h264_qsv";
            }
            if stdout.contains("h264_amf") {
                return "h264_amf";
            }
        }
        "libx264"
    })
}

/// Parses DirectShow device enumeration from FFmpeg stderr output
pub fn parse_dshow_devices(output: &str) -> (Vec<String>, Vec<String>) {
    let mut video_devices = Vec::new();
    let mut audio_devices = Vec::new();
    let mut current_section: Option<DeviceType> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // FFmpeg 9+ format: inline "(video)" or "(audio)" tags
        if trimmed.contains("(video)") && !trimmed.contains("Alternative name") {
            if let Some(name) = extract_quoted_string(trimmed) {
                if !video_devices.contains(&name) {
                    video_devices.push(name);
                }
                continue;
            }
        }
        if trimmed.contains("(audio)") && !trimmed.contains("Alternative name") {
            if let Some(name) = extract_quoted_string(trimmed) {
                if !audio_devices.contains(&name) {
                    audio_devices.push(name);
                }
                continue;
            }
        }

        // Standard / older FFmpeg format: section headers
        if trimmed.contains("DirectShow video devices") {
            current_section = Some(DeviceType::Video);
            continue;
        } else if trimmed.contains("DirectShow audio devices") {
            current_section = Some(DeviceType::AudioInput);
            continue;
        }

        if let Some(ref section) = current_section {
            if trimmed.contains("Alternative name") {
                continue;
            }
            if let Some(name) = extract_quoted_string(trimmed) {
                match section {
                    DeviceType::Video if !video_devices.contains(&name) => {
                        video_devices.push(name);
                    }
                    DeviceType::AudioInput if !audio_devices.contains(&name) => {
                        audio_devices.push(name);
                    }
                    _ => {}
                }
            }
        }
    }

    (video_devices, audio_devices)
}

fn extract_quoted_string(s: &str) -> Option<String> {
    let start = s.find('"')?;
    let end = s[start + 1..].find('"')?;
    Some(s[start + 1..start + 1 + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dshow_devices_classic_format() {
        let sample = r#"
[dshow @ 000001e3b6a94f00] DirectShow video devices (some may be both video and audio devices)
[dshow @ 000001e3b6a94f00]  "Integrated Camera"
[dshow @ 000001e3b6a94f00]     Alternative name "@device_pnp_\\?\usb#vid_04f2&pid_b6d9&mi_00#6&188825f&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global"
[dshow @ 000001e3b6a94f00]  "OBS Virtual Camera"
[dshow @ 000001e3b6a94f00] DirectShow audio devices
[dshow @ 000001e3b6a94f00]  "Microphone Array (Realtek Audio)"
[dshow @ 000001e3b6a94f00]     Alternative name "@device_cm_{33D9A762-90C8-11D0-BD43-00A0C911CE86}\wave_{453F7767-165D-47C2-9E1F-B198DC6B3212}"
"#;
        let (videos, audios) = parse_dshow_devices(sample);
        assert_eq!(videos, vec!["Integrated Camera", "OBS Virtual Camera"]);
        assert_eq!(audios, vec!["Microphone Array (Realtek Audio)"]);
    }

    #[test]
    fn test_parse_dshow_devices_ffmpeg9_format() {
        let sample = r#"
[in#0 @ 000001796ba40400] "Game Capture HD60 Pro" (video)
[in#0 @ 000001796ba40400]   Alternative name "@device_pnp_\\?\pci#..."
[in#0 @ 000001796ba40400] "OBS Virtual Camera" (video)
[in#0 @ 000001796ba40400] "Microphone (Game Capture HD60 Pro)" (audio)
[in#0 @ 000001796ba40400] "Game Capture HD60 Pro Audio" (audio)
"#;
        let (videos, audios) = parse_dshow_devices(sample);
        assert_eq!(videos, vec!["Game Capture HD60 Pro", "OBS Virtual Camera"]);
        assert_eq!(
            audios,
            vec![
                "Microphone (Game Capture HD60 Pro)",
                "Game Capture HD60 Pro Audio"
            ]
        );
    }

    #[test]
    fn test_windows_preview_args() {
        let win = WindowsPlatform;
        let desk_args = win.build_preview_input_args("desktop");
        assert_eq!(
            desk_args,
            vec!["-f", "gdigrab", "-framerate", "15", "-i", "desktop", "-an"]
        );

        let cam_args = win.build_preview_input_args("Integrated Camera");
        assert_eq!(
            cam_args,
            vec![
                "-rtbufsize",
                "128M",
                "-f",
                "dshow",
                "-i",
                "video=Integrated Camera",
                "-an"
            ]
        );
    }

    #[test]
    fn test_windows_recording_args() {
        let win = WindowsPlatform;
        let tracks = vec![
            AudioTrackConfig {
                enabled: true,
                name: "Mic".to_string(),
                device_index: 0,
                device_id: "Microphone Array".to_string(),
                volume: 1.0,
            },
            AudioTrackConfig {
                enabled: true,
                name: "Desktop Audio".to_string(),
                device_index: 1,
                device_id: "wasapi".to_string(),
                volume: 1.0,
            },
        ];

        let args = win.build_recording_input_args("desktop", 60, &tracks);
        assert_eq!(&args[0..10], &[
            "-f",
            "gdigrab",
            "-framerate",
            "60",
            "-i",
            "desktop",
            "-f",
            "dshow",
            "-i",
            "audio=Microphone Array",
        ]);
        // Track 1 ("wasapi") uses the native WASAPI Loopback TCP bridge (or lavfi fallback on headless CI)
        assert!(
            args.iter().any(|a| a.starts_with("tcp://127.0.0.1:"))
                || args.contains(&"anullsrc=channel_layout=stereo:sample_rate=48000".to_string())
        );
        assert_eq!(win.audio_input_index(0), 1);
        assert_eq!(win.audio_input_index(1), 2);
    }

    #[test]
    fn test_windows_audio_monitor_args() {
        let win = WindowsPlatform;
        let wasapi_args = win.build_audio_monitor_args("wasapi");
        assert_eq!(
            wasapi_args,
            vec![
                "-f", "wasapi", "-i", "default", "-vn", "-ac", "2", "-ar", "8000", "-f",
                "f32le", "pipe:1"
            ]
        );

        let mic_args = win.build_audio_monitor_args("Microphone Array");
        assert_eq!(
            mic_args,
            vec![
                "-f",
                "dshow",
                "-i",
                "audio=Microphone Array",
                "-vn",
                "-ac",
                "2",
                "-ar",
                "8000",
                "-f",
                "f32le",
                "pipe:1"
            ]
        );
    }

    #[test]
    fn test_windows_recording_args_numeric_and_empty_fallback() {
        let win = WindowsPlatform;
        let tracks = vec![
            AudioTrackConfig {
                enabled: true,
                name: "Taven’s AirPods Pro".to_string(),
                device_index: 0,
                device_id: "in:0".to_string(),
                volume: 1.0,
            },
            AudioTrackConfig {
                enabled: true,
                name: "None Track".to_string(),
                device_index: 1,
                device_id: "none".to_string(),
                volume: 1.0,
            },
        ];

        let args = win.build_recording_input_args("desktop", 30, &tracks);
        // Track 0 was "in:0" (Mac style numeric index) -> must NEVER produce "audio=0"
        assert!(!args.contains(&"audio=0".to_string()));
        // Track 1 was "none" -> should fall back to lavfi anullsrc silent generator
        assert!(args.contains(&"anullsrc=channel_layout=stereo:sample_rate=48000".to_string()));
    }
}
