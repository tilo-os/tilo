//! A sealed channel end to end through the room's host: the room is hosted
//! on the device of the person who made it, a member another person's
//! device admits to the channel reads what is written in it, and the host
//! keeps only ciphertext.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use collab_types::{
    ContentRetention, DocumentFrame, DocumentMutation, Ed25519PublicKey, OpKind, SignedOp,
    document_artifact_id, document_attestation_id, document_channel_id,
};
use tilo_user_identity::DeviceCapabilities;

use super::home_relay::holds;
use super::{Device, NOTES, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::channel_encryption::{ChannelAccess, ChannelEncryption, ChannelEncryptionConfig};
use crate::shell::channel_policy::WorkspaceChannelDirectory;
use crate::shell::collab_core::BrokerChannels;
use crate::shell::state::RoomId;

/// Text only the room's members may read.
const SECRET: &str = "the quarterly figures, for members only";
const RECORD: &str = "figures";

fn channels_of(device: &Device, dir: &std::path::Path) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: dir.join("channels"),
            vault: channel_crypto::VaultKey::new([9; 32]),
            device_key: device.key(),
        },
        Box::new(WorkspaceChannelDirectory::new(
            device.tree.clone(),
            Box::new(|_| Vec::new()),
        )),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap()
}

/// The channel's keys on `device` once its steward admitted it.
fn keys_on(
    devices: &[&Device],
    device: &Device,
    channels: &mut ChannelEncryption,
    others: &mut [(&Device, &mut ChannelEncryption)],
    room: RoomId,
) -> channel_crypto::ChannelContentKeys {
    let channel = document_channel_id(NOTES, &room.to_string(), RECORD);
    until(devices, "the channel's keys", || {
        let now = tilo_clock::try_unix_now().unwrap();
        for (other, other_channels) in others.iter_mut() {
            other_channels.pump(&**other.net(), Instant::now(), now);
        }
        channels.pump(&**device.net(), Instant::now(), now);
        match channels.access(room, channel, Instant::now()) {
            ChannelAccess::Ready(keys) => Some(keys),
            ChannelAccess::Waiting => None,
            other => panic!("{} is not admitted to the channel: {other:?}", device.label),
        }
    })
}

#[test]
#[serial_test::serial]
fn a_sealed_channel_is_read_by_members_through_a_device_host_that_keeps_only_ciphertext() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(101), Person::new(102));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A71,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B71,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&p, &[&q], "Finance");
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });
    let mut p_channels = channels_of(&p, &dir.path().join("ada-channels"));
    let mut q_channels = channels_of(&q, &dir.path().join("grace-channels"));

    // Ada's device writes into the sealed channel.
    let p_keys = keys_on(&[&p, &q], &p, &mut p_channels, &mut [], room);
    let workspace = room.to_string();
    let artifact = document_artifact_id(NOTES, &workspace, RECORD);
    let plan = {
        use automerge::transaction::Transactable as _;
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "text", SECRET).unwrap();
        document.save()
    };
    let frame = DocumentMutation::Snapshot(plan.clone())
        .encode_frames()
        .unwrap()
        .remove(0);
    let (metadata, ciphertext) = p_keys
        .seal_content(artifact, ContentRetention::Checkpoint, &frame)
        .unwrap();
    let mut op = SignedOp::new_unsigned_with_encryption(
        ciphertext.clone(),
        artifact,
        Ed25519PublicKey::from_bytes(p.cert().device_public_key),
        document_attestation_id(NOTES, &workspace, RECORD),
        OpKind::Change,
        SystemTime::now(),
        1,
        metadata,
    );
    collab_types::sign_op(&mut op, &p.key()).unwrap();
    p.net()
        .publish_document(NOTES.into(), workspace, RECORD, op);

    // Grace's device is admitted to the channel and reads it.
    let q_keys = keys_on(
        &[&p, &q],
        &q,
        &mut q_channels,
        &mut [(&p, &mut p_channels)],
        room,
    );
    let inbound = q.net().inbound_documents_handle();
    let read = until(&[&p, &q], "the sealed record to reach Grace", || {
        let mut inbound = inbound.lock().unwrap();
        let index = inbound
            .iter()
            .position(|document| document.record_id == RECORD)?;
        Some(inbound.remove(index))
    });
    let envelope = *read.envelope;
    let opened = q_keys
        .open_content(
            envelope.artifact,
            envelope.encryption.as_ref().expect("the record is sealed"),
            &envelope.payload,
        )
        .unwrap();
    assert_eq!(
        DocumentFrame::decode(&opened).unwrap(),
        DocumentFrame::Whole(DocumentMutation::Snapshot(plan))
    );

    // What the host keeps: the ciphertext, never the plaintext.
    let host = p.host_state_root();
    assert!(
        holds(&host, &ciphertext),
        "the host retains the sealed record"
    );
    assert!(
        !holds(&host, SECRET.as_bytes()),
        "no byte the host keeps holds the plaintext"
    );
    drop((p, q));
}
