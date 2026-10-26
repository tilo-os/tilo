//! Emoji as each side of the bridge writes them (discord-bridge.md §3,
//! "Reactions").
//!
//! A Unicode emoji crosses as itself, skin tone and all. Discord and Tilo
//! may spell one emoji with or without its variation selector (`❤️` and
//! `❤`), so the bridge compares emoji by [`key`], and a Discord reaction
//! takes the spelling the message's Tilo reactions already use.
//!
//! A workspace's own emoji is written `:name:` in Tilo, under the same
//! naming rule as Discord's custom emoji, so a Discord custom emoji arrives
//! as its `:name:` and shows as the workspace's emoji of that name (or as
//! its text where the workspace has none). The other way, see [`to_discord`].

use ui::tilo_resource::custom_emoji_name;

use crate::record::Guild;
use crate::tilo::Reaction;

/// Variation selector 16, asking for an emoji's colourful presentation.
const EMOJI_PRESENTATION: char = '\u{fe0f}';

/// `emoji` as the bridge compares it: without variation selectors, so two
/// spellings of one emoji are one.
#[must_use]
pub fn key(emoji: &str) -> String {
    emoji.chars().filter(|c| *c != EMOJI_PRESENTATION).collect()
}

/// What the bot reacts with in Discord for the Tilo reaction `emoji`, in
/// the form Discord's reaction routes take; none when it stays in Tilo.
///
/// A workspace emoji `:name:` becomes the server's custom emoji of that
/// name (`name:id`) when the server has one. Otherwise it is not mirrored:
///
/// - uploading the picture as a server emoji would need Manage
///   Expressions, which the bridge does not ask for, and would spend the
///   server's few emoji slots on whatever anyone in the room reacts with;
/// - a Unicode emoji whose name matches (`:pizza:` as 🍕) would show
///   Discord something the person did not pick: a workspace emoji is the
///   workspace's own picture, whatever it is called. Matching names would
///   also carry the whole emoji catalogue in the connector for it.
///
/// Discord readers miss that one reaction; everything else about the
/// message still crosses, and the reaction shows once the server adds an
/// emoji of that name and someone reacts again.
#[must_use]
pub fn to_discord(guild: &Guild, emoji: &str) -> Option<String> {
    if let Some(name) = custom_emoji_name(emoji) {
        return guild.emoji(name).map(|id| format!("{name}:{id}"));
    }
    // Anything else written with colons is no emoji Discord knows.
    (!emoji.is_empty() && !emoji.contains(':')).then(|| emoji.to_owned())
}

/// The Tilo form of a Discord reaction's `emoji` on a message holding
/// `reactions`: a custom emoji's `:name:` as it is, a Unicode one spelled
/// as a reaction there already spells it.
#[must_use]
pub fn to_tilo(emoji: &str, reactions: &[Reaction]) -> String {
    if custom_emoji_name(emoji).is_some() {
        return emoji.to_owned();
    }
    let wanted = key(emoji);
    reactions
        .iter()
        .find(|reaction| key(&reaction.emoji) == wanted)
        .map_or_else(|| emoji.to_owned(), |reaction| reaction.emoji.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guild_with(emoji: &[(&str, &str)]) -> Guild {
        let mut guild = Guild::default();
        guild.observe_emojis(
            emoji
                .iter()
                .map(|(name, id)| discord_wire::event::EmojiInfo {
                    id: (*id).into(),
                    name: (*name).into(),
                    animated: false,
                })
                .collect(),
        );
        guild
    }

    #[test]
    fn unicode_crosses_as_itself_and_compares_without_selectors() {
        let guild = Guild::default();
        assert_eq!(to_discord(&guild, "👍🏽").as_deref(), Some("👍🏽"));
        assert_eq!(to_discord(&guild, "❤️").as_deref(), Some("❤️"));
        assert_eq!(key("❤️"), key("❤"));
        assert_ne!(key("👍🏽"), key("👍"), "a skin tone is its own emoji");
    }

    #[test]
    fn a_workspace_emoji_is_the_servers_emoji_of_its_name_or_stays_in_tilo() {
        let guild = guild_with(&[("tilo_hello", "55")]);
        assert_eq!(
            to_discord(&guild, ":tilo_hello:").as_deref(),
            Some("tilo_hello:55")
        );
        assert_eq!(to_discord(&guild, ":pizza:"), None);
        assert_eq!(to_discord(&guild, ":not an emoji:"), None);
    }

    #[test]
    fn a_discord_reaction_takes_the_spelling_tilo_already_uses() {
        let held = [Reaction {
            emoji: "❤".into(),
            by: "u_ada".into(),
            ..Reaction::default()
        }];
        assert_eq!(to_tilo("❤️", &held), "❤");
        assert_eq!(to_tilo("❤️", &[]), "❤️");
        assert_eq!(to_tilo(":tilo_hello:", &held), ":tilo_hello:");
    }
}
