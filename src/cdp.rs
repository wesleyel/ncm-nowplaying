//! Evaluate JavaScript inside the NetEase Cloud Music renderer over the CEF DevTools protocol.
//!
//! The macOS player is native, so the position is pushed from native code to JS once per
//! second and persisted to localStorage under `lastPlaying` (encrypted by native code).
//! Decrypting it needs no key: the client's own `ncmChannelOSX` bridge exposes the inverse
//! operation, `deData`.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

/// Read `lastPlaying` and decrypt it through the native bridge, yielding the plaintext JSON.
pub const STATE_JS: &str = r#"
(function () {
  var raw = localStorage.getItem("lastPlaying");
  if (!raw) return null;
  return window.ncmChannelOSX.callSyncWithParams(
    JSON.stringify({ method: "deData", params: [raw] })) || null;
})()
"#;

/// Fetch song detail and lyrics from the page context: same origin with session cookies
/// already attached, so there is no eapi signing to deal with.
pub fn track_js(id: &str) -> String {
    format!(
        r#"
(async function () {{
  var id = {id:?};
  var detailUrl = "https://music.163.com/api/song/detail?ids=%5B" + encodeURIComponent(id) + "%5D";
  var lyricUrl = "https://music.163.com/api/song/lyric/v1?id=" + encodeURIComponent(id)
    + "&cp=false&lv=-1&kv=-1&tv=-1&rv=-1&yv=-1&ytv=-1&yrv=-1";
  var results = await Promise.all([
    fetch(detailUrl, {{ credentials: "include" }}).then(function (r) {{ return r.json(); }}),
    fetch(lyricUrl, {{ credentials: "include" }}).then(function (r) {{ return r.json(); }})
  ]);
  var detail = results[0], lyric = results[1];
  var s = (detail.songs || [])[0] || null;
  return JSON.stringify({{
    song: s ? {{
      id: String(id),
      name: s.name || "",
      artists: (s.artists || []).map(function (a) {{ return a.name; }}),
      album: s.album ? s.album.name : "",
      albumPic: s.album ? (s.album.picUrl || "") : "",
      durationMs: s.duration || 0
    }} : null,
    yrc: (lyric.yrc && lyric.yrc.lyric) || null,
    lrc: (lyric.lrc && lyric.lrc.lyric) || null,
    tlyric: (lyric.tlyric && lyric.tlyric.lyric) || null
  }});
}})()
"#
    )
}

pub struct Cdp {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    /// Attach to the single page target (`orpheus://orpheus/app.html`).
    pub async fn connect(port: u16) -> Result<Self> {
        let list: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list"))
            .await
            .with_context(|| {
                format!(
                    "cannot reach 127.0.0.1:{port} — was NeteaseMusic started \
                     without --remote-debugging-port?"
                )
            })?
            .json()
            .await
            .context("/json/list did not return JSON")?;

        let url = list
            .as_array()
            .and_then(|targets| targets.iter().find(|t| t["type"] == "page"))
            .and_then(|t| t["webSocketDebuggerUrl"].as_str())
            .ok_or_else(|| anyhow!("no page target in DevTools"))?
            .to_owned();

        let (ws, _) = connect_async(&url)
            .await
            .context("DevTools WebSocket handshake failed")?;
        Ok(Self { ws, next_id: 0 })
    }

    /// Evaluate an expression that resolves to a string (or null).
    pub async fn eval(&mut self, expr: &str) -> Result<Option<String>> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({
            "id": id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true,
                "timeout": 8000,
            }
        });
        self.ws.send(Message::Text(req.to_string().into())).await?;

        loop {
            let msg = self
                .ws
                .next()
                .await
                .ok_or_else(|| anyhow!("DevTools connection closed"))??;
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => bail!("DevTools connection closed"),
                // Skip protocol events and ping/pong; we only want our own reply.
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text)?;
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(details) = v["result"].get("exceptionDetails") {
                bail!("the page threw: {details}");
            }
            return Ok(v["result"]["result"]["value"].as_str().map(str::to_owned));
        }
    }
}
