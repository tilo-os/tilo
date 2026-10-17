//! Tilo to Discord (discord-bridge.md §3): what a Chat change becomes in
//! Discord. Every plan is a list per verb, empty when nothing goes across,
//! so a program runs it with one loop per list and needs no conditions.

use discord_wire::op::{DeleteOp, EditOp, OutAttachment, ReactOp, SendOp, StartThreadOp};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

use crate::record::{Guild, MAX_CONTENT_CHARS, MAX_UPLOAD_BYTES, MappedKind, Origin};
use crate::slug;
use crate::tilo::{self, Message};

/// A send, and the thread root the posted message maps under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendItem {
    pub op: SendOp,
    /// `map` the result as a message with this root.
    pub root: Option<String>,
}

/// What a Tilo post becomes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PostPlan {
    /// `start-thread`, then `map` as a thread, then plan again: the send
    /// needs the thread's id.
    pub threads: Vec<StartThreadOp>,
    /// `send`, then `map` as a message.
    pub sends: Vec<SendItem>,
}

/// Where the bridge's notice of a removed message points readers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Context {
    /// The base of public avatar URLs, `https://…/avatars/`.
    #[serde(default)]
    pub avatars: String,
    /// The message's Tilo link, for the line a cut or a large file adds.
    #[serde(default)]
    pub link: String,
}

/// The plan for the Chat message `message` of `chat`.
#[must_use]
pub fn plan_post(
    guild: &Guild,
    chat: &serde_json::Value,
    message: &str,
    context: &Context,
) -> PostPlan {
    let mut plan = PostPlan::default();
    let Some(message) = tilo::message(chat, message) else {
        return plan;
    };
    if guild.mapping(MappedKind::Message, &message.id).is_some() {
        return plan;
    }
    let Some(target) = target(guild, &message) else {
        return plan;
    };
    let thread = match &message.parent {
        Some(root) => {
            let Some(thread) = guild.mapping(MappedKind::Thread, root) else {
                // The root must be in Discord to grow a thread from.
                if let Some(root_message) = guild.mapping(MappedKind::Message, root) {
                    let name = tilo::message(chat, root)
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
    let author = tilo::person(chat, &message.author);
    let name = message
        .appearance_name
        .clone()
        .or_else(|| author.as_ref().map(|a| a.name.clone()))
        .unwrap_or_default();
    let automated = author.as_ref().is_some_and(|a| a.agent)
        || message.author.starts_with("bot:")
        || message.author.starts_with("agent:");
    let avatar_blob = author
        .as_ref()
        .and_then(|a| a.avatar.clone())
        .filter(|_| !context.avatars.is_empty());
    let (text, attachments) = body(guild, &message, context);
    if text.is_empty() && attachments.is_empty() {
        return plan;
    }
    plan.sends.push(SendItem {
        op: SendOp {
            channel: target.channel,
            webhook: target.webhook,
            thread,
            username: slug::webhook_username(&name, automated),
            avatar_url: avatar_blob
                .as_ref()
                .map(|b| format!("{}{b}", context.avatars)),
            avatar_blob,
            content: text,
            attachments,
            created_ms: 0,
            tilo: message.id.clone(),
        },
        root: message.parent,
    });
    plan
}

struct Target {
    channel: String,
    webhook: String,
}

fn target(guild: &Guild, message: &Message) -> Option<Target> {
    let channel = guild.mapping(MappedKind::Channel, &message.channel)?;
    let webhook = guild.mapping(MappedKind::Webhook, &message.channel)?;
    Some(Target {
        channel: channel.discord.clone(),
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

/// The Discord text of `message` and the files that go with it. Mentions of
/// linked people become `<@id>`; everything else stays plain text, and the
/// send allows only user mentions, so `@everyone` typed in Tilo pings nobody.
fn body(guild: &Guild, message: &Message, context: &Context) -> (String, Vec<OutAttachment>) {
    let mut text = String::new();
    let mut at = 0;
    let mut mentions = message.mentions.clone();
    mentions.sort_by_key(|(_, start, _)| *start);
    for (user, start, len) in &mentions {
        let end = start.saturating_add(*len);
        let (Some(before), Some(run)) =
            (message.text.get(at..*start), message.text.get(*start..end))
        else {
            continue;
        };
        text.push_str(before);
        match guild.links.iter().find(|l| l.person == *user) {
            Some(link) => {
                let _ = write!(text, "<@{}>", link.discord_user);
            }
            None => text.push_str(run.trim_start_matches('@')),
        }
        at = end;
    }
    text.push_str(message.text.get(at..).unwrap_or_default());

    let mut attachments = Vec::new();
    let mut extra = Vec::new();
    if let Some(file) = &message.attachment {
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
    #[allow(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!("{mib:.1} MiB")
}

/// What an edit of `message` becomes.
#[must_use]
pub fn plan_edit(
    guild: &Guild,
    chat: &serde_json::Value,
    message: &str,
    context: &Context,
) -> Vec<EditOp> {
    let Some(mapped) = guild.mapping(MappedKind::Message, message) else {
        return Vec::new();
    };
    if mapped.origin != Origin::Tilo {
        return Vec::new();
    }
    let Some(message) = tilo::message(chat, message) else {
        return Vec::new();
    };
    let Some(target) = target(guild, &message) else {
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteItem {
    pub op: DeleteOp,
    pub tilo: String,
}

/// What deleting or removing Tilo message `message` becomes: its Discord
/// copy deleted, through the webhook when the bridge sent it and as the bot
/// user otherwise. `channel` is the message's Tilo channel (the message is
/// gone from the record by now).
#[must_use]
pub fn plan_delete(guild: &Guild, channel: &str, message: &str) -> Vec<DeleteItem> {
    let Some(mapped) = guild.mapping(MappedKind::Message, message) else {
        return Vec::new();
    };
    let Some(discord_channel) = guild.mapping(MappedKind::Channel, channel) else {
        return Vec::new();
    };
    let thread = mapped
        .root
        .as_ref()
        .and_then(|root| guild.mapping(MappedKind::Thread, root))
        .map(|t| t.discord.clone());
    let webhook = match mapped.origin {
        Origin::Tilo => guild
            .mapping(MappedKind::Webhook, channel)
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
        tilo: message.to_owned(),
    }]
}

/// The bot's reaction plan after a Tilo `react` or `unreact`: the bot
/// reacts once while any Tilo person has the emoji, and takes it back with
/// the last one. Reactions the bridge made for Discord users do not count;
/// Discord shows those itself.
#[must_use]
pub fn plan_reaction(
    guild: &Guild,
    chat: &serde_json::Value,
    message: &str,
    emoji: &str,
    added: bool,
) -> (Vec<ReactOp>, Vec<ReactOp>) {
    let none = (Vec::new(), Vec::new());
    if emoji.starts_with(':') {
        return none;
    }
    let Some(mapped) = guild.mapping(MappedKind::Message, message) else {
        return none;
    };
    let Some(row) = tilo::message(chat, message) else {
        return none;
    };
    let channel_id = match row
        .parent
        .as_ref()
        .and_then(|root| guild.mapping(MappedKind::Thread, root))
    {
        Some(thread) => thread.discord.clone(),
        None => match guild.mapping(MappedKind::Channel, &row.channel) {
            Some(channel) => channel.discord.clone(),
            None => return none,
        },
    };
    let tilo_people = row
        .reactions
        .iter()
        .filter(|r| r.emoji == emoji && r.persona.is_none())
        .count();
    let op = ReactOp {
        channel: channel_id,
        message: mapped.discord.clone(),
        emoji: emoji.to_owned(),
    };
    match (added, tilo_people) {
        (true, 1) => (vec![op], Vec::new()),
        (false, 0) => (Vec::new(), vec![op]),
        _ => none,
    }
}

#[cfg(test)]
mod tests;
