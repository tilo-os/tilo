//! A workspace's refusal of a write, kept in the refusing device's own slice
//! of the workspace document (under
//! [`WORKSPACE_WRITE_REFUSALS_KEY`]), checked by every
//! receiver through [`crate::workspace::refusal_slices_kept`].
//!
//! A refusal travels by the sync that already carries the workspace: the
//! refuser's own change reaches the write's author live or on catch-up,
//! survives restarts, and joins the write's history row. One verdict per route
//! and author, the highest sequence: the verdict a device no longer holds is
//! deleted, so a write that later lands stops being shown as missing.
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::personal::change_hashes;
use super::{NeighbourMsg, SystemTime, now_ns};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ed25519_dalek::SigningKey;
use neighbour_wire::{RefusalReason, WriteRefusal};
use std::collections::BTreeSet;
use tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY;

/// The key one verdict is held under: the write's route and author, so a later
/// verdict about the same author on the same record replaces it.
fn entry_key(app: &str, record: &str, author: &[u8; 32]) -> String {
    format!("{app}:{record}:{}", hex::encode(author))
}

/// The map `key` names under `parent`, when it is there.
fn map_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key).ok()? {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// The map `key` names under `parent`, made when it is not there yet.
fn map_for(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Result<ObjId, String> {
    match doc.get(parent, key).map_err(|e| e.to_string())? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => doc
            .put_object(parent, key, ObjType::Map)
            .map_err(|e| e.to_string()),
    }
}

/// The verdict `key` holds under `entries`, when its bytes read as one.
fn held_refusal(
    doc: &AutoCommit,
    entries: &ObjId,
    key: &str,
) -> Result<Option<WriteRefusal>, String> {
    let Some((Value::Scalar(value), _)) = doc.get(entries, key).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let ScalarValue::Bytes(bytes) = value.as_ref() else {
        return Ok(None);
    };
    // ignored-failure: bytes this build cannot read name no author to delete for,
    Ok(postcard::from_bytes::<WriteRefusal>(bytes).ok())
}

impl PeerWorkspace<'_> {
    /// Every refusal this workspace holds: this device's own verdicts and the
    /// ones about its writes. A verdict whose bytes this build cannot read, or
    /// that is not the verdict of the device holding it, is no refusal of
    /// anything and is left out.
    #[must_use]
    pub fn write_refusals(self) -> Vec<WriteRefusal> {
        let Some(doc) = self.peer.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        let Some(people) = map_at(doc, &ROOT, WORKSPACE_WRITE_REFUSALS_KEY) else {
            return Vec::new();
        };
        let mut held = Vec::new();
        for person in doc.keys(&people) {
            let Some(devices) = map_at(doc, &people, &person) else {
                continue;
            };
            for device in doc.keys(&devices) {
                let Some(entries) = map_at(doc, &devices, &device) else {
                    continue;
                };
                for entry in doc.keys(&entries) {
                    let Some((Value::Scalar(value), _)) = doc.get(&entries, &entry).ok().flatten()
                    else {
                        continue;
                    };
                    let ScalarValue::Bytes(bytes) = value.as_ref() else {
                        continue;
                    };
                    let Ok(refusal) = postcard::from_bytes::<WriteRefusal>(bytes) else {
                        continue;
                    };
                    if hex::encode(refusal.by) == device && refusal.signed_by(&refusal.by) {
                        held.push(refusal);
                    }
                }
            }
        }
        held
    }
}

impl PeerWorkspaceMut<'_> {
    /// Record this device's verdict `refusal` on another device's write, in
    /// the workspace's slice of this device's person, signed by `key`, the
    /// device's own. Answers whether the workspace changed: a verdict at or
    /// below the sequence already held for the same route and author is the
    /// one kept.
    pub fn record_own_write_refusal(
        self,
        caller: &tilo_user_identity::DeviceCert,
        refusal: &WriteRefusal,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut candidate = self
            .own_refusal_candidate(caller, key, now)?
            .ok_or("refusal workspace is absent")?;
        if refusal.by != caller.device_public_key {
            return Err("a refusal is its recorder's own verdict".into());
        }
        if refusal.workspace != self.peer.core.workspace {
            return Err("a refusal is this workspace's".into());
        }
        if !refusal.signed_by(&refusal.by) {
            return Err("a refusal is signed by the device that made it".into());
        }
        let bytes = postcard::to_allocvec(refusal).map_err(|error| error.to_string())?;
        let people = map_for(&mut candidate, &ROOT, WORKSPACE_WRITE_REFUSALS_KEY)?;
        let devices = map_for(
            &mut candidate,
            &people,
            &hex::encode(caller.user_public_key),
        )?;
        let entries = map_for(
            &mut candidate,
            &devices,
            &hex::encode(caller.device_public_key),
        )?;
        let entry = entry_key(&refusal.app, &refusal.record, &refusal.author);
        if held_refusal(&candidate, &entries, &entry)?.is_some_and(|held| held.seq >= refusal.seq) {
            return Ok(false);
        }
        candidate
            .put(&entries, &entry, ScalarValue::Bytes(bytes))
            .map_err(|e| e.to_string())?;
        // A device the person revoked is told once why its writes stop, not
        // once per write: its earlier verdicts on that author go.
        if refusal.reason == RefusalReason::Revoked {
            let mut earlier = Vec::new();
            for held in candidate.keys(&entries) {
                if held != entry
                    && held_refusal(&candidate, &entries, &held)?
                        .is_some_and(|held| held.author == refusal.author)
                {
                    earlier.push(held);
                }
            }
            for held in earlier {
                candidate
                    .delete(&entries, &held)
                    .map_err(|e| e.to_string())?;
            }
        }
        candidate.commit();
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(true)
    }

    /// Forget this device's verdict on `author`'s `app`/`record` writes up to
    /// `up_to_seq`: the write it refused, or a later one of the same author,
    /// landed. Answers whether the workspace changed.
    ///
    /// # Errors
    /// As [`Self::record_own_write_refusal`].
    pub fn clear_own_write_refusal(
        self,
        caller: &tilo_user_identity::DeviceCert,
        app: &str,
        record: &str,
        author: [u8; 32],
        up_to_seq: u64,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::transaction::Transactable as _;
        let Some(mut candidate) = self.own_refusal_candidate(caller, key, now)? else {
            return Ok(false);
        };
        let entry = entry_key(app, record, &author);
        let Some(people) = map_at(&candidate, &ROOT, WORKSPACE_WRITE_REFUSALS_KEY) else {
            return Ok(false);
        };
        let Some(devices) = map_at(&candidate, &people, &hex::encode(caller.user_public_key))
        else {
            return Ok(false);
        };
        let Some(entries) = map_at(&candidate, &devices, &hex::encode(caller.device_public_key))
        else {
            return Ok(false);
        };
        let Some((Value::Scalar(value), _)) =
            candidate.get(&entries, &entry).map_err(|e| e.to_string())?
        else {
            return Ok(false);
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Ok(false);
        };
        let Ok(held) = postcard::from_bytes::<WriteRefusal>(bytes) else {
            return Err("a held refusal does not read back".into());
        };
        if held.seq > up_to_seq {
            return Ok(false);
        }
        candidate
            .delete(&entries, &entry)
            .map_err(|e| e.to_string())?;
        candidate.commit();
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(true)
    }

    /// File this device's verdict on the write `message` carried, which it
    /// refused because `reason`: the fact names the route, the write's author
    /// and sequence, the changes that write would have brought here, and the
    /// reason. Answers whether the workspace changed.
    ///
    /// # Errors
    /// As [`Self::record_own_write_refusal`], and for a message that is no
    /// document write of this workspace.
    pub fn refuse_write(
        self,
        caller: &tilo_user_identity::DeviceCert,
        message: &NeighbourMsg,
        reason: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return Err("a refused write is a document".into());
        };
        if *cluster_id != self.peer.core.workspace {
            return Err("a refused write is this workspace's".into());
        }
        let mut refusal = WriteRefusal {
            workspace: self.peer.core.workspace.clone(),
            app: app_id.clone(),
            record: record_id.clone(),
            author: *envelope.author.as_bytes(),
            seq: envelope.seq,
            operation: format!("{}:{}", envelope.artifact.0, envelope.seq),
            changes: self.unheld_changes(app_id, record_id, &envelope.payload),
            reason: RefusalReason::of(reason),
            detail: reason.into(),
            by: caller.device_public_key,
            at_ns: now_ns(now),
            signature: Vec::new(),
        };
        refusal.sign(key)?;
        self.record_own_write_refusal(caller, &refusal, key, now)
    }

    /// Clear this device's verdicts on the writes of `message`'s route up to
    /// its sequence: the write it refused, or a later one of the same author,
    /// landed. Answers whether the workspace changed.
    ///
    /// # Errors
    /// As [`Self::clear_own_write_refusal`].
    pub fn clear_landed_write(
        self,
        caller: &tilo_user_identity::DeviceCert,
        message: &NeighbourMsg,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = message
        else {
            return Ok(false);
        };
        self.clear_own_write_refusal(
            caller,
            app_id,
            record_id,
            *envelope.author.as_bytes(),
            envelope.seq,
            key,
            now,
        )
    }

    /// The changes of a write this device does not hold on its route, as their
    /// hashes: what the author's history joins a verdict to. A whole-document
    /// snapshot names the writes it brings, never the ones already here.
    fn unheld_changes(&self, app: &str, record: &str, payload: &[u8]) -> Vec<[u8; 32]> {
        let mut changes = change_hashes(payload);
        let Some(held) = self.peer.core.documents.get(&super::Route {
            app: app.into(),
            record: record.into(),
        }) else {
            return changes;
        };
        // A copy shares the document until asked to change it, so reading every
        // change it holds costs a pointer copy.
        let mut held = held.doc.clone();
        let held: BTreeSet<[u8; 32]> = held
            .get_changes(&[])
            .iter()
            .map(|change| change.hash().0)
            .collect();
        changes.retain(|hash| !held.contains(hash));
        changes
    }

    /// The workspace as `caller`, this device, writes its own refusals into
    /// it: under its actor, once its certificate and read permission hold and
    /// `key` is its own.
    fn own_refusal_candidate(
        &self,
        caller: &tilo_user_identity::DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<AutoCommit>, String> {
        if key.verifying_key().to_bytes() != caller.device_public_key {
            return Err("a refusal is signed by its recorder's own device".into());
        }
        if self
            .peer
            .certificate(&caller.device_public_key, now)
            .map_err(|error| error.to_string())?
            != caller
        {
            return Err("refusal recorder is not exact current admitted evidence".into());
        }
        let member = self.peer.membership(caller.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::workspace(&self.peer.core.workspace),
            tilo_action_policy::ActionKind::Read,
            now,
        ) {
            return Err("refusal recorder cannot currently read this workspace".into());
        }
        let Some(workspace) = self.peer.core.workspace_doc.as_deref() else {
            return Ok(None);
        };
        let mut candidate = workspace.clone();
        candidate.set_actor(self.peer.workspace_actor_for(&caller.device_public_key));
        Ok(Some(candidate))
    }
}
