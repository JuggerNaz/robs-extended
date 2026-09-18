//! QID marking: attribute spans of a recording to structure components.
//!
//! The QID rail lists `structure_components` rows (loaded over Postgres from
//! web-app-offshore's Supabase DB, see `db.rs`). While a recording runs,
//! clicking a QID closes the currently open segment at *now* and opens a new
//! one for the clicked component — "from this time to this time belonged to
//! that QID". This is the QID analogue of the clip marks in `clips.rs`:
//!
//! - Every segment is anchored to THREE clocks at once: wall clock (UTC),
//!   the recording's elapsed milliseconds (`RecordState::recording_time`,
//!   which excludes paused spans), and `frame_count` (the exact file
//!   position — the same anchor the clip marks use, so the web app can cut
//!   per-QID clips losslessly later).
//! - Segments survive a pause unchanged (pause gates frame sends and the
//!   elapsed accumulator; the wall clock spans the gap, like clip marks).
//! - On stop the open segment auto-closes at the final anchors, all closed
//!   segments are written to a sidecar JSON next to the recording (crash
//!   safety), and handed to the DB worker thread for persistence.
//!
//! The transitions are pure functions so they unit-test without a recording.

use chrono::{DateTime, Utc};
use serde::Serialize;

use robs_profiles::settings::DatabaseSettings;

use super::state::EventLogKind;
use super::RobsController;

/// One `structure_components` row shown in the QID rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QidComponent {
    /// `structure_components.id`.
    pub id: i64,
    /// `structure_components.q_id` (the display identity, e.g. `QID-2210-HP-101`).
    pub q_id: String,
    /// `structure_components.id_no` (sub-label under the QID).
    pub id_no: String,
    /// `structure_components.code` (component group, e.g. `ANODE`).
    pub code: String,
}

/// The anchors captured at the moment a segment opens or closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentAnchor {
    pub wall: DateTime<Utc>,
    /// Recording elapsed milliseconds at the anchor (excludes pause).
    pub elapsed_ms: u64,
    /// Recording file position in frames at the anchor.
    pub frame: u64,
}

/// One closed segment: recording time attributed to a QID.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QidSegment {
    pub component_id: i64,
    pub q_id: String,
    pub recording_path: String,
    pub wall_start: DateTime<Utc>,
    pub wall_end: DateTime<Utc>,
    pub elapsed_start_ms: u64,
    pub elapsed_end_ms: u64,
    pub frame_start: u64,
    pub frame_end: u64,
}

/// A segment that is currently open: which QID it belongs to plus the
/// anchors where it began.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenSegment {
    pub component: QidComponent,
    pub start: SegmentAnchor,
}

/// Outcome of one QID click; pure so it can be unit-tested.
#[derive(Clone, Debug, PartialEq)]
pub enum QidSelectAction {
    /// Nothing was open; the clicked QID is now open.
    Opened(QidComponent),
    /// The open segment closed at `now` and the clicked QID opened.
    Switched { closed: QidSegment, opened: QidComponent },
    /// The clicked QID is already the open one — no-op (re-click confirms
    /// rather than splitting the segment).
    SameQid,
}

/// Close `open` at `now`, attributing the span to its component.
/// Pure — the caller owns persistence and logging.
pub fn close_segment(open: &OpenSegment, now: SegmentAnchor, recording_path: &str) -> QidSegment {
    QidSegment {
        component_id: open.component.id,
        q_id: open.component.q_id.clone(),
        recording_path: recording_path.to_string(),
        wall_start: open.start.wall,
        wall_end: now.wall,
        elapsed_start_ms: open.start.elapsed_ms,
        elapsed_end_ms: now.elapsed_ms,
        frame_start: open.start.frame,
        frame_end: now.frame,
    }
}

/// Pure transition for a QID click while a recording runs.
///
/// - Clicking the already-open QID is a no-op (`SameQid`) so stray
///   double-clicks cannot split a segment.
/// - Clicking a different QID closes the open segment at `now` and opens
///   the new one (even when `now` <= open start, the closed segment simply
///   has a zero-or-negative length and the caller may discard it).
pub fn apply_qid_select(
    open: Option<&OpenSegment>,
    clicked: QidComponent,
    now: SegmentAnchor,
    recording_path: &str,
) -> QidSelectAction {
    match open {
        Some(current) if current.component.id == clicked.id => QidSelectAction::SameQid,
        Some(current) => QidSelectAction::Switched {
            closed: close_segment(current, now, recording_path),
            opened: clicked,
        },
        None => QidSelectAction::Opened(clicked),
    }
}

/// Client-side QID-rail filter: true when a component matches the search
/// text (case-insensitive on both sides). Empty text matches everything.
pub fn qid_matches(component: &QidComponent, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    needle.is_empty()
        || component.q_id.to_lowercase().contains(&needle)
        || component.id_no.to_lowercase().contains(&needle)
        || component.code.to_lowercase().contains(&needle)
}

/// QID-rail runtime state owned by the controller: the loaded component
/// list, the current selection, the open segment, and the closed segments
/// of the running session.
pub struct QidState {
    /// Persisted connection settings (`settings.json` `database` section).
    pub settings: DatabaseSettings,
    /// Components loaded from `structure_components` (ordered by `q_id`).
    pub components: Vec<QidComponent>,
    /// Selected QID. Selection persists across recordings; segments only
    /// open while a recording runs (clicking while idle pre-selects).
    pub current: Option<QidComponent>,
    /// Segment currently accumulating time (only while recording).
    pub open: Option<OpenSegment>,
    /// Closed segments of the running (or just-stopped) session.
    pub segments: Vec<QidSegment>,
    /// DB worker handle when the database is configured.
    pub db: Option<super::db::DbWorker>,
    /// Rail status word (DISABLED / OFFLINE / LOADING / SAVING / ONLINE / ERROR).
    pub status: String,
    /// 0 = neutral, 1 = good (green), 2 = warn (orange).
    pub status_state: i32,
    /// Last DB error (cleared on success), for the event-log detail.
    pub last_error: Option<String>,
}

impl QidState {
    pub fn new(settings: DatabaseSettings) -> Self {
        let (status, status_state) = if settings.is_configured() {
            ("OFFLINE".to_string(), 0)
        } else {
            ("DISABLED".to_string(), 0)
        };
        Self {
            settings,
            components: Vec::new(),
            current: None,
            open: None,
            segments: Vec::new(),
            db: None,
            status,
            status_state,
            last_error: None,
        }
    }
}

/// Sidecar file for one recording session:
/// `<recording_stem>_qid_segments.json` next to the recording file.
pub fn sidecar_path(recording_path: &str) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(recording_path);
    let stem = path.file_stem()?;
    let dir = path.parent()?;
    Some(dir.join(format!("{}_qid_segments.json", stem.to_string_lossy())))
}

/// Write the session's closed segments to the sidecar JSON (crash-safety
/// copy; each recording has a unique timestamped name so a plain write is
/// always a fresh file). Returns the path written.
pub fn write_sidecar(
    recording_path: &str,
    segments: &[QidSegment],
) -> Result<std::path::PathBuf, String> {
    let path = sidecar_path(recording_path)
        .ok_or_else(|| "recording path has no file stem".to_string())?;
    let json = serde_json::to_string_pretty(segments)
        .map_err(|e| format!("sidecar serialize failed: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("sidecar write failed: {e}"))?;
    Ok(path)
}

/// Current segment anchors from the controller's recording clock.
fn now_anchor(controller: &RobsController) -> SegmentAnchor {
    SegmentAnchor {
        wall: chrono::Utc::now(),
        elapsed_ms: controller.record.recording_time,
        frame: controller.record.frame_count,
    }
}

impl RobsController {
    /// Request a fresh QID list from the database (no-op when unconfigured).
    pub fn refresh_qids(&mut self) {
        let Some(db) = &self.qid.db else {
            self.log_event(
                "QID database not configured — set database.url + structure_id in settings.json",
                EventLogKind::Info,
            );
            return;
        };
        let structure_id = self.qid.settings.structure_id;
        if db
            .tx
            .send(super::db::DbCommand::LoadComponents { structure_id })
            .is_ok()
        {
            self.qid.status = "LOADING".into();
            self.qid.status_state = 0;
        }
    }

    /// QID rail click: pre-select when idle; close the open segment and
    /// open a new one for the clicked QID while recording.
    pub fn qid_select(&mut self, component_id: i64) {
        let Some(component) = self
            .qid
            .components
            .iter()
            .find(|c| c.id == component_id)
            .cloned()
        else {
            return;
        };
        if !self.record.recording {
            // Pre-select: the segment opens automatically at recording start.
            let changed = self.qid.current.as_ref().map(|c| c.id) != Some(component.id);
            self.qid.current = Some(component.clone());
            if changed {
                self.log_event(
                    format!(
                        "QID selected: {} (marking starts with the next recording)",
                        component.q_id
                    ),
                    EventLogKind::Record,
                );
            }
            return;
        }

        let anchors = now_anchor(self);
        let path = self.record.last_recording_path.clone();
        let mut closed: Option<QidSegment> = None;
        match apply_qid_select(self.qid.open.as_ref(), component.clone(), anchors, &path) {
            QidSelectAction::SameQid => return,
            QidSelectAction::Opened(c) => {
                self.qid.open = Some(OpenSegment { component: c, start: anchors });
                self.log_event(
                    format!(
                        "QID marking started: {} at {}",
                        component.q_id,
                        Self::format_time((anchors.elapsed_ms / 1000) as u64)
                    ),
                    EventLogKind::Record,
                );
            }
            QidSelectAction::Switched { closed: seg, opened } => {
                self.log_event(
                    format!(
                        "QID changed: {} (segment {} \u{2013} {})",
                        opened.q_id,
                        Self::format_time((seg.elapsed_start_ms / 1000) as u64),
                        Self::format_time((seg.elapsed_end_ms / 1000) as u64),
                    ),
                    EventLogKind::Record,
                );
                self.qid.open = Some(OpenSegment {
                    component: opened,
                    start: anchors,
                });
                closed = Some(seg);
            }
        }
        if let Some(seg) = closed {
            self.qid.segments.push(seg);
            self.append_qid_sidecar();
        }
    }

    /// Open a segment for the already-selected QID at recording start.
    /// Called from `start_recording` after the session state resets.
    pub(crate) fn start_qid_session(&mut self) {
        self.qid.segments.clear();
        self.qid.open = self.qid.current.clone().map(|component| OpenSegment {
            component,
            start: now_anchor(self),
        });
    }

    /// Auto-close the open segment at the final recording anchors, write
    /// the sidecar JSON, and hand all segments to the DB worker. Called
    /// from `stop_recording` BEFORE the elapsed-time reset.
    pub(crate) fn stop_qid_session(&mut self) {
        if let Some(open) = self.qid.open.take() {
            let anchors = now_anchor(self);
            let path = self.record.last_recording_path.clone();
            self.qid.segments.push(close_segment(&open, anchors, &path));
        }
        let segments = std::mem::take(&mut self.qid.segments);
        if segments.is_empty() {
            return;
        }
        // Sidecar first: it is the crash-safe copy regardless of DB state.
        match write_sidecar(&segments[0].recording_path, &segments) {
            Ok(path) => self.log_event(
                format!(
                    "{} QID segment(s) written to {}",
                    segments.len(),
                    path.display()
                ),
                EventLogKind::Record,
            ),
            Err(e) => self.log_event(format!("QID sidecar write failed: {e}"), EventLogKind::Info),
        }
        if self.qid.settings.is_configured() {
            if let Some(db) = &self.qid.db {
                let structure_id = self.qid.settings.structure_id;
                if db
                    .tx
                    .send(super::db::DbCommand::InsertSegments { structure_id, segments })
                    .is_ok()
                {
                    self.qid.status = "SAVING".into();
                    self.qid.status_state = 0;
                }
            }
        }
    }

    /// Append the already-closed segments to the sidecar (called per QID
    /// switch so a crash mid-recording keeps every closed span).
    fn append_qid_sidecar(&mut self) {
        if self.qid.segments.is_empty() {
            return;
        }
        let path = self.record.last_recording_path.clone();
        if let Err(e) = write_sidecar(&path, &self.qid.segments) {
            eprintln!("[QID] sidecar append failed: {e}");
        }
    }

    /// Drain DB worker results: apply the component list / surface errors.
    /// Returns `true` while a load or save is still in flight (the caller
    /// keeps ticking so the result surfaces promptly).
    pub(crate) fn drain_qid_db_events(&mut self) -> bool {
        let results: Vec<super::db::DbResult> = {
            let Some(db) = &self.qid.db else {
                return false;
            };
            let mut out = Vec::new();
            while let Ok(result) = db.rx.try_recv() {
                out.push(result);
            }
            out
        };
        for result in results {
            match result {
                super::db::DbResult::Components(list) => {
                    let count = list.len();
                    self.qid.components = list;
                    // Drop a selection that no longer exists.
                    if let Some(cur) = &self.qid.current {
                        if !self.qid.components.iter().any(|c| c.id == cur.id) {
                            self.qid.current = None;
                        }
                    }
                    self.qid.status = "ONLINE".into();
                    self.qid.status_state = 1;
                    self.qid.last_error = None;
                    self.log_event(
                        format!("QID list loaded ({count} components)"),
                        EventLogKind::Info,
                    );
                }
                super::db::DbResult::ComponentsFailed(error) => {
                    self.qid.status = "ERROR".into();
                    self.qid.status_state = 2;
                    self.qid.last_error = Some(error.clone());
                    self.log_event(format!("QID load failed: {error}"), EventLogKind::Info);
                }
                super::db::DbResult::SegmentsInserted { count } => {
                    self.qid.status = "ONLINE".into();
                    self.qid.status_state = 1;
                    self.log_event(
                        format!("{count} QID segment(s) saved to database"),
                        EventLogKind::Record,
                    );
                }
                super::db::DbResult::SegmentsFailed { error, segments } => {
                    self.qid.status = "ERROR".into();
                    self.qid.status_state = 2;
                    self.qid.last_error = Some(error.clone());
                    let sidecar = segments
                        .first()
                        .and_then(|s| sidecar_path(&s.recording_path))
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    self.log_event(
                        format!(
                            "QID segment insert failed: {error} \u{2014} sidecar copy at {sidecar}"
                        ),
                        EventLogKind::Info,
                    );
                }
            }
        }
        self.qid.status == "LOADING" || self.qid.status == "SAVING"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn anchor(secs: i64, elapsed_ms: u64, frame: u64) -> SegmentAnchor {
        SegmentAnchor {
            wall: Utc.timestamp_opt(secs, 0).unwrap(),
            elapsed_ms,
            frame,
        }
    }

    fn comp(id: i64, qid: &str) -> QidComponent {
        QidComponent {
            id,
            q_id: qid.into(),
            id_no: "Face A1-A2".into(),
            code: "ANODE".into(),
        }
    }

    #[test]
    fn first_click_opens_without_closing() {
        let clicked = comp(1, "QID-2210-HP-100");
        match apply_qid_select(None, clicked.clone(), anchor(100, 5_000, 150), "rec.mp4") {
            QidSelectAction::Opened(c) => assert_eq!(c, clicked),
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn second_click_closes_and_switches() {
        let first = comp(1, "QID-2210-HP-100");
        let open = OpenSegment { component: first, start: anchor(100, 0, 0) };
        let second = comp(2, "QID-2210-HP-101");
        match apply_qid_select(
            Some(&open),
            second.clone(),
            anchor(160, 60_000, 1_800),
            "rec.mp4",
        ) {
            QidSelectAction::Switched { closed, opened } => {
                assert_eq!(closed.component_id, 1);
                assert_eq!(closed.wall_start.timestamp(), 100);
                assert_eq!(closed.wall_end.timestamp(), 160);
                assert_eq!(closed.elapsed_start_ms, 0);
                assert_eq!(closed.elapsed_end_ms, 60_000);
                assert_eq!(closed.frame_start, 0);
                assert_eq!(closed.frame_end, 1_800);
                assert_eq!(closed.recording_path, "rec.mp4");
                assert_eq!(opened, second);
            }
            other => panic!("expected Switched, got {other:?}"),
        }
    }

    #[test]
    fn reclicking_open_qid_is_a_noop() {
        let first = comp(1, "QID-2210-HP-100");
        let open = OpenSegment { component: first.clone(), start: anchor(100, 0, 0) };
        match apply_qid_select(Some(&open), first, anchor(160, 60_000, 1_800), "rec.mp4") {
            QidSelectAction::SameQid => {}
            other => panic!("expected SameQid, got {other:?}"),
        }
    }

    #[test]
    fn filter_matches_qid_idno_and_code_case_insensitively() {
        let c = comp(1, "QID-2210-HP-101");
        assert!(qid_matches(&c, ""));
        assert!(qid_matches(&c, "hp-101"));
        assert!(qid_matches(&c, "face a1"));
        assert!(qid_matches(&c, "ANODE"));
        assert!(!qid_matches(&c, "hp-102"));
    }
}
