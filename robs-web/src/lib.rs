//! Embedded viewer web server for the self-hosted DVR live stream.
//!
//! Viewers open the DVR host in a browser, sign in with their existing
//! web-app-offshore account (Supabase Auth password grant), and watch the
//! live stream through this server:
//!
//! ```text
//! browser ──HTTPS──> reverse proxy (Caddy) ──HTTP──> robs-web ──proxy──> MediaMTX (localhost)
//!                                                        │
//!                                                        └──login──> Supabase (auth + tenant RLS)
//! ```
//!
//! Access control is tenant-level: after a successful password login the
//! user's own access token is used for a PostgREST read of the structure
//! configured in ROBS (`structure.id = database.structure_id`), so the web
//! app's existing company RLS decides who may watch. The verdict is cached
//! in the server-side session; afterwards the viewer can watch every stream
//! this DVR publishes. No passwords, tokens, or Supabase keys ever reach the
//! browser — it only holds a signed, HttpOnly session cookie.
//!
//! Media delivery: MediaMTX keeps re-publishing the RTMP push as HLS
//! (universal playback) and WebRTC/WHEP (low latency). This server proxies
//! `/hls/{name}/...` and `/whep/{name}` to the localhost-only MediaMTX
//! ports, so MediaMTX is never exposed.

pub mod auth;
pub mod pages;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

/// Localhost-only MediaMTX endpoints (generated config in the controller
/// pins every MediaMTX listener to `127.0.0.1`; nothing else may reach it).
pub const MTX_API_URL: &str = "http://127.0.0.1:9997";
pub const MTX_HLS_URL: &str = "http://127.0.0.1:8888";
pub const MTX_WHEP_URL: &str = "http://127.0.0.1:8889";

/// Session cookie name.
pub const SESSION_COOKIE: &str = "robs_session";

/// Streaming path names are MediaMTX path segments and our URL fragments —
/// keep them to this charset so no proxy path can escape the `/hls/{name}/`
/// prefix (traversal guard for the segment proxy).
fn valid_stream_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Configuration for the viewer web server. Built by the controller from
/// the persisted `live_server` settings (plus the database structure id the
/// tenant gate is pinned to).
#[derive(Debug, Clone)]
pub struct LiveWebConfig {
    /// TCP port to listen on (all interfaces; the reverse proxy provides
    /// TLS in production).
    pub port: u16,
    /// Supabase project base URL (`https://<ref>.supabase.co`).
    pub supabase_url: String,
    /// Supabase anon key (public-by-design).
    pub supabase_anon_key: String,
    /// `structure.id` this DVR is pinned to — the tenant gate target.
    /// `0` disables the gate; login then fails closed with an operator-facing
    /// reason (the DVR would otherwise be watchable by every tenant).
    pub structure_id: i32,
    /// Hex secret for session-cookie signatures (>= 32 hex chars).
    pub session_secret_hex: String,
    /// Idle session lifetime.
    pub idle_timeout_secs: u64,
}

/// Asynchronous events reported back to the controller (drained into the
/// ROBS event log on the UI tick).
#[derive(Debug, Clone)]
pub enum LiveWebEvent {
    /// The HTTP listener bound successfully.
    Started { addr: String },
    /// A viewer completed login and tenant verification.
    ViewerLogin { email: String },
    /// A login attempt failed (bad credentials or tenant check).
    LoginDenied { email: String, reason: String },
    /// The server could not start.
    Fatal { message: String },
}

/// Handle over the spawned web server thread. Dropping the handle does not
/// kill the thread; send `()` on `shutdown` for a graceful stop (the
/// controller does this when the live server is disabled or ROBS exits).
pub struct LiveWebHandle {
    pub shutdown: Sender<()>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

impl LiveWebHandle {
    /// Request graceful shutdown and join the thread (best-effort).
    pub fn stop(mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct AppState {
    cfg: LiveWebConfig,
    signing_key: Vec<u8>,
    sessions: auth::SessionStore,
    rate: auth::RateLimiter,
    http: reqwest::Client,
    events: Sender<LiveWebEvent>,
}

/// Spawn the viewer web server on its own thread + single-threaded tokio
/// runtime (the controller/UI stay synchronous). Returns a handle for
/// graceful shutdown.
///
/// Fails synchronously on invalid configuration; bind failures are reported
/// asynchronously via [`LiveWebEvent::Fatal`].
pub fn spawn(
    cfg: LiveWebConfig,
    events: Sender<LiveWebEvent>,
) -> Result<LiveWebHandle, String> {
    if cfg.port == 0 {
        return Err("live server web port is not set".into());
    }
    if !cfg.supabase_url.starts_with("http://") && !cfg.supabase_url.starts_with("https://") {
        return Err("live server Supabase URL must start with http(s)://".into());
    }
    if cfg.supabase_anon_key.trim().is_empty() {
        return Err("live server Supabase anon key is not set".into());
    }
    let signing_key = auth::key_from_hex(&cfg.session_secret_hex)
        .ok_or("live server session secret must be hex (>= 32 chars)")?;

    let state = Arc::new(AppState {
        sessions: auth::SessionStore::new(Duration::from_secs(
            cfg.idle_timeout_secs.max(60),
        )),
        rate: auth::RateLimiter::default(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| format!("could not build HTTP client: {e}"))?,
        cfg,
        signing_key,
        events: events.clone(),
    });

    let app = Router::new()
        .route("/", get(home))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
        .route("/watch/{name}", get(watch))
        .route("/hls/{name}/{*rest}", get(hls_proxy))
        .route("/whep/{name}", post(whep_offer))
        .with_state(state.clone());

    let port = state.cfg.port;
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("robs-live-web".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = events.send(LiveWebEvent::Fatal {
                        message: format!("web runtime: {e}"),
                    });
                    return;
                }
            };
            rt.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = events.send(LiveWebEvent::Fatal {
                            message: format!("bind 0.0.0.0:{port}: {e}"),
                        });
                        return;
                    }
                };
                let _ = events.send(LiveWebEvent::Started {
                    addr: format!("0.0.0.0:{port}"),
                });
                let shutdown = tokio::task::spawn_blocking(move || {
                    // Blocks this blocking-pool thread until stop is requested.
                    let _ = shutdown_rx.recv();
                });
                if let Err(e) = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = shutdown.await;
                })
                .await
                {
                    let _ = events.send(LiveWebEvent::Fatal {
                        message: format!("server error: {e}"),
                    });
                }
            });
        })
        .map_err(|e| format!("could not spawn web thread: {e}"))?;

    Ok(LiveWebHandle {
        shutdown: shutdown_tx,
        thread: Some(thread),
    })
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

/// Resolve the current session from the request's cookies, touching its
/// idle timestamp. Returns `(session_id, email)`.
fn current_session(state: &AppState, headers: &HeaderMap) -> Option<(String, String)> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    let id = auth::session_id_from_cookie(cookie, &state.signing_key)?;
    let snapshot = state.sessions.get(&id)?;
    Some((id, snapshot.email))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        "401 — sign in at /login to view this DVR",
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers: pages
// ---------------------------------------------------------------------------

async fn home(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some((_id, email)) = current_session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    match list_live_paths(&state.http).await {
        Ok(paths) => pages::home(&email, &paths).into_response(),
        Err(reason) => pages::home_offline(&email, &reason).into_response(),
    }
}

async fn login_page() -> Response {
    pages::login(None).into_response()
}

#[derive(Debug, Deserialize)]
struct LoginFields {
    email: String,
    password: String,
}

async fn login_submit(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(fields): Form<LoginFields>,
) -> Response {
    let ip = addr.ip();
    if let Some(wait_secs) = state.rate.locked_for(ip) {
        return pages::login(Some(&format!(
            "Too many failed attempts. Try again in {wait_secs}s."
        )))
        .into_response();
    }

    let email = fields.email.trim().to_string();
    let deny = |state: &AppState, email: String, reason: String| {
        state.rate.fail(ip);
        let _ = state.events.send(LiveWebEvent::LoginDenied {
            email: email.clone(),
            reason: reason.clone(),
        });
        pages::login(Some(&reason)).into_response()
    };

    let tokens = match auth::login(
        &state.http,
        &state.cfg.supabase_url,
        &state.cfg.supabase_anon_key,
        &email,
        &fields.password,
    )
    .await
    {
        Ok(tokens) => tokens,
        Err(reason) => return deny(&state, email, reason),
    };

    if let Err(reason) = auth::tenant_check(
        &state.http,
        &state.cfg.supabase_url,
        &state.cfg.supabase_anon_key,
        &tokens.access_token,
        state.cfg.structure_id,
    )
    .await
    {
        return deny(&state, email, reason);
    }

    state.rate.clear(ip);
    let id = state
        .sessions
        .insert(auth::Session::new(
            email.clone(),
            tokens.access_token,
            tokens.refresh_token,
        ));
    let _ = state.events.send(LiveWebEvent::ViewerLogin { email });

    let cookie_value = auth::seal(&id, &state.signing_key);
    let mut cookie = format!(
        "{SESSION_COOKIE}={cookie_value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=43200"
    );
    if headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"))
    {
        cookie.push_str("; Secure");
    }
    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some((id, _email)) = current_session(&state, &headers) {
        state.sessions.remove(&id);
    }
    let mut response = Redirect::to("/login").into_response();
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

async fn watch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let Some((_id, email)) = current_session(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    if !valid_stream_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid stream name").into_response();
    }
    pages::watch(&email, &name).into_response()
}

// ---------------------------------------------------------------------------
// Handlers: media proxies (MediaMTX stays on localhost)
// ---------------------------------------------------------------------------

/// Proxy a GET (playlist, segment, or LL-HLS parts) to MediaMTX HLS.
async fn hls_proxy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((name, rest)): Path<(String, String)>,
) -> Response {
    if current_session(&state, &headers).is_none() {
        return unauthorized();
    }
    if !valid_stream_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid stream name").into_response();
    }
    let url = format!("{MTX_HLS_URL}/{name}/{rest}");
    let mut request = state.http.get(&url);
    if let Some(range) = headers.get(header::RANGE) {
        request = request.header(header::RANGE, range);
    }
    match request.send().await {
        Ok(upstream) => {
            let status = upstream.status();
            let mut builder = Response::builder().status(status.as_u16());
            for hop in [header::CONTENT_TYPE, header::CACHE_CONTROL] {
                if let Some(value) = upstream.headers().get(&hop) {
                    builder = builder.header(hop, value);
                }
            }
            match builder.body(Body::from_stream(upstream.bytes_stream())) {
                Ok(response) => response,
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    format!("segment stream error: {e}"),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("media service unavailable: {e}"),
        )
            .into_response(),
    }
}

/// Proxy a WHEP SDP offer (POST) to MediaMTX WebRTC. ICE media flows
/// directly between the viewer and MediaMTX's UDP endpoint; signaling goes
/// through this authenticated proxy.
async fn whep_offer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: String,
) -> Response {
    if current_session(&state, &headers).is_none() {
        return unauthorized();
    }
    if !valid_stream_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid stream name").into_response();
    }
    let url = format!("{MTX_WHEP_URL}/{name}/whep");
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/sdp")
        .to_string();
    match state
        .http
        .post(&url)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
    {
        Ok(upstream) => {
            let status = upstream.status();
            let mut builder = Response::builder().status(status.as_u16());
            for hop in [header::CONTENT_TYPE, header::ETAG, header::LOCATION] {
                if let Some(value) = upstream.headers().get(&hop) {
                    builder = builder.header(hop, value);
                }
            }
            match builder.body(Body::from_stream(upstream.bytes_stream())) {
                Ok(response) => response,
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    format!("whep stream error: {e}"),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("WebRTC signaling unavailable: {e}"),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// MediaMTX path listing
// ---------------------------------------------------------------------------

/// Names of MediaMTX paths with a live publisher (ready). The loose
/// `serde_json::Value` walk tolerates response-shape drift between
/// MediaMTX versions.
async fn list_live_paths(http: &reqwest::Client) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct PathsList {
        #[serde(default)]
        items: Vec<PathItem>,
    }
    #[derive(Deserialize)]
    struct PathItem {
        name: String,
        #[serde(default)]
        ready: bool,
    }

    let response = http
        .get(format!("{MTX_API_URL}/v3/paths/list"))
        .send()
        .await
        .map_err(|e| format!("MediaMTX API unreachable: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "MediaMTX API returned {}",
            response.status()
        ));
    }
    let list: PathsList = response
        .json()
        .await
        .map_err(|e| format!("MediaMTX API response unreadable: {e}"))?;
    let mut names: Vec<String> = list
        .items
        .into_iter()
        .filter(|item| item.ready)
        .map(|item| item.name)
        .collect();
    names.sort();
    Ok(names)
}
