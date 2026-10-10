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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn test_config_roundtrip() {
        let mut cfg = Config::default();
        cfg.output_format = "mkv".to_string();
        cfg.audio_bitrate = "256k".to_string();
        cfg.sync_audio_to_secondary = false;
        cfg.embed_timecode = true;
        cfg.sources[0].resolution = Some("3840x2160".to_string());
        cfg.sources[0].fps = Some(60);

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.output_format, "mkv");
        assert_eq!(loaded.audio_bitrate, "256k");
        assert!(!loaded.sync_audio_to_secondary);
        assert!(loaded.embed_timecode);
        assert_eq!(loaded.sources[0].resolution, Some("3840x2160".to_string()));
    }
}
