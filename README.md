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

On the first run it creates the inbox key and the database key (on Linux and macOS as files only their owner can read, mode 600). **Keep both secret and backed up**: lose the key and you lose the inbox; lose the database key and you lose the message history.

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

## Watch mode (a monitor that texts you)

The daemon can also run check commands on a timer and text you when an answer changes. One text when something breaks, one when it is fine again, nothing in between. No bot or webhook is needed.

```bash
XMTP_WATCH_CMD="sh /opt/checks/disk.sh" XMTP_WATCH_TO=<your inbox id> ./target/release/xmtp_daemon
```

| Variable | Default | What it does |
|---|---|---|
| `XMTP_WATCH_CMD` | unset | A check, run through the shell (`sh -c`; `cmd /C` on Windows). Watch mode is on when at least one check and `XMTP_WATCH_TO` are set. |
| `XMTP_WATCH_CMD_<NAME>` | unset | One more check per variable (`XMTP_WATCH_CMD_DISK`, `XMTP_WATCH_CMD_BACKUP`, ...). Each has its own answer, and its texts carry the name in lower case: `disk: almost full`. The name is letters, digits and `_`. |
| `XMTP_WATCH_TO` | unset | Who gets the text: an inbox id, or a `0x` address that has an inbox. Several, separated by commas, each get the same text. |
| `XMTP_WATCH_EVERY_SECS` | `60` | Seconds between rounds. In a round the checks run one after the other. |
| `XMTP_WATCH_CONFIRM` | `2` | A new answer must repeat this many rounds in a row before it is texted, so a blip is not. |
| `XMTP_WATCH_TIMEOUT_SECS` | `30` | A check that runs longer is killed and answers `check timed out`. |
| `XMTP_WATCH_NAME` | unset | Put in front of every text, e.g. `web-1`. |

- **The answer is the first line the check prints.** Nothing printed and exit 0 is `ok`; nothing printed and a failure is `check failed`.
- **Keep the line the same while the situation is the same.** `disk almost full on /data` is good; `disk at 91.3%` is a new answer every minute, and a text every minute.
- **Each check is remembered on its own.** A check whose answer changes is one text, whatever the others say. A text reads `<XMTP_WATCH_NAME>: <check name>: <line>`, without the parts that are not set.
- The last line texted is kept in `{XMTP_DB_DIR}/watch_state` (`watch_state_<name>` for a named check), so a restart does not repeat it. Delete the file to be texted the current answer again.
- The very first answer of each check is texted too. It shows the path works, and it opens the DM: the first text from a new inbox arrives as a message request.
- If the text cannot be sent, nothing is saved and the next round tries again. With several recipients, only those still missing it are retried.
- **With no `XMTP_WEBHOOK_URL`, watch mode is send-only.** The daemon never opens a message it receives: it is not decoded and not logged. The recipients get the fixed text `<XMTP_WATCH_NAME>: incoming message` instead, at most once a day (the time is kept in `{XMTP_DB_DIR}/watch_incoming`). A message from one of the recipients themselves, such as a reply or a read receipt, is not texted about. The XMTP library still stores what it receives in its encrypted database; to read a message, open that database with its keys on another machine.

## The daemon's API (your bot calls these)

**`POST /send`** sends a message. Returns `{"id": "<message id>"}`, `400` for a bad body, `502` if XMTP refused, `504` on timeout.

```json
{"conversationId": "<id>", "content": "hello"}

{"conversationId": "<id>", "contentType": "attachment",
 "filename": "photo.png", "mimeType": "image/png",
 "content": "<base64 | data: URL | array of bytes>",
 "description": "an optional caption"}

{"conversationId": "<id>", "contentType": "readReceipt", "content": ""}

{"conversationId": "<id>", "contentType": "actions",
 "actions": {"id": "<menu id>", "description": "Pick one",
             "actions": [{"id": "status", "label": "Status", "style": "primary"},
                         {"id": "model", "label": "Model"}]}}
```

`actions` is a menu of buttons (`coinbase.com/actions` 1.0). Apps that know it show buttons and answer with an intent (below); every other app shows the fallback the daemon writes, a numbered list ending "Reply with the number to select". `style` is `primary`, `secondary` (the default) or `danger`.

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
  "contentType": "text | attachment | readReceipt | intent",
  "kind": "dm | group",
  "members": [{"inboxId": "...", "permissionLevel": "member", "identifiers": [...]}],
  "filename": "...", "mimeType": "...", "description": "caption",
  "attachments": [{"content": "<base64>", "filename": "...", "mimeType": "..."}]
}
```

- A button press arrives as `contentType: "intent"`, `content` = the button id, and `intent: {"id": "<menu id>", "actionId": "<button id>", "metadata": {...}}`.
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
| Menu of buttons (`coinbase.com/actions`) | no | yes, with a numbered fallback |
| Button press (`coinbase.com/intent`) | yes | no |
| Replies, reactions, other types | logged, not forwarded | no |

## Things to know

- **One daemon per database.** Never run two copies on the same `XMTP_DB_DIR`.
- **Each new database is a new "installation"** of the inbox, and an XMTP inbox holds at most 10. `cargo run --example inbox_state -- <inboxId>` lists them without logging in.
- Keep `XMTP_HTTP_ADDR` on localhost: `/send` has no authentication.

## For AI agents

Read [AGENTS.md](AGENTS.md): setup, diagnosis, configuration, and where to build on top.

## License

MIT, see [LICENSE](LICENSE). Third-party code: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
