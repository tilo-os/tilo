//! The `tilo.moderation` record a workspace keeps: reports anyone may file, and the
//! decisions a Moderator or the workspace host's moderation agent takes on them.

pub mod model;

pub use model::{Decision, Moderation, Outcome, Report, Reverted, Ruling, moderation};

pub use tilo_actions::policy::roles::WORKSPACE_MODERATION_DOCUMENT as DOCUMENT;

pub const KIND: &str = "tilo.moderation";
