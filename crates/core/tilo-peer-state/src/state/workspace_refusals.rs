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
use super::change_hashes::change_hashes;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{NeighbourMsg, SystemTime, now_ns};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use neighbour_wire::{RefusalReason, WriteRefusal};
use std::collections::BTreeSet;
use tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY;

/// A change a device refuses before it goes, as its author's session is told it: the document the
/// change writes, the changes it would have carried, and why — the same shape a server's refusal
/// of a change has, so a reader tells
/// one from the other by its words, never by which device spoke.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChangeRefused {
    /// The workspace the change was written in.
    pub workspace_id: String,
    /// The app whose document the change writes.
    pub app_id: String,
    /// The workspace the document belongs to, as the change named it.
    pub cluster_id: String,
    /// The document.
    pub document_id: String,
    /// The changes the refused change carried, as their hashes: what a reader withdraws and the
    /// author's history names as missing.
    pub changes: Vec<[u8; 32]>,
    /// Why, typed as a server's refusals are.
    pub refusal: tilo_server_protocol::refusal::Refusal,
    /// The refusing device's own words, kept as the filed verdict's detail.
    pub reason: String,
}

impl ChangeRefused {
    /// What `envelope`, a change of `document_id` of `app_id` in `workspace_id`, is refused as
    /// when the workspace's rules refuse it by `refusal`: the changes it carries — its claims,
    /// else those of its payload — and why, typed the way a server types its refusals and in
    /// `reason`, the refusing device's words.
    pub(crate) fn of(
        workspace_id: String,
        cluster_id: String,
        app_id: &str,
        document_id: &str,
        envelope: &collab_types::SignedOp,
        refusal: &collab_types::ReceiptRefusal,
        reason: String,
    ) -> Self {
        let mut changes: Vec<[u8; 32]> = envelope.claims.iter().map(|claim| claim.change).collect();
        if changes.is_empty() {
            changes = change_hashes(&envelope.payload);
        }
        Self {
            workspace_id,
            app_id: app_id.to_owned(),
            cluster_id,
            document_id: document_id.to_owned(),
            changes,
            refusal: match refusal {
                collab_types::ReceiptRefusal::NotGranted { .. }
                | collab_types::ReceiptRefusal::CallerNotGranted { .. } => {
                    tilo_server_protocol::refusal::Refusal::NotGranted(refusal.to_string())
                }
                other => tilo_server_protocol::refusal::Refusal::NotAllowed(other.to_string()),
            },
            reason,
        }
    }

    /// The class of the refusal, as its author is told it.
    #[must_use]
    pub const fn reason_class(&self) -> RefusalReason {
        use tilo_server_protocol::refusal::Refusal;
        match &self.refusal {
            Refusal::NotGranted(_) | Refusal::NotAllowed(_) | Refusal::WorkspaceDeleted => {
                RefusalReason::Forbidden
            }
            Refusal::NotAMember(_)
            | Refusal::Revoked(_)
            | Refusal::DeviceRevoked
            | Refusal::NotAdmitted => RefusalReason::NotCertified,
            Refusal::NotReady | Refusal::Offline => RefusalReason::MissingDependency,
            Refusal::Full => RefusalReason::Full,
            Refusal::Invalid(_) => RefusalReason::Malformed,
            Refusal::Conflict => RefusalReason::Conflict,
            Refusal::Other(_) | Refusal::Listed => RefusalReason::Other,
        }
    }
}

impl std::fmt::Display for ChangeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

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

/// The map `key` names under `parent`, made when it is not there yet: the row
/// of one device, which only that device writes.
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
    Ok(tilo_failure::ignored!(
        "bytes this build cannot read name no author to delete for",
        postcard::from_bytes::<WriteRefusal>(bytes).ok()
    ))
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
        let Some(rows) = map_at(doc, &ROOT, WORKSPACE_WRITE_REFUSALS_KEY) else {
            return Vec::new();
        };
        let mut held = Vec::new();
        for row in doc.keys(&rows) {
            let Some((_, device)) = super::device_rows::person_and_device(&row) else {
                continue;
            };
            let device = hex::encode(device);
            let Some(entries) = map_at(doc, &rows, &row) else {
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
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut candidate = self
            .own_row_candidate(caller, key, now)?
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
        let rows = super::governed_map(&candidate, WORKSPACE_WRITE_REFUSALS_KEY)?;
        let entries = map_for(
            &mut candidate,
            &rows,
            &super::device_rows::person_row_key(&caller.user_public_key, &caller.device_public_key),
        )?;
        let entry = entry_key(&refusal.app, &refusal.record, &refusal.author);
        if let Some(held) = held_refusal(&candidate, &entries, &entry)? {
            if held.seq >= refusal.seq {
                return Ok(false);
            }
            // The same words about the same changes is the verdict already here: a write a device
            // refuses on every pass that retries it is filed once, not once per pass.
            if held.reason == refusal.reason && held.changes == refusal.changes {
                return Ok(false);
            }
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
        super::commit_governance(&mut candidate, super::now_secs(now), None);
        let candidate = crate::cached_document::CachedDocument::written_on(
            self.peer.core.workspace_doc.as_ref(),
            candidate,
        );
        self.peer.core.workspace_doc = Some(candidate);
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
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        use automerge::transaction::Transactable as _;
        let Some(mut candidate) = self.own_row_candidate(caller, key, now)? else {
            return Ok(false);
        };
        let entry = entry_key(app, record, &author);
        let Some(rows) = map_at(&candidate, &ROOT, WORKSPACE_WRITE_REFUSALS_KEY) else {
            return Ok(false);
        };
        let key =
            super::device_rows::person_row_key(&caller.user_public_key, &caller.device_public_key);
        let Some(entries) = map_at(&candidate, &rows, &key) else {
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
        super::commit_governance(&mut candidate, super::now_secs(now), None);
        let candidate = crate::cached_document::CachedDocument::written_on(
            self.peer.core.workspace_doc.as_ref(),
            candidate,
        );
        self.peer.core.workspace_doc = Some(candidate);
        Ok(true)
    }

    /// File this device's verdict on the write `message` carried, which it
    /// refused as `refused` says: the fact names the route, the write's author
    /// and sequence, the changes that write would have brought here, and the
    /// refusal. Answers whether the workspace changed.
    ///
    /// # Errors
    /// As [`Self::record_own_write_refusal`], and for a message that is no
    /// document write.
    pub fn refuse_write(
        self,
        caller: &tilo_user_identity::DeviceCert,
        message: &NeighbourMsg,
        refused: &crate::AdmissionError,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("a refused write is a document".into());
        };
        // A verdict is filed in the workspace that refused the change, which is where its author
        // sent it and reads it, whether or not the document names that workspace: a change of
        // another workspace's document sent here is refused here, and the author is told there.
        let mut refusal = WriteRefusal {
            workspace: self.peer.core.workspace.clone(),
            app: app_id.clone(),
            record: document_id.clone(),
            author: *envelope.author.as_bytes(),
            seq: envelope.seq,
            operation: format!("{}:{}", envelope.artifact.0, envelope.seq),
            changes: self.unheld_changes(app_id, document_id, envelope),
            reason: refused.reason(),
            detail: refused.to_string(),
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
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Ok(false);
        };
        self.clear_own_write_refusal(
            caller,
            app_id,
            document_id,
            *envelope.author.as_bytes(),
            envelope.seq,
            key,
            now,
        )
    }

    /// The changes of a write this device does not hold on its route, as their
    /// hashes: what the author's history joins a verdict to. A sealed write's
    /// payload is its author's ciphertext, so the changes it claims are the
    /// ones named, and one claiming none names the changes of its payload. A
    /// whole-document snapshot names the writes it brings, never the ones
    /// already here.
    fn unheld_changes(
        &self,
        app: &str,
        record: &str,
        envelope: &collab_types::SignedOp,
    ) -> Vec<[u8; 32]> {
        let mut changes: Vec<[u8; 32]> = envelope.claims.iter().map(|claim| claim.change).collect();
        if changes.is_empty() {
            changes = change_hashes(&envelope.payload);
        }
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

    /// The workspace as `caller`, this device, writes its own rows into it (its refusals, the
    /// checkpoints it checked): under its actor, once its certificate and read permission hold
    /// and `key` is its own.
    pub(super) fn own_row_candidate(
        &self,
        caller: &tilo_user_identity::DeviceCert,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Option<AutoCommit>, String> {
        if key.device_public_key() != caller.device_public_key {
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
