//! Moderation of a shared room: people report people and content, a
//! moderation agent on the room's host looks into the reported person's
//! recent actions and decides, and every decision is a record a human
//! Moderator can overturn.
//!
//! Nothing here is a parallel mechanism. Reports and decisions are the
//! `tilo.moderation` record ([`record`]), changed through its verbs like any
//! record. A decision is enforced through the room's governance: a
//! temporary restriction is the room's expiring `restrict:<person>` Deny,
//! checked by the ordinary grant decision, and a ban is the room's `ban`.
//! Undoing what someone wrecked runs the published `undo` action of each
//! record they touched, with `by` naming them, so it lands in history as the
//! agent's step and can itself be undone ([`agent`]).
//!
//! The model is asked through [`judge::Judge`]; [`anthropic`] is the hosted
//! provider, configured as a provider profile with its key in the host's
//! secret store.

pub mod activity;
#[cfg(feature = "host")]
pub mod agent;
#[cfg(all(feature = "host", not(target_arch = "wasm32")))]
pub mod anthropic;
#[cfg(feature = "host")]
pub mod judge;
pub use moderation_model::{AGENT, KIND, record};

pub use activity::{Activity, Source, activity_of, burst, destructive_verbs};
#[cfg(feature = "host")]
pub use agent::{Enforced, Look, ModerationAgent, RoomDoor};
#[cfg(feature = "host")]
pub use judge::{Case, Judge, JudgeError, Verdict};
pub use record::{Decision, Moderation, Outcome, Report, Reverted, Ruling};

/// The model asked when the host configures none: small and cheap, since a
/// public room may see many reports.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
