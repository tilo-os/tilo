//! The bot's permissions in one channel, computed the way Discord does
//! from what the gateway already knows of the guild: the owner has all,
//! then `@everyone` and the bot's roles, `ADMINISTRATOR` meaning all, then
//! the channel's overwrites for `@everyone`, for the bot's roles together,
//! and for the bot itself.

use discord_wire::perm;

use crate::events::Guild;

pub const ADMINISTRATOR: u64 = 1 << 3;
pub const CHANGE_NICKNAME: u64 = 1 << 26;
pub const SET_VOICE_CHANNEL_STATUS: u64 = 1 << 48;

/// What joining a voice channel and speaking there takes, with the names
/// Discord's client gives them.
pub const TO_SPEAK: [(u64, &str); 3] = [
    (perm::VIEW_CHANNEL, "View Channel"),
    (perm::CONNECT, "Connect"),
    (perm::SPEAK, "Speak"),
];

const OVERWRITE_ROLE: u8 = 0;
const OVERWRITE_MEMBER: u8 = 1;

/// The bot's permissions in `channel`.
#[must_use]
pub fn in_channel(guild: &Guild, channel: &str) -> u64 {
    if !guild.owner.is_empty() && guild.owner == guild.bot_user {
        return u64::MAX;
    }
    let role = |id: &str| {
        guild
            .roles
            .get(id)
            .map_or(0, |role| perm::parse(&role.permissions))
    };
    let mut base = role(&guild.id);
    for id in &guild.bot_roles {
        base |= role(id);
    }
    if base & ADMINISTRATOR != 0 {
        return u64::MAX;
    }
    let Some(channel) = guild.channels.get(channel) else {
        return base;
    };
    let overwrites = &channel.overwrites;
    if let Some(everyone) = overwrites
        .iter()
        .find(|o| o.kind == OVERWRITE_ROLE && o.id == guild.id)
    {
        base &= !perm::parse(&everyone.deny);
        base |= perm::parse(&everyone.allow);
    }
    let (mut allow, mut deny) = (0, 0);
    for overwrite in overwrites
        .iter()
        .filter(|o| o.kind == OVERWRITE_ROLE && guild.bot_roles.contains(&o.id))
    {
        allow |= perm::parse(&overwrite.allow);
        deny |= perm::parse(&overwrite.deny);
    }
    base &= !deny;
    base |= allow;
    if let Some(member) = overwrites
        .iter()
        .find(|o| o.kind == OVERWRITE_MEMBER && o.id == guild.bot_user)
    {
        base &= !perm::parse(&member.deny);
        base |= perm::parse(&member.allow);
    }
    base
}

/// The names of what the bot lacks to join `channel` and speak there.
#[must_use]
pub fn missing_to_speak(guild: &Guild, channel: &str) -> Vec<&'static str> {
    let have = in_channel(guild, channel);
    TO_SPEAK
        .iter()
        .filter(|(bit, _)| have & bit == 0)
        .map(|(_, name)| *name)
        .collect()
}

/// `["Connect", "Speak"]` as "Connect and Speak".
#[must_use]
pub fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;
    use serde_json::json;

    fn guild(overwrites: &serde_json::Value, bot_roles: &[&str]) -> Guild {
        let mut guild = Guild::from_create(
            &json!({
                "id": "1000", "owner_id": "3000",
                "channels": [{"id": "1102", "type": 2, "name": "voice", "permission_overwrites": overwrites}],
                "roles": [
                    {"id": "1000", "name": "@everyone", "permissions": perm::bits(perm::VIEW_CHANNEL | perm::CONNECT | perm::SPEAK)},
                    {"id": "1200", "name": "Tilo Bridge", "permissions": "8"},
                    {"id": "1300", "name": "Muted", "permissions": "0"},
                ],
            }),
            "2000",
            "https://cdn.discordapp.com",
        );
        guild.bot_roles = bot_roles.iter().map(|r| (*r).to_owned()).collect();
        guild
    }

    #[test]
    fn everyone_may_speak_unless_an_overwrite_says_otherwise() {
        assert!(missing_to_speak(&guild(&json!([]), &[]), "1102").is_empty());
        let denied = guild(
            &json!([{"id": "1000", "type": 0, "allow": "0", "deny": perm::bits(perm::CONNECT | perm::SPEAK)}]),
            &[],
        );
        assert_eq!(missing_to_speak(&denied, "1102"), vec!["Connect", "Speak"]);
        assert_eq!(
            and_list(&missing_to_speak(&denied, "1102")),
            "Connect and Speak"
        );
    }

    #[test]
    fn role_and_member_overwrites_apply_in_order() {
        let role_denied = guild(
            &json!([{"id": "1300", "type": 0, "allow": "0", "deny": perm::bits(perm::SPEAK)}]),
            &["1300"],
        );
        assert_eq!(missing_to_speak(&role_denied, "1102"), vec!["Speak"]);
        let member_allowed = guild(
            &json!([
                {"id": "1300", "type": 0, "allow": "0", "deny": perm::bits(perm::SPEAK)},
                {"id": "2000", "type": 1, "allow": perm::bits(perm::SPEAK), "deny": "0"},
            ]),
            &["1300"],
        );
        assert!(missing_to_speak(&member_allowed, "1102").is_empty());
    }

    #[test]
    fn administrators_and_owners_have_everything() {
        let denied =
            json!([{"id": "1000", "type": 0, "allow": "0", "deny": perm::bits(perm::CONNECT)}]);
        assert!(missing_to_speak(&guild(&denied, &["1200"]), "1102").is_empty());
        let mut owned = guild(&denied, &[]);
        owned.owner = "2000".into();
        assert!(missing_to_speak(&owned, "1102").is_empty());
        assert_eq!(
            and_list(&["View Channel", "Connect", "Speak"]),
            "View Channel, Connect and Speak"
        );
    }

    /// An overwrite counts only for whom it names (`@everyone`, a role the
    /// bot has, the bot itself), in that order, and a channel the guild does
    /// not list has the server's permissions.
    #[test]
    fn an_overwrite_applies_only_to_whom_it_names() {
        // Whom the overwrites name, the overwrites, the bot's roles, and what
        // the bot then lacks.
        type Row = (
            &'static str,
            serde_json::Value,
            &'static [&'static str],
            &'static [&'static str],
        );
        let speak = |id: &str, kind: u8, allowed: bool| {
            let (allow, deny) = if allowed {
                (perm::SPEAK, 0)
            } else {
                (0, perm::SPEAK)
            };
            json!({"id": id, "type": kind, "allow": perm::bits(allow), "deny": perm::bits(deny)})
        };
        let everything = perm::VIEW_CHANNEL | perm::CONNECT | perm::SPEAK;
        let rows: [Row; 6] = [
            (
                "a role the bot does not have",
                json!([speak("1300", 0, false)]),
                &[],
                &[],
            ),
            ("another member", json!([speak("3001", 1, false)]), &[], &[]),
            (
                "a role overwrite that carries the bot's own id",
                json!([speak("2000", 0, false)]),
                &[],
                &[],
            ),
            (
                "the bot's role gives back what @everyone lost",
                json!([speak("1000", 0, false), speak("1300", 0, true)]),
                &["1300"],
                &[],
            ),
            (
                "the bot's own overwrite has the last word",
                json!([
                    speak("1000", 0, false),
                    speak("1300", 0, true),
                    speak("2000", 1, false)
                ]),
                &["1300"],
                &["Speak"],
            ),
            (
                "everything denied to @everyone",
                json!([{"id": "1000", "type": 0, "allow": "0", "deny": perm::bits(everything)}]),
                &[],
                &["View Channel", "Connect", "Speak"],
            ),
        ];
        for (what, overwrites, bot_roles, missing) in rows {
            assert_eq!(
                missing_to_speak(&guild(&overwrites, bot_roles), "1102"),
                missing,
                "{what}"
            );
        }
        let denied = guild(&json!([speak("1000", 0, false)]), &[]);
        assert_eq!(missing_to_speak(&denied, "1102"), vec!["Speak"]);
        assert!(
            missing_to_speak(&denied, "9999").is_empty(),
            "no channel, no overwrites"
        );
    }

    #[test]
    fn names_are_listed_as_a_sentence_would() {
        type Row = (&'static [&'static str], &'static str);
        let rows: [Row; 4] = [
            (&[], ""),
            (&["Speak"], "Speak"),
            (&["Connect", "Speak"], "Connect and Speak"),
            (
                &["View Channel", "Connect", "Speak"],
                "View Channel, Connect and Speak",
            ),
        ];
        for (names, said) in rows {
            assert_eq!(and_list(names), said, "{names:?}");
        }
    }
}
