//! Documents proposed for this room, and the owner's answers to them.
//!
//! A member proposes that a whole document, history included, become a new
//! record here. The owner checks that the member could create that record
//! exactly as an empty creation would ([`crate::canonical_records::authorize_creation`]),
//! validates the document ([`ui::record::transfer::validate_import`]), and
//! records the proposal in the room's governed state
//! ([`tilo_peer_state::document_import::ROOM_KEY`]). Nothing is created then.
//! Only the room owner's signed acceptance, before the proposal expires and
//! while the proposer still has those rights, makes the owner publish the
//! document as the new record; a declined, expired or unanswered proposal
//! never creates anything.
//!
//! The proposed bytes wait in memory, bounded; an owner restart forgets
//! them, and a later acceptance is refused with that reason.
use crate::SessionOwner;
use std::collections::BTreeMap;
use tilo_action_broker::peer_actions::PolicyTime;
use tilo_peer_state::desktop_command::DesktopCommandError;
use tilo_peer_state::document_import::{
    self as import, ImportAction, ImportEntry, ImportState, MAX_IMPORT_DOCUMENT_BYTES,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Most proposed documents held at once, and their total size.
const MAX_HELD: usize = 16;
const MAX_HELD_BYTES: usize = 4 * 1024 * 1024;
/// How long a settled proposal stays in the room's state after it expired.
const SETTLED_RETENTION_SECS: u64 = 24 * 60 * 60;

/// Proposed documents waiting for the owner's answer.
#[derive(Default)]
pub(crate) struct HeldDocuments(BTreeMap<[u8; 32], Vec<u8>>);

impl HeldDocuments {
    fn bytes(&self) -> usize {
        self.0.values().map(Vec::len).sum()
    }
    fn room_for(&self, len: usize) -> bool {
        self.0.len() < MAX_HELD && self.bytes().saturating_add(len) <= MAX_HELD_BYTES
    }
}

fn unix(now: SystemTime) -> Result<u64, String> {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| "owner time predates the epoch".into())
}

/// The action and caller of an import request.
fn parts(request: &OwnerCommitRequest) -> Result<(&DeviceCert, &ImportAction), String> {
    match &request.operation {
        CommitOperation::DocumentImport { caller, action, .. } => Ok((caller, action)),
        _ => Err("not a document import".into()),
    }
}

/// `device` is admitted in the room now, with the room's current policy.
fn admitted(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> Result<DeviceCert, String> {
    let certificate = peer
        .certificates()
        .into_iter()
        .find(|certificate| certificate.device_public_key == device)
        .ok_or("that device is no longer admitted to the room")?;
    peer.permissions(device, now)?;
    peer.permissions(peer.authority().device_public_key, now)?;
    peer.validated_media_policy()?;
    Ok(certificate)
}

/// `device` belongs to the room's owner.
fn owner_device(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> Result<(), String> {
    admitted(peer, device, now)?;
    let (role, _, _) = peer.permissions(device, now)?;
    if role != collab_types::Role::Owner {
        return Err("only the room's owner answers a proposed document".into());
    }
    Ok(())
}

/// The proposal an answer names, as the room keeps it.
fn kept(peer: &StatePeer, proposal_id: &[u8; 32]) -> Result<ImportEntry, String> {
    let room = peer.admitted_room().ok_or("room governance missing")?;
    import::entries(room)
        .remove(&import::key(proposal_id))
        .ok_or_else(|| "the room has no such proposal".into())
}

/// Every check that must hold at the moment an import is committed, for
/// the caller and, for an acceptance, the proposer it creates for.
pub(crate) fn authorize(
    peer: &StatePeer,
    request: &OwnerCommitRequest,
    now: SystemTime,
) -> Result<(), String> {
    if request.room != peer.room() {
        return Err("document import names another room".into());
    }
    let (caller, action) = parts(request)?;
    if !peer.certificates().contains(caller) {
        return Err("the caller's certificate is no longer admitted".into());
    }
    match action {
        ImportAction::Propose(argument) => {
            let proposal = &argument.proposal;
            admitted(peer, caller.device_public_key, now)?;
            crate::canonical_records::authorize_creation(
                peer,
                caller,
                &proposal.app,
                &proposal.record,
                now,
            )
        }
        ImportAction::Accept(argument) => {
            owner_device(peer, caller.device_public_key, now)?;
            let entry = kept(peer, &argument.proposal_id)?;
            let proposer = admitted(peer, *entry.proposal.proposer.as_bytes(), now)?;
            crate::canonical_records::authorize_creation(
                peer,
                &proposer,
                &entry.proposal.app,
                &entry.proposal.record,
                now,
            )
        }
        ImportAction::Decline(_) => owner_device(peer, caller.device_public_key, now),
    }
}

/// Final-barrier fence: [`authorize`] again at the owner's commit time.
pub(crate) struct DocumentImportPolicyFence {
    request: OwnerCommitRequest,
    time: PolicyTime,
}

impl DocumentImportPolicyFence {
    pub(crate) fn policy_time(&self) -> &PolicyTime {
        &self.time
    }
    pub(crate) fn revalidate(&self, owner: &SessionOwner) -> Result<(), String> {
        let now = crate::owner::policy_fence::sample(Some(&self.time))?;
        if !owner.permits_publication() {
            return Err("owner publication is not permitted".into());
        }
        authorize(owner.peer(), &self.request, now)
    }
}

impl SessionOwner {
    /// Commit one import action: record a proposal, create an accepted
    /// document, or record a decline.
    ///
    /// # Errors
    /// [`DesktopCommandError::Rejected`] for anything refused before the
    /// durable barrier; [`DesktopCommandError::Uncertain`] when the barrier
    /// itself failed.
    pub fn commit_document_import(
        &mut self,
        request: &OwnerCommitRequest,
        time: Option<&PolicyTime>,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        let rejected = DesktopCommandError::Rejected;
        self.require_owner_resource_scope()
            .map_err(|error| rejected(error.to_string()))?;
        let binding = request.binding().map_err(rejected)?;
        let now = crate::owner::policy_fence::sample(time).map_err(rejected)?;
        let now_unix = unix(now).map_err(rejected)?;
        if !self.permits_publication() {
            return Err(rejected("owner publication is not permitted".into()));
        }
        authorize(self.peer(), request, now).map_err(rejected)?;
        let (caller, action) = parts(request).map_err(rejected)?;
        let mut candidate = self.peer().clone();
        let (entry, created) = match action {
            ImportAction::Propose(argument) => {
                let proposal = &argument.proposal;
                proposal
                    .verify(now_unix)
                    .map_err(|error| rejected(error.to_string()))?;
                if proposal.proposer.as_bytes() != &caller.device_public_key {
                    return Err(rejected(
                        "a proposal is made only by the device that signed it".into(),
                    ));
                }
                if proposal.room != request.room {
                    return Err(rejected("the proposal names another room".into()));
                }
                let document = argument.document().map_err(rejected)?;
                self.check_importable(&proposal.app, &proposal.record, &document)
                    .map_err(rejected)?;
                if kept(self.peer(), &proposal.proposal_id).is_ok() {
                    return Err(rejected("that proposal was already made".into()));
                }
                if !self.proposed_documents.room_for(document.len()) {
                    return Err(rejected(
                        "too many documents are waiting for the owner's answer".into(),
                    ));
                }
                (
                    ImportEntry {
                        proposal: proposal.clone(),
                        decision: None,
                    },
                    Some(document),
                )
            }
            ImportAction::Accept(argument) | ImportAction::Decline(argument) => {
                let accepting = matches!(action, ImportAction::Accept(_));
                if argument.decision.accepted != accepting {
                    return Err(rejected("the decision does not match its action".into()));
                }
                if argument.decision.decider.as_bytes() != &caller.device_public_key {
                    return Err(rejected(
                        "a decision is given only by the device that signed it".into(),
                    ));
                }
                let entry = kept(self.peer(), &argument.proposal_id).map_err(rejected)?;
                if entry.decision.is_some() {
                    return Err(rejected("that proposal was already answered".into()));
                }
                argument
                    .decision
                    .verify(&entry.proposal, now_unix)
                    .map_err(|error| rejected(error.to_string()))?;
                if accepting {
                    if entry.state(now_unix) != ImportState::Pending {
                        return Err(rejected("that proposal is no longer open".into()));
                    }
                    let document = self
                        .proposed_documents
                        .0
                        .get(&entry.proposal.proposal_id)
                        .cloned()
                        .ok_or_else(|| {
                            rejected(
                                "the proposed document is no longer held here; ask for it to be proposed again"
                                    .into(),
                            )
                        })?;
                    if !entry.proposal.carries(&document) {
                        return Err(rejected("the held document changed".into()));
                    }
                    self.check_importable(&entry.proposal.app, &entry.proposal.record, &document)
                        .map_err(rejected)?;
                    // The whole saved document, history and all, as the
                    // record's first admitted state.
                    candidate
                        .publish_document(
                            &entry.proposal.app,
                            &entry.proposal.record,
                            document,
                            &self.identity.native,
                            now,
                        )
                        .map_err(rejected)?;
                }
                (
                    ImportEntry {
                        decision: Some(argument.decision.clone()),
                        ..entry
                    },
                    None,
                )
            }
        };
        self.write_entry(&mut candidate, &entry, now, now_unix)
            .map_err(rejected)?;
        let committed = match time {
            Some(time) => self.commit_mutation_candidate_with_policy(
                candidate,
                self.directory().to_vec(),
                crate::owner::OwnerPolicyFence::DocumentImport(DocumentImportPolicyFence {
                    request: request.clone(),
                    time: time.clone(),
                }),
            ),
            None => self.commit_mutation_candidate(candidate, self.directory().to_vec()),
        }
        .map_err(|error| {
            if let Some(refused) = error.downcast_ref::<crate::owner::OwnerPolicyRefused>() {
                DesktopCommandError::Rejected(refused.0.clone())
            } else {
                DesktopCommandError::Uncertain(error.to_string())
            }
        })?;
        let id = entry.proposal.proposal_id;
        match created {
            Some(document) => {
                self.proposed_documents.0.insert(id, document);
            }
            None => {
                self.proposed_documents.0.remove(&id);
            }
        }
        self.forget_expired(now_unix);
        let receipt = OwnerCommitReceipt {
            binding,
            generation: committed.generation,
            aggregate_hash: hex::decode(committed.aggregate_hash)
                .map_err(|e| DesktopCommandError::Uncertain(e.to_string()))?
                .try_into()
                .map_err(|_| DesktopCommandError::Uncertain("invalid aggregate hash".into()))?,
        };
        receipt
            .validate_for(request)
            .map_err(DesktopCommandError::Uncertain)?;
        Ok(receipt)
    }

    /// The document may become record `record` of `app` here: exactly that
    /// freshly minted record of this room, within bounds, not yet present.
    fn check_importable(&self, app: &str, record: &str, document: &[u8]) -> Result<(), String> {
        let id = ui::record::RecordId::parse(record).ok_or("invalid record id")?;
        if self.peer().document(app, record).is_some() {
            return Err("that record already exists in the room".into());
        }
        let (doc, _) = ui::record::transfer::validate_import(
            document,
            self.peer().room(),
            &id,
            usize::try_from(MAX_IMPORT_DOCUMENT_BYTES).unwrap_or(usize::MAX),
        )
        .map_err(|error| error.to_string())?;
        crate::canonical_records::accepts_import(app, &doc)
    }

    /// Record `entry` in the room's governed state, as this owner's change.
    fn write_entry(
        &self,
        candidate: &mut StatePeer,
        entry: &ImportEntry,
        now: SystemTime,
        now_unix: u64,
    ) -> Result<(), String> {
        let mut doc = candidate
            .admitted_room()
            .ok_or("room governance missing")?
            .clone();
        let base = doc.get_heads();
        doc.set_actor(ui::automerge::ActorId::from(
            self.identity.native_cert.device_public_key.to_vec(),
        ));
        import::prune(&mut doc, now_unix.saturating_sub(SETTLED_RETENTION_SECS))?;
        import::put_entry(&mut doc, entry)?;
        doc.commit();
        let changes = doc
            .get_changes(&base)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        candidate
            .receive_from(
                TransportOrigin::RelayDelta(self.identity.native_cert.device_public_key),
                NeighbourMsg::SyncResp {
                    room: candidate.room().to_owned(),
                    changes,
                },
                now,
            )
            .map(drop)
    }

    /// Drop held documents whose proposals can no longer be accepted.
    fn forget_expired(&mut self, now_unix: u64) {
        let open: Vec<[u8; 32]> = self
            .peer()
            .admitted_room()
            .map(|room| {
                import::entries(room)
                    .into_values()
                    .filter(|entry| entry.state(now_unix) == ImportState::Pending)
                    .map(|entry| entry.proposal.proposal_id)
                    .collect()
            })
            .unwrap_or_default();
        self.proposed_documents.0.retain(|id, _| open.contains(id));
    }
}
