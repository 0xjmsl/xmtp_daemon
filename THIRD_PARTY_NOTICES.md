# Third-party notices

- **libxmtp** (https://github.com/xmtp/libxmtp), tag v1.9.0, used as git dependencies (`xmtp_mls`, `xmtp_id`, `xmtp_db`, `xmtp_api*`, `xmtp_content_types`, `xmtp_proto`). MIT License, Copyright (c) 2023 XMTP. This project is not affiliated with or endorsed by XMTP.
- **hpke-rs** (https://github.com/cryspen/hpke-rs), tag v0.4.0, vendored **unmodified** in `deps/hpke-rs` because libxmtp's crypto stack needs `^0.4.0` while the upstream default branch is 0.5.x. Mozilla Public License 2.0 (https://mozilla.org/MPL/2.0/). Its source is available unchanged at the tag above.
- **diesel** fork by ephemerahq (https://github.com/ephemerahq/diesel, branch `coda/copy-to-sqlite`), used as a git dependency as libxmtp requires. MIT OR Apache-2.0.
- `src/api/` is the Rust facade from **xmtp_plugin** (https://pub.dev/packages/xmtp_plugin), MIT, by the same author. Some of its comments still mention the Dart bindings it was written for.
