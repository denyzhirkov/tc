use std::sync::atomic::Ordering;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::WebSocketUpgrade;
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::tui::{App, ConnectionState};

/// Command sent from browser to client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum WebCommand {
    Command { text: String },
}

/// Serializable snapshot of app state sent to browser.
#[derive(Debug, Clone, Serialize)]
pub struct WebState {
    pub conn_state: String,
    pub channel: Option<String>,
    pub participants: Vec<String>,
    pub voice_active: bool,
    pub muted: bool,
    pub messages: Vec<String>,
    pub active_speakers: Vec<String>,
    pub voice_quality: Option<(u8, String)>,
    pub voice_traffic: Option<(f64, f64, u64)>,
    pub name: Option<String>,
}

impl WebState {
    pub fn from_app(app: &App) -> Self {
        let conn_state = match &app.conn_state {
            ConnectionState::Connected => "connected".into(),
            ConnectionState::Disconnected => "disconnected".into(),
            ConnectionState::Reconnecting { attempt } => format!("reconnecting ({})", attempt),
        };
        Self {
            conn_state,
            channel: app.channel.clone(),
            participants: app.participants.clone(),
            voice_active: app.voice_active,
            muted: app.muted.load(Ordering::Relaxed),
            messages: app.messages.clone(),
            active_speakers: app.active_speakers.clone(),
            voice_quality: app.voice_quality.clone(),
            voice_traffic: app.voice_traffic,
            name: app.name.clone(),
        }
    }
}

pub async fn start_web_server(
    port: u16,
    cmd_tx: mpsc::Sender<WebCommand>,
    state_rx: broadcast::Sender<WebState>,
) {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(move |ws| ws_handler(ws, cmd_tx, state_rx)));

    let addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("web server bind failed on {}: {}", addr, e);
            return;
        }
    };
    tracing::info!("web UI at http://{}", addr);
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("web server error: {}", e);
    }
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("web_ui.html"))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    cmd_tx: mpsc::Sender<WebCommand>,
    state_broadcast: broadcast::Sender<WebState>,
) -> axum::response::Response {
    let state_rx = state_broadcast.subscribe();
    ws.on_upgrade(move |socket| ws_connection(socket, cmd_tx, state_rx))
}

async fn ws_connection(
    mut socket: WebSocket,
    cmd_tx: mpsc::Sender<WebCommand>,
    mut state_rx: broadcast::Receiver<WebState>,
) {
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<WebCommand>(&text) {
                            let _ = cmd_tx.send(cmd).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            state = state_rx.recv() => {
                match state {
                    Ok(s) => {
                        if let Ok(json) = serde_json::to_string(&s) {
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    }
}
