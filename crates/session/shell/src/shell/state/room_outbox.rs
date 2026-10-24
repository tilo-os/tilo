//! What this device asks of its rooms' hosts, kept until the room's
//! document carries it or the host refuses it.
//!
//! A room's governed fields change only at its host, and reach this tree as
//! the projection of the room's admitted document
//! ([`WorkspaceTree::project_native_room`]), which replaces them. So
//! whatever this device asked that the document does not carry yet would
//! be overwritten by the room's last commit, whatever the room committed in
//! the meantime. Every such change is an entry of one outbox, saved with
//! the tree:
//!
//! - it is asked of the room's host, again after a restart or once a host
//!   out of reach may be reached ([`crate::shell::network::ShellNetwork`]
//!   asks what is [`Standing::Asking`]);
//! - the changes this device shows ahead of the host (a sandbox capability
//!   taken back, an effect receipt it recorded) stay in force over every
//!   projection until the room's document carries them;
//! - a refusal is said ([`WorkspaceTree::take_governance_refusals`]), and
//!   ends the entry, except a capability taken back: what this device no
//!   longer lets its apps do never comes back from the room it was asked of.
use serde::{Deserialize, Serialize};

use super::{CapOverride, GovernanceRefusal, GovernanceRequest, RoomId, Workspace, WorkspaceTree};
use tilo_desktop_model::WorkspaceEffectReceipt;

/// What an entry asks of its room's host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomChange {
    /// A change to the room's governance, for its governor to decide.
    Governance(GovernanceRequest),
    /// A terminal effect this device made in the room, for the room's
    /// history: the room's host commits it, once, by its invocation.
    Receipt(Box<WorkspaceEffectReceipt>),
}

/// Where an entry stands with its room's host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// To be asked: never answered, or its host was out of reach.
    #[default]
    Asking,
    /// The host took it; this device waits for the room's document to
    /// carry it.
    Accepted,
    /// The host refused a capability taken back, which stays taken back
    /// here for as long as the room carries it.
    Refused,
}

/// One change for a room's host ([`WorkspaceTree::room_outbox`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outgoing {
    /// Stable across restarts: a receipt's is its invocation, which its
    /// host records it under once however often it is asked.
    pub id: uuid::Uuid,
    pub room: RoomId,
    pub change: RoomChange,
    /// The capability a sandbox narrowing took back, as the room carried
    /// it: kept out of the workspace here until the room no longer carries
    /// it. None when the room never carried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takes_back: Option<CapOverride>,
    #[serde(default)]
    pub standing: Standing,
}

/// How a room's host answered an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAnswer {
    Accepted,
    Refused(String),
    /// Not asked: the host could not be reached.
    Unreachable,
}

impl Outgoing {
    fn new(room: RoomId, change: RoomChange, takes_back: Option<CapOverride>) -> Self {
        let id = match &change {
            RoomChange::Receipt(receipt) => receipt.invocation.0,
            RoomChange::Governance(_) => uuid::Uuid::new_v4(),
        };
        Self {
            id,
            room,
            change,
            takes_back,
            standing: Standing::Asking,
        }
    }

    /// Whether what this entry changes shows here ahead of the room's
    /// document, and so stays in force until the document carries it.
    fn shown_ahead(&self) -> bool {
        matches!(self.change, RoomChange::Receipt(_)) || self.takes_back.is_some()
    }

    /// Whether `document`, the room's admitted document, carries this entry.
    fn carried_by(&self, document: &automerge::AutoCommit) -> bool {
        if let Some(capability) = &self.takes_back {
            return !tilo_desktop_model::projection::room_sandbox_of(document).contains(capability);
        }
        match &self.change {
            RoomChange::Receipt(receipt) => {
                tilo_desktop_model::projection::workspace_effect_receipts_of(document)
                    .iter()
                    .any(|held| held.invocation == receipt.invocation)
            }
            RoomChange::Governance(_) => false,
        }
    }
}

/// Take what `document`, `workspace`'s room's admitted document, says
/// about it ([`tilo_peer_state::room::adopt_room`]), with what `outbox`
/// shows ahead of the room still in force: its receipts recorded, its
/// capabilities taken back. Returns whether the workspace changed.
pub(crate) fn adopt_room_document(
    workspace: &mut Workspace,
    document: &automerge::AutoCommit,
    outbox: &[Outgoing],
) -> bool {
    let room = workspace.room();
    let pending = || outbox.iter().filter(move |entry| entry.room == room);
    let kept: Vec<WorkspaceEffectReceipt> = pending()
        .filter_map(|entry| match &entry.change {
            RoomChange::Receipt(receipt) => Some((**receipt).clone()),
            RoomChange::Governance(_) => None,
        })
        .collect();
    let mut changed = tilo_peer_state::room::adopt_room(workspace, document, &kept);
    changed |= keep_taken_back(workspace, outbox);
    changed
}

/// Keep out of `workspace` every capability `outbox` took back in its room.
/// Returns whether anything was taken out.
pub(crate) fn keep_taken_back(workspace: &mut Workspace, outbox: &[Outgoing]) -> bool {
    let room = workspace.room();
    let before = workspace.sandbox.len();
    for capability in outbox
        .iter()
        .filter(|entry| entry.room == room)
        .filter_map(|entry| entry.takes_back.as_ref())
    {
        workspace.sandbox.retain(|held| held != capability);
    }
    workspace.sandbox.len() != before
}

impl WorkspaceTree {
    /// Ask `room`'s host for `change`, and keep it until the room's
    /// document carries it or the host refuses it.
    pub(crate) fn send_to_room(
        &mut self,
        room: RoomId,
        change: RoomChange,
        takes_back: Option<CapOverride>,
    ) {
        if let RoomChange::Governance(GovernanceRequest::GrantSandbox(capability)) = &change {
            // Granting a capability again is the person's newer word on it.
            self.room_outbox.retain(|entry| {
                entry.room != room
                    || entry
                        .takes_back
                        .as_ref()
                        .is_none_or(|taken| taken.id != capability.id)
            });
        }
        self.room_outbox
            .push(Outgoing::new(room, change, takes_back));
        self.queue_save();
    }

    /// Record `receipt` in `room`'s history: shown in its workspace at once,
    /// and committed by the room's host from the outbox. Whether it was
    /// new.
    pub(crate) fn record_in_room(&mut self, room: RoomId, receipt: WorkspaceEffectReceipt) -> bool {
        let workspace = self
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| workspace.id);
        if let Some(workspace) = workspace
            && self
                .desktop
                .append_workspace_effect_receipt(workspace, receipt.clone())
                .is_none()
        {
            return false;
        }
        if self
            .room_outbox
            .iter()
            .any(|entry| entry.id == receipt.invocation.0)
        {
            return false;
        }
        self.send_to_room(room, RoomChange::Receipt(Box::new(receipt)), None);
        true
    }

    /// The entries to ask of their rooms' hosts, in the order they were
    /// made.
    pub fn outbox_to_ask(&self) -> impl Iterator<Item = &Outgoing> {
        self.room_outbox
            .iter()
            .filter(|entry| entry.standing == Standing::Asking)
    }

    /// The host of entry `id`'s room answered `answer`. An accepted change
    /// this device shows ahead of the room waits for the room's document;
    /// any other leaves the outbox. A refusal is said, and what this
    /// device showed ahead of it returns to what the room has, except a
    /// capability taken back.
    pub fn outbox_answered(&mut self, id: uuid::Uuid, answer: HostAnswer) {
        let Some(index) = self.room_outbox.iter().position(|entry| entry.id == id) else {
            // The room's document carried it while it was asked.
            return;
        };
        let Some(entry) = self.room_outbox.get_mut(index) else {
            return;
        };
        match answer {
            HostAnswer::Unreachable => {}
            HostAnswer::Accepted => {
                let carried = self
                    .room_docs
                    .get(&entry.room)
                    .is_some_and(|document| entry.carried_by(document));
                if entry.shown_ahead() && !carried {
                    entry.standing = Standing::Accepted;
                } else {
                    self.room_outbox.remove(index);
                }
                self.queue_save();
            }
            HostAnswer::Refused(reason) => {
                let (room, change) = (entry.room, entry.change.clone());
                if entry.takes_back.is_some() {
                    entry.standing = Standing::Refused;
                } else {
                    self.room_outbox.remove(index);
                }
                self.refuse_room_change(room, change, reason);
            }
        }
    }

    /// Drop every entry for `room` its admitted `document` carries.
    pub(super) fn settle_outbox(&mut self, room: RoomId) {
        let Some(document) = self.room_docs.get(&room) else {
            return;
        };
        let before = self.room_outbox.len();
        self.room_outbox
            .retain(|entry| entry.room != room || !entry.carried_by(document));
        if self.room_outbox.len() != before {
            self.queue_save();
        }
    }

    /// `room`'s host refused `change` for `reason`: what this device showed
    /// ahead of the answer returns to what the room's admitted document
    /// says, but for what the outbox still holds in force, and the refusal
    /// waits for the chrome to say it.
    pub(crate) fn refuse_room_change(&mut self, room: RoomId, change: RoomChange, reason: String) {
        if let Some(document) = self.room_docs.get(&room)
            && let Some(workspace) = self
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == room)
        {
            // This device's own sandbox capabilities (a files-home consent)
            // are not the room's, and stay.
            let held = workspace.sandbox.clone();
            adopt_room_document(workspace, document, &self.room_outbox);
            let own: Vec<_> = held
                .into_iter()
                .filter(|cap| {
                    !workspace
                        .sandbox
                        .iter()
                        .any(|room_cap| room_cap.id == cap.id)
                })
                .collect();
            workspace.sandbox.extend(own);
            keep_taken_back(workspace, &self.room_outbox);
            self.queue_save();
        }
        self.governance_refusals.push(GovernanceRefusal {
            room,
            change,
            reason,
        });
    }
}
