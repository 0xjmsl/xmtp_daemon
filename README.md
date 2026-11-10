# xmtp_daemon

A standalone XMTP client in Rust. It keeps one XMTP inbox online and lets any program send and receive messages, files and galleries over local HTTP, so your program never links an XMTP SDK. Built on [libxmtp](https://github.com/xmtp/libxmtp) v1.9.0. **Not affiliated with XMTP.**

> **Heads-up:** libxmtp v1.9.0 talks to XMTP's hosted network, which XMTP has announced will shut down on **2027-02-01**. Plan to move to whatever replaces it.

> **What this is.** The daemon our own bots run on, shared as it is. It covers what we use (text, files, galleries, DMs, groups) and little else. XMTP's official SDKs and agent tooling are far more complete; use them if you need the full protocol. This is for when you just want an inbox online behind a local HTTP API.

## How it fits

```
 XMTP network  <-->  xmtp_daemon  --POST /api/xmtp/incoming-->  your bot (any language)
                         ^                                            |
                         +------------------ POST /send --------------+
```

- The daemon holds the inbox's key and its encrypted local database.
- Every incoming message is POSTed to your webhook as JSON, in order, with retry.
- Your bot replies by POSTing to the daemon's `/send`.
- Files arrive already downloaded and decrypted; your bot only sees plain files.

## Build

```bash
cargo build --release          # Rust stable. A cold build takes ~6 minutes.
```

On Windows, the OpenSSL build step needs Perl: put Strawberry Perl first on `PATH`. The binary is `target/release/xmtp_daemon(.exe)`.

## Run

```bash
XMTP_WEBHOOK_URL=http://127.0.0.1:5000 ./target/release/xmtp_daemon
```

On the first run it creates the inbox key and the database key. **Keep both secret and backed up**: lose the key and you lose the inbox; lose the database key and you lose the message history.

## Configuration (environment variables)

| Variable | Default | What it does |
|---|---|---|
| `XMTP_WEBHOOK_URL` | unset | Base URL of your bot. **Unset = log-only**: messages are only logged, nothing is POSTed. |
| `XMTP_HTTP_ADDR` | `127.0.0.1:3100` | Where the daemon's own API listens. Keep it on localhost. |
| `XMTP_KEY_FILE` | `secrets/bot.key` | Hex private key of the inbox. Created if missing. |
| `XMTP_DB_KEY_FILE` | `secrets/db.key` | Hex 32-byte key of the local SQLCipher database. Created if missing. |
| `XMTP_DB_DIR` | `./xmtp_data` | Folder of the local database and the delivery cursor. |
| `XMTP_ENV` | `production` | `production`, `dev` or `local`. |
| `XMTP_MAX_DOWNLOAD` | `104857600` | Largest file (bytes) it will download from an incoming message. |
| `XMTP_SEND_TIMEOUT_SECS` | `120` | A `/send` that takes longer returns `504` instead of blocking later sends. |
| `RUST_LOG` | `xmtp_daemon=info,warn` | Log filter. |

`GPETTE_URL` is still read as an older name for `XMTP_WEBHOOK_URL`.

## The daemon's API (your bot calls these)

**`POST /send`** sends a message. Returns `{"id": "<message id>"}`, `400` for a bad body, `502` if XMTP refused, `504` on timeout.

```json
{"conversationId": "<id>", "content": "hello"}

{"conversationId": "<id>", "contentType": "attachment",
 "filename": "photo.png", "mimeType": "image/png",
 "content": "<base64 | data: URL | array of bytes>",
 "description": "an optional caption"}

{"conversationId": "<id>", "contentType": "readReceipt", "content": ""}
```

**`POST /dm`** finds the DM with someone, or opens it, so your bot can write first (a receipt, a hello). Give `inboxId` or an Ethereum `address`. Returns the `conversationId` that `/send` takes and incoming messages carry; the same person always gets the same id. `400` with neither, `404` if the address has no XMTP inbox, `502` if XMTP refused.

```json
{"inboxId": "<their inbox id>"}      ->  {"conversationId": "<id>", "peerInboxId": "<their inbox id>"}
{"address": "0x..."}
```

**`GET /health`** returns `{address, inboxId, queued, streamStartedMs, lastStreamEventMs}`. `queued` is how many messages wait for your webhook. Compare `lastStreamEventMs` with `streamStartedMs` to tell "alive" from "alive but no longer receiving".

**`POST /api/init`** always answers `200`. Handy as an "is it up?" poll.

## The webhook contract (the daemon calls your bot)

**`POST {XMTP_WEBHOOK_URL}/api/xmtp/incoming`**, one message per request:

```json
{
  "id": "...", "senderInboxId": "...", "timestamp": 1730000000000,
  "conversationId": "...", "role": "user",
  "content": "text, or base64 of the first file",
  "contentType": "text | attachment | readReceipt",
  "kind": "dm | group",
  "members": [{"inboxId": "...", "permissionLevel": "member", "identifiers": [...]}],
  "filename": "...", "mimeType": "...", "description": "caption",
  "attachments": [{"content": "<base64>", "filename": "...", "mimeType": "..."}]
}
```

- File fields appear only for attachments. `attachments` lists every file of a gallery; the top-level file fields carry the first one.
- For a DM, `members` is the other person only.
- **`POST {XMTP_WEBHOOK_URL}/api/xmtp/group_members`** sends `{conversationId, members:[...]}` the first time a group is seen.

Delivery rules:
- One ordered queue, at most 1,000 waiting; beyond that the oldest is dropped (logged).
- `2xx` = delivered. `4xx` = your bot refused it: dropped, not retried. Anything else = retried with backoff from 1 s to 30 s.
- After each `2xx`, the message's timestamp is saved to `{XMTP_DB_DIR}/forwarded_cursor`. On restart, anything at or below it is skipped, so nothing is delivered twice. A first run starts at "now": it does not replay old history.

## Content types

| | Receive | Send |
|---|---|---|
| Text | yes | yes |
| Read receipt | yes | yes |
| Inline attachment (with caption) | yes | yes |
| Remote attachment (uploaded file) | yes, downloaded and decrypted | no |
| Gallery (several uploaded files) | yes | no |
| Replies, reactions, other types | logged, not forwarded | no |

## Things to know

- **One daemon per database.** Never run two copies on the same `XMTP_DB_DIR`.
- **Each new database is a new "installation"** of the inbox, and an XMTP inbox holds at most 10. `cargo run --example inbox_state -- <inboxId>` lists them without logging in.
- Keep `XMTP_HTTP_ADDR` on localhost: `/send` has no authentication.

## For AI agents

Read [AGENTS.md](AGENTS.md): setup, diagnosis, configuration, and where to build on top.

## License

MIT, see [LICENSE](LICENSE). Third-party code: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
