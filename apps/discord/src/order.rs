//! The order of the bridged categories and channels (discord-bridge.md §4):
//! one fact both sides show, which category each channel is listed under
//! included. Whoever moved something last is followed:
//!
//! - Someone moving a channel or category in Discord rearranges Chat to
//!   Discord's order ([`moved_in_discord`], then [`to_chat`]). So does the
//!   layout the bot gets on (re)connecting, for what moved while it was
//!   away, and finishing the adoption.
//! - Someone rearranging Chat moves the bridged channels in Discord
//!   ([`to_discord`]) when the bridge mirrors channels. Order is a fact of
//!   every bridged channel, not of one: Tilo's order can only show in
//!   Discord when the server's own bridged channels take part, so the
//!   bridge sets their places too. It still never creates, renames or
//!   deletes them, never moves a channel it does not bridge to another
//!   category, and only renumbers those within their own list.
//!
//! Only what both sides know takes part: a channel kept on one side only,
//! one archived in Tilo, or one under a category the other side does not
//! have keeps its place. Discord sorts the categories, and each category's
//! (or the top level's) text channels and voice channels, each by
//! position and then by id, with gaps and repeats allowed; Tilo lists a
//! category's channels in one order, so a Tilo order that puts a voice
//! channel among text channels is the same order as Discord's when each
//! kind is in the same order.
//!
//! An arrangement the bridge asked for reaches Discord as one update per
//! channel, so its layout events show it half done first. Until Discord
//! shows it whole, it is laid over what Discord shows: those events are
//! its echo, never a move in Discord to copy back ([`in_flight`]).

use std::collections::{BTreeMap, BTreeSet};

use discord_wire::op::{ArrangeOp, CategoryPlace, ChannelKind, ChannelPlace, DiscordOp};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, JsonSchema, Keyed};

use crate::record::{DChannel, Guild, MappedKind};
use crate::tilo::WorkspaceChat;

/// Chat `arrange-channels`: the bridged categories, and the bridged
/// channels each under its category, in Discord's order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatArrangement {
    pub categories: Vec<String>,
    pub channels: Vec<ChatPlace>,
}

/// One channel's place in Chat: listed under `category`, none for the top.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatPlace {
    pub channel: String,
    pub category: Option<String>,
}

/// What Discord sorts together among one parent's channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Text,
    Voice,
}

impl Class {
    const fn of(kind: ChannelKind) -> Option<Self> {
        match kind {
            ChannelKind::Text => Some(Self::Text),
            ChannelKind::Voice => Some(Self::Voice),
            ChannelKind::Category | ChannelKind::Thread => None,
        }
    }
}

/// A bridged category (`class: None`) or channel, both sides known.
#[derive(Clone, Debug)]
struct Item {
    tilo: String,
    discord: String,
    class: Option<Class>,
    /// A channel's Tilo category as Discord has it: the Discord category
    /// it is bridged to, none for the top level (or a category of Tilo's
    /// own, which Discord lists at the top).
    parent: Option<String>,
    /// A channel's Tilo category.
    category: Option<String>,
}

/// Where Discord has a channel: under `parent`, at `position`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spot {
    parent: Option<String>,
    position: i64,
}

impl Spot {
    fn of(channel: &DChannel) -> Self {
        Self {
            parent: channel.parent.clone(),
            position: channel.position,
        }
    }
}

/// Discord's layout as the order reads it: `guild`'s observed channels,
/// with the arrangements in flight laid over them.
struct Seen<'g> {
    guild: &'g Guild,
    overlay: BTreeMap<String, Spot>,
}

impl<'g> Seen<'g> {
    /// `layout`'s channels, with what `asked` has in flight laid over them.
    fn new(layout: &'g Guild, asked: &Guild) -> Self {
        let mut overlay = BTreeMap::new();
        for op in in_flight(asked) {
            for place in &op.categories {
                overlay.insert(
                    place.category.clone(),
                    Spot {
                        parent: None,
                        position: place.position,
                    },
                );
            }
            for place in &op.channels {
                overlay.insert(
                    place.channel.clone(),
                    Spot {
                        parent: place.parent.clone(),
                        position: place.position,
                    },
                );
            }
        }
        Self {
            guild: layout,
            overlay,
        }
    }

    fn observed(&self, id: &str) -> Option<&'g DChannel> {
        self.guild.channels.find(&Id::from_raw(id))
    }

    /// Where Discord has `id`, if it shows it or was asked to place it.
    fn spot(&self, id: &str) -> Option<Spot> {
        self.overlay
            .get(id)
            .cloned()
            .or_else(|| self.observed(id).map(Spot::of))
    }

    /// Where Discord has `item`: one being made, not shown yet, goes last
    /// where Tilo has it.
    fn spot_of(&self, item: &Item) -> Spot {
        self.spot(&item.discord).unwrap_or_else(|| Spot {
            parent: item.parent.clone(),
            position: i64::MAX,
        })
    }
}

/// How Discord sorts within one list: by position, then by id (a snowflake,
/// so a longer one is the newer).
fn sort_key(id: &str, position: i64) -> (i64, usize, String) {
    (position, id.len(), id.to_owned())
}

/// The arrangements the bridge asked for that Discord has not shown whole:
/// not answered yet, or done and not all of their places seen. A failed
/// one changed nothing.
#[must_use]
pub fn in_flight(guild: &Guild) -> Vec<&ArrangeOp> {
    guild
        .outbox
        .entries()
        .filter_map(|(_, entry)| match &entry.value.op {
            DiscordOp::ArrangeChannels(op) => match &entry.settled {
                None => Some(op),
                Some(settled) if settled.is_done() && !shown(guild, op, &guild.channels) => {
                    Some(op)
                }
                Some(_) => None,
            },
            _ => None,
        })
        .collect()
}

/// Whether Discord shows every place `op` asked for in `channels`. A
/// channel deleted meanwhile is waited for no longer; one being made is.
fn shown(guild: &Guild, op: &ArrangeOp, channels: &Keyed<DChannel>) -> bool {
    let at = |id: &str, wanted: &Spot| match channels.find(&Id::from_raw(id)) {
        Some(channel) => Spot::of(channel) == *wanted,
        None => !guild.creating(id) || !guild.knows_mapped(id),
    };
    op.categories.iter().all(|place| {
        at(
            &place.category,
            &Spot {
                parent: None,
                position: place.position,
            },
        )
    }) && op.channels.iter().all(|place| {
        at(
            &place.channel,
            &Spot {
                parent: place.parent.clone(),
                position: place.position,
            },
        )
    })
}

/// The outbox entries of arrangements done with: refused or failed, or
/// shown whole in the layout the record holds or in `after`, the one just
/// reported.
#[must_use]
pub fn finished(guild: &Guild, after: &Keyed<DChannel>) -> Vec<Id> {
    guild
        .outbox
        .entries()
        .filter_map(|(id, entry)| match (&entry.value.op, &entry.settled) {
            (DiscordOp::ArrangeChannels(op), Some(settled))
                if !settled.is_done()
                    || shown(guild, op, &guild.channels)
                    || shown(guild, op, after) =>
            {
                Some(id.clone())
            }
            _ => None,
        })
        .collect()
}

/// The bridged categories and channels in Tilo's order, with each
/// channel's category as Discord would have it. A channel under a Tilo
/// category not bridged yet waits for it.
fn shared(guild: &Guild, workspace: &WorkspaceChat) -> Vec<Item> {
    let live = |kind: MappedKind, tilo: &str| {
        guild
            .mapping(kind, tilo)
            .filter(|m| !guild.vanished.contains(&m.discord))
            .map(|m| m.discord.clone())
    };
    let mut items: Vec<Item> = workspace
        .categories
        .iter()
        .filter_map(|category| {
            Some(Item {
                tilo: category.id.clone(),
                discord: live(MappedKind::Category, &category.id)?,
                class: None,
                parent: None,
                category: None,
            })
        })
        .collect();
    for channel in workspace.channels.iter().filter(|c| !c.archived) {
        let Some(discord) = live(MappedKind::Channel, &channel.id) else {
            continue;
        };
        let parent = match &channel.category {
            None => None,
            Some(category) => match live(MappedKind::Category, category) {
                Some(parent) => Some(parent),
                None if guild.tilo_only.contains(category) => None,
                None => continue,
            },
        };
        items.push(Item {
            tilo: channel.id.clone(),
            discord,
            class: Some(if channel.voice {
                Class::Voice
            } else {
                Class::Text
            }),
            parent,
            category: channel.category.clone(),
        });
    }
    items
}

/// Each list Discord sorts channels in: by parent (none for the top) and
/// kind.
type Lists<T> = BTreeMap<(Option<String>, Class), Vec<T>>;

/// Both sides' order of the same items.
#[derive(Debug, Default, PartialEq, Eq)]
struct Order {
    /// Discord ids of the categories.
    categories: Vec<String>,
    /// Discord ids of the channels, by parent and kind.
    lists: Lists<String>,
}

/// The items both sides can show where they are: a channel Discord lists
/// under a category the bridge does not carry is left out.
fn comparable(items: Vec<Item>, seen: &Seen<'_>) -> Vec<Item> {
    let categories: BTreeSet<String> = items
        .iter()
        .filter(|i| i.class.is_none())
        .map(|i| i.discord.clone())
        .collect();
    items
        .into_iter()
        .filter(|item| {
            item.class.is_none()
                || seen
                    .spot_of(item)
                    .parent
                    .is_none_or(|parent| categories.contains(&parent))
        })
        .collect()
}

fn tilo_order(items: &[Item]) -> Order {
    let mut order = Order::default();
    for item in items {
        match item.class {
            None => order.categories.push(item.discord.clone()),
            Some(class) => order
                .lists
                .entry((item.parent.clone(), class))
                .or_default()
                .push(item.discord.clone()),
        }
    }
    order
}

fn discord_order(items: &[Item], seen: &Seen<'_>) -> Order {
    let mut categories: Vec<(Spot, &Item)> = Vec::new();
    let mut lists: Lists<(Spot, &Item)> = BTreeMap::new();
    for item in items {
        let spot = seen.spot_of(item);
        match item.class {
            None => categories.push((spot, item)),
            Some(class) => lists
                .entry((spot.parent.clone(), class))
                .or_default()
                .push((spot, item)),
        }
    }
    let sorted = |mut list: Vec<(Spot, &Item)>| -> Vec<String> {
        list.sort_by_key(|(spot, item)| sort_key(&item.discord, spot.position));
        list.into_iter()
            .map(|(_, item)| item.discord.clone())
            .collect()
    };
    Order {
        categories: sorted(categories),
        lists: lists
            .into_iter()
            .map(|(key, list)| (key, sorted(list)))
            .collect(),
    }
}

/// Whether the bridge carries the order of `guild`'s server at all.
fn ordered(guild: &Guild) -> bool {
    !guild.guild_id.is_empty() && guild.only_channel.is_none()
}

/// Whether Discord's order changed between the layout `before` and the
/// one just reported, `after`: someone moved a bridged channel or
/// category there. The arrangements the bridge has in flight are laid
/// over both, so their echo is no move.
#[must_use]
pub fn moved_in_discord(before: &Guild, after: &Guild, workspace: &WorkspaceChat) -> bool {
    if !ordered(after) {
        return false;
    }
    let items = shared(after, workspace);
    let then = Seen::new(before, before);
    let now = Seen::new(after, before);
    let order = |seen: &Seen<'_>| discord_order(&comparable(items.clone(), seen), seen);
    order(&then) != order(&now)
}

/// Chat arranged as Discord lists the bridged categories and channels, by
/// the layout `layout` with the arrangements `asked` has in flight laid
/// over it; none when Chat lists them so already.
#[must_use]
pub fn to_chat(
    layout: &Guild,
    asked: &Guild,
    workspace: &WorkspaceChat,
) -> Option<ChatArrangement> {
    if !ordered(layout) {
        return None;
    }
    let seen = Seen::new(layout, asked);
    let items = comparable(shared(layout, workspace), &seen);
    let discord = discord_order(&items, &seen);
    if discord == tilo_order(&items) {
        return None;
    }
    let by_discord: BTreeMap<&str, &Item> = items.iter().map(|i| (i.discord.as_str(), i)).collect();
    let tilo = |discord: &str| by_discord.get(discord).map(|item| item.tilo.clone());
    let mut channels = Vec::new();
    let parents = std::iter::once(None).chain(discord.categories.iter().cloned().map(Some));
    for parent in parents {
        for class in [Class::Text, Class::Voice] {
            for id in discord
                .lists
                .get(&(parent.clone(), class))
                .into_iter()
                .flatten()
            {
                let Some(item) = by_discord.get(id.as_str()) else {
                    continue;
                };
                // Under the category it was: a channel Discord lists at the
                // top stays in a category of Tilo's own.
                let category = if item.parent == parent {
                    item.category.clone()
                } else {
                    parent.as_deref().and_then(tilo)
                };
                channels.push(ChatPlace {
                    channel: item.tilo.clone(),
                    category,
                });
            }
        }
    }
    Some(ChatArrangement {
        categories: discord
            .categories
            .iter()
            .filter_map(|id| tilo(id))
            .collect(),
        channels,
    })
}

/// `full`, one list as Discord sorts it, with the bridged entries put in
/// `wanted`'s order in the places bridged entries hold: the others keep
/// theirs. What `wanted` adds goes last.
fn fill(full: Vec<String>, wanted: &[String], bridged: &BTreeSet<String>) -> Vec<String> {
    let mut next = wanted.iter();
    let mut out: Vec<String> = full
        .into_iter()
        .filter_map(|id| {
            if bridged.contains(&id) {
                next.next().cloned()
            } else {
                Some(id)
            }
        })
        .collect();
    out.extend(next.cloned());
    out
}

/// The arrangement that puts Discord in Chat's order; none when Discord
/// shows it already, counting what is in flight. Each list that differs is
/// numbered anew from 0; only what that moves is placed.
#[must_use]
pub fn to_discord(guild: &Guild, workspace: &WorkspaceChat) -> Option<ArrangeOp> {
    if !ordered(guild) || !guild.mirror_channels {
        return None;
    }
    let seen = Seen::new(guild, guild);
    let items = comparable(shared(guild, workspace), &seen);
    let discord = discord_order(&items, &seen);
    let tilo = tilo_order(&items);
    if discord == tilo {
        return None;
    }
    let bridged: BTreeSet<String> = items.iter().map(|i| i.discord.clone()).collect();
    // Every channel Discord shows, and those being made, where it has them.
    let mut everything: Vec<(String, Option<Class>, Spot)> = guild
        .channels
        .iter()
        .filter_map(|c| {
            let class = match c.kind {
                ChannelKind::Category => None,
                kind => Some(Class::of(kind)?),
            };
            Some((c.id.clone(), class, seen.spot(&c.id)?))
        })
        .collect();
    everything.extend(
        items
            .iter()
            .filter(|i| seen.observed(&i.discord).is_none())
            .map(|i| (i.discord.clone(), i.class, seen.spot_of(i))),
    );
    let list = |class: Option<Class>, parent: Option<&String>| -> Vec<String> {
        let mut list: Vec<&(String, Option<Class>, Spot)> = everything
            .iter()
            .filter(|(_, c, spot)| {
                *c == class && (class.is_none() || spot.parent.as_ref() == parent)
            })
            .collect();
        list.sort_by_key(|(id, _, spot)| sort_key(id, spot.position));
        list.into_iter().map(|(id, _, _)| id.clone()).collect()
    };
    let at = |index: usize| i64::try_from(index).unwrap_or(i64::MAX);
    let mut op = ArrangeOp::default();
    if discord.categories != tilo.categories {
        let order = fill(list(None, None), &tilo.categories, &bridged);
        for (index, id) in order.into_iter().enumerate() {
            if seen.spot(&id).is_none_or(|spot| spot.position != at(index)) {
                op.categories.push(CategoryPlace {
                    category: id,
                    position: at(index),
                });
            }
        }
    }
    let keys: BTreeSet<&(Option<String>, Class)> =
        discord.lists.keys().chain(tilo.lists.keys()).collect();
    for key in keys {
        let wanted = tilo.lists.get(key).map_or(&[][..], Vec::as_slice);
        if discord.lists.get(key).map_or(&[][..], Vec::as_slice) == wanted {
            continue;
        }
        let (parent, class) = key;
        let order = fill(list(Some(*class), parent.as_ref()), wanted, &bridged);
        for (index, id) in order.into_iter().enumerate() {
            let place = Spot {
                parent: parent.clone(),
                position: at(index),
            };
            if seen.spot(&id).as_ref() != Some(&place) {
                op.channels.push(ChannelPlace {
                    channel: id,
                    parent: parent.clone(),
                    position: at(index),
                });
            }
        }
    }
    (!op.categories.is_empty() || !op.channels.is_empty()).then_some(op)
}

#[cfg(test)]
mod tests;
