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
use std::time::{Duration, Instant, SystemTime};

use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHost, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, Pending,
    ReceiptStatus, RemoteActionTarget,
};
use tilo_peer_state::document_import::{
    self as import, DocumentImportDecisionV1, DocumentImportProposalV1, DocumentImportTerms,
    ImportState, ProposeArgument,
};
use ui::record::RecordId;

use crate::shell::certified_action_session::CertifiedActionSession;
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use crate::shell::network::{NativeOwnerStatus, ShellNetwork};
use crate::shell::state::RoomId;

/// How long a proposal waits for the owner before it lapses.
pub const PROPOSAL_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// How long one import action may take on this device.
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a running import action is looked at again.
const POLL: Duration = Duration::from_millis(100);

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
    /// Whether `room` is governed by an owner this desktop reaches: its
    /// records are documents that owner commits, not sealed channels each
    /// app opens itself.
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

/// An import action's argument as the room's import surface takes it: the
/// typed argument, serialized exactly, under `import`.
fn action_argument(
    argument: &impl serde::Serialize,
) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("import".into(), serde_json::to_string(argument)?.into());
    Ok(arguments)
}

/// Invoke an action of the room's import surface as the person at this
/// screen, on a thread of its own — the caller asks from the frame loop,
/// and admitting the person reads this device's identity files — and answer
/// once the room recorded what it was asked.
fn invoke_room(
    room: RoomId,
    action: &'static str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Pending<Result<(), String>> {
    let (completer, answer) = tilo_actions::remote::pending();
    let spawned = std::thread::Builder::new()
        .name("tilo-document-import".into())
        .spawn(move || completer.complete(invoke_here(room, action, arguments)));
    if let Err(error) = spawned {
        return Pending::ready(Err(error.to_string()));
    }
    answer
}

fn invoke_here(
    room: RoomId,
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let broker = crate::shell::link_open::session_broker()?;
    let (host, caller) = CertifiedActionSession::open(broker, &room.to_string(), None)?.door();
    let deadline = Instant::now()
        .checked_add(ACTION_TIMEOUT)
        .unwrap_or_else(Instant::now);
    let provider = import_provider(host.as_ref(), &caller, action, deadline)?;
    let invocation = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider,
            address: ActionAddress::new(import::RESOURCE, action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let mut status = match host.invoke(&caller, request).wait_timeout(left.max(POLL)) {
        Ok(Some(Ok(receipt))) => receipt.status,
        Ok(Some(Err(error))) => return Err(error.to_string()),
        _ => return Err("the room did not answer".into()),
    };
    while matches!(status, ReceiptStatus::Accepted) {
        if Instant::now() >= deadline {
            return Err("the room did not record it in time".into());
        }
        std::thread::sleep(POLL);
        if let Some(state) = host.observe(&caller, &invocation) {
            status = state.receipt().status;
        }
    }
    match status {
        ReceiptStatus::Completed { .. } => Ok(()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Cancelled { .. } => Err("cancelled".into()),
        ReceiptStatus::Accepted => Err("still running".into()),
    }
}

/// The provider serving `action` of the room's import surface, as this
/// person's session sees it.
fn import_provider(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    action: &str,
    deadline: Instant,
) -> Result<String, String> {
    let query = CatalogueQuery {
        app_id: Some(import::APP_ID.to_owned()),
        needle: action.to_owned(),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let page = match host.query(caller, query).wait_timeout(left.max(POLL)) {
        Ok(Some(Ok(page))) => page,
        Ok(Some(Err(error))) => return Err(error.to_string()),
        _ => return Err("the room did not answer".into()),
    };
    page.rows
        .into_iter()
        .find(|row| {
            row.resource.id == import::RESOURCE
                && row
                    .action
                    .as_ref()
                    .is_some_and(|offered| offered.address.action == action)
        })
        .map(|row| row.provider)
        .ok_or_else(|| format!("this room does not take imports right now ({action})"))
}

/// Governed rooms as this desktop's network and native owner reach them.
///
/// Asked by chrome's window transfers on the frame loop, so only an owner
/// already open is read: opening one waits for the state file.
pub struct NativeHostedRooms(pub Arc<ShellNetwork>);

impl NativeHostedRooms {
    fn owner(&self) -> Result<NativeWorkspaceOwner, String> {
        match self.0.native_owner_status() {
            NativeOwnerStatus::Open(owner) => Ok(owner),
            NativeOwnerStatus::Opening => {
                Err("this desktop's workspace authority is still opening".into())
            }
            NativeOwnerStatus::Unavailable => Err("native owner unavailable".into()),
        }
    }

    /// This device's key for `room`, once its custody is current.
    fn signing_key(&self, room: RoomId) -> Result<ed25519_dalek::SigningKey, String> {
        self.0.native_signing_key_of(&self.owner()?, room)
    }
}

impl HostedRooms for NativeHostedRooms {
    fn hosts(&self, room: RoomId) -> bool {
        match self.0.native_owner_status() {
            // A room whose host takes part in its channels is sealed: its
            // records are channels each app opens and keeps in its own
            // profile, never documents the owner commits or reads out.
            NativeOwnerStatus::Open(owner) => {
                owner.contains_room(room)
                    && !owner
                        .with_current(room, |view| {
                            Ok(view.room_host().capabilities.takes_part_in_room_channels())
                        })
                        .unwrap_or(false)
            }
            // Not known yet: treated as governed, so nothing leaves or
            // enters it unchecked meanwhile.
            NativeOwnerStatus::Opening => true,
            NativeOwnerStatus::Unavailable => false,
        }
    }

    fn read(&self, room: RoomId, app: &str, record: &RecordId) -> Result<Vec<u8>, String> {
        use crate::shell::native_workspace_owner::DocumentRead;
        let owner = self.owner()?;
        self.0.check_native_custody(&owner, room)?;
        let limit =
            usize::try_from(tilo_actions::remote::MAX_FETCHED_RECORD_BYTES).unwrap_or(usize::MAX);
        owner.with_current(room, |view| {
            match view.read_document(
                view.device().device_public_key,
                app,
                if record.is_package_primary() {
                    ""
                } else {
                    record.as_str()
                },
                None,
                limit,
                SystemTime::now(),
            )? {
                DocumentRead::Captured(document) => Ok(document.bytes),
                DocumentRead::Unchanged(_) => Err("nothing was read".into()),
                DocumentRead::CheckpointPending => {
                    Err("the record's checkpoint is still arriving".into())
                }
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
        let signed = self.signing_key(room).and_then(|key| {
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
        invoke_room(room, import::PROPOSE, argument).map(move |answer| answer.map(|()| id))
    }

    fn proposal(&self, room: RoomId, id: &[u8; 32]) -> Option<ProposalStatus> {
        let owner = self.owner().ok()?;
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
        let owner = self.owner().ok()?;
        owner
            .with_current(room, |view| {
                let member = view
                    .peer()
                    .permissions(view.device().device_public_key, SystemTime::now())
                    .map_err(|error| error.to_string())?;
                if member.role != collab_types::Role::Owner {
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
        let decision = self.signing_key(room).and_then(|key| {
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
        invoke_room(room, action, argument)
    }
}
