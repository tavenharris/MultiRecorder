use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Duration of the rolling buffer in seconds (e.g. 300 for 5 minutes)
    pub buffer_duration_secs: u64,
    /// Duration of each intermediate segment in seconds (2-3 seconds recommended)
    pub segment_duration_secs: u64,
    /// Directory where saved flashback recordings are exported
    pub output_dir: PathBuf,
    /// Directory where the temporary rolling ring-buffer segments are stored
    pub buffer_dir: PathBuf,
    /// Video source configurations
    pub sources: Vec<SourceConfig>,
    /// Up to 5 discrete audio tracks to record simultaneously
    #[serde(default = "default_audio_tracks")]
    pub audio_tracks: Vec<AudioTrackConfig>,
    /// Exported container format ("mp4", "mov", "mkv")
    #[serde(default = "default_output_format")]
    pub output_format: String,
    /// Audio bitrate for recorded tracks ("320k", "256k", "192k")
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate: String,
    /// Mirror Audio Track 1 onto Video 2 recording for NLE waveform sync
    #[serde(default = "default_sync_audio_to_secondary")]
    pub sync_audio_to_secondary: bool,
    /// Embed standardized SMPTE Timecode track and metadata for NLE timecode sync
    #[serde(default = "default_embed_timecode")]
    pub embed_timecode: bool,
    /// Flashback clip intervals in seconds shown as quick save buttons in the UI
    #[serde(default = "default_clip_intervals")]
    pub clip_intervals: Vec<u64>,
}

fn default_sync_audio_to_secondary() -> bool {
    true
}

fn default_embed_timecode() -> bool {
    true
}

fn default_output_format() -> String {
    "mp4".to_string()
}

fn default_audio_bitrate() -> String {
    "320k".to_string()
}

pub fn default_clip_intervals() -> Vec<u64> {
    vec![30, 60, 300, 1200]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackConfig {
    pub enabled: bool,
    /// Custom track title (written into MP4 track metadata for Premiere Pro timeline)
    pub name: String,
    /// AVFoundation audio device index (e.g. 0, 1, 2)
    pub device_index: usize,
    /// Unique device identifier (e.g. "in:0", "out:MacBook Pro Speakers")
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    /// Unique identifier for this stream (e.g., "gameplay", "facecam")
    pub name: String,
    /// AVFoundation video device index or name (e.g., "0", "1", or "Capture screen 0")
    pub video_device: String,
    /// Target resolution (e.g., "3840x2160", "1920x1080", or None for native)
    pub resolution: Option<String>,
    /// Frame rate (e.g., 60)
    pub fps: Option<u32>,
    /// Target video bitrate (e.g., "50M" for 4K, "15M" for 1080p)
    pub video_bitrate: String,
    /// Video encoder to use (e.g., "h264_videotoolbox", "hevc_videotoolbox")
    pub encoder: String,
}

pub fn default_audio_tracks() -> Vec<AudioTrackConfig> {
    vec![
        AudioTrackConfig {
            enabled: true,
            name: "Game Audio".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
        },
        AudioTrackConfig {
            enabled: true,
            name: "Microphone".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
        },
        AudioTrackConfig {
            enabled: false,
            name: "Discord / Chat".to_string(),
            device_index: 0,
            device_id: "in:0".to_string(),
        },
        AudioTrackConfig {
            enabled: false,
            name: "Desktop / Speakers".to_string(),
            device_index: 0,
            device_id: "out:MacBook Pro Speakers".to_string(),
        },
        AudioTrackConfig {
            enabled: false,
            name: "AirPods / Aux".to_string(),
            device_index: 0,
            device_id: "out:AirPods".to_string(),
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        let output_dir = PathBuf::from("recordings");
        let buffer_dir = std::env::temp_dir().join("multirecording_buffer");

        Self {
            buffer_duration_secs: 300, // 5 minutes default
            segment_duration_secs: 2,  // 2-second segments
            output_dir,
            buffer_dir,
            sources: vec![
                SourceConfig {
                    name: "gameplay".to_string(),
                    video_device: "0".to_string(),
                    resolution: Some("1920x1080".to_string()),
                    fps: Some(60),
                    video_bitrate: "40M".to_string(),
                    encoder: crate::platform::CurrentPlatform.default_video_encoder().to_string(),
                },
                SourceConfig {
                    name: "facecam".to_string(),
                    video_device: "0".to_string(),
                    resolution: Some("1920x1080".to_string()),
                    fps: Some(60),
                    video_bitrate: "15M".to_string(),
                    encoder: crate::platform::CurrentPlatform.default_video_encoder().to_string(),
                },
            ],
            audio_tracks: default_audio_tracks(),
            output_format: "mp4".to_string(),
            audio_bitrate: "320k".to_string(),
            sync_audio_to_secondary: true,
            embed_timecode: true,
            clip_intervals: default_clip_intervals(),
        }
    }
}

impl Config {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let contents = fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read config file at {:?}", path.as_ref()))?;
        let config: Config = toml::from_str(&contents)
            .with_context(|| "Failed to parse config file format")?;
        Ok(config)
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let serialized = toml::to_string_pretty(self)
            .with_context(|| "Failed to serialize config to TOML")?;
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path.as_ref(), serialized)?;
        Ok(())
    }

    /// Validates and heals the configuration against the current OS platform and discovered hardware devices on launch.
    /// Checks:
    /// 1. OS path compatibility (`buffer_dir`)
    /// 2. Video encoder support on current platform
    /// 3. Video source device existence & role mapping (Screen/Desktop for Stream 1, Camera for Stream 2)
    /// 4. Framerate & resolution compatibility against hardware capabilities
    /// 5. Audio input/output device existence, AVFoundation index changes, loopback routing, and deduplication
    /// Returns a list of human-readable adjustments made.
    pub fn sanitize_for_platform(&mut self, devices: &[crate::devices::Device]) -> Vec<String> {
        use crate::devices::{Device, DeviceType};

        let mut fixes = Vec::new();

        // 1. Check buffer_dir for cross-OS path incompatibility
        let buf_str = self.buffer_dir.to_string_lossy().to_string();
        let invalid_buffer_path = if cfg!(target_os = "windows") {
            buf_str.starts_with("/tmp") || buf_str.starts_with("/var")
        } else {
            buf_str.contains('\\') || buf_str.contains(':')
        };
        if invalid_buffer_path || buf_str.trim().is_empty() {
            let new_buf = std::env::temp_dir().join("multirecording_buffer");
            fixes.push(format!("buffer_dir {:?} -> {:?}", self.buffer_dir, new_buf));
            self.buffer_dir = new_buf;
        }

        if self.segment_duration_secs == 0 || self.segment_duration_secs > 15 {
            self.segment_duration_secs = 2;
        }
        if self.clip_intervals.is_empty() {
            self.clip_intervals = default_clip_intervals();
        }

        // 2. Ensure minimum 2 video sources and 5 audio tracks exist
        let valid_encoders: Vec<&str> = crate::platform::CurrentPlatform
            .available_encoders()
            .iter()
            .map(|(enc, _)| *enc)
            .collect();
        let default_enc = crate::platform::CurrentPlatform.default_video_encoder();

        while self.sources.len() < 2 {
            let idx = self.sources.len();
            let name = if idx == 0 { "gameplay" } else { "facecam" };
            self.sources.push(SourceConfig {
                name: name.to_string(),
                video_device: "0".to_string(),
                resolution: Some("1920x1080".to_string()),
                fps: Some(60),
                video_bitrate: if idx == 0 { "40M".to_string() } else { "15M".to_string() },
                encoder: default_enc.to_string(),
            });
        }

        while self.audio_tracks.len() < 5 {
            let idx = self.audio_tracks.len() + 1;
            self.audio_tracks.push(AudioTrackConfig {
                enabled: false,
                name: format!("Track {}", idx),
                device_index: 0,
                device_id: "none".to_string(),
            });
        }

        // Detect whether config was previously saved on a different OS
        let mut switched_os = false;
        for src in &self.sources {
            if !valid_encoders.contains(&src.encoder.as_str()) {
                switched_os = true;
            }
            if cfg!(target_os = "macos") && src.video_device.eq_ignore_ascii_case("desktop") {
                switched_os = true;
            }
        }

        // 3. Validate encoders
        for src in &mut self.sources {
            if !valid_encoders.contains(&src.encoder.as_str()) {
                fixes.push(format!("source [{}] encoder '{}' -> '{}'", src.name, src.encoder, default_enc));
                src.encoder = default_enc.to_string();
            }
        }

        // 4. Validate video sources & framerates against discovered video devices
        let video_devs: Vec<&Device> = devices
            .iter()
            .filter(|d| d.device_type == DeviceType::Video)
            .collect();

        for (i, src) in self.sources.iter_mut().enumerate() {
            // Validate resolution string if present
            if let Some(res) = &src.resolution {
                let valid_res = res
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                    .map(|(w, h)| w >= 160 && h >= 120)
                    .unwrap_or(false);
                if !valid_res {
                    fixes.push(format!("source [{}] invalid resolution '{}' -> '1920x1080'", src.name, res));
                    src.resolution = Some("1920x1080".to_string());
                }
            }

            if !video_devs.is_empty() {
                let clean = src.video_device.strip_prefix("video:").unwrap_or(&src.video_device).trim();
                let wants_screen = i == 0
                    || clean.eq_ignore_ascii_case("desktop")
                    || clean.to_lowercase().contains("screen")
                    || src.name.to_lowercase().contains("game");

                // Match existing device (unless we just switched OSes and had a generic numeric ID that reversed screen/camera order)
                let matched_video = if !switched_os {
                    video_devs
                        .iter()
                        .find(|d| d.id == src.video_device || d.id == clean || d.name.eq_ignore_ascii_case(clean))
                        .copied()
                        .or_else(|| {
                            clean
                                .parse::<usize>()
                                .ok()
                                .and_then(|num| video_devs.iter().find(|d| d.index == num).copied())
                        })
                } else {
                    // When switching OSes, only match if the device name explicitly matches
                    video_devs
                        .iter()
                        .find(|d| !clean.chars().all(|c| c.is_ascii_digit()) && (d.id.eq_ignore_ascii_case(clean) || d.name.eq_ignore_ascii_case(clean)))
                        .copied()
                };

                let chosen_dev = matched_video.or_else(|| {
                    if wants_screen {
                        video_devs
                            .iter()
                            .find(|d| d.name.to_lowercase().contains("screen") || d.id.eq_ignore_ascii_case("desktop"))
                            .copied()
                    } else {
                        video_devs
                            .iter()
                            .find(|d| !d.name.to_lowercase().contains("screen") && !d.id.eq_ignore_ascii_case("desktop"))
                            .copied()
                    }
                    .or_else(|| video_devs.get(i).copied())
                    .or_else(|| video_devs.first().copied())
                });

                if let Some(dev) = chosen_dev {
                    if src.video_device != dev.id {
                        fixes.push(format!(
                            "source [{}] video_device '{}' -> '{}' ({})",
                            src.name, src.video_device, dev.id, dev.name
                        ));
                        src.video_device = dev.id.clone();
                    }

                    let is_screen_dev = dev.name.to_lowercase().contains("screen")
                        || dev.id.eq_ignore_ascii_case("desktop");
                    let hw_max_fps = if is_screen_dev {
                        60
                    } else {
                        dev.fps.unwrap_or(30)
                    };

                    let cur_fps = src.fps.unwrap_or(hw_max_fps);
                    let target_fps = if cur_fps < 24 {
                        hw_max_fps
                    } else {
                        cur_fps.min(hw_max_fps)
                    };
                    // On macOS AVFoundation, input negotiation requires 30 or 60
                    let target_fps = if cfg!(target_os = "macos") {
                        if target_fps >= 60 && hw_max_fps >= 60 {
                            60
                        } else {
                            30
                        }
                    } else {
                        target_fps
                    };

                    if src.fps != Some(target_fps) {
                        fixes.push(format!(
                            "source [{}] fps {:?} -> {} (device '{}')",
                            src.name, src.fps, target_fps, dev.name
                        ));
                        src.fps = Some(target_fps);
                    }
                }
            } else {
                // Even without discovered devices, heal any invalid <24fps value
                if let Some(cur_fps) = src.fps {
                    if cur_fps < 24 {
                        fixes.push(format!("source [{}] fps {} -> 60", src.name, cur_fps));
                        src.fps = Some(60);
                    }
                }
            }
        }

        // 5. Validate audio inputs & outputs
        let audio_inputs: Vec<&Device> = devices
            .iter()
            .filter(|d| d.device_type == DeviceType::AudioInput)
            .collect();
        let audio_outputs: Vec<&Device> = devices
            .iter()
            .filter(|d| d.device_type == DeviceType::AudioOutput)
            .collect();

        if !audio_inputs.is_empty() || !audio_outputs.is_empty() {
            let mut used_ids: Vec<String> = Vec::new();

            for (i, track) in self.audio_tracks.iter_mut().enumerate() {
                if !track.enabled || track.device_id == "none" {
                    track.device_id = "none".to_string();
                    continue;
                }

                if track.device_id.is_empty() {
                    track.device_id = format!("in:{}", track.device_index);
                }

                let clean_id = track
                    .device_id
                    .strip_prefix("out:")
                    .or_else(|| track.device_id.strip_prefix("in:"))
                    .or_else(|| track.device_id.strip_prefix("audio="))
                    .unwrap_or(&track.device_id)
                    .trim();

                let track_lower = track.name.to_lowercase();
                let id_lower = track.device_id.to_lowercase();
                let looks_like_desktop_or_output = track.device_id.starts_with("out:")
                    || id_lower == "wasapi"
                    || id_lower == "default"
                    || track_lower.contains("speaker")
                    || track_lower.contains("headphone")
                    || track_lower.contains("desktop")
                    || track_lower.contains("blackhole")
                    || track_lower.contains("game audio")
                    || track_lower.contains("loopback");

                // Priority 1: Match by exact hardware name on the current platform
                // (Crucial on macOS where AVFoundation renumbers in:0 / in:1 when AirPods connect/disconnect)
                let mut resolved: Option<&Device> = audio_inputs
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(&track.name) || (!clean_id.is_empty() && d.name.eq_ignore_ascii_case(clean_id)))
                    .copied()
                    .or_else(|| {
                        if cfg!(target_os = "windows") {
                            audio_outputs
                                .iter()
                                .find(|d| d.name.eq_ignore_ascii_case(&track.name) || (!clean_id.is_empty() && d.name.eq_ignore_ascii_case(clean_id)))
                                .copied()
                        } else {
                            None
                        }
                    });

                // Priority 2: Match by device_id
                if resolved.is_none() {
                    if cfg!(target_os = "macos") {
                        // On macOS, AVFoundation only captures from AudioInput devices (or BlackHole for AudioOutput)
                        resolved = audio_inputs
                            .iter()
                            .find(|d| d.id == track.device_id || (!clean_id.is_empty() && d.id == clean_id))
                            .copied();
                        if resolved.is_none() && looks_like_desktop_or_output {
                            resolved = audio_inputs.iter().find(|d| d.is_loopback()).copied();
                        }
                    } else {
                        resolved = devices
                            .iter()
                            .find(|d| {
                                (d.device_type == DeviceType::AudioInput || d.device_type == DeviceType::AudioOutput)
                                    && (d.id == track.device_id || (!clean_id.is_empty() && d.id == clean_id))
                            })
                            .or_else(|| {
                                if looks_like_desktop_or_output {
                                    audio_outputs.first().copied()
                                } else {
                                    None
                                }
                            });
                    }
                }

                // Priority 3: Cross-platform / disconnected hardware fallback (pick an unused device)
                if resolved.is_none() {
                    if cfg!(target_os = "windows") && looks_like_desktop_or_output {
                        resolved = audio_outputs
                            .iter()
                            .find(|d| !used_ids.contains(&d.id))
                            .copied();
                    } else if cfg!(target_os = "macos") && looks_like_desktop_or_output {
                        resolved = audio_inputs
                            .iter()
                            .find(|d| d.is_loopback() && !used_ids.contains(&d.id))
                            .copied();
                    }

                    if resolved.is_none() {
                        resolved = audio_inputs
                            .iter()
                            .find(|d| !used_ids.contains(&d.id))
                            .copied();
                    }
                }

                // Deduplicate: if this device is already used by an earlier track, try finding an unused input or disable
                if let Some(dev) = resolved {
                    let final_dev = if used_ids.contains(&dev.id) {
                        audio_inputs
                            .iter()
                            .find(|d| !used_ids.contains(&d.id))
                            .copied()
                    } else {
                        Some(dev)
                    };

                    if let Some(chosen) = final_dev {
                        let new_idx = chosen.av_index.unwrap_or(chosen.index);
                        if track.device_id != chosen.id || track.device_index != new_idx || track.name != chosen.name {
                            fixes.push(format!(
                                "audio track {} ('{}') -> '{}' (id: {}, idx: {})",
                                i + 1,
                                track.name,
                                chosen.name,
                                chosen.id,
                                new_idx
                            ));
                        }
                        track.device_id = chosen.id.clone();
                        track.name = chosen.name.clone();
                        track.device_index = new_idx;
                        used_ids.push(chosen.id.clone());
                    } else {
                        fixes.push(format!(
                            "audio track {} ('{}') disabled (no additional unused audio devices available)",
                            i + 1,
                            track.name
                        ));
                        track.enabled = false;
                        track.device_id = "none".to_string();
                        track.name = format!("Track {}", i + 1);
                    }
                } else {
                    fixes.push(format!(
                        "audio track {} ('{}') disabled (device not found on this system)",
                        i + 1,
                        track.name
                    ));
                    track.enabled = false;
                    track.device_id = "none".to_string();
                    track.name = format!("Track {}", i + 1);
                }
            }
        }

        fixes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{Device, DeviceType};

    #[test]
    fn test_config_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.output_format, "mp4");
        assert_eq!(cfg.audio_bitrate, "320k");
        assert!(cfg.sync_audio_to_secondary);
        assert!(cfg.embed_timecode);
        assert_eq!(cfg.sources.len(), 2);
        assert_eq!(cfg.audio_tracks.len(), 5);
        assert_eq!(cfg.sources[0].resolution, Some("1920x1080".to_string()));
        assert_eq!(cfg.sources[0].fps, Some(60));
        assert_eq!(cfg.clip_intervals, vec![30, 60, 300, 1200]);
    }

    #[test]
    fn test_config_roundtrip() {
        let mut cfg = Config::default();
        cfg.output_format = "mkv".to_string();
        cfg.audio_bitrate = "256k".to_string();
        cfg.sync_audio_to_secondary = false;
        cfg.embed_timecode = true;
        cfg.clip_intervals = vec![15, 60, 180];
        cfg.sources[0].resolution = Some("3840x2160".to_string());
        cfg.sources[0].fps = Some(60);

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.output_format, "mkv");
        assert_eq!(loaded.audio_bitrate, "256k");
        assert!(!loaded.sync_audio_to_secondary);
        assert!(loaded.embed_timecode);
        assert_eq!(loaded.clip_intervals, vec![15, 60, 180]);
        assert_eq!(loaded.sources[0].resolution, Some("3840x2160".to_string()));
    }

    #[test]
    fn test_sanitize_for_platform_heals_cross_os_and_fps() {
        let mut cfg = Config::default();
        // Simulate a Windows config loaded on macOS (or vice versa) with 15fps and foreign devices
        cfg.sources[0].video_device = "desktop".to_string();
        cfg.sources[0].fps = Some(15);
        cfg.sources[0].encoder = "invalid_foreign_encoder".to_string();
        cfg.sources[1].video_device = "OBS Virtual Camera".to_string();
        cfg.sources[1].fps = Some(60);
        cfg.sources[1].encoder = "invalid_foreign_encoder".to_string();

        cfg.audio_tracks[0].enabled = true;
        cfg.audio_tracks[0].name = "Speakers (Razer BlackShark V2 X USB)".to_string();
        cfg.audio_tracks[0].device_id = "Speakers (Razer BlackShark V2 X USB)".to_string();

        cfg.audio_tracks[1].enabled = true;
        cfg.audio_tracks[1].name = "Microphone (Razer BlackShark V2 X USB)".to_string();
        cfg.audio_tracks[1].device_id = "Microphone (Razer BlackShark V2 X USB)".to_string();

        let mock_devices = vec![
            Device {
                index: 0,
                id: "0".to_string(),
                name: "FaceTime HD Camera".to_string(),
                device_type: DeviceType::Video,
                av_index: Some(0),
                resolution: Some("1920x1080".to_string()),
                fps: Some(30),
            },
            Device {
                index: 1,
                id: "1".to_string(),
                name: "Capture screen 0".to_string(),
                device_type: DeviceType::Video,
                av_index: Some(1),
                resolution: Some("3024x1964".to_string()),
                fps: Some(60),
            },
            Device {
                index: 0,
                id: "in:0".to_string(),
                name: "BlackHole 2ch".to_string(),
                device_type: DeviceType::AudioInput,
                av_index: Some(0),
                resolution: None,
                fps: None,
            },
            Device {
                index: 1,
                id: "in:1".to_string(),
                name: "MacBook Pro Microphone".to_string(),
                device_type: DeviceType::AudioInput,
                av_index: Some(1),
                resolution: None,
                fps: None,
            },
        ];

        let fixes = cfg.sanitize_for_platform(&mock_devices);
        assert!(!fixes.is_empty());

        // Stream 1 ("gameplay") mapped to screen ("1") at 60fps
        assert_eq!(cfg.sources[0].video_device, "1");
        assert_eq!(cfg.sources[0].fps, Some(60));
        assert_eq!(cfg.sources[0].encoder, crate::platform::CurrentPlatform.default_video_encoder());

        // Stream 2 ("facecam") mapped to camera ("0") and clamped to 30fps
        assert_eq!(cfg.sources[1].video_device, "0");
        assert_eq!(cfg.sources[1].fps, Some(30));

        // Audio Track 1 & 2 mapped to distinct valid devices
        assert!(cfg.audio_tracks[0].enabled);
        assert!(cfg.audio_tracks[1].enabled);
        assert_ne!(cfg.audio_tracks[0].device_id, cfg.audio_tracks[1].device_id);
    }
}
