//! Real-time playback position and word-level lyrics for NetEase Cloud Music on macOS.
//!
//! The data does not come from memory scanning or a database. On macOS the player
//! itself is native, and it pushes the position to the CEF renderer once per second,
//! landing in localStorage under `lastPlaying` (encrypted by native code, AES-ECB).
//! We read that key over the DevTools protocol and decrypt it through `deData` on the
//! client's own `ncmChannelOSX` bridge — no key required.
//!
//! Prerequisite: NetEase Cloud Music must be launched with a debugging port.
//!   osascript -e 'quit app "NeteaseMusic"' && open -a NeteaseMusic --args --remote-debugging-port=9222

mod cdp;
mod lyrics;
mod model;
mod server;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;

use cdp::Cdp;
use lyrics::{Cache, TrackPayload};
use model::{RawState, Snapshot};
use server::AppState;

struct Config {
    bind: String,
    devtools_port: u16,
    poll: Duration,
    cache_dir: std::path::PathBuf,
}

impl Config {
    fn from_env() -> Result<Self> {
        let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let port = std::env::var("PORT").unwrap_or_else(|_| "3574".into());
        let devtools_port = std::env::var("NCM_DEVTOOLS_PORT")
            .unwrap_or_else(|_| "9222".into())
            .parse()
            .context("NCM_DEVTOOLS_PORT is not a valid port")?;
        let poll_ms: u64 = std::env::var("NCM_POLL_MS")
            .unwrap_or_else(|_| "250".into())
            .parse()
            .context("NCM_POLL_MS is not a valid number")?;

        let cache_dir = match std::env::var("NCM_CACHE_DIR") {
            Ok(d) => std::path::PathBuf::from(d),
            Err(_) => {
                let home = std::env::var("HOME").context("cannot read HOME")?;
                std::path::PathBuf::from(home).join("Library/Caches/ncm-nowplaying/lyrics")
            }
        };

        Ok(Self {
            bind: format!("{host}:{port}"),
            devtools_port,
            poll: Duration::from_millis(poll_ms.max(50)),
            cache_dir,
        })
    }
}

const USAGE: &str = "\
ncm-nowplaying — real-time playback position and word-level lyrics from
NetEase Cloud Music on macOS, over HTTP and WebSocket

USAGE:
    ncm-nowplaying [--version] [--help]

PREREQUISITE (NetEase Cloud Music must be launched with a debugging port):
    osascript -e 'quit app \"NeteaseMusic\"' && open -a NeteaseMusic --args --remote-debugging-port=9222

ENVIRONMENT:
    HOST                listen address        default 127.0.0.1
    PORT                listen port           default 3574
    NCM_DEVTOOLS_PORT   app debugging port    default 9222
    NCM_POLL_MS         poll interval in ms   default 250
    NCM_CACHE_DIR       lyric cache dir       default ~/Library/Caches/ncm-nowplaying/lyrics

ENDPOINTS:
    GET /          current snapshot as JSON
    GET /ws        event stream (snapshot / musicchange / timechange / idle)
    GET /overlay   transparent lyric page, usable as an OBS browser source
";

#[tokio::main]
async fn main() -> Result<()> {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("unknown argument {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let cfg = Config::from_env()?;
    let cache = Cache::new(cfg.cache_dir.clone())?;
    let state = AppState::new();

    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .with_context(|| format!("cannot bind {}", cfg.bind))?;
    eprintln!(
        "[serve] http://{}/  ws://{}/ws  overlay http://{}/overlay",
        cfg.bind, cfg.bind, cfg.bind
    );
    eprintln!("[cache] {}", cache.dir().display());

    let app = server::router(state.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[serve] stopped: {e}");
        }
    });

    // The app may not be running yet, or may quit while we are attached: reconnect forever.
    loop {
        match Cdp::connect(cfg.devtools_port).await {
            Ok(client) => {
                eprintln!("[cdp] attached to the renderer");
                if let Err(e) = poll_loop(client, &cache, &state, cfg.poll).await {
                    eprintln!("[cdp] detached: {e}");
                }
                go_idle(&state).await;
            }
            Err(e) => eprintln!("[cdp] {e}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn go_idle(state: &Arc<AppState>) {
    state
        .publish(Snapshot::default(), json!({ "type": "idle" }))
        .await;
}

async fn poll_loop(
    mut client: Cdp,
    cache: &Cache,
    state: &Arc<AppState>,
    poll: Duration,
) -> Result<()> {
    let mut track_id: Option<String> = None;
    let mut song = None;
    let mut lyrics = None;
    // The client only reports whole seconds. Remember the wall-clock instant of each
    // tick so we can interpolate a millisecond position between them.
    let mut anchor: Option<(u64, Instant)> = None;

    loop {
        let Some(plain) = client.eval(cdp::STATE_JS).await? else {
            // Nothing has ever been played, or the play record was cleared.
            if track_id.take().is_some() {
                song = None;
                lyrics = None;
                anchor = None;
                go_idle(state).await;
            }
            tokio::time::sleep(poll).await;
            continue;
        };

        let raw: RawState = match serde_json::from_str(&plain) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[cdp] cannot parse lastPlaying: {e}");
                tokio::time::sleep(poll).await;
                continue;
            }
        };

        let id = raw.track_id();
        let current = raw.current.max(0.0) as u64;
        let changed_track = track_id.as_deref() != Some(id.as_str());

        if changed_track {
            let payload = load_track(&mut client, cache, &id).await;
            let (s, l) = payload.into_parts();
            eprintln!(
                "[track] {id} {} lyrics={:?} {} lines",
                s.as_ref().map(|s| s.name.as_str()).unwrap_or("?"),
                l.kind,
                l.lines.len()
            );
            track_id = Some(id.clone());
            song = s;
            lyrics = Some(l);
            anchor = None;
        }

        // Re-anchor whenever the second ticks; the first frame after a track change counts too.
        let bumped = match anchor {
            Some((last, _)) => last != current,
            None => true,
        };
        if bumped {
            anchor = Some((current, Instant::now()));
        }
        let since_bump = anchor.map(|(_, at)| at.elapsed()).unwrap_or_default();

        // No tick for over 2.5s means playback is paused (or the app was suspended).
        let playing = since_bump < Duration::from_millis(2500);
        let position_ms = current * 1000
            + if playing {
                since_bump.as_millis().min(999) as u64
            } else {
                0
            };

        let line_index = lyrics
            .as_ref()
            .and_then(|l| lyrics::line_at(&l.lines, position_ms));

        let snapshot = Snapshot {
            playing,
            song: song.clone(),
            lyrics: lyrics.clone(),
            current_sec: current,
            position_ms,
            duration_sec: raw.resource_duration.max(0.0) as u64,
            line_index,
            quality: raw.quality.clone(),
            cache_progress: raw.cache_progress.clone(),
        };

        if changed_track {
            let event = json!({ "type": "musicchange", "data": &snapshot });
            state.publish(snapshot, event).await;
        } else if bumped {
            let event = json!({
                "type": "timechange",
                "playing": playing,
                "currentSec": current,
                "positionMs": position_ms,
                "lineIndex": line_index,
            });
            state.publish(snapshot, event).await;
        } else {
            // Same second: refresh the snapshot but do not broadcast, or we would
            // flood every /ws client at the poll rate.
            *state.snapshot.write().await = snapshot;
        }

        tokio::time::sleep(poll).await;
    }
}

/// Try the disk cache first, then the network request from inside the page. On failure
/// fall back to empty lyrics rather than breaking the loop.
async fn load_track(client: &mut Cdp, cache: &Cache, id: &str) -> TrackPayload {
    if let Some(hit) = cache.get(id) {
        return hit;
    }
    match client.eval(&cdp::track_js(id)).await {
        Ok(Some(text)) => match serde_json::from_str::<TrackPayload>(&text) {
            Ok(payload) => {
                cache.put(id, &payload);
                payload
            }
            Err(e) => {
                eprintln!("[track] {id} cannot parse response: {e}");
                empty()
            }
        },
        Ok(None) => empty(),
        Err(e) => {
            eprintln!("[track] {id} fetch failed: {e}");
            empty()
        }
    }
}

fn empty() -> TrackPayload {
    TrackPayload {
        song: None,
        yrc: None,
        lrc: None,
        tlyric: None,
    }
}
