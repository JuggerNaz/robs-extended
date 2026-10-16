//! QID marking: attribute spans of a recording to structure components.
//!
//! The QID rail lists `structure_components` rows (loaded over Postgres from
//! web-app-offshore's Supabase DB, see `db.rs`). While a recording runs,
//! clicking a QID toggles its recording link: a click on an idle QID opens a
//! new segment at *now* (leaving any other open links untouched), and a click
//! on a QID that is already recording closes only that segment — so several
//! QIDs can be attributed overlapping spans of the same recording
//! ("from this time to this time belonged to that QID"). This is the QID
//! analogue of the clip marks in `clips.rs`:
//!
//! - Every segment is anchored to THREE clocks at once: wall clock (UTC),
//!   the recording's elapsed milliseconds (`RecordState::recording_time`,
//!   which excludes paused spans), and `frame_count` (the exact file
//!   position — the same anchor the clip marks use, so the web app can cut
//!   per-QID clips losslessly later).
//! - Segments survive a pause unchanged (pause gates frame sends and the
//!   elapsed accumulator; the wall clock spans the gap, like clip marks).
//! - On stop every still-open segment auto-closes at the final anchors (each
//!   keeping its own start), all closed segments are written to a sidecar JSON
//!   next to the recording (crash safety), and handed to the DB worker thread
//!   for persistence. The sidecar/table are per-segment rows, so overlapping
//!   multi-QID spans need no schema change.
//!
//! The transitions are pure functions so they unit-test without a recording.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use robs_profiles::settings::DatabaseSettings;

use super::state::EventLogKind;
use super::RobsController;

/// One `structure_components` row shown in the QID rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QidComponent {
    /// `structure_components.id`.
    pub id: i64,
    /// `structure_components.structure_id` (the owning structure — used to
    /// tag segments so "all structures" mode still records provenance).
    pub structure_id: i32,
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

/// One closed segment: recording time attributed to a QID. (Deserialize is
/// for tests reading back a sidecar; the recorder only ever writes it.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QidSegment {
    pub component_id: i64,
    /// Structure owning the component (newer sidecars have this key; older
    /// ones simply lack it — the file is a write-only recovery copy).
    pub structure_id: i32,
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
pub enum QidClickAction {
    /// The clicked QID had no open link; one is now open (any other open
    /// links are untouched).
    Opened(QidComponent),
    /// The clicked QID's link closed at `now`; other open links continue.
    Closed(QidSegment),
}

/// Close `open` at `now`, attributing the span to its component.
/// Pure — the caller owns persistence and logging.
pub fn close_segment(open: &OpenSegment, now: SegmentAnchor, recording_path: &str) -> QidSegment {
    QidSegment {
        component_id: open.component.id,
        structure_id: open.component.structure_id,
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

/// Pure transition for a QID click while a recording runs: a toggle on the
/// clicked QID's link. Links are independent — at most one open segment per
/// component, but any number of components may be open concurrently and
/// their closed spans may overlap.
///
/// - Clicking an idle QID opens its segment at `now`.
/// - Clicking an already-open QID closes only its segment at `now` (even
///   when `now` <= its start, the closed segment simply has a zero-or-
///   negative length and the caller may discard it).
pub fn apply_qid_click(
    open: &[OpenSegment],
    clicked: QidComponent,
    now: SegmentAnchor,
    recording_path: &str,
) -> QidClickAction {
    match open.iter().find(|s| s.component.id == clicked.id) {
        Some(current) => QidClickAction::Closed(close_segment(current, now, recording_path)),
        None => QidClickAction::Opened(clicked),
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
/// list, the current selection, the open segments, and the closed segments
/// of the running session.
pub struct QidState {
    /// Persisted connection settings (`settings.json` `database` section).
    pub settings: DatabaseSettings,
    /// Components loaded from `structure_components` (ordered by `q_id`).
    pub components: Vec<QidComponent>,
    /// Last-clicked ("armed") QID. Selection persists across recordings;
    /// it auto-opens a segment when the next recording starts. Segments only
    /// open while a recording runs (clicking while idle never reaches here).
    pub current: Option<QidComponent>,
    /// Segments currently accumulating time (only while recording; one per
    /// component, several components concurrently).
    pub open: Vec<OpenSegment>,
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
            open: Vec::new(),
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

/// Service owning the QID-rail state cluster ([`QidState`]): the component
/// list, the open/closed segments, and the DB worker handle. Cross-service
/// reads (the recording clock anchors and the recording path) are explicit
/// parameters; event-log lines are returned to the facade in order.
pub struct QidService {
    state: QidState,
}

impl QidService {
    pub fn new(settings: DatabaseSettings) -> Self {
        Self {
            state: QidState::new(settings),
        }
    }

    /// Request a fresh QID list from the database. Returns the event-log
    /// line when the database is unconfigured (the caller logs it).
    pub fn refresh_qids(&mut self) -> Option<String> {
        let Some(db) = &self.db else {
            return Some(
                "QID database not configured — set database.url (and optionally structure_id, 0 = all) in settings.json"
                    .to_string(),
            );
        };
        // `structure_id == 0` means "all structures" (see `DatabaseSettings`).
        let structure_id = (self.settings.structure_id > 0).then_some(self.settings.structure_id);
        if db
            .tx
            .send(super::db::DbCommand::LoadComponents { structure_id })
            .is_ok()
        {
            self.status = "LOADING".into();
            self.status_state = 0;
        }
        None
    }

    /// QID rail click: toggle the clicked QID's recording link — open it
    /// when idle, close only it when already recording. Other open links are
    /// untouched, so several QIDs can record concurrently. Returns the
    /// event-log lines in order. The recording guard stays with the facade —
    /// idle clicks never reach this method.
    pub fn select(
        &mut self,
        component_id: i64,
        anchor: SegmentAnchor,
        recording_path: &str,
    ) -> Vec<(String, EventLogKind)> {
        let Some(component) = self
            .components
            .iter()
            .find(|c| c.id == component_id)
            .cloned()
        else {
            return Vec::new();
        };

        let mut logs: Vec<(String, EventLogKind)> = Vec::new();
        let mut closed: Option<QidSegment> = None;
        match apply_qid_click(&self.open, component.clone(), anchor, recording_path) {
            QidClickAction::Opened(c) => {
                self.open.push(OpenSegment { component: c, start: anchor });
                logs.push((
                    format!(
                        "QID marking started: {} at {} ({} link(s) open)",
                        component.q_id,
                        crate::RobsController::format_time((anchor.elapsed_ms / 1000) as u64),
                        self.open.len()
                    ),
                    EventLogKind::Record,
                ));
            }
            QidClickAction::Closed(seg) => {
                logs.push((
                    format!(
                        "QID marking ended: {} (segment {} \u{2013} {})",
                        component.q_id,
                        crate::RobsController::format_time((seg.elapsed_start_ms / 1000) as u64),
                        crate::RobsController::format_time((seg.elapsed_end_ms / 1000) as u64),
                    ),
                    EventLogKind::Record,
                ));
                self.open.retain(|s| s.component.id != component.id);
                closed = Some(seg);
            }
        }
        // Row highlights follow the open links; `current` tracks the last
        // click so the next recording auto-opens this QID.
        self.current = Some(component);
        if let Some(seg) = closed {
            self.segments.push(seg);
            self.append_sidecar(recording_path);
        }
        logs
    }

    /// Open a segment for the already-selected QID at recording start (the
    /// selection persists from the previous session). Called from
    /// `start_recording` after the session state resets.
    pub fn start_session(&mut self, anchor: SegmentAnchor) {
        self.segments.clear();
        self.open = self
            .current
            .clone()
            .map(|component| OpenSegment {
                component,
                start: anchor,
            })
            .into_iter()
            .collect();
    }

    /// Auto-close every open segment at the final recording anchors (each
    /// keeping its own start), write the sidecar JSON, and hand all segments
    /// to the DB worker. Called from `stop_recording` BEFORE the elapsed-time
    /// reset. Returns the event-log lines in order.
    pub fn stop_session(
        &mut self,
        anchor: SegmentAnchor,
        recording_path: &str,
    ) -> Vec<(String, EventLogKind)> {
        // Take first, then extend: field access goes through `DerefMut`, so
        // draining `self.open` while pushing to `self.segments` would hold
        // two overlapping borrows of `*self`.
        let open_segments = std::mem::take(&mut self.open);
        self.segments.extend(
            open_segments
                .iter()
                .map(|open| close_segment(open, anchor, recording_path)),
        );
        let segments = std::mem::take(&mut self.segments);
        let mut logs: Vec<(String, EventLogKind)> = Vec::new();
        if segments.is_empty() {
            return logs;
        }
        // Sidecar first: it is the crash-safe copy regardless of DB state.
        match write_sidecar(&segments[0].recording_path, &segments) {
            Ok(path) => logs.push((
                format!(
                    "{} QID segment(s) written to {}",
                    segments.len(),
                    path.display()
                ),
                EventLogKind::Record,
            )),
            Err(e) => logs.push((
                format!("QID sidecar write failed: {e}"),
                EventLogKind::Info,
            )),
        }
        if self.settings.is_configured() {
            if let Some(db) = &self.db {
                if db
                    .tx
                    .send(super::db::DbCommand::InsertSegments { segments })
                    .is_ok()
                {
                    self.status = "SAVING".into();
                    self.status_state = 0;
                }
            }
        }
        logs
    }

    /// Drain DB worker results: apply the component list / surface errors.
    /// Returns the event-log lines in order plus a `true` busy flag while a
    /// load or save is still in flight (the caller keeps ticking so the
    /// result surfaces promptly).
    pub fn drain_db_events(&mut self) -> (bool, Vec<(String, EventLogKind)>) {
        let results: Vec<super::db::DbResult> = {
            let Some(db) = &self.db else {
                return (false, Vec::new());
            };
            let mut out = Vec::new();
            while let Ok(result) = db.rx.try_recv() {
                out.push(result);
            }
            out
        };
        let mut logs: Vec<(String, EventLogKind)> = Vec::new();
        for result in results {
            match result {
                super::db::DbResult::Components(list) => {
                    let count = list.len();
                    self.components = list;
                    // Drop a selection that no longer exists.
                    if let Some(cur) = &self.current {
                        if !self.components.iter().any(|c| c.id == cur.id) {
                            self.current = None;
                        }
                    }
                    self.status = "ONLINE".into();
                    self.status_state = 1;
                    self.last_error = None;
                    logs.push((
                        format!("QID list loaded ({count} components)"),
                        EventLogKind::Info,
                    ));
                }
                super::db::DbResult::ComponentsFailed(error) => {
                    self.status = "ERROR".into();
                    self.status_state = 2;
                    self.last_error = Some(error.clone());
                    logs.push((format!("QID load failed: {error}"), EventLogKind::Info));
                }
                super::db::DbResult::SegmentsInserted { count } => {
                    self.status = "ONLINE".into();
                    self.status_state = 1;
                    logs.push((
                        format!("{count} QID segment(s) saved to database"),
                        EventLogKind::Record,
                    ));
                }
                super::db::DbResult::SegmentsFailed { error, segments } => {
                    self.status = "ERROR".into();
                    self.status_state = 2;
                    self.last_error = Some(error.clone());
                    let sidecar = segments
                        .first()
                        .and_then(|s| sidecar_path(&s.recording_path))
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    logs.push((
                        format!(
                            "QID segment insert failed: {error} \u{2014} sidecar copy at {sidecar}"
                        ),
                        EventLogKind::Info,
                    ));
                }
            }
        }
        (self.status == "LOADING" || self.status == "SAVING", logs)
    }

    /// Append the already-closed segments to the sidecar (called per QID
    /// link end so a crash mid-recording keeps every closed span).
    fn append_sidecar(&mut self, recording_path: &str) {
        if self.segments.is_empty() {
            return;
        }
        if let Err(e) = write_sidecar(recording_path, &self.segments) {
            eprintln!("[QID] sidecar append failed: {e}");
        }
    }
}

impl std::ops::Deref for QidService {
    type Target = QidState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl std::ops::DerefMut for QidService {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl RobsController {
    /// Request a fresh QID list from the database (no-op when unconfigured).
    pub fn refresh_qids(&mut self) {
        if let Some(message) = self.qid.refresh_qids() {
            self.log_event(message, EventLogKind::Info);
        }
    }

    /// QID rail click: toggle the clicked QID's recording link — open it
    /// when idle, end it when already recording (other links continue).
    /// Clicks only mark while a recording runs — idle clicks are ignored
    /// (the rail shows this via the default cursor and no hover highlight,
    /// see `qid_rail.slint`).
    pub fn qid_select(&mut self, component_id: i64) {
        if !self.record.recording {
            return;
        }
        let anchor = self.now_anchor();
        let path = self.record.last_recording_path.clone();
        for (message, kind) in self.qid.select(component_id, anchor, &path) {
            self.log_event(message, kind);
        }
    }

    /// Current segment anchors from the recording clock (elapsed
    /// milliseconds exclude paused spans; the frame count is the file
    /// position — see the module docs).
    fn now_anchor(&self) -> SegmentAnchor {
        SegmentAnchor {
            wall: chrono::Utc::now(),
            elapsed_ms: self.record.recording_time,
            frame: self.record.frame_count,
        }
    }

    /// Open a segment for the already-selected QID at recording start.
    pub(crate) fn start_qid_session(&mut self) {
        self.qid.start_session(self.now_anchor());
    }

    /// Auto-close every open segment at the final recording anchors, write
    /// the sidecar JSON, and hand all segments to the DB worker. Called
    /// from `stop_recording` BEFORE the elapsed-time reset.
    pub(crate) fn stop_qid_session(&mut self) {
        let anchor = self.now_anchor();
        let path = self.record.last_recording_path.clone();
        for (message, kind) in self.qid.stop_session(anchor, &path) {
            self.log_event(message, kind);
        }
    }

    /// Drain DB worker results; returns `true` while a load or save is
    /// still in flight (the caller keeps ticking so the result surfaces
    /// promptly).
    pub(crate) fn drain_qid_db_events(&mut self) -> bool {
        let (busy, logs) = self.qid.drain_db_events();
        for (message, kind) in logs {
            self.log_event(message, kind);
        }
        busy
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
            structure_id: 211,
            q_id: qid.into(),
            id_no: "Face A1-A2".into(),
            code: "ANODE".into(),
        }
    }

    /// Temp dir + a fake recording path inside it (the sidecar is really
    /// written next to it on every link close).
    fn temp_recording(tag: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "robs_qid_tests_{}_{}",
            std::process::id(),
            tag
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let rec = dir.join("rec.mp4").to_string_lossy().into_owned();
        (dir, rec)
    }

    fn service_with(components: &[QidComponent]) -> QidService {
        let mut service = QidService::new(DatabaseSettings::default());
        service.components = components.to_vec();
        service
    }

    #[test]
    fn first_click_opens_without_closing() {
        let clicked = comp(1, "QID-2210-HP-100");
        match apply_qid_click(&[], clicked.clone(), anchor(100, 5_000, 150), "rec.mp4") {
            QidClickAction::Opened(c) => assert_eq!(c, clicked),
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn second_click_opens_alongside_the_first() {
        let first = comp(1, "QID-2210-HP-100");
        let open = vec![OpenSegment { component: first, start: anchor(100, 0, 0) }];
        let second = comp(2, "QID-2210-HP-101");
        match apply_qid_click(
            &open,
            second.clone(),
            anchor(160, 60_000, 1_800),
            "rec.mp4",
        ) {
            QidClickAction::Opened(c) => assert_eq!(c, second),
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn clicking_open_qid_closes_only_it() {
        let first = comp(1, "QID-2210-HP-100");
        let second = comp(2, "QID-2210-HP-101");
        let open = vec![
            OpenSegment { component: first, start: anchor(100, 0, 0) },
            OpenSegment { component: second.clone(), start: anchor(160, 60_000, 1_800) },
        ];
        match apply_qid_click(&open, second, anchor(220, 120_000, 3_600), "rec.mp4") {
            QidClickAction::Closed(closed) => {
                assert_eq!(closed.component_id, 2);
                assert_eq!(closed.structure_id, 211);
                assert_eq!(closed.wall_start.timestamp(), 160);
                assert_eq!(closed.wall_end.timestamp(), 220);
                assert_eq!(closed.elapsed_start_ms, 60_000);
                assert_eq!(closed.elapsed_end_ms, 120_000);
                assert_eq!(closed.frame_start, 1_800);
                assert_eq!(closed.frame_end, 3_600);
                assert_eq!(closed.recording_path, "rec.mp4");
            }
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn service_keeps_independent_links_and_stop_closes_all() {
        let (dir, rec) = temp_recording("independent");
        let mut service = service_with(&[
            comp(1, "QID-2210-HP-100"),
            comp(2, "QID-2210-HP-101"),
            comp(3, "QID-2210-HP-102"),
        ]);

        // Open two concurrent links.
        let logs = service.select(1, anchor(100, 0, 0), &rec);
        assert!(logs[0].0.contains("QID marking started"), "{}", logs[0].0);
        service.select(2, anchor(160, 60_000, 1_800), &rec);
        assert_eq!(service.open.len(), 2);

        // End the first link only; the second keeps recording.
        let logs = service.select(1, anchor(220, 120_000, 3_600), &rec);
        assert!(logs[0].0.contains("QID marking ended"), "{}", logs[0].0);
        assert_eq!(service.open.len(), 1);
        assert_eq!(service.open[0].component.id, 2);
        assert_eq!(service.segments.len(), 1);
        assert_eq!(service.segments[0].component_id, 1);

        // Stop auto-closes the remaining link at the FINAL anchors (its own
        // start preserved) and writes both segments to the sidecar.
        service.stop_session(anchor(300, 180_000, 5_400), &rec);
        assert!(service.open.is_empty());
        let sidecar = sidecar_path(&rec).expect("sidecar path");
        let written: Vec<QidSegment> =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("sidecar read"))
                .expect("sidecar json");
        assert_eq!(written.len(), 2);
        let first_seg = written.iter().find(|s| s.component_id == 1).expect("seg 1");
        assert_eq!(first_seg.elapsed_start_ms, 0);
        assert_eq!(first_seg.elapsed_end_ms, 120_000);
        assert_eq!(first_seg.frame_end, 3_600);
        let second_seg = written.iter().find(|s| s.component_id == 2).expect("seg 2");
        assert_eq!(second_seg.elapsed_start_ms, 60_000);
        assert_eq!(second_seg.elapsed_end_ms, 180_000);
        assert_eq!(second_seg.frame_start, 1_800);
        assert_eq!(second_seg.frame_end, 5_400);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn armed_selection_auto_opens_at_next_session_start() {
        let (dir, rec) = temp_recording("armed");
        let mut service = service_with(&[comp(7, "QID-2210-HP-100")]);

        service.select(7, anchor(100, 0, 0), &rec);
        service.stop_session(anchor(140, 40_000, 1_200), &rec);
        assert!(service.open.is_empty());

        // The last click stays armed across the session boundary: the next
        // recording auto-opens its segment at the start anchors.
        service.start_session(anchor(200, 0, 0));
        assert_eq!(service.open.len(), 1);
        assert_eq!(service.open[0].component.id, 7);
        assert_eq!(service.open[0].start.elapsed_ms, 0);
        assert_eq!(service.open[0].start.frame, 0);

        let _ = std::fs::remove_dir_all(&dir);
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
