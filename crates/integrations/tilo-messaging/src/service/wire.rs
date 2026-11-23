//! What Messages and the service say over the app socket.
//!
//! The launcher connects the socket for the app and writes the handoff naming it
//! ([`Handoff`]) before the app holds the descriptor; the service then answers the
//! app's calls and tells it when its accounts' state or history changed.

use crate::{
    AccountState, EnrollmentPhase, EnrollmentReview, GroupChangeRequest, GroupHistory, GroupList,
    GroupMembership, GroupReceipt, GroupSendRequest, HistoryCursor, HistoryError, HistoryPage,
    LinkedAccount, Pairing, SendError, SendReceipt, SendRequest, ServiceAccount, SimChoice,
};
use serde::{Deserialize, Serialize};
use tilo_contact_directory::routing::Route;

#[derive(Serialize, Deserialize)]
pub struct Handoff {
    pub protocol: u16,
    pub app_id: String,
}

pub const PROTOCOL: u16 = 3;

pub const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4 * 1024);

#[derive(Serialize, Deserialize)]
pub enum GroupCall {
    List,
    Read(String),
    Change(GroupChangeRequest),
    Send(GroupSendRequest),
    History(String),
}

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
}

#[derive(Serialize, Deserialize)]
pub enum Reply {
    Done,
    Review(Result<EnrollmentReview, SendError>),
    Pairing(Result<Pairing, SendError>),
    Reviewed(Result<(), SendError>),
    Choices(Result<Vec<SimChoice>, SendError>),
    Sent(Result<SendReceipt, SendError>),
    History(Result<HistoryPage, HistoryError>),
    Groups(Result<GroupList, SendError>),
    Membership(Result<GroupMembership, SendError>),
    GroupReceipt(Result<GroupReceipt, SendError>),
    GroupHistory(Result<GroupHistory, SendError>),
    Linked(Result<LinkedAccount, SendError>),
    Refused,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountView {
    pub account: ServiceAccount,
    pub state: AccountState,
    pub groups: bool,
    /// The account's relinking, when it offers one: its generation and state, and when
    /// its presentation must be looked at again.
    pub setup: Option<SetupView>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupView {
    pub generation: u64,
    pub state: AccountState,
    pub wake_after_ms: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EnrollmentView {
    pub account: ServiceAccount,
    pub phase: EnrollmentPhase,
    pub linked: Option<LinkedAccount>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub actor: String,
    pub accounts: Vec<AccountView>,
    pub enrollments: Vec<EnrollmentView>,
}

#[derive(Serialize, Deserialize)]
pub enum Event {
    State(Snapshot),
    HistoryChanged,
}

#[cfg(unix)]
pub type AppLink = super::link::Link<Call, Reply, Event>;
