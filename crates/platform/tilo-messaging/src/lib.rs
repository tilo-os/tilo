//! Portable messaging account metadata the workspace keeps, read through
//! `ViewCx::shared_workspace` under the workspace's grants on its keeper.
//! It exposes neither credential storage nor a transport connection: selecting an
//! account does not authorize sending.
//!
//! Transport implementations stay behind the system messaging broker, which
//! validates the exact service, account, recipient and device grant again.

pub use accounts::person::{AccountList, ListedAccount};

mod groups;
mod history;
pub use groups::{
    GroupActionFuture, GroupChange, GroupChangeRequest, GroupClient, GroupHistory,
    GroupHistoryFuture, GroupHost, GroupList, GroupMember, GroupMembership, GroupMessage,
    GroupReceipt, GroupRole, GroupSendRequest, GroupSummary, GroupsFuture, MAX_GROUP_MEMBERS,
    MAX_GROUPS, MembershipFuture,
};
pub use history::{
    Delivery, Direction, HistoryClient, HistoryCursor, HistoryError, HistoryFuture, HistoryHost,
    HistoryPage, MAX_HISTORY_MESSAGES, Message,
};

mod sending;
pub use sending::{
    MessagingClient, MessagingHost, SendError, SendFuture, SendReceipt, SendRequest,
};

mod router;
pub use router::{MessagingRouter, RegistrationError, ServiceAccount};

mod registry;
pub use registry::{MessagingRegistry, messaging_registry};

mod account;
pub use account::AccountState;

mod enrollment;
pub use enrollment::{
    EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase, EnrollmentReview,
    EnrollmentReviewFuture, SimChoice, SimChoicesFuture,
};

#[cfg(feature = "service-client")]
pub mod service;

mod setup;
pub use setup::{LinkedAccount, LinkingFuture, Pairing, PairingFuture, SetupClient, SetupHost};
