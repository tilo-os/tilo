//! The encrypted channels of a room hosted on a device have one steward.
//!
//! A room placed on this device is governed by the device's own room host,
//! which signs with the device's key. The host stewards the room's channels;
//! the device's broker is only ever a channel member, here as on any other
//! device. Two stewards signing a channel's frames under one key would fork
//! the channel: the member admitted by one could not read what the other's
//! group sealed.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::{
    ContentRetention, MlsGovernanceAction, document_artifact_id, document_channel_id,
};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::channel_encryption::{
    ChannelAccess, ChannelEncryption, ChannelEncryptionConfig, GovernanceTransport,
    InboundChannelGovernance,
};
use crate::shell::channel_policy::WorkspaceChannelDirectory;
use crate::shell::collab_core::BrokerChannels;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};

const NOTES: &str = "so.tilo.notes";

fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn person_of(root: &SigningKey) -> String {
    hex::encode(root.verifying_key().to_bytes())
}

/// A device of `root`'s person, with its device key.
fn device(
    tree: WorkspaceTreeHandle,
    dir: &std::path::Path,
    root: &SigningKey,
    capabilities: DeviceCapabilities,
) -> (Arc<ShellNetwork>, SigningKey) {
    let root = root.clone();
    let device_key = Arc::new(Mutex::new(None));
    let net = {
        let device_key = Arc::clone(&device_key);
        ShellNetwork::spawn_with(
            tree,
            &dir.join("device-private/keypair.bin"),
            &dir.join("peer-sync"),
            Reach::Loopback,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let mut cert = DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: "Device".into(),
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &root).unwrap();
                *device_key.lock().unwrap() = Some(key.clone());
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: std::collections::HashMap::new(),
                    workspace_revocations: std::collections::HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap()
    };
    let key = device_key.lock().unwrap().clone().unwrap();
    (Arc::new(net), key)
}

/// The device's channel service, as its broker opens it: the room's
/// governor, as the device's admitted view names it, is who it asks.
fn channels(
    net: &Arc<ShellNetwork>,
    tree: &WorkspaceTreeHandle,
    dir: &std::path::Path,
    key: &SigningKey,
) -> ChannelEncryption {
    ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: dir.join("channels"),
            vault: channel_crypto::VaultKey::new([9; 32]),
            device_key: key.clone(),
        },
        Box::new(
            WorkspaceChannelDirectory::new(tree.clone(), Box::new(|_| Vec::new()))
                .with_stewards(crate::shell::network::ShellNetwork::room_stewards(net))
                .with_hosted_channel_keys(crate::shell::collab_broker::hosted_channel_keys_of(
                    Arc::clone(net),
                )),
        ),
        Arc::new(Mutex::new(BrokerChannels::default())),
    )
    .unwrap()
}

/// The room transport of a device, keeping every governance frame that
/// passes it: what the device's channel service signed and sent, or
/// received.
struct Recording<'a> {
    net: &'a ShellNetwork,
    sent: RefCell<Vec<collab_types::MlsGovernanceFrameV1>>,
    received: RefCell<Vec<collab_types::MlsGovernanceFrameV1>>,
}

impl<'a> Recording<'a> {
    fn new(net: &'a ShellNetwork) -> Self {
        Self {
            net,
            sent: RefCell::default(),
            received: RefCell::default(),
        }
    }
}

impl GovernanceTransport for Recording<'_> {
    fn publish(&self, message: neighbour_wire::NeighbourMsg) -> bool {
        if let neighbour_wire::NeighbourMsg::MlsGovernance { frame, .. } = &message {
            self.sent.borrow_mut().push((**frame).clone());
        }
        self.net.publish_channel_governance(message)
    }

    fn take_inbound(&self) -> Vec<InboundChannelGovernance> {
        let inbound = self.net.take_inbound();
        for message in &inbound {
            if let InboundChannelGovernance::Frame { frame, .. } = message {
                self.received.borrow_mut().push((**frame).clone());
            }
        }
        inbound
    }
}

/// One line per frame of `channel`: its transaction, the frame it follows,
/// its epoch and what it does, so a failure shows each steward's chain.
fn describe(
    frames: &[collab_types::MlsGovernanceFrameV1],
    channel: collab_types::ChannelId,
) -> String {
    frames
        .iter()
        .filter(|frame| frame.channel == channel)
        .map(|frame| {
            format!(
                "  {} signed {} after {} epoch {} {}",
                hex::encode(&frame.transaction_id[..4]),
                hex::encode(&frame.signature.as_bytes()[..4]),
                hex::encode(&frame.previous_frame_digest[..4]),
                frame.epoch,
                match &frame.action {
                    MlsGovernanceAction::Add { recipient, .. } => {
                        format!("adds {}", hex::encode(&recipient.as_bytes()[..4]))
                    }
                    MlsGovernanceAction::AddWithKeyHistory { recipients, .. } => format!(
                        "adds {} with key history",
                        recipients
                            .iter()
                            .map(|recipient| hex::encode(&recipient.as_bytes()[..4]))
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                    MlsGovernanceAction::Remove { removed, .. } => {
                        format!("removes {}", hex::encode(&removed.as_bytes()[..4]))
                    }
                    MlsGovernanceAction::EnableEncryption { .. } => "enables encryption".into(),
                    MlsGovernanceAction::Genesis => "genesis".into(),
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A collaborator joins a room hosted on the owner's device, and both open
/// one record's channel. Exactly one steward signs the channel's frames,
/// with the device's key, and what the owner's device seals the
/// collaborator's device opens.
#[test]
#[serial_test::serial]
fn a_room_hosted_on_a_device_has_one_channel_steward() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[50; 32]);
    home.env().set("TILO_USER_ID", person_of(&root));

    let mut owner_tree = WorkspaceTree::default();
    // A new workspace waits to be placed on this device's host.
    let (workspace, room) = owner_tree.create_hosted_workspace("Home");
    owner_tree.persist_path = Some(dir.path().join("owner/workspaces.json"));
    let owner_tree = owner_tree.shared();
    let (owner, owner_key) = device(
        owner_tree.clone(),
        &dir.path().join("owner"),
        &root,
        DeviceCapabilities::primary(),
    );
    assert!(owner.join_default_workspace());
    until("the owner's device to host its workspace", || {
        owner.tick();
        owner.administered_room(room)
    });

    let member_tree = WorkspaceTree {
        persist_path: Some(dir.path().join("member/workspaces.json")),
        ..WorkspaceTree::default()
    }
    .shared();
    let (member, member_key) = device(
        member_tree.clone(),
        &dir.path().join("member"),
        &SigningKey::from_bytes(&[51; 32]),
        DeviceCapabilities::member(),
    );
    assert!(member.join_default_workspace());
    let owner_addr = until("the owner's device to listen", || owner.dial_addr());
    member
        .cmd_tx
        .try_send(NetCmd::Connect { addr: owner_addr })
        .unwrap();
    let invitation = owner.issue_invite(workspace, 3600).unwrap();
    let PeerSyncResponse::EnrollmentPending { id } =
        member.accept_peer_invitation(&invitation).unwrap()
    else {
        panic!("redeeming the invitation enrolls with the room's host");
    };
    until("the collaborator to enroll at the room's host", || {
        owner.tick();
        member.tick();
        match member.poll_enrollment(id).unwrap() {
            PeerSyncResponse::EnrollmentPending { .. } => None,
            joined => Some(joined),
        }
    });
    until("the collaborator to hold the room", || {
        owner.tick();
        member.tick();
        member_tree
            .read()
            .native_authority
            .holds(room)
            .then_some(())
    });

    let channel = document_channel_id(NOTES, &room.to_string(), "plan");
    let mut owner_channels = channels(&owner, &owner_tree, &dir.path().join("owner"), &owner_key);
    let mut member_channels = channels(
        &member,
        &member_tree,
        &dir.path().join("member"),
        &member_key,
    );
    let owner_transport = Recording::new(&owner);
    let member_transport = Recording::new(&member);
    let pump = |channels: &mut ChannelEncryption, transport: &Recording<'_>| {
        transport.net.tick();
        channels.pump(
            transport,
            Instant::now(),
            tilo_clock::try_unix_now().unwrap(),
        );
        channels.access(room, channel, Instant::now())
    };
    let ready = |access: ChannelAccess, who: &str| match access {
        ChannelAccess::Ready(keys) => Some(keys),
        ChannelAccess::Waiting => None,
        other => panic!("{who} cannot open the channel: {other:?}"),
    };
    // How many stewards signed the channel's frames: the owner's broker,
    // when it sent any, and whoever signed the frames the collaborator
    // received that the broker never sent.
    let stewards = || {
        let sent = owner_transport.sent.borrow();
        let broker = sent.iter().any(|frame| frame.channel == channel);
        // A steward names a channel's frames by their revision alone, so two
        // stewards signing under one key send frames with the same ids: only
        // the signature tells one's frame from the other's.
        let other = member_transport.received.borrow().iter().any(|frame| {
            frame.channel == channel
                && !sent
                    .iter()
                    .any(|own| own.signature.as_bytes() == frame.signature.as_bytes())
        });
        (broker, other)
    };
    let deadline = tilo_test_env::hang_deadline();
    let (owner_keys, member_keys) = loop {
        let owner_keys = ready(
            pump(&mut owner_channels, &owner_transport),
            "the owner's device",
        );
        let member_keys = ready(
            pump(&mut member_channels, &member_transport),
            "the collaborator",
        );
        if let Some(keys) = owner_keys.zip(member_keys) {
            break keys;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for both devices to open the channel; owner ready {}, \
             collaborator ready {}, (broker signed, another steward signed) {:?}",
            matches!(
                owner_channels.access(room, channel, Instant::now()),
                ChannelAccess::Ready(_)
            ),
            matches!(
                member_channels.access(room, channel, Instant::now()),
                ChannelAccess::Ready(_)
            ),
            stewards()
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    let artifact = document_artifact_id(NOTES, &room.to_string(), "plan");
    let (metadata, sealed) = owner_keys
        .seal_content(artifact, ContentRetention::Checkpoint, b"the plan")
        .unwrap();
    let opened = member_keys.open_content(artifact, &metadata, &sealed);
    let (broker, other) = stewards();
    eprintln!(
        "channel stewards: the owner's broker signed {broker}, another steward signed {other}; \
         keys at epoch {} on the owner's device and {} on the collaborator's",
        owner_keys.epoch(),
        member_keys.epoch()
    );
    eprintln!(
        "frames the owner's broker sent:\n{}\nframes the collaborator received:\n{}",
        describe(&owner_transport.sent.borrow(), channel),
        describe(&member_transport.received.borrow(), channel)
    );
    // What the owner's device seals, the collaborator's opens.
    assert_eq!(
        opened
            .map_err(|error| format!("the collaborator cannot open it: {error}"))
            .unwrap()
            .as_slice(),
        b"the plan"
    );
    let owner_device = owner_key.verifying_key().to_bytes();
    assert!(
        member_transport
            .received
            .borrow()
            .iter()
            .all(|frame| frame.signed_by.as_bytes() == &owner_device),
        "a frame signed by another key reached the collaborator"
    );
    drop((owner_transport, member_transport));
    assert!(
        !broker,
        "the owner's broker stewarded a channel its device's host governs"
    );
    assert!(other, "the room's host stewards the channel");
    // The owner's device takes its keys from its own host's steward; the
    // collaborator, whose room is hosted elsewhere, is a plain member.
    assert!(
        owner
            .hosted_channel_keys(room, channel, None, false)
            .is_some_and(|keys| keys.is_ok())
    );
    assert!(
        member
            .hosted_channel_keys(room, channel, None, false)
            .is_none()
    );
    drop((owner, member));
}
