//! Telemetry (bottom data-string bar) glue: wires the `TelemetryApi` commands
//! to the controller's telemetry engine and pushes the latest snapshot into
//! the bar's slots every tick. Follows the same borrow idioms as
//! `panels_glue` (callbacks take `Rc<RefCell<RobsController>>`).

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Local;
use robs_controller::telemetry::available_serial_ports;
use robs_controller::RobsController;
use robs_profiles::settings::SerialTelemetrySettings;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{MainWindow, TelemetryApi};

/// Baud choices offered in the dialog; indexes address this table.
const BAUD_CHOICES: [u32; 6] = [4800, 9600, 19200, 38400, 57600, 115200];

/// Missing-value placeholder for empty bar slots.
const EMPTY: &str = "—";

pub fn install(component: &slint::Weak<MainWindow>, controller: &Rc<RefCell<RobsController>>) {
    let Some(comp) = component.upgrade() else {
        return;
    };
    // `api` borrows `comp`; both stay alive for the rest of this function.
    let api = comp.global::<TelemetryApi>();

    // ---- Gear button: seed the dialog from the engine, list the ports ----
    {
        let component = component.clone();
        let controller = Rc::clone(controller);
        api.on_open_dialog(move || {
            let Some(comp) = component.upgrade() else {
                return;
            };
            let api = comp.global::<TelemetryApi>();
            let ports = available_serial_ports();
            api.set_available_ports(ModelRc::new(VecModel::from(
                ports.iter().map(|p| SharedString::from(p.as_str())).collect::<Vec<_>>(),
            )));
            let controller = controller.borrow();
            api.set_cfg_port(controller.telemetry.settings.port.as_str().into());
            api.set_cfg_baud_index(
                BAUD_CHOICES
                    .iter()
                    .position(|&b| b == controller.telemetry.settings.baud)
                    .unwrap_or(1) as i32,
            );
            api.set_dialog_open(true);
        });
    }

    // ---- SAVE & CLOSE / click-away: persist port+baud, restart if live ----
    {
        let component = component.clone();
        let controller = Rc::clone(controller);
        api.on_dialog_apply(move |port: SharedString, baud_index: i32| {
            let port = port.trim().to_string();
            let baud = BAUD_CHOICES
                .get(baud_index.max(0) as usize)
                .copied()
                .unwrap_or(9600);
            let was_running;
            {
                let mut controller = controller.borrow_mut();
                was_running = controller.telemetry.running;
                if !port.is_empty() {
                    controller.telemetry.settings = SerialTelemetrySettings {
                        port,
                        baud,
                        // `enabled` mirrors "was live", so a manual stop stays
                        // stopped across relaunches.
                        enabled: was_running,
                    };
                } else {
                    controller.telemetry.settings.baud = baud;
                }
                let _ = controller.telemetry.settings.save();
                if was_running {
                    // Restart the reader so the new settings take effect now.
                    controller.start_telemetry();
                }
            }
            if let Some(comp) = component.upgrade() {
                comp.global::<TelemetryApi>().set_dialog_open(false);
            }
        });
    }

    // ---- START / STOP in the dialog (persists across relaunches) ----
    {
        let controller = Rc::clone(controller);
        api.on_toggle_running(move || {
            let mut controller = controller.borrow_mut();
            if controller.telemetry.running {
                controller.stop_telemetry();
                controller.telemetry.settings.enabled = false;
            } else {
                controller.start_telemetry();
                controller.telemetry.settings.enabled = true;
            }
            let _ = controller.telemetry.settings.save();
        });
    }
}

/// Push the snapshot into the bar: status word/dot plus the nine slots
/// (units per the mockup; missing fields show an em dash).
pub fn push(component: &MainWindow, controller: &RobsController) {
    let api = component.global::<TelemetryApi>();
    let telemetry = &controller.telemetry;
    api.set_running(telemetry.running);

    let snap = telemetry.snapshot.read();
    let receiving = snap
        .last_line_at
        .map(|t| (Local::now() - t).num_milliseconds().abs() < 3000)
        .unwrap_or(false);
    let (status, state): (&str, i32) = if !telemetry.running {
        ("STOPPED", 0)
    } else if snap.port_open && receiving {
        ("RUNNING", 1)
    } else if snap.port_open {
        ("NO DATA", 2)
    } else {
        ("RECONNECTING", 2)
    };
    api.set_status(status.into());
    api.set_status_state(state);

    let field = |key: &str| -> Option<String> {
        snap.fields.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
    };
    let with_unit = |raw: Option<String>, unit: &str| -> SharedString {
        raw.map(|v| format!("{v}{unit}").into()).unwrap_or_else(|| EMPTY.into())
    };

    api.set_easting(with_unit(field("E"), " m"));
    api.set_northing(with_unit(field("N"), " m"));
    // The primary format carries no depth; a `Z` key (legacy formats) fills
    // the slot when present.
    api.set_depth(with_unit(field("Z"), " m"));
    api.set_date(field("D").map(Into::into).unwrap_or_else(|| EMPTY.into()));
    api.set_time(field("T").map(Into::into).unwrap_or_else(|| EMPTY.into()));
    api.set_heading(with_unit(field("H"), "°"));
    api.set_cp(with_unit(field("CP"), " mV"));
    api.set_fg(field("FG").map(Into::into).unwrap_or_else(|| EMPTY.into()));
    api.set_last_check(
        snap.last_line_at
            .map(|t| t.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| EMPTY.to_string())
            .into(),
    );
}
