//! Rolling-window tracker for the Anomaly buffer's finalized scratch segments.
//!
//! The worker pushes each finalized segment here; [`Ring::sweep`] trims the ring
//! to roughly the pre-roll window (plus a small buffer) and a hard byte cap by
//! deleting the oldest segments; [`Ring::pin_pre_roll`] returns the newest
//! segments covering at least the requested pre-roll duration, oldest-first.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

/// One finalized scratch segment tracked by the ring.
#[derive(Debug, Clone)]
pub struct RingEntry {
    pub path: PathBuf,
    pub index: u64,
    pub started_utc: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub bytes: u64,
}

/// The rolling buffer of finalized segments. Entries are ordered oldest-first
/// (front = oldest, back = newest).
pub struct Ring {
    entries: VecDeque<RingEntry>,
    total_bytes: u64,
    max_bytes: u64,
}

impl Ring {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total footage currently held, in whole seconds (floored).
    pub fn secs_filled(&self) -> u64 {
        let ms: u64 = self.entries.iter().map(|e| e.duration_ms).sum();
        ms / 1000
    }

    /// Register a newly finalized segment.
    pub fn push(&mut self, entry: RingEntry) {
        self.total_bytes = self.total_bytes.saturating_add(entry.bytes);
        self.entries.push_back(entry);
    }

    /// Trim the ring. Drops oldest segments while the held footage exceeds
    /// `keep_secs` *or* the byte cap is exceeded, always keeping at least one
    /// entry. Deleted segments are removed from disk (best effort).
    pub fn sweep(&mut self, keep_secs: u64) {
        let keep_ms = keep_secs.saturating_mul(1000);
        loop {
            if self.entries.len() <= 1 {
                break;
            }
            let total_ms: u64 = self.entries.iter().map(|e| e.duration_ms).sum();
            let over_time = total_ms > keep_ms;
            let over_size = self.total_bytes > self.max_bytes;
            if !over_time && !over_size {
                break;
            }
            if let Some(e) = self.entries.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(e.bytes);
                let _ = fs::remove_file(&e.path);
            }
        }
    }

    /// Return the newest segments covering at least `pre_secs` of footage,
    /// oldest-first. Returns fewer (or none) if the ring holds less than that.
    pub fn pin_pre_roll(&self, pre_secs: u64) -> Vec<PathBuf> {
        let want_ms = pre_secs.saturating_mul(1000);
        let mut acc_ms = 0u64;
        let mut count = 0usize;
        for e in self.entries.iter().rev() {
            if acc_ms >= want_ms {
                break;
            }
            acc_ms = acc_ms.saturating_add(e.duration_ms);
            count += 1;
        }
        let start = self.entries.len().saturating_sub(count);
        self.entries
            .iter()
            .skip(start)
            .map(|e| e.path.clone())
            .collect()
    }
}
