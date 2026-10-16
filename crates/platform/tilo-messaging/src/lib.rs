//! Portable messaging account metadata supplied by the person-record host.
//!
//! Apps read these records through `ViewCx::shared_person`. The host enforces
//! their existing owner/editor permissions. This API exposes neither credential
//! custody nor a transport connection: selecting an account does not authorize
//! sending, and an account record is not evidence of a linked device.
//!
//! Transport implementations remain behind the system messaging broker; they
//! must validate the exact service, account, recipient and device grant again.

pub use accounts::person::{PersonAccount, PersonAccounts};

mod groups;
mod history;
pub use groups::{
    GroupActionFuture, GroupChange, GroupChangeRequest, GroupClient, GroupHistory,
    GroupHistoryFuture, GroupHost, GroupList, GroupMember, GroupMembership, GroupMessage,
    GroupReceipt, GroupRole, GroupSendRequest, GroupSummary, GroupsFuture, MAX_GROUP_MEMBERS,
    MAX_GROUPS, MembershipFuture,
};
pub use history::{
    Delivery, Direction, HistoryClient, HistoryError, HistoryFuture, HistoryHost, HistoryPage,
    MAX_HISTORY_MESSAGES, Message,
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

mod setup;
pub use setup::{LinkedAccount, LinkingFuture, Pairing, PairingFuture, SetupClient, SetupHost};
