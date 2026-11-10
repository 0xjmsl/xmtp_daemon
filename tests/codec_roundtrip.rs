//! Our encoders read back by our own decoder, and every `outgoing_message`
//! shape a bot can POST to `/send`. No network: nothing is sent.

use serde_json::json;
use xmtp_daemon::codec::{self, Content, FileItem};
use xmtp_daemon::outgoing;

fn png() -> FileItem {
    FileItem {
        filename: "pic.png".into(),
        mime_type: "image/png".into(),
        data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
    }
}

#[tokio::test]
async fn text_roundtrip() {
    let c = codec::decode(&codec::encode_text("hola 👋")).await.unwrap();
    assert!(matches!(c, Content::Text(t) if t == "hola 👋"));
}

#[tokio::test]
async fn attachment_keeps_caption() {
    let bytes = codec::encode_attachment(&png(), Some("  beach day "));
    match codec::decode(&bytes).await.unwrap() {
        Content::Files { items, description } => {
            assert_eq!(description.as_deref(), Some("beach day"));
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].filename, "pic.png");
            assert_eq!(items[0].mime_type, "image/png");
            assert_eq!(items[0].data, png().data);
        }
        other => panic!("expected Files, got {other:?}"),
    }
}

#[tokio::test]
async fn attachment_without_caption_has_none() {
    match codec::decode(&codec::encode_attachment(&png(), None)).await.unwrap() {
        Content::Files { description, .. } => assert_eq!(description, None),
        other => panic!("expected Files, got {other:?}"),
    }
}

#[tokio::test]
async fn read_receipt_roundtrip() {
    let c = codec::decode(&codec::encode_read_receipt()).await.unwrap();
    assert!(matches!(c, Content::ReadReceipt));
}

#[test]
fn outgoing_text_is_pushed() {
    let (topic, bytes, push) =
        outgoing::encode(&json!({"conversationId": "ab12", "content": "hi", "tool_calls": null}))
            .unwrap();
    assert_eq!(topic, "ab12");
    assert!(push);
    assert_eq!(codec::type_of(&bytes).as_deref(), Some("xmtp.org/text"));
}

#[test]
fn outgoing_read_receipt_is_silent() {
    let (_, bytes, push) =
        outgoing::encode(&json!({"conversationId": "ab12", "contentType": "readReceipt", "content": ""}))
            .unwrap();
    assert!(!push, "a read receipt must not ring the phone");
    assert_eq!(codec::type_of(&bytes).as_deref(), Some("xmtp.org/readReceipt"));
}

#[tokio::test]
async fn outgoing_attachment_accepts_all_three_encodings() {
    let raw = png().data;
    let b64 = "iVBORwECAw=="; // base64 of the 7 bytes above
    for content in [
        json!(b64),
        json!(format!("data:image/png;base64,{b64}")),
        json!(raw.clone()),
    ] {
        let (_, bytes, push) = outgoing::encode(&json!({
            "conversationId": "ab12",
            "contentType": "attachment",
            "filename": "pic.png",
            "mimeType": "image/png",
            "content": content,
        }))
        .unwrap();
        assert!(push);
        match codec::decode(&bytes).await.unwrap() {
            Content::Files { items, .. } => assert_eq!(items[0].data, raw),
            other => panic!("expected Files, got {other:?}"),
        }
    }
}

#[test]
fn outgoing_rejects_bad_input() {
    assert!(outgoing::encode(&json!({"content": "no conversation"})).is_err());
    assert!(outgoing::encode(&json!({"conversationId": "ab12", "content": ""})).is_err());
    assert!(outgoing::encode(&json!({
        "conversationId": "ab12", "contentType": "attachment", "content": [1, 999]
    }))
    .is_err());
}
