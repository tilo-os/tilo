//! The bridge bot's planning (discord-bridge.md §3): what one event, a Chat
//! change or a Discord event, means for both sides.
//!
//! The bot's program only sequences steps, so everything it does comes from
//! two effects. `plan-route` names the Chat message an event is about, for
//! the program to read with Chat's `read-room`. `plan-event` answers a
//! [`BridgePlan`]: one list per verb, each element holding exactly that
//! verb's arguments, so the program is a fixed run of loops with no
//! conditions. Work that needs an earlier result (a thread before the reply
//! in it, an avatar before the post shown with it, a category before its
//! channels) is planned on the next pass, after the earlier step's mapping
//! is in the record; [`BridgePlan::again`] asks for that pass.

use discord_wire::event::{
    Authorized, Command, LayoutChanged, MemberChanged, MessageCreated, MessageDeleted,
    MessageUpdated, ReactionChanged, SignInOffered, source,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::inbound::{self, InboundPlan};
use crate::outbound::{self, Context};
use crate::record::{Guild, MappedKind, Ownership};
use crate::tilo::{self, RoomChat};
use crate::{import, layout, roles};

/// The Chat record kind the bridge mirrors.
pub const CHAT_KIND: &str = "chat.workspace";
/// The kind of the parts Chat keeps its messages in, beside its record.
pub const MESSAGES_KIND: &str = "chat.messages";
/// The workspace's own emoji, which the bridge uploads to the server.
pub const EMOJI_KIND: &str = "tilo.workspace.emoji";
/// The room record kind whose role changes the bridge mirrors.
pub const ROOM_KIND: &str = "tilo.room";
/// Chat's verb filing a feedback report in a channel.
pub const SUBMIT_FEEDBACK: &str = "submit-feedback";

/// The message a filed feedback report shows as in its channel: Chat reads
/// report `report` out as the message `m_feedback_<report>`, under the
/// person who filed it, with its masked picture as the image.
#[must_use]
pub fn report_message(report: &str) -> String {
    format!("m_feedback_{report}")
}

/// The report a `submit-feedback` change filed, as its message.
fn filed_report(event: &Value) -> Option<String> {
    event
        .pointer("/arguments/submission/id")
        .and_then(Value::as_str)
        .filter(|report| !report.is_empty())
        .map(report_message)
}

/// Every list the program loops over, by the verb each runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BridgePlan {
    /// `observe-layout`: Discord's layout changed. Planned alone; the
    /// reconcile runs on the next pass against what it recorded.
    pub observe: Vec<Value>,
    /// `start-thread`, then `map` the thread.
    pub threads: Vec<Value>,
    pub sends: Vec<Value>,
    pub edits: Vec<Value>,
    /// `delete`, then `unmap` the message.
    pub deletes: Vec<Value>,
    /// `react` / `unreact`: the bot's own reaction in Discord.
    pub reacts: Vec<Value>,
    pub unreacts: Vec<Value>,
    /// `apply-channel`, then `map` it as `kind`.
    pub apply_channels: Vec<Value>,
    pub delete_channels: Vec<Value>,
    pub unmaps: Vec<Value>,
    pub webhooks: Vec<Value>,
    /// `apply-role`, then `map` the role.
    pub apply_roles: Vec<Value>,
    /// `set-overwrite`.
    pub overwrites: Vec<Value>,
    /// `delete-emoji`, then `settle-emoji`: an emoji the bridge uploaded
    /// whose workspace emoji is gone or has another picture.
    pub delete_emojis: Vec<Value>,
    /// `apply-emoji`, then `settle-emoji`: a workspace emoji uploaded to
    /// the server, or the one uploaded for it renamed.
    pub apply_emojis: Vec<Value>,
    pub member_roles: Vec<Value>,
    /// `kick`: a linked person was removed from the room.
    pub kicks: Vec<Value>,
    /// `ban`: a linked person was banned from the room.
    pub bans: Vec<Value>,
    pub unbans: Vec<Value>,
    /// `fetch-asset`, then `remember-avatar`.
    pub avatars: Vec<Value>,
    /// Chat `post` with the Discord author's appearance, then `map`.
    pub posts: Vec<Value>,
    /// `fetch-asset`, then Chat `post` with the file, then `map`.
    pub files: Vec<Value>,
    pub chat_edits: Vec<Value>,
    pub chat_deletes: Vec<Value>,
    /// Chat `remove-message`, then `unmap`.
    pub chat_removes: Vec<Value>,
    /// The `react` field edit of a Chat message's reactions, with the
    /// Discord user's appearance.
    pub chat_reacts: Vec<Value>,
    /// The `unreact` field edit, likewise.
    pub chat_unreacts: Vec<Value>,
    /// `block-persona`.
    pub blocks: Vec<Value>,
    /// `offer-link`.
    pub offers: Vec<Value>,
    /// `offer-sign-in`.
    pub sign_in_offers: Vec<Value>,
    /// `answer-interaction`: a slash command's reply says what came of it.
    pub answers: Vec<Value>,
    /// `set-sign-in`: the host's offer of signing in with Discord changed.
    pub sign_ins: Vec<Value>,
    pub unlinks: Vec<Value>,
    /// `rename-link`.
    pub renames: Vec<Value>,
    pub import_categories: Vec<Value>,
    pub import_channels: Vec<Value>,
    /// `map` of a Tilo channel or category that already matches.
    pub adopt: Vec<Value>,
    /// Chat `set-channel-category`: a joined channel moves to its Discord
    /// channel's category.
    pub file_channels: Vec<Value>,
    /// Chat `delete-channel`: a channel of the room's the server had no
    /// match for and nothing was written in.
    pub drop_channels: Vec<Value>,
    /// Chat `delete-category`: a category of Tilo's own the moves or dropped
    /// channels left empty.
    pub drop_categories: Vec<Value>,
    /// `finish-import`, with the Tilo channels left Tilo's own.
    pub finish: Vec<Value>,
    /// Plan again after this pass: something here unblocks more.
    pub again: bool,
}

/// A reaction someone added to or took back from a Chat message: the
/// `react` or `unreact` field edit of its reactions.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reacted {
    message: String,
    emoji: String,
    added: bool,
}

/// What a change did, from its history entry (`did`): the verbs it ran and
/// the reactions it changed.
#[derive(Default)]
struct Did {
    verbs: Vec<String>,
    reactions: Vec<Reacted>,
}

/// What fired the bot, read from its `event` input.
enum Fired<'e> {
    Chat {
        verbs: Vec<String>,
        reactions: Vec<Reacted>,
        item: Option<&'e str>,
        event: &'e Value,
    },
    Room {
        verbs: Vec<String>,
        event: &'e Value,
    },
    /// The workspace's emoji changed.
    Emoji,
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
    let mut did = Did::default();
    if let Some(entry) = event.get("did") {
        did_of(entry, &mut did);
    }
    let Did { verbs, reactions } = did;
    match event.get("kind").and_then(Value::as_str) {
        Some(CHAT_KIND | MESSAGES_KIND) => Fired::Chat {
            verbs,
            reactions,
            item: event.pointer("/item/id").and_then(Value::as_str),
            event,
        },
        Some(ROOM_KIND) => Fired::Room { verbs, event },
        Some(EMOJI_KIND) => Fired::Emoji,
        Some(crate::KIND) => Fired::Guild { verbs, event },
        _ => Fired::Other,
    }
}

/// What a change did, from its history entry (`did`).
fn did_of(did: &Value, out: &mut Did) {
    match did.get("did").and_then(Value::as_str) {
        Some("verb") => {
            if let Some(verb) = did.pointer("/action/action").and_then(Value::as_str) {
                out.verbs.push(verb.to_owned());
            }
        }
        Some("edit") => out.reactions.extend(reacted(did)),
        Some("group") => {
            for member in did
                .get("members")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                did_of(member, out);
            }
        }
        _ => {}
    }
}

/// A field edit's entry, when it is a reaction to a message: `react` or
/// `unreact` on `messages › <message> › reactions`, the emoji its `new`.
fn reacted(edit: &Value) -> Option<Reacted> {
    let added = match edit.get("op").and_then(Value::as_str)? {
        "react" => true,
        "unreact" => false,
        _ => return None,
    };
    let path = edit.get("path")?.as_array()?;
    let step = |at: usize, kind: &str| path.get(at)?.get(kind)?.as_str();
    if path.len() != 3 || step(2, "field")? != "reactions" {
        return None;
    }
    // A filed report takes reactions in the chat record, on its message.
    let message = match step(0, "field")? {
        "messages" => step(1, "item")?.to_owned(),
        "feedback" => report_message(step(1, "item")?),
        _ => return None,
    };
    Some(Reacted {
        message,
        emoji: edit.get("new")?.as_str()?.to_owned(),
        added,
    })
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

/// The Chat verbs that make a message: a post, and sharing a resource or a
/// live window, which post the share.
const POSTS: &[&str] = &["post", "share-resource", "share-window"];

/// The Chat verbs that change how a share reads without editing its words:
/// its sharer stopping it, or closing or reopening the window it shows.
const SHARE_CHANGES: &[&str] = &["stop-sharing", "share-open"];

fn posts(verb: &str) -> bool {
    POSTS.contains(&verb)
}

/// The channel whose latest messages the program reads with `event`: a
/// post names its channel, not the message it made, so the plan finds that
/// message among the channel's newest. Empty for any other event.
#[must_use]
pub fn route_channel(event: &Value) -> String {
    match fired(event) {
        Fired::Chat {
            verbs, item, event, ..
        } if verbs.iter().any(|verb| posts(verb)) => channel_of(item, event).to_owned(),
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
    // A reply in a thread is made on the message it answers, which its
    // change names: that one is not the post.
    let answers = event.pointer("/arguments/parent").and_then(Value::as_str);
    if answers != Some(named) && tilo::message(chat, named).is_some() {
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
        Fired::Chat {
            verbs,
            reactions,
            item,
            event,
        } => reactions.first().map_or_else(
            || {
                verbs
                    .iter()
                    .any(|verb| verb == SUBMIT_FEEDBACK)
                    .then(|| filed_report(event))
                    .flatten()
                    .unwrap_or_else(|| about(item, event).to_owned())
            },
            |reacted| reacted.message.clone(),
        ),
        // A reaction is read with the message it is on, for the spelling
        // of the emoji its reactions already use.
        Fired::Discord {
            source: source::REACTION_ADDED | source::REACTION_REMOVED,
            payload: body,
        } => payload::<ReactionChanged>(body)
            .and_then(|event| guild.by_discord(MappedKind::Message, &event.message_id))
            .map(|mapped| mapped.tilo.clone())
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

/// What a plan reads: the guild, Chat's `read-room` answer for the room
/// and the routed message, and where links point.
#[derive(Clone, Copy)]
pub struct Reading<'a> {
    pub guild: &'a Guild,
    pub chat: &'a Value,
    pub context: &'a Context,
    /// The room's members with their roles (`{"members": [{"person",
    /// "role"}]}`), for a Discord-side moderator's standing in Tilo; `null`
    /// when the host binds none, which treats nobody as a moderator.
    pub room: &'a Value,
    /// The workspace's emoji as the host hands them (`{"emoji": [{"name",
    /// "picture"}]}`), or `None` when it binds none, which leaves the
    /// server's emoji alone.
    pub emoji: Option<&'a Value>,
}

/// Everything `event` means.
#[must_use]
pub fn plan(read: Reading<'_>, event: &Value) -> BridgePlan {
    let mut plan = BridgePlan::default();
    match fired(event) {
        Fired::Chat {
            verbs,
            reactions,
            item,
            event,
        } => {
            for verb in &verbs {
                let message = if posts(verb) {
                    posted(read.guild, read.chat, item, event).unwrap_or_default()
                } else if verb == SUBMIT_FEEDBACK {
                    filed_report(event).unwrap_or_default()
                } else {
                    about(item, event).to_owned()
                };
                tilo_change(&mut plan, read, verb, &message, event);
            }
            for reacted in &reactions {
                let (reacts, unreacts) = outbound::plan_reaction(
                    read.guild,
                    read.chat,
                    &reacted.message,
                    &reacted.emoji,
                    reacted.added,
                );
                plan.reacts.extend(values(reacts));
                plan.unreacts.extend(values(unreacts));
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
        Fired::Emoji => emoji(&mut plan, read),
        Fired::Discord { source, payload } => discord_event(&mut plan, read, source, payload),
        Fired::Other => {}
    }
    plan.again = !(plan.observe.is_empty()
        && plan.threads.is_empty()
        && plan.avatars.is_empty()
        && plan.import_categories.is_empty()
        && plan.import_channels.is_empty()
        && plan.adopt.is_empty()
        && plan.file_channels.is_empty()
        && plan.drop_channels.is_empty()
        && plan.drop_categories.is_empty()
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
        // A filed report goes across as the message Chat reads it out as.
        verb if posts(verb) || verb == SUBMIT_FEEDBACK => post(plan, guild, chat, message, context),
        verb if verb == "edit-message" || SHARE_CHANGES.contains(&verb) => {
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
    let tilo = RoomChat::from_chat(read.chat);
    let import = import::plan(guild, &tilo);
    plan.import_categories
        .extend(import.categories.into_iter().map(|c| {
            json!({
                "name": c.name,
                "discord": c.discord,
            })
        }));
    plan.import_channels
        .extend(import.channels.into_iter().map(|c| {
            json!({
                "name": c.name,
                "category": c.category,
                "kind": c.kind,
                "discord": c.discord,
            })
        }));
    plan.adopt.extend(values(import.adopt));
    plan.file_channels.extend(values(import.file));
    plan.drop_channels.extend(
        import
            .drop_channels
            .into_iter()
            .map(|channel| json!({ "channel": channel })),
    );
    plan.drop_categories.extend(
        import
            .drop_categories
            .into_iter()
            .map(|category| json!({ "category": category })),
    );
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

/// Bring the server's emoji in line with the workspace's.
fn emoji(plan: &mut BridgePlan, read: Reading<'_>) {
    let Some(wanted) = crate::emoji_sync::wanted(read.emoji) else {
        return;
    };
    let emoji = crate::emoji_sync::plan(read.guild, &wanted);
    plan.delete_emojis.extend(values(emoji.deletes));
    plan.apply_emojis.extend(values(emoji.applies));
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
    if verb == "complete-link" {
        // A person took a link offer: the `/link` it answered says so.
        if let Some(person) = event.pointer("/actor/id").and_then(Value::as_str) {
            inbound_plan(plan, inbound::plan_linked(read.guild, read.chat, person));
        }
        return;
    }
    if verb != "block-persona" {
        return;
    }
    let Some(user) = event
        .pointer("/arguments/discord_user")
        .and_then(Value::as_str)
    else {
        return;
    };
    inbound_plan(plan, inbound::plan_block(read.guild, user));
}

fn discord_event(plan: &mut BridgePlan, read: Reading<'_>, source: &str, body: &Value) {
    let guild = read.guild;
    let inbound = match source {
        source::MESSAGE_CREATED => {
            payload::<MessageCreated>(body).map(|e| inbound::plan_created(guild, read.chat, &e))
        }
        source::MESSAGE_UPDATED => {
            payload::<MessageUpdated>(body).map(|e| inbound::plan_updated(guild, &e))
        }
        source::MESSAGE_DELETED => {
            payload::<MessageDeleted>(body).map(|e| inbound::plan_deleted(guild, &e))
        }
        source::REACTION_ADDED => payload::<ReactionChanged>(body)
            .map(|e| inbound::plan_reaction(guild, &e, read.chat, true)),
        source::REACTION_REMOVED => payload::<ReactionChanged>(body)
            .map(|e| inbound::plan_reaction(guild, &e, read.chat, false)),
        source::MEMBER_CHANGED => {
            payload::<MemberChanged>(body).map(|e| inbound::plan_member(guild, &e, read.room))
        }
        source::COMMAND => {
            payload::<Command>(body).map(|e| inbound::plan_command(guild, read.chat, &e))
        }
        source::AUTHORIZED => payload::<Authorized>(body).map(|e| inbound::plan_authorized(&e)),
        source::SIGN_IN => {
            if let Some(offered) = payload::<SignInOffered>(body)
                && guild.sign_in_page.as_deref().unwrap_or_default() != offered.page
            {
                plan.sign_ins.push(json!({ "page": offered.page }));
            }
            None
        }
        source::LAYOUT_CHANGED => {
            if let Some(layout) = payload::<LayoutChanged>(body) {
                layout_changed(plan, read, &layout);
            }
            None
        }
        _ => None,
    };
    if let Some(inbound) = inbound {
        inbound_plan(plan, inbound);
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
    observed.observe_emojis(layout.emojis.clone());
    if observed == *read.guild {
        reconcile(plan, read);
        // On every start too: the workspace's emoji made while the bridge
        // was away, or that the server had no room or permission for.
        emoji(plan, read);
    } else {
        plan.observe.push(json!({
            "bot_user": layout.bot_user,
            "channels": layout.channels,
            "roles": layout.roles,
            "emojis": layout.emojis,
        }));
    }
}

fn inbound_plan(plan: &mut BridgePlan, inbound: InboundPlan) {
    plan.avatars.extend(values(inbound.avatars));
    plan.posts.extend(inbound.posts.into_iter().map(|p| {
        json!({
            "channel": p.channel,
            "text": p.text,
            "parent": p.parent,
            "appearance": p.appearance,
            "discord": p.discord,
            "persona": p.persona,
            "written": p.written,
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
            "written": f.post.written,
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
    plan.offers.extend(values(inbound.offers));
    plan.sign_in_offers.extend(values(inbound.sign_in_offers));
    plan.answers.extend(values(inbound.answers));
    plan.unlinks.extend(
        inbound
            .unlinks
            .into_iter()
            .map(|user| json!({ "discord_user": user })),
    );
    plan.renames.extend(values(inbound.renames));
}

#[cfg(test)]
mod tests;
