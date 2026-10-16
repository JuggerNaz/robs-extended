# Live Server — Secure Remote Viewing Deployment

The ROBS **Live Server** replaces YouTube-based streaming for viewer access: the
DVR itself serves the live video behind a login (inspection-platform accounts),
so authorized users can watch from anywhere on any device. This guide covers
putting it on the internet securely.

## How it works

```
FFmpeg (ROBS broadcast)
   │  RTMP push  rtmp://127.0.0.1:1935/live/<stream name>
   ▼
MediaMTX sidecar (localhost only: RTMP 1935, HLS 8888, WebRTC 8889, API 9997)
   ▲                    │
   │ HLS / WebRTC       │ HLS segments + WebRTC signaling (proxied)
   │ (localhost)        ▼
   └────────── ROBS web server (login + tenant check) :8080
                        │
                reverse proxy + TLS (Caddy) :443
                        │
                viewers on any device
```

- **MediaMTX never listens on a public interface.** ROBS generates a
  localhost-only `mediamtx.yml` (next to `settings.json`) when it starts the
  sidecar. If you run MediaMTX yourself, keep every listener on `127.0.0.1`.
- **The ROBS web server is the only public surface.** Every playback request
  (HLS segments, WebRTC signaling) requires a signed session cookie issued
  after a successful login **and** tenant check.

## 1. Prerequisites

- MediaMTX binary (v1.x): download from <https://github.com/bluenviron/mediamtx/releases>
  (`mediamtx_vX.Y.Z_windows_amd64.zip` → `mediamtx.exe`).
- A Supabase project with the inspection-platform accounts
  (the same one web-app-offshore uses).
- Public IP or DNS name for the DVR machine, and router admin access for
  port forwarding.

## 2. ROBS configuration (Settings → Streaming)

1. **Service**: pick `DVR (Self-hosted)` — Server autofills
   `rtmp://127.0.0.1:1935/live`. Leave **Stream Key** empty to publish as
   `main` (the default stream name).
2. **Live Server section**:
   - **Enabled**: check to run the viewer server (starts immediately; applies
     on settings close when changed at launch).
   - **Stream Name**: publish/watch path, default `main`.
   - **MediaMTX Path**: full path to `mediamtx.exe`. If something already
     serves RTMP on `127.0.0.1:1935`, ROBS uses it instead and this can stay
     empty.
   - **Supabase URL** / **Supabase Anon Key**: from Supabase Dashboard →
     Project Settings → API (`Project URL`, `anon public` key). The anon key
     is safe to distribute here — viewer authorization still goes through
     each user's own login.
3. **Database tab** (optional but recommended): set the inspection Database
   **structure id** this DVR is pinned to. When set, only users whose active
   company owns that structure can sign in; with `0` (all structures) any
   platform account can view. Closing the settings window saves everything.

The web port (default **8080**) and the auto-generated session secret live in
`settings.json` (`live_server` section) and rarely need editing. Deleting the
`session_secret` value signs out all current viewers and regenerates on next
start.

## 3. Remote access (TLS via reverse proxy)

Viewers must reach the ROBS web server over HTTPS. The recommended setup is
[Caddy](https://caddyserver.com/) as a reverse proxy.

1. **DNS**: create an `A` record (e.g. `dvr.yourdomain.com`) pointing at the
   site's public IP.
2. **Router**: forward external **TCP 443** → the DVR machine's TCP 443
   (where Caddy listens). Do **not** forward 8080, 1935, 8888, 8889, or 9997.
3. **Caddyfile** (e.g. `C:\caddy\Caddyfile`):

   ```
   dvr.yourdomain.com {
       reverse_proxy 127.0.0.1:8080
   }
   ```

   Run: `caddy run --config C:\caddy\Caddyfile`. Caddy obtains and renews the
   certificate automatically (ports 80/443 reachable). The web server sees
   `x-forwarded-proto: https` and marks session cookies `Secure`.

4. **Optional — WebRTC (lowest latency)**: also forward **UDP 8889** and set
   `public_host` (in `settings.json` → `live_server`) to
   `dvr.yourdomain.com:8889`, then restart the live server. This fills
   `webrtcAdditionalHosts` in the generated MediaMTX config so ICE candidates
   resolve from outside. Without it, viewers fall back to HLS automatically —
   HLS over the 443 proxy is the default remote playback path and works
   everywhere.

## 4. Verifying

1. Start ROBS with Live Server enabled → event log shows
   `Live server ready at http://0.0.0.0:8080`.
2. Start a broadcast → `Streaming started → 127.0.0.1:1935/live/main`.
3. Locally: `ffplay rtmp://127.0.0.1:1935/live/main` confirms the publish.
4. From another network, open `https://dvr.yourdomain.com`, sign in with an
   inspection-platform account, and pick the stream. Watchers appear in the
   event log (`Live viewer signed in: …`); rejected logins show as
   `Live viewer denied (…)` (wrong password, wrong company, or lockout after
   5 failures for 60 s).
5. Cross-tenant check: sign in with an account from a different company →
   login is denied when a structure id is configured.

## Security notes

- Login uses Supabase GoTrue (password grant) with each user's own JWT; the
  DVR never stores passwords.
- Sessions are in-memory, HttpOnly + SameSite=Lax (+ Secure behind HTTPS),
  and expire after 12 h or 2 h idle.
- MediaMTX's control API and every media port are localhost-bound; viewers
  cannot reach them through the proxy.
- Put the DVR machine behind a firewall that only exposes 80/443 (and
  optionally UDP 8889).
