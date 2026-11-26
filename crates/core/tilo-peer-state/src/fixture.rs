//! The people, devices and workspaces the engine's tests tell their stories
//! with: a test names who is in the workspace and what it holds, then walks
//! what happens to it.
#![allow(
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    reason = "test fixtures: a failed step is the test failing"
)]

use crate::{SeedDocument, SeedState, StatePeer, TransportOrigin, seed_workspace};
use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

/// The workspace every fixture holds.
pub const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/// The app its seeded records belong to.
pub const APP: &str = "test.app";
/// The record a fixture seeds.
pub const RECORD: &str = "tab:1";

/// The moment a fixture's story starts: certificates are issued before it and
/// expire, when they do, after it.
#[must_use]
pub fn at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(2000)
}

/// `now` in nanoseconds since the epoch, as certificates and invitations count.
#[must_use]
pub fn ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// The key made of the byte `n`.
#[must_use]
pub fn key(n: u8) -> SigningKey {
    SigningKey::from_bytes(&[n; 32])
}

/// A member device: key `n`, certified by the person whose root key is `root`.
#[must_use]
pub fn device(root: u8, n: u8) -> (SigningKey, DeviceCert) {
    certify(&key(root), n, DeviceCapabilities::member())
}

/// Device key `n` certified by `root` with `capabilities`.
#[must_use]
pub fn certify(
    root: &SigningKey,
    n: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = key(n);
    let cert = cert(root, &key, capabilities);
    (key, cert)
}

/// `key`'s device, certified by `root` with `capabilities` since the start of
/// time and without expiry.
#[must_use]
pub fn cert(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{}", key.to_bytes()[0]),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// Frames `key`'s device relays to a peer.
#[must_use]
pub fn origin(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::ServerDelta(key.verifying_key().to_bytes())
}

/// A record whose root `value` is `value`, saved.
#[must_use]
pub fn record(value: &str) -> Vec<u8> {
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", value).unwrap();
    document.commit();
    document.save()
}

/// A workspace founded by one device, with the devices on its roster and the
/// records it is seeded with.
#[must_use]
pub struct Founding {
    workspace: String,
    authority: (SigningKey, DeviceCert),
    roster: Vec<DeviceCert>,
    documents: Vec<SeedDocument>,
    now: SystemTime,
}

impl Founding {
    /// [`WORKSPACE`], founded by `authority` alone at [`at`].
    pub fn by(authority: &(SigningKey, DeviceCert)) -> Self {
        Self {
            workspace: WORKSPACE.into(),
            authority: authority.clone(),
            roster: vec![authority.1.clone()],
            documents: Vec::new(),
            now: at(),
        }
    }

    /// The workspace is `workspace` instead.
    pub fn named(mut self, workspace: &str) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// `devices` are on the roster too.
    pub fn with<'a>(mut self, devices: impl IntoIterator<Item = &'a DeviceCert>) -> Self {
        self.roster.extend(devices.into_iter().cloned());
        self
    }

    /// The story starts at `now`.
    pub fn at(mut self, now: SystemTime) -> Self {
        self.now = now;
        self
    }

    /// [`APP`]'s `id` is seeded with `bytes`.
    pub fn seeding(self, id: &str, bytes: Vec<u8>) -> Self {
        self.seeding_in(APP, id, bytes)
    }

    /// `app`'s `id` is seeded with `bytes`.
    pub fn seeding_in(mut self, app: &str, id: &str, bytes: Vec<u8>) -> Self {
        self.documents.push(SeedDocument {
            app_id: app.into(),
            document_id: id.into(),
            kind: "test".into(),
            bytes,
        });
        self
    }

    /// The roster the workspace is founded with.
    #[must_use]
    pub fn roster(&self) -> &[DeviceCert] {
        &self.roster
    }

    /// A peer that knows the workspace's roster and holds nothing of it yet.
    #[must_use]
    pub fn empty(&self) -> StatePeer {
        StatePeer::new(
            &self.workspace,
            self.authority.1.clone(),
            self.roster.clone(),
            RevocationList::new(),
            self.now,
        )
        .unwrap()
    }

    /// What founding the workspace writes into it.
    #[must_use]
    pub fn seed(&self) -> SeedState {
        SeedState {
            workspace_changes: seed_workspace(&self.workspace, &self.authority.1, &self.roster)
                .unwrap(),
            documents: self.documents.clone(),
        }
    }

    /// The founding device's peer, holding the workspace and its records.
    #[must_use]
    pub fn peer(&self) -> StatePeer {
        let mut peer = self.empty();
        peer.install_seed(self.seed(), &self.authority.0, self.now)
            .unwrap();
        peer
    }

    /// The founding device's peer, holding the workspace and its records
    /// exactly as their writers admitted them.
    #[must_use]
    pub fn exact(&self) -> StatePeer {
        let mut peer = self.empty();
        peer.select_exact_admissions().unwrap();
        peer.install_seed(self.seed(), &self.authority.0, self.now)
            .unwrap();
        peer
    }
}

/// Hands `frames` from `from`'s device to `to`, each taken in.
pub fn relay(to: &mut StatePeer, from: &SigningKey, frames: &[NeighbourMsg], now: SystemTime) {
    for frame in frames {
        to.receive_from(origin(from), frame.clone(), now).unwrap();
    }
}

/// A device the founding device brought up to date: the frames it sends a
/// newcomer, taken in.
#[must_use]
pub fn caught_up(native: &mut StatePeer, founding: &Founding) -> StatePeer {
    let mut peer = founding.empty();
    peer.select_exact_admissions().unwrap();
    let frames = native
        .publication_mut()
        .bootstrap_messages(&founding.authority.0, founding.now)
        .unwrap();
    relay(&mut peer, &founding.authority.0, &frames, founding.now);
    peer
}
