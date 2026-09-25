use std::collections::BTreeSet;

use channel_crypto::{ChannelGroup, DeliveryKind, GovernanceJournal, MemberKey};
use collab_types::{
    ChannelId, EncryptionMetadata, MlsGovernanceAction, MlsGovernanceContext, MlsGovernanceError,
    MlsGovernanceFrameV1, generate_keypair,
};

/// A complete `JournalState` V1 containing one frame signed over the original
/// `qos.mls-governance.v1` domain. The key, UUID, transaction id, action and
/// signature are fixed so this remains an external wire fixture rather than
/// being regenerated through the implementation under test.
const LEGACY_QOS_JOURNAL_HEX: &str = "010101111111111111111111111111111111111111111111111111111111111111111110359d126a15574730a029c4081e53d95701000000000000000000000000000000000000000000000000000000000000000000022222222222222222222222222222222222222222222222222222222222222222eb82f3e16d0570a87e1eeb9b1190668af26abd423a10a560666e4b11fca08f7c40a216f3fd74a0fd8ac9df5624251ef5b1ed115780defc18c6b76d99d518e5a2b36f5ffaf1a05733e35b14b77d6bcb58adb24f9a41d4bbc53f8e7b27b72b7349040001";

fn legacy_qos_journal_bytes() -> Vec<u8> {
    LEGACY_QOS_JOURNAL_HEX
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |byte| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid fixture hex"),
            };
            nibble(pair[0]) << 4 | nibble(pair[1])
        })
        .collect()
}

fn write_legacy_qos_journal(path: &std::path::Path) {
    std::fs::write(path, legacy_qos_journal_bytes()).unwrap();
}

#[test]
fn legacy_qos_journal_opens_but_legacy_frame_stays_invalid_on_live_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-qos-governance.log");
    write_legacy_qos_journal(&path);

    let journal = GovernanceJournal::open(&path).expect("open legacy qOS journal");
    let frame = journal
        .frame_for_transaction([0x11; 32])
        .expect("literal fixture contains its signed frame");
    assert!(
        frame.verify_signature().is_err(),
        "legacy signatures must remain invalid on the current live path"
    );

    let members = BTreeSet::from([frame.signed_by]);
    let context = MlsGovernanceContext {
        channel: frame.channel,
        membership_revision: frame.membership_revision,
        previous_frame_digest: frame.previous_frame_digest,
        expected_epoch: frame.epoch,
        governors: &members,
        desired_members: &members,
        current_mls_members: &members,
        durable_snapshot_ack: Some([0x22; 32]),
    };
    assert!(matches!(
        frame.validate(&context),
        Err(MlsGovernanceError::Signature(_))
    ));
}

#[test]
fn legacy_qos_journal_appends_current_tilo_frame_and_reopens_mixed_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed-governance.log");
    write_legacy_qos_journal(&path);

    let mut journal = GovernanceJournal::open(&path).expect("open legacy qOS journal");
    let head = journal.head_digest();
    let (signer, signer_public) = generate_keypair();
    let members = BTreeSet::from([signer_public]);
    let frame = MlsGovernanceFrameV1::signed(
        journal
            .frame_for_transaction([0x11; 32])
            .expect("legacy head")
            .channel,
        [0x33; 32],
        2,
        head,
        0,
        MlsGovernanceAction::EnableEncryption {
            encrypted_snapshot_digest: [0x44; 32],
        },
        &signer,
    )
    .unwrap();
    let context = MlsGovernanceContext {
        channel: frame.channel,
        membership_revision: 2,
        previous_frame_digest: head,
        expected_epoch: 0,
        governors: &members,
        desired_members: &members,
        current_mls_members: &members,
        durable_snapshot_ack: Some([0x44; 32]),
    };
    journal.enqueue(frame, &context).unwrap();
    drop(journal);

    let reopened = GovernanceJournal::open(&path).expect("reopen mixed qOS/Tilo chain");
    assert!(reopened.frame_for_transaction([0x11; 32]).is_some());
    reopened
        .frame_for_transaction([0x33; 32])
        .expect("current Tilo frame")
        .verify_signature()
        .expect("new frame retains current Tilo signing domain");
}

#[test]
fn tampered_legacy_qos_journal_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tampered-legacy-governance.log");
    let mut bytes = legacy_qos_journal_bytes();
    let signature_tail = bytes.len() - 3;
    bytes[signature_tail] ^= 0x01;
    std::fs::write(&path, bytes).unwrap();

    assert!(GovernanceJournal::open(&path).is_err());
}

#[test]
fn add_is_targeted_durable_replay_safe_and_restartable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("governance.log");
    let channel = ChannelId::new();
    let (alice_sk, alice) = generate_keypair();
    let (_, bob) = generate_keypair();
    let (_, carol) = generate_keypair();
    let governors = BTreeSet::from([alice]);
    let current = BTreeSet::from([alice, bob]);
    let desired = BTreeSet::from([alice, bob, carol]);
    let frame = MlsGovernanceFrameV1::signed(
        channel,
        [1; 32],
        9,
        [0; 32],
        2,
        MlsGovernanceAction::Add {
            recipient: carol,
            welcome: vec![1, 2, 3],
            existing_member_commit: Some(vec![4, 5]),
        },
        &alice_sk,
    )
    .unwrap();
    let context = MlsGovernanceContext {
        channel,
        membership_revision: 9,
        previous_frame_digest: [0; 32],
        expected_epoch: 2,
        governors: &governors,
        desired_members: &desired,
        current_mls_members: &current,
        durable_snapshot_ack: None,
    };
    let mut journal = GovernanceJournal::open(&path).unwrap();
    let digest = journal.enqueue(frame.clone(), &context).unwrap();
    assert_eq!(journal.pending_for(carol).len(), 1);
    assert_eq!(journal.pending_for(carol)[0].kind, DeliveryKind::Welcome);
    assert_eq!(journal.pending_for(bob).len(), 1);
    assert_eq!(journal.pending_for(bob)[0].kind, DeliveryKind::Commit);
    assert!(journal.pending_for(alice).is_empty());
    assert!(journal.enqueue(frame, &context).is_err());
    drop(journal);

    let mut journal = GovernanceJournal::open(&path).unwrap();
    assert_eq!(journal.pending_for(carol).len(), 1);
    journal
        .acknowledge(digest, carol, DeliveryKind::Welcome)
        .unwrap();
    drop(journal);
    assert!(
        GovernanceJournal::open(&path)
            .unwrap()
            .pending_for(carol)
            .is_empty()
    );

    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x40;
    std::fs::write(&path, bytes).unwrap();
    assert!(GovernanceJournal::open(&path).is_err());
}

#[test]
fn encryption_gate_requires_durable_snapshot_ack_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gate.log");
    let channel = ChannelId::new();
    let (alice_sk, alice) = generate_keypair();
    let members = BTreeSet::from([alice]);
    let digest = [42; 32];
    let frame = MlsGovernanceFrameV1::signed(
        channel,
        [2; 32],
        1,
        [0; 32],
        0,
        MlsGovernanceAction::EnableEncryption {
            encrypted_snapshot_digest: digest,
        },
        &alice_sk,
    )
    .unwrap();
    let mut journal = GovernanceJournal::open(&path).unwrap();
    let without_ack = MlsGovernanceContext {
        channel,
        membership_revision: 1,
        previous_frame_digest: [0; 32],
        expected_epoch: 0,
        governors: &members,
        desired_members: &members,
        current_mls_members: &members,
        durable_snapshot_ack: None,
    };
    assert!(journal.enqueue(frame.clone(), &without_ack).is_err());
    assert!(!journal.encryption_required());
    let with_ack = MlsGovernanceContext {
        durable_snapshot_ack: Some(digest),
        ..without_ack
    };
    journal.enqueue(frame, &with_ack).unwrap();
    drop(journal);

    let journal = GovernanceJournal::open(&path).unwrap();
    assert!(journal.encryption_required());
    assert!(!journal.accepts_document_encryption(None, channel));
    assert!(
        journal
            .accepts_document_encryption(Some(&EncryptionMetadata { channel, epoch: 0 }), channel)
    );
    assert!(!journal.accepts_document_encryption(
        Some(&EncryptionMetadata {
            channel: ChannelId::new(),
            epoch: 0
        }),
        channel
    ));
}

#[test]
fn failed_durable_handoff_rolls_back_pending_add() {
    let channel = ChannelId::new();
    let mut alice = ChannelGroup::create(channel, MemberKey::generate("alice").unwrap()).unwrap();
    let bob = MemberKey::generate("bob").unwrap();
    let package = bob.fresh_key_package().unwrap();
    let epoch = alice.epoch();
    let result = alice.add_member_transactional(package.clone(), |_, _, _, _| {
        Err(channel_crypto::Error::Governance(
            "simulated disk failure".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(alice.epoch(), epoch);
    let (welcome, commit) = alice
        .add_member_transactional(package, |_, _, _, _| Ok(()))
        .unwrap();
    assert!(!welcome.is_empty());
    assert!(commit.is_none());
    assert_eq!(alice.epoch(), epoch + 1);
}

#[test]
fn governance_rejects_wrong_route_epoch_outsider_and_inconsistent_removal() {
    let channel = ChannelId::new();
    let (alice_sk, alice) = generate_keypair();
    let (_, bob) = generate_keypair();
    let (_, outsider) = generate_keypair();
    let governors = BTreeSet::from([alice]);
    let current = BTreeSet::from([alice, bob]);
    let desired = BTreeSet::from([alice]);
    let frame = MlsGovernanceFrameV1::signed(
        channel,
        [3; 32],
        4,
        [0; 32],
        8,
        MlsGovernanceAction::Remove {
            removed: bob,
            commit: vec![7],
        },
        &alice_sk,
    )
    .unwrap();
    let valid = MlsGovernanceContext {
        channel,
        membership_revision: 4,
        previous_frame_digest: [0; 32],
        expected_epoch: 8,
        governors: &governors,
        desired_members: &desired,
        current_mls_members: &current,
        durable_snapshot_ack: None,
    };
    assert!(frame.validate(&valid).is_ok());
    assert!(
        frame
            .validate(&MlsGovernanceContext {
                channel: ChannelId::new(),
                ..valid
            })
            .is_err()
    );
    assert!(
        frame
            .validate(&MlsGovernanceContext {
                expected_epoch: 9,
                ..valid
            })
            .is_err()
    );
    let outsider_governors = BTreeSet::from([outsider]);
    assert!(
        frame
            .validate(&MlsGovernanceContext {
                governors: &outsider_governors,
                ..valid
            })
            .is_err()
    );
    let still_present = BTreeSet::from([alice, bob]);
    assert!(
        frame
            .validate(&MlsGovernanceContext {
                desired_members: &still_present,
                ..valid
            })
            .is_err()
    );
}
