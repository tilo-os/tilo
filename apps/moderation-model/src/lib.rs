//! The `tilo.moderation` record a workspace keeps: reports anyone may file, and the
//! decisions a Moderator or the workspace host's moderation agent takes on them.

pub mod model;

pub use model::{Decision, Moderation, Outcome, Report, Reverted, Ruling, moderation};

pub const KIND: &str = "tilo.moderation";

/// The workspace's moderation record, which Chat keeps.
pub const DOCUMENT: &str = "derived:tilo.moderation@tab:0000000000000000";
