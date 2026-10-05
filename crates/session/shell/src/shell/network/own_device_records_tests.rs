//! App records between two devices of one person, over real Iroh endpoints.
//!
//! [`PairedDevices`] is the harness: the first device mints the person's
//! workspace, the second (enrolled by the same root, holding no root key)
//! receives it through the person's workspace list, and each runs this
//! session's end-to-end channel encryption over its own network, with the
//! person's certified devices as its own fleet. Tests drive a record the
//! way an app does: they ask for the record channel's keys, seal a document
//! write with them, sign it with the device key and publish it, and check
//! what the other device receives and can open.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use channel_crypto::ChannelContentKeys;
use collab_types::{ChannelId, ContentRetention, SignedOp};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::channel_encryption::{
    ChannelAccess, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
};
use crate::shell::channel_policy::WorkspaceChannelDirectory;
use crate::shell::collab_core::BrokerChannels;
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

const NOTES: &str = "so.tilo.notes";
/// The record every device's Notes opens in a room by default.
const RECORD: &str = "derived:default@tab:0000000000000000";

/// One device of the person: its network, tree, certified key and channel
/// encryption service.
pub(super) struct Device {
    pub net: ShellNetwork,
    pub tree: WorkspaceTreeHandle,
    pub key: SigningKey,
    pub cert: DeviceCert,
    pub channels: ChannelKeyAccess,
    _state: tempfile::TempDir,
}

impl Device {
    fn spawn(
        tree: WorkspaceTreeHandle,
        dir: &std::path::Path,
        root: &SigningKey,
        label: &str,
        capabilities: DeviceCapabilities,
        fleet: &Arc<Fleet>,
    ) -> Self {
        let root = root.clone();
        let certified: Arc<Mutex<Option<(SigningKey, DeviceCert)>>> = Arc::default();
        let captured = Arc::clone(&certified);
        let label = label.to_string();
        let net = ShellNetwork::spawn_with(
            tree.clone(),
            &dir.join("device-private/keypair.bin"),
            Reach::Loopback,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let mut cert = DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: label,
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &root).unwrap();
                *captured.lock().unwrap() = Some((key.clone(), cert.clone()));
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: std::collections::HashMap::new(),
                    workspace_revocations: std::collections::HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap();
        let (key, cert) = certified.lock().unwrap().take().unwrap();
        fleet.add(cert.clone());
        let state = tempfile::tempdir().unwrap();
        let own = Arc::clone(fleet);
        let channels = ChannelKeyAccess::new(
            ChannelEncryption::open(
                ChannelEncryptionConfig {
                    root: state.path().join("channels"),
                    vault: channel_crypto::VaultKey::new(key.to_bytes()),
                    device_key: key.clone(),
                },
                Box::new(WorkspaceChannelDirectory::new(
                    tree.clone(),
                    Box::new(move |_| own.devices()),
                    key.verifying_key().to_bytes(),
                )),
                Arc::new(Mutex::new(BrokerChannels::default())),
            )
            .unwrap(),
        );
        Self {
            net,
            tree,
            key,
            cert,
            channels,
            _state: state,
        }
    }

    fn device(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    /// One pass of the session loop: network events, then the channel
    /// protocol over the same network.
    fn step(&self) {
        self.net.tick();
        self.channels.pump(&self.net);
    }

    fn access(&self, room: RoomId) -> ChannelAccess {
        self.channels.access(room, channel(room)).0
    }

    /// A sealed write to the default Notes record, signed by this device.
    fn sealed_edit(
        &self,
        room: RoomId,
        keys: &ChannelContentKeys,
        seq: u64,
        text: &[u8],
    ) -> SignedOp {
        let cluster = room.to_string();
        let artifact = collab_types::document_artifact_id(NOTES, &cluster, RECORD);
        let (metadata, payload) = keys
            .seal_content(artifact, ContentRetention::Delta, text)
            .unwrap();
        let mut envelope = SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            collab_types::Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes()),
            collab_types::document_attestation_id(NOTES, &cluster, RECORD),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            seq,
            metadata,
        );
        collab_types::sign_op(&mut envelope, &self.key).unwrap();
        envelope
    }

    fn publish(&self, room: RoomId, envelope: SignedOp) {
        self.net
            .publish_document(NOTES.into(), room.to_string(), RECORD, envelope);
    }

    /// The Notes writes this device received from the other one so far.
    fn received(&self) -> Vec<SignedOp> {
        self.net
            .inbound_documents_handle()
            .lock()
            .unwrap()
            .iter()
            .filter(|doc| doc.app_id == NOTES && doc.record_id == RECORD)
            .map(|doc| (*doc.envelope).clone())
            .collect()
    }
}

/// The person's certified devices, as each device's fleet holds them.
#[derive(Default)]
pub(super) struct Fleet {
    devices: Mutex<Vec<DeviceCert>>,
    revoked: Mutex<Vec<[u8; 32]>>,
}

impl Fleet {
    fn add(&self, cert: DeviceCert) {
        self.devices.lock().unwrap().push(cert);
    }

    fn revoke(&self, device: [u8; 32]) {
        self.revoked.lock().unwrap().push(device);
    }

    fn devices(&self) -> Vec<(DeviceCert, bool)> {
        let revoked = self.revoked.lock().unwrap().clone();
        self.devices
            .lock()
            .unwrap()
            .iter()
            .map(|cert| (cert.clone(), revoked.contains(&cert.device_public_key)))
            .collect()
    }
}

fn channel(room: RoomId) -> ChannelId {
    collab_types::document_channel_id(NOTES, &room.to_string(), RECORD)
}

fn open(keys: &ChannelContentKeys, envelope: &SignedOp) -> Vec<u8> {
    let metadata = envelope.encryption.expect("a sealed write");
    let ciphertext = collab_types::sealed_ciphertext(envelope).unwrap();
    let plain = keys
        .open_content(envelope.artifact, &metadata, &ciphertext)
        .unwrap_or_else(|error| panic!("the other device's write does not open here: {error}"));
    plain.to_vec()
}

/// Two devices of one person, connected directly.
pub(super) struct PairedDevices {
    pub first: Device,
    pub second: Device,
    pub root: SigningKey,
    pub room: RoomId,
    pub fleet: Arc<Fleet>,
    _home: tilo_test_env::TestHome,
    _dir: tempfile::TempDir,
}

impl PairedDevices {
    /// The first device mints the person's "Home" workspace; the second
    /// starts empty and joins it through the person's workspace list.
    pub fn connect() -> Self {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let person_root = SigningKey::from_bytes(&[43; 32]);
        let person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(
                person_root.verifying_key().to_bytes(),
            ),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            other => panic!("a root key names a person: {other:?}"),
        };
        home.env().set("TILO_USER_ID", &person);
        let fleet = Arc::new(Fleet::default());

        let mut first_tree = WorkspaceTree::default();
        let workspace = first_tree.append_empty_workspace("Home");
        first_tree.adopt_ownerless_workspaces(&person);
        let room = first_tree.room_of(workspace).unwrap();
        let first = Device::spawn(
            first_tree.shared(),
            &dir.path().join("first"),
            &person_root,
            "Primary device",
            DeviceCapabilities::primary(),
            &fleet,
        );
        assert!(first.net.join_default_workspace());
        let second = Device::spawn(
            WorkspaceTree::default().shared(),
            &dir.path().join("second"),
            &person_root,
            "Board B",
            DeviceCapabilities::member(),
            &fleet,
        );
        assert!(second.net.join_default_workspace());
        let devices = Self {
            first,
            second,
            root: person_root,
            room,
            fleet,
            _home: home,
            _dir: dir,
        };
        let addr = devices.until("the first device to listen", || {
            devices.first.net.dial_addr()
        });
        devices
            .second
            .net
            .cmd_tx
            .try_send(NetCmd::Connect { addr })
            .unwrap();
        devices.until("the second device to hold the person's workspace", || {
            devices
                .second
                .tree
                .read()
                .rooms()
                .contains(&room)
                .then_some(())
        });
        devices
    }

    fn step(&self) {
        self.first.step();
        self.second.step();
    }

    /// Run both devices until `probe` answers.
    pub fn until<T>(&self, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.step();
            if let Some(value) = probe() {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run both devices for `period`, for checks that something does not
    /// happen.
    pub fn idle(&self, period: Duration) {
        let end = Instant::now().checked_add(period).unwrap();
        while Instant::now() < end {
            self.step();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ready(&self, device: &Device, what: &str) -> ChannelContentKeys {
        self.until(what, || match device.access(self.room) {
            ChannelAccess::Ready(keys) => Some(keys),
            _ => None,
        })
    }

    /// The person's root revokes the second device; the first learns it as
    /// its fleet store would tell it.
    pub fn revoke_second(&self) {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: self.root.verifying_key().to_bytes(),
            revoked_device_public_key: self.second.device(),
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &self.root).unwrap();
        self.fleet.revoke(self.second.device());
        self.first.net.refresh_local_revocations(
            entry.user_public_key,
            &[entry],
            tilo_clock::unix_now_ns(),
        );
    }
}

/// The device that minted the person's room stewards its record channels;
/// the other device asks it for the keys instead of founding a group of its
/// own. Each device's sealed edits reach the other and open there, and a
/// device that opens the record later asks the room for its document. Once
/// the person revokes the second device, the steward rotates the channel's
/// keys without it and its edits are refused.
#[test]
#[serial_test::serial]
fn a_persons_devices_exchange_sealed_record_edits() {
    let devices = PairedDevices::connect();
    let room = devices.room;
    let (first, second) = (&devices.first, &devices.second);

    devices.until("the person's list to name the room's steward", || {
        (second.tree.read().channel_stewards.get(&room) == Some(&first.device())).then_some(())
    });

    devices.ready(first, "the steward's channel keys");
    let second_keys = devices.ready(second, "the other device to be admitted to the channel");
    // The steward's keys as of the commit that admitted the other device.
    let first_keys = devices.ready(first, "the steward's current keys");

    // The second device edits; the first receives the write and opens it.
    second.publish(
        room,
        second.sealed_edit(room, &second_keys, 1, b"Groceries"),
    );
    let from_second = devices.until("the second device's edit to arrive", || {
        first.received().into_iter().find(|op| op.seq == 1)
    });
    assert_eq!(open(&first_keys, &from_second), b"Groceries");

    // And the other way round.
    first.publish(room, first.sealed_edit(room, &first_keys, 1, b"Errands"));
    let from_first = devices.until("the first device's edit to arrive", || {
        second.received().into_iter().find(|op| op.seq == 1)
    });
    assert_eq!(open(&second_keys, &from_first), b"Errands");

    // A device that opens the record later asks the room for its document;
    // the device holding it gets the request for its app.
    first
        .net
        .publish_document_snapshot_request(NOTES, &room.to_string(), RECORD);
    let asked = devices.until("the snapshot request to reach the other device", || {
        second
            .net
            .inbound_snapshot_requests_handle()
            .lock()
            .unwrap()
            .pop()
    });
    assert_eq!(
        (
            asked.app_id.as_str(),
            asked.cluster_id.as_str(),
            asked.record_id.as_str()
        ),
        (NOTES, room.to_string().as_str(), RECORD)
    );

    // The person revokes the second device: the steward rotates the
    // channel's keys without it, and its later edits are refused.
    devices.revoke_second();
    let probe = |keys: &ChannelContentKeys| {
        let artifact = collab_types::document_artifact_id(NOTES, &room.to_string(), RECORD);
        keys.seal_content(artifact, ContentRetention::Delta, b"probe")
            .map(|(metadata, payload)| (artifact, metadata, payload))
            .unwrap()
    };
    devices.until("the steward to rotate the revoked device out", || {
        let ChannelAccess::Ready(keys) = first.access(room) else {
            return None;
        };
        let (artifact, metadata, payload) = probe(&keys);
        second_keys
            .open_content(artifact, &metadata, &payload)
            .is_err()
            .then_some(())
    });
    let before = first.received().len();
    second.publish(
        room,
        second.sealed_edit(room, &second_keys, 2, b"after revocation"),
    );
    devices.idle(Duration::from_secs(2));
    assert_eq!(
        first.received().len(),
        before,
        "a revoked device's later edits are refused"
    );
    drop(devices);
}
