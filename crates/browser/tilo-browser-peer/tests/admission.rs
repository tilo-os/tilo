//! The browser and the broker decide each op's verdict from the same
//! journal, through the same gate.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use channel_crypto::{ChannelSteward, GovernanceJournal, VaultKey};
use collab_types::inbound::OpAdmission;
use collab_types::{ContentRetention, document_artifact_id, document_channel_id};
use tilo_action_broker::collab_core::BrokerChannels;
use tilo_browser_peer::{DocumentRoute, document_admission};

#[test]
fn the_browser_and_the_broker_give_one_verdict_for_one_journal() {
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: "00000000-0000-4000-8000-000000000003".into(),
        record: String::new(),
    };
    let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let dir = tempfile::tempdir().unwrap();
    let steward = ChannelSteward::open_or_create(
        dir.path(),
        channel,
        ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        &VaultKey::new([8; 32]),
    )
    .unwrap();
    let (sealed, _) = steward
        .content_keys()
        .unwrap()
        .seal_content(
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record),
            ContentRetention::Delta,
            b"sealed",
        )
        .unwrap();
    for (journal, cleartext) in [
        (
            GovernanceJournal::in_memory(),
            OpAdmission::CleartextAdmitted,
        ),
        (steward.journal().clone(), OpAdmission::EncryptionRequired),
    ] {
        let mut broker = BrokerChannels::default();
        broker.governance.insert(channel, journal.clone());
        for (encryption, verdict) in [
            (None, cleartext),
            (Some(&sealed), OpAdmission::EncryptionRequired),
        ] {
            assert_eq!(broker.document_admission(channel, encryption), verdict);
            assert_eq!(
                document_admission(Some(&journal), true, false, &route, encryption),
                verdict
            );
        }
    }
}

#[test]
fn a_page_holding_a_channels_keys_refuses_cleartext_with_no_journal() {
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: "00000000-0000-4000-8000-000000000003".into(),
        record: String::new(),
    };
    for known in [false, true] {
        assert_eq!(
            document_admission(None, known, true, &route, None),
            OpAdmission::EncryptionRequired
        );
    }
    assert_eq!(
        document_admission(None, true, false, &route, None),
        OpAdmission::CleartextAdmitted
    );
}

/// Before the host answered `Ready` for a room, the page has not heard its
/// governance: cleartext waits rather than opening, whatever the page's
/// mirror holds of the channel so far.
#[test]
fn cleartext_before_the_rooms_governance_is_known_is_never_admitted() {
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: "00000000-0000-4000-8000-000000000003".into(),
        record: String::new(),
    };
    for journal in [None, Some(GovernanceJournal::in_memory())] {
        assert_eq!(
            document_admission(journal.as_ref(), false, false, &route, None),
            OpAdmission::Unstated
        );
        assert_eq!(
            document_admission(journal.as_ref(), false, false, &route, None).admits_cleartext(),
            None
        );
    }
}
