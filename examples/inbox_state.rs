//! Read-only: list an inbox's installations, straight from the network.
//! No client, no key, nothing written (ephemeral in-memory DB).
//!
//!   cargo run --example inbox_state -- <inboxId> [<inboxId> ...]
//!
//! Used before a cut-over: each new daemon DB adds an installation, and an
//! inbox holds at most 10.

use xmtp_daemon::api::signing::static_inbox_states_for_inbox_ids;

fn main() -> anyhow::Result<()> {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    if ids.is_empty() {
        anyhow::bail!("usage: inbox_state <inboxId> [<inboxId> ...]");
    }
    // Same reason as main.rs: libxmtp futures need a big stack on Windows.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    rt.block_on(async {
        tokio::spawn(async move {
            for s in static_inbox_states_for_inbox_ids(ids).await? {
                println!("inbox {}", s.inbox_id);
                println!("  identities: {}", s.identities.len());
                println!("  installations: {}", s.installations.len());
                for i in &s.installations {
                    println!("    {} created_at_ms={:?}", i.id, i.created_at);
                }
            }
            anyhow::Ok(())
        })
        .await?
    })
}
