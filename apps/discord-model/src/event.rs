//! Payloads of the connector's trigger sources (discord-bridge.md §2,
//! "Trigger sources") and their event ids.

use serde::{Deserialize, Serialize};
#[cfg(feature = "resource")]
use ui::tilo_resource::{self, JsonSchema};

use crate::op::ChannelKind;

/// The connector's declared source names.
pub mod source {
    pub const MESSAGE_CREATED: &str = "message-created";
    pub const MESSAGE_UPDATED: &str = "message-updated";
    pub const MESSAGE_DELETED: &str = "message-deleted";
    pub const REACTION_ADDED: &str = "reaction-added";
    pub const REACTION_REMOVED: &str = "reaction-removed";
    pub const LAYOUT_CHANGED: &str = "layout-changed";
    pub const MEMBER_CHANGED: &str = "member-changed";
    pub const COMMAND: &str = "command";
    /// Someone signed in with Discord on the room's host (OAuth 2): the
    /// host appends it, not the worker.
    pub const AUTHORIZED: &str = "authorized";
    /// Whether, and where, the room's host offers signing in with Discord:
    /// the host appends it when it starts, not the worker.
    pub const SIGN_IN: &str = "sign-in";
    pub const ALL: [&str; 10] = [
        MESSAGE_CREATED,
        MESSAGE_UPDATED,
        MESSAGE_DELETED,
        REACTION_ADDED,
        REACTION_REMOVED,
        LAYOUT_CHANGED,
        MEMBER_CHANGED,
        COMMAND,
        AUTHORIZED,
        SIGN_IN,
    ];
}

/// Event ids: the service's identity of an event, which the host's event log
/// deduplicates by.
pub mod event_id {
    #[must_use]
    pub fn message_created(message: &str) -> String {
        message.to_owned()
    }
    #[must_use]
    pub fn message_updated(message: &str, edited_ms: i64) -> String {
        format!("{message}:{edited_ms}")
    }
    #[must_use]
    pub fn message_deleted(message: &str) -> String {
        format!("{message}:deleted")
    }
    #[must_use]
    pub fn reaction(message: &str, user: &str, emoji: &str, added: bool, sequence: u64) -> String {
        let what = if added { "add" } else { "remove" };
        format!("{message}:{user}:{emoji}:{what}:{sequence}")
    }
    #[must_use]
    pub fn sequenced(source: &str, sequence: u64) -> String {
        format!("{source}:{sequence}")
    }
    #[must_use]
    pub fn command(interaction: &str) -> String {
        interaction.to_owned()
    }
    #[must_use]
    pub fn authorized(link_check: &str) -> String {
        format!("authorized:{link_check}")
    }
    /// One event per page the host offers, so a restart with the same
    /// settings repeats nothing.
    #[must_use]
    pub fn sign_in(page: &str) -> String {
        let digest = super::sha256_hex(page.as_bytes());
        format!("sign-in:{}", digest.get(..16).unwrap_or(&digest))
    }
}

/// Discord's consent page for signing in to this room's host (OAuth 2's
/// authorization code flow, `identify` only), without the round trip's
/// `state`, which each start appends: the application's `client_id`, and
/// `redirect_uri`, the host's callback.
#[must_use]
pub fn sign_in_page(authorize_url: &str, client_id: &str, redirect_uri: &str) -> String {
    let query = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("scope", "identify"),
        ("redirect_uri", redirect_uri),
        ("prompt", "none"),
    ]
    .iter()
    .map(|(key, value)| format!("{key}={}", query_value(value)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{authorize_url}?{query}")
}

/// `text` percent-encoded for a query value.
#[must_use]
pub fn query_value(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// How long a link offer lasts: the button `/link` answers with, or the
/// return from Discord's consent page, is good for ten minutes. Discord
/// keeps an interaction's token for fifteen, so the reply can still say
/// what came of it afterwards.
pub const LINK_TTL_MS: i64 = 10 * 60 * 1000;

/// The proof of a link token: SHA-256 (lower-case hex) of the token. Who
/// holds the token hands its proof to `complete-link`, which the record
/// checks against the offer's [`link_check`]; the token itself never
/// reaches a record or the event log.
#[must_use]
pub fn link_proof(token: &str) -> String {
    sha256_hex(token.trim().as_bytes())
}

/// What a link offer is kept under: SHA-256 of its [`link_proof`]. Members
/// read it in the record, and nobody can work the proof out of it, so
/// nobody but the token's holder completes the link.
#[must_use]
pub fn link_check(proof: &str) -> String {
    sha256_hex(proof.as_bytes())
}

/// SHA-256 (lower-case hex) of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct Author {
    pub id: String,
    /// The member's display name in the guild (nick, else global name, else username).
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub bot: bool,
    #[serde(default)]
    pub webhook_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct AttachmentInfo {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub content_type: String,
    pub url: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

/// The thread a message was posted in: `id` is the thread channel, `parent`
/// the text channel it hangs off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct ThreadRef {
    pub id: String,
    pub parent: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MessageCreated {
    pub guild_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub author: Author,
    pub content: String,
    /// The message this one replies to (`message_reference`).
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The answered message's author name and text (`referenced_message`),
    /// for the quote a reply to a reply starts with in Tilo.
    #[serde(default)]
    pub reply_author: Option<String>,
    #[serde(default)]
    pub reply_text: Option<String>,
    #[serde(default)]
    pub thread: Option<ThreadRef>,
    #[serde(default)]
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MessageUpdated {
    pub guild_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub author: Author,
    pub content: String,
    pub edited_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MessageDeleted {
    pub guild_id: String,
    pub channel_id: String,
    pub message_id: String,
    /// Who deleted it, from the audit log, when Discord reports one.
    #[serde(default)]
    pub executor: Option<String>,
}

/// `reaction-added` and `reaction-removed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct ReactionChanged {
    pub guild_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub user_avatar_url: Option<String>,
    /// A unicode emoji, or a custom emoji's name as `:name:`.
    pub emoji: String,
    /// A custom emoji's id; none for a unicode one.
    #[serde(default)]
    pub emoji_id: Option<String>,
    /// Whether the custom emoji is animated.
    #[serde(default)]
    pub animated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema, tilo_resource::Model))]
pub struct OverwriteInfo {
    #[cfg_attr(feature = "resource", model(atom))]
    pub id: String,
    /// 0 role, 1 member, as Discord numbers them.
    pub kind: u8,
    #[cfg_attr(feature = "resource", model(atom))]
    pub allow: String,
    #[cfg_attr(feature = "resource", model(atom))]
    pub deny: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct ChannelInfo {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub position: i64,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub overwrites: Vec<OverwriteInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct RoleInfo {
    pub id: String,
    pub name: String,
    pub permissions: String,
    pub color: u32,
    pub position: i64,
    /// Owned by an integration (the bot's own role): never edited.
    pub managed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct LayoutChanged {
    pub guild_id: String,
    /// The bot user's id, for echo suppression.
    pub bot_user: String,
    pub channels: Vec<ChannelInfo>,
    pub roles: Vec<RoleInfo>,
    /// The guild's custom emoji, which a Tilo reaction of the same `:name:`
    /// is mirrored as.
    #[serde(default)]
    pub emojis: Vec<EmojiInfo>,
}

/// One of a guild's custom emoji.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct EmojiInfo {
    pub id: String,
    /// 2 to 32 letters, digits and underscores.
    pub name: String,
    #[serde(default)]
    pub animated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct MemberChanged {
    pub guild_id: String,
    pub user_id: String,
    pub roles: Vec<String>,
    #[serde(default)]
    pub banned: bool,
    /// Left or was kicked: Discord does not tell the two apart without the audit log.
    #[serde(default)]
    pub left: bool,
    /// The member's names, when the event carries them.
    #[serde(default)]
    pub user: Option<UserName>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct Command {
    pub guild_id: String,
    pub user_id: String,
    /// `link` or `unlink`.
    pub name: String,
    /// The interaction, whose reply the bridge edits to what came of it.
    #[serde(default)]
    pub interaction: String,
    /// For `/link`: [`link_check`] of the token in the reply's button. The
    /// worker made the token and keeps it out of the event log.
    #[serde(default)]
    pub link_check: Option<String>,
    /// Who ran it, as Discord names them.
    #[serde(default)]
    pub user: Option<UserName>,
    /// Their Discord avatar hash, if they set one.
    #[serde(default)]
    pub avatar: Option<String>,
}

/// What the room's host offers for signing in with Discord: the consent
/// page ([`sign_in_page`]), or empty when it offers none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct SignInOffered {
    #[serde(default)]
    pub page: String,
}

/// Someone signed in with Discord through the room's host (OAuth 2 with
/// the `identify` scope): the account Discord vouched for, the round
/// trip's state that names who started it, and the token the host handed
/// back to the browser that came back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct Authorized {
    pub user_id: String,
    #[serde(default)]
    pub user: Option<UserName>,
    #[serde(default)]
    pub avatar: Option<String>,
    /// SHA-256 (hex) of the OAuth 2 `state` the round trip came back with.
    pub state_sha256: String,
    /// [`link_check`] of the token the browser came back with.
    pub link_check: String,
}

/// A Discord user's names: their unique username, and the name they show
/// when it is another (their server nickname, else their global name).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "resource", derive(JsonSchema))]
pub struct UserName {
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl UserName {
    /// The names a member object (`{nick, user: {username, global_name}}`)
    /// gives, or `None` without a username.
    #[must_use]
    pub fn of_member(member: &serde_json::Value) -> Option<Self> {
        let user = member.get("user")?;
        let user_name = user.get("username")?.as_str()?.to_owned();
        let display_name = member
            .get("nick")
            .and_then(serde_json::Value::as_str)
            .or_else(|| user.get("global_name").and_then(serde_json::Value::as_str))
            .filter(|name| !name.is_empty() && *name != user_name)
            .map(str::to_owned);
        Some(Self {
            user_name,
            display_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_ids_follow_the_spec_table() {
        assert_eq!(event_id::message_created("5"), "5");
        assert_eq!(event_id::message_updated("5", 1_700), "5:1700");
        assert_eq!(event_id::message_deleted("5"), "5:deleted");
        assert_eq!(
            event_id::reaction("5", "7", "👍", true, 42),
            "5:7:👍:add:42"
        );
        assert_eq!(
            event_id::reaction("5", "7", "👍", false, 43),
            "5:7:👍:remove:43"
        );
        assert_eq!(event_id::sequenced("layout", 44), "layout:44");
        assert_eq!(event_id::command("99"), "99");
    }

    #[test]
    fn a_link_check_is_the_hash_of_the_proof() {
        assert_eq!(
            link_proof("ABC"),
            "b5d4045c3f466fa91fe2cc6abe79232a1a57cdf104f7a26e716e0a1e2789df78",
            "SHA-256 of ABC"
        );
        assert_eq!(
            link_check(&link_proof("ABC")),
            sha256_hex(link_proof("ABC").as_bytes())
        );
        assert_ne!(link_check(&link_proof("ABC")), link_proof("ABC"));
    }

    #[test]
    fn message_created_round_trips() {
        let event = MessageCreated {
            guild_id: "g".into(),
            channel_id: "c".into(),
            message_id: "m".into(),
            author: Author {
                id: "u".into(),
                name: "alice".into(),
                avatar: Some("hash".into()),
                avatar_url: Some("https://cdn.discordapp.com/avatars/u/hash.png".into()),
                bot: false,
                webhook_id: None,
            },
            content: "hello".into(),
            reply_to: None,
            reply_author: None,
            reply_text: None,
            thread: Some(ThreadRef {
                id: "c".into(),
                parent: "p".into(),
            }),
            attachments: vec![],
        };
        let value = serde_json::to_value(&event).unwrap();
        let back: MessageCreated = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }
}
