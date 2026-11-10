# AGENTS.md: working with xmtp_daemon

You are an AI agent setting up, running, diagnosing or extending this daemon. Read README.md for the full API; this page is the short operating manual.

## What this repo is, and where it ends

- **It is** the XMTP layer only: one inbox online, incoming messages POSTed to a webhook, outgoing messages through `POST /send`.
- **It is not** a bot. It has no LLM, no replies of its own, no access rules, no memory of conversations beyond XMTP's own database. All of that belongs in the program behind `XMTP_WEBHOOK_URL`.
- **Build on top, don't fork, when you can.** Most needs (who may talk to the bot, what it answers, which model) live in your webhook program. Change this repo only for XMTP-level work: a new content type, a new route, transport fixes. If you do, keep the webhook contract backward compatible.
- A ready-made bot server that speaks this contract exists as a separate project ("gpette"); any HTTP server in any language works too.

## Set up

1. Build: `cargo build --release` (Rust stable; on Windows put Strawberry Perl first on `PATH`).
2. Choose a folder for the keys and the database. Set `XMTP_KEY_FILE`, `XMTP_DB_KEY_FILE`, `XMTP_DB_DIR` to paths inside it. Back the key files up; never commit them.
3. Start your webhook program first, then run the daemon with `XMTP_WEBHOOK_URL` pointing at it.
4. Read the inbox's address and id from the first log lines or from `GET /health`. That address is who people message.
5. Test without a bot: leave `XMTP_WEBHOOK_URL` unset (log-only mode) and send the inbox a message from any XMTP app; it shows in the log.

## Diagnose

| Symptom | Check | Likely cause / fix |
|---|---|---|
| Nothing reaches the bot | `GET /health` → `queued` growing | The webhook is down or slow. Fix the bot; the queue retries on its own. |
| `queued` stays 0, no log lines on new messages | `lastStreamEventMs` vs `streamStartedMs` | The stream died; the daemon resubscribes with backoff. Restart if it doesn't recover. |
| Messages were dropped | log lines `bot refused, dropped` | Your webhook answered `4xx`. Return `2xx` for anything you accept. |
| `/send` returns 504 | log `send timed out` | The XMTP network was slow or stuck. Retry; raise `XMTP_SEND_TIMEOUT_SECS` if it's routine. |
| `/send` returns 400 | the JSON body | Wrong shape; see README § The daemon's API. |
| Start-up fails about installations | `cargo run --example inbox_state -- <inboxId>` | The inbox already has 10 installations. Revoke old ones from a client that holds the key. |
| Old messages delivered again | `{XMTP_DB_DIR}/forwarded_cursor` | The cursor file was deleted or unwritable. |

More logs: `RUST_LOG=xmtp_daemon=debug,warn`.

## Configure

Everything is an environment variable; the table is in README.md § Configuration. There is no config file in this repo by design: the bot behind the webhook owns behaviour.

## Rules for agents

- Never print, log or send the contents of `XMTP_KEY_FILE` or `XMTP_DB_KEY_FILE`.
- Never run two daemons on one `XMTP_DB_DIR`.
- Keep `XMTP_HTTP_ADDR` on localhost.
- Don't create new databases casually: each one uses one of the inbox's 10 installation slots.
