use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, RwLock};

use crate::model::Snapshot;

pub struct AppState {
    pub snapshot: RwLock<Snapshot>,
    pub events: broadcast::Sender<String>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        Arc::new(Self {
            snapshot: RwLock::new(Snapshot::default()),
            events,
        })
    }

    /// Update the snapshot and broadcast an event. Events are dropped when nobody is
    /// subscribed, which is intended.
    pub async fn publish(&self, snapshot: Snapshot, event: serde_json::Value) {
        *self.snapshot.write().await = snapshot;
        if let Ok(text) = serde_json::to_string(&event) {
            let _ = self.events.send(text);
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(snapshot))
        .route("/ws", get(ws_upgrade))
        .route("/overlay", get(overlay))
        .with_state(state)
}

async fn snapshot(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.snapshot.read().await.clone())
}

async fn overlay() -> impl IntoResponse {
    Html(include_str!("overlay.html"))
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| ws_session(socket, state))
}

async fn ws_session(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();

    // Subscribe before reading the snapshot so no event slips through the gap.
    let mut rx = state.events.subscribe();
    let initial = serde_json::json!({
        "type": "snapshot",
        "data": &*state.snapshot.read().await,
    });
    if let Ok(text) = serde_json::to_string(&initial) {
        if sink.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                // The client fell behind; resynchronise it with the latest snapshot.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let resync = serde_json::json!({
                        "type": "snapshot",
                        "data": &*state.snapshot.read().await,
                    });
                    if let Ok(text) = serde_json::to_string(&resync) {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                // This endpoint takes no commands; ignore everything else.
                Some(Ok(_)) => {}
            },
        }
    }
}
