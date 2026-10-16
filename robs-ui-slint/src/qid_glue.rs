//! QID rail glue: wires the `QidApi` commands to the controller's QID engine
//! (`robs-controller/src/qid.rs`) and pushes the filtered component list plus
//! the RECORDING panel state (every open link) into the rail every tick.
//! Follows the same borrow idioms as `telemetry_glue` (callbacks take
//! `Rc<RefCell<RobsController>>`).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use chrono::{DateTime, Local, Utc};
use robs_controller::qid::{qid_matches, QidComponent};
use robs_controller::state::EventLogKind;
use robs_controller::RobsController;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{MainWindow, QidApi, QidOpenView, QidRowView};

/// UI-side state for the QID rail: the live search filter (lowercased;
/// matches `q_id`, `id_no`, and `code` — see `qid::qid_matches`) plus the
/// persistent rows model.
pub struct QidUi {
    filter: String,
    /// The rows model installed as `QidApi.rows`, updated IN PLACE per tick.
    /// Installing a fresh `ModelRc` every push would reset the ListView and
    /// recreate every row delegate — and their TouchAreas — at tick rate
    /// (~30x/s while recording), swallowing clicks: the press lands on a
    /// delegate that no longer exists at release, so `clicked` never fires.
    /// Same idiom as `push::PushedState`'s in-place models: rows are updated
    /// per-index and the model is only rebuilt wholesale when the row id
    /// SEQUENCE changes (search filter edit, DB reload).
    rows: Rc<VecModel<QidRowView>>,
    /// Owned copy of the rows currently in `rows`, for change detection.
    rows_mirror: Vec<QidRowView>,
    /// The RECORDING panel's open-link rows — same persistent-model idiom as
    /// `rows` (the panel entries carry TouchAreas; rebuilding the model every
    /// tick would recreate the delegates mid-click and swallow it).
    open_rows: Rc<VecModel<QidOpenView>>,
    open_mirror: Vec<QidOpenView>,
}

impl QidUi {
    pub fn new() -> Self {
        Self {
            filter: String::new(),
            rows: Rc::new(VecModel::from(Vec::new())),
            rows_mirror: Vec::new(),
            open_rows: Rc::new(VecModel::from(Vec::new())),
            open_mirror: Vec::new(),
        }
    }
}

/// True when a pushed row's visible fields differ from its mirrored copy.
fn row_differs(a: &QidRowView, b: &QidRowView) -> bool {
    a.id != b.id || a.recording != b.recording || a.q_id != b.q_id || a.sub != b.sub
}

/// Same check for the RECORDING panel's open-link rows.
fn open_row_differs(a: &QidOpenView, b: &QidOpenView) -> bool {
    a.id != b.id || a.q_id != b.q_id || a.sub != b.sub || a.start != b.start
}

/// Sub-label under the QID: `id_no`, falling back to `code`.
fn sub_label(c: &QidComponent) -> &str {
    if c.id_no.is_empty() {
        c.code.as_str()
    } else {
        c.id_no.as_str()
    }
}

pub fn install(
    component: &slint::Weak<MainWindow>,
    controller: &Rc<RefCell<RobsController>>,
    qid_ui: &Rc<RefCell<QidUi>>,
) {
    let Some(comp) = component.upgrade() else {
        return;
    };
    // `api` borrows `comp`; both stay alive for the rest of this function.
    let api = comp.global::<QidApi>();

    // ---- Row click: mark a segment (only meaningful while recording) ----
    {
        let controller = Rc::clone(controller);
        api.on_select_qid(move |id: i32| {
            let mut controller = controller.borrow_mut();
            if controller.record.recording {
                controller.qid_select(id as i64);
            }
        });
    }

    // ---- Refresh: reload the component list from the database ----
    {
        let controller = Rc::clone(controller);
        api.on_refresh(move || {
            controller.borrow_mut().refresh_qids();
        });
    }

    // ---- Search: store the filter; the next push shows only matches ----
    {
        let qid_ui = Rc::clone(qid_ui);
        api.on_search(move |text: SharedString| {
            qid_ui.borrow_mut().filter = text.trim().to_lowercase();
        });
    }

    // ---- Info button: log every open link (or the armed QID) to the event log
    {
        let controller = Rc::clone(controller);
        api.on_details(move || {
            let mut controller = controller.borrow_mut();
            let open: Vec<(String, String, String)> = controller
                .qid
                .open
                .iter()
                .map(|seg| {
                    (
                        seg.component.q_id.clone(),
                        sub_label(&seg.component).to_string(),
                        seg.start
                            .wall
                            .with_timezone(&Local)
                            .format("%H:%M:%S")
                            .to_string(),
                    )
                })
                .collect();
            if !open.is_empty() {
                for (q_id, sub, start) in open {
                    controller.log_event(
                        format!("QID {q_id} ({sub}) — recording since {start}"),
                        EventLogKind::Info,
                    );
                }
                return;
            }
            let Some(current) = controller.qid.current.clone() else {
                return;
            };
            let sub = sub_label(&current).to_string();
            let msg = if controller.record.recording {
                format!("QID {} ({}) — no open link", current.q_id, sub)
            } else {
                format!(
                    "QID {} ({}) — selected; marking starts with the next recording",
                    current.q_id, sub
                )
            };
            controller.log_event(msg, EventLogKind::Info);
        });
    }
}

/// Push the snapshot into the rail: filtered rows (with per-QID recording
/// highlights), header count, RECORDING panel, and the DB status chip.
pub fn push(component: &MainWindow, controller: &RobsController, qid_ui: &mut QidUi) {
    let api = component.global::<QidApi>();
    let qid = &controller.qid;
    let open_ids: HashSet<i64> = qid.open.iter().map(|s| s.component.id).collect();

    let rows: Vec<QidRowView> = qid
        .components
        .iter()
        .filter(|c| qid_matches(c, &qid_ui.filter))
        .map(|c| QidRowView {
            id: c.id as i32,
            q_id: c.q_id.as_str().into(),
            sub: sub_label(c).into(),
            recording: open_ids.contains(&c.id),
        })
        .collect();
    if qid_ui.rows_mirror.len() != rows.len()
        || qid_ui
            .rows_mirror
            .iter()
            .zip(rows.iter())
            .any(|(m, r)| m.id != r.id)
    {
        // Row set/identity changed (filter edit, DB reload): wholesale swap.
        qid_ui.rows.set_vec(rows.clone());
    } else {
        // Same rows: update only the changed indices in place so the
        // delegates (and their TouchAreas) survive the tick (see `QidUi::rows`).
        for (i, row) in rows.iter().enumerate() {
            if row_differs(&qid_ui.rows_mirror[i], row) {
                qid_ui.rows.set_row_data(i, row.clone());
            }
        }
    }
    qid_ui.rows_mirror = rows;
    api.set_rows(ModelRc::from(qid_ui.rows.clone()));
    // Header count shows the TOTAL loaded rows, not the filtered view.
    api.set_total(qid.components.len() as i32);

    // RECORDING panel: every open link, ordered by start time. Same
    // wholesale-swap vs in-place-update split as the main rows.
    let mut open_rows: Vec<(DateTime<Utc>, QidOpenView)> = qid
        .open
        .iter()
        .map(|seg| {
            (
                seg.start.wall,
                QidOpenView {
                    id: seg.component.id as i32,
                    q_id: seg.component.q_id.as_str().into(),
                    sub: sub_label(&seg.component).into(),
                    start: seg
                        .start
                        .wall
                        .with_timezone(&Local)
                        .format("%H:%M:%S")
                        .to_string()
                        .into(),
                },
            )
        })
        .collect();
    open_rows.sort_by_key(|(wall, _)| *wall);
    let open_views: Vec<QidOpenView> = open_rows.into_iter().map(|(_, view)| view).collect();
    if qid_ui.open_mirror.len() != open_views.len()
        || qid_ui
            .open_mirror
            .iter()
            .zip(open_views.iter())
            .any(|(m, r)| m.id != r.id)
    {
        qid_ui.open_rows.set_vec(open_views.clone());
    } else {
        for (i, view) in open_views.iter().enumerate() {
            if open_row_differs(&qid_ui.open_mirror[i], view) {
                qid_ui.open_rows.set_row_data(i, view.clone());
            }
        }
    }
    qid_ui.open_mirror = open_views;
    api.set_open_qids(ModelRc::from(qid_ui.open_rows.clone()));
    api.set_has_open(!qid.open.is_empty());
    api.set_open_count(qid.open.len() as i32);

    api.set_db_status(qid.status.as_str().into());
    api.set_db_status_state(qid.status_state);
    // Rows are clickable only while a recording runs (cursor/hover follow).
    api.set_can_select(controller.record.recording);
}
