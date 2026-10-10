use anyhow::{Context, Result};
use regex::Regex;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceType {
    Video,
    AudioInput,
    AudioOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub device_type: DeviceType,
    pub av_index: Option<usize>,
}

impl Device {
    pub fn is_loopback(&self) -> bool {
        let lower = self.name.to_lowercase();
        lower.contains("blackhole")
            || lower.contains("loopback")
            || lower.contains("soundflower")
            || lower.contains("multi-output")
            || lower.contains("aggregate")
    }

    pub fn display_label(&self, has_loopback: bool) -> String {
        match self.device_type {
            DeviceType::Video => format!("[Video {}] {}", self.index, self.name),
            DeviceType::AudioInput => {
                if self.is_loopback() {
                    format!("🔄 [Loopback] {}", self.name)
                } else {
                    format!("🎙 [Mic {}] {}", self.index, self.name)
                }
            }
            DeviceType::AudioOutput => {
                if has_loopback {
                    format!("🔊 [Output] {} (via Loopback)", self.name)
                } else {
                    format!("🔊 [Output] {} ⚠️ (Needs Loopback)", self.name)
                }
            }
        }
    }
}

pub struct DeviceManager;

impl DeviceManager {
    pub fn has_loopback_device(devices: &[Device]) -> bool {
        devices.iter().any(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
    }

    pub fn find_loopback_device(devices: &[Device]) -> Option<&Device> {
        devices.iter().find(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
    }

    /// Discovers AVFoundation video/audio inputs and CoreAudio audio outputs
    pub fn list_devices() -> Result<Vec<Device>> {
        let mut devices = Vec::new();

        // 1. Discover Video inputs & Audio inputs via AVFoundation
        let output = Command::new("ffmpeg")
            .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
            .context("Failed to execute ffmpeg. Ensure ffmpeg is installed.")?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut in_devices = Self::parse_avfoundation_devices(&stderr)?;
        devices.append(&mut in_devices);

        let loopback_dev = Self::find_loopback_device(&devices).cloned();
        let loopback_idx = loopback_dev.as_ref().and_then(|d| d.av_index);

        // 2. Discover Audio Outputs via CoreAudio (system_profiler)
        if let Ok(mut out_devices) = Self::list_output_devices(loopback_idx) {
            devices.append(&mut out_devices);
        }

        Ok(devices)
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
                            devices.push(Device {
                                id,
                                index: idx,
                                name,
                                device_type: dev_type.clone(),
                                av_index: av_idx,
                            });
                        }
                    }
                }
            }
        }

        Ok(devices)
    }

    /// Discovers macOS CoreAudio output devices (Speakers, AirPods, Headphones)
    pub fn list_output_devices(loopback_index: Option<usize>) -> Result<Vec<Device>> {
        let output = Command::new("system_profiler")
            .args(["SPAudioDataType", "-json"])
            .output()
            .context("Failed to query macOS audio outputs")?;

        let json_str = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&json_str)?;

        let mut outputs = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        if let Some(data_arr) = parsed.get("SPAudioDataType").and_then(|v| v.as_array()) {
            for wrapper in data_arr {
                if let Some(items) = wrapper.get("_items").and_then(|v| v.as_array()) {
                    for item in items {
                        let name = item.get("_name").and_then(|v| v.as_str()).unwrap_or("Unknown Output");
                        let out_channels = item.get("coreaudio_device_output").and_then(|v| v.as_u64()).unwrap_or(0);
                        if out_channels > 0 && !seen_names.contains(name) {
                            seen_names.insert(name.to_string());
                            let idx = outputs.len();
                            outputs.push(Device {
                                id: format!("out:{}", name),
                                index: idx,
                                name: name.to_string(),
                                device_type: DeviceType::AudioOutput,
                                av_index: loopback_index,
                            });
                        }
                    }
                }
            }
        }

        Ok(outputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_parsing() {
        let sample = r#"
[AVFoundation indev @ 0x76f7020140] AVFoundation video devices:
[AVFoundation indev @ 0x76f7020140] [0] Capture screen 0
[AVFoundation indev @ 0x76f7020140] [1] Elgato 4K X
[AVFoundation indev @ 0x76f7020140] AVFoundation audio devices:
[AVFoundation indev @ 0x76f7020140] [0] MacBook Pro Microphone
[AVFoundation indev @ 0x76f7020140] [1] Elgato HDMI Audio
"#;
        let devices = DeviceManager::parse_avfoundation_devices(sample).unwrap();
        assert_eq!(devices.len(), 4);
        assert_eq!(devices[0].index, 0);
        assert_eq!(devices[0].name, "Capture screen 0");
        assert_eq!(devices[0].device_type, DeviceType::Video);

        assert_eq!(devices[2].index, 0);
        assert_eq!(devices[2].name, "MacBook Pro Microphone");
        assert_eq!(devices[2].device_type, DeviceType::AudioInput);
    }
}
