#![allow(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one end-to-end journey reads as one test"
)]
use super::*;
use crate::external_broker::receipt_tests::{Fixture, fixture as fixture_account};
use std::sync::atomic::{AtomicBool, Ordering};
use whatsapp_rust::wacore::{
    proto_helpers::MessageBuilderExt,
    types::message::{MessageInfo, MessageSource},
};
struct Grant(AtomicBool);
impl AccountAuthority for Grant {
    fn check(&self) -> Result<(), SendError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }
}
fn event(chat: &str, sender: &str, text: &str, group: bool) -> InboundMessage {
    InboundMessage::builder()
        .message(Arc::new(whatsapp_rust::waproto::whatsapp::Message::text(
            text,
        )))
        .info(Arc::new(MessageInfo {
            source: MessageSource {
                chat: chat.parse().unwrap(),
                sender: sender.parse().unwrap(),
                is_group: group,
                ..Default::default()
            },
            id: "same-stanza".into(),
            timestamp: "2026-10-11T00:00:00Z".parse().unwrap(),
            ..Default::default()
        }))
        .build()
}
fn group(author: &str) -> GroupIncoming {
    GroupIncoming {
        group: "123@g.us".into(),
        author: author.into(),
        id: "same-stanza".into(),
        text: "private mixed group fixture".into(),
        timestamp_ms: 1_791_676_800_000,
        outgoing: false,
    }
}
fn events(fixture: &Fixture) -> Vec<InboundMessage> {
    vec![
        event(
            &fixture.route.recipient,
            &fixture.route.recipient,
            "private mixed direct fixture",
            false,
        ),
        event("123@g.us", "111@lid", "private mixed group fixture", true),
    ]
}

/// A mixed SDK batch carries direct and group messages together: both private
/// histories commit once and restore encrypted, a conflict or a foreign source
/// commits neither, a revocation stops both, and one stanza id in different
/// authors or chats never merges.
#[tokio::test]
async fn whatsapp_mixed_batch_journey_commits_both_histories_once_or_not_at_all() {
    let fixture = fixture_account().await;
    let grant = Arc::new(Grant(AtomicBool::new(true)));
    assert_eq!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                events(&fixture),
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                events(&fixture),
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .unwrap(),
        0,
        "a redelivered batch commits nothing twice"
    );
    let page = fixture
        .broker
        .client()
        .history()
        .read(fixture.route.clone(), None)
        .await
        .unwrap();
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private mixed direct fixture")
    );
    assert!(
        !page
            .messages
            .iter()
            .any(|m| m.text.as_str() == "private mixed group fixture")
    );
    let groups = fixture
        .broker
        .group_history("123@g.us".into(), grant.clone())
        .await
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].author, "111@lid");
    let encrypted = std::fs::read(fixture.root.path().join("history/history.enc")).unwrap();
    for marker in [
        "private mixed group fixture",
        "private mixed direct fixture",
        "123@g.us",
        "111@lid",
    ] {
        assert!(
            !encrypted
                .windows(marker.len())
                .any(|w| w == marker.as_bytes())
        );
    }
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "durability must not send network messages"
    );
    let Fixture {
        root,
        owner,
        route,
        broker,
        ..
    } = fixture;
    drop(broker);
    let restored = crate::signal_broker::History::open(
        &root.path().join("history"),
        &owner,
        secret_vault::VaultKey::from_bytes([8; 32]),
    )
    .unwrap();
    assert_eq!(restored.group_inbox("123@g.us").unwrap().len(), 1);
    assert!(
        restored
            .page(route, 128)
            .unwrap()
            .messages
            .iter()
            .any(|m| m.text.as_str() == "private mixed direct fixture")
    );

    // A conflicting group review, an invalid direct event and a foreign SDK
    // source each leave both histories exactly as they were.
    let fixture = fixture_account().await;
    let grant = Arc::new(Grant(AtomicBool::new(true)));
    let before = std::fs::read(fixture.root.path().join("history/history.enc")).unwrap();
    let mut conflict = group("111@lid");
    conflict.text = "conflicting private group text".into();
    let mut batch = events(&fixture);
    batch.push(batch[1].clone());
    assert!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                batch,
                vec![group("111@lid"), conflict],
                Some(grant.clone())
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("history/history.enc")).unwrap(),
        before
    );
    let mut invalid = events(&fixture);
    Arc::make_mut(&mut invalid[0].message)
        .ephemeral_message
        .get_or_insert_default();
    assert!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                invalid,
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .is_err()
    );
    let other = fixture_account().await;
    assert!(
        fixture
            .broker
            .observe_mixed(
                other.source.clone(),
                events(&fixture),
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("history/history.enc")).unwrap(),
        before
    );

    // Revoking the original direct account or the group grant stops both
    // histories together.
    fixture.revoked.store(true, Ordering::Release);
    assert!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                events(&fixture),
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("history/history.enc")).unwrap(),
        before,
        "revoked original direct authority cannot partly admit the group"
    );
    fixture.revoked.store(false, Ordering::Release);
    grant.0.store(false, Ordering::Release);
    assert!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                events(&fixture),
                vec![group("111@lid")],
                Some(grant.clone())
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .broker
            .group_history("123@g.us".into(), grant)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.path().join("history/history.enc")).unwrap(),
        before
    );

    // The same stanza id from different authors and chats stays separate.
    let fixture = fixture_account().await;
    let grant = Arc::new(Grant(AtomicBool::new(true)));
    let mut other = group("111@lid");
    other.group = "456@g.us".into();
    let batch = vec![
        event("123@g.us", "111@lid", "private mixed group fixture", true),
        event("123@g.us", "222@lid", "private mixed group fixture", true),
        event("456@g.us", "111@lid", "private mixed group fixture", true),
    ];
    assert_eq!(
        fixture
            .broker
            .observe_mixed(
                fixture.source.clone(),
                batch,
                vec![group("111@lid"), group("222@lid"), other],
                Some(grant.clone())
            )
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        fixture
            .broker
            .group_history("123@g.us".into(), grant.clone())
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fixture
            .broker
            .group_history("456@g.us".into(), grant)
            .await
            .unwrap()
            .len(),
        1
    );
}
