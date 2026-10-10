use anyhow::Result;

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
    pub resolution: Option<String>,
    pub fps: Option<u32>,
}

impl Device {
    pub fn is_loopback(&self) -> bool {
        let lower = self.name.to_lowercase();
        lower.contains("blackhole")
            || lower.contains("loopback")
            || lower.contains("soundflower")
            || lower.contains("multi-output")
            || lower.contains("aggregate")
            || lower.contains("wasapi")
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
        crate::platform::CurrentPlatform.has_loopback_support(devices)
    }

    #[allow(dead_code)]
    pub fn find_loopback_device(devices: &[Device]) -> Option<&Device> {
        devices.iter().find(|d| d.device_type == DeviceType::AudioInput && d.is_loopback())
    }

    /// Discovers video and audio devices via the current platform HAL
    pub fn list_devices() -> Result<Vec<Device>> {
        crate::platform::CurrentPlatform.discover_devices()
    }

    #[allow(dead_code)]
    pub fn query_primary_display_resolution() -> Option<String> {
        crate::platform::CurrentPlatform.primary_display_resolution()
    }

    #[allow(dead_code)]
    pub fn parse_avfoundation_devices(output: &str) -> Result<Vec<Device>> {
        crate::platform::macos::parse_avfoundation_devices(output)
    }

    #[allow(dead_code)]
    pub fn list_output_devices(loopback_index: Option<usize>) -> Result<Vec<Device>> {
        crate::platform::macos::list_output_devices(loopback_index)
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

    #[test]
    fn test_loopback_detection_and_labels() {
        let loopback = Device {
            id: "in:2".to_string(),
            index: 2,
            name: "BlackHole 2ch".to_string(),
            device_type: DeviceType::AudioInput,
            av_index: Some(2),
            resolution: None,
            fps: None,
        };
        assert!(loopback.is_loopback());
        assert_eq!(loopback.display_label(true), "🔄 [Loopback] BlackHole 2ch");

        let mic = Device {
            id: "in:0".to_string(),
            index: 0,
            name: "Internal Mic".to_string(),
            device_type: DeviceType::AudioInput,
            av_index: Some(0),
            resolution: None,
            fps: None,
        };
        assert!(!mic.is_loopback());
        assert_eq!(mic.display_label(false), "🎙 [Mic 0] Internal Mic");

        let output = Device {
            id: "out:MacBook Pro Speakers".to_string(),
            index: 0,
            name: "MacBook Pro Speakers".to_string(),
            device_type: DeviceType::AudioOutput,
            av_index: None,
            resolution: None,
            fps: None,
        };
        assert_eq!(output.display_label(false), "🔊 [Output] MacBook Pro Speakers ⚠️ (Needs Loopback)");
        assert_eq!(output.display_label(true), "🔊 [Output] MacBook Pro Speakers (via Loopback)");

        let list = vec![mic, loopback.clone()];
        assert!(DeviceManager::has_loopback_device(&list));
        assert_eq!(DeviceManager::find_loopback_device(&list), Some(&loopback));
    }
}
