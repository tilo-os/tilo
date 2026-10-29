//! What Messages and the service say over the app socket.
//!
//! The launcher connects the socket for the app and writes the handoff
//! naming it ([`Handoff`]) before the app holds the descriptor, as it does
//! for the model gateway; the service then reads calls from the app and
//! answers them, and tells it when its accounts' state or history changed.

use crate::{
    AccountState, EnrollmentPhase, EnrollmentReview, GroupChangeRequest, GroupHistory, GroupList,
    GroupMembership, GroupReceipt, GroupSendRequest, HistoryCursor, HistoryError, HistoryPage,
    LinkedAccount, Pairing, SendError, SendReceipt, SendRequest, ServiceAccount, SimChoice,
};
use serde::{Deserialize, Serialize};
use tilo_contact_directory::routing::Route;

/// The launcher's handoff, written before the app holds the connection:
/// which app it is. Same shape as the model gateway's.
#[derive(Serialize, Deserialize)]
pub struct Handoff {
    pub protocol: u16,
    pub app_id: String,
}

pub const PROTOCOL: u16 = 1;

/// The handoff's framing: a little-endian `u32` length, then JSON.
pub const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4 * 1024);

/// A group operation on one account.
#[derive(Serialize, Deserialize)]
pub enum GroupCall {
    List,
    Read(String),
    Change(GroupChangeRequest),
    Send(GroupSendRequest),
    History(String),
}

/// An app's call.
#[derive(Serialize, Deserialize)]
pub enum Call {
    Prepare(String),
    Begin(String),
    Finish(ServiceAccount, String),
    Review(ServiceAccount, String),
    Cancel(ServiceAccount),
    SimChoices,
    SelectSim(String),
    CancelSimSelection,
    Send(SendRequest),
    History {
        route: Route,
        cursor: Option<HistoryCursor>,
        limit: usize,
    },
    Group(ServiceAccount, GroupCall),
    SetupBegin(ServiceAccount),
    SetupFinish(ServiceAccount, String),
    SetupCancel(ServiceAccount),
    /// Passwords: the permission requests waiting on the person.
    Proposals,
    /// Passwords: the person's answer to one.
    Decide {
        proposal: String,
        decision: Decision,
    },
}

/// The person's answer to a permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Deny,
    /// For this session only.
    Session,
    Always,
}

/// One request for permission the service waits on, as Passwords shows it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub app_id: String,
    pub item: String,
    pub purpose: String,
    pub origin: Option<String>,
    pub scope: accounts::CredentialScope,
}

impl Proposal {
    /// The request as the vault describes it.
    pub fn credential(&self) -> accounts::CredentialProposal {
        accounts::CredentialProposal {
            id: self.id.clone(),
            request: accounts::AccessRequest {
                app_id: self.app_id.clone(),
                item_id: accounts::SecretId(self.item.clone()),
                purpose: self.purpose.clone(),
                origin: self.origin.clone(),
            },
            scope: self.scope.clone(),
        }
    }
}

/// The service's answer to one call.
#[derive(Serialize, Deserialize)]
pub enum Reply {
    Done,
    Review(Result<EnrollmentReview, SendError>),
    Pairing(Result<Pairing, SendError>),
    Proposal(Result<String, SendError>),
    Choices(Result<Vec<SimChoice>, SendError>),
    Sent(Result<SendReceipt, SendError>),
    History(Result<HistoryPage, HistoryError>),
    Groups(Result<GroupList, SendError>),
    Membership(Result<GroupMembership, SendError>),
    GroupReceipt(Result<GroupReceipt, SendError>),
    GroupHistory(Result<GroupHistory, SendError>),
    Linked(Result<LinkedAccount, SendError>),
    Proposals(Vec<Proposal>),
    Refused,
}

/// A running account as the app sees it, without a call.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountView {
    pub account: ServiceAccount,
    pub state: AccountState,
    pub groups: bool,
    /// The account's relinking, when it offers one: its generation, state and
    /// when its presentation must be looked at again.
    pub setup: Option<SetupView>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupView {
    pub generation: u64,
    pub state: AccountState,
    pub wake_after_ms: Option<u64>,
}

/// An enrollment as the app sees it.
#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollmentView {
    pub account: ServiceAccount,
    pub phase: EnrollmentPhase,
    pub linked: Option<LinkedAccount>,
}

/// What the app reads without asking: every account and enrollment.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// The person the service holds accounts for.
    pub principal: String,
    pub accounts: Vec<AccountView>,
    pub enrollments: Vec<EnrollmentView>,
    pub proposals: u64,
}

/// What the service tells the app unasked.
#[derive(Serialize, Deserialize)]
pub enum Event {
    State(Snapshot),
    HistoryChanged,
}

#[cfg(unix)]
pub type AppLink = super::link::Link<Call, Reply, Event>;
