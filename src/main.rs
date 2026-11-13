//! xmtp_daemon: keeps one XMTP inbox online and hands every incoming message
//! to your program over local HTTP (see README.md and AGENTS.md).
//!
//! Config (env), the full table is in README.md:
//! - `XMTP_KEY_FILE`     hex private key of the bot identity (created if missing)
//! - `XMTP_DB_KEY_FILE`  hex 32-byte SQLCipher key for the local DB (created if missing)
//! - `XMTP_DB_DIR`       where the local DB lives (default `./xmtp_data`)
//! - `XMTP_ENV`          `production` (default) | `dev` | `local`

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use prost::Message as _;
use xmtp_proto::xmtp::message_contents::EncodedContent;
use xmtp_daemon::api::{client, conversations, messaging};
use xmtp_daemon::{bridge, codec, envelope, watch};

/// One line describing a raw message: content type, parameters (our caption
/// rides in `description`), fallback and payload size. Step-2 groundwork.
fn describe(bytes: &[u8]) -> String {
    match EncodedContent::decode(bytes) {
        Ok(ec) => {
            let ty = ec
                .r#type
                .as_ref()
                .map(|t| format!("{}/{}:{}", t.authority_id, t.type_id, t.version_major))
                .unwrap_or_else(|| "?".into());
            // Remote-attachment decryption material never goes to the log.
            let mut params: Vec<(&String, &str)> = ec
                .parameters
                .iter()
                .map(|(k, v)| {
                    let shown = match k.as_str() {
                        "secret" | "salt" | "nonce" => "<redacted>",
                        _ => v.as_str(),
                    };
                    (k, shown)
                })
                .collect();
            params.sort();
            let text = if ec.r#type.as_ref().map(|t| t.type_id == "text").unwrap_or(false) {
                format!(" text={:?}", String::from_utf8_lossy(&ec.content))
            } else {
                String::new()
            };
            format!(
                "type={ty} params={params:?} fallback={:?} content_bytes={}{text}",
                ec.fallback.unwrap_or_default(),
                ec.content.len()
            )
        }
        Err(e) => format!("undecodable EncodedContent ({} bytes): {e}", bytes.len()),
    }
}

/// Message ids already handled. The live stream re-delivers what the startup
/// catch-up just read, so every message goes through here
/// first. Bounded: the oldest ids fall out after `CAP`.
struct Seen {
    set: std::collections::HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl Seen {
    const CAP: usize = 5000;
    fn new() -> Self {
        Self { set: Default::default(), order: Default::default() }
    }
    /// True the first time an id is offered.
    fn first(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > Self::CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// Send-only mode (watch mode with no webhook): a received message is counted
/// for the watch loop and never opened. It is not decoded and not logged.
struct SendOnly {
    incoming: tokio::sync::mpsc::UnboundedSender<String>,
    cursor_path: PathBuf,
    cursor_ms: i64,
}

impl SendOnly {
    fn note(&mut self, m: &messaging::MessageInfo) {
        let _ = self.incoming.send(m.sender_inbox_id.clone());
        // Move the cursor, so a restart does not count this message again.
        if m.sent_at_ms > self.cursor_ms {
            self.cursor_ms = m.sent_at_ms;
            bridge::save_cursor(&self.cursor_path, self.cursor_ms);
        }
    }
}

/// Decode one message with our codecs and log what the bot would receive.
/// Test mode: decrypted files are written to `received/` for inspection.
/// In send-only mode none of that happens: the message is only counted.
async fn handle(
    prefix: &str,
    m: &messaging::MessageInfo,
    seen: &mut Seen,
    my_inbox: &str,
    convos: &mut envelope::Conversations,
    fwd: &bridge::Forwarder,
    floor_ms: i64,
    send_only: Option<&mut SendOnly>,
) {
    if !seen.first(&m.id) {
        tracing::debug!(id = %m.id, "duplicate, skipped");
        return;
    }
    // At or below the boot floor = already handed to the bot (or older than
    // this daemon's first run). Both the catch-up AND the live stream can hand
    // back old messages.
    if m.sent_at_ms <= floor_ms {
        tracing::debug!(id = %m.id, "before cursor, skipped");
        return;
    }
    if m.sender_inbox_id == my_inbox {
        return; // our own sends echo back on the stream
    }
    if let Some(send_only) = send_only {
        send_only.note(m);
        return;
    }
    let content = match codec::decode(&m.encoded_content_bytes).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                "{prefix} id={} type={:?} decode failed: {e:#}",
                m.id,
                codec::type_of(&m.encoded_content_bytes)
            );
            return;
        }
    };
    // Group message: refresh members BEFORE building the envelope, so both the
    // envelope and the group_members update carry the current list.
    let forwards = !matches!(
        content,
        codec::Content::ReadReceipt | codec::Content::Unsupported { .. }
    );
    let group_body = if forwards {
        convos.refresh_group(&m.conversation_topic).await
    } else {
        None
    };
    let Some(env) = envelope::build(m, &content, convos).await else { return };
    // Log the exact envelope, with file bodies shortened to their size.
    let mut json = serde_json::to_value(&env).unwrap_or_default();
    shorten_b64(&mut json);
    tracing::info!("{prefix} envelope {json}");
    if let Some(body) = group_body {
        fwd.forward(bridge::Outbound::GroupMembers {
            conversation_id: m.conversation_topic.clone(),
            body,
        });
    }
    fwd.forward(bridge::Outbound::Message(env));
}

/// Replace base64 bodies (`content` of attachments) with `<N b64 chars>`.
fn shorten_b64(v: &mut serde_json::Value) {
    let is_file = v.get("contentType").and_then(|t| t.as_str()) == Some("attachment");
    if let Some(obj) = v.as_object_mut() {
        if is_file {
            if let Some(serde_json::Value::String(s)) = obj.get_mut("content") {
                *s = format!("<{} b64 chars>", s.len());
            }
        }
        if let Some(serde_json::Value::Array(items)) = obj.get_mut("attachments") {
            for it in items {
                if let Some(serde_json::Value::String(s)) = it.get_mut("content") {
                    *s = format!("<{} b64 chars>", s.len());
                }
            }
        }
    }
}

#[allow(dead_code)]
fn log_message(prefix: &str, m: &messaging::MessageInfo) {
    tracing::info!(
        "{prefix} id={} from={} topic={} {}",
        m.id,
        m.sender_inbox_id,
        m.conversation_topic,
        describe(&m.encoded_content_bytes)
    );
}

/// Read a hex key from `path`, or create one (32 random bytes) if the file is absent.
fn load_or_create_key(path: &PathBuf, what: &str) -> Result<Vec<u8>> {
    if path.exists() {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {what}"))?;
        let key = hex::decode(text.trim().trim_start_matches("0x"))
            .with_context(|| format!("{what} is not hex"))?;
        if key.len() != 32 {
            return Err(anyhow!("{what} must be 32 bytes, got {}", key.len()));
        }
        return Ok(key);
    }
    let key = client::generate_private_key();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_private(path, &hex::encode(&key)).with_context(|| format!("write {what}"))?;
    tracing::warn!("created a new {what} at {}", path.display());
    Ok(key)
}

/// Create `path` with `text` in it, readable by its owner only (mode 600 on unix).
fn write_private(path: &PathBuf, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

/// Sync everything (all consent states, so a stranger's first DM counts),
/// then hand every message since `since_ms` to [`handle`] (de-duped there).
async fn catch_up(
    since_ms: i64,
    seen: &mut Seen,
    my_inbox: &str,
    convos: &mut envelope::Conversations,
    fwd: &bridge::Forwarder,
    floor_ms: i64,
    send_only: &mut Option<SendOnly>,
) -> Result<()> {
    let synced = conversations::sync_all(None).await?;
    let list = conversations::list_conversations(None).await?;
    tracing::info!(synced, conversations = list.len(), since_ms, floor_ms, "catch-up");
    // One time-ordered list across conversations: the cursor only moves
    // forward, so delivering out of order could skip a message on a restart.
    let mut all = Vec::new();
    for c in &list {
        all.extend(messaging::get_messages_after_date_by_topic(c.topic.clone(), since_ms).await?);
    }
    all.sort_by_key(|m| m.sent_at_ms);
    for m in &all {
        handle("backlog", m, seen, my_inbox, convos, fwd, floor_ms, send_only.as_mut()).await;
    }
    Ok(())
}

/// Sleep for `d`, or return early with `true` on Ctrl-C (= quit).
async fn wait_or_quit(d: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c, shutting down");
            true
        }
    }
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

/// The daemon runs as a spawned task on a worker thread with a 16 MB stack.
/// libxmtp's futures are very large, and the whole daemon as one future
/// overflowed Windows' 1 MB main-thread stack under `#[tokio::main]`.
fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    rt.block_on(async { tokio::spawn(run()).await? })
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "xmtp_daemon=info,warn".into()),
        )
        .init();

    let env = std::env::var("XMTP_ENV").unwrap_or_else(|_| "production".into());
    let key = load_or_create_key(&env_path("XMTP_KEY_FILE", "secrets/bot.key"), "bot key")?;
    let db_key =
        load_or_create_key(&env_path("XMTP_DB_KEY_FILE", "secrets/db.key"), "db key")?;

    let address = client::initialize_client(key, db_key, env.clone()).await?;
    let inbox_id = client::get_client_inbox_id()?;
    tracing::info!(%env, %address, %inbox_id, "XMTP client ready");

    // Catch-up: messages sent while the daemon was down. Sync everything
    // (all consent states, so a first DM from a stranger counts), then log the
    // last 24 h per conversation.
    let mut seen = Seen::new();
    let mut convos = envelope::Conversations::default();
    // No default XMTP_WEBHOOK_URL on purpose: unset = log-only, never a live bot.
    // Boot floor: the last message the bot accepted, or "now" on a first run
    // (a fresh daemon forwards nothing old).
    let cursor_path = env_path("XMTP_DB_DIR", "./xmtp_data").join("forwarded_cursor");
    // A first run SAVES its floor at once: otherwise a restart before the
    // first delivery would move "now" forward and drop what arrived meanwhile.
    let floor_ms = match bridge::load_cursor(&cursor_path) {
        Some(ms) => ms,
        None => {
            let now = chrono_now_ms();
            bridge::save_cursor(&cursor_path, now);
            now
        }
    };
    // XMTP_WEBHOOK_URL; GPETTE_URL is still read as an older alias.
    let webhook = std::env::var("XMTP_WEBHOOK_URL")
        .or_else(|_| std::env::var("GPETTE_URL"))
        .ok()
        .filter(|u| !u.is_empty());
    let has_webhook = webhook.is_some();
    let fwd = bridge::Forwarder::start(webhook, cursor_path.clone());
    let api_addr = std::env::var("XMTP_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:3100".into());
    let api = tokio::spawn(bridge::serve(api_addr, address.clone(), inbox_id.clone(), fwd.clone()));
    // Watch mode (off unless XMTP_WATCH_CMD and XMTP_WATCH_TO are set).
    let watcher = watch::start_from_env(env_path("XMTP_DB_DIR", "./xmtp_data"));
    // Watch mode with no webhook is send-only: nothing here reads a message.
    let mut send_only = match &watcher {
        Some(w) if !has_webhook => {
            tracing::info!("send-only: received messages are counted, never decoded or logged");
            Some(SendOnly { incoming: w.incoming.clone(), cursor_path, cursor_ms: floor_ms })
        }
        _ => None,
    };
    // Supervisor: catch up, then stream. When the stream ends (network drop,
    // libxmtp error), re-read the gap and resubscribe with backoff instead of
    // exiting. `seen` de-dups the overlap between the re-read and what the
    // stream already delivered. Ctrl-C exits cleanly from any wait.
    // Read back 24 h, but `handle` drops everything at or below `floor_ms`.
    let mut since_ms = floor_ms.min(chrono_now_ms() - 24 * 3600 * 1000);
    let mut backoff = Duration::from_secs(5);
    'outer: loop {
        let caught_up_at = chrono_now_ms();
        // Box::pin: libxmtp's futures are huge; nested inline in main's future
        // they overflowed the 1 MB Windows main-thread stack .
        if let Err(e) = Box::pin(catch_up(
            since_ms, &mut seen, &inbox_id, &mut convos, &fwd, floor_ms, &mut send_only,
        ))
        .await
        {
            tracing::warn!("catch-up failed: {e:#}; retrying in {backoff:?}");
            if wait_or_quit(backoff).await {
                break 'outer;
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
            continue;
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let stream = tokio::spawn(messaging::subscribe_to_all_messages(tx));
        fwd.stream_started_ms
            .store(chrono_now_ms(), std::sync::atomic::Ordering::Relaxed);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => {
                        backoff = Duration::from_secs(5);
                        fwd.last_stream_event_ms
                            .store(chrono_now_ms(), std::sync::atomic::Ordering::Relaxed);
                        handle(
                            "incoming", &m, &mut seen, &inbox_id, &mut convos, &fwd, floor_ms,
                            send_only.as_mut(),
                        )
                        .await;
                    }
                    None => break,
                },
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("ctrl-c, shutting down");
                    stream.abort();
                    break 'outer;
                }
            }
        }
        let why = match stream.await {
            Ok(Ok(())) => "ended".to_string(),
            Ok(Err(e)) => format!("{e:#}"),
            Err(e) => e.to_string(),
        };
        tracing::warn!("message stream stopped ({why}); resubscribing in {backoff:?}");
        // Re-read from a little before this round's catch-up: anything the
        // stream missed while it was dying lands in that window.
        since_ms = caught_up_at - 10 * 60 * 1000;
        if wait_or_quit(backoff).await {
            break;
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
    api.abort();
    if let Some(w) = watcher {
        w.task.abort();
    }
    let _ = client::close_client().await;
    Ok(())
}
