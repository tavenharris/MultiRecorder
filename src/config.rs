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
    /// Video/Audio source configurations
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    /// Unique identifier for this stream (e.g., "gameplay", "facecam")
    pub name: String,
    /// AVFoundation video device index or name (e.g., "0", "1", or "Capture screen 0")
    pub video_device: String,
    /// Optional AVFoundation audio device index (e.g., "0" for mic, "1" for capture card)
    pub audio_device: Option<String>,
    /// Target resolution (e.g., "3840x2160", "1920x1080", or empty for native)
    pub resolution: Option<String>,
    /// Frame rate (e.g., 60)
    pub fps: Option<u32>,
    /// Target video bitrate (e.g., "50M" for 4K, "15M" for 1080p)
    pub video_bitrate: String,
    /// Video encoder to use (e.g., "h264_videotoolbox", "hevc_videotoolbox", "prores_videotoolbox")
    pub encoder: String,
}

impl Default for Config {
    fn default() -> Self {
        let output_dir = PathBuf::from("recordings");
        let buffer_dir = PathBuf::from("/tmp").join("multirecording_buffer");

        Self {
            buffer_duration_secs: 300, // 5 minutes default
            segment_duration_secs: 2,  // 2-second segments
            output_dir,
            buffer_dir,
            sources: vec![
                SourceConfig {
                    name: "gameplay".to_string(),
                    video_device: "0".to_string(), // capture screen or capture card
                    audio_device: Some("0".to_string()),
                    resolution: Some("1920x1080".to_string()),
                    fps: Some(60),
                    video_bitrate: "40M".to_string(),
                    encoder: "h264_videotoolbox".to_string(),
                },
            ],
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
