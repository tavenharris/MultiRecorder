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
        timecode: Option<&str>,
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
            let seg_str = seg.to_string_lossy();
            if seg_str.contains('\n') || seg_str.contains('\r') {
                bail!("Security violation: segment path contains invalid newline character: {:?}", seg);
            }
            // FFmpeg concat demuxer expects single-quote escaped paths
            let canonical_str = seg_str.replace('\'', "'\\''");
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

        if let Some(tc) = timecode {
            if !is_mkv {
                // MP4 and MOV support dedicated tmcd timecode track + metadata
                cmd.args(["-timecode", tc, "-metadata", &format!("timecode={}", tc)]);
            } else {
                cmd.args(["-metadata", &format!("timecode={}", tc)]);
            }
        }

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

    /// Extracts a standard SMPTE timecode (HH:MM:SS:00) from the first segment's filename or current time
    pub fn extract_start_timecode_from_segments(segments: &[PathBuf]) -> String {
        if let Some(first) = segments.first() {
            let name = first.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
            if let Some(pos) = name.rfind('_') {
                let time_part = &name[pos + 1..];
                if time_part.len() >= 6 && time_part.chars().all(|c| c.is_ascii_digit()) {
                    let hh = &time_part[0..2];
                    let mm = &time_part[2..4];
                    let ss = &time_part[4..6];
                    return format!("{}:{}:{}:00", hh, mm, ss);
                }
            }
        }
        Local::now().format("%H:%M:%S:00").to_string()
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
    fn test_extract_timecode_from_segments() {
        let segs = vec![
            PathBuf::from("/tmp/gameplay_20261010_143215.mp4"),
            PathBuf::from("/tmp/gameplay_20261010_143217.mp4"),
        ];
        let tc = Remuxer::extract_start_timecode_from_segments(&segs);
        assert_eq!(tc, "14:32:15:00");

        let empty: Vec<PathBuf> = vec![];
        let fallback_tc = Remuxer::extract_start_timecode_from_segments(&empty);
        assert_eq!(fallback_tc.len(), 11); // HH:MM:SS:00
        assert!(fallback_tc.ends_with(":00"));
    }

    #[test]
    fn test_stitch_empty_segments_fails() {
        let dummy_out = PathBuf::from("/tmp/dummy_test_out.mp4");
        let result = Remuxer::stitch_segments(&[], &dummy_out, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No segments provided"));
    }

    #[test]
    fn test_stitch_segments_newline_rejected() {
        let bad_seg = PathBuf::from("/tmp/bad\nsegment.mp4");
        let dummy_out = PathBuf::from("/tmp/dummy_test_out.mp4");
        let result = Remuxer::stitch_segments(&[bad_seg], &dummy_out, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("newline character"));
    }
}
