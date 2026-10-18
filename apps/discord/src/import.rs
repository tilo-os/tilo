//! Adopting a server (discord-bridge.md §4, "Adopt"): its categories and
//! channels become Tilo's, mapped, and Discord is left exactly as it was.
//! Planned in rounds: categories first, channels once their category is
//! mapped, and `finish-import` once everything is, naming the Tilo channels
//! the server had no match for so they stay Tilo's. A Tilo category with
//! the name, or a Tilo channel with the name, is joined rather than
//! duplicated: a channel in the matching category first, else one in a
//! category of Tilo's own (the channels a room opened with), so a room's
//! `#general` becomes the server's `#general` whatever category it sits in.
//! A channel joined that way moves into its Discord channel's category, and
//! a category of Tilo's own those moves leave empty is removed, so the room
//! is laid out as the server is. Once adopted, a channel made in Discord
//! later is imported the same way. Message history is not imported.

use discord_wire::op::{ChannelKind, DiscordOp, DiscordResult};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

use crate::record::{DChannel, Guild, MappedKind, Ownership};
use crate::slug;
use crate::tilo::{Channel, Workspace};
use std::collections::HashSet;
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

/// Chat `set-channel-category`: a joined channel moves to its Discord
/// channel's category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileItem {
    pub channel: String,
    pub category: Option<String>,
}

/// One import round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportPlan {
    pub categories: Vec<CategoryItem>,
    pub channels: Vec<ChannelItem>,
    pub adopt: Vec<AdoptItem>,
    /// Joined channels moving to their Discord channel's category.
    pub file: Vec<FileItem>,
    /// Chat `delete-category`: categories of Tilo's own this round's moves
    /// leave empty.
    pub drop_categories: Vec<String>,
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

/// The Discord categories not joined yet, each adopting the workspace's
/// category of its name or made anew; `pending` when there was one.
fn plan_categories(
    guild: &Guild,
    (workspace_id, workspace): (&str, &Workspace),
    discord_categories: &[&DChannel],
    plan: &mut ImportPlan,
    pending: &mut bool,
) {
    for category in discord_categories {
        if guild
            .by_discord(MappedKind::Category, &category.id)
            .is_some()
        {
            continue;
        }
        *pending = true;
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
}

/// The categories of Tilo's own whose every channel is moving out in
/// `file`: nothing would be left listed under them.
fn emptied(
    workspace: &Workspace,
    file: &[FileItem],
    tilo_own: impl Fn(&String) -> bool,
) -> Vec<String> {
    workspace
        .categories
        .iter()
        .filter(|category| tilo_own(&category.id))
        .filter(|category| {
            let mut channels = workspace
                .channels
                .iter()
                .filter(|c| c.category.as_ref() == Some(&category.id))
                .peekable();
            channels.peek().is_some()
                && channels.all(|c| {
                    file.iter()
                        .any(|f| f.channel == c.id && f.category.as_ref() != Some(&category.id))
                })
        })
        .map(|category| category.id.clone())
        .collect()
}

/// The import done: the workspace's categories and channels no Discord one
/// joined stay Tilo's own.
fn finish(guild: &Guild, workspace: &Workspace) -> FinishItem {
    let unmatched = |kind, id: &String| guild.mapping(kind, id).is_none();
    FinishItem {
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
    }
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
    let discord_categories = ordered(guild, &[ChannelKind::Category]);
    // A Tilo category no Discord category is, or will be, joined with.
    let tilo_own = |category: &String| {
        guild.mapping(MappedKind::Category, category).is_none()
            && workspace
                .categories
                .iter()
                .find(|c| c.id == *category)
                .is_none_or(|tilo| {
                    !discord_categories.iter().any(|d| {
                        guild.by_discord(MappedKind::Category, &d.id).is_none()
                            && d.name.eq_ignore_ascii_case(&tilo.name)
                    })
                })
    };
    let mut claimed: HashSet<&str> = HashSet::new();
    plan_categories(
        guild,
        (workspace_id, workspace),
        &discord_categories,
        &mut plan,
        &mut pending,
    );
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
        let free = |c: &&Channel| {
            guild.mapping(MappedKind::Channel, &c.id).is_none()
                && !claimed.contains(c.id.as_str())
                && c.voice == voice
                && slug::channel_slug(&c.name) == slug::channel_slug(&channel.name)
        };
        // The same name in the same category first; else one in a category
        // the server has no counterpart for (such as the channels a room
        // opened with), which no other Discord channel will claim.
        let existing = workspace
            .channels
            .iter()
            .filter(free)
            .find(|c| c.category == category)
            .or_else(|| {
                workspace
                    .channels
                    .iter()
                    .filter(free)
                    .find(|c| c.category.as_ref().is_none_or(&tilo_own))
            });
        match existing {
            Some(existing) => {
                claimed.insert(&existing.id);
                if existing.category != category {
                    plan.file.push(FileItem {
                        channel: existing.id.clone(),
                        category: category.clone(),
                    });
                }
                plan.adopt.push(AdoptItem {
                    kind: MappedKind::Channel,
                    tilo: existing.id.clone(),
                    discord: channel.id.clone(),
                });
            }
            None => plan.channels.push(ChannelItem {
                workspace: workspace_id.into(),
                name: channel.name.clone(),
                category,
                kind: if voice { "voice" } else { "text" }.into(),
                discord: channel.id.clone(),
            }),
        }
    }
    plan.drop_categories = emptied(workspace, &plan.file, &tilo_own);
    if !pending && guild.ownership == Ownership::Importing {
        plan.finish.push(finish(guild, workspace));
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::{dchan, mapping};
    use crate::record::{Mapping, Origin};

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

    /// The live room: its chat opened with `Channels › #general`, and the
    /// server keeps its `#general` under `chat`.
    #[test]
    fn the_rooms_starting_channel_becomes_the_servers_of_that_name() {
        let mut guild = tilo_server();
        let ws = Workspace {
            categories: vec![crate::tilo::Category {
                id: "k_seeded".into(),
                name: "Channels".into(),
            }],
            channels: vec![Channel {
                id: "c_general".into(),
                name: "general".into(),
                category: Some("k_seeded".into()),
                ..Channel::default()
            }],
        };
        let first = plan(&guild, "w1", &ws);
        assert!(first.adopt.is_empty(), "Channels has no counterpart");
        for c in &first.channels {
            guild.map.push(discord(
                MappedKind::Channel,
                &format!("c_{}", c.name),
                &c.discord,
            ));
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
            "one #general, bridged"
        );
        assert_eq!(
            second.file,
            [FileItem {
                channel: "c_general".into(),
                category: Some("k_chat".into()),
            }],
            "it moves under chat, as in Discord"
        );
        assert_eq!(
            second.drop_categories,
            ["k_seeded"],
            "Channels would list nothing"
        );
        assert_eq!(
            second
                .channels
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["voice"]
        );
        guild
            .map
            .push(discord(MappedKind::Channel, "c_general", "d_general"));
        guild
            .map
            .push(discord(MappedKind::Channel, "c_voice", "d_voice"));
        let filed = Workspace {
            categories: vec![crate::tilo::Category {
                id: "k_chat".into(),
                name: "chat".into(),
            }],
            channels: vec![Channel {
                id: "c_general".into(),
                name: "general".into(),
                category: Some("k_chat".into()),
                ..Channel::default()
            }],
        };
        let last = plan(&guild, "w1", &filed);
        assert!(last.file.is_empty() && last.drop_categories.is_empty());
        assert_eq!(
            last.finish,
            [FinishItem { tilo_only: vec![] }],
            "nothing is left Tilo's own"
        );
    }

    /// A channel whose category the server is joining waits for that
    /// category's own channel of the name, and two of Discord's never
    /// join the same one.
    #[test]
    fn a_channel_is_joined_once_and_not_away_from_a_joined_category() {
        let mut guild = existing_server();
        guild
            .channels
            .push(dchan("d_help", ChannelKind::Text, "help", None, 3));
        guild.channels.push(dchan(
            "d_help_2",
            ChannelKind::Text,
            "help",
            Some("d_chat"),
            0,
        ));
        let ws = seeded();
        let first = plan(&guild, "w1", &ws);
        assert!(
            first.adopt.iter().all(|a| a.kind == MappedKind::Category),
            "Channels is about to be joined, so its #help is not taken: {:?}",
            first.adopt
        );
        assert!(first.channels.iter().any(|c| c.discord == "d_help"));

        let mut ws = ws;
        ws.categories.clear();
        for c in &mut ws.channels {
            c.category = None;
        }
        let mut guild = existing_server();
        guild
            .channels
            .push(dchan("d_help", ChannelKind::Text, "help", None, 3));
        guild
            .channels
            .push(dchan("d_help_2", ChannelKind::Text, "help", None, 4));
        let once = plan(&guild, "w1", &ws);
        let helps: Vec<_> = once
            .adopt
            .iter()
            .filter(|a| a.tilo == "c_help")
            .map(|a| a.discord.as_str())
            .collect();
        assert_eq!(helps, ["d_help"]);
        assert!(once.channels.iter().any(|c| c.discord == "d_help_2"));
    }
}
