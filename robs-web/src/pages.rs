//! Server-rendered HTML for the viewer web app. Kept dependency-free
//! (no templating engine): three pages — login, stream list, watch —
//! sharing one small dark layout. The watch page embeds a vanilla-JS
//! WHEP (WebRTC) player with automatic HLS fallback, so playback works
//! on every modern browser incl. iOS Safari (native HLS).

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

/// Common chrome: dark DVR-ish theme, mobile-friendly viewport. `user`
/// (the signed-in email) renders the header account chip + sign-out form;
/// empty on the login page.
fn layout(title: &str, user: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · ROBS DVR</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; background: #10141a; color: #d7dde5;
    font: 15px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
    display: flex; flex-direction: column; align-items: center;
  }}
  header {{
    width: 100%; display: flex; align-items: center; gap: 12px;
    padding: 14px 20px; background: #171d26; border-bottom: 1px solid #26303e;
  }}
  header .brand {{ font-weight: 700; letter-spacing: .3px; color: #fff; }}
  header .spacer {{ flex: 1; }}
  header .user {{ color: #8b97a8; font-size: 13px; }}
  a, button {{ color: #e8edf3; }}
  .card {{
    width: min(860px, 94vw); margin: 40px auto; background: #171d26;
    border: 1px solid #26303e; border-radius: 12px; padding: 26px 28px;
  }}
  h1 {{ font-size: 19px; margin: 0 0 14px; color: #fff; }}
  .muted {{ color: #8b97a8; font-size: 13px; }}
  .error {{
    background: #3a1f22; border: 1px solid #6b2f36; color: #f2b8bd;
    border-radius: 8px; padding: 10px 12px; margin-bottom: 14px;
  }}
  label {{ display: block; margin: 12px 0 4px; font-size: 13px; color: #aab4c2; }}
  input {{
    width: 100%; padding: 10px 12px; border-radius: 8px; font-size: 15px;
    border: 1px solid #2c3846; background: #10141a; color: #e8edf3;
  }}
  input:focus {{ outline: 2px solid #2f6fed; border-color: transparent; }}
  button {{
    margin-top: 16px; width: 100%; padding: 11px; font-size: 15px; font-weight: 600;
    background: #2f6fed; border: 0; border-radius: 8px; cursor: pointer;
  }}
  button:hover {{ background: #3d7bf5; }}
  button.ghost {{
    width: auto; margin: 0; padding: 6px 14px; font-size: 13px; font-weight: 500;
    background: transparent; border: 1px solid #2c3846;
  }}
  .streams {{ list-style: none; margin: 8px 0 0; padding: 0; }}
  .streams li {{
    display: flex; align-items: center; gap: 10px;
    padding: 12px 14px; margin: 8px 0; background: #10141a;
    border: 1px solid #26303e; border-radius: 10px;
  }}
  .live-dot {{ width: 9px; height: 9px; border-radius: 50%; background: #e5484d;
    box-shadow: 0 0 6px #e5484d; flex: none; }}
  .streams a {{ font-weight: 600; text-decoration: none; }}
  .streams a:hover {{ text-decoration: underline; }}
  video {{
    width: 100%; aspect-ratio: 16 / 9; background: #000; border-radius: 10px;
    border: 1px solid #26303e;
  }}
</style>
</head>
<body>
<header>
  <span class="brand">ROBS DVR — Live</span>
  <span class="spacer"></span>
  {header_right}
</header>
{body}
</body>
</html>
"#,
        title = html_escape(title),
        header_right = header_right(user),
        body = body,
    )
}

fn header_right(user: &str) -> String {
    if user.is_empty() {
        String::new()
    } else {
        format!(
            r#"<span class="user">{}</span><form method="post" action="/logout" style="margin:0"><button class="ghost" type="submit">Sign out</button></form>"#,
            html_escape(user)
        )
    }
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn render(title: &str, user: &str, body: String) -> Response {
    (StatusCode::OK, Html(layout(title, user, &body))).into_response()
}

/// Login page; `error` renders the failure reason (invalid credentials,
/// tenant denial, lockout…).
pub fn login(error: Option<&str>) -> Response {
    let error_html = error
        .map(|e| format!(r#"<div class="error">{}</div>"#, html_escape(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<div class="card" style="max-width:420px">
<h1>Sign in</h1>
<p class="muted">Use your inspection-platform account to watch this DVR.</p>
{error_html}
<form method="post" action="/login">
  <label for="email">Email</label>
  <input id="email" name="email" type="email" autocomplete="username" required autofocus>
  <label for="password">Password</label>
  <input id="password" name="password" type="password" autocomplete="current-password" required>
  <button type="submit">Sign in</button>
</form>
</div>"#
    );
    render("Sign in", "", body)
}

/// Stream list (home). Shows every live path this DVR publishes — access was
/// already granted at login (tenant-level).
pub fn home(email: &str, paths: &[String]) -> Response {
    let items: String = if paths.is_empty() {
        r#"<p class="muted">No live streams right now. This page updates when the operator starts a broadcast — refresh to check.</p>"#
            .to_string()
    } else {
        paths
            .iter()
            .map(|name| {
                format!(
                    r#"<li><span class="live-dot"></span><a href="/watch/{name}">{name}</a><span class="muted">— live</span></li>"#,
                    name = html_escape(name)
                )
            })
            .collect()
    };
    let body = format!(
        r#"<div class="card">
<h1>Live streams</h1>
<p class="muted">Streams published by this DVR. Select one to watch.</p>
<ul class="streams">{items}</ul>
</div>"#
    );
    render("Live streams", email, body)
}

/// Home variant when MediaMTX is unreachable (starting up or stopped).
pub fn home_offline(email: &str, reason: &str) -> Response {
    let body = format!(
        r#"<div class="card">
<h1>Live streams</h1>
<div class="error">Streaming service is starting up or unavailable.</div>
<p class="muted">{}</p>
</div>"#,
        html_escape(reason)
    );
    render("Live streams", email, body)
}

/// Watch page: WHEP first, HLS fallback (native on Safari, hls.js elsewhere).
pub fn watch(email: &str, name: &str) -> Response {
    let body = format!(
        r#"<div class="card">
<h1>Watching “{name}”</h1>
<video id="video" autoplay playsinline muted controls></video>
<p class="muted" id="status">Connecting…</p>
<script src="https://cdn.jsdelivr.net/npm/hls.js@1/dist/hls.min.js"></script>
<script>
(function() {{
  var name = "{name}";
  var video = document.getElementById("video");
  var status = document.getElementById("status");
  var hlsUrl = "/hls/" + name + "/index.m3u8";

  function show(text) {{ status.textContent = text; }}

  function startHls() {{
    if (window.hls) {{ window.hls.destroy(); window.hls = null; }}
    if (video.canPlayType("application/vnd.apple.mpegurl")) {{
      show("Playing (native HLS).");
      video.src = hlsUrl;
      return;
    }}
    if (window.Hls && Hls.isSupported()) {{
      show("Playing (HLS).");
      var hls = new Hls({{ lowLatencyMode: true }});
      window.hls = hls;
      hls.loadSource(hlsUrl);
      hls.attachMedia(video);
      hls.on(Hls.Events.ERROR, function(_e, data) {{
        if (data.fatal) {{
          show("Stream unavailable. The operator may not be broadcasting yet — retrying…");
          setTimeout(startHls, 4000);
        }}
      }});
      return;
    }}
    show("This browser cannot play the stream.");
  }}

  function waitIce(pc, ms) {{
    return new Promise(function(resolve) {{
      if (pc.iceGatheringState === "complete") {{ resolve(); return; }}
      var timer = setTimeout(done, ms);
      function done() {{
        clearTimeout(timer);
        pc.removeEventListener("icegatheringstatechange", check);
        resolve();
      }}
      function check() {{ if (pc.iceGatheringState === "complete") done(); }}
      pc.addEventListener("icegatheringstatechange", check);
    }});
  }}

  async function startWhep() {{
    var pc = new RTCPeerConnection();
    pc.addTransceiver("video", {{ direction: "recvonly" }});
    pc.addTransceiver("audio", {{ direction: "recvonly" }});
    pc.ontrack = function(e) {{
      if (video.srcObject !== e.streams[0]) {{ video.srcObject = e.streams[0]; }}
    }};
    pc.onconnectionstatechange = function() {{
      if (pc.connectionState === "failed" || pc.connectionState === "disconnected") {{
        try {{ pc.close(); }} catch (_e) {{}}
        startHls();
      }}
    }};
    var offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await waitIce(pc, 2000);
    var resp = await fetch("/whep/" + name, {{
      method: "POST",
      headers: {{ "Content-Type": "application/sdp" }},
      body: pc.localDescription.sdp
    }});
    if (!resp.ok) {{ throw new Error("WebRTC signaling failed (" + resp.status + ")"); }}
    var answer = await resp.text();
    await pc.setRemoteDescription({{ type: "answer", sdp: answer }});
    show("Playing (WebRTC, low latency).");
  }}

  startWhep().catch(function(err) {{
    show("WebRTC unavailable — switching to HLS…");
    startHls();
  }});
}})();
</script>
</div>"#
    );
    render("Watch", email, body)
}
