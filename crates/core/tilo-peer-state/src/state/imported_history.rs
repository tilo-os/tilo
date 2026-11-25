//! Who made the changes of a person's own history that name no one, once it was imported.
//!
//! Every change names who made it, and one that names no one is refused. A history kept before
//! that rule, on one of the person's own devices, still holds such changes. When a device of the
//! person imports it, it records one step in the workspace's governance, signed by itself: which
//! of the person's devices wrote the history, on which records and up to which of their writes,
//! and that what those writes made naming no one is the person's. A receiver decides such a change as the person's.
//! A change that names someone keeps that name, and a write after the ones the step names that
//! names no one is refused as before.

use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::signer::DeviceSigner;
use ed25519_dalek::{SigningKey, Verifier as _};

/// The prefix of each import step's key in the workspace's governance: one step per
/// key, its JSON the value.
pub const IMPORTED_HISTORY_PREFIX: &str = "imported_history_v1/";
/// What an import step's signature is over, before the step itself.
const SIGNED_AS: &[u8] = b"tilo imported history v1";

/// One line of writes an import holds: a device's writes on one record, up to `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Imported {
    pub device: [u8; 32],
    pub record: [u8; 32],
    pub seq: u64,
}

/// One import of a person's own history: the step, as the importing device signed it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedHistory {
    /// The device that imported the history, which signs the step.
    pub by: [u8; 32],
    /// The person the history's changes that name no one are attributed to.
    pub person: [u8; 32],
    /// Each line of writes the history holds: the person's device that wrote them, the record
    /// ([`neighbour_wire::app_document_group`]), and the newest of their sequence numbers there.
    pub through: Vec<Imported>,
    /// `by`'s signature of the step, in `workspace`.
    pub signature: Vec<u8>,
}

impl ImportedHistory {
    /// The step of `key`'s device importing `person`'s history of `through` into `workspace`.
    #[must_use]
    pub fn signed(
        workspace: &str,
        person: [u8; 32],
        through: Vec<Imported>,
        key: &SigningKey,
    ) -> Self {
        let by = key.verifying_key().to_bytes();
        let signature =
            ed25519_dalek::Signer::sign(key, &signing_bytes(workspace, &by, &person, &through))
                .to_bytes()
                .to_vec();
        Self {
            by,
            person,
            through,
            signature,
        }
    }

    /// Whether `by` signed this step in `workspace`.
    fn signed_by_its_device(&self, workspace: &str) -> bool {
        let Ok(by) = ed25519_dalek::VerifyingKey::from_bytes(&self.by) else {
            return false;
        };
        let Ok(signature) = ed25519_dalek::Signature::from_slice(&self.signature) else {
            return false;
        };
        by.verify(
            &signing_bytes(workspace, &self.by, &self.person, &self.through),
            &signature,
        )
        .is_ok()
    }

    /// The key the step is kept under in the workspace's governance.
    fn key(&self) -> String {
        let id: Vec<u8> = self.signature.iter().take(16).copied().collect();
        format!("{IMPORTED_HISTORY_PREFIX}{}", hex::encode(id))
    }
}

/// The bytes a step's signature is over.
fn signing_bytes(
    workspace: &str,
    by: &[u8; 32],
    person: &[u8; 32],
    through: &[Imported],
) -> Vec<u8> {
    let mut bytes = SIGNED_AS.to_vec();
    bytes.extend_from_slice(
        &u64::try_from(workspace.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    bytes.extend_from_slice(workspace.as_bytes());
    bytes.extend_from_slice(by);
    bytes.extend_from_slice(person);
    for imported in through {
        bytes.extend_from_slice(&imported.device);
        bytes.extend_from_slice(&imported.record);
        bytes.extend_from_slice(&imported.seq.to_le_bytes());
    }
    bytes
}

impl PeerGovernance<'_> {
    /// The import steps the workspace's governance keeps: each signed by its importing device,
    /// which is a device of the person it names. Any other is left out.
    #[must_use]
    pub fn imported_histories(self) -> Vec<ImportedHistory> {
        let Some(doc) = self.peer.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        doc.keys(ROOT)
            .filter(|key| key.starts_with(IMPORTED_HISTORY_PREFIX))
            .filter_map(|key| {
                let (automerge::Value::Scalar(value), _) = doc.get(ROOT, key.as_str()).ok()??
                else {
                    return None;
                };
                let automerge::ScalarValue::Str(json) = value.as_ref() else {
                    return None;
                };
                tilo_failure::ignored!(
                    "a step that does not parse is none, as one whose signature fails",
                    serde_json::from_str::<ImportedHistory>(json).ok()
                )
            })
            .filter(|step| {
                step.signed_by_its_device(&self.peer.core.workspace)
                    && self.person_of(&step.by) == Some(step.person)
            })
            .collect()
    }

    /// The person a write of `author`'s on `record` ([`neighbour_wire::app_document_group`]) at
    /// `seq` that names no one was made by, when an import step holds it: one of the person's own
    /// devices' writes, at or before the newest the step names there.
    #[must_use]
    pub fn imported_as(self, author: &[u8; 32], record: &[u8; 32], seq: u64) -> Option<[u8; 32]> {
        let person = self.person_of(author)?;
        self.imported_histories()
            .into_iter()
            .filter(|step| step.person == person)
            .any(|step| {
                step.through.iter().any(|imported| {
                    imported.device == *author && imported.record == *record && seq <= imported.seq
                })
            })
            .then_some(person)
    }

    /// [`Self::imported_as`] for a write of `author`'s on `app`'s `record`, as the person who acts.
    pub(crate) fn imported_invoker(
        self,
        author: &[u8; 32],
        app: &str,
        record: &str,
        seq: u64,
    ) -> Option<tilo_action_policy::Acting> {
        self.imported_as(
            author,
            &neighbour_wire::app_document_group(app, record),
            seq,
        )
        .map(|person| tilo_action_policy::Acting::person(hex::encode(person)))
    }

    /// The person whose device `device` is, as its certificate in the workspace says.
    fn person_of(self, device: &[u8; 32]) -> Option<[u8; 32]> {
        self.peer
            .core
            .certificates
            .get(device)
            .map(|certificate| certificate.as_ref().user_public_key)
    }
}

impl PeerGovernanceMut<'_> {
    /// Record `step` in the workspace's governance, authored by `key`. Returns whether it changed
    /// the workspace: a step kept already changes nothing.
    ///
    /// # Errors
    /// The step is not signed by a device of the person it names, or the governance cannot be
    /// written.
    pub fn author_imported_history(
        self,
        step: &ImportedHistory,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let governance = self.peer.governance();
        if !step.signed_by_its_device(&self.peer.core.workspace)
            || governance.person_of(&step.by) != Some(step.person)
        {
            return Err("an import step is signed by a device of the person it names".into());
        }
        if governance.imported_histories().contains(step) {
            return Ok(false);
        }
        let json = serde_json::to_string(step).map_err(|error| error.to_string())?;
        let at = step.key();
        self.write(key, now, |doc| {
            doc.put(ROOT, at.as_str(), json.as_str()).map_err(err)
        })
    }
}
