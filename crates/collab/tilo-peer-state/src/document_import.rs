//! Bringing a whole document into a governed room: the proposals the
//! governor keeps in the room and the actions that carry them.
//!
//! A member proposes ([`PROPOSE`]) with a signed
//! [`DocumentImportProposalV1`] and the document itself. The governor
//! checks that the proposer could create that record, validates the
//! document, and records the proposal under [`ROOM_KEY`] in the room's
//! governed state, where every member's desktop sees it. The room's owner
//! answers ([`ACCEPT`] or [`DECLINE`]) with a signed [`DocumentImportDecisionV1`]. Only an
//! accepted, unexpired proposal becomes a record, created by the governor
//! with the rights an empty creation needs; a declined or expired one never
//! creates anything. The map is governed: only the room's authority writes
//! it.
pub use collab_types::document_import::{
    DOCUMENT_IMPORT_VERSION, DocumentImportDecisionV1, DocumentImportError,
    DocumentImportProposalV1, DocumentImportTerms, MAX_IMPORT_DOCUMENT_BYTES,
    MAX_IMPORT_LIFETIME_SECS,
};

use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The surface the governor publishes the import actions on: its own, so
/// clients that look for the desktop surface by its app id never find it.
pub const APP_ID: &str = "so.tilo.document-imports";
pub const RESOURCE: &str = "desktop:document-import";
pub const PROPOSE: &str = "propose";
/// Separate actions, so the room's activity says which answer was given.
pub const ACCEPT: &str = "accept";
pub const DECLINE: &str = "decline";
/// Governed room map: proposal id (hex) to its [`ImportEntry`] as JSON bytes.
pub const ROOM_KEY: &str = "document_imports_v1";
/// Most proposals a room keeps at once.
pub const MAX_ROOM_IMPORTS: usize = 64;

/// One proposal and, once given, the owner's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportEntry {
    pub proposal: DocumentImportProposalV1,
    pub decision: Option<DocumentImportDecisionV1>,
}

/// Where a proposal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportState {
    /// Waiting for the owner.
    Pending,
    /// Accepted: the record exists in the room.
    Accepted,
    Declined,
    /// Nobody answered in time; nothing was created.
    Expired,
}

impl ImportEntry {
    #[must_use]
    pub fn state(&self, now_unix: u64) -> ImportState {
        match &self.decision {
            Some(decision) if decision.accepted => ImportState::Accepted,
            Some(_) => ImportState::Declined,
            None if now_unix >= self.proposal.expires_unix => ImportState::Expired,
            None => ImportState::Pending,
        }
    }
}

/// The map key of a proposal.
#[must_use]
pub fn key(proposal_id: &[u8; 32]) -> String {
    hex::encode(proposal_id)
}

fn raw_entries(doc: &AutoCommit) -> BTreeMap<String, Vec<u8>> {
    let Some((Value::Object(ObjType::Map), object)) = doc.get(ROOT, ROOM_KEY).ok().flatten() else {
        return BTreeMap::new();
    };
    doc.keys(&object)
        .filter_map(|key| match doc.get(&object, &key).ok().flatten()? {
            (Value::Scalar(value), _) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => Some((key, bytes.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The governed bytes of every entry: what only the authority may change.
pub(crate) fn governed_bytes(doc: Option<&AutoCommit>) -> BTreeMap<String, Vec<u8>> {
    doc.map(raw_entries).unwrap_or_default()
}

/// Every well-formed proposal the room keeps, by key.
#[must_use]
pub fn entries(doc: &AutoCommit) -> BTreeMap<String, ImportEntry> {
    raw_entries(doc)
        .into_iter()
        .filter_map(|(key, bytes)| {
            let entry: ImportEntry = serde_json::from_slice(&bytes).ok()?;
            (self::key(&entry.proposal.proposal_id) == key).then_some((key, entry))
        })
        .collect()
}

/// Write `entry` into the room's governed map; the authority's own change.
///
/// # Errors
/// An automerge failure, or a room already keeping as many proposals as it may.
pub fn put_entry(doc: &mut AutoCommit, entry: &ImportEntry) -> Result<(), String> {
    let object = match doc.get(ROOT, ROOM_KEY).map_err(|e| e.to_string())? {
        Some((Value::Object(ObjType::Map), object)) => object,
        Some(_) => return Err("governed import map is not a map".into()),
        None => doc
            .put_object(ROOT, ROOM_KEY, ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    let key = key(&entry.proposal.proposal_id);
    if doc.get(&object, &key).map_err(|e| e.to_string())?.is_none()
        && doc.length(&object) >= MAX_ROOM_IMPORTS
    {
        return Err("the room keeps too many document proposals".into());
    }
    let bytes = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
    doc.put(&object, key, ScalarValue::Bytes(bytes))
        .map_err(|e| e.to_string())
}

/// Remove settled proposals older than `before_unix`, so the map stays
/// small; pending ones stay until they expire.
///
/// # Errors
/// An automerge failure.
pub fn prune(doc: &mut AutoCommit, before_unix: u64) -> Result<usize, String> {
    let Some((Value::Object(ObjType::Map), object)) =
        doc.get(ROOT, ROOM_KEY).map_err(|e| e.to_string())?
    else {
        return Ok(0);
    };
    let stale: Vec<String> = entries(doc)
        .into_iter()
        .filter(|(_, entry)| entry.proposal.expires_unix < before_unix)
        .map(|(key, _)| key)
        .collect();
    for key in &stale {
        doc.delete(&object, key.as_str())
            .map_err(|e| e.to_string())?;
    }
    Ok(stale.len())
}

/// The [`PROPOSE`] action's argument: the proposal and the document it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeArgument {
    pub proposal: DocumentImportProposalV1,
    /// The saved document, base64.
    pub document: String,
}

impl ProposeArgument {
    #[must_use]
    pub fn new(proposal: DocumentImportProposalV1, document: &[u8]) -> Self {
        Self {
            proposal,
            document: base64::engine::general_purpose::STANDARD.encode(document),
        }
    }

    /// The document, when it is exactly the one the proposal names.
    ///
    /// # Errors
    /// Malformed or oversized bytes, or bytes other than the proposal's.
    pub fn document(&self) -> Result<Vec<u8>, String> {
        let limit = usize::try_from(MAX_IMPORT_DOCUMENT_BYTES).unwrap_or(usize::MAX);
        if self.document.len() > limit.div_ceil(3).saturating_mul(4) {
            return Err("the proposed document is too large".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.document)
            .map_err(|_| "the proposed document is malformed")?;
        if !self.proposal.carries(&bytes) {
            return Err("the document is not the one the proposal names".into());
        }
        Ok(bytes)
    }
}

/// The [`ACCEPT`] and [`DECLINE`] actions' argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideArgument {
    pub proposal_id: [u8; 32],
    pub decision: DocumentImportDecisionV1,
}

/// One import action as the owner commit path carries it. The caller is
/// never part of it: the broker supplies it from authenticated admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportAction {
    Propose(ProposeArgument),
    Accept(DecideArgument),
    Decline(DecideArgument),
}

impl ImportAction {
    /// The action named `action` with its JSON `argument`.
    ///
    /// # Errors
    /// An unknown action, or an argument that is not its typed form.
    pub fn parse(action: &str, argument: &str) -> Result<Self, String> {
        let limit = usize::try_from(MAX_IMPORT_DOCUMENT_BYTES)
            .unwrap_or(usize::MAX)
            .div_ceil(3)
            .saturating_mul(4)
            .saturating_add(16 * 1024);
        if argument.len() > limit {
            return Err("document import argument exceeds its bound".into());
        }
        let decide = || serde_json::from_str::<DecideArgument>(argument).map_err(|e| e.to_string());
        match action {
            PROPOSE => serde_json::from_str(argument)
                .map(Self::Propose)
                .map_err(|e| e.to_string()),
            ACCEPT => decide().map(Self::Accept),
            DECLINE => decide().map(Self::Decline),
            _ => Err("unknown document import action".into()),
        }
    }

    /// The proposal this action is about.
    #[must_use]
    pub fn proposal_id(&self) -> [u8; 32] {
        match self {
            Self::Propose(argument) => argument.proposal.proposal_id,
            Self::Accept(argument) | Self::Decline(argument) => argument.proposal_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u8, expires_unix: u64) -> ImportEntry {
        let key = ed25519_dalek::SigningKey::from_bytes(&[id; 32]);
        ImportEntry {
            proposal: DocumentImportProposalV1::new(
                &DocumentImportTerms {
                    room: "3f2b1c00-0000-4000-8000-00000000000b".into(),
                    proposal_id: [id; 32],
                    app: "so.tilo.notes".into(),
                    record: "uuid:0f2b1c00-0000-4000-8000-000000000001".into(),
                    title: "Groceries".into(),
                    document: b"doc".to_vec(),
                    issued_unix: 1,
                    expires_unix,
                },
                &key,
            )
            .unwrap(),
            decision: None,
        }
    }

    #[test]
    fn the_room_keeps_proposals_by_id_and_prunes_settled_ones() {
        let mut doc = AutoCommit::new();
        put_entry(&mut doc, &entry(1, 100)).unwrap();
        put_entry(&mut doc, &entry(2, 1_000)).unwrap();
        let kept = entries(&doc);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[&key(&[1; 32])].state(50), ImportState::Pending);
        assert_eq!(kept[&key(&[1; 32])].state(100), ImportState::Expired);
        assert_eq!(prune(&mut doc, 500).unwrap(), 1);
        assert_eq!(entries(&doc).len(), 1);
        assert_eq!(governed_bytes(Some(&doc)).len(), 1);
    }

    #[test]
    fn import_proposals_are_governance() {
        let before = AutoCommit::new();
        let mut after = before.clone();
        put_entry(&mut after, &entry(4, 100)).unwrap();
        // So only the pinned governance writer may write one
        // (`room::admit_room_changes`).
        assert!(!crate::room::governance_unchanged(Some(&before), &after));
    }

    #[test]
    fn a_propose_argument_carries_only_the_named_document() {
        let proposal = entry(3, 100).proposal;
        assert_eq!(
            ProposeArgument::new(proposal.clone(), b"doc").document(),
            Ok(b"doc".to_vec())
        );
        assert!(ProposeArgument::new(proposal, b"dog").document().is_err());
    }
}
