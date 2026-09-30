//! Bringing a document into a governed workspace, as this desktop takes
//! part in it ([`tilo_peer_state::document_import`]).
//!
//! As a member, this desktop reads a record out of a governed room through
//! the bounded committed read, and proposes a document as a new record of
//! one: its device signs the proposal, and the room's governor records it
//! once it has checked that this person could create that record. As the
//! room's owner, this desktop asks its person whether to accept each
//! proposal waiting for them and sends the signed answer. Only the governor
//! creates the record, and only for an accepted proposal.
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tilo_actions::remote::Pending;
use tilo_peer_state::document_import::{
    self as import, DocumentImportDecisionV1, DocumentImportProposalV1, DocumentImportTerms,
    ImportState, ProposeArgument,
};
use ui::record::RecordId;

use crate::shell::network::ShellNetwork;
use crate::shell::state::RoomId;

/// How long a proposal waits for the owner before it lapses.
pub const PROPOSAL_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Where a proposal stands, as this device's copy of the room says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalStatus {
    pub state: ImportState,
    /// The accepted record has arrived here, so it can be opened.
    pub record_present: bool,
}

/// A proposal waiting for this desktop's person to answer as owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Awaiting {
    pub proposal: DocumentImportProposalV1,
    /// The proposing person, when the room still knows their device.
    pub proposer: Option<String>,
}

/// What this desktop needs from governed rooms to carry a document into or
/// out of one.
pub trait HostedRooms {
    /// Whether `room` is governed by an owner this desktop reaches.
    fn hosts(&self, room: RoomId) -> bool;
    /// `app`'s `record` in `room` as the room committed it, read as this device.
    ///
    /// # Errors
    /// This device may not read it, or the room does not hold it.
    fn read(&self, room: RoomId, app: &str, record: &RecordId) -> Result<Vec<u8>, String>;
    /// Propose `document` as `app`'s new record `record` in `room`. Answers
    /// with the proposal's id once the governor has recorded it.
    fn propose(
        &self,
        room: RoomId,
        app: &str,
        record: &RecordId,
        title: &str,
        document: &[u8],
    ) -> Pending<Result<[u8; 32], String>>;
    /// Where proposal `id` in `room` stands.
    fn proposal(&self, room: RoomId, id: &[u8; 32]) -> Option<ProposalStatus>;
    /// Proposals in `room` waiting for this desktop's person as its owner;
    /// `None` when they do not own it.
    fn awaiting_me(&self, room: RoomId) -> Option<Vec<Awaiting>>;
    /// Answer `proposal` in `room`, signed by this device.
    fn decide(
        &self,
        room: RoomId,
        proposal: &DocumentImportProposalV1,
        accepted: bool,
    ) -> Pending<Result<(), String>>;
}

fn fresh_id() -> [u8; 32] {
    let mut id = [0; 32];
    id[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    id[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    id
}

/// An import action's argument as the governor takes it: the typed
/// argument, serialized exactly, under `import`.
fn action_argument(
    argument: &impl serde::Serialize,
) -> Result<serde_json::Value, serde_json::Error> {
    Ok(serde_json::json!({ "import": serde_json::to_string(argument)? }))
}

/// Governed rooms as this desktop's network and native owner reach them.
pub struct NativeHostedRooms(pub Arc<ShellNetwork>);

impl HostedRooms for NativeHostedRooms {
    fn hosts(&self, room: RoomId) -> bool {
        self.0
            .native_owner()
            .is_ok_and(|owner| owner.contains_room(room))
    }

    fn read(&self, room: RoomId, app: &str, record: &RecordId) -> Result<Vec<u8>, String> {
        use crate::shell::native_workspace_owner::DocumentRead;
        let owner = self.0.native_owner()?;
        self.0.check_native_custody(&owner, room)?;
        let limit =
            usize::try_from(tilo_actions::remote::MAX_FETCHED_RECORD_BYTES).unwrap_or(usize::MAX);
        owner.with_current(room, |view| {
            match view.read_document(
                view.device().device_public_key,
                app,
                record.as_str(),
                None,
                limit,
                SystemTime::now(),
            )? {
                DocumentRead::Captured(document) => Ok(document.bytes),
                DocumentRead::Unchanged(_) => Err("nothing was read".into()),
            }
        })
    }

    fn propose(
        &self,
        room: RoomId,
        app: &str,
        record: &RecordId,
        title: &str,
        document: &[u8],
    ) -> Pending<Result<[u8; 32], String>> {
        let signed = self.0.native_signing_key(room).and_then(|key| {
            let issued = tilo_clock::unix_now();
            DocumentImportProposalV1::new(
                &DocumentImportTerms {
                    room: room.to_string(),
                    proposal_id: fresh_id(),
                    app: app.to_owned(),
                    record: record.as_str().to_owned(),
                    title: title.to_owned(),
                    document: document.to_vec(),
                    issued_unix: issued,
                    expires_unix: issued.saturating_add(PROPOSAL_LIFETIME.as_secs()),
                },
                &key,
            )
            .map_err(|error| error.to_string())
        });
        let proposal = match signed {
            Ok(proposal) => proposal,
            Err(reason) => return Pending::ready(Err(reason)),
        };
        let id = proposal.proposal_id;
        let argument = match action_argument(&ProposeArgument::new(proposal, document)) {
            Ok(argument) => argument,
            Err(error) => return Pending::ready(Err(error.to_string())),
        };
        self.0
            .invoke_governor(
                room,
                import::APP_ID,
                import::RESOURCE,
                import::PROPOSE,
                Some(argument),
            )
            .map(move |answer| answer.map(|_| id))
    }

    fn proposal(&self, room: RoomId, id: &[u8; 32]) -> Option<ProposalStatus> {
        let owner = self.0.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                let doc = view.peer().admitted_room().ok_or("no room")?;
                let entry = import::entries(doc)
                    .remove(&import::key(id))
                    .ok_or("no such proposal")?;
                Ok(ProposalStatus {
                    state: entry.state(tilo_clock::unix_now()),
                    record_present: view
                        .peer()
                        .document_heads(&entry.proposal.app, &entry.proposal.record)
                        .is_some(),
                })
            })
            .ok()
    }

    fn awaiting_me(&self, room: RoomId) -> Option<Vec<Awaiting>> {
        // Only a listing: answering signs, and checks custody then.
        let owner = self.0.native_owner().ok()?;
        owner
            .with_current(room, |view| {
                let (role, _, _) = view
                    .peer()
                    .permissions(view.device().device_public_key, SystemTime::now())?;
                if role != collab_types::Role::Owner {
                    return Ok(None);
                }
                let doc = view.peer().admitted_room().ok_or("no room")?;
                let certificates = view.peer().certificates();
                let now = tilo_clock::unix_now();
                Ok(Some(
                    import::entries(doc)
                        .into_values()
                        .filter(|entry| entry.state(now) == ImportState::Pending)
                        .map(|entry| Awaiting {
                            proposer: certificates
                                .iter()
                                .find(|cert| {
                                    cert.device_public_key == *entry.proposal.proposer.as_bytes()
                                })
                                .map(|cert| {
                                    collab_types::caps::Principal::person_from_key(
                                        collab_types::Ed25519PublicKey::from_bytes(
                                            cert.user_public_key,
                                        ),
                                    )
                                    .id()
                                    .to_owned()
                                }),
                            proposal: entry.proposal,
                        })
                        .collect(),
                ))
            })
            .ok()
            .flatten()
    }

    fn decide(
        &self,
        room: RoomId,
        proposal: &DocumentImportProposalV1,
        accepted: bool,
    ) -> Pending<Result<(), String>> {
        let decision = self.0.native_signing_key(room).and_then(|key| {
            DocumentImportDecisionV1::new(proposal, accepted, tilo_clock::unix_now(), &key)
                .map_err(|error| error.to_string())
        });
        let argument = decision.and_then(|decision| {
            action_argument(&import::DecideArgument {
                proposal_id: proposal.proposal_id,
                decision,
            })
            .map_err(|error| error.to_string())
        });
        let argument = match argument {
            Ok(argument) => argument,
            Err(reason) => return Pending::ready(Err(reason)),
        };
        let action = if accepted {
            import::ACCEPT
        } else {
            import::DECLINE
        };
        self.0
            .invoke_governor(
                room,
                import::APP_ID,
                import::RESOURCE,
                action,
                Some(argument),
            )
            .map(|answer| answer.map(drop))
    }
}
