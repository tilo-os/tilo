//! The `tilo.moderation` record a workspace's apps embed: reports anyone may file, and the
//! decisions a Moderator or the workspace host's moderation agent takes on them.

pub mod record;

pub use record::{Decision, Moderation, Outcome, Report, Reverted, Ruling, moderation};

pub const KIND: &str = "tilo.moderation";

pub const AGENT: &str = "agent:moderation";
