//! What a Discord event or a workspace change means for both sides.
//!
//! The app's own verbs record what the bridge observed and queue what it asks Discord to do; the
//! planners here work out what a change means, because a bot's program only sequences steps.
//! One entry, [`plan`], answers every list of verb calls one event means.

pub mod bridge;
pub mod emoji;
pub mod emoji_sync;
pub mod format;
pub mod guild;
pub mod import;
pub mod inbound;
pub mod layout;
pub mod order;
pub mod outbound;
pub mod roles;
pub mod slug;
pub mod tilo;

pub use bridge::{BridgePlan, Reading, plan};
pub use guild::Guild;

/// The app whose plans these are.
pub const APP_ID: &str = "so.tilo.discord";

/// The kind of the app's own resource: what a change to it names.
pub const KIND: &str = "tl.so.tilo.discord.Guild";
