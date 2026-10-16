//! The `tilo.moderation` record a room's apps embed: reports anyone may
//! file, and the decisions a Moderator or the room host's moderation agent
//! takes on them. The agent itself lives with the room host
//! (`tilo-moderation`); an app only needs the record to offer reporting.

pub mod record;

pub use record::{Decision, Moderation, Outcome, Report, Reverted, Ruling, moderation};

/// The record kind reports and decisions live in.
pub const KIND: &str = "tilo.moderation";

/// The actor id the moderation agent acts under on its room's host.
pub const AGENT: &str = "agent:moderation";
