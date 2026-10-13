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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
            | Self::FetchAsset(_) => None,
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
    /// A unicode emoji.
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
#[cfg_attr(feature = "resource", derive(JsonSchema))]
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

/// Copy a file from Discord's CDN into the room's content store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct FetchAssetOp {
    /// An `https://cdn.discordapp.com/…` or `https://media.discordapp.net/…` URL; anything else is refused.
    pub url: String,
    pub max_bytes: u64,
    /// The Chat attachment this becomes, `blob` filled in by the result.
    #[serde(default)]
    pub attachment: Option<InAttachment>,
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
/// `/blob`, `/attachment`, `/tilo`.
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
    Asset {
        blob: String,
        size: u64,
        content_type: String,
        #[serde(default)]
        attachment: Option<InAttachment>,
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
