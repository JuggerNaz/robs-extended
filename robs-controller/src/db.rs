//! Postgres access for the QID rail (web-app-offshore's Supabase DB).
//!
//! A dedicated worker thread owns the `postgres::Client` — the UI thread
//! only sends commands over an mpsc channel and drains results each tick
//! (the same idiom as the telemetry reader and the clip-export thread; the
//! sync `postgres` client would otherwise block the Slint event loop).
//!
//! Connection settings come from `robs_profiles::settings::DatabaseSettings`
//! (the `database` section of `settings.json`). The expected URL is the
//! Supabase **session pooler** (`...pooler.supabase.com:5432/postgres`,
//! user `postgres.<project-ref>`): TLS is mandatory, the host is
//! IPv4-friendly, and session mode keeps prepared statements working — the
//! transaction pooler (6543) does not. TLS uses `native-tls` (SChannel on
//! Windows, no OpenSSL toolchain needed).
//!
//! Failures never panic the worker: every error is reported back as a
//! `DbResult::*Failed` for the event log, and the client is dropped so the
//! next command starts a fresh connection (one retry per command is the
//! natural outcome of the lazy-connect-per-batch design).
//!
//! Note: the worker thread has no stop signal; it lives for the process
//! lifetime and ends with it. Segment inserts are sent at recording stop,
//! so an immediate app exit can in principle drop the in-flight insert —
//! the sidecar JSON next to the recording (see `qid.rs`) is the recovery
//! copy in that case.

use std::sync::mpsc::{self, Receiver, Sender};

use postgres::Client;
use postgres_native_tls::MakeTlsConnector;

use robs_profiles::settings::DatabaseSettings;

use super::qid::{QidComponent, QidSegment};

/// One command for the worker thread.
pub enum DbCommand {
    /// Reload `structure_components` for `structure_id` (active rows only).
    LoadComponents { structure_id: i32 },
    /// Insert closed QID segments of one recording session (single transaction).
    InsertSegments { structure_id: i32, segments: Vec<QidSegment> },
}

/// One outcome from the worker thread, drained by the UI tick.
pub enum DbResult {
    /// QID list loaded, ordered by `q_id`.
    Components(Vec<QidComponent>),
    ComponentsFailed(String),
    /// `n` segments persisted.
    SegmentsInserted { count: usize },
    SegmentsFailed { error: String, segments: Vec<QidSegment> },
}

/// Handle to the worker: send commands, drain results.
pub struct DbWorker {
    pub tx: Sender<DbCommand>,
    pub rx: Receiver<DbResult>,
}

/// Spawn the worker thread. Commands sent before any successful connection
/// trigger a fresh connect attempt — there is no separate "connect" step.
pub fn spawn(settings: DatabaseSettings) -> DbWorker {
    let (cmd_tx, cmd_rx) = mpsc::channel::<DbCommand>();
    let (res_tx, res_rx) = mpsc::channel::<DbResult>();

    std::thread::Builder::new()
        .name("qid-db".into())
        .spawn(move || worker_loop(settings, cmd_rx, res_tx))
        .expect("spawn qid-db worker thread");

    DbWorker { tx: cmd_tx, rx: res_rx }
}

/// Worker main loop: take commands one at a time, run each against a
/// lazily (re)connected client. A failed connect or a failed command drops
/// the client so the next command starts a fresh connection.
fn worker_loop(
    settings: DatabaseSettings,
    cmd_rx: Receiver<DbCommand>,
    res_tx: Sender<DbResult>,
) {
    let mut client: Option<Client> = None;
    while let Ok(command) = cmd_rx.recv() {
        if client.is_none() {
            match connect(&settings) {
                Ok(conn) => client = Some(conn),
                Err(error) => {
                    report_failure(&res_tx, &command, error);
                    continue;
                }
            }
        }
        // `expect` is safe: the match above guarantees `Some` here.
        let conn = client.as_mut().expect("db client connected above");
        if run_command(conn, command, &res_tx) {
            client = None; // command failed — reconnect on the next one
        }
    }
}

/// Run one command against a live client, sending the outcome. Returns
/// `true` when the command failed (the connection is suspect and should be
/// dropped).
fn run_command(
    client: &mut Client,
    command: DbCommand,
    res_tx: &Sender<DbResult>,
) -> bool {
    let outcome = match &command {
        DbCommand::LoadComponents { structure_id } => load_components(client, *structure_id)
            .map(DbResult::Components)
            .unwrap_or_else(DbResult::ComponentsFailed),
        DbCommand::InsertSegments { structure_id, segments } => {
            insert_segments(client, *structure_id, segments)
                .map(|count| DbResult::SegmentsInserted { count })
                .unwrap_or_else(|e| DbResult::SegmentsFailed {
                    error: e,
                    segments: segments.clone(),
                })
        }
    };
    let failed = matches!(
        outcome,
        DbResult::ComponentsFailed(_) | DbResult::SegmentsFailed { .. }
    );
    res_tx.send(outcome).ok();
    failed
}

fn report_failure(res_tx: &Sender<DbResult>, command: &DbCommand, error: String) {
    let result = match command {
        DbCommand::LoadComponents { .. } => DbResult::ComponentsFailed(error),
        DbCommand::InsertSegments { segments, .. } => DbResult::SegmentsFailed {
            error,
            segments: segments.clone(),
        },
    };
    res_tx.send(result).ok();
}

/// Open a TLS Postgres connection to the configured URL.
fn connect(settings: &DatabaseSettings) -> Result<Client, String> {
    let url = settings.url.trim();
    if url.is_empty() {
        return Err("database URL not configured (settings.json `database.url`)".into());
    }
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| format!("TLS setup failed: {e}"))?;
    Client::connect(url, MakeTlsConnector::new(connector)).map_err(|e| {
        format!(
            "connect failed: {e} (check settings.json database.url — session pooler :5432, user postgres.<ref>)"
        )
    })
}

/// `SELECT` the active `structure_components` rows for one structure,
/// ordered by `q_id` (mirrors web-app-offshore's structure-components API).
fn load_components(client: &mut Client, structure_id: i32) -> Result<Vec<QidComponent>, String> {
    let rows = client
        .query(
            "SELECT id, q_id, COALESCE(id_no, ''), COALESCE(code, '') \
             FROM structure_components \
             WHERE structure_id = $1 AND is_deleted = false \
             ORDER BY q_id",
            &[&structure_id],
        )
        .map_err(|e| format!("load components failed: {e}"))?;
    Ok(rows
        .iter()
        .map(|row| QidComponent {
            id: row.get::<_, i64>(0),
            q_id: row.get::<_, String>(1),
            id_no: row.get::<_, String>(2),
            code: row.get::<_, String>(3),
        })
        .collect())
}

/// Insert all segments of one recording session in a single transaction.
fn insert_segments(
    client: &mut Client,
    structure_id: i32,
    segments: &[QidSegment],
) -> Result<usize, String> {
    let mut tx = client
        .transaction()
        .map_err(|e| format!("transaction begin failed: {e}"))?;
    for segment in segments {
        tx.execute(
            "INSERT INTO robs_qid_segments \
             (structure_id, component_id, q_id, recording_path, \
              wall_start, wall_end, elapsed_start_ms, elapsed_end_ms, \
              frame_start, frame_end) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &(structure_id as i64),
                &segment.component_id,
                &segment.q_id,
                &segment.recording_path,
                &segment.wall_start,
                &segment.wall_end,
                &(segment.elapsed_start_ms as i64),
                &(segment.elapsed_end_ms as i64),
                &(segment.frame_start as i64),
                &(segment.frame_end as i64),
            ],
        )
        .map_err(|e| format!("insert segment failed: {e}"))?;
    }
    tx.commit().map_err(|e| format!("commit failed: {e}"))?;
    Ok(segments.len())
}
