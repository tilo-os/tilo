//! Adopting a server (discord-bridge.md §4, "Adopt"): its categories and
//! channels become Tilo's, mapped, and Discord is left exactly as it was.
//! Planned in rounds: categories first, channels once their category is
//! mapped, and `finish-import` once everything is, naming the Tilo channels
//! the server had no match for so they stay Tilo's. A Tilo category with
//! the name, or a Tilo channel with the name in the matching category, is
//! joined rather than duplicated. Once adopted, a channel made in Discord
//! later is imported the same way. Message history is not imported.

use discord_wire::op::{ChannelKind, DiscordOp, DiscordResult};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

use crate::record::{DChannel, Guild, MappedKind, Ownership};
use crate::slug;
use crate::tilo::Workspace;
use ui::tilo_resource::Settled;

/// Chat `create-category`, then `map` (category, origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CategoryItem {
    pub workspace: String,
    pub name: String,
    pub discord: String,
}

/// Chat `create-channel`, then `map` (channel, origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelItem {
    pub workspace: String,
    pub name: String,
    pub category: Option<String>,
    /// Chat's channel kind: `text` or `voice`.
    pub kind: String,
    pub discord: String,
}

/// `map` an existing Tilo object (origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdoptItem {
    pub kind: MappedKind,
    pub tilo: String,
    pub discord: String,
}

/// One import round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportPlan {
    pub categories: Vec<CategoryItem>,
    pub channels: Vec<ChannelItem>,
    pub adopt: Vec<AdoptItem>,
    /// One element while adopting, once every channel is mapped: run
    /// `finish-import` with the Tilo channels and categories left unmatched.
    pub finish: Vec<FinishItem>,
}

/// `finish-import`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FinishItem {
    pub tilo_only: Vec<String>,
}

/// The channels of `kinds`, in Discord's order; a bridge confined to one
/// channel sees only that one, outside any category.
fn ordered<'g>(guild: &'g Guild, kinds: &[ChannelKind]) -> Vec<&'g DChannel> {
    let mut channels: Vec<&DChannel> = guild
        .channels
        .iter()
        .filter(|c| kinds.contains(&c.kind.0))
        .filter(|c| guild.only_channel.as_ref().is_none_or(|only| *only == c.id))
        .filter(|c| !guild.discord_only.contains(&c.id) && !made_by_bridge(guild, &c.id))
        .collect();
    channels.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    channels
}

/// A Discord channel the bridge made for a Tilo one, its mapping maybe not
/// in yet: never imported back.
fn made_by_bridge(guild: &Guild, discord: &str) -> bool {
    guild.outbox.entries().any(|(_, entry)| {
        matches!(
            &entry.settled,
            Some(Settled::Done(DiscordResult::Channel { channel_id, .. })) if channel_id == discord
        )
    })
}

/// A channel the bridge is making in Discord and has no id for yet.
fn making(guild: &Guild) -> bool {
    guild.outbox.entries().any(|(_, entry)| {
        entry.settled.is_none()
            && matches!(&entry.value.op, DiscordOp::ApplyChannel(op) if op.id.is_none())
    })
}

/// The next import round for `guild` into `workspace` (id `workspace_id`).
#[must_use]
pub fn plan(guild: &Guild, workspace_id: &str, workspace: &Workspace) -> ImportPlan {
    let mut plan = ImportPlan::default();
    // Discord's echo of a channel the bridge is making could look like one
    // made there; the next layout event, after it is mapped, tells.
    if guild.guild_id.is_empty() || making(guild) {
        return plan;
    }
    let mut pending = false;
    for category in ordered(guild, &[ChannelKind::Category]) {
        if guild
            .by_discord(MappedKind::Category, &category.id)
            .is_some()
        {
            continue;
        }
        pending = true;
        match workspace.categories.iter().find(|c| {
            guild.mapping(MappedKind::Category, &c.id).is_none()
                && c.name.eq_ignore_ascii_case(&category.name)
        }) {
            Some(existing) => plan.adopt.push(AdoptItem {
                kind: MappedKind::Category,
                tilo: existing.id.clone(),
                discord: category.id.clone(),
            }),
            None => plan.categories.push(CategoryItem {
                workspace: workspace_id.into(),
                name: category.name.clone(),
                discord: category.id.clone(),
            }),
        }
    }
    // Text channels before voice, as Discord lists them.
    for channel in ordered(guild, &[ChannelKind::Text])
        .into_iter()
        .chain(ordered(guild, &[ChannelKind::Voice]))
    {
        if guild.by_discord(MappedKind::Channel, &channel.id).is_some() {
            continue;
        }
        let category = match channel
            .parent
            .as_ref()
            .filter(|_| guild.only_channel.is_none())
        {
            Some(parent) => match guild.by_discord(MappedKind::Category, parent) {
                Some(mapping) => Some(mapping.tilo.clone()),
                // A category kept in Discord only takes its channels along.
                None if guild.discord_only.contains(parent) => continue,
                // Its category is created this round; the channel follows next.
                None => {
                    pending = true;
                    continue;
                }
            },
            None => None,
        };
        pending = true;
        let voice = channel.kind.0 == ChannelKind::Voice;
        let existing = workspace.channels.iter().find(|c| {
            guild.mapping(MappedKind::Channel, &c.id).is_none()
                && c.voice == voice
                && c.category == category
                && slug::channel_slug(&c.name) == slug::channel_slug(&channel.name)
        });
        match existing {
            Some(existing) => plan.adopt.push(AdoptItem {
                kind: MappedKind::Channel,
                tilo: existing.id.clone(),
                discord: channel.id.clone(),
            }),
            None => plan.channels.push(ChannelItem {
                workspace: workspace_id.into(),
                name: channel.name.clone(),
                category,
                kind: if voice { "voice" } else { "text" }.into(),
                discord: channel.id.clone(),
            }),
        }
    }
    if !pending && guild.ownership == Ownership::Importing {
        let unmatched = |kind, id: &String| guild.mapping(kind, id).is_none();
        plan.finish.push(FinishItem {
            tilo_only: workspace
                .categories
                .iter()
                .map(|c| &c.id)
                .filter(|id| unmatched(MappedKind::Category, id))
                .chain(
                    workspace
                        .channels
                        .iter()
                        .map(|c| &c.id)
                        .filter(|id| unmatched(MappedKind::Channel, id)),
                )
                .cloned()
                .collect(),
        });
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::{dchan, mapping};
    use crate::record::{Mapping, Origin};
    use crate::tilo::Channel;

    /// The Tilo Discord server as it was when the bridge was added.
    fn tilo_server() -> Guild {
        Guild {
            guild_id: "1".into(),
            ownership: Ownership::Importing,
            channels: [
                dchan("d_goal", ChannelKind::Text, "goal", None, 0),
                dchan("d_roadmap", ChannelKind::Text, "roadmap", None, 1),
                dchan("d_progress", ChannelKind::Text, "progress", None, 2),
                dchan("d_chat", ChannelKind::Category, "chat", None, 0),
                dchan("d_voice", ChannelKind::Voice, "voice", Some("d_chat"), 0),
                dchan("d_general", ChannelKind::Text, "general", Some("d_chat"), 1),
            ]
            .into_iter()
            .collect(),
            ..Guild::default()
        }
    }

    fn discord(kind: MappedKind, tilo: &str, discord: &str) -> Mapping {
        Mapping {
            origin: Origin::Discord,
            ..mapping(kind, tilo, discord)
        }
    }

    #[test]
    fn the_tilo_server_imports_in_two_rounds_then_finishes() {
        let mut guild = tilo_server();
        let empty = Workspace::default();
        let first = plan(&guild, "w1", &empty);
        assert_eq!(
            first.categories,
            [CategoryItem {
                workspace: "w1".into(),
                name: "chat".into(),
                discord: "d_chat".into()
            }]
        );
        let names: Vec<_> = first
            .channels
            .iter()
            .map(|c| (c.name.as_str(), c.category.as_deref()))
            .collect();
        assert_eq!(
            names,
            [("goal", None), ("roadmap", None), ("progress", None)],
            "channels in chat wait for it"
        );
        assert!(first.finish.is_empty());

        guild
            .map
            .push(discord(MappedKind::Category, "k_chat", "d_chat"));
        for (tilo, d) in [
            ("c_goal", "d_goal"),
            ("c_roadmap", "d_roadmap"),
            ("c_progress", "d_progress"),
        ] {
            guild.map.push(discord(MappedKind::Channel, tilo, d));
        }
        let second = plan(&guild, "w1", &empty);
        let rest: Vec<_> = second
            .channels
            .iter()
            .map(|c| (c.name.as_str(), c.category.as_deref(), c.kind.as_str()))
            .collect();
        assert_eq!(
            rest,
            [
                ("general", Some("k_chat"), "text"),
                ("voice", Some("k_chat"), "voice")
            ]
        );

        guild
            .map
            .push(discord(MappedKind::Channel, "c_general", "d_general"));
        guild
            .map
            .push(discord(MappedKind::Channel, "c_voice", "d_voice"));
        let last = plan(&guild, "w1", &empty);
        assert_eq!(last.finish.len(), 1);
        assert!(last.categories.is_empty() && last.channels.is_empty());
    }

    #[test]
    fn existing_tilo_channels_are_adopted() {
        let ws = Workspace {
            categories: vec![],
            channels: vec![Channel {
                id: "c_seed".into(),
                name: "Goal".into(),
                ..Channel::default()
            }],
        };
        let p = plan(&tilo_server(), "w1", &ws);
        assert_eq!(
            p.adopt,
            [AdoptItem {
                kind: MappedKind::Channel,
                tilo: "c_seed".into(),
                discord: "d_goal".into()
            }]
        );
        assert!(p.channels.iter().all(|c| c.name != "goal"));
    }

    #[test]
    fn once_adopted_only_what_discord_makes_later_is_imported() {
        let mut guild = tilo_server();
        guild.ownership = Ownership::Adopted;
        for (kind, tilo, d) in [
            (MappedKind::Category, "k_chat", "d_chat"),
            (MappedKind::Channel, "c_goal", "d_goal"),
            (MappedKind::Channel, "c_roadmap", "d_roadmap"),
            (MappedKind::Channel, "c_progress", "d_progress"),
            (MappedKind::Channel, "c_voice", "d_voice"),
            (MappedKind::Channel, "c_general", "d_general"),
        ] {
            guild.map.push(discord(kind, tilo, d));
        }
        assert_eq!(
            plan(&guild, "w1", &Workspace::default()),
            ImportPlan::default(),
            "no second finish"
        );
        guild
            .channels
            .push(dchan("d_new", ChannelKind::Text, "new", Some("d_chat"), 2));
        guild
            .channels
            .push(dchan("d_gone", ChannelKind::Text, "gone", None, 3));
        guild.discord_only.insert("d_gone".into());
        let later = plan(&guild, "w1", &Workspace::default());
        assert_eq!(
            later
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.category.as_deref()))
                .collect::<Vec<_>>(),
            [("new", Some("k_chat"))],
            "a channel kept in Discord only is not imported again"
        );
        assert!(later.finish.is_empty());
    }

    /// The real server when its bridge first ran, into a Chat its room had
    /// already given channels of its own, one category's name alike.
    fn existing_server() -> Guild {
        Guild {
            guild_id: "1".into(),
            ownership: Ownership::Importing,
            channels: [
                dchan("d_channels", ChannelKind::Category, "Channels", None, 0),
                dchan(
                    "d_general",
                    ChannelKind::Text,
                    "general",
                    Some("d_channels"),
                    0,
                ),
                dchan(
                    "d_voice",
                    ChannelKind::Voice,
                    "voice",
                    Some("d_channels"),
                    0,
                ),
                dchan("d_goal", ChannelKind::Text, "goal", None, 0),
                dchan("d_roadmap", ChannelKind::Text, "roadmap", None, 1),
                dchan("d_progress", ChannelKind::Text, "progress", None, 2),
                dchan("d_chat", ChannelKind::Category, "chat", None, 1),
            ]
            .into_iter()
            .collect(),
            ..Guild::default()
        }
    }

    fn seeded() -> Workspace {
        let channel = |id: &str, name: &str| Channel {
            id: id.into(),
            name: name.into(),
            category: Some("k_seeded".into()),
            ..Channel::default()
        };
        Workspace {
            categories: vec![crate::tilo::Category {
                id: "k_seeded".into(),
                name: "Channels".into(),
            }],
            channels: vec![
                channel("c_general", "general"),
                channel("c_help", "help"),
                channel("c_show", "show-and-tell"),
            ],
        }
    }

    #[test]
    fn an_existing_server_is_adopted_by_name_within_its_categories() {
        let mut guild = existing_server();
        let ws = seeded();
        let first = plan(&guild, "w1", &ws);
        assert_eq!(
            first.adopt,
            [AdoptItem {
                kind: MappedKind::Category,
                tilo: "k_seeded".into(),
                discord: "d_channels".into()
            }]
        );
        assert_eq!(
            first
                .categories
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["chat"]
        );
        for item in first.adopt {
            guild
                .map
                .push(discord(item.kind, &item.tilo, &item.discord));
        }
        guild
            .map
            .push(discord(MappedKind::Category, "k_chat", "d_chat"));

        let second = plan(&guild, "w1", &ws);
        assert_eq!(
            second.adopt,
            [AdoptItem {
                kind: MappedKind::Channel,
                tilo: "c_general".into(),
                discord: "d_general".into()
            }],
            "#general is joined with the one Tilo had, not doubled"
        );
        assert_eq!(
            second
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.category.as_deref(), c.kind.as_str()))
                .collect::<Vec<_>>(),
            [
                ("goal", None, "text"),
                ("roadmap", None, "text"),
                ("progress", None, "text"),
                ("voice", Some("k_seeded"), "voice"),
            ]
        );
        for item in second.adopt {
            guild
                .map
                .push(discord(item.kind, &item.tilo, &item.discord));
        }
        for c in second.channels {
            guild.map.push(discord(
                MappedKind::Channel,
                &format!("c_{}", c.name),
                &c.discord,
            ));
        }

        let last = plan(&guild, "w1", &ws);
        assert_eq!(
            last.finish,
            [FinishItem {
                tilo_only: vec!["c_help".into(), "c_show".into()]
            }],
            "Tilo's own #help and #show-and-tell stay Tilo's"
        );
    }
}
