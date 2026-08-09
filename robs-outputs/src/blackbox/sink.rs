//! Storage sink abstraction for the Blackbox engine.
//!
//! The engine talks to storage only through [`BlackboxSink`]. Today the only
//! implementation is [`LocalFileSink`] (local disk with ring-buffer cleanup).
//! A future `CloudArchiveSink` can stage segments locally, upload them on
//! close, and prune remote storage — without the engine changing.
//!
//! All sink methods are synchronous by design: the engine runs on plain std
//! threads and must never need a runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::config::BlackboxConfig;

/// Metadata for a segment that has just been finalized (ffmpeg exited and the
/// output file is complete).
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub path: PathBuf,
    pub index: u64,
    pub bytes: u64,
    pub duration_ms: u64,
}

/// Disk-space snapshot used by the engine's monitor.
#[derive(Debug, Clone, Default)]
pub struct StorageStatus {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl StorageStatus {
    /// Percentage of the disk that is free (0.0–100.0). Returns 0.0 when the
    /// total is unknown (avoids div-by-zero).
    pub fn free_percent(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.free_bytes as f64 / self.total_bytes as f64 * 100.0) as f32
        }
    }
}

/// A place for blackbox segments to land.
///
/// Implementations must be cheap to clone: the engine holds one clone on its
/// worker thread and another on its monitor thread.
pub trait BlackboxSink: Send + Sync {
    /// Return the output path for the segment with the given 0-based index and
    /// start time. Called when the engine is about to open a new ffmpeg
    /// invocation.
    fn segment_target(&self, index: u64, started_at: chrono::DateTime<chrono::Utc>) -> PathBuf;

    /// Hook fired after a segment has been finalized on disk. Used by cloud
    /// sinks to upload; a no-op for local sinks.
    fn on_segment_closed(&self, info: &SegmentInfo) -> Result<()>;

    /// Free up at least `bytes_needed` bytes (best effort). Returns the number
    /// of bytes actually reclaimed. Local sinks delete oldest segments past the
    /// retention limit; cloud sinks prune remote storage.
    fn reclaim(&self, bytes_needed: u64) -> Result<u64>;

    /// Probe free / total space on the target filesystem. Returns zeros on
    /// failure (the monitor treats unknown space as healthy rather than
    /// false-alarming a critical condition).
    fn storage_status(&self) -> StorageStatus;
}

/// Local-disk sink. Segments are written directly to `output_dir`; oldest
/// segments beyond `max_retention_gb` are deleted on reclaim.
#[derive(Clone)]
pub struct LocalFileSink {
    output_dir: PathBuf,
    /// Segment files this sink has finalized, newest-last. Guarded so the
    /// monitor (reclaim) and worker (on_segment_closed) can touch it from
    /// different threads.
    known_segments: Arc<Mutex<Vec<PathBuf>>>,
    max_retention_bytes: u64,
}

impl std::fmt::Debug for LocalFileSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFileSink")
            .field("output_dir", &self.output_dir)
            .field("max_retention_bytes", &self.max_retention_bytes)
            .finish()
    }
}

impl LocalFileSink {
    pub fn new(config: &BlackboxConfig) -> Self {
        Self {
            output_dir: config.output_dir.clone(),
            known_segments: Arc::new(Mutex::new(Vec::new())),
            max_retention_bytes: config.max_retention_gb as u64 * 1024 * 1024 * 1024,
        }
    }

    /// List finalized `.mkv` segments in the output dir, oldest-first, paired
    /// with their size. Used for retention accounting on reclaim.
    fn list_segments_oldest_first(&self) -> Vec<(PathBuf, u64)> {
        let mut entries: Vec<(PathBuf, u64, u64)> = Vec::new();
        let read = match fs::read_dir(&self.output_dir) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("mkv") {
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    entries.push((path, meta.len(), mtime));
                }
            }
        }
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        entries.into_iter().map(|(p, s, _)| (p, s)).collect()
    }
}

impl BlackboxSink for LocalFileSink {
    fn segment_target(&self, index: u64, started_at: chrono::DateTime<chrono::Utc>) -> PathBuf {
        let stamp = started_at.format("%Y-%m-%d_%H-%M-%S");
        self.output_dir
            .join(format!("blackbox_{:04}_{}.mkv", index, stamp))
    }

    fn on_segment_closed(&self, info: &SegmentInfo) -> Result<()> {
        self.known_segments.lock().push(info.path.clone());
        Ok(())
    }

    fn reclaim(&self, bytes_needed: u64) -> Result<u64> {
        let segments = self.list_segments_oldest_first();

        // We want to free enough that BOTH hold:
        //   (a) the on-disk total drops to <= the retention cap, and
        //   (b) at least `bytes_needed` extra bytes are available.
        let total: u64 = segments.iter().map(|(_, s)| s).sum();
        let over_retention = total.saturating_sub(self.max_retention_bytes);
        let target = bytes_needed.max(over_retention);

        let mut freed = 0u64;
        for (path, size) in segments {
            if freed >= target {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                freed += size;
            }
        }

        if freed > 0 {
            self.known_segments.lock().retain(|p| p.exists());
        }
        Ok(freed)
    }

    fn storage_status(&self) -> StorageStatus {
        // Ensure the dir exists so the probe targets the right volume; if it
        // can't be created, fall back to its parent.
        let probe_path = if fs::metadata(&self.output_dir).is_ok() {
            self.output_dir.clone()
        } else if let Some(parent) = self.output_dir.parent() {
            parent.to_path_buf()
        } else {
            return StorageStatus::default();
        };

        let total = fs4::total_space(&probe_path).unwrap_or(0);
        let free = fs4::free_space(&probe_path).unwrap_or(0);
        StorageStatus {
            free_bytes: free,
            total_bytes: total,
        }
    }
}

/// Ensure the given directory exists, creating it (and parents) as needed.
pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("creating blackbox dir {:?}", path))?;
    Ok(())
}
