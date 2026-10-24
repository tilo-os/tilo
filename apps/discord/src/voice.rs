//! What a call bridge reads of a guild: which Discord voice channel pairs
//! with which Tilo one, who an account is in the room, and what an avatar
//! is stored as. The room's host carries the calls; the record only says
//! where they go.

use ui::tilo_resource::Id;

use crate::record::{Guild, MappedKind};

/// A Discord voice channel and the Tilo voice channel it is mapped to:
/// one call, heard on both sides.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VoicePair {
    /// The Discord channel's snowflake.
    pub discord: String,
    /// The Tilo Chat channel's id.
    pub tilo: String,
}

impl Guild {
    /// Every mapped pair of voice channels the bridge reaches, by Discord
    /// id: a voice channel the latest layout shows, within the one channel
    /// the bridge is confined to, if it is.
    #[must_use]
    pub fn voice_pairs(&self) -> Vec<VoicePair> {
        let mut pairs: Vec<VoicePair> = self
            .map
            .iter()
            .filter(|mapping| mapping.kind == MappedKind::Channel)
            .filter(|mapping| {
                self.only_channel
                    .as_deref()
                    .is_none_or(|only| only == mapping.discord)
            })
            .filter(|mapping| {
                self.channels
                    .find(&Id::from_raw(mapping.discord.clone()))
                    .is_some_and(|channel| channel.kind == discord_wire::op::ChannelKind::Voice)
            })
            .map(|mapping| VoicePair {
                discord: mapping.discord.clone(),
                tilo: mapping.tilo.clone(),
            })
            .collect();
        pairs.sort();
        pairs.dedup();
        pairs
    }

    /// The room member (their root, hex) Discord account `account` is
    /// linked to.
    #[must_use]
    pub fn linked_person(&self, account: &str) -> Option<&str> {
        self.links
            .find(&Id::from_raw(account.to_owned()))
            .map(|link| link.person.as_str())
    }

    /// Whether `account` is kept out of the room.
    #[must_use]
    pub fn blocks(&self, account: &str) -> bool {
        self.blocked_personas.contains(&account.to_owned())
    }

    /// The stored image of the Discord avatar served at `url`, when the
    /// bridge already fetched it: the avatar's hash is the URL's file name.
    #[must_use]
    pub fn stored_avatar(&self, url: &str) -> Option<&str> {
        let path = url.split(['?', '#']).next()?;
        let file = path.rsplit('/').next()?;
        let hash = file.split('.').next()?;
        self.avatars.get(hash).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{DChannel, Link, Mapping, Origin};

    fn guild() -> Guild {
        let mut guild = Guild::default();
        let channel = |id: &str, kind| DChannel {
            id: id.into(),
            kind,
            name: id.into(),
            ..DChannel::default()
        };
        for (id, kind) in [
            ("11", discord_wire::op::ChannelKind::Text),
            ("12", discord_wire::op::ChannelKind::Voice),
            ("13", discord_wire::op::ChannelKind::Voice),
        ] {
            guild.channels.push(channel(id, kind));
        }
        for (tilo, discord) in [("general", "11"), ("lounge", "13"), ("voice", "12")] {
            let key = crate::record::map_key(MappedKind::Channel, tilo);
            guild.map.push(Mapping {
                key,
                kind: MappedKind::Channel,
                tilo: tilo.into(),
                discord: discord.into(),
                origin: Origin::Discord,
                persona: None,
                root: None,
            });
        }
        guild.links.push(Link {
            discord_user: "100".into(),
            person: "ab".repeat(32),
            ..Link::default()
        });
        guild.avatars.insert("a_1f".into(), "cd".repeat(32));
        guild
    }

    #[test]
    fn a_guild_pairs_its_mapped_voice_channels_by_discord_id() {
        let mut guild = guild();
        assert_eq!(
            guild.voice_pairs(),
            [
                VoicePair {
                    discord: "12".into(),
                    tilo: "voice".into()
                },
                VoicePair {
                    discord: "13".into(),
                    tilo: "lounge".into()
                }
            ],
            "text channels are no call"
        );
        guild.only_channel = Some("13".into());
        assert_eq!(
            guild.voice_pairs().len(),
            1,
            "a confined bridge reaches one"
        );
    }

    #[test]
    fn a_guild_knows_who_an_account_is_and_its_stored_avatar() {
        let guild = guild();
        assert_eq!(guild.linked_person("100"), Some("ab".repeat(32).as_str()));
        assert_eq!(guild.linked_person("200"), None);
        assert_eq!(
            guild.stored_avatar("https://cdn.discordapp.com/avatars/100/a_1f.png?size=128"),
            Some("cd".repeat(32).as_str())
        );
        assert_eq!(
            guild.stored_avatar("https://cdn.discordapp.com/avatars/100/ff.png"),
            None
        );
    }
}
