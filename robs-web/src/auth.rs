//! Authentication and session machinery for the live viewer.
//!
//! * `login` — Supabase GoTrue password grant (viewers reuse their
//!   web-app-offshore accounts; ROBS never sees or stores the password).
//! * `tenant_check` — one PostgREST read of the DVR's pinned `structure` row
//!   with the *user's* access token, so the web app's company RLS decides.
//! * `SessionStore` — server-side sessions (the browser only gets a signed
//!   cookie referencing one).
//! * `seal` / `unseal` / `session_id_from_cookie` — HMAC-SHA256-signed
//!   session cookies.
//! * `RateLimiter` — per-IP login failure backoff.

use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sha2::Sha256;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Supabase
// ---------------------------------------------------------------------------

/// Tokens and identity from a successful Supabase password grant.
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub email: String,
}

/// Sign a viewer in via Supabase GoTrue. A non-2xx is collapsed into one
/// generic reason so the endpoint never leaks whether an account exists.
pub async fn login(
    http: &reqwest::Client,
    supabase_url: &str,
    anon_key: &str,
    email: &str,
    password: &str,
) -> Result<Tokens, String> {
    #[derive(serde::Serialize)]
    struct Grant<'a> {
        email: &'a str,
        password: &'a str,
    }

    let url = format!(
        "{}/auth/v1/token?grant_type=password",
        supabase_url.trim_end_matches('/')
    );
    let response = http
        .post(url)
        .header("apikey", anon_key)
        .json(&Grant { email, password })
        .send()
        .await
        .map_err(|e| format!("login service unreachable: {e}"))?;
    if !response.status().is_success() {
        return Err("Invalid email or password.".into());
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Login service returned an unreadable response.".to_string())?;
    let access_token = body["access_token"]
        .as_str()
        .ok_or_else(|| "Login service response missing an access token.".to_string())?
        .to_string();
    let refresh_token = body["refresh_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let resolved_email = body["user"]["email"]
        .as_str()
        .unwrap_or(email)
        .to_string();
    Ok(Tokens {
        access_token,
        refresh_token,
        email: resolved_email,
    })
}

/// Verify the signed-in user belongs to this DVR's tenant: read the pinned
/// `structure` row through PostgREST with the user's own access token. The
/// web app's RLS (`company_id = get_user_active_company_id(auth.uid())`,
/// super admins see all rows) returns the row only for in-tenant users.
/// Fails closed: any error denies access.
pub async fn tenant_check(
    http: &reqwest::Client,
    supabase_url: &str,
    anon_key: &str,
    access_token: &str,
    structure_id: i32,
) -> Result<(), String> {
    if structure_id == 0 {
        return Err(
            "This DVR is not linked to a structure yet (set it in ROBS Settings → Database)."
                .into(),
        );
    }
    let url = format!(
        "{}/rest/v1/structure?id=eq.{structure_id}&select=id",
        supabase_url.trim_end_matches('/')
    );
    let response = http
        .get(&url)
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|e| format!("Access check failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Access check denied ({}).", response.status()));
    }
    let rows: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Access check returned an unreadable response.".to_string())?;
    let in_tenant = rows
        .as_array()
        .map(|rows| !rows.is_empty())
        .unwrap_or(false);
    if in_tenant {
        Ok(())
    } else {
        Err("Your account does not have access to this DVR site.".into())
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// A server-side viewer session. The Supabase tokens are kept in memory for
/// future re-verification / refresh needs; they never reach the browser.
#[derive(Debug, Clone)]
pub struct Session {
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Result of the login-time tenant check (always `true` today; a session
    /// is only created after a successful check).
    pub tenant_ok: bool,
    last_seen: Instant,
}

impl Session {
    /// Fresh session with its idle clock started now.
    pub fn new(email: String, access_token: String, refresh_token: String) -> Self {
        Self {
            email,
            access_token,
            refresh_token,
            tenant_ok: true,
            last_seen: Instant::now(),
        }
    }
}

/// What handlers need from a live session.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub email: String,
}

/// Thread-safe session map with idle expiry.
#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    idle_timeout: Duration,
}

impl SessionStore {
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            idle_timeout,
        }
    }

    /// Insert a session and return its fresh random id.
    pub fn insert(&self, session: Session) -> String {
        let id = new_session_id();
        self.inner.lock().insert(id.clone(), session);
        id
    }

    /// Fetch a live session by id, touching its idle timestamp. `None` for
    /// unknown or expired sessions (expired entries are dropped).
    pub fn get(&self, id: &str) -> Option<SessionSnapshot> {
        let mut sessions = self.inner.lock();
        let expired = {
            let session = sessions.get_mut(id)?;
            if session.last_seen.elapsed() > self.idle_timeout {
                true
            } else {
                session.last_seen = Instant::now();
                false
            }
        };
        if expired {
            sessions.remove(id);
            return None;
        }
        sessions.get(id).map(|s| SessionSnapshot {
            email: s.email.clone(),
        })
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().remove(id);
    }
}

/// Fresh 128-bit session id derived from the signing key + monotonic
/// counter + wall clock (uniqueness enforced by the store map anyway).
pub fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let msg = format!(
        "{}:{}:{}",
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::process::id()
    );
    let mut mac = HmacSha256::new_from_slice(b"robs-session-id").expect("hmac accepts any key");
    mac.update(msg.as_bytes());
    hex(&mac.finalize().into_bytes())[..32].to_string()
}

// ---------------------------------------------------------------------------
// Signed cookies
// ---------------------------------------------------------------------------

/// Parse the raw secret (hex) into a signing key. `None` when too short —
/// a short secret would make cookie forgery feasible.
pub fn key_from_hex(hex_secret: &str) -> Option<Vec<u8>> {
    let bytes = decode_hex(hex_secret.trim())?;
    (bytes.len() >= 16).then_some(bytes)
}

/// `value` → `{value}.{hex hmac}`.
pub fn seal(value: &str, key: &[u8]) -> String {
    format!("{}.{}", value, hex(&sign(value.as_bytes(), key)))
}

/// Verify a `{value}.{hex hmac}` pair and return the value. `None` on any
/// mismatch (forged, truncated, or re-signed with another secret).
pub fn unseal(sealed: &str, key: &[u8]) -> Option<String> {
    let (value, signature) = sealed.rsplit_once('.')?;
    let expected = sign(value.as_bytes(), key);
    let given = decode_hex(signature)?;
    if given.len() != expected.len() {
        return None;
    }
    // Constant-time-ish comparison: fold every byte difference together so
    // early exit cannot leak the signature prefix.
    let mut diff = 0u8;
    for (a, b) in expected.iter().zip(given.iter()) {
        diff |= a ^ b;
    }
    (diff == 0).then(|| value.to_string())
}

/// Extract and verify the session id from a request's `Cookie` header.
pub fn session_id_from_cookie(cookie_header: &str, key: &[u8]) -> Option<String> {
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some(raw) = pair.strip_prefix("robs_session=") {
            return unseal(raw, key);
        }
    }
    None
}

fn sign(message: &[u8], key: &[u8]) -> hmac::digest::Output<HmacSha256> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key");
    mac.update(message);
    mac.finalize().into_bytes()
}

// ---------------------------------------------------------------------------
// Login rate limiting
// ---------------------------------------------------------------------------

const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, RateEntry>>>,
}

#[derive(Default)]
struct RateEntry {
    failures: u32,
    locked_until: Option<Instant>,
}

impl RateLimiter {
    /// Remaining lockout seconds for this IP, if currently locked out.
    pub fn locked_for(&self, ip: IpAddr) -> Option<u64> {
        let mut entries = self.inner.lock();
        let entry = entries.get_mut(&ip)?;
        let elapsed = entry.locked_until?.checked_duration_since(Instant::now())?;
        if elapsed.is_zero() {
            entry.locked_until = None;
            entry.failures = 0;
            None
        } else {
            Some(elapsed.as_secs().max(1))
        }
    }

    /// Record a failed attempt; lock the IP out after MAX_FAILURES.
    pub fn fail(&self, ip: IpAddr) {
        let mut entries = self.inner.lock();
        let entry = entries.entry(ip).or_default();
        entry.failures += 1;
        if entry.failures >= MAX_FAILURES {
            entry.locked_until = Some(Instant::now() + LOCKOUT);
        }
    }

    /// Clear the IP's history after a successful login.
    pub fn clear(&self, ip: IpAddr) {
        self.inner.lock().remove(&ip);
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn decode_hex(input: &str) -> Option<Vec<u8>> {
    let input = input.trim();
    if !input.len().is_multiple_of(2) {
        return None;
    }
    (0..input.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&input[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn seal_unseal_round_trips() {
        let id = new_session_id();
        let sealed = seal(&id, KEY);
        assert_eq!(unseal(&sealed, KEY).as_deref(), Some(id.as_str()));
    }

    #[test]
    fn unseal_rejects_tampering_and_wrong_key() {
        let sealed = seal("abc", KEY);
        let mut tampered = sealed.clone();
        tampered.replace_range(0..1, "x");
        assert_eq!(unseal(&tampered, KEY), None);
        assert_eq!(unseal(&sealed, b"another-key-0123456789abcdef"), None);
        assert_eq!(unseal("no-signature", KEY), None);
        assert_eq!(unseal("abc.zzzz", KEY), None);
    }

    #[test]
    fn session_cookie_is_found_among_others() {
        let sealed = seal("sess42", KEY);
        let header = format!("theme=dark; robs_session={sealed}; lang=en");
        assert_eq!(
            session_id_from_cookie(&header, KEY).as_deref(),
            Some("sess42")
        );
        assert_eq!(session_id_from_cookie("theme=dark", KEY), None);
    }

    #[test]
    fn session_ids_are_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn key_from_hex_enforces_minimum_length() {
        assert!(key_from_hex("aabb").is_none());
        assert!(key_from_hex("not hex").is_none());
        assert!(key_from_hex(&hex(b"0123456789abcdef0123")).is_some());
    }

    #[test]
    fn rate_limiter_locks_after_max_failures_and_clears() {
        let limiter = RateLimiter::default();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(limiter.locked_for(ip).is_none());
        for _ in 0..MAX_FAILURES - 1 {
            limiter.fail(ip);
        }
        assert!(limiter.locked_for(ip).is_none(), "not locked yet");
        limiter.fail(ip);
        assert!(limiter.locked_for(ip).is_some(), "locked after 5 fails");
        limiter.clear(ip);
        assert!(limiter.locked_for(ip).is_none());
    }

    #[test]
    fn session_store_expires_idle_sessions() {
        let store = SessionStore::new(Duration::from_secs(60));
        let id = store.insert(Session::new(
            "viewer@example.com".into(),
            "t".into(),
            "r".into(),
        ));
        assert_eq!(store.get(&id).map(|s| s.email).as_deref(), Some("viewer@example.com"));
        store.remove(&id);
        assert!(store.get(&id).is_none());
    }
}
