//! ROBS Slint shell (Phase 1): the window frame, scenes rail, live preview
//! canvas, Quick Actions bar, and event-log strip.
//!
//! The glue owns the tick timer: a `slint::Timer` in SingleShot mode that
//! calls [`RobsController::tick`], pushes a state snapshot into the frozen
//! `Api` global, and re-arms itself with exactly the wake duration the engine
//! asked for (or a 250 ms idle tick so chat/log/status stay responsive).
//! Callbacks and the timer both run on the UI thread, so the controller is
//! shared behind `Rc<RefCell<..>>` — no locks.

mod push;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use robs_controller::state::EventLogKind;
use robs_controller::RobsController;
use slint::{ComponentHandle, Timer, TimerMode};

slint::include_modules!();

/// Idle tick used when the engine reports no pending wake.
const IDLE_TICK: Duration = Duration::from_millis(250);

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

    // ---- Models + initial paint before the first tick ----
    let pushed = Rc::new(RefCell::new(push::PushedState::new()));
    api.set_scene_items(pushed.borrow().scene_items.clone());
    api.set_item_frames(pushed.borrow().item_frames.clone());
    push::push_state(&component, &mut controller.borrow_mut(), &mut pushed.borrow_mut());

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
    delay: Duration,
) {
    let this = Rc::clone(timer);
    let this_for_closure = Rc::clone(timer);
    let next = Rc::clone(next);
    let component = component.clone();
    let controller = Rc::clone(controller);
    let pushed = Rc::clone(pushed);
    this.start(TimerMode::SingleShot, delay, move || {
        let wake = controller.borrow_mut().tick();
        if let Some(component) = component.upgrade() {
            push::push_state(&component, &mut controller.borrow_mut(), &mut pushed.borrow_mut());
        }
        arm_tick(
            &next,
            &this_for_closure,
            &component,
            &controller,
            &pushed,
            wake.unwrap_or(IDLE_TICK),
        );
    });
}
