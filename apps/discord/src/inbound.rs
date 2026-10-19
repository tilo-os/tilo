//! Discord to Tilo (discord-bridge.md §3, §5, §7): what a connector event
//! becomes in the room's Chat. Echo suppression lives here: an event from
//! one of the bridge's webhooks or from the bot user is the bridge's own
//! output and plans nothing, and so does anything by a blocked persona.

use discord_wire::event::{
    Author, Command, MemberChanged, MessageCreated, MessageDeleted, MessageUpdated,
    ReactionChanged, UserName,
};
use discord_wire::op::{FetchAssetOp, InAttachment, MemberRolesOp};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

use crate::record::{Guild, MAX_FETCH_BYTES, MappedKind, Origin};
use crate::{roles, tilo};

/// Discord's avatars are small; anything larger is not an avatar.
pub const MAX_AVATAR_BYTES: u64 = 1024 * 1024;

/// Who a Discord-origin change is shown as (bots.md §7, `Appearance`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Appearance {
    pub name: String,
    /// The avatar's content address.
    #[serde(default)]
    pub avatar: Option<String>,
    pub persona: Persona,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Persona {
    pub service: String,
    pub account: String,
}

/// Copy an avatar, then `remember-avatar(hash, result.blob)`, then plan again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AvatarFetch {
    pub op: FetchAssetOp,
    pub hash: String,
}

/// A Chat `post` with an appearance, then `map` of the new message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PostItem {
    pub channel: String,
    pub text: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub appearance: Appearance,
    /// The Discord message the post mirrors.
    pub discord: String,
    pub persona: String,
    /// When it was written in Discord, in Unix seconds: Chat shows it then.
    pub written: i64,
}

/// `fetch-asset`, then a Chat `post` whose attachment is the result's
/// `/attachment`, then `map`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FilePost {
    pub fetch: FetchAssetOp,
    pub post: PostItem,
}

/// A Chat `edit-message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EditItem {
    pub message: String,
    pub text: String,
}

/// A Chat `remove-message` by the bot for a linked moderator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoveItem {
    pub message: String,
    pub reason: String,
}

/// A Chat `react` or `unreact` with an appearance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReactItem {
    pub message: String,
    pub emoji: String,
    pub appearance: Appearance,
}

/// `complete-link`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompleteLinkItem {
    pub discord_user: String,
    pub person: String,
    pub code_sha256: String,
    pub user_name: String,
    /// Empty when the account shows its username.
    pub display_name: String,
}

/// `rename-link`: a linked account seen under newer names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenameItem {
    pub discord_user: String,
    pub user_name: String,
    /// Empty when the account shows its username.
    pub display_name: String,
}

/// A `rename-link` for `user` seen as `names`, when they are not what the
/// link holds; nothing for an account nobody linked.
fn rename(guild: &Guild, user: &str, names: Option<&UserName>) -> Option<RenameItem> {
    let names = names?;
    let link = guild.links.iter().find(|link| link.discord_user == user)?;
    (link.user_name != names.user_name || link.display_name != names.display_name).then(|| {
        RenameItem {
            discord_user: user.to_owned(),
            user_name: names.user_name.clone(),
            display_name: names.display_name.clone().unwrap_or_default(),
        }
    })
}

/// Everything one Discord event does in Tilo, each list one verb.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboundPlan {
    /// Run first; the rest waits for the next plan.
    pub avatars: Vec<AvatarFetch>,
    pub posts: Vec<PostItem>,
    pub files: Vec<FilePost>,
    pub edits: Vec<EditItem>,
    /// Chat `delete-message` (the bot is the author), then `unmap`.
    pub deletes: Vec<String>,
    /// Chat `remove-message`, then `unmap`.
    pub removes: Vec<RemoveItem>,
    /// `unmap`, then send the Tilo message again (outbound `plan-post`).
    pub resends: Vec<String>,
    pub reacts: Vec<ReactItem>,
    pub unreacts: Vec<ReactItem>,
    /// `block-persona`.
    pub blocks: Vec<String>,
    /// `set-member-roles`.
    pub member_roles: Vec<MemberRolesOp>,
    pub complete_links: Vec<CompleteLinkItem>,
    /// `unlink`.
    pub unlinks: Vec<String>,
    /// `rename-link`.
    pub renames: Vec<RenameItem>,
}

fn persona(user: &str) -> Persona {
    Persona {
        service: "discord".into(),
        account: user.into(),
    }
}

/// The bridge's own output, or someone whose messages are not mirrored.
#[must_use]
pub fn ignored(guild: &Guild, user: &str, webhook: Option<&str>) -> bool {
    webhook.is_some_and(|w| guild.is_bridge_webhook(w))
        || (!guild.bot_user.is_empty() && user == guild.bot_user)
        || guild.blocked_personas.contains(&user.to_owned())
}

/// The appearance of `author`, or the avatar to fetch first.
fn appearance(guild: &Guild, author: &Author) -> Result<Appearance, Box<AvatarFetch>> {
    let avatar = match (&author.avatar, &author.avatar_url) {
        (Some(hash), Some(url)) => match guild.avatars.get(hash) {
            Some(blob) => Some(blob.clone()),
            None => {
                return Err(Box::new(AvatarFetch {
                    op: FetchAssetOp {
                        url: url.clone(),
                        max_bytes: MAX_AVATAR_BYTES,
                        attachment: None,
                    },
                    hash: hash.clone(),
                }));
            }
        },
        _ => None,
    };
    Ok(Appearance {
        name: author.name.clone(),
        avatar,
        persona: persona(&author.id),
    })
}

/// The Tilo channel and thread root a Discord message lands in.
fn place(
    guild: &Guild,
    event: &MessageCreated,
) -> Option<(String, Option<String>, Option<String>)> {
    // A message in a Discord thread replies to the thread's mapped root.
    if let Some(thread) = &event.thread {
        let root = guild
            .by_discord(MappedKind::Thread, &thread.id)?
            .tilo
            .clone();
        let channel = guild
            .by_discord(MappedKind::Channel, &thread.parent)?
            .tilo
            .clone();
        return Some((channel, Some(root), None));
    }
    let channel = guild
        .by_discord(MappedKind::Channel, &event.channel_id)?
        .tilo
        .clone();
    let Some(answered) = event
        .reply_to
        .as_ref()
        .and_then(|r| guild.by_discord(MappedKind::Message, r))
    else {
        return Some((channel, None, None));
    };
    // Tilo threads are one level deep: a reply to a reply joins the root's
    // thread and quotes what it answered.
    match &answered.root {
        Some(root) => {
            let quote = event.reply_text.as_ref().map(|text| {
                let line = text.lines().next().unwrap_or_default();
                match &event.reply_author {
                    Some(who) => format!("> {who}: {line}"),
                    None => format!("> {line}"),
                }
            });
            Some((channel, Some(root.clone()), quote))
        }
        None => Some((channel, Some(answered.tilo.clone()), None)),
    }
}

/// `message-created`.
#[must_use]
pub fn plan_created(guild: &Guild, event: &MessageCreated) -> InboundPlan {
    let mut plan = InboundPlan::default();
    if ignored(guild, &event.author.id, event.author.webhook_id.as_deref())
        || guild
            .by_discord(MappedKind::Message, &event.message_id)
            .is_some()
    {
        return plan;
    }
    let Some((channel, parent, quote)) = place(guild, event) else {
        return plan;
    };
    let appearance = match appearance(guild, &event.author) {
        Ok(appearance) => appearance,
        Err(fetch) => {
            plan.avatars.push(*fetch);
            return plan;
        }
    };
    let mut text = match quote {
        Some(quote) => format!("{quote}\n{}", event.content),
        None => event.content.clone(),
    };
    let mut files = Vec::new();
    for file in &event.attachments {
        if file.size > MAX_FETCH_BYTES {
            // Too big to copy: link it, though Discord's links expire.
            if !text.is_empty() {
                text.push('\n');
            }
            let _ = write!(text, "📎 {} {}", file.name, file.url);
        } else {
            files.push(file);
        }
    }
    let post = |text: String| PostItem {
        channel: channel.clone(),
        text,
        parent: parent.clone(),
        appearance: appearance.clone(),
        discord: event.message_id.clone(),
        persona: event.author.id.clone(),
        written: discord_wire::snowflake_ms(&event.message_id).div_euclid(1000),
    };
    // The text goes with the first file, as one Chat message, the way
    // Discord shows them; any further file is a message of its own.
    let mut text = Some(text).filter(|text| !text.trim().is_empty());
    if files.is_empty() {
        plan.posts.extend(text.take().map(post));
    }
    for file in files {
        // Chat lays an image out by its size, so one Discord gives no size
        // for is posted as a plain file, as Chat's own import does.
        let kind = if file.content_type.starts_with("image/") && file.width > 0 && file.height > 0 {
            "image"
        } else if file.content_type.starts_with("video/") {
            "video"
        } else {
            "file"
        };
        plan.files.push(FilePost {
            fetch: FetchAssetOp {
                url: file.url.clone(),
                max_bytes: MAX_FETCH_BYTES,
                attachment: Some(InAttachment {
                    kind: kind.into(),
                    name: file.name.clone(),
                    blob: String::new(),
                    width: file.width,
                    height: file.height,
                    size: file.size,
                }),
            },
            post: post(text.take().unwrap_or_default()),
        });
    }
    plan
}

/// `message-updated`.
#[must_use]
pub fn plan_updated(guild: &Guild, event: &MessageUpdated) -> InboundPlan {
    let mut plan = InboundPlan::default();
    if ignored(guild, &event.author.id, event.author.webhook_id.as_deref()) {
        return plan;
    }
    if let Some(mapped) = guild.by_discord(MappedKind::Message, &event.message_id)
        && mapped.origin == Origin::Discord
    {
        plan.edits.push(EditItem {
            message: mapped.tilo.clone(),
            text: event.content.clone(),
        });
    }
    plan
}

/// `message-deleted`. `room` is the `tilo.room` read, for the executor's role.
#[must_use]
pub fn plan_deleted(
    guild: &Guild,
    event: &MessageDeleted,
    room: &serde_json::Value,
) -> InboundPlan {
    let mut plan = InboundPlan::default();
    let Some(mapped) = guild.by_discord(MappedKind::Message, &event.message_id) else {
        return plan;
    };
    match mapped.origin {
        // Each attachment after the first is a separate Chat message, but
        // all copies share the same Discord id and must disappear together.
        Origin::Discord => plan.deletes.extend(
            guild
                .map
                .iter()
                .filter(|m| {
                    m.kind == MappedKind::Message
                        && m.origin == Origin::Discord
                        && m.discord == event.message_id
                })
                .map(|m| m.tilo.clone()),
        ),
        Origin::Tilo => {
            let moderator = event
                .executor
                .as_ref()
                .filter(|executor| **executor != guild.bot_user)
                .and_then(|executor| guild.links.iter().find(|l| l.discord_user == *executor))
                .filter(|link| {
                    matches!(
                        tilo::room_role(room, &link.person).as_deref(),
                        Some("moderator" | "owner")
                    )
                });
            match moderator {
                Some(link) => plan.removes.push(RemoveItem {
                    message: mapped.tilo.clone(),
                    reason: format!("removed in Discord by the linked moderator {}", link.person),
                }),
                // A Discord-only permission must not remove a Tilo person's message.
                None if event.executor.as_deref() != Some(guild.bot_user.as_str()) => {
                    plan.resends.push(mapped.tilo.clone());
                }
                None => {}
            }
        }
    }
    plan
}

/// `reaction-added` (`added`) and `reaction-removed`.
#[must_use]
pub fn plan_reaction(guild: &Guild, event: &ReactionChanged, added: bool) -> InboundPlan {
    let mut plan = InboundPlan::default();
    if ignored(guild, &event.user_id, None) {
        return plan;
    }
    let Some(mapped) = guild.by_discord(MappedKind::Message, &event.message_id) else {
        return plan;
    };
    let avatar = event.user_avatar_url.as_ref().and_then(|url| {
        guild
            .avatars
            .iter()
            .find(|(hash, _)| url.contains(hash.as_str()))
            .map(|(_, blob)| blob.clone())
    });
    let item = ReactItem {
        message: mapped.tilo.clone(),
        emoji: event.emoji.clone(),
        appearance: Appearance {
            name: event.user_name.clone(),
            avatar,
            persona: persona(&event.user_id),
        },
    };
    if added {
        plan.reacts.push(item);
    } else {
        plan.unreacts.push(item);
    }
    plan
}

/// `member-changed`: a ban blocks the persona and removes what was mirrored
/// of them; a linked member's roles changed by hand are set back.
#[must_use]
pub fn plan_member(guild: &Guild, event: &MemberChanged, room: &serde_json::Value) -> InboundPlan {
    let mut plan = InboundPlan::default();
    if event.banned {
        // The bridge's own `block-persona` fires nothing, so the ban
        // removes what was mirrored of them here, as a Moderator's block does.
        if !guild.blocked_personas.contains(&event.user_id) {
            plan = plan_block(guild, &event.user_id);
            plan.blocks.push(event.user_id.clone());
        }
        return plan;
    }
    if event.left {
        return plan;
    }
    if let Some(link) = guild.links.iter().find(|l| l.discord_user == event.user_id) {
        let role = tilo::room_role(room, &link.person);
        plan.member_roles.extend(roles::plan_member(
            guild,
            &event.user_id,
            role.as_deref(),
            Some(&event.roles),
        ));
    }
    plan.renames
        .extend(rename(guild, &event.user_id, event.user.as_ref()));
    plan
}

/// `command`: `/link` and `/unlink`. A `/link` always reaches
/// `complete-link`, which counts a wrong code as a failure.
#[must_use]
pub fn plan_command(guild: &Guild, event: &Command) -> InboundPlan {
    let mut plan = InboundPlan::default();
    match event.name.as_str() {
        "link" => {
            let Some(hash) = &event.code_sha256 else {
                return plan;
            };
            let person = guild
                .pending_links
                .iter()
                .find(|p| p.code_sha256 == *hash)
                .map(|p| p.person.clone())
                .unwrap_or_default();
            plan.complete_links.push(CompleteLinkItem {
                discord_user: event.user_id.clone(),
                person,
                code_sha256: hash.clone(),
                user_name: event
                    .user
                    .as_ref()
                    .map(|user| user.user_name.clone())
                    .unwrap_or_default(),
                display_name: event
                    .user
                    .as_ref()
                    .and_then(|user| user.display_name.clone())
                    .unwrap_or_default(),
            });
        }
        "unlink" if guild.links.iter().any(|l| l.discord_user == event.user_id) => {
            plan.unlinks.push(event.user_id.clone());
        }
        _ => {}
    }
    plan
}

/// A Tilo `block-persona`: the mirrored messages of `discord_user` are removed.
#[must_use]
pub fn plan_block(guild: &Guild, discord_user: &str) -> InboundPlan {
    InboundPlan {
        deletes: guild
            .map
            .iter()
            .filter(|m| {
                m.kind == MappedKind::Message
                    && m.origin == Origin::Discord
                    && m.persona.as_deref() == Some(discord_user)
            })
            .map(|m| m.tilo.clone())
            .collect(),
        ..InboundPlan::default()
    }
}

#[cfg(test)]
mod tests;
