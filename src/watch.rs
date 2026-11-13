//! Watch mode: run check commands on a timer and text their owner when an
//! answer changes. One text when something breaks, one when it is fine again,
//! nothing in between.
//!
//! On when at least one check and `XMTP_WATCH_TO` are set:
//! - `XMTP_WATCH_CMD`           a check, run through the shell (`sh -c`, `cmd /C` on Windows)
//! - `XMTP_WATCH_CMD_<NAME>`    one more check per variable, each with its own answer;
//!                              its texts carry the name in lower case (`..._DISK` -> `disk: `)
//! - `XMTP_WATCH_TO`            who gets the text: an inbox id or a `0x` address;
//!                              several, separated by commas, each get the same text
//! - `XMTP_WATCH_EVERY_SECS`    seconds between rounds of checks (default 60)
//! - `XMTP_WATCH_CONFIRM`       a new answer must repeat this many rounds in a row (default 2)
//! - `XMTP_WATCH_TIMEOUT_SECS`  a check that runs longer is killed (default 30)
//! - `XMTP_WATCH_NAME`          put in front of every text, e.g. `DE box`
//!
//! A check's answer is the FIRST LINE it prints (nothing printed = `ok`). The
//! line must stay the same for as long as the situation is the same: a line
//! that carries a counter or a clock would be a new answer every time, and a
//! text every time. Each check is remembered on its own: one that changes is
//! one text, whatever the others say. The last line texted is kept in
//! `{XMTP_DB_DIR}/watch_state` (`watch_state_<name>` for a named check), so a
//! restart does not repeat it. The very first answer of each check is texted
//! too: it shows the path works. With several recipients the line is saved once
//! every one of them has it; until then each round retries only those missing.
//!
//! Send-only: with watch mode on and no webhook, the daemon never opens a
//! message it receives. It is not decoded and not logged. The recipients get
//! the one fixed text `incoming message` instead, at most once a day (the time
//! is kept in `{XMTP_DB_DIR}/watch_incoming`). A message from one of the
//! recipients themselves (a reply, a read receipt) is not texted about.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::api::conversations;
use crate::outgoing;

/// Longest text sent; a longer first line is cut.
const MAX_TEXT: usize = 500;

/// The one line texted for a received message. Nothing of the message is in it.
pub const INCOMING_LINE: &str = "incoming message";

/// At most one `incoming message` text in this long.
const INCOMING_EVERY_MS: i64 = 24 * 3600 * 1000;

/// Send-only mode: decides when a received message is worth the fixed text.
pub struct Incoming {
    texted_ms: Option<i64>,
    due: bool,
}

impl Incoming {
    /// `texted_ms`: when the last `incoming message` text went out, if ever.
    pub fn new(texted_ms: Option<i64>) -> Self {
        Self { texted_ms, due: false }
    }

    /// True when a message from somebody else would start a text now: none is
    /// on its way and the last one is at least a day old.
    pub fn open(&self, now_ms: i64) -> bool {
        let quiet = self.texted_ms.is_some_and(|t| now_ms - t < INCOMING_EVERY_MS);
        !self.due && !quiet
    }

    /// Somebody who is not a recipient wrote while [`Incoming::open`].
    pub fn note(&mut self) {
        self.due = true;
    }

    pub fn due(&self) -> bool {
        self.due
    }

    /// The text went out to everyone.
    pub fn texted(&mut self, now_ms: i64) {
        self.texted_ms = Some(now_ms);
        self.due = false;
    }
}

/// The text for a received message: `<box>: incoming message`.
pub fn incoming_text(watcher: Option<&str>) -> String {
    match watcher {
        Some(name) => format!("{name}: {INCOMING_LINE}"),
        None => INCOMING_LINE.into(),
    }
}

/// The inbox id `to` is, when it is one. A `0x` address has to be looked up.
pub fn inbox_of(to: &str) -> Option<String> {
    let is_address = to.len() == 42 && to.starts_with("0x");
    (!is_address).then(|| to.trim_start_matches("0x").to_lowercase())
}

/// Decides when an answer is worth a text: it differs from the last one
/// texted and has repeated `confirm` checks in a row.
pub struct Debounce {
    confirm: u32,
    sent: Option<String>,
    candidate: Option<(String, u32)>,
}

impl Debounce {
    pub fn new(confirm: u32, sent: Option<String>) -> Self {
        Self { confirm: confirm.max(1), sent, candidate: None }
    }

    /// Offer the answer of one check. True = text it now.
    pub fn offer(&mut self, line: &str) -> bool {
        if self.sent.as_deref() == Some(line) {
            self.candidate = None;
            return false;
        }
        let n = match &self.candidate {
            Some((l, n)) if l == line => n + 1,
            _ => 1,
        };
        self.candidate = Some((line.to_string(), n));
        n >= self.confirm
    }

    /// The text went out: this answer is now the last one texted.
    pub fn texted(&mut self, line: &str) {
        self.sent = Some(line.to_string());
        self.candidate = None;
    }
}

/// Who still needs the current answer, when it goes to several recipients.
pub struct Fanout {
    line: Option<String>,
    done: Vec<bool>,
}

impl Fanout {
    pub fn new(recipients: usize) -> Self {
        Self { line: None, done: vec![false; recipients] }
    }

    /// The recipients (by position) that do not have `line` yet. A new line
    /// starts over: nobody has it.
    pub fn pending(&mut self, line: &str) -> Vec<usize> {
        if self.line.as_deref() != Some(line) {
            self.line = Some(line.to_string());
            self.done.iter_mut().for_each(|d| *d = false);
        }
        (0..self.done.len()).filter(|i| !self.done[*i]).collect()
    }

    /// Recipient `i` got the current line.
    pub fn delivered(&mut self, i: usize) {
        self.done[i] = true;
    }

    pub fn all_delivered(&self) -> bool {
        self.done.iter().all(|d| *d)
    }
}

/// `XMTP_WATCH_TO` as a list: split on commas and spaces, no empties, no repeats.
pub fn recipients(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let part = part.trim();
        if !part.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(part)) {
            out.push(part.to_string());
        }
    }
    out
}

/// One check: its command, and the name its texts carry (none for `XMTP_WATCH_CMD`).
#[derive(Debug, PartialEq)]
pub struct Check {
    pub name: Option<String>,
    pub cmd: String,
}

impl Check {
    /// The file under the db folder that keeps this check's last texted line.
    pub fn state_file(&self) -> String {
        match &self.name {
            Some(name) => format!("watch_state_{name}"),
            None => "watch_state".into(),
        }
    }

    /// The text for one answer: `<box>: <check>: <line>`, without the parts not set.
    pub fn text(&self, watcher: Option<&str>, line: &str) -> String {
        let mut out = String::new();
        for part in [watcher, self.name.as_deref()].into_iter().flatten() {
            out.push_str(part);
            out.push_str(": ");
        }
        out.push_str(line);
        out
    }
}

/// The checks named by the environment: `XMTP_WATCH_CMD` first, then every
/// `XMTP_WATCH_CMD_<NAME>` by name. A name is letters, digits and `_`, used
/// in lower case; a variable with another kind of name, or an empty command,
/// is not a check.
pub fn checks_from(vars: impl Iterator<Item = (String, String)>) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    for (key, value) in vars {
        let cmd = value.trim();
        if cmd.is_empty() {
            continue;
        }
        if key == "XMTP_WATCH_CMD" {
            out.push(Check { name: None, cmd: cmd.to_string() });
        } else if let Some(suffix) = key.strip_prefix("XMTP_WATCH_CMD_") {
            let ok = !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if ok {
                out.push(Check { name: Some(suffix.to_ascii_lowercase()), cmd: cmd.to_string() });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

struct Config {
    checks: Vec<Check>,
    to: Vec<String>,
    every: Duration,
    confirm: u32,
    timeout: Duration,
    name: Option<String>,
    db_dir: PathBuf,
}

fn env_num(var: &str, default: u64) -> u64 {
    std::env::var(var).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

fn env_text(var: &str) -> Option<String> {
    std::env::var(var).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A running watch loop.
pub struct Watching {
    pub task: tokio::task::JoinHandle<()>,
    /// Send-only mode puts the sender inbox id of every received message here.
    pub incoming: tokio::sync::mpsc::UnboundedSender<String>,
}

/// Start the watch loop if it is configured. `db_dir` holds the state files.
pub fn start_from_env(db_dir: PathBuf) -> Option<Watching> {
    let checks = checks_from(std::env::vars());
    let to = match (checks.is_empty(), env_text("XMTP_WATCH_TO")) {
        (false, Some(to)) => to,
        (true, None) => return None,
        _ => {
            tracing::error!(
                "watch mode needs both a check (XMTP_WATCH_CMD) and XMTP_WATCH_TO; it is OFF"
            );
            return None;
        }
    };
    let to = recipients(&to);
    if to.is_empty() {
        tracing::error!("XMTP_WATCH_TO names nobody; watch mode is OFF");
        return None;
    }
    let cfg = Config {
        checks,
        to,
        every: Duration::from_secs(env_num("XMTP_WATCH_EVERY_SECS", 60).max(1)),
        confirm: env_num("XMTP_WATCH_CONFIRM", 2).max(1) as u32,
        timeout: Duration::from_secs(env_num("XMTP_WATCH_TIMEOUT_SECS", 30).max(1)),
        name: env_text("XMTP_WATCH_NAME"),
        db_dir,
    };
    let (incoming, received) = tokio::sync::mpsc::unbounded_channel();
    Some(Watching { task: tokio::spawn(run(cfg, received)), incoming })
}

/// What the loop keeps per check between rounds.
struct Watched {
    state_path: PathBuf,
    debounce: Debounce,
    fanout: Fanout,
}

/// One of `XMTP_WATCH_TO`, and what is learned about it on first use.
struct Recipient {
    to: String,
    inbox: Option<String>,
    conversation: Option<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn run(cfg: Config, mut received: tokio::sync::mpsc::UnboundedReceiver<String>) {
    let mut watched: Vec<Watched> = Vec::new();
    for check in &cfg.checks {
        let state_path = cfg.db_dir.join(check.state_file());
        let sent = std::fs::read_to_string(&state_path).ok().map(|s| s.trim().to_string());
        tracing::info!(
            check = check.name.as_deref().unwrap_or("-"), cmd = %check.cmd,
            last_texted = ?sent, "watching"
        );
        watched.push(Watched {
            state_path,
            debounce: Debounce::new(cfg.confirm, sent),
            fanout: Fanout::new(cfg.to.len()),
        });
    }
    tracing::info!(
        checks = cfg.checks.len(), to = ?cfg.to, every = ?cfg.every, confirm = cfg.confirm,
        "watch mode on"
    );
    let mut to: Vec<Recipient> = cfg
        .to
        .iter()
        .map(|t| Recipient { to: t.clone(), inbox: inbox_of(t), conversation: None })
        .collect();
    let incoming_path = cfg.db_dir.join("watch_incoming");
    let mut incoming = Incoming::new(
        std::fs::read_to_string(&incoming_path).ok().and_then(|s| s.trim().parse().ok()),
    );
    let mut incoming_fanout = Fanout::new(to.len());
    loop {
        for (check, w) in cfg.checks.iter().zip(watched.iter_mut()) {
            let label = check.name.as_deref().unwrap_or("-");
            let line = run_check(&check.cmd, cfg.timeout).await;
            tracing::debug!(check = label, %line, "check");
            if !w.debounce.offer(&line) {
                continue;
            }
            let text = check.text(cfg.name.as_deref(), &line);
            for i in w.fanout.pending(&line) {
                // Box::pin: libxmtp's futures are huge (see main.rs).
                let sent = Box::pin(text_owner(&mut to[i], &text)).await;
                match sent {
                    Ok(()) => w.fanout.delivered(i),
                    // Not marked: the next round tries this recipient again.
                    Err(e) => tracing::warn!(
                        check = label, %line, to = %to[i].to,
                        "answer changed but the text failed: {e:#}"
                    ),
                }
            }
            if w.fanout.all_delivered() {
                tracing::info!(check = label, %line, "answer changed, texted");
                save_state(&w.state_path, &line);
                w.debounce.texted(&line);
            }
        }
        // Send-only mode: what was received since the last round. Only the
        // sender is looked at, and only until one of them is somebody else.
        while let Ok(sender) = received.try_recv() {
            if incoming.open(now_ms()) && !Box::pin(is_recipient(&mut to, &sender)).await {
                incoming.note();
            }
        }
        if incoming.due() {
            let text = incoming_text(cfg.name.as_deref());
            for i in incoming_fanout.pending(INCOMING_LINE) {
                let sent = Box::pin(text_owner(&mut to[i], &text)).await;
                match sent {
                    Ok(()) => incoming_fanout.delivered(i),
                    Err(e) => tracing::warn!(
                        to = %to[i].to, "a message was received but the text failed: {e:#}"
                    ),
                }
            }
            if incoming_fanout.all_delivered() {
                tracing::info!("a message was received, texted");
                let now = now_ms();
                save_state(&incoming_path, &now.to_string());
                incoming.texted(now);
                incoming_fanout = Fanout::new(to.len());
            }
        }
        tokio::time::sleep(cfg.every).await;
    }
}

/// Is `sender` (an inbox id) one of the recipients? An address that cannot be
/// looked up is nobody's, so its owner counts as somebody else.
async fn is_recipient(to: &mut [Recipient], sender: &str) -> bool {
    for r in to.iter_mut() {
        let inbox = tokio::time::timeout(Duration::from_secs(30), resolve(r)).await;
        if matches!(inbox, Ok(Ok(id)) if id.eq_ignore_ascii_case(sender)) {
            return true;
        }
    }
    false
}

/// Run the check once. Always returns an answer: its first line, `ok` for no
/// output, or a fixed line when the check itself failed.
async fn run_check(cmd: &str, timeout: Duration) -> String {
    let mut command = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => return "check timed out".into(),
        Ok(Err(e)) => {
            tracing::warn!("check could not run: {e}");
            return "check could not run".into();
        }
        Ok(Ok(out)) => out,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.is_empty() {
        if out.status.success() {
            return "ok".into();
        }
        tracing::warn!(
            status = %out.status,
            "check failed with no answer: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return "check failed".into();
    }
    line.chars().take(MAX_TEXT).collect()
}

async fn text_owner(to: &mut Recipient, text: &str) -> Result<()> {
    let limit = Duration::from_secs(120);
    let id = match &to.conversation {
        Some(id) => id.clone(),
        None => {
            let id = tokio::time::timeout(limit, open_dm(to))
                .await
                .map_err(|_| anyhow!("opening the DM timed out"))??;
            to.conversation = Some(id.clone());
            id
        }
    };
    let msg = json!({ "conversationId": id, "content": text });
    tokio::time::timeout(limit, outgoing::send(&msg))
        .await
        .map_err(|_| anyhow!("send timed out"))??;
    Ok(())
}

/// The recipient's inbox id, looked up once when it was given as a `0x` address.
async fn resolve(to: &mut Recipient) -> Result<String> {
    if let Some(inbox) = &to.inbox {
        return Ok(inbox.clone());
    }
    let id = conversations::inbox_id_from_address(to.to.clone()).await?;
    if id.is_empty() {
        return Err(anyhow!("{} has no XMTP inbox", to.to));
    }
    to.inbox = Some(id.clone());
    Ok(id)
}

/// The DM with the recipient, opened if new.
async fn open_dm(to: &mut Recipient) -> Result<String> {
    let inbox = resolve(to).await?;
    Ok(conversations::find_or_create_dm_with_inbox_id(inbox).await?.id)
}

/// Temp file + rename, so a crash never leaves half a line.
fn save_state(path: &std::path::Path, line: &str) {
    let tmp = path.with_extension("tmp");
    let res = std::fs::write(&tmp, line).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        tracing::error!("could not save watch state {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        checks_from, inbox_of, incoming_text, recipients, Check, Debounce, Fanout, Incoming,
    };

    #[test]
    fn a_received_message_is_one_fixed_text_a_day() {
        const HOUR: i64 = 3600 * 1000;
        let mut i = Incoming::new(None);
        assert!(i.open(0) && !i.due());
        i.note();
        // one is on its way: a second message changes nothing
        assert!(i.due() && !i.open(1));
        i.texted(10);
        assert!(!i.due());
        assert!(!i.open(10 + 23 * HOUR));
        assert!(i.open(10 + 24 * HOUR));
        // a restart inside the day stays quiet
        let restarted = Incoming::new(Some(10));
        assert!(!restarted.open(10 + HOUR) && !restarted.due());
    }

    #[test]
    fn the_incoming_text_carries_nothing_of_the_message() {
        assert_eq!(incoming_text(None), "incoming message");
        assert_eq!(incoming_text(Some("DE box")), "DE box: incoming message");
    }

    #[test]
    fn an_inbox_id_is_known_without_a_lookup() {
        assert_eq!(inbox_of("AAbb"), Some("aabb".into()));
        assert_eq!(inbox_of(&format!("0x{}", "AB".repeat(32))), Some("ab".repeat(32)));
        assert_eq!(inbox_of(&format!("0x{}", "ab".repeat(20))), None);
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Iterator<Item = (String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn checks_come_from_the_environment_in_a_fixed_order() {
        let got = checks_from(vars(&[
            ("XMTP_WATCH_CMD_UNITS", "u.sh"),
            ("PATH", "/bin"),
            ("XMTP_WATCH_CMD", " main.sh "),
            ("XMTP_WATCH_CMD_DISK", "d.sh"),
            ("XMTP_WATCH_CMD_EMPTY", "  "),
            ("XMTP_WATCH_CMD_", "x"),
            ("XMTP_WATCH_CMD_BAD-NAME", "x"),
            ("XMTP_WATCH_CONFIRM", "2"),
        ]));
        assert_eq!(
            got,
            vec![
                Check { name: None, cmd: "main.sh".into() },
                Check { name: Some("disk".into()), cmd: "d.sh".into() },
                Check { name: Some("units".into()), cmd: "u.sh".into() },
            ]
        );
        assert!(checks_from(vars(&[("XMTP_WATCH_TO", "aa")])).is_empty());
    }

    #[test]
    fn a_text_names_the_box_and_the_check() {
        let plain = Check { name: None, cmd: "x".into() };
        let disk = Check { name: Some("disk".into()), cmd: "x".into() };
        assert_eq!(plain.text(None, "ok"), "ok");
        assert_eq!(plain.text(Some("DE box"), "ok"), "DE box: ok");
        assert_eq!(disk.text(None, "over 90 %"), "disk: over 90 %");
        assert_eq!(disk.text(Some("DE box"), "check failed"), "DE box: disk: check failed");
        assert_eq!(plain.state_file(), "watch_state");
        assert_eq!(disk.state_file(), "watch_state_disk");
    }

    #[test]
    fn recipients_are_split_trimmed_and_unique() {
        assert_eq!(recipients(" aa, bb ,,AA
cc "), vec!["aa", "bb", "cc"]);
        assert!(recipients(" , ").is_empty());
    }

    #[test]
    fn only_the_missing_recipients_are_retried() {
        let mut f = Fanout::new(2);
        assert_eq!(f.pending("down"), vec![0, 1]);
        f.delivered(0);
        assert!(!f.all_delivered());
        assert_eq!(f.pending("down"), vec![1]);
        f.delivered(1);
        assert!(f.all_delivered());
        // a new answer goes to everyone again
        assert_eq!(f.pending("ok"), vec![0, 1]);
    }

    #[test]
    fn first_answer_is_texted_after_confirm() {
        let mut d = Debounce::new(2, None);
        assert!(!d.offer("ok"));
        assert!(d.offer("ok"));
        d.texted("ok");
        assert!(!d.offer("ok"));
    }

    #[test]
    fn one_text_per_change() {
        let mut d = Debounce::new(2, Some("ok".into()));
        assert!(!d.offer("ok"));
        assert!(!d.offer("MISSING a"));
        assert!(d.offer("MISSING a"));
        d.texted("MISSING a");
        for _ in 0..50 {
            assert!(!d.offer("MISSING a"));
        }
        assert!(!d.offer("ok"));
        assert!(d.offer("ok"));
    }

    #[test]
    fn a_blip_is_not_texted() {
        let mut d = Debounce::new(2, Some("ok".into()));
        assert!(!d.offer("MISSING a"));
        assert!(!d.offer("ok"));
        assert!(!d.offer("MISSING a"));
        assert!(!d.offer("ok"));
    }

    #[test]
    fn a_failed_send_is_retried() {
        let mut d = Debounce::new(2, Some("ok".into()));
        assert!(!d.offer("MISSING a"));
        assert!(d.offer("MISSING a"));
        // not marked texted: the next check offers it again
        assert!(d.offer("MISSING a"));
    }

    #[test]
    fn a_restart_does_not_repeat() {
        let mut d = Debounce::new(2, Some("MISSING a".into()));
        assert!(!d.offer("MISSING a"));
        assert!(!d.offer("MISSING a"));
    }
}
