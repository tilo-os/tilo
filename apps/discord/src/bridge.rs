//! The bridge bot's planning (discord-bridge.md §3): what one event, a Chat
//! change or a Discord event, means for both sides.
//!
//! The bot's program only sequences steps, so everything it does comes from
//! one effect. The program reads tl Chat's tree through the core read, and
//! `plan-event` answers a [`BridgePlan`]: one list per verb, each element holding exactly that
//! verb's arguments, so the program is a fixed run of loops with no
//! conditions. Work that needs an earlier result (a thread before the reply
//! in it, an avatar before the post shown with it, a category before its
//! channels) is planned on the next pass, after the earlier step's mapping
//! is in the resource; [`BridgePlan::again`] asks for that pass.

use discord_wire::event::{
    Authorized, Command, LayoutChanged, MemberChanged, MessageCreated, MessageDeleted,
    MessageUpdated, ReactionChanged, SignInOffered, source,
};
use serde::Serialize;
use serde_json::{Value, json};
use ui::tilo_actions::Acting;
use ui::tilo_resource::MemberId;

use crate::guild::{Guild, MappedKind, Ownership};
use crate::inbound::{self, InboundPlan};
use crate::outbound::{self, Context};
use crate::tilo;
use crate::{import, layout, order, roles};

/// Every tl Chat resource's kind begins with this: the type the change wrote.
pub const CHAT_PREFIX: &str = "tl.so.tilo.chat.";
/// Chat's root resource kind: the sidebar's conversations.
pub const CHAT_KIND: &str = "tl.so.tilo.chat.Conversations";
/// The workspace's own emoji, which the bridge uploads to the server.
pub const EMOJI_KIND: &str = "tilo.workspace.emoji";
/// The workspace's own governance resource kind, whose role changes the
/// bridge mirrors.
pub const WORKSPACE_KIND: &str = "tilo.workspace";

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
    /// `arrange-channels`: Discord's channels put in Chat's order.
    pub arrange_channels: Vec<Value>,
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
    /// `kick`: a linked person was removed from the workspace.
    pub kicks: Vec<Value>,
    /// `ban`: a linked person was banned from the workspace.
    pub bans: Vec<Value>,
    pub unbans: Vec<Value>,
    /// `fetch-asset`, then `remember-avatar`.
    pub avatars: Vec<Value>,
    /// Chat `relay` with the Discord author's name, then `map`.
    pub posts: Vec<Value>,
    /// `fetch-asset`, then Chat `relay` with the file, then `map`.
    pub files: Vec<Value>,
    /// Chat `relay` with the Discord author's name and where it was written, then `map`: what
    /// Discord wrote before the newest message its Tilo conversation holds.
    pub imports: Vec<Value>,
    /// `fetch-asset`, then Chat `relay` with the file, then `map`.
    pub file_imports: Vec<Value>,
    /// Chat `start-thread` in a conversation, then `map` the thread, then plan again: a reply in
    /// a Discord thread whose Tilo thread does not exist yet.
    pub tilo_threads: Vec<Value>,
    pub chat_edits: Vec<Value>,
    pub chat_deletes: Vec<Value>,
    /// Chat `delete`, then `unmap`.
    pub chat_removes: Vec<Value>,
    /// Chat `react`: the bot's own reaction for a Discord member.
    pub chat_reacts: Vec<Value>,
    /// Chat `react` again: the bot takes its reaction back.
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
    /// `fetch-asset` of a linked account's avatar, then `set-link-photo`.
    pub photos: Vec<Value>,
    pub import_categories: Vec<Value>,
    pub import_channels: Vec<Value>,
    /// `map` of a Tilo channel or category that already matches.
    pub adopt: Vec<Value>,
    /// Chat `move_channel`: a joined channel moves to its Discord channel's category.
    pub file_channels: Vec<Value>,
    /// Chat `delete_channel`: a channel of the workspace's the server had no
    /// match for and nothing was written in.
    pub drop_channels: Vec<Value>,
    /// Chat `delete_category`: a category of Tilo's own the moves or dropped
    /// channels left empty.
    pub drop_categories: Vec<Value>,
    /// Chat `move_channel` / `move_category`: Chat put in Discord's order.
    pub chat_moves: Vec<Value>,
    /// `finish-import`, with the Tilo channels left Tilo's own.
    pub finish: Vec<Value>,
    /// Plan again after this pass: something here unblocks more.
    pub again: bool,
}

/// A reaction someone made on a Chat message: Chat's `react`, which adds or takes back the
/// invoker's own reaction.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reacted {
    message: String,
    emoji: String,
    added: bool,
}

/// What a change did, from its history entry (`did`): the verbs it ran.
#[derive(Default)]
struct Did {
    verbs: Vec<String>,
}

/// What fired the bot, read from its `event` input.
enum Fired<'e> {
    Chat {
        verbs: Vec<String>,
        item: Option<&'e str>,
        event: &'e Value,
    },
    Workspace {
        verbs: Vec<String>,
        event: &'e Value,
    },
    /// The workspace's emoji changed.
    Emoji,
    /// A change to the bridged guild resource itself, such as a Moderator's
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
    let Did { verbs } = did;
    match event.get("kind").and_then(Value::as_str) {
        Some(kind) if kind.starts_with(CHAT_PREFIX) => Fired::Chat {
            verbs,
            item: event.pointer("/item/id").and_then(Value::as_str),
            event,
        },
        Some(WORKSPACE_KIND) => Fired::Workspace { verbs, event },
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

/// The id `value` names: a reference to a resource, or the resource's id.
fn named(value: &Value) -> Option<&str> {
    match value {
        Value::String(id) => Some(id),
        Value::Object(reference) => reference.get("id").and_then(Value::as_str),
        _ => None,
    }
}

/// The message a Chat change is about: the one its verb names, else the
/// item it addressed.
fn about<'a>(item: Option<&'a str>, event: &'a Value) -> &'a str {
    event
        .pointer("/arguments/m")
        .and_then(named)
        .or_else(|| event.pointer("/arguments/message").and_then(named))
        .or(item)
        .unwrap_or_default()
}

/// The Chat verbs that make a message: a send, a relay, and sharing a view, which posts one.
const POSTS: &[&str] = &["send", "relay", "send_view"];

fn posts(verb: &str) -> bool {
    POSTS.contains(&verb)
}

/// The conversation a Chat change names: its target, which for a send is where it posts.
fn channel_of<'a>(item: Option<&'a str>, event: &'a Value) -> &'a str {
    event
        .pointer("/arguments/to")
        .and_then(named)
        .or_else(|| event.pointer("/arguments/channel").and_then(named))
        .or(item)
        .unwrap_or_default()
}

/// The message a post made: the newest message in its conversation by its
/// author with its text that the bridge has not sent yet.
fn posted(
    guild: &Guild,
    chat: &tilo::ChatRead,
    item: Option<&str>,
    event: &Value,
) -> Option<String> {
    let named = about(item, event);
    // A reply is made in the conversation it answers in.
    if let Some(message) = chat.message(named) {
        return Some(message.id);
    }
    let channel = tilo::published(channel_of(item, event));
    let text = event.pointer("/arguments/body").and_then(Value::as_str);
    let author = actor(event);
    chat.messages()
        .into_iter()
        .rev()
        .find(|message| {
            message.channel == channel
                && author.as_ref() == Some(&message.author)
                && text.is_none_or(|body| message.text == body)
                && guild.mapping(MappedKind::Message, &message.id).is_none()
        })
        .map(|message| message.id)
}

/// The member who made the change an event describes.
fn actor(event: &Value) -> Option<MemberId> {
    let acting: Acting = serde_json::from_value(event.get("actor")?.clone()).ok()?;
    acting.member_id().cloned()
}

fn payload<T: serde::de::DeserializeOwned>(payload: &Value) -> Option<T> {
    serde_json::from_value(payload.clone()).ok()
}

fn values<T: Serialize>(items: impl IntoIterator<Item = T>) -> Vec<Value> {
    items
        .into_iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .collect()
}

/// What a plan reads: the guild, Chat's tree of the workspace, and where
/// links point.
#[derive(Clone, Copy)]
pub struct Reading<'a> {
    pub guild: &'a Guild,
    pub chat: &'a tilo::ChatRead,
    pub context: &'a Context,
    /// The workspace's members with their roles (`{"members": [{"member":
    /// {"person"}, "role"}]}`), for a Discord-side moderator's access in Tilo; `null`
    /// when the host binds none, which treats nobody as a moderator.
    pub workspace: &'a Value,
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
        Fired::Chat { verbs, item, event } => {
            for verb in &verbs {
                let message = if posts(verb) {
                    posted(read.guild, read.chat, item, event).unwrap_or_default()
                } else {
                    about(item, event).to_owned()
                };
                tilo_change(&mut plan, read, verb, &message, event);
            }
        }
        Fired::Workspace { verbs, event } => {
            for verb in &verbs {
                workspace_change(&mut plan, read.guild, verb, event);
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
        && plan.tilo_threads.is_empty()
        && plan.finish.is_empty()
        // A channel made now is put in its place on the next pass.
        && !plan
            .apply_channels
            .iter()
            .any(|apply| apply.pointer("/op/id").is_none_or(Value::is_null)));
    plan
}

/// The Chat verbs that rearrange the workspace's conversations: after them the
/// bridge puts Discord in Chat's order.
const ARRANGES: &[&str] = &[
    "new_channel",
    "add_channel",
    "new_voice_channel",
    "add_voice_channel",
    "new_category",
    "move_channel",
    "move_category",
    "delete_channel",
    "delete_category",
    "start_thread",
];

fn tilo_change(plan: &mut BridgePlan, read: Reading<'_>, verb: &str, message: &str, event: &Value) {
    let Reading {
        guild,
        chat,
        context,
        ..
    } = read;
    match verb {
        verb if posts(verb) => post(plan, guild, chat, message, context),
        "edit" => plan
            .edits
            .extend(values(outbound::plan_edit(guild, chat, message, context))),
        "delete" => {
            let channel = chat
                .message(message)
                .map_or_else(|| channel_of(None, event).to_owned(), |m| m.channel);
            plan.deletes.extend(values(outbound::plan_delete(
                guild, chat, &channel, message,
            )));
        }
        "react" => {
            if let Some(emoji) = event.pointer("/arguments/emoji").and_then(Value::as_str)
                && let Some(member) = actor(event)
            {
                let added = chat.reacted(message, emoji, &member);
                let reacted = Reacted {
                    message: message.to_owned(),
                    emoji: emoji.to_owned(),
                    added,
                };
                let (reacts, unreacts) = outbound::plan_reaction(
                    guild,
                    chat,
                    &reacted.message,
                    &reacted.emoji,
                    reacted.added,
                );
                plan.reacts.extend(values(reacts));
                plan.unreacts.extend(values(unreacts));
            }
        }
        // A reaction the bridge relayed from Discord is already there: it is not sent back.
        "relay_reaction" => {}
        verb => reconcile(plan, read, ARRANGES.contains(&verb)),
    }
}

fn post(
    plan: &mut BridgePlan,
    guild: &Guild,
    chat: &tilo::ChatRead,
    message: &str,
    context: &Context,
) {
    let post = outbound::plan_post(guild, chat, message, context);
    plan.threads.extend(values(post.threads));
    plan.sends.extend(values(post.sends));
}

/// Adopting the server, then keeping it bridged: what Discord makes is
/// imported, and the layout and role reconciles do the rest. `arranged`:
/// someone rearranged Chat, which Discord follows.
fn reconcile(plan: &mut BridgePlan, read: Reading<'_>, arranged: bool) {
    let guild = read.guild;
    let tilo = read.chat.workspace();
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
                "kind": if c.kind == "voice" {
                    "tl.so.tilo.chat.VoiceChannel"
                } else {
                    "tl.so.tilo.chat.TextChannel"
                },
                "discord": c.discord,
            })
        }));
    plan.adopt.extend(values(import.adopt));
    plan.file_channels.extend(import.file.into_iter().map(|f| {
        let kind = tilo.channels.iter().find(|c| c.id == f.channel).map_or(
            "tl.so.tilo.chat.TextChannel",
            |c| {
                if c.voice {
                    "tl.so.tilo.chat.VoiceChannel"
                } else {
                    "tl.so.tilo.chat.TextChannel"
                }
            },
        );
        json!({ "channel": f.channel, "category": f.category, "kind": kind })
    }));
    let sidebar = read.chat.root_item().unwrap_or_default();
    plan.drop_channels
        .extend(import.drop_channels.into_iter().map(|channel| {
            let found = tilo.channels.iter().find(|c| c.id == channel);
            let category = found.and_then(|c| c.category.clone());
            let from = category.clone().unwrap_or_else(|| sidebar.clone());
            let from_kind = if category.is_some() {
                "tl.so.tilo.chat.Category"
            } else {
                "tl.so.tilo.chat.Conversations"
            };
            let kind = if found.is_some_and(|c| c.voice) {
                "tl.so.tilo.chat.VoiceChannel"
            } else {
                "tl.so.tilo.chat.TextChannel"
            };
            json!({ "channel": channel, "category": category, "from": from,
                    "from_kind": from_kind, "kind": kind })
        }));
    plan.drop_categories.extend(
        import
            .drop_categories
            .into_iter()
            .map(|category| json!({ "category": category })),
    );
    // Adopted, the workspace lists the server's channels as the server does.
    if !import.finish.is_empty() {
        plan.chat_moves
            .extend(values(order::to_chat(guild, guild, &tilo)));
    }
    plan.finish.extend(values(import.finish));
    if guild.ownership == Ownership::Adopted {
        let layout = layout::plan(guild, &tilo);
        if arranged && plan.arrange_channels.is_empty() {
            plan.arrange_channels
                .extend(values(order::to_discord(guild, &tilo)));
        }
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

fn workspace_change(plan: &mut BridgePlan, guild: &Guild, verb: &str, event: &Value) {
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
        if let Some(person) = actor(event).as_ref().and_then(MemberId::person_id) {
            inbound_plan(
                plan,
                read,
                inbound::plan_linked(read.guild, read.chat, person),
            );
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
    inbound_plan(plan, read, inbound::plan_block(read.guild, user));
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
            payload::<MemberChanged>(body).map(|e| inbound::plan_member(guild, &e, read.workspace))
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
        inbound_plan(plan, read, inbound);
    }
}

/// Record what Discord reported first; once the resource holds it, reconcile.
fn layout_changed(plan: &mut BridgePlan, read: Reading<'_>, layout: &LayoutChanged) {
    let mut observed = read.guild.clone();
    observed.observe(
        layout.bot_user.clone(),
        layout.channels.clone(),
        layout.roles.clone(),
    );
    observed.observe_emojis(layout.emojis.clone());
    let adopted = read.guild.ownership == Ownership::Adopted;
    let workspace = read.chat.workspace();
    if observed == *read.guild {
        reconcile(plan, read, false);
        // Back after a while: what moved in Discord meanwhile shows in Chat.
        if adopted && layout.connected {
            plan.chat_moves
                .extend(values(order::to_chat(read.guild, read.guild, &workspace)));
        }
        // On every start too: the workspace's emoji made while the bridge
        // was away, or that the server had no workspace or permission for.
        emoji(plan, read);
        // Linked avatars that changed while the bridge was away.
        plan.photos
            .extend(values(inbound::plan_link_photos(read.guild)));
    } else {
        plan.observe.push(json!({
            "bot_user": layout.bot_user,
            "channels": layout.channels,
            "roles": layout.roles,
            "emojis": layout.emojis,
        }));
        // Someone moved a channel or category in Discord: Chat follows. The
        // bridge's own order on its way there is no such move.
        if adopted
            && (layout.connected || order::moved_in_discord(read.guild, &observed, &workspace))
        {
            plan.chat_moves
                .extend(values(order::to_chat(&observed, read.guild, &workspace)));
        }
    }
}

/// What a Discord post becomes in Chat: a message relayed at the name and time it was written
/// there, answering the message it replied to.
fn relay_item(post: &inbound::PostItem, chat: &tilo::ChatRead) -> Value {
    relay_item_fetched(post, chat, None)
}

/// [`relay_item`], with what the worker fetches for the file's post (`fetch`) when the item
/// relays a file.
fn relay_item_fetched(
    post: &inbound::PostItem,
    chat: &tilo::ChatRead,
    fetch: Option<Value>,
) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("channel".into(), json!(post.channel));
    fields.insert(
        "channel_kind".into(),
        json!(chat.conversation_kind(&post.channel)),
    );
    fields.insert("text".into(), json!(post.text));
    fields.insert("reply_to".into(), json!(post.parent));
    fields.insert("by".into(), json!(post.appearance.name));
    fields.insert("written_in".into(), json!("Discord"));
    fields.insert("written".into(), json!(post.written.saturating_mul(1000)));
    fields.insert("appearance".into(), json!(post.appearance));
    fields.insert("discord".into(), json!(post.discord));
    fields.insert("persona".into(), json!(post.persona));
    if let Some(fetch) = fetch {
        fields.insert("fetch".into(), fetch);
    }
    Value::Object(fields)
}

fn inbound_plan(plan: &mut BridgePlan, read: Reading<'_>, inbound: InboundPlan) {
    plan.avatars.extend(values(inbound.avatars));
    plan.posts
        .extend(inbound.posts.iter().map(|p| relay_item(p, read.chat)));
    plan.files.extend(
        inbound
            .files
            .into_iter()
            .map(|f| relay_item_fetched(&f.post, read.chat, Some(json!(f.fetch)))),
    );
    plan.imports
        .extend(inbound.imports.iter().map(|p| relay_item(p, read.chat)));
    plan.file_imports.extend(
        inbound
            .file_imports
            .into_iter()
            .map(|f| relay_item_fetched(&f.post, read.chat, Some(json!(f.fetch)))),
    );
    plan.chat_edits.extend(
        inbound
            .edits
            .into_iter()
            .map(|e| json!({ "message": e.message, "text": e.text })),
    );
    for id in inbound.deletes {
        let channel = read.chat.message(&id).map(|m| m.channel);
        let kind = channel
            .as_deref()
            .map_or("tl.so.tilo.chat.TextChannel", |channel| {
                read.chat.conversation_kind(channel)
            });
        plan.chat_deletes
            .push(json!({ "message": id, "channel": channel, "channel_kind": kind }));
    }
    plan.chat_removes
        .extend(inbound.removes.into_iter().map(|r| {
            let channel = read.chat.message(&r.message).map(|m| m.channel);
            let kind = channel
                .as_deref()
                .map_or("tl.so.tilo.chat.TextChannel", |channel| {
                    read.chat.conversation_kind(channel)
                });
            json!({ "message": r.message, "channel": channel, "channel_kind": kind,
                    "reason": r.reason })
        }));
    let reaction = |r: inbound::ReactItem| {
        json!({
            "message": r.message,
            "emoji": r.emoji,
            "persona": r.persona,
            "by": r.by,
        })
    };
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
    plan.photos.extend(values(inbound.photos));
}

#[cfg(test)]
mod tests;
