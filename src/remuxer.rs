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
            "-map",
            "0:v",
            "-map",
            "0:a?",
            "-disposition:a",
            "default",
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

        if !is_mkv {
            let _ = Self::clear_mp4_mov_alternate_groups(output_file);
        }

        tracing::info!("Successfully exported {:?}", output_file);
        Ok(())
    }

    /// Clears `alternate_group` (sets to `0`) and ensures `flags = 0x0003` (`Track_enabled | Track_in_movie`)
    /// on all audio/video `tkhd` (Track Header) atoms in the faststart `moov` header of `.mp4` / `.mov` files.
    /// By default, FFmpeg assigns non-zero `alternate_group` values to audio tracks, which causes QuickTime
    /// and Adobe Premiere Pro to treat tracks 2..N as mutually-exclusive language dubs instead of simultaneous
    /// multitrack stems.
    pub fn clear_mp4_mov_alternate_groups(file_path: &Path) -> Result<()> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(file_path)?;
        let len = file.metadata()?.len() as usize;
        let scan_len = len.min(262_144);
        if scan_len < 64 {
            return Ok(());
        }

        let mut buf = vec![0u8; scan_len];
        file.read_exact(&mut buf)?;

        let mut modified = false;
        let mut i = 4usize;
        while i + 56 <= scan_len {
            if &buf[i..i + 4] == b"tkhd" {
                let box_size = u32::from_be_bytes([buf[i - 4], buf[i - 3], buf[i - 2], buf[i - 1]]);
                let version = buf[i + 4];
                if (version == 0 && box_size == 92) || (version == 1 && box_size == 104) {
                    let alt_offset = if version == 0 { i + 38 } else { i + 50 };
                    let vol_offset = if version == 0 { i + 40 } else { i + 52 };
                    // Audio tracks in MP4/MOV have volume > 0 (typically 0x0100 = 1.0)
                    let volume = u16::from_be_bytes([buf[vol_offset], buf[vol_offset + 1]]);
                    if volume > 0 {
                        // Ensure Track_enabled | Track_in_movie | Track_in_preview (0x07)
                        buf[i + 7] |= 0x07;
                    }
                    if buf[alt_offset] != 0 || buf[alt_offset + 1] != 0 {
                        buf[alt_offset] = 0;
                        buf[alt_offset + 1] = 0;
                        modified = true;
                    }
                }
            }
            i += 1;
        }

        if modified {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&buf)?;
            file.flush()?;
        }
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

    /// Converts an `HH:MM:SS:FF` SMPTE timecode into Broadcast Wave Format (BWF) `time_reference`
    /// (number of audio samples since midnight at `sample_rate`, e.g. 48000 Hz) so Adobe Premiere Pro
    /// and DaVinci Resolve can auto-synchronize standalone `.wav` files with `.mp4`/`.mov` video via Timecode.
    pub fn timecode_to_bwf_sample_reference(timecode: &str, sample_rate: u64) -> Option<u64> {
        let parts: Vec<&str> = timecode.split(':').collect();
        if parts.len() >= 3 {
            let hh: u64 = parts[0].parse().ok()?;
            let mm: u64 = parts[1].parse().ok()?;
            let ss: u64 = parts[2].parse().ok()?;
            let total_secs = hh * 3600 + mm * 60 + ss;
            Some(total_secs * sample_rate)
        } else {
            None
        }
    }

    /// Produces a clean, concise filesystem-safe label for an audio track filename.
    pub fn sanitize_track_label(name: &str) -> String {
        let cleaned = name
            .replace("(Default Output)", "")
            .replace("(WASAPI Loopback)", "");
        let mut out = String::new();
        let mut prev_underscore = false;
        for c in cleaned.trim().chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
                prev_underscore = false;
            } else if !prev_underscore && !out.is_empty() {
                out.push('_');
                prev_underscore = true;
            }
        }
        let trimmed = out.trim_matches('_');
        if trimmed.is_empty() {
            "Audio".to_string()
        } else if trimmed.len() > 32 {
            trimmed[..32].trim_matches('_').to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Extracts each enabled audio track from the stitched primary video container into its own
    /// standalone Broadcast WAV (`.wav`) file with embedded BWF SMPTE Timecode (`time_reference`)
    /// and zero-aligned start timestamps (`first_pts=0`) for instant sync in Premiere Pro.
    pub fn export_separate_audio_tracks(
        primary_video_file: &Path,
        output_dir: &Path,
        prefix: &str,
        timestamp: &str,
        audio_tracks: &[crate::config::AudioTrackConfig],
        timecode: Option<&str>,
    ) -> Result<Vec<PathBuf>> {
        let enabled: Vec<(usize, &crate::config::AudioTrackConfig)> = audio_tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.enabled && t.device_id != "none")
            .collect();

        if enabled.is_empty() {
            return Ok(Vec::new());
        }

        fs::create_dir_all(output_dir)?;

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-i",
            &primary_video_file.to_string_lossy(),
        ]);

        let bwf_ref = timecode.and_then(|tc| Self::timecode_to_bwf_sample_reference(tc, 48000));
        let mut exported_wavs = Vec::new();

        for (stream_idx, (track_slot_idx, track)) in enabled.iter().enumerate() {
            let label = Self::sanitize_track_label(&track.name);
            let wav_path = output_dir.join(format!(
                "{}_{}_track{}_{}.wav",
                prefix,
                timestamp,
                track_slot_idx + 1,
                label
            ));

            cmd.args([
                "-map",
                &format!("0:a:{}", stream_idx),
                "-af",
                "aresample=async=1:first_pts=0",
                "-c:a",
                "pcm_s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-rf64",
                "auto",
                "-write_bext",
                "1",
                "-metadata",
                &format!("title={}", track.name),
            ]);

            if let Some(tc) = timecode {
                cmd.args(["-metadata", &format!("timecode={}", tc)]);
            }
            if let Some(samples) = bwf_ref {
                cmd.args(["-metadata", &format!("time_reference={}", samples)]);
            }

            cmd.arg(&wav_path);
            exported_wavs.push(wav_path);
        }

        tracing::info!(
            "Exporting {} standalone synchronized BWF WAV audio track(s) from {:?}...",
            exported_wavs.len(),
            primary_video_file
        );

        let status = cmd
            .output()
            .context("Failed to run ffmpeg standalone audio track exporter")?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            tracing::warn!("Standalone audio track export warning: {}", stderr);
            // Return any wav files that were successfully created
            exported_wavs.retain(|p| p.exists());
        }

        Ok(exported_wavs)
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
