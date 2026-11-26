//! Adopting a server (discord-bridge.md §4, "Adopt"): its categories and
//! channels become Tilo's, mapped, and Discord is left exactly as it was.
//! Planned in rounds: categories first, channels once their category is
//! mapped, and `finish-import` once everything is, naming the Tilo channels
//! the server had no match for so they stay Tilo's. A Tilo category with
//! the name, or a Tilo channel with the name, is joined rather than
//! duplicated: a channel in the matching category first, else one in a
//! category of Tilo's own (the channels a workspace opened with), so a workspace's
//! `#general` becomes the server's `#general` whatever category it sits in.
//! A channel joined that way moves into its Discord channel's category, and
//! a category of Tilo's own those moves leave empty is removed, so the workspace
//! is laid out as the server is. Once every Discord channel is joined, the
//! workspace's channels the server had no match for and nothing was written in
//! (the `#chat` every workspace opens with) are removed too, with any category
//! of Tilo's own they leave empty. Once adopted, a channel made in Discord
//! later is imported the same way. The messages it already held come as
//! the worker reads them the first time it meets the server (its newest
//! messages per channel), at the time they were written.

use discord_wire::op::ChannelKind;
use serde::{Deserialize, Serialize};

use crate::guild::Settled;
use crate::guild::{DChannel, Guild, MappedKind, Ownership};
use crate::slug;
use crate::tilo::{Channel, WorkspaceChat};
use std::collections::HashSet;

/// Chat `create-category`, then `map` (category, origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryItem {
    pub name: String,
    pub discord: String,
}

/// Chat `create-channel`, then `map` (channel, origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelItem {
    pub name: String,
    pub category: Option<String>,
    /// Chat's channel kind: `text` or `voice`.
    pub kind: String,
    pub discord: String,
}

/// `map` an existing Tilo object (origin Discord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptItem {
    pub kind: MappedKind,
    pub tilo: String,
    pub discord: String,
}

/// Chat `set-channel-category`: a joined channel moves to its Discord
/// channel's category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileItem {
    pub channel: String,
    pub category: Option<String>,
}

/// One import round.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlan {
    pub categories: Vec<CategoryItem>,
    pub channels: Vec<ChannelItem>,
    pub adopt: Vec<AdoptItem>,
    /// Joined channels moving to their Discord channel's category.
    pub file: Vec<FileItem>,
    /// Chat `delete-category`: categories of Tilo's own this round's moves
    /// or dropped channels leave empty.
    pub drop_categories: Vec<String>,
    /// Chat `delete-channel`: once every Discord channel is joined, the
    /// workspace's channels the server had no match for that hold no message.
    pub drop_channels: Vec<String>,
    /// One element while adopting, once every channel is mapped: run
    /// `finish-import` with the Tilo channels and categories left unmatched.
    pub finish: Vec<FinishItem>,
}

/// `finish-import`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishItem {
    pub tilo_only: Vec<String>,
}

/// The channels of `kinds`, in Discord's order; a bridge confined to one
/// channel sees only that one, outside any category.
fn ordered<'g>(guild: &'g Guild, kinds: &[ChannelKind]) -> Vec<&'g DChannel> {
    let mut channels: Vec<&DChannel> = guild
        .channels
        .iter()
        .filter(|c| kinds.contains(&c.kind))
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
        entry
            .settled
            .as_ref()
            .and_then(Settled::done_result)
            .is_some_and(|result| {
                result.get("result").and_then(serde_json::Value::as_str) == Some("channel")
                    && result.get("channel_id").and_then(serde_json::Value::as_str) == Some(discord)
            })
    })
}

/// A channel the bridge is making in Discord and has no id for yet.
fn making(guild: &Guild) -> bool {
    guild.outbox.entries().any(|(_, entry)| {
        entry.settled.is_none()
            && entry
                .op
                .get("apply-channel")
                .is_some_and(|op| op.get("id").is_none_or(serde_json::Value::is_null))
    })
}

/// The Discord categories not joined yet, each adopting the workspace's
/// category of its name or made anew; `pending` when there was one.
fn plan_categories(
    guild: &Guild,
    workspace: &WorkspaceChat,
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
                name: category.name.clone(),
                discord: category.id.clone(),
            }),
        }
    }
}

/// The categories of Tilo's own whose every channel is moving out in
/// `file`: nothing would be left listed under them.
fn emptied(
    workspace: &WorkspaceChat,
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

/// The workspace's channels no Discord channel joined that hold no message,
/// and the categories of Tilo's own that removing them leaves empty: what
/// adopting the server removes once every Discord channel is joined.
fn unused(guild: &Guild, workspace: &WorkspaceChat) -> (Vec<String>, Vec<String>) {
    let channels: Vec<String> = workspace
        .channels
        .iter()
        .filter(|c| guild.mapping(MappedKind::Channel, &c.id).is_none())
        .filter(|c| !workspace.busy.contains(&c.id))
        .map(|c| c.id.clone())
        .collect();
    let categories = workspace
        .categories
        .iter()
        .filter(|category| guild.mapping(MappedKind::Category, &category.id).is_none())
        .filter(|category| {
            let mut listed = workspace
                .channels
                .iter()
                .filter(|c| c.category.as_ref() == Some(&category.id))
                .peekable();
            listed.peek().is_some() && listed.all(|c| channels.contains(&c.id))
        })
        .map(|category| category.id.clone())
        .collect();
    (channels, categories)
}

/// The import done: the workspace's categories and channels no Discord one
/// joined, and that are not `dropped`, stay Tilo's own.
fn finish(guild: &Guild, workspace: &WorkspaceChat, dropped: &[String]) -> FinishItem {
    let unmatched = |kind, id: &String| guild.mapping(kind, id).is_none() && !dropped.contains(id);
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

/// The channels Discord lists that the workspace has not joined yet, each
/// adopted into the channel it already is there or added: what `pending` says
/// when one waits for the category it is listed under.
fn plan_new_channels(
    guild: &Guild,
    workspace: &WorkspaceChat,
    plan: &mut ImportPlan,
    pending: &mut bool,
    tilo_own: &dyn Fn(&String) -> bool,
) {
    let mut claimed: HashSet<&str> = HashSet::new();
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
                    *pending = true;
                    continue;
                }
            },
            None => None,
        };
        *pending = true;
        let voice = channel.kind == ChannelKind::Voice;
        let free = |c: &&Channel| {
            guild.mapping(MappedKind::Channel, &c.id).is_none()
                && !claimed.contains(c.id.as_str())
                && c.voice == voice
                && slug::channel_slug(&c.name) == slug::channel_slug(&channel.name)
        };
        // The same name in the same category first; else one in a category
        // the server has no counterpart for (such as the channels a workspace
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
                    .find(|c| c.category.as_ref().is_none_or(tilo_own))
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
                name: channel.name.clone(),
                category,
                kind: if voice { "voice" } else { "text" }.into(),
                discord: channel.id.clone(),
            }),
        }
    }
}

/// The next import round for `guild` into the workspace's chat, `workspace`.
#[must_use]
pub fn plan(guild: &Guild, workspace: &WorkspaceChat) -> ImportPlan {
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
    plan_categories(
        guild,
        workspace,
        &discord_categories,
        &mut plan,
        &mut pending,
    );
    plan_new_channels(guild, workspace, &mut plan, &mut pending, &tilo_own);
    plan.drop_categories = emptied(workspace, &plan.file, tilo_own);
    if !pending && guild.ownership == Ownership::Importing {
        let (channels, categories) = unused(guild, workspace);
        let dropped: Vec<String> = channels.iter().chain(&categories).cloned().collect();
        plan.finish.push(finish(guild, workspace, &dropped));
        plan.drop_channels = channels;
        for category in categories {
            if !plan.drop_categories.contains(&category) {
                plan.drop_categories.push(category);
            }
        }
    }
    plan
}
