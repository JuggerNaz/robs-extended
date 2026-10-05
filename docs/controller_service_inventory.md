# RobsController method inventory (Phase 2 — service decomposition)

Inventory of every `impl RobsController` method, the state cluster(s) it
touches, and the owning service it moves to. `F:` = facade-owned state
(scenes, flat settings/view fields, event log); cross-service reads become
explicit parameters on the service method. Signatures below are the target
shape after the refactor; `→ logs` means the method returns
`(String, EventLogKind)` pairs for the facade delegate to feed to
`log_event` in the original order.

Legend: services `Rec`(RecordService), `Str`(StreamService), `Prev`(PreviewService),
`Bb`(BlackboxService), `An`(AnomalyService), `Qid`(QidService), `Tel`(TelemetryService),
`Ovl`(OverlayService). Facade methods marked **delegate** stay on `RobsController`
as thin glue-facing wrappers.

## record.rs + clips.rs → RecordService (state: `RecordState`)

| Method | Touches | New home / signature |
|---|---|---|
| `scene_has_sources` | `scenes` (F) | stays on facade (view guard, glue-called) |
| `start_recording` | `record.*`, `recording_path/format` (F), `video_encoder` (F), `fps_setting/keyframe_interval/recording_bitrate` (F), `output_w/h` (F), `scenes` (F), `blackbox.*` (Bb), `anomaly.*` (An), `preview.last_output_frame` (Prev), qid session (Qid), `log_event` (F) | facade orchestrator (glue-called): builds `SessionConfig`, calls `Rec::plan_session`, does blackbox/anomaly redirect, `Rec::launch_ffmpeg(&CaptureTarget)`, `Rec::begin_session_state`, clears `preview.last_output_frame`, `Qid::start_session(anchor)`, logs |
| `stop_recording` | `record.*`, `fps_setting` (F), qid stop (Qid), `blackbox/anomaly.session_override` (Bb/An), `log_event` (F) | facade orchestrator (glue-called): `Rec::shutdown_pipeline → bool`, `Rec::auto_close_clip_marks → logs`, `Rec::take_clip_marks`, `Rec::start_clip_exports`, `Qid::stop_session(anchor)`, `Rec::reset_for_next_session → elapsed_ms`, file verify (F), engine-override release (F) |
| `toggle_clip_mark` | `record.clip_*`, `fps_setting` (F), `log_event` | `Rec::toggle_clip_mark(fps) → logs`; facade **delegate** (glue-called) |
| `start_clip_exports` | `record.clip_export_*` | `Rec::start_clip_exports(path, marks, fps)` (pub(crate)) |
| `drain_clip_export_events` | `record.clip_export_rx` | `Rec::drain_clip_export_events()`; facade **delegate** (handle_events) |
| `apply_clip_export_events` | `record.clip_export_pending`, `log_event` | `Rec::apply_clip_export_events → logs`; facade **delegate** (handle_events) |
| free fns `gop_size`, `date_folder_name`, `pick_session_dir` | none | unchanged (unit-tested pure helpers) |

## stream.rs → StreamService (state: `StreamState`)

| Method | Touches | New home / signature |
|---|---|---|
| `start_streaming` | `stream.*`, `streaming/_paused/_time`, `bitrate` (F), `stream_server/key/service/bitrate` (F), `video_encoder` (F), `output_w/h`, `fps_setting`, `keyframe_interval` (F), `log_event` (F) | `Str::start(&StreamStartParams) → Result<(), String>` (Err = full log line); facade **delegate** sets flat fields + logs (glue-called) |
| `stop_streaming` | `stream.*`, `streaming/_paused/_time` (F), `log_event` (F) | `Str::stop()`; facade **delegate** computes elapsed first, resets flat fields, logs (glue-called; also called from handle_events ffmpeg-death path) |
| free fn `build_rtmp_url` | none | unchanged (unit-tested pure helper) |

## blackbox.rs → BlackboxService (state: `BlackboxState`)

| Method | Touches | New home / signature |
|---|---|---|
| `sync_blackbox_engine` | `blackbox.*`, `output_w/h`, `fps_setting`, `recording_path` (F) | `Bb::sync(has_capture_source, OutputSpec, recording_path)`; facade **delegate** `sync_blackbox_engine(has_capture_source)` (tick) |
| `start_blackbox` (priv) | `blackbox.*` + config inputs (F) | `Bb::start(OutputSpec, recording_path)` (private) |
| `stop_blackbox` | `blackbox.engine/status` | `Bb::stop()`; facade **delegate** (glue-called + record start/stop) |
| `build_blackbox_config` (priv) | `blackbox.*`, `recording_path`, `output_w/h`, `fps_setting` (F) | `Bb::build_config(OutputSpec, recording_path)` (private) |
| `submit_blackbox_frame` | `blackbox.engine` | `Bb::submit_frame(data, w, h)`; facade **delegate** (capture.rs tap) |
| `drain_blackbox_events` | `blackbox.event_rx` | `Bb::drain_events()`; facade **delegate** (handle_events) |
| `apply_blackbox_events` | `blackbox.status`, `log_event` | `Bb::apply_events(events) → logs`; facade **delegate** (handle_events) |

`OutputSpec { output_width, output_height, fps }` (new, `state.rs`) is the
explicit cross-service parameter replacing reads of the flat facade fields.

## anomaly.rs → AnomalyService (state: `AnomalyState`)

| Method | Touches | New home / signature |
|---|---|---|
| `start_anomaly` | `anomaly.*` + config inputs (F) | `An::start(OutputSpec, recording_path)`; facade **delegate** (glue n/a; record start/stop) |
| `stop_anomaly` | `anomaly.*` | `An::stop()`; facade **delegate** |
| `save_anomaly_settings` | `anomaly.settings`, `log_event` | `An::save_settings() → Option<log>`; facade **delegate** (glue-called) |
| `build_anomaly_config` (priv) | `anomaly.*`, `recording_path`, `fps_setting` (F) | `An::build_config(OutputSpec, recording_path)` (private) |
| `submit_anomaly_frame` | `anomaly.engine` | `An::submit_frame(data, w, h)`; facade **delegate** (capture.rs tap) |
| `request_anomaly_clip` | `anomaly.*`, `log_event` | `An::request_clip() → Option<log>`; facade **delegate** (glue-called) |
| `drain_anomaly_events` | `anomaly.event_rx` | `An::drain_events()`; facade **delegate** (handle_events) |
| `apply_anomaly_events` | `anomaly.status`, `log_event` | `An::apply_events(events) → logs`; facade **delegate** (handle_events) |

## qid.rs (+ db.rs) → QidService (state: `QidState`)

| Method | Touches | New home / signature |
|---|---|---|
| `refresh_qids` | `qid.*`, `log_event` (unconfigured only) | `Qid::refresh_qids() → Option<log>`; facade **delegate** (glue-called, `init`) |
| `qid_select` | `qid.*`, `record.recording/time/frame_count/last_recording_path` (Rec), `log_event` | `Qid::select(component_id, recording, SegmentAnchor, recording_path) → logs`; facade **delegate** builds anchor from `record` (glue-called) |
| `start_qid_session` | `qid.*`, anchor from `record` | `Qid::start_session(anchor)`; facade helper `start_qid_session` supplies anchor (record.rs) |
| `stop_qid_session` | `qid.*`, anchor + path from `record`, `log_event` | `Qid::stop_session(anchor) → logs`; facade helper supplies anchor (record.rs) |
| `drain_qid_db_events` | `qid.*`, `log_event` | `Qid::drain_db_events() → (busy, logs)`; facade **delegate** (handle_events) |
| `append_qid_sidecar` (priv) | `qid.segments`, `record.last_recording_path` (Rec) | `Qid::append_sidecar(recording_path)` (private) |
| pure fns (`close_segment`, `apply_qid_select`, `qid_matches`, `write_sidecar`, `sidecar_path`) | none | unchanged (unit-tested) |
| `now_anchor(controller)` (priv) | `record.*` | replaced by facade `now_anchor()` helper |

`db.rs` (worker thread) untouched — already decoupled over channels.

## telemetry.rs → TelemetryService (state: `TelemetryState`)

| Method | Touches | New home / signature |
|---|---|---|
| `start_telemetry` | `telemetry.*` | `Tel::start_telemetry()`; facade **delegate** (glue-called, `new`) |
| `stop_telemetry` | `telemetry.*` | `Tel::stop_telemetry()`; facade **delegate** (glue-called) |
| `stop_telemetry_inner` (priv) | `telemetry.*` | `Tel::stop_telemetry_inner()` (private) |
| free fns (`parse_line`, `spawn_reader`, …) | none | unchanged (unit-tested) |

## overlay.rs → OverlayService (state: `OverlayState`)

| Method | Touches | New home / signature |
|---|---|---|
| `set_logo_from_path` | `overlay.*`, `log_event` | `Ovl::install_logo(path) → Result<log, log>`; facade **delegate** logs (also called by `new`) |
| `overlay_settings` | `overlay.*` | `Ovl::overlay_settings()` (pure projection) |
| `save_overlay_settings` | `overlay.*` | `Ovl::save_overlay_settings()`; facade **delegate** (glue-called) |
| `data_string_rows` | `overlay`-shaped mapping + `telemetry.snapshot` (Tel) | `Ovl::data_string_rows(&TelemetrySnapshot) → rows` (explicit param); facade **delegate** reads the snapshot (glue-called + compose) |
| `logo_for_output` | `overlay.*` | `Ovl::logo_for_output(out_h)` (compose consumer) |

## capture.rs → PreviewService (state: `PreviewState` + dxgi_manager)

Glue-constraint note: the Slint glue writes `controller.take_snapshot`
(lib.rs:107), reads `controller.snapshot_flash` (push.rs:401-403), and
mutates `controller.window_hwnds` / `controller.webcam_captures` in place
(sources_glue.rs:255/282/331/332). Keeping the target of ZERO glue edits
therefore pins those five snapshot/capture-map fields to the FACADE
(`dxgi_manager` is glue-free and moves into the service).

| Method | Touches | New home / signature |
|---|---|---|
| `start_preview_capture` / `stop_preview_capture` | `preview.preview_capture_active` | `Prev::start_capture` / `Prev::stop_capture` (called from tick) |
| `capture_desktop_frame` (priv) | `dxgi_manager` | `Prev::capture_desktop_frame((x, y))` |
| `encoder_frame_due` (priv) | `record.last_frame_time` (Rec), `fps_setting` (F) | stays on facade (cross-service pacer) |
| `compose_output_frame` (priv) | `record.last_frame_time` (Rec), `preview.last_output_frame` (Prev), `annotation` (F), `text_overlays` (F), `overlay` (Ovl), `telemetry` rows (Tel), `scenes` (F) | stays on facade-private (the ONE compose point); the take_snapshot/snapshot block moves OUT to the facade's consumer dispatch — one compose, four consumers (recording stdin, stream stdin, blackbox/anomaly already tapped pre-swap, snapshot) |
| `resend_last_output_frame` (priv) | `preview.last_output_frame`, `record.*` senders (Rec), `stream.*` senders (Str), `log/stop` | stays on facade-private (orchestration; feeds encoders via Deref handles, stops via facade delegates) |
| `process_preview_frames` | everything above | stays on facade-private (tick orchestration): capture loop calls `Prev` per-source methods, blackbox/anomaly taps via facade delegates, compose once → feed record/stream/snapshot |
| `scale_frame_to_output` (priv) | none (pure) | becomes free fn `scale_frame_to_output` |
| `save_snapshot` (priv) | `record.recording/last_recording_path` (Rec), `snapshot_seq/flash` (facade, glue-pinned), `log_event` (F) | stays on facade-private (snapshot is one of the four facade-fed consumers) |
| free fn `frame_pacer_due` | none | unchanged (unit-tested) |

## lib.rs facade (stays on `RobsController`)

`new`/`build` (constructs services), `init`, `with_chat`, `tick` (same order:
`has_capture_source` → `sync_blackbox_engine` → `process_preview_frames`),
`handle_events` (chat/timer drain + blackbox/anomaly/clip/qid drains via
delegates), `format_time`, `log_event`, and flat view/settings fields
(`streaming*`, `bitrate`, encoders, `output_*`, `recording_*`, `stream_*`,
`fps*`, `keyframe_interval`, `active_video_source`, `text_overlays`) +
`annotation` / `editing` / `event_log` / `chat_messages` / `scenes`.

## Glue-facing facade surface (must keep compiling unchanged — verified
## against robs-ui-slint/src/*)

Methods: `tick`, `start_recording`, `stop_recording`, `start_streaming`,
`stop_streaming`, `toggle_clip_mark`, `log_event`, `format_time`,
`scene_has_sources`, `start_telemetry`, `stop_telemetry`, `qid_select`,
`refresh_qids`, `save_anomaly_settings`, `save_overlay_settings`,
`stop_blackbox`, `request_anomaly_clip`, `data_string_rows`.
Fields read/written by glue: `record.*`, `streaming`, `streaming_paused`,
`streaming_time`, `bitrate`, `dropped_frames`, `fps`, `fps_setting`,
`output_width/height`, `stream_service/server/key/bitrate`,
`keyframe_interval`, `recording_bitrate/path/format`, `video_encoder`,
`audio_encoder`, `telemetry.*`, `overlay.*`, `qid.*`, `blackbox.*`,
`anomaly.*`, `preview.preview_frames`, `annotation.*`, `event_log`,
`chat_messages`, `scenes`, `text_overlays`, `take_snapshot`.
All keep working through `Deref`/`DerefMut` on the owning service.
