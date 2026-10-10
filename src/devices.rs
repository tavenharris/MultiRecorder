use anyhow::{Context, Result};
use regex::Regex;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceType {
    Video,
    Audio,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub index: usize,
    pub name: String,
    pub device_type: DeviceType,
}

pub struct DeviceManager;

impl DeviceManager {
    /// Discovers AVFoundation video and audio devices using ffmpeg
    pub fn list_devices() -> Result<Vec<Device>> {
        let output = Command::new("ffmpeg")
            .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
            .context("Failed to execute ffmpeg. Ensure ffmpeg is installed.")?;

        // ffmpeg outputs the device listing on stderr
        let stderr = String::from_utf8_lossy(&output.stderr);
        Self::parse_devices(&stderr)
    }

    pub fn parse_devices(output: &str) -> Result<Vec<Device>> {
        let mut devices = Vec::new();
        let mut current_type: Option<DeviceType> = None;

        let video_header_re = Regex::new(r"AVFoundation video devices:")?;
        let audio_header_re = Regex::new(r"AVFoundation audio devices:")?;
        // Matches e.g. `[AVFoundation indev @ 0x...] [0] FaceTime HD Camera` or `[0] Capture screen 0`
        let device_re = Regex::new(r"\[(\d+)\]\s+(.+)$")?;

        for line in output.lines() {
            let line = line.trim();
            if video_header_re.is_match(line) {
                current_type = Some(DeviceType::Video);
                continue;
            } else if audio_header_re.is_match(line) {
                current_type = Some(DeviceType::Audio);
                continue;
            }

            if let Some(ref dev_type) = current_type {
                if let Some(caps) = device_re.captures(line) {
                    if let (Some(idx_match), Some(name_match)) = (caps.get(1), caps.get(2)) {
                        if let Ok(idx) = idx_match.as_str().parse::<usize>() {
                            let name = name_match.as_str().trim().to_string();
                            devices.push(Device {
                                index: idx,
                                name,
                                device_type: dev_type.clone(),
                            });
                        }
                    }
                }
            }
        }

        Ok(devices)
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
        let devices = DeviceManager::parse_devices(sample).unwrap();
        assert_eq!(devices.len(), 4);
        assert_eq!(devices[0].index, 0);
        assert_eq!(devices[0].name, "Capture screen 0");
        assert_eq!(devices[0].device_type, DeviceType::Video);

        assert_eq!(devices[1].index, 1);
        assert_eq!(devices[1].name, "Elgato 4K X");
        assert_eq!(devices[1].device_type, DeviceType::Video);

        assert_eq!(devices[2].index, 0);
        assert_eq!(devices[2].name, "MacBook Pro Microphone");
        assert_eq!(devices[2].device_type, DeviceType::Audio);

        assert_eq!(devices[3].index, 1);
        assert_eq!(devices[3].name, "Elgato HDMI Audio");
        assert_eq!(devices[3].device_type, DeviceType::Audio);
    }
}
