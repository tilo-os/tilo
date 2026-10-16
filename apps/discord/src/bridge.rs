//! The bridge bot's planning (discord-bridge.md §3): what one event, a Chat
//! change or a Discord event, means for both sides.
//!
//! The bot's program only sequences steps, so everything it does comes from
//! two effects. `plan-route` names the Chat message an event is about, for
//! the program to read with Chat's `read-workspace`. `plan-event` answers a
//! [`BridgePlan`]: one list per verb, each element holding exactly that
//! verb's arguments, so the program is a fixed run of loops with no
//! conditions. Work that needs an earlier result (a thread before the reply
//! in it, an avatar before the post shown with it, a category before its
//! channels) is planned on the next pass, after the earlier step's mapping
//! is in the record; [`BridgePlan::again`] asks for that pass.

use discord_wire::event::{
    Command, LayoutChanged, MemberChanged, MessageCreated, MessageDeleted, MessageUpdated,
    ReactionChanged, source,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::inbound::{self, InboundPlan};
use crate::outbound::{self, Context};
use crate::record::{Guild, MappedKind, Ownership, map_key};
use crate::tilo::{self, Workspace};
use crate::{import, layout, roles};
use ui::tilo_resource::Id;

/// The Chat record kind the bridge mirrors.
pub const CHAT_KIND: &str = "chat.workspace";
/// The room record kind whose role changes the bridge mirrors.
pub const ROOM_KIND: &str = "tilo.room";

/// Every list the program loops over, by the verb each runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BridgePlan {
    /// `observe-layout`: Discord's layout changed. Planned alone; the
    /// reconcile runs on the next pass against what it recorded.
    pub observe: Vec<Value>,
    /// `start-thread`, then `map` the thread.
    pub threads: Vec<Value>,
    /// `send`, then `map` the message.
    pub sends: Vec<Value>,
    /// `edit`.
    pub edits: Vec<Value>,
    /// `delete`, then `unmap` the message.
    pub deletes: Vec<Value>,
    /// `react`.
    pub reacts: Vec<Value>,
    /// `unreact`.
    pub unreacts: Vec<Value>,
    /// `apply-channel`, then `map` it as `kind`.
    pub apply_channels: Vec<Value>,
    /// `delete-channel`.
    pub delete_channels: Vec<Value>,
    /// `unmap`.
    pub unmaps: Vec<Value>,
    /// `create-webhook`, then `map` the webhook.
    pub webhooks: Vec<Value>,
    /// `apply-role`, then `map` the role.
    pub apply_roles: Vec<Value>,
    /// `set-overwrite`.
    pub overwrites: Vec<Value>,
    /// `set-member-roles`.
    pub member_roles: Vec<Value>,
    /// `kick`: a linked person was removed from the room.
    pub kicks: Vec<Value>,
    /// `ban`: a linked person was banned from the room.
    pub bans: Vec<Value>,
    /// `unban`.
    pub unbans: Vec<Value>,
    /// `fetch-asset`, then `remember-avatar`.
    pub avatars: Vec<Value>,
    /// Chat `post` with the Discord author's appearance, then `map`.
    pub posts: Vec<Value>,
    /// `fetch-asset`, then Chat `post` with the file, then `map`.
    pub files: Vec<Value>,
    /// Chat `edit-message`.
    pub chat_edits: Vec<Value>,
    /// Chat `delete-message`, then `unmap`.
    pub chat_deletes: Vec<Value>,
    /// Chat `remove-message`, then `unmap`.
    pub chat_removes: Vec<Value>,
    /// Chat `react` with the Discord user's appearance.
    pub chat_reacts: Vec<Value>,
    /// Chat `unreact` with the Discord user's appearance.
    pub chat_unreacts: Vec<Value>,
    /// `block-persona`.
    pub blocks: Vec<Value>,
    /// `complete-link`.
    pub complete_links: Vec<Value>,
    /// `unlink`.
    pub unlinks: Vec<Value>,
    /// Chat `create-category`, then `map` it.
    pub import_categories: Vec<Value>,
    /// Chat `create-channel`, then `map` it.
    pub import_channels: Vec<Value>,
    /// `map` of a Tilo channel or category that already matches.
    pub adopt: Vec<Value>,
    /// `finish-import`, with the Tilo channels left Tilo's own.
    pub finish: Vec<Value>,
    /// Plan again after this pass: something here unblocks more.
    pub again: bool,
}

/// What fired the bot, read from its `event` input.
enum Fired<'e> {
    Chat {
        verbs: Vec<String>,
        item: Option<&'e str>,
        event: &'e Value,
    },
    Room {
        verbs: Vec<String>,
        event: &'e Value,
    },
    /// A change to the bridged guild record itself, such as a Moderator's
    /// `block-persona`.
    Guild {
        verbs: Vec<String>,
        event: &'e Value,
    },
    Discord {
        source: &'e str,
        payload: &'e Value,
    },
    Other,
}

fn fired(event: &Value) -> Fired<'_> {
    if let Some(source) = event.get("source").and_then(Value::as_str) {
        return Fired::Discord {
            source,
            payload: event.get("payload").unwrap_or(&Value::Null),
        };
    }
    let mut verbs = Vec::new();
    if let Some(did) = event.get("did") {
        verbs_of(did, &mut verbs);
    }
    match event.get("kind").and_then(Value::as_str) {
        Some(CHAT_KIND) => Fired::Chat {
            verbs,
            item: event.pointer("/item/id").and_then(Value::as_str),
            event,
        },
        Some(ROOM_KIND) => Fired::Room { verbs, event },
        Some(crate::KIND) => Fired::Guild { verbs, event },
        _ => Fired::Other,
    }
}

/// The verbs a change ran, from its history entry (`did`).
fn verbs_of(did: &Value, out: &mut Vec<String>) {
    match did.get("did").and_then(Value::as_str) {
        Some("verb") => {
            if let Some(verb) = did.pointer("/action/action").and_then(Value::as_str) {
                out.push(verb.to_owned());
            }
        }
        Some("group") => {
            for member in did
                .get("members")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                verbs_of(member, out);
            }
        }
        _ => {}
    }
}

/// The message a Chat change is about: the one its verb names, else the
/// item it addressed.
fn about<'a>(item: Option<&'a str>, event: &'a Value) -> &'a str {
    event
        .pointer("/arguments/message")
        .and_then(Value::as_str)
        .or(item)
        .unwrap_or_default()
}

/// The channel whose latest messages the program reads with `event`: a
/// post names its channel, not the message it made, so the plan finds that
/// message among the channel's newest. Empty for any other event.
#[must_use]
pub fn route_channel(event: &Value) -> String {
    match fired(event) {
        Fired::Chat { verbs, item, event } if verbs.iter().any(|verb| verb == "post") => {
            channel_of(item, event).to_owned()
        }
        _ => String::new(),
    }
}

/// The channel a Chat change names: its `channel` argument, else the item
/// it addressed (a post is made on its channel).
fn channel_of<'a>(item: Option<&'a str>, event: &'a Value) -> &'a str {
    event
        .pointer("/arguments/channel")
        .and_then(Value::as_str)
        .or(item)
        .unwrap_or_default()
}

/// The message a post made: the newest message in its channel by its
/// author with its text that the bridge has not sent yet.
fn posted(guild: &Guild, chat: &Value, item: Option<&str>, event: &Value) -> Option<String> {
    let named = about(item, event);
    if tilo::message(chat, named).is_some() {
        return Some(named.to_owned());
    }
    let channel = channel_of(item, event);
    let text = event.pointer("/arguments/text").and_then(Value::as_str);
    let author = event
        .pointer("/actor/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    tilo::messages(chat)
        .into_iter()
        .rev()
        .find(|message| {
            message.channel == channel
                && message.author == author
                && text.is_none_or(|text| message.text == text)
                && guild.mapping(MappedKind::Message, &message.id).is_none()
        })
        .map(|message| message.id)
}

fn payload<T: serde::de::DeserializeOwned>(payload: &Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}

/// The Chat message `event` is about, for the program to read; empty when
/// it is about none.
#[must_use]
pub fn route(guild: &Guild, event: &Value) -> String {
    match fired(event) {
        Fired::Chat { item, event, .. } => about(item, event).to_owned(),
        // A Tilo message deleted in Discord is read in case it goes back.
        // Without the room every delete counts as one to send again; the
        // plan, which has the room, decides.
        Fired::Discord {
            source: source::MESSAGE_DELETED,
            payload: body,
        } => payload::<MessageDeleted>(body)
            .and_then(|deleted| {
                inbound::plan_deleted(guild, &deleted, &Value::Null)
                    .resends
                    .into_iter()
                    .next()
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn values<T: Serialize>(items: impl IntoIterator<Item = T>) -> Vec<Value> {
    items
        .into_iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .collect()
}

/// What a plan reads: the guild, Chat's `read-workspace` answer for the
/// bridged workspace and the routed message, and where links point.
#[derive(Clone, Copy)]
pub struct Reading<'a> {
    pub guild: &'a Guild,
    pub chat: &'a Value,
    pub context: &'a Context,
    /// The room's members with their roles (`{"members": [{"person",
    /// "role"}]}`), for a Discord-side moderator's standing in Tilo; `null`
    /// when the host binds none, which treats nobody as a moderator.
    pub room: &'a Value,
}

/// Everything `event` means.
#[must_use]
pub fn plan(read: Reading<'_>, event: &Value) -> BridgePlan {
    let mut plan = BridgePlan::default();
    match fired(event) {
        Fired::Chat { verbs, item, event } => {
            for verb in &verbs {
                let message = if verb == "post" {
                    posted(read.guild, read.chat, item, event).unwrap_or_default()
                } else {
                    about(item, event).to_owned()
                };
                tilo_change(&mut plan, read, verb, &message, event);
            }
        }
        Fired::Room { verbs, event } => {
            for verb in &verbs {
                room_change(&mut plan, read.guild, verb, event);
            }
        }
        Fired::Guild { verbs, event } => {
            for verb in &verbs {
                guild_change(&mut plan, read, verb, event);
            }
        }
        Fired::Discord { source, payload } => discord_event(&mut plan, read, source, payload),
        Fired::Other => {}
    }
    plan.again = !(plan.observe.is_empty()
        && plan.threads.is_empty()
        && plan.avatars.is_empty()
        && plan.import_categories.is_empty()
        && plan.import_channels.is_empty()
        && plan.adopt.is_empty()
        && plan.finish.is_empty());
    plan
}

fn tilo_change(plan: &mut BridgePlan, read: Reading<'_>, verb: &str, message: &str, event: &Value) {
    let Reading {
        guild,
        chat,
        context,
        ..
    } = read;
    match verb {
        "post" => post(plan, guild, chat, message, context),
        "edit-message" => {
            plan.edits
                .extend(values(outbound::plan_edit(guild, chat, message, context)));
        }
        "delete-message" | "remove-message" => {
            let channel = event
                .pointer("/before/channel")
                .and_then(Value::as_str)
                .unwrap_or_default();
            plan.deletes
                .extend(values(outbound::plan_delete(guild, channel, message)));
        }
        "react" | "unreact" => {
            let emoji = event
                .pointer("/arguments/emoji")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let (reacts, unreacts) =
                outbound::plan_reaction(guild, chat, message, emoji, verb == "react");
            plan.reacts.extend(values(reacts));
            plan.unreacts.extend(values(unreacts));
        }
        _ => reconcile(plan, read),
    }
}

fn post(plan: &mut BridgePlan, guild: &Guild, chat: &Value, message: &str, context: &Context) {
    let post = outbound::plan_post(guild, chat, message, context);
    plan.threads.extend(values(post.threads));
    plan.sends.extend(values(post.sends));
}

/// Adopting the server, then keeping it bridged: what Discord makes is
/// imported, and the layout and role reconciles do the rest.
fn reconcile(plan: &mut BridgePlan, read: Reading<'_>) {
    let guild = read.guild;
    let workspace = &guild.workspace;
    let tilo = Workspace::from_chat(read.chat, workspace);
    let import = import::plan(guild, workspace, &tilo);
    plan.import_categories
        .extend(import.categories.into_iter().map(|c| {
            json!({
                "workspace": c.workspace,
                "name": c.name,
                "discord": c.discord,
            })
        }));
    plan.import_channels
        .extend(import.channels.into_iter().map(|c| {
            json!({
                "workspace": c.workspace,
                "name": c.name,
                "category": c.category,
                "kind": c.kind,
                "discord": c.discord,
            })
        }));
    plan.adopt.extend(values(import.adopt));
    plan.finish.extend(values(import.finish));
    if guild.ownership == Ownership::Adopted {
        let layout = layout::plan(guild, &tilo);
        plan.apply_channels.extend(values(layout.apply));
        plan.delete_channels.extend(values(layout.delete));
        plan.unmaps.extend(values(layout.unmap));
        plan.webhooks.extend(values(layout.webhooks));
        let roles = roles::plan(guild);
        plan.apply_roles.extend(values(roles.apply));
        plan.overwrites.extend(values(roles.overwrites));
    }
}

fn room_change(plan: &mut BridgePlan, guild: &Guild, verb: &str, event: &Value) {
    // A bridge confined to one channel leaves the server's members alone.
    if guild.only_channel.is_some() {
        return;
    }
    let Some(person) = event.pointer("/arguments/person").and_then(Value::as_str) else {
        return;
    };
    let reason = event
        .pointer("/arguments/reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for account in roles::linked_accounts(guild, person) {
        let member = || json!({ "user": account, "reason": reason });
        match verb {
            // Joining again, a linked person gets their role back in Discord.
            "add-member" | "set-role" => {
                let role = event.pointer("/arguments/role").and_then(Value::as_str);
                plan.member_roles
                    .extend(values(roles::plan_member(guild, &account, role, None)));
            }
            // Leaving the server takes every role with it.
            "remove-member" => plan.kicks.push(member()),
            "ban" => plan.bans.push(member()),
            "unban" => plan.unbans.push(member()),
            _ => {}
        }
    }
}

/// A Moderator blocked a Discord author: what was already mirrored of them
/// is removed from Chat.
fn guild_change(plan: &mut BridgePlan, read: Reading<'_>, verb: &str, event: &Value) {
    if verb != "block-persona" {
        return;
    }
    let Some(user) = event
        .pointer("/arguments/discord_user")
        .and_then(Value::as_str)
    else {
        return;
    };
    inbound_plan(plan, read, inbound::plan_block(read.guild, user));
}

fn discord_event(plan: &mut BridgePlan, read: Reading<'_>, source: &str, body: &Value) {
    let guild = read.guild;
    let inbound = match source {
        source::MESSAGE_CREATED => {
            payload::<MessageCreated>(body).map(|e| inbound::plan_created(guild, &e))
        }
        source::MESSAGE_UPDATED => {
            payload::<MessageUpdated>(body).map(|e| inbound::plan_updated(guild, &e))
        }
        source::MESSAGE_DELETED => {
            payload::<MessageDeleted>(body).map(|e| inbound::plan_deleted(guild, &e, read.room))
        }
        source::REACTION_ADDED => {
            payload::<ReactionChanged>(body).map(|e| inbound::plan_reaction(guild, &e, true))
        }
        source::REACTION_REMOVED => {
            payload::<ReactionChanged>(body).map(|e| inbound::plan_reaction(guild, &e, false))
        }
        source::MEMBER_CHANGED => {
            payload::<MemberChanged>(body).map(|e| inbound::plan_member(guild, &e, read.room))
        }
        source::COMMAND => payload::<Command>(body).map(|e| inbound::plan_command(guild, &e)),
        source::LAYOUT_CHANGED => {
            if let Some(layout) = payload::<LayoutChanged>(body) {
                layout_changed(plan, read, &layout);
            }
            None
        }
        _ => None,
    };
    if let Some(inbound) = inbound {
        inbound_plan(plan, read, inbound);
    }
}

/// Record what Discord reported first; once the record holds it, reconcile.
fn layout_changed(plan: &mut BridgePlan, read: Reading<'_>, layout: &LayoutChanged) {
    let mut observed = read.guild.clone();
    observed.observe(
        layout.bot_user.clone(),
        layout.channels.clone(),
        layout.roles.clone(),
    );
    if observed == *read.guild {
        reconcile(plan, read);
    } else {
        plan.observe.push(json!({
            "bot_user": layout.bot_user,
            "channels": layout.channels,
            "roles": layout.roles,
        }));
    }
}

fn inbound_plan(plan: &mut BridgePlan, read: Reading<'_>, inbound: InboundPlan) {
    plan.avatars.extend(values(inbound.avatars));
    plan.posts.extend(inbound.posts.into_iter().map(|p| {
        json!({
            "channel": p.channel,
            "text": p.text,
            "parent": p.parent,
            "appearance": p.appearance,
            "discord": p.discord,
            "persona": p.persona,
        })
    }));
    plan.files.extend(inbound.files.into_iter().map(|f| {
        json!({
            "fetch": f.fetch,
            "channel": f.post.channel,
            "text": f.post.text,
            "parent": f.post.parent,
            "appearance": f.post.appearance,
            "discord": f.post.discord,
            "persona": f.post.persona,
        })
    }));
    plan.chat_edits.extend(
        inbound
            .edits
            .into_iter()
            .map(|e| json!({ "message": e.message, "text": e.text })),
    );
    plan.chat_deletes.extend(
        inbound
            .deletes
            .into_iter()
            .map(|id| json!({ "message": id })),
    );
    plan.chat_removes.extend(
        inbound
            .removes
            .into_iter()
            .map(|r| json!({ "message": r.message })),
    );
    // Sent again: the Discord copy is gone, so its mapping no longer counts.
    for id in inbound.resends {
        let mut unmapped = read.guild.clone();
        unmapped
            .map
            .take(&Id::from_raw(map_key(MappedKind::Message, &id)));
        post(plan, &unmapped, read.chat, &id, read.context);
    }
    let reaction = |r: inbound::ReactItem| json!({ "message": r.message, "emoji": r.emoji, "appearance": r.appearance });
    plan.chat_reacts
        .extend(inbound.reacts.into_iter().map(reaction));
    plan.chat_unreacts
        .extend(inbound.unreacts.into_iter().map(reaction));
    plan.blocks.extend(
        inbound
            .blocks
            .into_iter()
            .map(|user| json!({ "discord_user": user })),
    );
    plan.member_roles.extend(values(inbound.member_roles));
    plan.complete_links.extend(values(inbound.complete_links));
    plan.unlinks.extend(
        inbound
            .unlinks
            .into_iter()
            .map(|user| json!({ "discord_user": user })),
    );
}

#[cfg(test)]
mod tests;
