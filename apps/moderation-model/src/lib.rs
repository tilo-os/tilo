//! The `tilo.moderation` record a room's apps embed: reports anyone may
//! file, and the decisions a Moderator or the room host's moderation agent
//! takes on them. The agent itself lives with the room host
//! (`tilo-moderation`); an app only needs the record to offer reporting.
//!
//! This crate has no window and no surface of its own: it ships no
//! `tilo-app.toml` and no `surface.toml`, so nothing it holds is drawn by
//! this crate. What a person sees of moderation is another app's surface
//! rendering a `tilo.moderation` record — report and decision rows in
//! Messages, Chat and the room's governance pages — so a missing affordance
//! there is that app's, not this record's.

pub mod record;

pub use record::{Decision, Moderation, Outcome, Report, Reverted, Ruling, moderation};

/// The record kind reports and decisions live in.
pub const KIND: &str = "tilo.moderation";

/// The actor id the moderation agent acts under on its room's host.
pub const AGENT: &str = "agent:moderation";
