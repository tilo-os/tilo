use std::collections::BTreeSet;

use channel_crypto::{ChannelGroup, DeliveryKind, GovernanceJournal, MemberKey};
use collab_types::{
    ChannelId, EncryptionMetadata, MlsGovernanceAction, MlsGovernanceContext, MlsGovernanceFrameV1,
    generate_keypair,
};

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
