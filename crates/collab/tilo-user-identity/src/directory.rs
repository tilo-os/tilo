//! The person's list of their workspaces, as every device of theirs holds it.
//!
//! A small Automerge document. It carries:
//!
//! - The workspaces the person has (`id`, `name` and its place in their
//!   list). Whose each one is, and the part the person takes in it, is its
//!   own governance's to say, never this list's.
//! - Per workspace, the devices that hold it, host it, keep it pinned or keep
//!   it off their desktop, and where each device that holds it reaches its
//!   host ([`WorkspaceDirectory::host_hint`]).
//! - The person's devices as they named them.
//!
//! The document is one resource of the person's own store, which carries it
//! between their devices as it carries every other resource of theirs: each
//! write is signed by the device that made it and admitted only from a
//! device the person's root certified and did not revoke. This type reads
//! and writes the document; [`WorkspaceDirectory::merge`] takes in the whole
//! state another device wrote, and [`WorkspaceDirectory::lacks`] says whether
//! a state misses something this one holds.

use std::collections::HashMap;
use tilo_automerge::ApplyIncoming;
use tilo_automerge::{ReadTyped as _, WriteTyped as _};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use collab_types::WorkspaceId;

/// Public shape of one workspace as it appears in the directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMeta {
    /// The workspace's id: the UUID its document is named by.
    pub id: WorkspaceId,
    /// Human-readable name — "Personal", "Company Wiki", etc.
    pub name: String,
    /// Where the workspace sits in the person's list: entries sort by
    /// `(order, id)`, an entry without one last. Each entry's position is
    /// its own register, so concurrent moves of different workspaces both
    /// land.
    pub order: Option<f64>,
    /// Device public keys that hold a full replica of this
    /// workspace. Every device in this set will project + hydrate;
    /// devices outside it see nothing.
    pub member_devices: Vec<[u8; 32]>,
    /// Devices of the person that keep this workspace off their desktop
    /// (they still carry the directory). Every other device shows it,
    /// including one linked later. Each device is its own map key, so
    /// toggles for different devices made at the same time both land.
    pub kept_off_devices: Vec<[u8; 32]>,
    /// Device public keys marked as always-on hosts for this
    /// workspace. Plural, per the design; empty for a strictly-P2P
    /// workspace.
    pub host_devices: Vec<[u8; 32]>,
    /// Device public keys with this workspace pinned (i.e. cache
    /// eagerly, do not evict). Independent from membership.
    pub pinned_devices: Vec<[u8; 32]>,
}

/// Key prefix on a workspace entry marking a device that keeps the
/// workspace off its desktop; the rest of the key is the device's hex key.
const KEPT_OFF_PREFIX: &str = "kept_off:";

/// Key prefix on a workspace entry holding where one device of the person's
/// reaches the workspace's host; the rest of the key is the device's hex key.
const HINT_PREFIX: &str = "hint:";

/// Errors that surface from directory operations.
#[derive(Debug, Error)]
pub enum DirectoryError {
    /// The underlying Automerge merge failed.
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
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
        let mut doc = AutoCommit::load(bytes)?;
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
        match self.doc.get(ROOT, key.as_str())? {
            Some((Value::Object(ObjType::Map), oid)) => Ok(oid),
            _ => self
                .doc
                .put_object(ROOT, key, ObjType::Map)
                .map_err(DirectoryError::from),
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
        self.doc.put_str_if_changed(&obj, "name", name)?;
        // Ensure the three lists exist so `list_workspaces` finds them.
        for list in ["members", "hosts", "pinned"] {
            if !matches!(
                self.doc.get(&obj, list),
                Ok(Some((Value::Object(ObjType::List), _)))
            ) {
                self.doc.put_object(&obj, list, ObjType::List)?;
            }
        }
        Ok(())
    }

    /// Place workspace `id` at `order` in the person's list (see
    /// [`WorkspaceMeta::order`]). An unchanged value writes nothing; a
    /// non-finite one is refused.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn set_order(&mut self, id: WorkspaceId, order: f64) -> Result<(), DirectoryError> {
        if !order.is_finite() {
            return Ok(());
        }
        let obj = self.ensure_workspace_obj(id)?;
        let current = self.doc.f64_at(&obj, "order");
        if current == Some(order) {
            return Ok(());
        }
        self.doc
            .put(&obj, "order", ScalarValue::F64(order))
            .map_err(DirectoryError::from)
    }

    /// Record that device `device` belongs to this person and is called
    /// `label`, so the person's other devices can name it, for instance
    /// when choosing which devices show a workspace. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn register_device(&mut self, device: [u8; 32], label: &str) -> Result<(), DirectoryError> {
        let key = Self::device_key(&device);
        let obj = match self.doc.get(ROOT, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), obj))) => obj,
            _ => self.doc.put_object(ROOT, key, ObjType::Map)?,
        };
        Ok(self.doc.put_str_if_changed(&obj, "label", label)?)
    }

    /// Every device of this person that registered itself, with its label,
    /// sorted by key.
    pub fn devices(&self) -> Vec<([u8; 32], String)> {
        let mut out: Vec<([u8; 32], String)> = self
            .doc
            .keys(ROOT)
            .filter_map(|key| {
                let hex = key.strip_prefix("d:")?;
                if hex.len() != 64 {
                    return None;
                }
                let mut device = [0u8; 32];
                for (byte, pair) in device.iter_mut().zip(hex.as_bytes().chunks(2)) {
                    *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
                }
                let Ok(Some((Value::Object(ObjType::Map), obj))) = self.doc.get(ROOT, key.as_str())
                else {
                    return None;
                };
                Some((device, self.doc.str_at(&obj, "label").unwrap_or_default()))
            })
            .collect();
        out.sort();
        out
    }

    fn device_key(device: &[u8; 32]) -> String {
        format!("d:{}", hex::encode(device))
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

    /// Keep workspace `id` off `device`'s desktop, or show it there again.
    /// Each choice is its own key on the workspace entry (no shared
    /// container two devices could create at once), so choices for
    /// different devices made concurrently both land. Idempotent; an
    /// unchanged choice writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying
    /// Automerge document operation fails.
    pub fn set_kept_off(
        &mut self,
        id: WorkspaceId,
        device: [u8; 32],
        kept_off: bool,
    ) -> Result<(), DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        let key = format!("{KEPT_OFF_PREFIX}{}", hex::encode(device));
        let present = matches!(self.doc.get(&obj, key.as_str()), Ok(Some(_)));
        let written = match (kept_off, present) {
            (true, false) => self.doc.put(&obj, key.as_str(), true),
            (false, true) => self.doc.delete(&obj, key.as_str()),
            _ => return Ok(()),
        };
        written.map_err(DirectoryError::from)
    }

    fn read_kept_off(&self, workspace_obj: &ObjId) -> Vec<[u8; 32]> {
        let mut out: Vec<[u8; 32]> = self
            .doc
            .keys(workspace_obj)
            .filter_map(|key| {
                let hex = key.strip_prefix(KEPT_OFF_PREFIX)?;
                let bytes = hex::decode(hex).ok()?;
                <[u8; 32]>::try_from(bytes).ok()
            })
            .collect();
        out.sort_unstable();
        out
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
            let Ok(id) = id_str.parse::<WorkspaceId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), obj))) = self.doc.get(ROOT, &key) else {
                continue;
            };
            let name = self.doc.str_at(&obj, "name").unwrap_or_default();
            let order = match self.doc.get(&obj, "order") {
                Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                    ScalarValue::F64(v) if v.is_finite() => Some(*v),
                    _ => None,
                },
                _ => None,
            };
            out.push(WorkspaceMeta {
                id,
                name,
                order,
                member_devices: self.read_pubkey_list(&obj, "members"),
                kept_off_devices: self.read_kept_off(&obj),
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
        let list = self.doc.put_object(workspace_obj, key, ObjType::List)?;
        for k in keys {
            self.doc.insert(
                &list,
                self.doc.length(&list),
                ScalarValue::Bytes(k.to_vec()),
            )?;
        }
        Ok(())
    }

    /// Take in the whole state `bytes` another device of the person wrote,
    /// as their store carries it. Returns whether this list changed.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if `bytes` is not an Automerge
    /// document.
    pub fn merge(&mut self, bytes: &[u8]) -> Result<bool, DirectoryError> {
        let before = self.doc.get_heads();
        self.doc.load_incoming(bytes)?;
        Ok(self.doc.get_heads() != before)
    }

    /// The list's heads: what moves whenever it changes, here or by a merge.
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// Whether the Automerge bytes `state` miss a change this list holds:
    /// what says the person's store is owed this device's list. Bytes that
    /// are no document hold nothing.
    pub fn lacks(&mut self, state: &[u8]) -> bool {
        let Ok(theirs) = AutoCommit::load(state) else {
            return true;
        };
        self.doc
            .get_heads()
            .iter()
            .any(|head| !theirs.has_change(head))
    }

    /// Say where `device`, which holds workspace `id`, reaches its host:
    /// the host's signed hint, as that device's own state of the workspace
    /// names it. Each device writes its own key of the entry, so two devices
    /// that hold the workspace never write over each other. An unchanged
    /// hint writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`DirectoryError::Automerge`] if the underlying Automerge
    /// document operation fails.
    pub fn set_host_hint(
        &mut self,
        id: WorkspaceId,
        device: [u8; 32],
        hint: &[u8],
    ) -> Result<bool, DirectoryError> {
        let obj = self.ensure_workspace_obj(id)?;
        let key = format!("{HINT_PREFIX}{}", hex::encode(device));
        if self.doc.bytes_at(&obj, key.as_str()).as_deref() == Some(hint) {
            return Ok(false);
        }
        self.doc
            .put(&obj, key.as_str(), ScalarValue::Bytes(hint.to_vec()))?;
        Ok(true)
    }

    /// Where workspace `id`'s host is reached, as a device of the person's
    /// that holds it said: the hint its host device wrote first, then any
    /// other. `None` while no device said one.
    pub fn host_hint(&self, id: WorkspaceId) -> Option<Vec<u8>> {
        let Ok(Some((Value::Object(ObjType::Map), obj))) =
            self.doc.get(ROOT, Self::workspace_key(id).as_str())
        else {
            return None;
        };
        let hosts = self.read_pubkey_list(&obj, "hosts");
        let mut said: Vec<(bool, Vec<u8>)> = self
            .doc
            .keys(&obj)
            .filter_map(|key| {
                let device = key.strip_prefix(HINT_PREFIX)?;
                let by_host = hosts.iter().any(|host| hex::encode(host) == device);
                Some((by_host, self.doc.bytes_at(&obj, key.as_str())?))
            })
            .collect();
        said.sort_by_key(|(by_host, _)| !*by_host);
        said.into_iter().next().map(|(_, hint)| hint)
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
            issuer: None,
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
            let (_, dev_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let mut dir = WorkspaceDirectory::new(f.user_pk, dev_pk);
            dir.add_workspace(WorkspaceId::from_u128(1), "Personal")
                .unwrap();
            dir.add_workspace(WorkspaceId::from_u128(2), "Work")
                .unwrap();
            let ws = dir.list_workspaces();
            assert_eq!(ws.len(), 2);
            assert_eq!(ws[0].name, "Personal");
            assert_eq!(ws[1].name, "Work");
        }

        // Phase: hosts_pin_unpin.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_, h_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "server",
                DeviceCapabilities::member(),
            );

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(WorkspaceId::from_u128(9), "Notes")
                .unwrap();
            dir.assign_host(WorkspaceId::from_u128(9), h_pk).unwrap();
            dir.pin(WorkspaceId::from_u128(9), a_pk).unwrap();
            dir.pin(WorkspaceId::from_u128(9), a_pk).unwrap(); // idempotent

            let ws = dir.list_workspaces();
            assert_eq!(ws.len(), 1);
            assert_eq!(ws[0].host_devices, vec![h_pk]);
            assert_eq!(ws[0].pinned_devices, vec![a_pk]);

            dir.unpin(WorkspaceId::from_u128(9), a_pk).unwrap();
            assert!(dir.list_workspaces()[0].pinned_devices.is_empty());
        }

        // Phase: concurrent_add_merges.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_, b_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());

            let mut dev_a = WorkspaceDirectory::new(f.user_pk, a_pk);
            let mut dev_b = WorkspaceDirectory::new(f.user_pk, b_pk);

            dev_a
                .add_workspace(WorkspaceId::from_u128(1), "Personal")
                .unwrap();
            dev_b
                .add_workspace(WorkspaceId::from_u128(2), "Work")
                .unwrap();

            // Each takes in the other's whole state.
            let state_a = dev_a.save();
            let state_b = dev_b.save();
            assert!(dev_a.merge(&state_b).unwrap());
            assert!(dev_b.merge(&state_a).unwrap());

            let id = WorkspaceId::from_u128;
            let ids_a: Vec<WorkspaceId> =
                dev_a.list_workspaces().into_iter().map(|w| w.id).collect();
            let ids_b: Vec<WorkspaceId> =
                dev_b.list_workspaces().into_iter().map(|w| w.id).collect();
            assert_eq!(ids_a, vec![id(1), id(2)]);
            assert_eq!(ids_b, vec![id(1), id(2)]);
        }

        // Phase: save_and_load_round_trip.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );

            let mut dir = WorkspaceDirectory::new(f.user_pk, a_pk);
            dir.add_workspace(WorkspaceId::from_u128(42), "Answer")
                .unwrap();
            dir.pin(WorkspaceId::from_u128(42), a_pk).unwrap();
            let snapshot = dir.save();
            let reloaded = WorkspaceDirectory::load(f.user_pk, a_pk, &snapshot).unwrap();
            let ws = reloaded.list_workspaces();
            assert_eq!(ws.len(), 1);
            assert_eq!(ws[0].id, WorkspaceId::from_u128(42));
            assert_eq!(ws[0].name, "Answer");
            assert_eq!(ws[0].pinned_devices, vec![a_pk]);
        }
    }

    tilo_test_env::scenario! {
        fn what_one_device_records_reaches_the_persons_other_devices;

        phase an_unchanged_entry_writes_nothing_and_reaches_another_device;
        // Republishing the same entry adds no history, and a signed snapshot
        // brings it to another device of the same person.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_, b_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "board", DeviceCapabilities::member());
            let workspace = WorkspaceId::from_u128(7);

            let mut laptop = WorkspaceDirectory::new(f.user_pk, a_pk);
            laptop.add_workspace(workspace, "Research").unwrap();
            let heads = laptop.doc.get_heads();
            laptop.add_workspace(workspace, "Research").unwrap();
            laptop.doc.commit();
            assert_eq!(
                laptop.doc.get_heads(),
                heads,
                "an unchanged entry writes nothing"
            );

            let mut board = WorkspaceDirectory::new(f.user_pk, b_pk);
            let state = laptop.save();
            assert!(board.merge(&state).unwrap());
            let listed = board.list_workspaces();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "Research");
            assert_eq!(listed[0].id, workspace);

            assert!(
                !board.lacks(&state),
                "the board holds nothing the laptop lacks"
            );
            assert!(!laptop.lacks(&board.save()));
            assert!(!board.merge(&state).unwrap(), "a state taken in is no news");

            laptop.add_workspace(workspace, "Thesis").unwrap();
            assert!(laptop.lacks(&state), "the rename is news to the board");
            board.merge(&laptop.save()).unwrap();
            assert_eq!(board.list_workspaces()[0].name, "Thesis");

            // States saved between uncommitted edits stay one history: the
            // second edit must not reuse the first state's change.
            let mut fresh = WorkspaceDirectory::new(f.user_pk, a_pk);
            fresh
                .add_workspace(WorkspaceId::from_u128(8), "One")
                .unwrap();
            let first = fresh.save();
            fresh
                .add_workspace(WorkspaceId::from_u128(8), "Two")
                .unwrap();
            let second = fresh.save();
            let mut other = WorkspaceDirectory::new(f.user_pk, b_pk);
            for state in [&first, &second] {
                other.merge(state).unwrap();
            }
            assert_eq!(other.list_workspaces()[0].name, "Two");

            // Where each device that holds a workspace reaches its host is that
            // device's to say; the host's own word comes first.
            let hosted = WorkspaceId::from_u128(9);
            laptop.add_workspace(hosted, "Hosted").unwrap();
            laptop.set_host_devices(hosted, &[b_pk]).unwrap();
            assert!(laptop.set_host_hint(hosted, a_pk, b"laptop's").unwrap());
            assert!(!laptop.set_host_hint(hosted, a_pk, b"laptop's").unwrap());
            assert_eq!(laptop.host_hint(hosted).as_deref(), Some(&b"laptop's"[..]));
            board.merge(&laptop.save()).unwrap();
            board.set_host_hint(hosted, b_pk, b"the host's").unwrap();
            laptop.merge(&board.save()).unwrap();
            assert_eq!(laptop.host_hint(hosted).as_deref(), Some(&b"the host's"[..]));
        }

        phase order_and_devices_reach_another_device;
        // Positions and the device roster travel like the rest of the list.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_, b_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "board", DeviceCapabilities::member());
            let mut laptop = WorkspaceDirectory::new(f.user_pk, a_pk);
            laptop
                .add_workspace(WorkspaceId::from_u128(1), "One")
                .unwrap();
            laptop.set_order(WorkspaceId::from_u128(1), 2.5).unwrap();
            laptop.register_device(a_pk, "Laptop").unwrap();
            let heads = laptop.doc.get_heads();
            laptop.set_order(WorkspaceId::from_u128(1), 2.5).unwrap();
            laptop.register_device(a_pk, "Laptop").unwrap();
            assert_eq!(
                laptop.doc.get_heads(),
                heads,
                "unchanged values write nothing"
            );
            let mut board = WorkspaceDirectory::new(f.user_pk, b_pk);
            board
                .merge(&laptop.save())
                .unwrap();
            assert_eq!(board.list_workspaces()[0].order, Some(2.5));
            assert_eq!(board.devices(), vec![(a_pk, "Laptop".to_string())]);
        }

        phase keeping_a_workspace_off_names_only_the_devices_that_opted_out;
        // A workspace kept off one device shows everywhere else, including on
        // a device linked afterwards, and concurrent toggles for different
        // devices both land.
        {
            let f = fresh_user();
            let (_, a_pk, _) = enrol_device(
                &f.user_sk,
                f.user_pk,
                "laptop",
                DeviceCapabilities::primary(),
            );
            let (_, b_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "board", DeviceCapabilities::member());
            let (_, later_pk, _) =
                enrol_device(&f.user_sk, f.user_pk, "phone", DeviceCapabilities::member());
            let mut laptop = WorkspaceDirectory::new(f.user_pk, a_pk);
            laptop
                .add_workspace(WorkspaceId::from_u128(4), "Work")
                .unwrap();
            let mut board = WorkspaceDirectory::new(f.user_pk, b_pk);
            board
                .merge(&laptop.save())
                .unwrap();
            laptop
                .set_kept_off(WorkspaceId::from_u128(4), a_pk, true)
                .unwrap();
            board
                .set_kept_off(WorkspaceId::from_u128(4), b_pk, true)
                .unwrap();
            laptop
                .merge(&board.save())
                .unwrap();
            let mut both = vec![a_pk, b_pk];
            both.sort_unstable();
            let entry = &laptop.list_workspaces()[0];
            assert_eq!(entry.kept_off_devices, both);
            assert!(!entry.kept_off_devices.contains(&later_pk));
            laptop
                .set_kept_off(WorkspaceId::from_u128(4), a_pk, false)
                .unwrap();
            assert_eq!(laptop.list_workspaces()[0].kept_off_devices, vec![b_pk]);
        }
    }
}
