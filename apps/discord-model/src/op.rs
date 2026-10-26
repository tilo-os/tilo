//! Ops the connector's outbox queues and the worker performs, and what each
//! one settles with. Every id is a Discord snowflake as a string, except
//! `tilo`, which carries the Tilo id the op is about so the bot can map the
//! result without remembering it between steps.

use serde::{Deserialize, Serialize};
#[cfg(feature = "resource")]
use ui::tilo_resource::{self, JsonSchema};

/// One outbox entry: the op, plus the content-store blobs the host must
/// publish (an author's avatar, served at a public URL) or hand to the
/// worker (an uploaded attachment) before the worker performs it. The worker
/// never reads a content store itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct Queued {
    pub op: DiscordOp,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attach: Vec<String>,
}

#[cfg(feature = "resource")]
tilo_resource::json_model!(Queued);

/// On the wire an object tagged by `op`, its op's fields beside the tag, as
/// serde's internally tagged enums are. It is read by hand rather than
/// derived: a derived internally tagged enum buffers the whole op and reads
/// it again from the buffer, a second reader compiled for every op type,
/// while this one reads the object once and each op from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DiscordOp {
    Send(SendOp),
    Edit(EditOp),
    Delete(DeleteOp),
    React(ReactOp),
    Unreact(ReactOp),
    StartThread(StartThreadOp),
    CreateWebhook(CreateWebhookOp),
    ApplyChannel(ChannelSpec),
    DeleteChannel(DeleteChannelOp),
    ApplyRole(RoleSpec),
    DeleteRole(DeleteRoleOp),
    SetMemberRoles(MemberRolesOp),
    Kick(MemberOp),
    Ban(MemberOp),
    Unban(MemberOp),
    SetOverwrite(OverwriteOp),
    FetchAsset(FetchAssetOp),
    AnswerInteraction(AnswerInteractionOp),
    ApplyEmoji(EmojiSpec),
    DeleteEmoji(DeleteEmojiOp),
}

impl<'de> Deserialize<'de> for DiscordOp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        fn op<T: serde::de::DeserializeOwned, E: Error>(
            fields: serde_json::Map<String, serde_json::Value>,
        ) -> Result<T, E> {
            serde_json::from_value(serde_json::Value::Object(fields)).map_err(E::custom)
        }
        let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let tag = match fields.remove("op") {
            Some(serde_json::Value::String(tag)) => tag,
            Some(_) => return Err(D::Error::custom("op is not a string")),
            None => return Err(D::Error::missing_field("op")),
        };
        Ok(match tag.as_str() {
            "send" => Self::Send(op(fields)?),
            "edit" => Self::Edit(op(fields)?),
            "delete" => Self::Delete(op(fields)?),
            "react" => Self::React(op(fields)?),
            "unreact" => Self::Unreact(op(fields)?),
            "start-thread" => Self::StartThread(op(fields)?),
            "create-webhook" => Self::CreateWebhook(op(fields)?),
            "apply-channel" => Self::ApplyChannel(op(fields)?),
            "delete-channel" => Self::DeleteChannel(op(fields)?),
            "apply-role" => Self::ApplyRole(op(fields)?),
            "delete-role" => Self::DeleteRole(op(fields)?),
            "set-member-roles" => Self::SetMemberRoles(op(fields)?),
            "kick" => Self::Kick(op(fields)?),
            "ban" => Self::Ban(op(fields)?),
            "unban" => Self::Unban(op(fields)?),
            "set-overwrite" => Self::SetOverwrite(op(fields)?),
            "fetch-asset" => Self::FetchAsset(op(fields)?),
            "answer-interaction" => Self::AnswerInteraction(op(fields)?),
            "apply-emoji" => Self::ApplyEmoji(op(fields)?),
            "delete-emoji" => Self::DeleteEmoji(op(fields)?),
            other => {
                return Err(D::Error::unknown_variant(
                    other,
                    &[
                        "send",
                        "edit",
                        "delete",
                        "react",
                        "unreact",
                        "start-thread",
                        "create-webhook",
                        "apply-channel",
                        "delete-channel",
                        "apply-role",
                        "delete-role",
                        "set-member-roles",
                        "kick",
                        "ban",
                        "unban",
                        "set-overwrite",
                        "fetch-asset",
                        "answer-interaction",
                        "apply-emoji",
                        "delete-emoji",
                    ],
                ));
            }
        })
    }
}

/// The value of an absent outbox slot only; no verb queues it on purpose.
impl Default for DiscordOp {
    fn default() -> Self {
        Self::Delete(DeleteOp::default())
    }
}

impl DiscordOp {
    /// The channel the op acts in, if it acts in one.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        match self {
            Self::Send(op) => Some(&op.channel),
            Self::Edit(op) => Some(&op.channel),
            Self::Delete(op) => Some(&op.channel),
            Self::React(op) | Self::Unreact(op) => Some(&op.channel),
            Self::StartThread(op) => Some(&op.channel),
            Self::CreateWebhook(op) => Some(&op.channel),
            Self::ApplyChannel(spec) => spec.id.as_deref(),
            Self::DeleteChannel(op) => Some(&op.channel),
            Self::SetOverwrite(op) => Some(&op.channel),
            Self::ApplyRole(_)
            | Self::DeleteRole(_)
            | Self::SetMemberRoles(_)
            | Self::Kick(_)
            | Self::Ban(_)
            | Self::Unban(_)
            | Self::FetchAsset(_)
            | Self::AnswerInteraction(_)
            | Self::ApplyEmoji(_)
            | Self::DeleteEmoji(_) => None,
        }
    }

    /// Whether the op changes the server rather than messages in one
    /// channel: its layout, roles, permissions or members.
    #[must_use]
    pub const fn reaches_server(&self) -> bool {
        matches!(
            self,
            Self::ApplyChannel(_)
                | Self::DeleteChannel(_)
                | Self::SetOverwrite(_)
                | Self::ApplyRole(_)
                | Self::DeleteRole(_)
                | Self::SetMemberRoles(_)
                | Self::Kick(_)
                | Self::Ban(_)
                | Self::Unban(_)
                | Self::ApplyEmoji(_)
                | Self::DeleteEmoji(_)
        )
    }
}

/// A webhook message under someone else's name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct SendOp {
    pub channel: String,
    /// The channel's webhook id; its token is in the host's secret store.
    pub webhook: String,
    /// Post into this thread of the channel.
    #[serde(default)]
    pub thread: Option<String>,
    pub username: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// The content-store blob behind `avatar_url`, which the `send` verb
    /// names in `Queued::publish` so the host serves it before Discord
    /// fetches it.
    #[serde(default)]
    pub avatar_blob: Option<String>,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<OutAttachment>,
    /// When the entry was queued: the duplicate check looks only after it.
    pub created_ms: i64,
    /// The Tilo message this posts.
    pub tilo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct OutAttachment {
    pub blob: String,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct EditOp {
    pub channel: String,
    pub webhook: String,
    pub message: String,
    #[serde(default)]
    pub thread: Option<String>,
    pub content: String,
}

/// Delete a message: through its webhook when the bridge sent it, as the bot
/// user otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct DeleteOp {
    pub channel: String,
    #[serde(default)]
    pub webhook: Option<String>,
    pub message: String,
    #[serde(default)]
    pub thread: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct ReactOp {
    pub channel: String,
    pub message: String,
    /// A unicode emoji, or one of the guild's custom emoji as `name:id`,
    /// as Discord's reaction routes name them.
    pub emoji: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct StartThreadOp {
    pub channel: String,
    /// The Discord message the thread grows from.
    pub message: String,
    pub name: String,
    /// The Tilo thread root.
    pub tilo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct CreateWebhookOp {
    pub channel: String,
    /// The Tilo channel.
    pub tilo: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema, tilo_resource::Model))]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    #[default]
    Text,
    Voice,
    Category,
    Thread,
}

impl ChannelKind {
    /// Discord's channel type number.
    #[must_use]
    pub const fn discord_type(self) -> u8 {
        match self {
            Self::Text => 0,
            Self::Voice => 2,
            Self::Category => 4,
            Self::Thread => 11,
        }
    }

    /// The kind of Discord channel type `kind`, threads of every flavour as
    /// one; `None` for kinds the bridge does not mirror (forums, stages).
    #[must_use]
    pub const fn from_discord(kind: u8) -> Option<Self> {
        match kind {
            0 | 5 => Some(Self::Text),
            2 => Some(Self::Voice),
            4 => Some(Self::Category),
            10..=12 => Some(Self::Thread),
            _ => None,
        }
    }
}

/// Create (`id: None`) or bring back in line (`id: Some`) one channel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct ChannelSpec {
    #[serde(default)]
    pub id: Option<String>,
    /// The Tilo category or channel.
    pub tilo: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub position: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct DeleteChannelOp {
    pub channel: String,
}

/// Create (`id: None`) or bring back in line one role the bridge manages.
/// `@everyone` has the guild's id as its role id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct RoleSpec {
    #[serde(default)]
    pub id: Option<String>,
    /// The Tilo role name (`viewer` … `owner`, or `everyone`).
    pub tilo: String,
    pub name: String,
    /// Permission bits as a decimal string, as Discord writes them.
    pub permissions: String,
    pub color: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct DeleteRoleOp {
    pub role: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MemberRolesOp {
    pub user: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MemberOp {
    pub user: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum OverwriteKind {
    #[default]
    Role,
    Member,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct OverwriteOp {
    pub channel: String,
    pub target: String,
    pub target_kind: OverwriteKind,
    pub allow: String,
    pub deny: String,
}

/// Edit the reply to a slash command to what came of it. The worker keeps
/// the interaction's token (good for fifteen minutes) and never hands it
/// out, so the op names only the interaction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct AnswerInteractionOp {
    pub interaction: String,
    pub content: String,
}

/// Upload (`id: None`) or rename (`id: Some`) one of the server's custom
/// emoji for a workspace emoji of Tilo's. Only emoji the bridge made itself
/// are ever renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct EmojiSpec {
    #[serde(default)]
    pub id: Option<String>,
    /// The name in Discord: 2 to 32 letters, digits and underscores.
    pub name: String,
    /// The picture's content-store blob, which the `apply-emoji` verb names
    /// in `Queued::attach` so the host hands its bytes to the worker. A
    /// rename carries it too: an emoji deleted in Discord meanwhile is
    /// uploaded again.
    #[serde(default)]
    pub picture: Option<String>,
    /// The workspace emoji's name in Tilo.
    pub tilo: String,
}

/// Delete one of the custom emoji the bridge made.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct DeleteEmojiOp {
    pub emoji: String,
    /// The workspace emoji's name in Tilo it stood for.
    pub tilo: String,
}

/// Copy a file from Discord's CDN into the room's content store, or answer
/// a small picture inline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct FetchAssetOp {
    /// An `https://cdn.discordapp.com/…` or `https://media.discordapp.net/…` URL, or a path on the worker's own CDN (`avatars/<user>/<hash>.png?size=64`); anything else is refused.
    pub url: String,
    pub max_bytes: u64,
    /// The Chat attachment this becomes, `blob` filled in by the result.
    #[serde(default)]
    pub attachment: Option<InAttachment>,
    /// Answer the picture as a `data:` URL (the result's `data_url`)
    /// instead of keeping it in the content store: one a record holds.
    #[serde(default)]
    pub inline: bool,
}

/// A Chat attachment as `chat.post` takes it (`AttachmentUpdate`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct InAttachment {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub blob: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub size: u64,
}

/// What an op settled with. Pointers the bridge program uses:
/// `/message_id`, `/thread_id`, `/webhook_id`, `/channel_id`, `/role_id`,
/// `/emoji_id`, `/problem`, `/blob`, `/attachment`, `/tilo`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum DiscordResult {
    Message {
        message_id: String,
        tilo: String,
    },
    Thread {
        thread_id: String,
        tilo: String,
    },
    Webhook {
        webhook_id: String,
        tilo: String,
    },
    Channel {
        channel_id: String,
        tilo: String,
    },
    Role {
        role_id: String,
        tilo: String,
    },
    /// An emoji op: the emoji's id once Discord holds it (empty when it
    /// did not, or after a delete), or why the server would not take it
    /// (the bot may not create emoji there, the server has no free emoji
    /// slot), said so a person can do something about it.
    Emoji {
        #[serde(default)]
        emoji_id: String,
        tilo: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    Asset {
        blob: String,
        size: u64,
        content_type: String,
        #[serde(default)]
        attachment: Option<InAttachment>,
        /// The picture as a `data:` URL, for an inline fetch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_url: Option<String>,
    },
    #[default]
    Done,
}

#[cfg(feature = "resource")]
tilo_resource::json_model!(DiscordResult);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_send_has_op_tag_and_host_fields() {
        let queued = Queued {
            op: DiscordOp::Send(SendOp {
                channel: "11".into(),
                webhook: "22".into(),
                thread: None,
                username: "Ada".into(),
                avatar_url: Some("https://rooms.example/avatars/ab".into()),
                avatar_blob: Some("ab".into()),
                content: "hi".into(),
                attachments: vec![OutAttachment {
                    blob: "cd".into(),
                    name: "a.png".into(),
                    size: 3,
                }],
                created_ms: 5,
                tilo: "m1".into(),
            }),
            publish: vec!["ab".into()],
            attach: vec!["cd".into()],
        };
        let value = serde_json::to_value(&queued).unwrap();
        assert_eq!(value["op"]["op"], json!("send"));
        assert_eq!(value["op"]["channel"], json!("11"));
        assert_eq!(value["publish"], json!(["ab"]));
        assert_eq!(value["attach"], json!(["cd"]));
        let back: Queued = serde_json::from_value(value).unwrap();
        assert_eq!(back, queued);
    }

    /// Every op reads back from the form it is written in, from text as
    /// well as from a value, with the tag anywhere in the object; an op
    /// without a tag, or with one no op has, is refused.
    #[test]
    fn every_op_reads_back_from_its_tagged_form() {
        let ops = [
            DiscordOp::Delete(DeleteOp::default()),
            DiscordOp::Kick(MemberOp::default()),
            DiscordOp::Ban(MemberOp::default()),
            DiscordOp::Unban(MemberOp::default()),
            DiscordOp::SetOverwrite(OverwriteOp {
                channel: "1".into(),
                target: "2".into(),
                target_kind: OverwriteKind::Role,
                allow: "0".into(),
                deny: "2048".into(),
            }),
        ];
        for op in ops {
            let text = serde_json::to_string(&op).unwrap();
            assert_eq!(
                serde_json::from_str::<DiscordOp>(&text).unwrap(),
                op,
                "{text}"
            );
            let value = serde_json::to_value(&op).unwrap();
            assert_eq!(serde_json::from_value::<DiscordOp>(value).unwrap(), op);
        }
        let tag_last = json!({"channel": "1", "target": "2", "target_kind": "role",
            "allow": "0", "deny": "2048", "op": "set-overwrite"});
        assert!(matches!(
            serde_json::from_value::<DiscordOp>(tag_last).unwrap(),
            DiscordOp::SetOverwrite(_)
        ));
        assert!(serde_json::from_value::<DiscordOp>(json!({"channel": "1"})).is_err());
        assert!(serde_json::from_value::<DiscordOp>(json!({"op": "dance"})).is_err());
    }

    #[test]
    fn results_are_tagged_and_default_to_done() {
        let result = DiscordResult::Message {
            message_id: "9".into(),
            tilo: "m1".into(),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(
            value,
            json!({"result": "message", "message_id": "9", "tilo": "m1"})
        );
        assert_eq!(DiscordResult::default(), DiscordResult::Done);
    }

    #[test]
    fn op_names_the_channel_it_acts_in() {
        let op = DiscordOp::SetOverwrite(OverwriteOp {
            channel: "1".into(),
            target: "2".into(),
            target_kind: OverwriteKind::Role,
            allow: "0".into(),
            deny: "2048".into(),
        });
        assert_eq!(op.channel(), Some("1"));
        assert_eq!(DiscordOp::Kick(MemberOp::default()).channel(), None);
    }

    #[test]
    fn channel_kinds_map_discord_type_numbers() {
        assert_eq!(ChannelKind::from_discord(0), Some(ChannelKind::Text));
        assert_eq!(ChannelKind::from_discord(11), Some(ChannelKind::Thread));
        assert_eq!(
            ChannelKind::from_discord(15),
            None,
            "forums are not mirrored"
        );
        assert_eq!(ChannelKind::Category.discord_type(), 4);
    }
}
