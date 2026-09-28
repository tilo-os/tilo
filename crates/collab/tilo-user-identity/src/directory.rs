//! The workspace-directory CRDT.
//!
//! Every user device holds a full copy of this small Automerge
//! document. It carries:
//!
//! - The list of workspaces the user owns (`id`, `name`).
//! - Per-workspace member-device set — which of the user's devices
//!   replicate the workspace.
//! - Host assignments — which device(s) act as the always-on host
//!   for a given workspace. Multiple hosts are replicas; hosts can
//!   serve different workspaces.
//! - Per-device pin state — a pinned workspace is held fully
//!   offline on that device.
//! - The revocation list (kept alongside the directory for the same
//!   reason: it must reach every device).
//!
//! **Authenticated deltas.** Updates travel between devices as
//! [`DirectoryUpdate`] blobs: an incremental automerge byte-string
//! wrapped with the author's device pubkey + a signature over the
//! bytes. Applying a delta requires:
//!
//! 1. `verify_device_cert` — the author's cert chains to this
//!    directory's user identity.
//! 2. `!revocation_list.is_revoked_at(author, timestamp)` — the
//!    author was not already revoked.
//! 3. Ed25519 signature check against `author_device_public_key`.
//!
//! Failed checks return `Err(DirectoryError::*)` and DO NOT merge —
//! the caller is expected to log + drop.
//!

use std::collections::HashMap;
use tilo_automerge::ApplyIncoming;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::CanonicalError;
use crate::cert::{DeviceCert, DeviceCertError, verify_device_cert_at};
use crate::revocation::{RevocationError, RevocationList};

/// Signing-domain tag for a directory delta. Same shape as the
/// other v1 tags: `tilo-dir-delta-v1\0`.
pub const DIR_DELTA_PREIMAGE_TAG: &[u8] = b"tilo-dir-delta-v1\0";

/// A workspace's stable id — 64-bit blob-addr-style handle.
pub type WorkspaceId = u64;

/// Public shape of one workspace as it appears in the directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMeta {
    /// Stable workspace id (mirrors the collaboration topic id).
    pub id: WorkspaceId,
    /// Human-readable name — "Personal", "Company Wiki", etc.
    pub name: String,
    /// Device public keys that hold a full replica of this
    /// workspace. Every device in this set will project + hydrate;
    /// devices outside it see nothing.
    pub member_devices: Vec<[u8; 32]>,
    /// Device public keys marked as always-on hosts for this
    /// workspace. Plural, per the design; empty for a strictly-P2P
    /// workspace.
    pub host_devices: Vec<[u8; 32]>,
    /// Device public keys with this workspace pinned (i.e. cache
    /// eagerly, do not evict). Independent from membership.
    pub pinned_devices: Vec<[u8; 32]>,
}

/// A signed, incremental directory delta. Produced by
/// [`WorkspaceDirectory::sync_incremental`] and consumed by
/// [`WorkspaceDirectory::apply_incremental`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryUpdate {
    /// Device that authored the update. The receiver looks up the
    /// device's cert to check it chains to the user's root.
    pub author_device_public_key: [u8; 32],
    /// Nanoseconds since epoch when the update was produced. Feeds
    /// into `revocation_list.is_revoked_at` — a delta authored
    /// after a device was revoked is rejected even if the delta
    /// itself arrives quickly.
    pub authored_at_ns: u64,
    /// Automerge incremental bytes.
    pub delta: Vec<u8>,
    /// Ed25519 signature over
    /// `blake3(DIR_DELTA_PREIMAGE_TAG || postcard(view))` where
    /// `view` is the update without the signature.
    #[serde(with = "serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct DirectoryUpdateView<'a> {
    author_device_public_key: &'a [u8; 32],
    authored_at_ns: u64,
    delta: &'a [u8],
}

impl DirectoryUpdate {
    fn view(&self) -> DirectoryUpdateView<'_> {
        DirectoryUpdateView {
            author_device_public_key: &self.author_device_public_key,
            authored_at_ns: self.authored_at_ns,
            delta: &self.delta,
        }
    }
}

/// Errors that surface from directory operations.
#[derive(Debug, Error)]
pub enum DirectoryError {
    /// The author's device cert did not verify or was expired.
    #[error("author device cert invalid: {0}")]
    Cert(#[from] DeviceCertError),
    /// The author's device was revoked at or before the update's
    /// `authored_at_ns`.
    #[error("author device was revoked")]
    Revoked,
    /// Delta signature did not verify against the author's device
    /// public key.
    #[error("delta signature did not verify")]
    BadSignature,
    /// Author's cert is for a *different* user identity than this
    /// directory belongs to.
    #[error("author identity does not match directory owner")]
    WrongIdentity,
    /// The underlying Automerge merge failed.
    #[error("automerge error: {0}")]
    Automerge(String),
    /// Canonical encoding failed. Effectively unreachable.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
    /// A revocation-list operation failed.
    #[error("revocation-list error: {0}")]
    Revocation(#[from] RevocationError),
}

/// One user's workspace directory.
///
/// Holds a bounded Automerge doc keyed by workspace-id under
/// `ROOT/workspaces`. The device-set / host-set / pinned-set for
/// each workspace live as lists inside the workspace's map.
pub struct WorkspaceDirectory {
    doc: AutoCommit,
    user_public_key: [u8; 32],
}

impl WorkspaceDirectory {
    /// Fresh empty directory for the given user identity.
    ///
    /// Sets the automerge actor id to the *device* public key
    /// (not the user's), so concurrent edits from two of the
    /// user's devices are distinguishable in the change graph.
    pub fn new(user_public_key: [u8; 32], device_public_key: [u8; 32]) -> Self {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from(device_public_key.as_slice()));
        Self {
            doc,
            user_public_key,
        }
    }

    /// The user identity this directory belongs to.
    pub fn user_public_key(&self) -> &[u8; 32] {
        &self.user_public_key
    }

    /// Load an existing directory from its full byte-serialisation.
    /// Consumes the full snapshot (see
    /// [`WorkspaceDirectory::save`]).
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if `bytes` is not a valid
    /// Automerge document.
    pub fn load(
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
        bytes: &[u8],
    ) -> Result<Self, DirectoryError> {
        let mut doc =
            AutoCommit::load(bytes).map_err(|e| DirectoryError::Automerge(e.to_string()))?;
        doc.set_actor(automerge::ActorId::from(device_public_key.as_slice()));
        Ok(Self {
            doc,
            user_public_key,
        })
    }

    /// Full snapshot of the current directory.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// Key under `ROOT` for a workspace entry. We deliberately use
    /// `ROOT` as the workspaces map (rather than nesting under
    /// `ROOT/workspaces`) so two devices concurrently creating
    /// their first workspaces do not both `put_object` on the same
    /// container key — that would produce a Map register conflict
    /// under which the losing peer's workspace contents become
    /// unreachable through `get`. Top-level keys are namespaced with
    /// `w:` so we can add other top-level fields later (e.g.
    /// `revocations:*`) without collision.
    fn workspace_key(id: WorkspaceId) -> String {
        format!("w:{id}")
    }

    /// Ensure a workspace entry (identified by `w:{id}` key at
    /// ROOT) exists and return its `ObjId`.
    ///
    /// When two devices concurrently `add_workspace(id, ...)` for
    /// the *same* id, both `put_object` operations create a map
    /// register conflict at `ROOT/w:{id}`. Automerge picks one
    /// winner deterministically; the loser's nested lists are still
    /// in the doc but hidden from the default `get` path. See
    /// [`WorkspaceMeta`] docs for the convergence guarantee.
    fn ensure_workspace_obj(&mut self, id: WorkspaceId) -> Result<ObjId, DirectoryError> {
        let key = Self::workspace_key(id);
        match self
            .doc
            .get(ROOT, key.as_str())
            .map_err(|e| DirectoryError::Automerge(e.to_string()))?
        {
            Some((Value::Object(ObjType::Map), oid)) => Ok(oid),
            _ => self
                .doc
                .put_object(ROOT, key, ObjType::Map)
                .map_err(|e| DirectoryError::Automerge(e.to_string())),
        }
    }

    /// Add or rename a workspace.
    ///
    /// Idempotent: calling twice with the same `(id, name)` is a
    /// no-op after the first call. Calling with a different `name`
    /// renames the workspace.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn add_workspace(&mut self, id: WorkspaceId, name: &str) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        self.doc
            .put(&obj, "name", name)
            .map_err(|e| DirectoryError::Automerge(e.to_string()))?;
        // Ensure the three lists exist so `list_workspaces` finds them.
        for list in ["members", "hosts", "pinned"] {
            if !matches!(
                self.doc.get(&obj, list),
                Ok(Some((Value::Object(ObjType::List), _)))
            ) {
                self.doc
                    .put_object(&obj, list, ObjType::List)
                    .map_err(|e| DirectoryError::Automerge(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Remove a workspace from the directory. If the workspace does
    /// not exist, this is a no-op.
    ///
    /// # Errors
    ///
    /// Never actually returns an error today — deleting an absent key
    /// is a no-op in Automerge, and this discards any underlying
    /// error to preserve that. `Result` is kept for API symmetry with
    /// the rest of `WorkspaceDirectory`.
    pub fn remove_workspace(&mut self, id: WorkspaceId) -> Result<(), DirectoryError> {
        let key = Self::workspace_key(id);
        // `AutoCommit::delete` returns `Ok(())` for both existing +
        // absent keys.
        let _ = self.doc.delete(ROOT, key.as_str());
        Ok(())
    }

    /// Replace a workspace's member-device set. Passing an empty
    /// vec means "no device carries this workspace" — legal but
    /// unusual.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn set_member_devices(
        &mut self,
        id: WorkspaceId,
        devices: &[[u8; 32]],
    ) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        self.replace_pubkey_list(&obj, "members", devices)
    }

    /// Replace a workspace's host-device set. Multiple hosts are
    /// allowed and interpreted as redundant replicas that merge —
    /// see design §5.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn set_host_devices(
        &mut self,
        id: WorkspaceId,
        hosts: &[[u8; 32]],
    ) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        self.replace_pubkey_list(&obj, "hosts", hosts)
    }

    /// Add a single host to a workspace's host set.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn assign_host(&mut self, id: WorkspaceId, host: [u8; 32]) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        let mut hosts = self.read_pubkey_list(&obj, "hosts");
        if !hosts.contains(&host) {
            hosts.push(host);
        }
        self.replace_pubkey_list(&obj, "hosts", &hosts)
    }

    /// Pin a workspace on the given device — mark it should hold a
    /// full offline replica.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn pin(&mut self, id: WorkspaceId, device: [u8; 32]) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        let mut pinned = self.read_pubkey_list(&obj, "pinned");
        if !pinned.contains(&device) {
            pinned.push(device);
        }
        self.replace_pubkey_list(&obj, "pinned", &pinned)
    }

    /// Unpin — the workspace goes back to on-demand hydration on
    /// that device.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn unpin(&mut self, id: WorkspaceId, device: [u8; 32]) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        let pinned: Vec<[u8; 32]> = self
            .read_pubkey_list(&obj, "pinned")
            .into_iter()
            .filter(|d| d != &device)
            .collect();
        self.replace_pubkey_list(&obj, "pinned", &pinned)
    }

    /// Read all workspaces + their device sets.
    pub fn list_workspaces(&self) -> Vec<WorkspaceMeta> {
        let keys: Vec<String> = self.doc.keys(ROOT).collect();
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let Some(id_str) = key.strip_prefix("w:") else {
                continue;
            };
            let Ok(id) = id_str.parse::<u64>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), obj))) = self.doc.get(ROOT, &key) else {
                continue;
            };
            let name = match self.doc.get(&obj, "name") {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::Str(s) => s.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            out.push(WorkspaceMeta {
                id,
                name,
                member_devices: self.read_pubkey_list(&obj, "members"),
                host_devices: self.read_pubkey_list(&obj, "hosts"),
                pinned_devices: self.read_pubkey_list(&obj, "pinned"),
            });
        }
        out.sort_by_key(|w| w.id);
        out
    }

    fn read_pubkey_list(&self, workspace_obj: &ObjId, key: &str) -> Vec<[u8; 32]> {
        let Ok(Some((Value::Object(ObjType::List), list_obj))) = self.doc.get(workspace_obj, key)
        else {
            return Vec::new();
        };
        let len = self.doc.length(&list_obj);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let Ok(Some((Value::Scalar(s), _))) = self.doc.get(&list_obj, i) {
                match s.as_ref() {
                    ScalarValue::Bytes(b) if b.len() == 32 => {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(b);
                        out.push(arr);
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn replace_pubkey_list(
        &mut self,
        workspace_obj: &ObjId,
        key: &str,
        keys: &[[u8; 32]],
    ) -> Result<(), DirectoryError> {
        let list = self
            .doc
            .put_object(workspace_obj, key, ObjType::List)
            .map_err(|e| DirectoryError::Automerge(e.to_string()))?;
        for k in keys {
            self.doc
                .insert(
                    &list,
                    self.doc.length(&list),
                    ScalarValue::Bytes(k.to_vec()),
                )
                .map_err(|e| DirectoryError::Automerge(e.to_string()))?;
        }
        Ok(())
    }

    /// Produce an incremental delta of everything committed since
    /// the last call — the wire payload devices push to each other.
    ///
    /// The delta is signed with the author's *device* signing key
    /// (which the caller identifies by pubkey), and the wire form
    /// includes the pubkey so a recipient can locate the matching
    /// cert. Devices with a cert issued by the same user root
    /// accept the delta; anyone else drops it.
    ///
    /// `now_ns` is stamped into the delta as `authored_at_ns` and
    /// is used by recipients' revocation-list checks.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Canonical`] if the update cannot be
    /// canonically encoded for signing (effectively unreachable).
    pub fn sync_incremental(
        &mut self,
        device_signing_key: &ed25519_dalek::SigningKey,
        now_ns: u64,
    ) -> Result<DirectoryUpdate, DirectoryError> {
        let author = device_signing_key.verifying_key().to_bytes();
        let delta = self.doc.save_incremental();
        let mut update = DirectoryUpdate {
            author_device_public_key: author,
            authored_at_ns: now_ns,
            delta,
            signature: [0u8; 64],
        };
        let digest = crate::canonical::signing_digest(DIR_DELTA_PREIMAGE_TAG, &update.view())?;
        update.signature = device_signing_key.sign(&digest).to_bytes();
        Ok(update)
    }

    /// Sign a complete dependency-closed snapshot for a newly enrolled empty peer.
    /// Unlike `sync_incremental`, this does not depend on the previous publication cursor.
    ///
    /// # Errors
    /// Returns `DirectoryError::Canonical` if signing serialization fails.
    pub fn signed_snapshot(
        &mut self,
        device_signing_key: &ed25519_dalek::SigningKey,
        now_ns: u64,
    ) -> Result<DirectoryUpdate, DirectoryError> {
        let mut update = DirectoryUpdate {
            author_device_public_key: device_signing_key.verifying_key().to_bytes(),
            authored_at_ns: now_ns,
            delta: self.doc.clone().save(),
            signature: [0; 64],
        };
        let digest = crate::canonical::signing_digest(DIR_DELTA_PREIMAGE_TAG, &update.view())?;
        update.signature = device_signing_key.sign(&digest).to_bytes();
        Ok(update)
    }

    /// Apply a peer's incremental delta after verifying signatures
    /// + cert chain + revocation status.
    ///
    /// `author_cert` is the peer's device cert (fetched separately
    /// from the peer or from a cached cert table). `revocations`
    /// is the current merged revocation list.
    ///
    /// Returns `Ok(())` on merge; every check failure returns an
    /// explicit `DirectoryError` variant so the caller can log the
    /// exact rejection reason.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::WrongIdentity`] if `author_cert` is
    /// for a different user or a different device than the update
    /// claims, [`DirectoryError::Cert`] if the cert itself fails to
    /// verify or has expired, [`DirectoryError::Revoked`] if the
    /// author device was revoked at or before `update.authored_at_ns`,
    /// [`DirectoryError::BadSignature`] if the delta's signature does
    /// not verify, and [`DirectoryError::Automerge`] if merging the
    /// delta into the local document fails.
    pub fn apply_incremental(
        &mut self,
        update: &DirectoryUpdate,
        author_cert: &DeviceCert,
        revocations: &RevocationList,
        now_ns: u64,
    ) -> Result<(), DirectoryError> {
        // Cert must be for THIS directory's user.
        if author_cert.user_public_key != self.user_public_key {
            return Err(DirectoryError::WrongIdentity);
        }
        // Cert must cover THIS device pubkey.
        if author_cert.device_public_key != update.author_device_public_key {
            return Err(DirectoryError::WrongIdentity);
        }
        // Cert must verify + not have expired.
        verify_device_cert_at(author_cert, now_ns)?;
        // Author must not have been revoked at the time of authoring.
        if revocations.is_revoked_at(&update.author_device_public_key, update.authored_at_ns) {
            return Err(DirectoryError::Revoked);
        }
        // Signature must cover the delta bytes.
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&update.author_device_public_key)
            .map_err(|_| DirectoryError::BadSignature)?;
        let digest = crate::canonical::signing_digest(DIR_DELTA_PREIMAGE_TAG, &update.view())?;
        let sig = ed25519_dalek::Signature::from_bytes(&update.signature);
        vk.verify(&digest, &sig)
            .map_err(|_| DirectoryError::BadSignature)?;
        // All checks pass — merge the automerge delta.
        self.doc
            .load_incoming(&update.delta)
            .map_err(|e| DirectoryError::Automerge(e.to_string()))?;
        Ok(())
    }

    /// Snapshot of the current directory as a `HashMap` keyed by
    /// workspace id. Convenience for callers that already have a
    /// map-oriented model.
    pub fn as_map(&self) -> HashMap<WorkspaceId, WorkspaceMeta> {
        self.list_workspaces()
            .into_iter()
            .map(|w| (w.id, w))
            .collect()
    }
}

mod serde_bytes_64 {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v: serde_bytes::ByteBuf = serde_bytes::ByteBuf::deserialize(d)?;
        let bytes = v.into_vec();
        if bytes.len() != 64 {
            return Err(D::Error::custom(format!(
                "expected 64-byte signature, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
// `_sk`/`_pk` (signing key / public key) is this crate's established,
// unambiguous naming convention for a device/user keypair — used in
// nearly every test below. Renaming one half per pair to dodge the
// lint would fight that convention and risks a copy-paste mixup of
// which variable is the secret vs. the public half, in exactly the
// code where that mistake matters most.
#[allow(
    clippy::similar_names,
    reason = "the _sk/_pk keypair naming convention is deliberate and used throughout this crate's tests"
)]
mod tests {
    use super::*;
    use crate::cert::{DeviceCapabilities, DeviceCert, sign_device_cert};
    use crate::revocation::{
        RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
    };
    use crate::{DeviceIdentity, UserIdentity};

    struct Fixture {
        user_sk: ed25519_dalek::SigningKey,
        user_pk: [u8; 32],
    }

    fn fresh_user() -> Fixture {
        let (user_sk, user) = UserIdentity::generate("Alice", 1);
        Fixture {
            user_pk: user.public_key,
            user_sk,
        }
    }

    fn enrol_device(
        user_sk: &ed25519_dalek::SigningKey,
        user_pk: [u8; 32],
        label: &str,
        caps: DeviceCapabilities,
    ) -> (ed25519_dalek::SigningKey, [u8; 32], DeviceCert) {
        let (dev_sk, dev) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: user_pk,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, user_sk).unwrap();
        (dev_sk, dev.public_key, cert)
    }

    /// Workspaces are added and listed, member hosts pin and unpin, concurrent adds merge, and the
    /// directory saves and loads.
    #[test]
    fn the_workspace_directory_merges_and_persists_membership() {
        // Phase: add_and_list_workspaces.
        {
            let f = fresh_user();
            let (_dev_sk, dev_pk, _cert) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let mut dir = WorkspaceDirectory::new(f.user_pk, dev_pk);
            dir.add_workspace(1, "Personal").unwrap();
            dir.add_workspace(2, "Work").unwrap();
            let ws = dir.list_workspaces();
            assert_eq!(ws.len(), 2);
            assert_eq!(ws[0].name, "Personal");
            assert_eq!(ws[1].name, "Work");
        }

        // Phase: set_member_hosts_pin_unpin.
        {
            let f = fresh_user();
            let (_a_sk, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_b_sk, b_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());
            let (_h_sk, h_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "server", DeviceCapabilities::host());

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(9, "Notes").unwrap();
            dir.set_member_devices(9, &[a_pk, b_pk]).unwrap();
            dir.assign_host(9, h_pk).unwrap();
            dir.pin(9, a_pk).unwrap();
            dir.pin(9, a_pk).unwrap(); // idempotent

            let ws = dir.list_workspaces();
            assert_eq!(ws.len(), 1);
            assert_eq!(ws[0].member_devices, vec![a_pk, b_pk]);
            assert_eq!(ws[0].host_devices, vec![h_pk]);
            assert_eq!(ws[0].pinned_devices, vec![a_pk]);

            dir.unpin(9, a_pk).unwrap();
            assert!(dir.list_workspaces()[0].pinned_devices.is_empty());
        }

        // Phase: concurrent_add_merges.
        {
            let f = fresh_user();
            let (a_sk, a_pk, a_cert) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (b_sk, b_pk, b_cert) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());

            let mut dev_a = WorkspaceDirectory::new(f.user_pk, a_pk);
            let mut dev_b = WorkspaceDirectory::new(f.user_pk, b_pk);

            dev_a.add_workspace(1, "Personal").unwrap();
            dev_b.add_workspace(2, "Work").unwrap();

            // Both push a delta; both apply the other's.
            let delta_a = dev_a.sync_incremental(&a_sk, 1_000).unwrap();
            let delta_b = dev_b.sync_incremental(&b_sk, 1_000).unwrap();

            let revs = RevocationList::new();
            dev_a
                .apply_incremental(&delta_b, &b_cert, &revs, 1_100)
                .unwrap();
            dev_b
                .apply_incremental(&delta_a, &a_cert, &revs, 1_100)
                .unwrap();

            let ids_a: Vec<u64> = dev_a.list_workspaces().into_iter().map(|w| w.id).collect();
            let ids_b: Vec<u64> = dev_b.list_workspaces().into_iter().map(|w| w.id).collect();
            assert_eq!(ids_a, vec![1, 2]);
            assert_eq!(ids_b, vec![1, 2]);
        }

        // Phase: save_and_load_round_trip.
        {
            let f = fresh_user();
            let (a_sk, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(42, "Answer").unwrap();
            dir.pin(42, a_pk).unwrap();
            let snapshot = dir.save();
            let reloaded = WorkspaceDirectory::load(f.user_pk, a_pk, &snapshot).unwrap();
            let ws = reloaded.list_workspaces();
            assert_eq!(ws.len(), 1);
            assert_eq!(ws[0].id, 42);
            assert_eq!(ws[0].name, "Answer");
            assert_eq!(ws[0].pinned_devices, vec![a_pk]);
            let _ = a_sk;
        }
    }

    /// A change's author is provable via its signature; deltas from a revoked device, the wrong
    /// device or another user are rejected.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn directory_deltas_are_accepted_only_from_the_users_live_devices() {
        // Phase: author_of_change_is_provable_via_signature.
        {
            let f = fresh_user();
            let (a_sk, a_pk, a_cert) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(1, "Personal").unwrap();
            let mut delta = dir.sync_incremental(&a_sk, 500).unwrap();

            // Baseline: apply cleanly on a fresh replica.
            let mut peer = WorkspaceDirectory::new(f.user_pk, [7u8; 32]);
            peer.apply_incremental(&delta, &a_cert, &RevocationList::new(), 600)
                .unwrap();

            // Now flip a byte in the delta payload — signature should
            // reject.
            peer = WorkspaceDirectory::new(f.user_pk, [7u8; 32]);
            if let Some(b) = delta.delta.get_mut(0) {
                *b ^= 0x01;
            }
            assert!(matches!(
                peer.apply_incremental(&delta, &a_cert, &RevocationList::new(), 600)
                    .unwrap_err(),
                DirectoryError::BadSignature
            ));
        }

        // Phase: revoked_device_delta_rejected.
        {
            let f = fresh_user();
            let (a_sk, a_pk, a_cert) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (b_sk, b_pk, b_cert) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());

            // The user revokes phone at t=500.
            let mut rev = RevocationEntry {
                user_public_key: f.user_pk,
                revoked_device_public_key: b_pk,
                revoked_at_ns: 500,
                reason: RevocationReason::Lost,
                signature: [0u8; 64],
            };
            sign_revocation_entry(&mut rev, &f.user_sk).unwrap();
            let mut revs = RevocationList::new();
            revs.insert(rev).unwrap();

            // Laptop's directory.
            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(1, "Personal").unwrap();
            let a_delta = dir.sync_incremental(&a_sk, 400).unwrap();

            // Phone tries to publish an addition at t=600 (after
            // revocation). Even though the phone still has its old
            // cert, the apply-side rejects because the *update*
            // timestamp postdates the revocation.
            let mut phone_dir = WorkspaceDirectory::new(f.user_pk, b_pk);
            phone_dir
                .apply_incremental(&a_delta, &a_cert, &revs, 700)
                .unwrap();
            phone_dir.add_workspace(2, "Work").unwrap();
            let b_delta = phone_dir.sync_incremental(&b_sk, 600).unwrap();

            assert!(matches!(
                dir.apply_incremental(&b_delta, &b_cert, &revs, 700)
                    .unwrap_err(),
                DirectoryError::Revoked
            ));

            // But a delta authored *before* the revocation cut-off is
            // still accepted (grace: the change was legitimately made
            // by a trusted device at the time).
            let mut phone_dir = WorkspaceDirectory::new(f.user_pk, b_pk);
            phone_dir.add_workspace(3, "PreRevoke").unwrap();
            let pre_delta = phone_dir.sync_incremental(&b_sk, 400).unwrap();
            dir.apply_incremental(&pre_delta, &b_cert, &revs, 700)
                .unwrap();
            assert!(dir.list_workspaces().iter().any(|w| w.id == 3));
        }

        // Phase: delta_signed_by_wrong_device_rejected.
        {
            let f = fresh_user();
            let (_a_sk, a_pk, a_cert) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (b_sk, _b_pk, _b_cert) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(1, "Personal").unwrap();
            // Sign with B's key but pass A's cert — the apply-side
            // catches the identity mismatch.
            let mut delta = dir.sync_incremental(&b_sk, 400).unwrap();
            // Rewrite the author to A so the cert-vs-author check
            // passes; the signature check then fails because the delta
            // was signed with B's key.
            delta.author_device_public_key = a_pk;
            assert!(matches!(
                dir.apply_incremental(&delta, &a_cert, &RevocationList::new(), 600)
                    .unwrap_err(),
                DirectoryError::BadSignature
            ));
        }

        // Phase: cross_user_delta_rejected.
        {
            let f_alice = fresh_user();
            let f_bob = fresh_user();
            let (_a_sk, a_pk, a_cert_alice) = enrol_device(
                &f_alice.user_sk,
                f_alice.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            // Alice's directory. Bob attempts to apply a delta with a
            // cert issued under Bob's identity — rejected as WrongIdentity.
            let (b_sk, b_pk, _) = enrol_device(
                &f_bob.user_sk,
                f_bob.user_pk,
                "phone",
                DeviceCapabilities::primary(),
            );
            let mut alice_dir = WorkspaceDirectory::new(f_alice.user_pk, a_pk);
            alice_dir.add_workspace(1, "AlicePersonal").unwrap();
            let delta = alice_dir.sync_incremental(&b_sk, 100).unwrap();
            // Pass Alice's own cert but the delta's author is Bob's
            // device key — the cert-vs-author guard rejects.
            assert!(matches!(
                alice_dir
                    .apply_incremental(&delta, &a_cert_alice, &RevocationList::new(), 200)
                    .unwrap_err(),
                DirectoryError::WrongIdentity
            ));
            // And a delta whose author matches Bob's cert, but the
            // cert's user identity is Bob's — again WrongIdentity, but
            // triggered by the different guard.
            let (b_sk2, b_pk2, b_cert_bob) = enrol_device(
                &f_bob.user_sk,
                f_bob.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let mut bob_dir = WorkspaceDirectory::new(f_bob.user_pk, b_pk2);
            bob_dir.add_workspace(1, "BobPersonal").unwrap();
            let bob_delta = bob_dir.sync_incremental(&b_sk2, 100).unwrap();
            assert!(matches!(
                alice_dir
                    .apply_incremental(&bob_delta, &b_cert_bob, &RevocationList::new(), 200)
                    .unwrap_err(),
                DirectoryError::WrongIdentity
            ));
            let _ = b_pk;
        }
    }
}
