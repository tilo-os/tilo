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
//!
//! The bridge uploads the workspace's emoji to the server itself
//! ([`crate::emoji_sync`]), so a `:name:` written or reacted in Tilo is the
//! server's emoji in Discord: `<:name:id>` in a message, `name:id` as a
//! reaction. A server's own emoji in a Discord message arrives as its
//! `:name:` too ([`from_discord_text`]).

use ui::tilo_resource::{custom_emoji_name, valid_custom_emoji_name};

use crate::record::{Guild, MappedKind};
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
        return server_emoji(guild, name).map(|(name, id)| format!("{name}:{id}"));
    }
    // Anything else written with colons is no emoji Discord knows.
    (!emoji.is_empty() && !emoji.contains(':')).then(|| emoji.to_owned())
}

/// Whether Discord takes `name` for a custom emoji: 2 to 32 letters,
/// digits and underscores, the rule a workspace emoji's name keeps too.
#[must_use]
pub fn valid_discord_name(name: &str) -> bool {
    valid_custom_emoji_name(name)
}

/// The name a workspace emoji `name` has in Discord: itself, since both
/// sides name emoji alike, with anything else made an underscore and the
/// length brought within 2 to 32.
#[must_use]
pub fn discord_name(name: &str) -> String {
    let mut named: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(ui::tilo_resource::reactions::MAX_CUSTOM_NAME)
        .collect();
    while named.len() < 2 {
        named.push('_');
    }
    named
}

/// The server's emoji standing for the workspace emoji `name`, as its name
/// in Discord and its id: the one the bridge uploaded for it, else the
/// server's own of that name.
#[must_use]
pub fn server_emoji<'g>(guild: &'g Guild, name: &str) -> Option<(String, &'g str)> {
    if let Some(uploaded) = guild.mapping(MappedKind::Emoji, name) {
        return Some((discord_name(name), uploaded.discord.as_str()));
    }
    guild.emoji(name).map(|id| (name.to_owned(), id))
}

/// `text` as Discord shows it: each `:name:` the server has an emoji for
/// written `<:name:id>`, everything else as it is.
#[must_use]
pub fn to_discord_text(guild: &Guild, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(':') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let after = from.get(1..).unwrap_or_default();
        let found = after.find(':').and_then(|close| {
            let name = after.get(..close)?;
            // Already Discord's form (`<:name:id>`) stays as it is.
            if before.ends_with('<') || !valid_custom_emoji_name(name) {
                return None;
            }
            server_emoji(guild, name).map(|(name, id)| (close, format!("<:{name}:{id}>")))
        });
        if let Some((close, written)) = found {
            out.push_str(&written);
            rest = after.get(close.saturating_add(1)..).unwrap_or_default();
        } else {
            out.push(':');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `text` as Tilo writes it: each custom emoji Discord wrote `<:name:id>`
/// (or `<a:name:id>`, animated) becomes `:name:`, which shows as the
/// workspace's emoji of that name, or as its text where it has none.
#[must_use]
pub fn from_discord_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let found = from.find('>').and_then(|close| {
            let inner = from.get(1..close)?;
            let inner = inner
                .strip_prefix("a:")
                .or_else(|| inner.strip_prefix(':'))?;
            let (name, id) = inner.split_once(':')?;
            (valid_custom_emoji_name(name)
                && !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit()))
            .then(|| (close, format!(":{name}:")))
        });
        if let Some((close, written)) = found {
            out.push_str(&written);
            rest = from.get(close.saturating_add(1)..).unwrap_or_default();
        } else {
            out.push('<');
            rest = from.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
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
    fn an_emoji_the_bridge_uploaded_stands_for_its_workspace_emoji() {
        let mut guild = guild_with(&[("party", "7")]);
        guild.map.push(crate::record::Mapping {
            key: crate::record::map_key(MappedKind::Emoji, "tilo_wave"),
            kind: MappedKind::Emoji,
            tilo: "tilo_wave".into(),
            discord: "91".into(),
            ..Default::default()
        });
        assert_eq!(
            to_discord(&guild, ":tilo_wave:").as_deref(),
            Some("tilo_wave:91")
        );
        assert_eq!(
            to_discord_text(&guild, "hi :tilo_wave: and :party: but :pizza: 10:30:45"),
            "hi <:tilo_wave:91> and <:party:7> but :pizza: 10:30:45"
        );
        assert_eq!(
            to_discord_text(&guild, "already <:party:7>, a:tilo_wave:b"),
            "already <:party:7>, a<:tilo_wave:91>b"
        );
    }

    #[test]
    fn a_servers_emoji_in_a_discord_message_arrives_as_its_name() {
        assert_eq!(
            from_discord_text("look <:party:7> <a:dance:8> <@12> <:no:x> <3"),
            "look :party: :dance: <@12> <:no:x> <3"
        );
    }

    #[test]
    fn a_discord_name_keeps_discords_rule() {
        assert_eq!(discord_name("tilo_wave"), "tilo_wave");
        assert_eq!(discord_name("a"), "a_");
        assert_eq!(discord_name("x-y"), "x_y");
        assert_eq!(discord_name(&"a".repeat(40)).len(), 32);
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
