//! Self-hosted live-stream viewer service ("Live Server").
//!
//! Owns the two runtime pieces of the DVR web-access feature:
//!
//! 1. **MediaMTX sidecar** — re-publishes the RTMP push for HLS/WebRTC
//!    playback. ROBS generates a localhost-only `mediamtx.yml` (RTMP 1935,
//!    HLS 8888, WebRTC 8889, control API 9997 — every listener pinned to
//!    `127.0.0.1` so the embedded web server is the only public surface),
//!    spawns the configured `mediamtx` binary with it, and kills the child
//!    on shutdown. An already-running MediaMTX on 1935 is detected and used
//!    as-is (external setup).
//! 2. **`robs-web` viewer server** — the login-protected web app. Spawned
//!    on its own thread; its events are drained into the ROBS event log by
//!    the UI tick (same pattern as the blackbox/anomaly channels).
//!
//! The stream pipeline (`stream.rs`) only talks to the MediaMTX half: a DVR
//! broadcast pushes RTMP to `127.0.0.1:1935` and falls back to the
//! configured `stream_name` when no stream key was typed.

use crate::state::EventLogKind;
use crate::RobsController;
use robs_profiles::settings::{DatabaseSettings, LiveServerSettings};
use robs_web::LiveWebEvent;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Where the FFmpeg DVR push (and our readiness probe) meets MediaMTX.
const MTX_RTMP_ENDPOINT: &str = "127.0.0.1:1935";

/// Generated MediaMTX config, written next to `settings.json`.
const MTX_CONFIG_FILE: &str = "mediamtx.yml";

/// A spawned MediaMTX child + the config it was started with.
struct MediamtxProcess {
    child: Child,
    #[allow(dead_code)]
    config_path: PathBuf,
}

/// Controller-owned live-server cluster (settings + sidecar + web server).
pub struct LiveServerService {
    /// Persisted `live_server` settings (mirrored by the settings UI).
    pub settings: LiveServerSettings,
    /// Tenant gate target: the inspection structure this DVR is pinned to.
    /// Loaded from the `database` settings at construction; kept here so
    /// the web config snapshot is stable across restarts.
    structure_id: i32,
    web: Option<robs_web::LiveWebHandle>,
    events: Option<(Sender<LiveWebEvent>, Receiver<LiveWebEvent>)>,
    mediamtx: Option<MediamtxProcess>,
}

impl LiveServerService {
    pub fn new(settings: LiveServerSettings, database: &DatabaseSettings) -> Self {
        Self {
            settings,
            structure_id: database.structure_id,
            web: None,
            events: None,
            mediamtx: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.web.is_some()
    }

    /// Start the viewer web server (and the MediaMTX sidecar when a binary
    /// is configured). Idempotent. Errors carry the complete event-log line.
    pub fn start(&mut self) -> Result<(), String> {
        if self.web.is_some() {
            return Ok(());
        }
        if !self.settings.is_configured() {
            return Err(
                "Live server not started: set the Supabase URL and anon key (Settings \u{2192} Streaming \u{2192} Live Server)"
                    .into(),
            );
        }
        // A missing secret is generated once and persisted: regenerating it
        // would sign every existing viewer session out.
        if self.settings.session_secret.trim().is_empty() {
            self.settings.session_secret = generate_secret_hex();
            let _ = self.settings.save();
        }

        self.ensure_mediamtx()?;

        let (tx, rx) = channel::<LiveWebEvent>();
        let config = robs_web::LiveWebConfig {
            port: self.settings.web_port,
            supabase_url: self.settings.supabase_url.trim().to_string(),
            supabase_anon_key: self.settings.supabase_anon_key.trim().to_string(),
            structure_id: self.structure_id,
            session_secret_hex: self.settings.session_secret.trim().to_string(),
            idle_timeout_secs: 2 * 60 * 60,
        };
        let handle = robs_web::spawn(config, tx.clone()).map_err(|e| {
            format!("Live server not started: {e} (Settings \u{2192} Streaming \u{2192} Live Server)")
        })?;
        self.web = Some(handle);
        self.events = Some((tx, rx));
        Ok(())
    }

    /// Graceful shutdown of both halves (idempotent).
    pub fn stop(&mut self) {
        if let Some(web) = self.web.take() {
            web.stop();
        }
        self.kill_mediamtx();
        self.events = None;
    }

    /// Ensure MediaMTX is serving RTMP on 127.0.0.1:1935 before a DVR push.
    /// An external instance wins; otherwise the configured binary is spawned
    /// with a generated localhost-only config.
    fn ensure_mediamtx(&mut self) -> Result<(), String> {
        if rtmp_port_open() {
            return Ok(());
        }
        let binary = self.settings.mediamtx_path.trim().to_string();
        if binary.is_empty() {
            return Err(
                "Live server not started: MediaMTX path not set and nothing is serving RTMP on 127.0.0.1:1935 (Settings \u{2192} Streaming \u{2192} Live Server)"
                    .into(),
            );
        }
        let config_path = mediamtx_config_path();
        std::fs::write(&config_path, self.mediamtx_config())
            .map_err(|e| format!("Live server not started: could not write {}: {e}", config_path.display()))?;

        let mut child = Command::new(&binary)
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Live server not started: could not launch {binary}: {e}"))?;

        // Mirror MediaMTX logs to stderr (consistent with the FFmpeg piping
        // in stream.rs) so startup problems are visible in the console.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[LiveServer:mediamtx] {line}");
                }
            });
        }
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    eprintln!("[LiveServer:mediamtx] {line}");
                }
            });
        }

        // Wait for the RTMP listener before letting FFmpeg push at it.
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            if rtmp_port_open() {
                self.mediamtx = Some(MediamtxProcess {
                    child,
                    config_path,
                });
                return Ok(());
            }
            // Bail out early if the process already died (bad config, port
            // conflict on one of the other listeners).
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "Live server not started: MediaMTX exited immediately ({status})"
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(
            "Live server not started: MediaMTX did not open RTMP 127.0.0.1:1935 within 8s"
                .into(),
        )
    }

    /// localhost-only MediaMTX config. Auth is intentionally disabled: the
    /// embedded web server is the only externally reachable surface, and it
    /// gates every playback request.
    fn mediamtx_config(&self) -> String {
        let additional_hosts = self.settings.public_host.trim();
        let webrtc_hosts = if additional_hosts.is_empty() {
            String::new()
        } else {
            format!("webrtcAdditionalHosts: {additional_hosts}\n")
        };
        format!(
            "# Generated by ROBS (Settings \u{2192} Streaming \u{2192} Live Server). \
Every listener is bound to 127.0.0.1; remote viewers reach the stream\n\
# only through the ROBS web server, which enforces login + tenant checks.\n\
api: yes\n\
apiAddress: 127.0.0.1:9997\n\
rtsp: no\n\
rtmp: yes\n\
rtmpAddress: 127.0.0.1:1935\n\
srt: no\n\
hls: yes\n\
hlsAddress: 127.0.0.1:8888\n\
hlsVariant: lowLatency\n\
hlsAllowOrigin: '*'\n\
webrtc: yes\n\
webrtcAddress: 127.0.0.1:8889\n\
{webrtc_hosts}\n\
paths:\n\
  all_others:\n"
        )
    }

    fn kill_mediamtx(&mut self) {
        if let Some(mut process) = self.mediamtx.take() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }

    /// Pop every event the web server reported since the last drain.
    pub fn drain_events(&mut self) -> Vec<LiveWebEvent> {
        let Some((_, rx)) = &self.events else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }
}

impl Drop for LiveServerService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// True when something is already listening on the MediaMTX RTMP endpoint.
fn rtmp_port_open() -> bool {
    TcpStream::connect_timeout(
        &MTX_RTMP_ENDPOINT
            .parse()
            .expect("static endpoint parses"),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// `<config_dir>/mediamtx.yml`, sibling of `settings.json`.
fn mediamtx_config_path() -> PathBuf {
    robs_profiles::settings::settings_file_path()
        .map(|p| {
            p.parent()
                .map(|dir| dir.join(MTX_CONFIG_FILE))
                .unwrap_or_else(|| PathBuf::from(MTX_CONFIG_FILE))
        })
        .unwrap_or_else(|| PathBuf::from(MTX_CONFIG_FILE))
}

/// 256-bit random hex secret via `fastrand` (already a workspace dep).
fn generate_secret_hex() -> String {
    let mut rng = fastrand::Rng::new();
    (0..32)
        .map(|_| format!("{:02x}", rng.u8(..)))
        .collect()
}

// ---------------------------------------------------------------------------
// Facade integration: start/stop/save + event-log plumbing
// ---------------------------------------------------------------------------

impl RobsController {
    /// Start the live server, logging the outcome. Called at launch when the
    /// saved settings are enabled, and by the settings UI toggle.
    pub fn start_live_server(&mut self) {
        match self.live_server.start() {
            Ok(()) => self.log_event(
                format!(
                    "Live server starting \u{2192} http://0.0.0.0:{} (stream name \"{}\")",
                    self.live_server.settings.web_port, self.live_server.settings.stream_name
                ),
                EventLogKind::Stream,
            ),
            Err(message) => self.log_event(message, EventLogKind::Stream),
        }
    }

    /// Stop the live server (viewer sessions and the MediaMTX sidecar die).
    pub fn stop_live_server(&mut self) {
        if self.live_server.is_running() {
            self.live_server.stop();
            self.log_event("Live server stopped", EventLogKind::Stream);
        }
    }

    /// Persist the `live_server` settings section (settings-close gesture).
    pub fn save_live_server_settings(&mut self) {
        if let Err(e) = self.live_server.settings.save() {
            eprintln!("[LiveServer] could not save settings: {e}");
        }
    }

    /// Drain web-server events into the event log; returns true when any
    /// event was drained so the UI keeps ticking at its fast cadence.
    pub(crate) fn drain_live_server_events(&mut self) -> bool {
        let events = self.live_server.drain_events();
        if events.is_empty() {
            return false;
        }
        for event in events {
            match event {
                LiveWebEvent::Started { addr } => self.log_event(
                    format!("Live server ready at http://{addr}"),
                    EventLogKind::Stream,
                ),
                LiveWebEvent::ViewerLogin { email } => self.log_event(
                    format!("Live viewer signed in: {email}"),
                    EventLogKind::Stream,
                ),
                LiveWebEvent::LoginDenied { email, reason } => self.log_event(
                    format!("Live viewer denied ({email}): {reason}"),
                    EventLogKind::Stream,
                ),
                LiveWebEvent::Fatal { message } => self.log_event(
                    format!("Live server error: {message}"),
                    EventLogKind::Stream,
                ),
            }
        }
        true
    }
}
