use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub path: PathBuf,
    pub created_at: SystemTime,
    pub size_bytes: u64,
}

#[derive(Debug)]
pub struct RingBufferManager {
    source_name: String,
    buffer_dir: PathBuf,
    segment_duration_secs: u64,
}

#[derive(Debug, Clone)]
pub struct BufferStats {
    pub segment_count: usize,
    pub total_size_mb: f64,
    pub estimated_duration_secs: u64,
}

impl RingBufferManager {
    pub fn new(source_name: impl Into<String>, buffer_dir: impl Into<PathBuf>, segment_duration_secs: u64) -> Self {
        Self {
            source_name: source_name.into(),
            buffer_dir: buffer_dir.into(),
            segment_duration_secs,
        }
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Finds all segments on disk for this source, sorted chronologically (oldest to newest)
    pub fn list_segments(&self) -> Result<Vec<SegmentInfo>> {
        let mut segments = Vec::new();
        if !self.buffer_dir.exists() {
            return Ok(segments);
        }

        let prefix = format!("{}_", self.source_name);
        for entry in fs::read_dir(&self.buffer_dir)? {
            let entry = entry?;
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with(&prefix) && file_name.ends_with(".mp4") {
                    let metadata = entry.metadata()?;
                    let created_at = metadata.modified().unwrap_or(SystemTime::now());
                    segments.push(SegmentInfo {
                        path,
                        created_at,
                        size_bytes: metadata.len(),
                    });
                }
            }
        }

        // Sort by modification time
        segments.sort_by_key(|s| s.created_at);
        Ok(segments)
    }

    /// Evicts segments older than `max_age` from disk
    pub fn evict_older_than(&self, max_age: Duration) -> Result<usize> {
        let segments = self.list_segments()?;
        let now = SystemTime::now();
        let mut removed_count = 0;

        for seg in segments {
            if let Ok(age) = now.duration_since(seg.created_at) {
                if age > max_age {
                    if let Err(e) = fs::remove_file(&seg.path) {
                        tracing::warn!("Failed to delete old segment {:?}: {}", seg.path, e);
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        Ok(removed_count)
    }

    /// Retrieves segments spanning the last `duration` of time
    pub fn get_segments_for_duration(&self, duration: Duration) -> Result<Vec<PathBuf>> {
        let segments = self.list_segments()?;
        if segments.is_empty() {
            return Ok(vec![]);
        }

        let needed_count = ((duration.as_secs() + self.segment_duration_secs - 1)
            / self.segment_duration_secs.max(1))
        .max(1) as usize;

        let selected: Vec<PathBuf> = segments
            .iter()
            .rev()
            .take(needed_count)
            .rev()
            .map(|s| s.path.clone())
            .collect();

        Ok(selected)
    }

    /// Returns current buffer metrics
    pub fn get_stats(&self) -> Result<BufferStats> {
        let segments = self.list_segments()?;
        let count = segments.len();
        let total_bytes: u64 = segments.iter().map(|s| s.size_bytes).sum();
        let size_mb = total_bytes as f64 / (1024.0 * 1024.0);
        let est_duration = count as u64 * self.segment_duration_secs;

        Ok(BufferStats {
            segment_count: count,
            total_size_mb: size_mb,
            estimated_duration_secs: est_duration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_lifecycle() {
        let temp_dir = std::env::temp_dir().join(format!("test_ring_buf_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);

        let mgr = RingBufferManager::new("gameplay", &temp_dir, 2);

        // Create 3 mock segment files
        let file1 = temp_dir.join("gameplay_20261010_120000.mp4");
        let file2 = temp_dir.join("gameplay_20261010_120002.mp4");
        let other_stream = temp_dir.join("facecam_20261010_120000.mp4");

        fs::write(&file1, b"mock video 1").unwrap();
        fs::write(&file2, b"mock video 2").unwrap();
        fs::write(&other_stream, b"mock facecam").unwrap();

        let segments = mgr.list_segments().unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(mgr.source_name(), "gameplay");

        let stats = mgr.get_stats().unwrap();
        assert_eq!(stats.segment_count, 2);
        assert_eq!(stats.estimated_duration_secs, 4);

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
