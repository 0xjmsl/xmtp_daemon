//! The plain-HTTP link between the daemon and your program (the "bot").
//!
//! Daemon -> bot: `POST {XMTP_WEBHOOK_URL}/api/xmtp/incoming` with the envelope
//! (envelope.rs), and `POST {XMTP_WEBHOOK_URL}/api/xmtp/group_members` when a
//! group's members are first seen. One ordered queue with retry and backoff, so
//! a bot restart doesn't lose messages.
//!
//! Bot -> daemon (`XMTP_HTTP_ADDR`, default 127.0.0.1:3100):
//! - `POST /send`      body = an outgoing message (outgoing.rs) -> `{id}`
//! - `POST /dm`        body = `{inboxId}` or `{address}` -> `{conversationId, peerInboxId}`:
//!   the DM with that person, opened if new, so the bot can write first
//! - `POST /api/init`  no-op 200, kept so a bot can poll "is the daemon up?"
//! - `GET  /health`    `{address, inboxId, queued, streamStartedMs, lastStreamEventMs}`

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use crate::envelope::Incoming;

/// What goes to the bot, in arrival order through ONE queue.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// `POST /api/xmtp/incoming`
    Message(Incoming),
    /// `POST /api/xmtp/group_members`
    GroupMembers { conversation_id: String, body: Value },
}

impl Outbound {
    fn route(&self) -> &'static str {
        match self {
            Outbound::Message(_) => "/api/xmtp/incoming",
            Outbound::GroupMembers { .. } => "/api/xmtp/group_members",
        }
    }
    fn label(&self) -> String {
        match self {
            Outbound::Message(m) => format!("message {}", m.id),
            Outbound::GroupMembers { conversation_id, .. } => format!("group_members {conversation_id}"),
        }
    }
    fn json(&self) -> Value {
        match self {
            Outbound::Message(m) => serde_json::to_value(m).unwrap_or(Value::Null),
            Outbound::GroupMembers { body, .. } => body.clone(),
        }
    }
}
use crate::outgoing;

/// Most envelopes held while the bot is unreachable; beyond this the OLDEST
/// is dropped (logged).
const QUEUE_CAP: usize = 1000;

/// Hands envelopes to the forwarder task.
#[derive(Clone)]
pub struct Forwarder {
    tx: mpsc::UnboundedSender<Outbound>,
    pub queued: Arc<std::sync::atomic::AtomicUsize>,
    /// ms epoch of the last event from the live XMTP stream (0 = none yet).
    /// Surfaced by `/health` so a watchdog can tell "alive" from "alive but
    /// deaf". A quiet chat is normal, so compare against the stream's age.
    pub last_stream_event_ms: Arc<std::sync::atomic::AtomicI64>,
    /// ms epoch when the current stream subscription started.
    pub stream_started_ms: Arc<std::sync::atomic::AtomicI64>,
}

impl Forwarder {
    /// `webhook_url = None` → log-only mode (nothing leaves the daemon). There
    /// is deliberately no default URL: a test run must never reach a live bot.
    ///
    /// `cursor_path`: after the bot ACCEPTS a message (2xx), its `timestamp` is
    /// written there if newer. The next boot drops anything at or below it, so
    /// a restart doesn't replay old messages. Not written in log-only mode.
    pub fn start(webhook_url: Option<String>, cursor_path: std::path::PathBuf) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Outbound>();
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let q = queued.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("http client");
            let mut pending: std::collections::VecDeque<Outbound> = Default::default();
            let mut cursor = load_cursor(&cursor_path).unwrap_or(0);
            let mut backoff = Duration::from_secs(1);
            loop {
                // Wait for work when idle; otherwise drain whatever else arrived.
                if pending.is_empty() {
                    match rx.recv().await {
                        Some(e) => pending.push_back(e),
                        None => return,
                    }
                }
                while let Ok(e) = rx.try_recv() {
                    pending.push_back(e);
                }
                while pending.len() > QUEUE_CAP {
                    if let Some(old) = pending.pop_front() {
                        tracing::error!(item = %old.label(), "queue full, dropped oldest");
                    }
                }
                q.store(pending.len(), std::sync::atomic::Ordering::Relaxed);

                let Some(url) = webhook_url.as_deref() else {
                    for e in pending.drain(..) {
                        tracing::info!(item = %e.label(), "log-only (no XMTP_WEBHOOK_URL)");
                    }
                    q.store(0, std::sync::atomic::Ordering::Relaxed);
                    continue;
                };
                let env = pending.front().expect("non-empty");
                match http.post(format!("{url}{}", env.route())).json(&env.json()).send().await {
                    Ok(r) if r.status().is_success() => {
                        tracing::info!(item = %env.label(), "delivered to the bot");
                        if let Outbound::Message(m) = env {
                            if m.timestamp > cursor {
                                cursor = m.timestamp;
                                save_cursor(&cursor_path, cursor);
                            }
                        }
                        pending.pop_front();
                        q.store(pending.len(), std::sync::atomic::Ordering::Relaxed);
                        backoff = Duration::from_secs(1);
                    }
                    Ok(r) if r.status().is_client_error() => {
                        // The bot rejected it on purpose: retrying won't help.
                        tracing::error!(item = %env.label(), status = %r.status(), "bot refused, dropped");
                        pending.pop_front();
                        q.store(pending.len(), std::sync::atomic::Ordering::Relaxed);
                    }
                    other => {
                        let why = match other {
                            Ok(r) => format!("HTTP {}", r.status()),
                            Err(e) => e.to_string(),
                        };
                        tracing::warn!(item = %env.label(), queued = pending.len(), "bot unreachable ({why}), retry in {backoff:?}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        });
        Self {
            tx,
            queued,
            last_stream_event_ms: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            stream_started_ms: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        }
    }

    pub fn forward(&self, item: Outbound) {
        let _ = self.tx.send(item);
    }
}

/// The saved cursor (ms epoch), or None if there is no readable file.
pub fn load_cursor(path: &std::path::Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Write via a temp file + rename so a crash never leaves a half-written cursor.
pub fn save_cursor(path: &std::path::Path, ms: i64) {
    let tmp = path.with_extension("tmp");
    let res = std::fs::write(&tmp, ms.to_string()).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        tracing::error!("could not save cursor {}: {e}", path.display());
    }
}

#[derive(Clone)]
struct AppState {
    address: String,
    inbox_id: String,
    fwd: Forwarder,
    /// One send at a time keeps message order within a conversation.
    send_lock: Arc<Mutex<()>>,
}

async fn send(State(s): State<AppState>, Json(msg): Json<Value>) -> (StatusCode, Json<Value>) {
    let _guard = s.send_lock.lock().await;
    // A send that never returns would hold the lock and freeze every later
    // send. Give up after XMTP_SEND_TIMEOUT_SECS (default 120) with a 504.
    let limit = Duration::from_secs(
        std::env::var("XMTP_SEND_TIMEOUT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(120),
    );
    let result = match tokio::time::timeout(limit, outgoing::send(&msg)).await {
        Ok(r) => r,
        Err(_) => {
            tracing::error!("send timed out after {limit:?}");
            return (StatusCode::GATEWAY_TIMEOUT, Json(json!({ "error": format!("send timed out after {}s", limit.as_secs()) })));
        }
    };
    match result {
        Ok(id) => (StatusCode::OK, Json(json!({ "id": id }))),
        Err(e) => {
            tracing::error!("send failed: {e:#}");
            let code = if outgoing::encode(&msg).is_err() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            (code, Json(json!({ "error": format!("{e:#}") })))
        }
    }
}

/// `POST /dm` body `{inboxId}` or `{address}` -> `{conversationId, peerInboxId}`.
/// Finds the DM with that person, or opens one, so the bot can write first
/// (a receipt, a hello). The id is the one `/send` takes and incoming messages carry.
async fn dm(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let inbox = match (body.get("inboxId").and_then(Value::as_str), body.get("address").and_then(Value::as_str)) {
        (Some(id), _) if !id.trim().is_empty() => id.trim().trim_start_matches("0x").to_lowercase(),
        (_, Some(addr)) if !addr.trim().is_empty() => {
            match crate::api::conversations::inbox_id_from_address(addr.trim().to_string()).await {
                Ok(id) if !id.is_empty() => id,
                Ok(_) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "that address has no XMTP inbox" }))),
                Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("{e:#}") }))),
            }
        }
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "give inboxId or address" }))),
    };
    match crate::api::conversations::find_or_create_dm_with_inbox_id(inbox.clone()).await {
        Ok(info) => (StatusCode::OK, Json(json!({
            "conversationId": info.id,
            "peerInboxId": info.peer_inbox_id.unwrap_or(inbox),
        }))),
        Err(e) => {
            tracing::error!("dm failed: {e:#}");
            (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("{e:#}") })))
        }
    }
}

async fn init() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn health(State(s): State<AppState>) -> Json<Value> {
    Json(json!({
        "address": s.address,
        "inboxId": s.inbox_id,
        "queued": s.fwd.queued.load(std::sync::atomic::Ordering::Relaxed),
        "streamStartedMs": s.fwd.stream_started_ms.load(std::sync::atomic::Ordering::Relaxed),
        "lastStreamEventMs": s.fwd.last_stream_event_ms.load(std::sync::atomic::Ordering::Relaxed),
    }))
}

/// Serve the local API until the process ends.
pub async fn serve(addr: String, address: String, inbox_id: String, fwd: Forwarder) -> anyhow::Result<()> {
    let state = AppState { address, inbox_id, fwd, send_lock: Arc::new(Mutex::new(())) };
    let app = Router::new()
        .route("/send", post(send))
        .route("/dm", post(dm))
        .route("/api/init", post(init))
        .route("/health", get(health))
        // axum's default is 2 MB, too small for a base64 attachment of a few
        // MB. libxmtp's own message-size
        // limit rejects anything truly too big (a 502 from /send).
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "local API listening");
    axum::serve(listener, app).await?;
    Ok(())
}
