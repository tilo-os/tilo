//! The layout reconcile (discord-bridge.md §4). An adopted server keeps
//! what it had: the bridge never renames, moves, recreates or deletes a
//! channel it did not make. What it always does is keep a webhook on every
//! bridged text channel and let go of a pair whose one side was deleted,
//! keeping the other apart. A bridge that mirrors channels also makes, in
//! Discord, the channels and categories made in Tilo after the adoption,
//! and keeps those (and only those) in line with Tilo. Applying a plan and
//! planning again yields nothing, so an interrupted reconcile is simply run
//! again. A channel whose category has no Discord counterpart yet waits for
//! the next run, which the category's creation triggers.

use discord_wire::op::{ChannelKind, ChannelSpec, CreateWebhookOp, DeleteChannelOp, DiscordOp};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, JsonSchema};

use crate::record::{DChannel, Guild, MappedKind, Mapping, Origin, Ownership};
use crate::slug;
use crate::tilo::{Channel, Workspace};

/// Create or correct one Discord channel, then map the result as `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyItem {
    pub op: ChannelSpec,
    pub kind: MappedKind,
}

/// Forget a mapping; with `keep_apart`, neither side is bridged again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnmapItem {
    pub kind: MappedKind,
    pub tilo: String,
    pub keep_apart: bool,
}

/// Everything one reconcile run does, each list one verb.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LayoutPlan {
    /// `apply-channel`, then `map` with the result.
    pub apply: Vec<ApplyItem>,
    /// `delete-channel`.
    pub delete: Vec<DeleteChannelOp>,
    /// `unmap`.
    pub unmap: Vec<UnmapItem>,
    /// `create-webhook`, then `map` as a webhook.
    pub webhooks: Vec<CreateWebhookOp>,
}

impl LayoutPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.apply.is_empty()
            && self.delete.is_empty()
            && self.unmap.is_empty()
            && self.webhooks.is_empty()
    }
}

fn observed<'g>(guild: &'g Guild, id: &str) -> Option<&'g DChannel> {
    guild.channels.find(&Id::from_raw(id))
}

/// The plan for an adopted server, given `workspace`.
#[must_use]
pub fn plan(guild: &Guild, workspace: &Workspace) -> LayoutPlan {
    let mut plan = LayoutPlan::default();
    if guild.ownership != Ownership::Adopted {
        return plan;
    }
    if let Some(only) = &guild.only_channel {
        plan_one(&mut plan, guild, only);
        return plan;
    }
    plan_parted(&mut plan, guild, workspace);
    plan_webhooks(&mut plan, guild);
    if guild.mirror_channels {
        plan_mirror(&mut plan, guild, workspace);
    }
    plan
}

/// A bridge confined to one channel leaves the layout alone and only
/// makes that channel's webhook.
fn plan_one(plan: &mut LayoutPlan, guild: &Guild, only: &str) {
    let Some(mapping) = guild.by_discord(MappedKind::Channel, only) else {
        return;
    };
    let text = observed(guild, only).is_some_and(|c| c.kind.0 == ChannelKind::Text);
    if text && guild.mapping(MappedKind::Webhook, &mapping.tilo).is_none() {
        plan.webhooks.push(CreateWebhookOp {
            channel: only.to_owned(),
            tilo: mapping.tilo.clone(),
        });
    }
}

/// Every bridged text channel Discord shows gets the bridge's webhook.
fn plan_webhooks(plan: &mut LayoutPlan, guild: &Guild) {
    for mapping in guild.map.iter().filter(|m| m.kind == MappedKind::Channel) {
        let text = observed(guild, &mapping.discord).is_some_and(|c| c.kind.0 == ChannelKind::Text);
        if text && guild.mapping(MappedKind::Webhook, &mapping.tilo).is_none() {
            plan.webhooks.push(CreateWebhookOp {
                channel: mapping.discord.clone(),
                tilo: mapping.tilo.clone(),
            });
        }
    }
}

/// Pairs one side of which was deleted: the bridge lets go of them and
/// keeps the side left over to itself. Only a channel the bridge made in
/// Discord goes with its Tilo side.
fn plan_parted(plan: &mut LayoutPlan, guild: &Guild, workspace: &Workspace) {
    for mapping in &guild.map {
        let tilo_gone = match mapping.kind {
            MappedKind::Category => !workspace.categories.iter().any(|c| c.id == mapping.tilo),
            MappedKind::Channel => workspace.channel(&mapping.tilo).is_none(),
            _ => continue,
        };
        let discord_gone = guild.vanished.contains(&mapping.discord);
        if !tilo_gone && !discord_gone {
            continue;
        }
        if tilo_gone
            && !discord_gone
            && ours(mapping)
            && guild.mirror_channels
            && observed(guild, &mapping.discord).is_some()
        {
            plan.delete.push(DeleteChannelOp {
                channel: mapping.discord.clone(),
            });
        }
        plan.unmap.push(UnmapItem {
            kind: mapping.kind,
            tilo: mapping.tilo.clone(),
            keep_apart: true,
        });
        if mapping.kind == MappedKind::Channel
            && guild.mapping(MappedKind::Webhook, &mapping.tilo).is_some()
        {
            plan.unmap.push(UnmapItem {
                kind: MappedKind::Webhook,
                tilo: mapping.tilo.clone(),
                keep_apart: false,
            });
        }
    }
}

/// Made by the bridge in Discord for a Tilo channel or category.
fn ours(mapping: &Mapping) -> bool {
    mapping.origin == Origin::Tilo
}

/// What Tilo made after the adoption is made in Discord, and what the
/// bridge made there follows Tilo's names and categories. Positions are
/// left to Discord: a new channel goes last among its siblings.
fn plan_mirror(plan: &mut LayoutPlan, guild: &Guild, workspace: &Workspace) {
    let mut categories = guild
        .channels
        .iter()
        .filter(|c| c.kind.0 == ChannelKind::Category)
        .count();
    for category in &workspace.categories {
        if guild.tilo_only.contains(&category.id) || queued(guild, &category.id) {
            continue;
        }
        let name = slug::discord_channel_name(ChannelKind::Category, &category.name);
        let want = |id: Option<String>, position: i64| ChannelSpec {
            id,
            tilo: category.id.clone(),
            kind: ChannelKind::Category,
            name: name.clone(),
            topic: String::new(),
            parent: None,
            position,
        };
        match guild.mapping(MappedKind::Category, &category.id) {
            None => {
                plan.apply.push(ApplyItem {
                    op: want(None, position(categories)),
                    kind: MappedKind::Category,
                });
                categories = categories.saturating_add(1);
            }
            Some(mapping) if ours(mapping) => {
                if let Some(current) = observed(guild, &mapping.discord)
                    && current.name != name
                {
                    plan.apply.push(ApplyItem {
                        op: want(Some(current.id.clone()), current.position),
                        kind: MappedKind::Category,
                    });
                }
            }
            Some(_) => {}
        }
    }
    for channel in workspace.channels.iter().filter(|c| !c.archived) {
        if guild.tilo_only.contains(&channel.id) || queued(guild, &channel.id) {
            continue;
        }
        let parent = match &channel.category {
            None => None,
            Some(category) => match guild.mapping(MappedKind::Category, category) {
                Some(m) if observed(guild, &m.discord).is_some() => Some(m.discord.clone()),
                // A category of Tilo's own keeps none of Discord's channels.
                None if guild.tilo_only.contains(category) => None,
                // The category is created first; this channel follows next run.
                _ => continue,
            },
        };
        let want = |id: Option<String>, position: i64| ChannelSpec {
            id,
            tilo: channel.id.clone(),
            kind: kind_of(channel),
            name: slug::discord_channel_name(kind_of(channel), &channel.name),
            topic: if channel.voice {
                String::new()
            } else {
                channel.topic.clone()
            },
            parent: parent.clone(),
            position,
        };
        match guild.mapping(MappedKind::Channel, &channel.id) {
            None => {
                let siblings = guild
                    .channels
                    .iter()
                    .filter(|c| c.kind.0 == kind_of(channel) && c.parent == parent)
                    .count();
                plan.apply.push(ApplyItem {
                    op: want(None, position(siblings)),
                    kind: MappedKind::Channel,
                });
            }
            Some(mapping) if ours(mapping) => {
                if let Some(current) = observed(guild, &mapping.discord) {
                    let wanted = want(Some(current.id.clone()), current.position);
                    if current.name != wanted.name
                        || current.topic != wanted.topic
                        || current.parent != wanted.parent
                    {
                        plan.apply.push(ApplyItem {
                            op: wanted,
                            kind: MappedKind::Channel,
                        });
                    }
                }
            }
            Some(_) => {}
        }
    }
}

/// An `apply-channel` for Tilo's `tilo` not answered yet: planned once.
fn queued(guild: &Guild, tilo: &str) -> bool {
    guild.outbox.entries().any(|(_, entry)| {
        entry.settled.is_none()
            && matches!(&entry.value.op, DiscordOp::ApplyChannel(op) if op.tilo == tilo)
    })
}

fn position(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

const fn kind_of(channel: &Channel) -> ChannelKind {
    if channel.voice {
        ChannelKind::Voice
    } else {
        ChannelKind::Text
    }
}

impl Guild {
    /// A mapped channel Discord has not shown yet: created, its echo not in.
    #[must_use]
    pub fn creating(&self, discord: &str) -> bool {
        observed(self, discord).is_none() && !self.vanished.contains(&discord.to_owned())
    }

    /// A Discord channel some mapping points at.
    #[must_use]
    pub fn knows_mapped(&self, discord: &str) -> bool {
        self.map.iter().any(|m| {
            matches!(
                m.kind,
                MappedKind::Channel | MappedKind::Category | MappedKind::Thread
            ) && m.discord == discord
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
