//! The Discord connector (docs/architecture/discord-bridge.md §2): one
//! `discord.guild` record per bridged server, whose verbs record what the
//! bridge bot observed and queue what it asks Discord to do, and whose
//! planning effects work out what a change means, because a bot's program
//! only sequences steps. The `discord-worker` binary performs the queued
//! calls and reports Discord's events.

pub mod bridge;
pub mod effects;
pub mod emoji;
pub mod emoji_sync;
pub mod guard;
pub mod import;
pub mod inbound;
pub mod layout;
pub mod locales;
pub mod outbound;
pub mod record;
pub mod resource;
pub mod roles;
pub mod setup;
pub mod slug;
pub mod tilo;
pub mod token;
pub mod voice;

pub use discord_wire::event;
pub use record::{
    Connect, DChannel, DRole, Guild, Link, LinkOffer, MappedKind, Mapping, Origin, Ownership,
    map_key,
};

/// The connector's package id.
pub const APP_ID: &str = "so.tilo.discord";
/// The record kind it publishes.
pub const KIND: &str = "discord.guild";

/// The app's toolkit definition, which a room's host mounts so the bridge
/// bot can work on the guild record.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(resource::Discord)
}
