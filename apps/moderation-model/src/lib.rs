//! The `tilo.moderation` record a workspace keeps: reports anyone may file, and the
//! decisions a Moderator or the workspace host's moderation agent takes on them.

pub mod model;

pub use model::{
    Decision, Moderation, Outcome, Report, Reverted, Ruling, earlier_chat_history, moderation,
};

pub use tilo_actions::policy::roles::WORKSPACE_MODERATION_DOCUMENT as DOCUMENT;

pub const KIND: &str = "tilo.moderation";

/// The workspace host's moderation agent.
#[must_use]
pub fn agent() -> tilo_resource::MemberId {
    tilo_resource::MemberId::agent("moderation")
}
