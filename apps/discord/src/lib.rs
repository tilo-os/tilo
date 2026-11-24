//! The Discord connector: one
//! `discord.guild` resource per bridged server, whose verbs record what the
//! bridge bot observed and queue what it asks Discord to do, and whose
//! planning effects work out what a change means, because a bot's program
//! only sequences steps. The `discord-worker` binary performs the queued
//! calls and reports Discord's events.

// What a Discord event or a workspace change means for both sides, and the protocol client,
// live in the engine crates; the app keeps its resource, its views and its setup.
pub use discord_bridge::{
    bridge, emoji, emoji_sync, import, inbound, layout, order, outbound, roles, slug, tilo,
};
pub mod effects;
pub mod guard;
pub mod guild;
pub mod locales;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;

pub mod setup;
pub mod token;
pub mod voice;

pub use discord_wire::event;
pub use guild::{
    Connect, DChannel, DRole, Guild, Link, LinkOffer, MappedKind, Mapping, Origin, Ownership,
    map_key,
};

/// The connector's package id.
pub use ui::apps::DISCORD as APP_ID;
/// The resource kind it publishes.
pub const KIND: &str = "discord.guild";

/// The app's toolkit definition, which a workspace's host mounts so the bridge
/// bot can work on the guild resource.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(resource::Discord)
}

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source beside
/// it, so a device set to German reads a German window.
#[cfg(test)]
mod german_catalogue_tests {
    use crate::locales::CATALOG;

    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalogue = &CATALOG;
        assert!(
            catalogue.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalogue.languages()
        );
        catalogue.set_locale("de-DE");
        assert_eq!(
            catalogue.t("discord-empty-title"),
            "Einen Discord-Server in diesen Arbeitsbereich holen"
        );
        catalogue.set_locale("en-US");
        assert_eq!(
            catalogue.t("discord-empty-title"),
            "Bring a Discord server into this workspace"
        );
    }
}
