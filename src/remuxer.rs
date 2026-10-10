use anyhow::{bail, Context, Result};
use chrono::Local;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Remuxer;

impl Remuxer {
    /// Losslessly stitches segmented MP4 files into a final MP4 file via FFmpeg concat demuxer.
    pub fn stitch_segments(
        segments: &[PathBuf],
        output_file: &Path,
    ) -> Result<()> {
        if segments.is_empty() {
            bail!("No segments provided to remux");
        }

        if let Some(parent) = output_file.parent() {
            fs::create_dir_all(parent)?;
        }

        // Create temporary concat list file
        let temp_dir = std::env::temp_dir();
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let list_file = temp_dir.join(format!("multirec_concat_{}.txt", unique_id));

        let mut manifest_content = String::new();
        for seg in segments {
            // FFmpeg concat demuxer expects single-quote escaped paths
            let canonical_str = seg.to_string_lossy().replace('\'', "'\\''");
            manifest_content.push_str(&format!("file '{}'\n", canonical_str));
        }

        fs::write(&list_file, &manifest_content)
            .with_context(|| format!("Failed to write concat manifest to {:?}", list_file))?;

        tracing::info!(
            "Remuxing {} segments into {:?} (lossless stream copy)...",
            segments.len(),
            output_file
        );

        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                &list_file.to_string_lossy(),
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                &output_file.to_string_lossy(),
            ])
            .output()
            .context("Failed to run ffmpeg concat remuxer")?;

        let _ = fs::remove_file(&list_file);

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            bail!("FFmpeg remuxing failed: {}", stderr);
        }

        tracing::info!("Successfully exported {:?}", output_file);
        Ok(())
    }

    /// Generates standard session timestamp for output files
    pub fn generate_session_timestamp() -> String {
        Local::now().format("%Y%m%d_%H%M%S").to_string()
    }
}
