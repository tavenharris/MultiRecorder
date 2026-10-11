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
        for entry in fs::read_dir(&self.buffer_dir)?.flatten() {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with(&prefix) && file_name.ends_with(".mp4") {
                    if let Ok(metadata) = entry.metadata() {
                        let created_at = metadata.modified().unwrap_or(SystemTime::now());
                        segments.push(SegmentInfo {
                            path,
                            created_at,
                            size_bytes: metadata.len(),
                        });
                    }
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

        let start = segments.len().saturating_sub(needed_count);
        let selected: Vec<PathBuf> = segments
            .into_iter()
            .skip(start)
            .map(|s| s.path)
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

    /// Deletes all segment files on disk for this source
    pub fn wipe_all(&self) -> Result<usize> {
        let segments = self.list_segments()?;
        let mut removed = 0;
        for seg in segments {
            if let Ok(()) = fs::remove_file(&seg.path) {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Wipes all mp4 segment files in the buffer directory
    pub fn wipe_buffer_dir(buffer_dir: &std::path::Path) -> Result<usize> {
        if !buffer_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        if let Ok(entries) = fs::read_dir(buffer_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "mp4").unwrap_or(false) {
                    if let Ok(()) = fs::remove_file(p) {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
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

        // Test get_segments_for_duration
        let one_seg = mgr.get_segments_for_duration(Duration::from_secs(2)).unwrap();
        assert_eq!(one_seg.len(), 1);
        assert_eq!(one_seg[0], file2);

        let both_segs = mgr.get_segments_for_duration(Duration::from_secs(4)).unwrap();
        assert_eq!(both_segs.len(), 2);
        assert_eq!(both_segs[0], file1);
        assert_eq!(both_segs[1], file2);

        // Test evict_older_than with 0 duration (evicts older than now)
        std::thread::sleep(Duration::from_millis(100));
        let evicted = mgr.evict_older_than(Duration::from_millis(50)).unwrap();
        assert_eq!(evicted, 2);
        assert_eq!(mgr.list_segments().unwrap().len(), 0);

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_wipe_buffer_operations() {
        let temp_dir = std::env::temp_dir().join(format!("test_ring_wipe_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);

        let mgr = RingBufferManager::new("gameplay", &temp_dir, 2);

        let file1 = temp_dir.join("gameplay_20261010_120000.mp4");
        let file2 = temp_dir.join("gameplay_20261010_120002.mp4");
        let other = temp_dir.join("facecam_20261010_120000.mp4");
        let txt = temp_dir.join("not_a_segment.txt");

        fs::write(&file1, b"mock 1").unwrap();
        fs::write(&file2, b"mock 2").unwrap();
        fs::write(&other, b"mock other").unwrap();
        fs::write(&txt, b"keep me").unwrap();

        // wipe_all deletes only gameplay segments
        let removed = mgr.wipe_all().unwrap();
        assert_eq!(removed, 2);
        assert!(!file1.exists());
        assert!(!file2.exists());
        assert!(other.exists());
        assert!(txt.exists());

        // wipe_buffer_dir removes all .mp4 segments in the folder, preserving non-mp4 files
        let wiped = RingBufferManager::wipe_buffer_dir(&temp_dir).unwrap();
        assert_eq!(wiped, 1);
        assert!(!other.exists());
        assert!(txt.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
