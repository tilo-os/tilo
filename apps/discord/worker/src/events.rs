//! Gateway payloads to the connector's trigger-source events
//! (discord-bridge.md §2, "Trigger sources"). Pure: the gateway feeds the
//! guild's state in and sends what comes out.
//!
//! A message in a Discord thread is reported in the thread's parent text
//! channel with `thread` naming the thread, the way the ops address it
//! (`channel` plus `thread`), so the bridge maps channels only.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::{BTreeMap, HashMap};

use discord_wire::event::{
    AttachmentInfo, Author, ChannelInfo, Command, LayoutChanged, MemberChanged, MessageCreated,
    MessageDeleted, MessageUpdated, OverwriteInfo, ReactionChanged, RoleInfo, ThreadRef, UserName,
    code_sha256, event_id, source,
};
use discord_wire::op::ChannelKind;
use serde::Serialize;
use serde_json::{Map, Value};

pub use discord_wire::{DISCORD_EPOCH_MS, snowflake_ms};

/// One event for the host's log.
#[derive(Clone, Debug, PartialEq)]
pub struct Emit {
    pub source: &'static str,
    pub id: String,
    pub at_ms: i64,
    pub payload: Map<String, Value>,
}

impl Emit {
    fn new<T: Serialize>(source: &'static str, id: String, at_ms: i64, payload: &T) -> Self {
        let payload = match serde_json::to_value(payload) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        Self {
            source,
            id,
            at_ms,
            payload,
        }
    }
}

/// What the worker knows of the bridged guild.
#[derive(Clone, Debug, Default)]
pub struct Guild {
    pub id: String,
    pub bot_user: String,
    /// Where avatar URLs point (`https://cdn.discordapp.com`).
    pub cdn_base: String,
    pub channels: BTreeMap<String, ChannelInfo>,
    pub roles: BTreeMap<String, RoleInfo>,
    /// Thread → its parent text channel.
    pub threads: HashMap<String, String>,
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn opt_text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// `2026-10-12T10:30:00.123000+00:00` (Discord writes UTC) as Unix ms.
#[must_use]
pub fn rfc3339_ms(stamp: &str) -> Option<i64> {
    let (date, rest) = stamp.split_once('T')?;
    let mut date = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let clock = rest.get(..8)?;
    let mut clock = clock.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    let millis = rest
        .get(8..)
        .and_then(|r| r.strip_prefix('.'))
        .map_or(0, |frac| {
            let digits: String = frac
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();
            format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
        });
    let days = days_from_civil(year, month, day)?;
    days.checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?
        .checked_mul(1000)?
        .checked_add(millis)
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = year.div_euclid(400);
    let yoe = year.checked_sub(era.checked_mul(400)?)?;
    let shifted = if month > 2 {
        month.checked_sub(3)?
    } else {
        month.checked_add(9)?
    };
    let doy = shifted
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day)?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe / 4)?
        .checked_sub(yoe / 100)?
        .checked_add(doy)?;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

/// The name a guild shows for a user: their nick, else their global name,
/// else their username.
#[must_use]
pub fn display_name(user: &Value, member: &Value) -> String {
    opt_text(&member["nick"])
        .or_else(|| opt_text(&user["global_name"]))
        .or_else(|| opt_text(&user["username"]))
        .unwrap_or_default()
}

/// A unicode emoji as is, a custom one as `:name:`.
#[must_use]
pub fn emoji_name(emoji: &Value) -> String {
    let name = text(&emoji["name"]);
    if emoji["id"].is_null() {
        name
    } else {
        format!(":{name}:")
    }
}

impl Guild {
    /// The guild as `GUILD_CREATE` describes it.
    #[must_use]
    pub fn from_create(data: &Value, bot_user: &str, cdn_base: &str) -> Self {
        let mut guild = Self {
            id: text(&data["id"]),
            bot_user: bot_user.to_owned(),
            cdn_base: cdn_base.trim_end_matches('/').to_owned(),
            ..Self::default()
        };
        for channel in data["channels"].as_array().into_iter().flatten() {
            guild.put_channel(channel);
        }
        for thread in data["threads"].as_array().into_iter().flatten() {
            guild.put_channel(thread);
        }
        for role in data["roles"].as_array().into_iter().flatten() {
            guild.put_role(role);
        }
        guild
    }

    fn put_channel(&mut self, channel: &Value) {
        let id = text(&channel["id"]);
        let Some(kind) = channel["type"]
            .as_u64()
            .and_then(|k| u8::try_from(k).ok())
            .and_then(ChannelKind::from_discord)
        else {
            return;
        };
        if kind == ChannelKind::Thread {
            self.threads.insert(id, text(&channel["parent_id"]));
            return;
        }
        let overwrites = channel["permission_overwrites"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| OverwriteInfo {
                id: text(&o["id"]),
                kind: o["type"]
                    .as_u64()
                    .and_then(|k| u8::try_from(k).ok())
                    .unwrap_or(0),
                allow: text(&o["allow"]),
                deny: text(&o["deny"]),
            })
            .collect();
        self.channels.insert(
            id.clone(),
            ChannelInfo {
                id,
                kind,
                name: text(&channel["name"]),
                parent: opt_text(&channel["parent_id"]),
                position: channel["position"].as_i64().unwrap_or(0),
                topic: text(&channel["topic"]),
                overwrites,
            },
        );
    }

    fn put_role(&mut self, role: &Value) {
        let id = text(&role["id"]);
        self.roles.insert(
            id.clone(),
            RoleInfo {
                id,
                name: text(&role["name"]),
                permissions: text(&role["permissions"]),
                color: role["color"]
                    .as_u64()
                    .and_then(|c| u32::try_from(c).ok())
                    .unwrap_or(0),
                position: role["position"].as_i64().unwrap_or(0),
                managed: role["managed"].as_bool().unwrap_or(false),
            },
        );
    }

    /// Apply a channel, thread or role event. True when the layout (the
    /// channels and roles, not threads) changed.
    pub fn apply(&mut self, kind: &str, data: &Value) -> bool {
        match kind {
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
                self.put_channel(data);
                true
            }
            "CHANNEL_DELETE" => {
                self.channels
                    .remove(data["id"].as_str().unwrap_or_default());
                true
            }
            "THREAD_CREATE" | "THREAD_UPDATE" => {
                self.put_channel(data);
                false
            }
            "THREAD_DELETE" => {
                self.threads.remove(data["id"].as_str().unwrap_or_default());
                false
            }
            "THREAD_LIST_SYNC" => {
                for thread in data["threads"].as_array().into_iter().flatten() {
                    self.put_channel(thread);
                }
                false
            }
            "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" => {
                self.put_role(&data["role"]);
                true
            }
            "GUILD_ROLE_DELETE" => {
                self.roles
                    .remove(data["role_id"].as_str().unwrap_or_default());
                true
            }
            _ => false,
        }
    }

    /// `channel` as the bridge addresses it: a thread's parent, and the
    /// thread itself when `channel` is one.
    #[must_use]
    pub fn place(&self, channel: &str) -> (String, Option<ThreadRef>) {
        match self.threads.get(channel) {
            Some(parent) => (
                parent.clone(),
                Some(ThreadRef {
                    id: channel.to_owned(),
                    parent: parent.clone(),
                }),
            ),
            None => (channel.to_owned(), None),
        }
    }

    fn avatar_url(&self, user: &Value) -> Option<String> {
        let hash = opt_text(&user["avatar"])?;
        let id = text(&user["id"]);
        Some(format!(
            "{}/avatars/{id}/{hash}.png?size=128",
            self.cdn_base
        ))
    }

    fn author(&self, user: &Value, member: &Value, webhook_id: Option<String>) -> Author {
        Author {
            id: text(&user["id"]),
            name: display_name(user, member),
            avatar: opt_text(&user["avatar"]),
            avatar_url: self.avatar_url(user),
            bot: user["bot"].as_bool().unwrap_or(false),
            webhook_id,
        }
    }

    /// The layout event for gateway sequence `sequence`.
    #[must_use]
    pub fn layout(&self, sequence: u64) -> Emit {
        let layout = LayoutChanged {
            guild_id: self.id.clone(),
            bot_user: self.bot_user.clone(),
            channels: self.channels.values().cloned().collect(),
            roles: self.roles.values().cloned().collect(),
        };
        Emit::new(
            source::LAYOUT_CHANGED,
            event_id::sequenced(source::LAYOUT_CHANGED, sequence),
            tilo_clock::unix_now_ms_i64(),
            &layout,
        )
    }

    /// `MESSAGE_CREATE`, or a message read back during a backfill. System
    /// messages (joins, pins, thread notices) are not mirrored.
    #[must_use]
    pub fn message_created(&self, data: &Value) -> Option<Emit> {
        let kind = data["type"].as_u64().unwrap_or(0);
        if kind != 0 && kind != 19 {
            return None;
        }
        let id = opt_text(&data["id"])?;
        let (channel_id, thread) = self.place(data["channel_id"].as_str().unwrap_or_default());
        let referenced = &data["referenced_message"];
        let reply_to = if kind == 19 {
            opt_text(&data["message_reference"]["message_id"])
        } else {
            None
        };
        let (reply_author, reply_text) = if reply_to.is_some() && referenced.is_object() {
            (
                Some(display_name(&referenced["author"], &Value::Null)),
                Some(text(&referenced["content"])),
            )
        } else {
            (None, None)
        };
        let attachments = data["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| AttachmentInfo {
                name: text(&a["filename"]),
                size: a["size"].as_u64().unwrap_or(0),
                content_type: text(&a["content_type"]),
                url: text(&a["url"]),
                width: a["width"]
                    .as_u64()
                    .and_then(|w| u32::try_from(w).ok())
                    .unwrap_or(0),
                height: a["height"]
                    .as_u64()
                    .and_then(|h| u32::try_from(h).ok())
                    .unwrap_or(0),
            })
            .collect();
        let event = MessageCreated {
            guild_id: self.id.clone(),
            channel_id,
            message_id: id.clone(),
            author: self.author(
                &data["author"],
                &data["member"],
                opt_text(&data["webhook_id"]),
            ),
            content: text(&data["content"]),
            reply_to,
            reply_author,
            reply_text,
            thread,
            attachments,
        };
        Some(Emit::new(
            source::MESSAGE_CREATED,
            event_id::message_created(&id),
            snowflake_ms(&id),
            &event,
        ))
    }

    /// `MESSAGE_UPDATE` that changed the text; embed-only updates carry no
    /// content or edit time and are skipped.
    #[must_use]
    pub fn message_updated(&self, data: &Value) -> Option<Emit> {
        let id = opt_text(&data["id"])?;
        let content = data["content"].as_str()?;
        let edited_ms = rfc3339_ms(data["edited_timestamp"].as_str()?)?;
        let (channel_id, _) = self.place(data["channel_id"].as_str().unwrap_or_default());
        let event = MessageUpdated {
            guild_id: self.id.clone(),
            channel_id,
            message_id: id.clone(),
            author: self.author(
                &data["author"],
                &data["member"],
                opt_text(&data["webhook_id"]),
            ),
            content: content.to_owned(),
            edited_ms,
        };
        Some(Emit::new(
            source::MESSAGE_UPDATED,
            event_id::message_updated(&id, edited_ms),
            edited_ms,
            &event,
        ))
    }

    /// `MESSAGE_DELETE` of `id` in `channel`.
    #[must_use]
    pub fn message_deleted(&self, id: &str, channel: &str, executor: Option<String>) -> Emit {
        let (channel_id, _) = self.place(channel);
        let event = MessageDeleted {
            guild_id: self.id.clone(),
            channel_id,
            message_id: id.to_owned(),
            executor,
        };
        Emit::new(
            source::MESSAGE_DELETED,
            event_id::message_deleted(id),
            tilo_clock::unix_now_ms_i64(),
            &event,
        )
    }

    /// `MESSAGE_REACTION_ADD` / `_REMOVE`.
    #[must_use]
    pub fn reaction(&self, data: &Value, added: bool, sequence: u64) -> Option<Emit> {
        let message = opt_text(&data["message_id"])?;
        let user = opt_text(&data["user_id"])?;
        let emoji = emoji_name(&data["emoji"]);
        let member = &data["member"];
        let (channel_id, _) = self.place(data["channel_id"].as_str().unwrap_or_default());
        let event = ReactionChanged {
            guild_id: self.id.clone(),
            channel_id,
            message_id: message.clone(),
            user_id: user.clone(),
            user_name: display_name(&member["user"], member),
            user_avatar_url: self.avatar_url(&member["user"]),
            emoji: emoji.clone(),
        };
        Some(Emit::new(
            if added {
                source::REACTION_ADDED
            } else {
                source::REACTION_REMOVED
            },
            event_id::reaction(&message, &user, &emoji, added, sequence),
            tilo_clock::unix_now_ms_i64(),
            &event,
        ))
    }

    /// Member role changes, leaves and kicks, bans and unbans.
    #[must_use]
    pub fn member_changed(&self, kind: &str, data: &Value, sequence: u64) -> Option<Emit> {
        let user = opt_text(&data["user"]["id"])?;
        let roles = data["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_owned))
            .collect();
        let event = MemberChanged {
            guild_id: self.id.clone(),
            user_id: user,
            roles,
            banned: kind == "GUILD_BAN_ADD",
            left: kind == "GUILD_MEMBER_REMOVE",
            user: UserName::of_member(data),
        };
        Some(Emit::new(
            source::MEMBER_CHANGED,
            event_id::sequenced(source::MEMBER_CHANGED, sequence),
            tilo_clock::unix_now_ms_i64(),
            &event,
        ))
    }

    /// A `/link` or `/unlink` interaction. The code is hashed here, so it
    /// never reaches the host.
    #[must_use]
    pub fn command(&self, data: &Value) -> Option<Emit> {
        if data["type"].as_u64() != Some(2) {
            return None;
        }
        let name = data["data"]["name"].as_str()?;
        if name != "link" && name != "unlink" {
            return None;
        }
        let interaction = opt_text(&data["id"])?;
        let user = opt_text(&data["member"]["user"]["id"])?;
        let code_sha256 = (name == "link")
            .then(|| {
                data["data"]["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|o| o["name"] == "code")
                    .and_then(|o| o["value"].as_str())
                    .map(code_sha256)
            })
            .flatten();
        let event = Command {
            guild_id: self.id.clone(),
            user_id: user,
            name: name.to_owned(),
            code_sha256,
            user: UserName::of_member(&data["member"]),
        };
        Some(Emit::new(
            source::COMMAND,
            event_id::command(&interaction),
            snowflake_ms(&interaction),
            &event,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn guild() -> Guild {
        Guild::from_create(
            &json!({
                "id": "1000",
                "channels": [
                    {"id": "1100", "type": 4, "name": "chat", "position": 1},
                    {"id": "1101", "type": 0, "name": "general", "parent_id": "1100", "position": 2, "topic": null,
                     "permission_overwrites": [{"id": "1000", "type": 0, "allow": "0", "deny": "2048"}]},
                    {"id": "1104", "type": 15, "name": "forum", "position": 5},
                ],
                "threads": [{"id": "1500", "type": 11, "parent_id": "1101", "name": "t"}],
                "roles": [{"id": "1000", "name": "@everyone", "permissions": "3072", "color": 0, "position": 0, "managed": false}],
            }),
            "2000",
            "https://cdn.discordapp.com/",
        )
    }

    #[test]
    fn guild_create_builds_the_layout_without_forums_or_threads() {
        let guild = guild();
        assert_eq!(guild.channels.len(), 2, "forums are not mirrored");
        assert_eq!(guild.threads.get("1500").map(String::as_str), Some("1101"));
        let layout = guild.layout(7);
        assert_eq!(layout.id, "layout-changed:7");
        assert_eq!(layout.payload["bot_user"], "2000");
        assert_eq!(
            layout.payload["channels"][1]["overwrites"][0]["deny"],
            "2048"
        );
    }

    #[test]
    fn a_thread_message_is_placed_in_its_parent_with_the_thread() {
        let guild = guild();
        let emit = guild
            .message_created(&json!({
                "id": "175928847299117063", "type": 0, "channel_id": "1500", "content": "hi",
                "author": {"id": "7", "username": "ada", "global_name": "Ada L", "avatar": "abc"},
                "member": {"nick": "Ada"}, "attachments": [
                    {"filename": "a.png", "size": 3, "content_type": "image/png", "url": "https://cdn.discordapp.com/a.png", "width": 2, "height": 1}
                ],
            }))
            .unwrap();
        assert_eq!(emit.source, "message-created");
        assert_eq!(emit.id, "175928847299117063");
        assert_eq!(emit.at_ms, 1_462_015_105_796, "snowflake time");
        let event: MessageCreated = serde_json::from_value(Value::Object(emit.payload)).unwrap();
        assert_eq!(event.channel_id, "1101");
        assert_eq!(event.thread.unwrap().id, "1500");
        assert_eq!(event.author.name, "Ada");
        assert_eq!(
            event.author.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/avatars/7/abc.png?size=128")
        );
        assert_eq!(event.attachments[0].width, 2);
    }

    #[test]
    fn replies_carry_the_quoted_message_and_system_messages_are_skipped() {
        let guild = guild();
        let emit = guild
            .message_created(&json!({
                "id": "9", "type": 19, "channel_id": "1101", "content": "yes",
                "author": {"id": "7", "username": "ada"},
                "message_reference": {"message_id": "8"},
                "referenced_message": {"id": "8", "content": "really?", "author": {"id": "6", "username": "bob"}},
            }))
            .unwrap();
        assert_eq!(emit.payload["reply_to"], "8");
        assert_eq!(emit.payload["reply_author"], "bob");
        assert_eq!(emit.payload["reply_text"], "really?");
        assert!(
            guild
                .message_created(&json!({"id": "10", "type": 7, "channel_id": "1101"}))
                .is_none()
        );
    }

    #[test]
    fn edits_are_keyed_by_their_edit_time() {
        let emit = guild()
            .message_updated(&json!({
                "id": "9", "channel_id": "1101", "content": "fixed",
                "edited_timestamp": "2026-10-12T10:30:00.123000+00:00", "author": {"id": "7", "username": "ada"},
            }))
            .unwrap();
        assert_eq!(emit.id, "9:1791801000123");
        assert!(
            guild()
                .message_updated(&json!({"id": "9", "channel_id": "1101", "embeds": []}))
                .is_none()
        );
    }

    #[test]
    fn reactions_and_commands() {
        let guild = guild();
        let emit = guild
            .reaction(
                &json!({"message_id": "9", "user_id": "7", "channel_id": "1101", "emoji": {"id": "55", "name": "tilo_hello"},
                        "member": {"nick": "Ada", "user": {"id": "7", "username": "ada"}}}),
                true,
                42,
            )
            .unwrap();
        assert_eq!(emit.id, "9:7::tilo_hello::add:42");
        assert_eq!(emit.payload["user_name"], "Ada");
        let command = guild
            .command(&json!({
                "id": "77", "type": 2, "member": {"user": {"id": "7"}},
                "data": {"name": "link", "options": [{"name": "code", "value": "abcd efgh ijkl"}]},
            }))
            .unwrap();
        assert_eq!(command.id, "77");
        assert_eq!(command.payload["code_sha256"], code_sha256("ABCDEFGHIJKL"));
        assert!(
            !serde_json::to_string(&command.payload)
                .unwrap()
                .contains("abcd"),
            "the code itself never leaves the worker"
        );
    }

    #[test]
    fn layout_events_follow_channel_and_role_changes() {
        let mut guild = guild();
        assert!(guild.apply(
            "CHANNEL_UPDATE",
            &json!({"id": "1101", "type": 0, "name": "renamed", "position": 2})
        ));
        assert_eq!(guild.channels["1101"].name, "renamed");
        assert!(guild.apply("CHANNEL_DELETE", &json!({"id": "1101"})));
        assert!(!guild.channels.contains_key("1101"));
        assert!(!guild.apply(
            "THREAD_CREATE",
            &json!({"id": "1600", "type": 11, "parent_id": "1100"})
        ));
        assert!(guild.apply("GUILD_ROLE_DELETE", &json!({"role_id": "1000"})));
        assert!(guild.roles.is_empty());
    }

    #[test]
    fn timestamps_parse() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            rfc3339_ms("2026-10-12T10:30:00.5+00:00"),
            Some(1_791_801_000_500)
        );
        assert_eq!(rfc3339_ms("garbage"), None);
    }
}
