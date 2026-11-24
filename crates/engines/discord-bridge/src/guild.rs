//! The guild's state as the planners read it: one bridged Discord server, its channels, roles and
//! emoji, and the mapping between it and the workspace's conversations.
//!
//! The app keeps this as its resource and hands it to [`crate::plan`] as JSON on every event; the
//! types here are that JSON read back, with the lookups a plan makes.

use std::collections::BTreeMap;

use discord_wire::event::{ChannelInfo, EmojiInfo, OverwriteInfo, RoleInfo};
use discord_wire::op::ChannelKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A key an item stands under, as the app stores it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    #[must_use]
    pub fn from_raw(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An item that carries the key it stands under.
pub trait Identified {
    fn id(&self) -> Id;
}

/// Items keyed by the id they carry, in the order the app holds them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Keyed<T>(Vec<T>);

impl<T> Keyed<T> {
    #[must_use]
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push(&mut self, item: T) {
        self.0.push(item);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    #[must_use]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.0.iter_mut()
    }
}

impl<T> std::ops::Index<usize> for Keyed<T> {
    type Output = T;

    fn index(&self, at: usize) -> &T {
        &self.0[at]
    }
}

impl<T> std::ops::IndexMut<usize> for Keyed<T> {
    fn index_mut(&mut self, at: usize) -> &mut T {
        &mut self.0[at]
    }
}

impl<T: Identified> Keyed<T> {
    #[must_use]
    pub fn find(&self, id: &Id) -> Option<&T> {
        self.0.iter().find(|item| item.id() == *id)
    }

    #[must_use]
    pub fn find_mut(&mut self, id: &Id) -> Option<&mut T> {
        self.0.iter_mut().find(|item| item.id() == *id)
    }

    pub fn take(&mut self, id: &Id) -> Option<T> {
        let at = self.0.iter().position(|item| item.id() == *id)?;
        Some(self.0.remove(at))
    }
}

impl<T> FromIterator<T> for Keyed<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a, T> IntoIterator for &'a Keyed<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T> IntoIterator for Keyed<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// A set of words, in their order.
pub type Set<T> = std::collections::BTreeSet<T>;

/// What became of an op the app queued: `{"done": result}`, `{"failed": why}` or
/// `{"withdrawn": why}`, as the app writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Settled {
    Done(Value),
    Failed(Value),
    Withdrawn(Value),
}

impl Settled {
    /// Whether the service carried it out.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }

    /// What the service answered, when it carried it out.
    #[must_use]
    pub fn done_result(&self) -> Option<&Value> {
        match self {
            Self::Done(result) => Some(result),
            Self::Failed(_) | Self::Withdrawn(_) => None,
        }
    }
}

/// One op the app queued for outside: the op as the app encoded it (`{"send": {…}}`), and what
/// became of it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    /// The app's key for the entry, so a settled one is forgotten under it.
    pub key: String,
    pub op: Value,
    #[serde(default)]
    pub settled: Option<Settled>,
}

/// The app's outbox of ops, as it hands it to a plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Outbox<T>(Vec<T>);

impl Outbox<Queued> {
    /// The entries with the key the app holds each under.
    pub fn entries(&self) -> impl Iterator<Item = (Id, &Queued)> {
        self.0
            .iter()
            .map(|entry| (Id::from_raw(entry.key.clone()), entry))
    }

    /// Queue `entry` under `key`, as the app's verbs do.
    pub fn push(&mut self, key: Id, mut entry: Queued) -> Id {
        entry.key = key.as_str().to_owned();
        let key = Id::from_raw(entry.key.clone());
        self.0.push(entry);
        key
    }

    /// Mark the entry under `id` as settled with what became of it.
    pub fn settle(&mut self, id: &Id, settled: Settled) {
        if let Some(entry) = self.0.iter_mut().find(|entry| entry.key == id.as_str()) {
            entry.settled = Some(settled);
        }
    }

    /// Forget the entry under `id`: what it asked for is done with.
    pub fn forget(&mut self, id: &Id) -> bool {
        let before = self.0.len();
        self.0.retain(|entry| entry.key != id.as_str());
        self.0.len() != before
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One bridged Discord server: what the bridge observed, what it maps, and what it last heard of
/// the two sides' trouble.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guild {
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub only_channel: Option<String>,
    #[serde(default)]
    pub mirror_channels: bool,
    #[serde(default)]
    pub map_roles: bool,
    #[serde(default)]
    pub ownership: Ownership,
    #[serde(default)]
    pub token_item: Option<String>,
    #[serde(default)]
    pub bot_user: String,
    #[serde(default)]
    pub channels: Keyed<DChannel>,
    #[serde(default)]
    pub roles: Keyed<DRole>,
    #[serde(default)]
    pub emojis: BTreeMap<String, String>,
    #[serde(default)]
    pub map: Keyed<Mapping>,
    #[serde(default)]
    pub links: Keyed<Link>,
    #[serde(default)]
    pub link_offers: Keyed<LinkOffer>,
    #[serde(default)]
    pub connects: Keyed<Connect>,
    #[serde(default)]
    pub sign_in_page: Option<String>,
    #[serde(default)]
    pub avatars: BTreeMap<String, String>,
    #[serde(default)]
    pub blocked_personas: Set<String>,
    #[serde(default)]
    pub vanished: Set<String>,
    #[serde(default)]
    pub tilo_only: Set<String>,
    #[serde(default)]
    pub discord_only: Set<String>,
    #[serde(default)]
    pub outbox: Outbox<Queued>,
    #[serde(default)]
    pub voice_problem: Option<String>,
    #[serde(default)]
    pub emoji_problem: Option<String>,
}

/// Whether the server is still being adopted or is bridged both ways.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ownership {
    #[default]
    Importing,
    Adopted,
}

/// What a mapping joins: a channel, a category, a thread, a message, a webhook, a role or an emoji.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappedKind {
    #[default]
    Channel,
    Category,
    Thread,
    Message,
    Webhook,
    Role,
    Emoji,
}

impl MappedKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Category => "category",
            Self::Thread => "thread",
            Self::Message => "message",
            Self::Webhook => "webhook",
            Self::Role => "role",
            Self::Emoji => "emoji",
        }
    }
}

/// Which side a mapped object was made on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    #[default]
    Tilo,
    Discord,
}

/// A Discord channel or category as the bridge last observed it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DChannel {
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub overwrites: Vec<OverwriteInfo>,
}

impl From<&ChannelInfo> for DChannel {
    fn from(channel: &ChannelInfo) -> Self {
        Self {
            id: channel.id.clone(),
            kind: channel.kind,
            name: channel.name.clone(),
            parent: channel.parent.clone(),
            position: channel.position,
            topic: channel.topic.clone(),
            overwrites: channel.overwrites.clone(),
        }
    }
}

impl Identified for DChannel {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

/// A Discord role as the bridge last observed it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DRole {
    pub id: String,
    pub name: String,
    pub permissions: String,
    pub color: u32,
    pub position: i64,
    pub managed: bool,
}

impl From<&RoleInfo> for DRole {
    fn from(role: &RoleInfo) -> Self {
        Self {
            id: role.id.clone(),
            name: role.name.clone(),
            permissions: role.permissions.clone(),
            color: role.color,
            position: role.position,
            managed: role.managed,
        }
    }
}

impl Identified for DRole {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

/// A Tilo id and the Discord id it is carried to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub key: String,
    pub kind: MappedKind,
    pub tilo: String,
    pub discord: String,
    pub origin: Origin,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub picture: Option<String>,
}

impl Identified for Mapping {
    fn id(&self) -> Id {
        Id::from_raw(self.key.clone())
    }
}

/// A Discord account someone in the workspace linked.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub discord_user: String,
    pub person: String,
    #[serde(default)]
    pub linked_at: i64,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub interaction: Option<String>,
    #[serde(default)]
    pub photo: Option<String>,
    #[serde(default)]
    pub photo_avatar: Option<String>,
}

impl Identified for Link {
    fn id(&self) -> Id {
        Id::from_raw(self.discord_user.clone())
    }
}

/// A Discord account offered for linking, to whoever holds the one-time token it was offered with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkOffer {
    pub check: String,
    pub discord_user: String,
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub interaction: Option<String>,
    #[serde(default)]
    pub expires_ms: i64,
}

impl Identified for LinkOffer {
    fn id(&self) -> Id {
        Id::from_raw(self.check.clone())
    }
}

/// Someone on their way through Discord's consent page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connect {
    pub person: String,
    pub state_sha256: String,
    #[serde(default)]
    pub expires_ms: i64,
}

impl Identified for Connect {
    fn id(&self) -> Id {
        Id::from_raw(self.person.clone())
    }
}

fn overwrite_for<'a>(
    overwrites: Option<&'a Vec<OverwriteInfo>>,
    target: &str,
) -> Option<&'a OverwriteInfo> {
    overwrites?.iter().find(|o| o.id == target)
}

/// The map key of `tilo` as a `kind`.
#[must_use]
pub fn map_key(kind: MappedKind, tilo: &str) -> String {
    format!("{}:{tilo}", kind.as_str())
}

/// The server's custom emoji, name → id: two of one name are one, the first reported.
#[must_use]
pub fn named_emojis(emojis: &[EmojiInfo]) -> BTreeMap<String, String> {
    let mut by_name = BTreeMap::new();
    for emoji in emojis {
        by_name.entry(emoji.name.clone()).or_insert(emoji.id.clone());
    }
    by_name
}

impl Guild {
    /// Record the layout Discord reported, remembering what vanished since the last report so
    /// the reconcile can tell a deletion from a creation still in flight.
    pub fn observe(&mut self, bot_user: String, channels: Vec<ChannelInfo>, roles: Vec<RoleInfo>) {
        self.bot_user = bot_user;
        let now: std::collections::BTreeSet<&str> = channels
            .iter()
            .map(|c| c.id.as_str())
            .chain(roles.iter().map(|r| r.id.as_str()))
            .collect();
        let gone: Vec<String> = self
            .channels
            .iter()
            .map(|c| c.id.clone())
            .filter(|id| self.knows_mapped(id))
            .chain(
                self.roles
                    .iter()
                    .map(|r| r.id.clone())
                    .filter(|id| self.by_discord(MappedKind::Role, id).is_some()),
            )
            .filter(|id| !now.contains(id.as_str()))
            .collect();
        let back: Vec<String> = self
            .vanished
            .iter()
            .filter(|id| now.contains(id.as_str()))
            .cloned()
            .collect();
        for id in back {
            self.vanished.remove(&id);
        }
        for id in gone {
            self.vanished.insert(id);
        }
        // An overwrite the bridge asked for and saw done is answered once a layout reports any
        // change to it.
        let answered: Vec<Id> = self
            .outbox
            .entries()
            .filter(|(_, entry)| entry.settled.as_ref().is_some_and(Settled::is_done))
            .filter_map(|(id, entry)| match entry.op.get("set-overwrite") {
                Some(op) => Some((id, op)),
                None => None,
            })
            .filter(|(_, op)| {
                let channel = op.get("channel").and_then(Value::as_str).unwrap_or_default();
                let target = op.get("target").and_then(Value::as_str).unwrap_or_default();
                let before = self
                    .channels
                    .find(&Id::from_raw(channel))
                    .map(|c| &c.overwrites);
                let after = channels
                    .iter()
                    .find(|c| c.id == channel)
                    .map(|c| &c.overwrites);
                overwrite_for(before, target) != overwrite_for(after, target)
            })
            .map(|(id, _)| id)
            .collect();
        for id in &answered {
            self.outbox.forget(id);
        }
        let channels: Keyed<DChannel> = channels.iter().map(DChannel::from).collect();
        for id in crate::order::finished(self, &channels) {
            self.outbox.forget(&id);
        }
        self.channels = channels;
        self.roles = roles.iter().map(DRole::from).collect();
    }

    /// The server's custom emoji as Discord reported them.
    pub fn observe_emojis(&mut self, emojis: Vec<EmojiInfo>) {
        self.emojis = named_emojis(&emojis);
    }

    #[must_use]
    pub fn mapping(&self, kind: MappedKind, tilo: &str) -> Option<&Mapping> {
        self.map.find(&Id::from_raw(map_key(kind, tilo)))
    }

    /// The reverse index, derived on read.
    #[must_use]
    pub fn by_discord(&self, kind: MappedKind, discord: &str) -> Option<&Mapping> {
        self.map
            .iter()
            .find(|m| m.kind == kind && m.discord == discord)
    }

    /// A channel the bot observed, or one it created and mapped since.
    #[must_use]
    pub fn knows_channel(&self, discord: &str) -> bool {
        self.channels.find(&Id::from_raw(discord)).is_some()
            || self.map.iter().any(|m| {
                matches!(
                    m.kind,
                    MappedKind::Channel | MappedKind::Category | MappedKind::Thread
                ) && m.discord == discord
            })
    }

    #[must_use]
    pub fn is_bridge_webhook(&self, webhook: &str) -> bool {
        self.by_discord(MappedKind::Webhook, webhook).is_some()
    }

    /// The id of the server's custom emoji `name`, when it has one.
    #[must_use]
    pub fn emoji(&self, name: &str) -> Option<&str> {
        self.emojis.get(name).map(String::as_str)
    }
}

/// The longest content a Discord message takes.
pub const MAX_CONTENT_CHARS: usize = 2000;

/// The most one upload may be.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;

/// The largest file the bridge fetches from Discord.
pub const MAX_FETCH_BYTES: u64 = 32 * 1024 * 1024;
