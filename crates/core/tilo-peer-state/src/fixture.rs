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
    labelled(
        root,
        key,
        &format!("device-{}", key.to_bytes()[0]),
        capabilities,
    )
}

/// [`cert`], labelled `label`.
#[must_use]
pub fn labelled(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
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
        for device in devices {
            if !self.roster.contains(device) {
                self.roster.push(device.clone());
            }
        }
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

/// The device certificate `key` holds in `peer`'s workspace.
#[must_use]
pub fn certificate_of(peer: &StatePeer, key: &SigningKey) -> DeviceCert {
    let device = key.verifying_key().to_bytes();
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace holds the device's certificate")
}

/// The person whose device `key` is in `peer`'s workspace.
#[must_use]
pub fn person(peer: &StatePeer, key: &SigningKey) -> [u8; 32] {
    certificate_of(peer, key).user_public_key
}

/// `peer` as it comes back from its durable bytes after a restart, its owner
/// device the first it holds.
pub fn restored(peer: &StatePeer, now: SystemTime) -> Result<StatePeer, String> {
    StatePeer::restore(
        &peer.durable().map_err(|error| error.to_string())?,
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
        vec![],
        RevocationList::new(),
        now,
    )
}

/// An invitation into `peer`'s workspace that `issuer` signs at `now`: an
/// editor's, issued by the certificate `peer` holds for `issuer`, open to
/// anyone, valid for ten seconds, unless `grant` says otherwise.
pub fn invitation(
    peer: &StatePeer,
    issuer: &SigningKey,
    now: SystemTime,
    grant: impl FnOnce(&mut crate::join::InviteGrant),
) -> Vec<u8> {
    let mut invite = crate::join::InviteGrant {
        workspace_id: crate::join::WorkspaceId::parse(peer.workspace_id()).unwrap(),
        issuer: peer
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == issuer.verifying_key().to_bytes())
            .unwrap_or_else(|| peer.owner_certificates()[0].clone()),
        role: crate::join::Role::Editor,
        nonce: [1; 16],
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: ns(now) + 10_000_000_000,
        transport: vec![],
    };
    grant(&mut invite);
    crate::join::issue_invitation(invite, issuer).unwrap()
}

/// `device` redeems `signed` at `now`: it answers the challenge numbered by
/// `seed` with a proof the workspace verifies.
pub fn try_redeem(
    signed: &[u8],
    (key, cert): (&SigningKey, &DeviceCert),
    seed: u8,
    now: SystemTime,
) -> Result<crate::join::VerifiedRedemption, String> {
    let now = ns(now);
    let revocations = RevocationList::new();
    let join = crate::join::decode_and_verify_invitation(
        signed,
        Some(cert.user_public_key),
        &revocations,
        now,
    )
    .map_err(|error| error.to_string())?;
    let challenge = crate::join::JoinChallenge::new(
        &join,
        cert,
        [seed; 32],
        [seed.wrapping_add(1); 32],
        now + 1_000_000_000,
        now,
    )
    .map_err(|error| error.to_string())?;
    let proof =
        crate::join::sign_join_proof(challenge.clone(), key).map_err(|error| error.to_string())?;
    crate::join::verify_join_proof(&proof, &challenge, cert, &revocations, now)
        .map_err(|error| error.to_string())
}

/// [`try_redeem`] that succeeds.
#[must_use]
pub fn redeem(
    signed: &[u8],
    device: (&SigningKey, &DeviceCert),
    seed: u8,
    now: SystemTime,
) -> crate::join::VerifiedRedemption {
    try_redeem(signed, device, seed, now).unwrap()
}

/// `issuer` invites `device`'s person as an editor, and the device redeems the
/// invitation into `peer`: whether the workspace took it in.
pub fn join(
    peer: &mut StatePeer,
    issuer: &SigningKey,
    (key, cert): (&SigningKey, &DeviceCert),
    seed: u8,
    now: SystemTime,
) -> bool {
    let signed = invitation(peer, issuer, now, |grant| {
        grant.nonce = [seed; 16];
        grant.recipient_user_public_key = Some(cert.user_public_key);
    });
    let Ok(redemption) = try_redeem(&signed, (key, cert), seed, now) else {
        return false;
    };
    peer.governance_mut()
        .admit_invited_member(&redemption, key, now)
        .unwrap_or(false)
}

/// `app`'s record `id` of `kind`, saved as `bytes`, as a workspace is seeded with it.
#[must_use]
pub fn document(app: &str, id: &str, kind: &str, bytes: Vec<u8>) -> SeedDocument {
    SeedDocument {
        app_id: app.into(),
        document_id: id.into(),
        kind: kind.into(),
        bytes,
    }
}

/// `peer`'s workspace, founded by `authority`'s device `key` with `roster`
/// on it and seeded with `documents`.
pub fn found(
    peer: &mut StatePeer,
    (key, authority): (&SigningKey, &DeviceCert),
    roster: &[DeviceCert],
    documents: Vec<SeedDocument>,
    now: SystemTime,
) {
    let workspace = peer.workspace_id().to_owned();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(&workspace, authority, roster).unwrap(),
            documents,
        },
        key,
        now,
    )
    .unwrap();
}
