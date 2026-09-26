//! ROBS Slint shell (Phase 1): the window frame, scenes rail, live preview
//! canvas, Quick Actions bar, and event-log strip.
//!
//! The glue owns the tick timer: a `slint::Timer` in SingleShot mode that
//! calls [`RobsController::tick`], pushes a state snapshot into the frozen
//! `Api` global, and re-arms itself with exactly the wake duration the engine
//! asked for (or a 250 ms idle tick so chat/log/status stay responsive).
//! Callbacks and the timer both run on the UI thread, so the controller is
//! shared behind `Rc<RefCell<..>>` — no locks.

mod canvas_glue;
mod panels_glue;
mod push;
mod qid_glue;
mod sources_glue;
mod telemetry_glue;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use robs_controller::state::EventLogKind;
use robs_controller::RobsController;
use slint::{ComponentHandle, Timer, TimerMode};

slint::include_modules!();

/// Idle tick used when the engine reports no pending wake.
const IDLE_TICK: Duration = Duration::from_millis(250);

// The bundled Nunito typeface is embedded and registered at compile time
// via TTF imports in `ui/mainwindow.slint` (Slint 1.17 has no public
// pre-component font-registration function).

/// Build the window, wire the Api commands to controller operations, start
/// the tick timer, and run the Slint event loop.
pub fn run(controller: RobsController) -> Result<(), slint::PlatformError> {
    let component = MainWindow::new()?;
    let api = component.global::<Api>();

    let controller = Rc::new(RefCell::new(controller));

    // ---- Commands (port of the old quick-action click handlers) ----
    {
        let controller = Rc::clone(&controller);
        api.on_start_pause_resume_record(move || {
            let (recording, paused) = {
                let controller = controller.borrow();
                (
                    controller.record.recording,
                    controller.record.recording_paused,
                )
            };
            let mut controller = controller.borrow_mut();
            if !recording {
                // START requires at least one source in the scene (enforced
                // by the button's enabled state; keep the guard anyway).
                if controller.scene_has_sources() {
                    controller.start_recording();
                }
            } else if paused {
                controller.record.recording_paused = false;
                controller.log_event("Recording resumed", EventLogKind::Record);
            } else {
                controller.record.recording_paused = true;
                controller.log_event("Recording paused", EventLogKind::Record);
            }
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_stop_all(move || {
            let mut controller = controller.borrow_mut();
            // STOP halts whichever encoder is live (recording and/or the
            // stream; they run as separate FFmpeg processes and are stopped
            // independently).
            if controller.record.recording {
                controller.stop_recording();
            }
            if controller.streaming {
                controller.stop_streaming();
            }
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_toggle_stream(move || {
            let streaming = controller.borrow().streaming;
            let paused = controller.borrow().streaming_paused;
            let mut controller = controller.borrow_mut();
            if !streaming {
                // Spawns the RTMP FFmpeg; on a missing server/key it logs
                // guidance and stays off.
                controller.start_streaming();
            } else if paused {
                controller.streaming_paused = false;
                controller.log_event("Streaming resumed", EventLogKind::Stream);
            } else {
                controller.streaming_paused = true;
                controller.log_event("Streaming paused", EventLogKind::Stream);
            }
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_snapshot(move || {
            controller.borrow_mut().take_snapshot = true;
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_toggle_clip_mark(move || {
            controller.borrow_mut().toggle_clip_mark();
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_bookmark(move || {
            controller.borrow_mut().log_event("Bookmark added", EventLogKind::Info);
        });
    }
    {
        let controller = Rc::clone(&controller);
        api.on_select_scene(move |name: slint::SharedString| {
            controller.borrow_mut().scenes.set_current_scene(&name);
        });
    }

    // ---- Phase 2 panels: scenes rail / sources / overlays / properties ----
    let sources = Rc::new(RefCell::new(sources_glue::SourcesUi::new()));
    sources_glue::install(&component.as_weak(), &controller, &sources);

    // ---- Scene manager dialog: rename / remove by name. Add reuses the
    // `SourcesApi.add-scene` command the old Scenes tab drove. ----
    {
        let ctl = Rc::clone(&controller);
        let scene_api = component.global::<SceneApi>();
        scene_api.on_rename_scene(move |old: slint::SharedString, new: slint::SharedString| {
            let new = new.trim().to_string();
            if new.is_empty() {
                return;
            }
            let mut c = ctl.borrow_mut();
            if c.scenes.rename(&old, &new) {
                c.log_event(format!("Scene renamed: \"{old}\" -> \"{new}\""), EventLogKind::Info);
            }
        });
        let ctl = Rc::clone(&controller);
        let scene_api = component.global::<SceneApi>();
        scene_api.on_remove_scene(move |name: slint::SharedString| {
            let mut c = ctl.borrow_mut();
            if c.scenes.count() <= 1 {
                return;
            }
            let was_current = c.scenes.current_scene_name() == Some(name.as_str());
            if c.scenes.remove(&name) {
                if was_current {
                    // Mirror the old Scenes-tab remove: land on the first
                    // remaining scene (the pushed list is sorted).
                    let mut names: Vec<String> =
                        c.scenes.list().iter().map(|s| s.to_string()).collect();
                    names.sort();
                    if let Some(first) = names.first() {
                        c.scenes.set_current_scene(first);
                    }
                }
                c.log_event(format!("Scene removed: \"{name}\""), EventLogKind::Info);
            }
        });
    }

    // ---- Phase 2 panels: mixer / chat / stats / event log / settings / menu ----
    let panels = Rc::new(RefCell::new(panels_glue::PanelsUi::new()));
    panels_glue::install(&component.as_weak(), &controller, &panels);

    // ---- Data-string telemetry: bar callbacks + dialog ----
    telemetry_glue::install(&component.as_weak(), &controller);

    // ---- QID rail: Postgres component list + click-to-mark segments ----
    let qid_ui = Rc::new(RefCell::new(qid_glue::QidUi::new()));
    qid_glue::install(&component.as_weak(), &controller, &qid_ui);

    // ---- Phase 3: canvas editing (items + annotations + overlays) ----
    // `pushed` is created before the canvas install: the pointer callbacks
    // read `pushed.canvas` (refreshed every tick by `push_state`) to convert
    // canvas-relative pixels to scene coordinates.
    let pushed = Rc::new(RefCell::new(push::PushedState::new()));
    let canvas = Rc::new(RefCell::new(canvas_glue::CanvasUi::new()));
    canvas_glue::install(&component.as_weak(), &controller, &canvas, &pushed);

    // ---- Models + initial paint before the first tick ----
    api.set_scene_items(pushed.borrow().scene_items.clone());
    api.set_item_frames(pushed.borrow().item_frames.clone());
    push::push_state(&component, &mut controller.borrow_mut(), &mut pushed.borrow_mut());
    sources_glue::push(&component, &mut controller.borrow_mut(), &mut sources.borrow_mut());
    panels_glue::push(&component, &mut controller.borrow_mut(), &mut panels.borrow_mut());
    telemetry_glue::push(&component, &controller.borrow());
    qid_glue::push(&component, &controller.borrow(), &mut qid_ui.borrow_mut());

    // ---- Tick timer: SingleShot, re-armed with the engine's wake hint ----
    // A `slint::Timer` cannot be re-armed with a NEW interval from inside its
    // own callback (`restart()` only replays the previous one), so two timers
    // ping-pong: the timer that just fired schedules the other one with
    // exactly the engine's wake duration (or the idle tick when the engine
    // has no pending wake).
    let timer_a = Rc::new(Timer::default());
    let timer_b = Rc::new(Timer::default());
    {
        let component = component.as_weak();
    arm_tick(
        &timer_a,
        &timer_b,
        &component,
        &controller,
        &pushed,
        &sources,
        &panels,
        &qid_ui,
        IDLE_TICK,
    );
    }

    component.run()
}

/// Arm one half of the ping-pong tick-timer pair. When it fires, it drives
/// one engine tick, pushes the state snapshot, and schedules the other timer
/// with the returned wake duration.
fn arm_tick(
    timer: &Rc<Timer>,
    next: &Rc<Timer>,
    component: &slint::Weak<MainWindow>,
    controller: &Rc<RefCell<RobsController>>,
    pushed: &Rc<RefCell<push::PushedState>>,
    sources: &Rc<RefCell<sources_glue::SourcesUi>>,
    panels: &Rc<RefCell<panels_glue::PanelsUi>>,
    qid: &Rc<RefCell<qid_glue::QidUi>>,
    delay: Duration,
) {
    let this = Rc::clone(timer);
    let this_for_closure = Rc::clone(timer);
    let next = Rc::clone(next);
    let component = component.clone();
    let controller = Rc::clone(controller);
    let pushed = Rc::clone(pushed);
    let sources = Rc::clone(sources);
    let panels = Rc::clone(panels);
    let qid = Rc::clone(qid);
    this.start(TimerMode::SingleShot, delay, move || {
        let wake = controller.borrow_mut().tick();
        if let Some(component) = component.upgrade() {
            push::push_state(&component, &mut controller.borrow_mut(), &mut pushed.borrow_mut());
            sources_glue::push(&component, &mut controller.borrow_mut(), &mut sources.borrow_mut());
            panels_glue::push(&component, &mut controller.borrow_mut(), &mut panels.borrow_mut());
            telemetry_glue::push(&component, &controller.borrow());
            qid_glue::push(&component, &controller.borrow(), &mut qid.borrow_mut());
        }
        arm_tick(
            &next,
            &this_for_closure,
            &component,
            &controller,
            &pushed,
            &sources,
            &panels,
            &qid,
            wake.unwrap_or(IDLE_TICK),
        );
    });
}
