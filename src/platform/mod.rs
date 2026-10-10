use crate::devices::Device;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub trait CapturePlatform: Send + Sync {
    /// Enumerate video input devices and audio input/output devices
    fn discover_devices(&self) -> anyhow::Result<Vec<Device>>;

    /// Query the primary display resolution (e.g. "1920x1080", "2560x1440")
    fn primary_display_resolution(&self) -> Option<String>;

    /// Build FFmpeg CLI arguments for standalone preview streaming (320x180 RGBA pipe)
    fn build_preview_input_args(&self, video_device: &str) -> Vec<String>;

    /// Build FFmpeg CLI arguments for recording input streams (video + primary audio, plus extra audio tracks)
    fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[crate::config::AudioTrackConfig],
    ) -> Vec<String>;

    /// Build FFmpeg CLI arguments for real-time audio VU meter monitoring (raw f32le PCM)
    fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String>;

    /// Select default hardware/software encoder for the platform
    fn default_video_encoder(&self) -> &'static str;

    /// Returns true if desktop/loopback audio capture capability is available
    fn has_loopback_support(&self, devices: &[Device]) -> bool;

    /// Returns the FFmpeg input index for the k-th enabled audio track
    fn audio_input_index(&self, track_idx: usize) -> usize {
        track_idx
    }

    /// List of available encoders and their user-friendly labels for UI
    fn available_encoders(&self) -> &'static [(&'static str, &'static str)];

    /// Validates audio tracks before starting the recording engine.
    /// Returns Err(warning_message) if an audio configuration issue is detected on the current platform.
    fn validate_audio_tracks(
        &self,
        tracks: &[crate::config::AudioTrackConfig],
        devices: &[Device],
    ) -> Result<(), String> {
        let _ = (tracks, devices);
        Ok(())
    }
}

pub mod macos;
pub mod windows;

#[cfg(target_os = "macos")]
pub use macos::MacOSPlatform;
#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub type CurrentPlatform = MacOSPlatform;
#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
pub const CurrentPlatform: MacOSPlatform = MacOSPlatform;

#[cfg(target_os = "windows")]
pub use windows::WindowsPlatform;
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub type CurrentPlatform = WindowsPlatform;
#[cfg(target_os = "windows")]
#[allow(non_upper_case_globals)]
pub const CurrentPlatform: WindowsPlatform = WindowsPlatform;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use macos::MacOSPlatform as FallbackPlatform;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[allow(dead_code)]
pub type CurrentPlatform = FallbackPlatform;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[allow(non_upper_case_globals)]
pub const CurrentPlatform: FallbackPlatform = FallbackPlatform;

/// Cross-platform helper to open a directory in the OS file explorer
pub fn reveal_path(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Cross-platform helper to select/highlight a specific file in the OS file manager
pub fn reveal_file(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").args(["-R", &path.to_string_lossy()]).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").args(["/select,", &path.to_string_lossy()]).spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Cross-platform helper to open a text or log file in default editor
pub fn open_text_file(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-a").arg("TextEdit").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("notepad").arg(path).spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Cross-platform folder picker dialog
pub fn pick_folder() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("osascript")
            .args(["-e", "POSIX path of (choose folder with prompt \"Select Recordings Destination\")"])
            .output()
        {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let ps_cmd = r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = "Select Recordings Destination"
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    Write-Output $dialog.SelectedPath
}
"#;
        if let Ok(out) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", ps_cmd])
            .output()
        {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

/// Cross-platform helper to launch system audio configuration utility
pub fn open_system_audio_control() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-a").arg("Audio MIDI Setup").spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("control").arg("mmsys.cpl,,sounds").spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = std::process::Command::new("pavucontrol").spawn();
    }
}

