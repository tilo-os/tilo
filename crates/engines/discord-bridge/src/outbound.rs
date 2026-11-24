//! Tilo to Discord (discord-bridge.md §3): what a Chat change becomes in
//! Discord. Every plan is a list per verb, empty when nothing goes across,
//! so a program runs it with one loop per list and needs no conditions.

use discord_wire::op::{DeleteOp, EditOp, OutAttachment, ReactOp, SendOp, StartThreadOp};

use serde::{Deserialize, Serialize};


use tilo_resource::MemberId;

use crate::guild::{Guild, MAX_CONTENT_CHARS, MAX_UPLOAD_BYTES, MappedKind, Origin};
use crate::tilo::Message;
use crate::{emoji, slug};

/// A send, and the thread root the posted message maps under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendItem {
    pub op: SendOp,
    /// `map` the result as a message with this root.
    pub root: Option<String>,
    /// The message's Tilo channel, which the webhook a send reports it
    /// went out through (its old one gone from Discord) maps to.
    pub channel: String,
}

/// What a Tilo post becomes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostPlan {
    /// `start-thread`, then `map` as a thread, then plan again: the send
    /// needs the thread's id.
    pub threads: Vec<StartThreadOp>,
    /// `send`, then `map` as a message.
    pub sends: Vec<SendItem>,
}

/// Where the bridge's notice of a removed message points readers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// The base of public avatar URLs, `https://…/avatars/`.
    #[serde(default)]
    pub avatars: String,
    /// The message's Tilo link, for the line a cut or a large file adds.
    #[serde(default)]
    pub link: String,
    /// Where the workspace's resources open on the web, for the link a shared
    /// window or resource carries; none when the workspace's host serves no web
    /// page.
    #[serde(default)]
    pub links: Option<Box<Links>>,
}

/// Where a workspace's host serves its resources on the web: what `share-link`
/// mints its links from (`ResourceViewLocator`), less the resource, which
/// each message names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Links {
    /// The web entry the browser bundle is served under (`https://…/`).
    pub web_entry: String,
    /// The host's browser origin.
    pub origin: String,
    /// The workspace's id.
    pub workspace: String,
    /// The certified host device key, hex.
    pub host_device: String,
}

/// The plan for the Chat message `message` of `chat`.
#[must_use]
pub fn plan_post(
    guild: &Guild,
    chat: &crate::tilo::ChatRead,
    message: &str,
    context: &Context,
) -> PostPlan {
    let mut plan = PostPlan::default();
    let Some(message) = chat.message(message) else {
        return plan;
    };
    if guild.mapping(MappedKind::Message, &message.id).is_some() {
        return plan;
    }
    // A message the bridge relayed from Discord is already there: never sent back.
    if !message.by.is_empty() {
        return plan;
    }
    let Some(target) = target(guild, chat, &message) else {
        return plan;
    };
    let thread = match &message.parent {
        Some(root) => {
            let Some(thread) = guild.mapping(MappedKind::Thread, root) else {
                // The root must be in Discord to grow a thread from.
                if let Some(root_message) = guild.mapping(MappedKind::Message, root) {
                    let name = chat
                        .message(root)
                        .map(|m| thread_name(&m.text))
                        .unwrap_or_default();
                    plan.threads.push(StartThreadOp {
                        channel: target.channel,
                        message: root_message.discord.clone(),
                        name,
                        tilo: root.clone(),
                    });
                }
                return plan;
            };
            Some(thread.discord.clone())
        }
        None => None,
    };
    let author = chat.person(&message.author);
    let name = author.as_ref().map(|a| a.name.clone()).unwrap_or_default();
    let automated = author.as_ref().is_some_and(|a| a.agent) || !message.author.is_person();
    // Chat keeps no picture of a member: a linked account's Discord avatar stands for them.
    let avatar_blob: Option<String> = None;
    let (text, attachments) = body(guild, &message, context);
    if text.is_empty() && attachments.is_empty() {
        return plan;
    }
    // A person who linked their Discord account reads as themselves there:
    // their name with their handle, and their Discord picture while they
    // have none of their own in Tilo.
    let link = guild
        .links
        .iter()
        .find(|link| message.author.person_id() == Some(link.person.as_str()));
    let username = match link {
        Some(link) if !automated => slug::linked_webhook_username(&name, &link.user_name),
        _ => slug::webhook_username(&name, automated),
    };
    let discord_avatar = link.filter(|_| avatar_blob.is_none()).and_then(|link| {
        Some(discord_avatar_url(
            &link.discord_user,
            link.avatar.as_deref()?,
        ))
    });
    plan.sends.push(SendItem {
        op: SendOp {
            channel: target.channel.clone(),
            webhook: target.webhook,
            thread,
            username,
            avatar_url: avatar_blob
                .as_ref()
                .map(|b| format!("{}{b}", context.avatars))
                .or(discord_avatar),
            avatar_blob,
            content: text,
            attachments,
            created_ms: 0,
            tilo: message.id.clone(),
        },
        root: message.parent.clone(),
        channel: target.tilo,
    });
    plan
}

/// Where Discord serves the avatar `hash` of `user`.
#[must_use]
pub fn discord_avatar_url(user: &str, hash: &str) -> String {
    let format = if hash.starts_with("a_") { "gif" } else { "png" };
    format!("https://cdn.discordapp.com/avatars/{user}/{hash}.{format}?size=128")
}

struct Target {
    /// The Discord channel the message lands in.
    channel: String,
    /// The Chat channel it was posted in, the one holding the thread it answers in.
    tilo: String,
    webhook: String,
}

fn target(guild: &Guild, chat: &crate::tilo::ChatRead, message: &Message) -> Option<Target> {
    let tilo = chat
        .thread_channel(&message.channel)
        .unwrap_or_else(|| message.channel.clone());
    let channel = guild.mapping(MappedKind::Channel, &tilo)?;
    let webhook = guild.mapping(MappedKind::Webhook, &tilo)?;
    Some(Target {
        channel: channel.discord.clone(),
        tilo,
        webhook: webhook.discord.clone(),
    })
}

fn thread_name(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let name: String = line.chars().take(90).collect();
    if name.is_empty() {
        "Thread".to_owned()
    } else {
        name
    }
}

/// The Discord text of `message` and the files that go with it, written as plain text: the send
/// allows only user mentions, so `@everyone` typed in Tilo pings nobody.
fn body(guild: &Guild, message: &Message, context: &Context) -> (String, Vec<OutAttachment>) {
    // A workspace emoji the server has goes across as that emoji.
    let text = emoji::to_discord_text(guild, &message.text);
    let mut attachments = Vec::new();
    let mut extra = Vec::new();
    for file in &message.attachments {
        match file.size {
            Some(size) if size > MAX_UPLOAD_BYTES => {
                extra.push(
                    format!(
                        "📎 {} ({}) – open in Tilo {}",
                        file.name,
                        human_size(size),
                        context.link
                    )
                    .trim_end()
                    .to_owned(),
                );
            }
            size => attachments.push(OutAttachment {
                blob: file.blob.clone(),
                name: file.name.clone(),
                size: size.unwrap_or(0),
            }),
        }
    }
    let mut text = text;
    for line in extra {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line);
    }
    (fit(&text, &context.link), attachments)
}

/// `text` cut to Discord's limit at a character boundary, with "…" and the
/// Tilo link when it was longer.
#[must_use]
pub fn fit(text: &str, link: &str) -> String {
    if text.chars().count() <= MAX_CONTENT_CHARS {
        return text.to_owned();
    }
    let tail = if link.is_empty() {
        "…".to_owned()
    } else {
        format!("…\n{link}")
    };
    let keep = MAX_CONTENT_CHARS.saturating_sub(tail.chars().count());
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(&tail);
    out
}

fn human_size(bytes: u64) -> String {
    crate::format::size(bytes)
}

/// What an edit of `message` becomes.
#[must_use]
pub fn plan_edit(
    guild: &Guild,
    chat: &crate::tilo::ChatRead,
    message: &str,
    context: &Context,
) -> Vec<EditOp> {
    let message = crate::tilo::published(message);
    let Some(mapped) = guild.mapping(MappedKind::Message, &message) else {
        return Vec::new();
    };
    if mapped.origin != Origin::Tilo {
        return Vec::new();
    }
    let Some(message) = chat.message(&message) else {
        return Vec::new();
    };
    let Some(target) = target(guild, chat, &message) else {
        return Vec::new();
    };
    let (text, attachments) = body(guild, &message, context);
    if text.is_empty() && attachments.is_empty() {
        return Vec::new();
    }
    let thread = message
        .parent
        .as_ref()
        .and_then(|root| guild.mapping(MappedKind::Thread, root))
        .map(|t| t.discord.clone());
    vec![EditOp {
        channel: target.channel,
        webhook: target.webhook,
        message: mapped.discord.clone(),
        thread,
        content: text,
    }]
}

/// A delete, then `unmap` of the message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteItem {
    pub op: DeleteOp,
    pub tilo: String,
}

/// What deleting or removing Tilo message `message` becomes: its Discord
/// copy deleted, through the webhook when the bridge sent it and as the bot
/// user otherwise. `channel` is the message's Tilo channel (the message is
/// gone from the resource by now).
#[must_use]
pub fn plan_delete(
    guild: &Guild,
    chat: &crate::tilo::ChatRead,
    channel: &str,
    message: &str,
) -> Vec<DeleteItem> {
    let message = crate::tilo::published(message);
    let Some(mapped) = guild.mapping(MappedKind::Message, &message) else {
        return Vec::new();
    };
    let channel = chat
        .thread_channel(channel)
        .unwrap_or_else(|| channel.to_owned());
    let Some(discord_channel) = guild.mapping(MappedKind::Channel, &channel) else {
        return Vec::new();
    };
    let thread = mapped
        .root
        .as_ref()
        .and_then(|root| guild.mapping(MappedKind::Thread, root))
        .map(|t| t.discord.clone());
    let webhook = match mapped.origin {
        Origin::Tilo => guild
            .mapping(MappedKind::Webhook, &channel)
            .map(|w| w.discord.clone()),
        Origin::Discord => None,
    };
    vec![DeleteItem {
        op: DeleteOp {
            channel: discord_channel.discord.clone(),
            webhook,
            message: mapped.discord.clone(),
            thread,
        },
        tilo: message.clone(),
    }]
}

/// The bot's reaction plan after a Tilo reaction with `emoji` was added
/// (`added`) or taken back: the bot reacts once while any Tilo person has
/// the emoji, and takes it back with the last one. A reaction relayed here
/// for a Discord member is that member's own and Discord shows it itself,
/// so nothing mirrored in comes back out. A workspace emoji goes across only
/// as the server's emoji of its name ([`emoji::to_discord`]).
#[must_use]
pub fn plan_reaction(
    guild: &Guild,
    chat: &crate::tilo::ChatRead,
    message: &str,
    emoji: &str,
    added: bool,
) -> (Vec<ReactOp>, Vec<ReactOp>) {
    let none = (Vec::new(), Vec::new());
    let Some(discord_emoji) = emoji::to_discord(guild, emoji) else {
        return none;
    };
    let message = crate::tilo::published(message);
    let Some(mapped) = guild.mapping(MappedKind::Message, &message) else {
        return none;
    };
    let Some(row) = chat.message(&message) else {
        return none;
    };
    let channel_id = if let Some(thread) = row
        .parent
        .as_ref()
        .and_then(|root| guild.mapping(MappedKind::Thread, root))
    {
        thread.discord.clone()
    } else {
        let channel = chat
            .thread_channel(&row.channel)
            .unwrap_or_else(|| row.channel.clone());
        match guild.mapping(MappedKind::Channel, &channel) {
            Some(channel) => channel.discord.clone(),
            None => return none,
        }
    };
    let wanted = emoji::key(&discord_emoji);
    let tilo_people = row
        .reactions
        .iter()
        .filter(|r| {
            r.member
                .as_ref()
                .is_some_and(MemberId::is_person)
        })
        .filter(|r| emoji::to_discord(guild, &r.emoji).is_some_and(|e| emoji::key(&e) == wanted))
        .count();
    let op = ReactOp {
        channel: channel_id,
        message: mapped.discord.clone(),
        emoji: discord_emoji,
    };
    match (added, tilo_people) {
        (true, 1) => (vec![op], Vec::new()),
        (false, 0) => (Vec::new(), vec![op]),
        _ => none,
    }
}

#[cfg(test)]
mod tests;
