use super::CapturePlatform;
use crate::config::AudioTrackConfig;
use crate::devices::{Device, DeviceType};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

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
        match Command::new("ffmpeg")
            .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
            .output()
        {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let (dshow_videos, dshow_audios) = parse_dshow_devices(&stderr);

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

                // 3. Audio Outputs: WASAPI loopback device for desktop speaker/headphone recording
                devices.push(Device {
                    id: "wasapi".to_string(),
                    index: 0,
                    name: "Desktop Audio (WASAPI Loopback)".to_string(),
                    device_type: DeviceType::AudioOutput,
                    av_index: Some(0),
                    resolution: None,
                    fps: None,
                });

                // 4. Audio Inputs: DirectShow microphone / input devices
                for (i, a_name) in dshow_audios.into_iter().enumerate() {
                    devices.push(Device {
                        id: a_name.clone(),
                        index: i,
                        name: a_name,
                        device_type: DeviceType::AudioInput,
                        av_index: Some(i),
                        resolution: None,
                        fps: None,
                    });
                }
            }
            Err(e) => {
                // Preserve synthetic devices even if ffmpeg failed
                devices.push(Device {
                    id: "wasapi".to_string(),
                    index: 0,
                    name: "Desktop Audio (WASAPI Loopback)".to_string(),
                    device_type: DeviceType::AudioOutput,
                    av_index: Some(0),
                    resolution: None,
                    fps: None,
                });
                eprintln!("[MultiRecording] Warning: DirectShow discovery via FFmpeg failed: {e}. Ensure FFmpeg is installed.");
            }
        }

        Ok(devices)
    }

    pub fn primary_display_resolution(&self) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
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
        if let Ok(output) = Command::new("powershell")
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
                "-f".to_string(),
                "dshow".to_string(),
                "-framerate".to_string(),
                "15".to_string(),
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
                "-f".to_string(),
                "dshow".to_string(),
                "-framerate".to_string(),
                in_fps.to_string(),
                "-i".to_string(),
                dev_arg,
            ]);
        }

        // Inputs 1..N: Enabled audio tracks
        for track in enabled_tracks {
            let id = &track.device_id;
            let is_wasapi = id == "wasapi"
                || id == "default"
                || id.starts_with("out:")
                || id.to_lowercase().contains("wasapi")
                || track.name.to_lowercase().contains("desktop")
                || track.name.to_lowercase().contains("speaker");

            if is_wasapi {
                args.extend([
                    "-f".to_string(),
                    "wasapi".to_string(),
                    "-i".to_string(),
                    "default".to_string(),
                ]);
            } else {
                let clean_name = id
                    .strip_prefix("audio=")
                    .or_else(|| id.strip_prefix("in:"))
                    .unwrap_or(id);
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
                "1".to_string(),
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
                "1".to_string(),
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
        // Windows natively supports WASAPI loopback capture on all endpoints
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
}

pub fn detect_best_windows_encoder() -> &'static str {
    static ENCODER: OnceLock<&'static str> = OnceLock::new();
    *ENCODER.get_or_init(|| {
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
                    DeviceType::Video => {
                        if !video_devices.contains(&name) {
                            video_devices.push(name);
                        }
                    }
                    DeviceType::AudioInput => {
                        if !audio_devices.contains(&name) {
                            audio_devices.push(name);
                        }
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
                "-f",
                "dshow",
                "-framerate",
                "15",
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
            },
            AudioTrackConfig {
                enabled: true,
                name: "Desktop Audio".to_string(),
                device_index: 1,
                device_id: "wasapi".to_string(),
            },
        ];

        let args = win.build_recording_input_args("desktop", 60, &tracks);
        assert_eq!(
            args,
            vec![
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
                "-f",
                "wasapi",
                "-i",
                "default",
            ]
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
                "-f", "wasapi", "-i", "default", "-vn", "-ac", "1", "-ar", "8000", "-f",
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
                "1",
                "-ar",
                "8000",
                "-f",
                "f32le",
                "pipe:1"
            ]
        );
    }
}
