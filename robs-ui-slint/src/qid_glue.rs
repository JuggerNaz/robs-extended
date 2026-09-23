//! QID rail glue: wires the `QidApi` commands to the controller's QID engine
//! (`robs-controller/src/qid.rs`) and pushes the filtered component list plus
//! the CURRENT QID panel state into the rail every tick. Follows the same
//! borrow idioms as `telemetry_glue` (callbacks take
//! `Rc<RefCell<RobsController>>`).

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Local;
use robs_controller::qid::{qid_matches, QidComponent};
use robs_controller::state::EventLogKind;
use robs_controller::RobsController;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{MainWindow, QidApi, QidRowView};

/// Placeholder for the "START —" slot when no segment is open.
const EMPTY: &str = "—";

/// UI-side state for the QID rail: the live search filter (lowercased;
/// matches `q_id`, `id_no`, and `code` — see `qid::qid_matches`).
pub struct QidUi {
    filter: String,
}

impl QidUi {
    pub fn new() -> Self {
        Self { filter: String::new() }
    }
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

    // ---- Info button: log the current QID (and mark start) to the event log
    {
        let controller = Rc::clone(controller);
        api.on_details(move || {
            let mut controller = controller.borrow_mut();
            let Some(current) = controller.qid.current.clone() else {
                return;
            };
            let sub = sub_label(&current).to_string();
            let msg = match controller.qid.open.as_ref() {
                Some(open) => format!(
                    "QID {} ({}) — marking since {}",
                    current.q_id,
                    sub,
                    open.start.wall.with_timezone(&Local).format("%H:%M:%S")
                ),
                None if controller.record.recording => {
                    format!("QID {} ({}) — no open segment", current.q_id, sub)
                }
                None => format!(
                    "QID {} ({}) — selected; marking starts with the next recording",
                    current.q_id, sub
                ),
            };
            controller.log_event(msg, EventLogKind::Info);
        });
    }
}

/// Push the snapshot into the rail: filtered rows, header count, CURRENT QID
/// panel, and the DB status chip.
pub fn push(component: &MainWindow, controller: &RobsController, qid_ui: &QidUi) {
    let api = component.global::<QidApi>();
    let qid = &controller.qid;

    let rows: Vec<QidRowView> = qid
        .components
        .iter()
        .filter(|c| qid_matches(c, &qid_ui.filter))
        .map(|c| QidRowView {
            id: c.id as i32,
            q_id: c.q_id.as_str().into(),
            sub: sub_label(c).into(),
            current: qid.current.as_ref().map(|cur| cur.id) == Some(c.id),
        })
        .collect();
    api.set_rows(ModelRc::new(VecModel::from(rows)));
    // Header count shows the TOTAL loaded rows, not the filtered view.
    api.set_total(qid.components.len() as i32);

    let current = qid.current.as_ref();
    api.set_has_current(current.is_some());
    api.set_current_qid(current.map(|c| c.q_id.as_str().into()).unwrap_or_default());
    api.set_current_sub(
        current.map(|c| SharedString::from(sub_label(c))).unwrap_or_default(),
    );
    api.set_current_start(
        qid.open
            .as_ref()
            .map(|s| s.start.wall.with_timezone(&Local).format("%H:%M:%S").to_string())
            .unwrap_or_else(|| EMPTY.to_string())
            .into(),
    );

    api.set_db_status(qid.status.as_str().into());
    api.set_db_status_state(qid.status_state);
    // Rows are clickable only while a recording runs (cursor/hover follow).
    api.set_can_select(controller.record.recording);
}
