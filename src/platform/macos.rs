#![allow(dead_code)]

use super::CapturePlatform;
use crate::config::AudioTrackConfig;
use crate::devices::{Device, DeviceType};
use anyhow::{Context, Result};
use regex::Regex;
use std::process::Command;

#[derive(Debug, Default, Clone, Copy)]
pub struct MacOSPlatform;

impl MacOSPlatform {
    pub fn discover_devices(&self) -> Result<Vec<Device>> {
        let mut devices = Vec::new();

        // 1. Discover Video inputs & Audio inputs via AVFoundation
        let output = Command::new("ffmpeg")
            .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
            .context("Failed to execute ffmpeg. Ensure ffmpeg is installed.")?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut in_devices = parse_avfoundation_devices(&stderr)?;
        let display_res = self.primary_display_resolution();
        for dev in &mut in_devices {
            if dev.device_type == DeviceType::Video && dev.name.to_lowercase().contains("screen") {
                if let Some(r) = &display_res {
                    dev.resolution = Some(r.clone());
                }
            }
        }
        devices.append(&mut in_devices);

        let loopback_dev = devices
            .iter()
            .find(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
            .cloned();
        let loopback_idx = loopback_dev.as_ref().and_then(|d| d.av_index);

        // 2. Discover Audio Outputs via CoreAudio (system_profiler)
        if let Ok(mut out_devices) = list_output_devices(loopback_idx) {
            devices.append(&mut out_devices);
        }

        Ok(devices)
    }

    pub fn primary_display_resolution(&self) -> Option<String> {
        let output = Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-json"])
            .output()
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        let displays = json.get("SPDisplaysDataType")?.as_array()?;
        for d in displays {
            if let Some(ndrvs) = d.get("spdisplays_ndrvs").and_then(|v| v.as_array()) {
                for screen in ndrvs {
                    if let Some(pixels) = screen.get("_spdisplays_pixels").and_then(|v| v.as_str()) {
                        let clean = pixels.replace(' ', "");
                        return Some(clean);
                    }
                }
            }
        }
        None
    }

    pub fn build_preview_input_args(&self, video_device: &str) -> Vec<String> {
        let is_screen = video_device.to_lowercase().contains("screen");
        let framerate = if is_screen { "60" } else { "30" };
        vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-framerate".to_string(),
            framerate.to_string(),
            "-i".to_string(),
            format!("{}:none", video_device),
        ]
    }

    pub fn build_recording_input_args(
        &self,
        video_device: &str,
        in_fps: u32,
        enabled_tracks: &[AudioTrackConfig],
    ) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(first_track) = enabled_tracks.first() {
            args.extend([
                "-f".to_string(),
                "avfoundation".to_string(),
                "-framerate".to_string(),
                in_fps.to_string(),
                "-i".to_string(),
                format!("{}:{}", video_device, first_track.device_index),
            ]);
        } else {
            args.extend([
                "-f".to_string(),
                "avfoundation".to_string(),
                "-framerate".to_string(),
                in_fps.to_string(),
                "-i".to_string(),
                format!("{}:none", video_device),
            ]);
        }

        for track in enabled_tracks.iter().skip(1) {
            args.extend([
                "-f".to_string(),
                "avfoundation".to_string(),
                "-i".to_string(),
                format!("none:{}", track.device_index),
            ]);
        }

        args
    }

    pub fn build_audio_monitor_args(&self, audio_device_identifier: &str) -> Vec<String> {
        let idx = audio_device_identifier
            .strip_prefix("in:")
            .unwrap_or(audio_device_identifier);
        let input = if idx.contains(':') {
            idx.to_string()
        } else {
            format!("none:{}", idx)
        };
        vec![
            "-f".to_string(),
            "avfoundation".to_string(),
            "-i".to_string(),
            input,
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

    pub fn default_video_encoder(&self) -> &'static str {
        "h264_videotoolbox"
    }

    pub fn has_loopback_support(&self, devices: &[Device]) -> bool {
        devices
            .iter()
            .any(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
    }

    pub fn audio_input_index(&self, track_idx: usize) -> usize {
        track_idx
    }

    pub fn available_encoders(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("h264_videotoolbox", "Apple H.264 (Hardware)"),
            ("hevc_videotoolbox", "Apple HEVC / H.265"),
            ("libx264", "libx264 (CPU)"),
        ]
    }
}

impl CapturePlatform for MacOSPlatform {
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

pub fn parse_avfoundation_devices(output: &str) -> Result<Vec<Device>> {
    let mut devices = Vec::new();
    let mut current_type: Option<DeviceType> = None;

    let video_header_re = Regex::new(r"AVFoundation video devices:")?;
    let audio_header_re = Regex::new(r"AVFoundation audio devices:")?;
    let device_re = Regex::new(r"\[(\d+)\]\s+(.+)$")?;

    for line in output.lines() {
        let line = line.trim();
        if video_header_re.is_match(line) {
            current_type = Some(DeviceType::Video);
            continue;
        } else if audio_header_re.is_match(line) {
            current_type = Some(DeviceType::AudioInput);
            continue;
        }

        if let Some(ref dev_type) = current_type {
            if let Some(caps) = device_re.captures(line) {
                if let (Some(idx_match), Some(name_match)) = (caps.get(1), caps.get(2)) {
                    if let Ok(idx) = idx_match.as_str().parse::<usize>() {
                        let name = name_match.as_str().trim().to_string();
                        let (id, av_idx) = match dev_type {
                            DeviceType::Video => (format!("video:{}", idx), None),
                            DeviceType::AudioInput => (format!("in:{}", idx), Some(idx)),
                            DeviceType::AudioOutput => (format!("out:{}", idx), None),
                        };
                        let (res, fps) = match dev_type {
                            DeviceType::Video => {
                                let lower = name.to_lowercase();
                                if lower.contains("screen") {
                                    (Some("3024x1964".to_string()), Some(60))
                                } else if lower.contains("cam") || lower.contains("face") {
                                    (Some("1920x1080".to_string()), Some(30))
                                } else if lower.contains("4k") {
                                    (Some("3840x2160".to_string()), Some(60))
                                } else {
                                    (Some("1920x1080".to_string()), Some(60))
                                }
                            }
                            _ => (None, None),
                        };
                        devices.push(Device {
                            id,
                            index: idx,
                            name,
                            device_type: dev_type.clone(),
                            av_index: av_idx,
                            resolution: res,
                            fps,
                        });
                    }
                }
            }
        }
    }

    Ok(devices)
}

pub fn list_output_devices(loopback_index: Option<usize>) -> Result<Vec<Device>> {
    let output = Command::new("system_profiler")
        .args(["SPAudioDataType", "-json"])
        .output()
        .context("Failed to query macOS audio outputs")?;

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let mut outputs = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    if let Some(data_arr) = parsed.get("SPAudioDataType").and_then(|v| v.as_array()) {
        for wrapper in data_arr {
            if let Some(items) = wrapper.get("_items").and_then(|v| v.as_array()) {
                for item in items {
                    let name = item
                        .get("_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Output");
                    let out_channels = item
                        .get("coreaudio_device_output")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if out_channels > 0 && !seen_names.contains(name) {
                        seen_names.insert(name.to_string());
                        let idx = outputs.len();
                        outputs.push(Device {
                            id: format!("out:{}", name),
                            index: idx,
                            name: name.to_string(),
                            device_type: DeviceType::AudioOutput,
                            av_index: loopback_index,
                            resolution: None,
                            fps: None,
                        });
                    }
                }
            }
        }
    }

    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_preview_args() {
        let mac = MacOSPlatform;
        let args = mac.build_preview_input_args("0");
        assert_eq!(args, vec!["-f", "avfoundation", "-framerate", "30", "-i", "0:none"]);

        let screen_args = mac.build_preview_input_args("Capture screen 0");
        assert_eq!(
            screen_args,
            vec!["-f", "avfoundation", "-framerate", "60", "-i", "Capture screen 0:none"]
        );
    }

    #[test]
    fn test_macos_recording_args() {
        let mac = MacOSPlatform;
        let tracks = vec![
            AudioTrackConfig {
                enabled: true,
                name: "Mic".to_string(),
                device_index: 0,
                device_id: "in:0".to_string(),
            },
            AudioTrackConfig {
                enabled: true,
                name: "Desktop".to_string(),
                device_index: 1,
                device_id: "in:1".to_string(),
            },
        ];
        let args = mac.build_recording_input_args("0", 60, &tracks);
        assert_eq!(
            args,
            vec![
                "-f", "avfoundation", "-framerate", "60", "-i", "0:0",
                "-f", "avfoundation", "-i", "none:1"
            ]
        );
    }

    #[test]
    fn test_macos_audio_monitor_args() {
        let mac = MacOSPlatform;
        let args = mac.build_audio_monitor_args("0");
        assert_eq!(
            args,
            vec![
                "-f", "avfoundation", "-i", "none:0", "-vn", "-ac", "1", "-ar", "8000", "-f",
                "f32le", "pipe:1"
            ]
        );
    }
}
