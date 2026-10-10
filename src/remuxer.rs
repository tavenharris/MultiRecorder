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

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &list_file.to_string_lossy(),
            "-c",
            "copy",
        ]);

        let is_mkv = output_file
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("mkv"))
            .unwrap_or(false);

        if !is_mkv {
            cmd.args(["-movflags", "+faststart"]);
        }

        cmd.arg(output_file);

        let status = cmd.output().context("Failed to run ffmpeg concat remuxer")?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_timestamp_format() {
        let ts = Remuxer::generate_session_timestamp();
        assert_eq!(ts.len(), 15); // YYYYMMDD_HHMMSS
        assert_eq!(&ts[8..9], "_");
        // Verify digits
        assert!(ts.chars().filter(|c| c.is_ascii_digit()).count() == 14);
    }

    #[test]
    fn test_stitch_empty_segments_fails() {
        let dummy_out = PathBuf::from("/tmp/dummy_test_out.mp4");
        let result = Remuxer::stitch_segments(&[], &dummy_out);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No segments provided"));
    }
}
