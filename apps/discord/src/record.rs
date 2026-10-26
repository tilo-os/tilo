//! The `discord.guild` record. Its verbs check who calls them and validate
//! what they are given; deciding what a change means is the planners' work
//! (`layout`, `outbound`, `inbound`, `roles`, `import`).

use std::collections::BTreeMap;

use discord_wire::event::{ChannelInfo, EmojiInfo, OverwriteInfo, RoleInfo};
use discord_wire::event::{LINK_TTL_MS, link_check};
use discord_wire::op::{
    AnswerInteractionOp, ChannelKind, ChannelSpec, CreateWebhookOp, DeleteChannelOp, DeleteOp,
    DeleteRoleOp, DiscordOp, DiscordResult, EditOp, FetchAssetOp, MemberOp, MemberRolesOp,
    OverwriteKind, OverwriteOp, Queued, ReactOp, RoleSpec, SendOp, StartThreadOp,
};
use ui::tilo_resource::{
    self, ActionCx, Id, Identified, JsonSchema, Keyed, Model, Outbox, Refusal, Set, Settled,
    resource,
};

use crate::guard;

/// One bridged Discord server. See the module docs.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Guild {
    /// The server's snowflake.
    #[model(readonly)]
    pub guild_id: String,
    #[model(label)]
    pub name: String,
    /// The one Discord channel bridged, when the bridge is confined to it:
    /// nothing else in the server is imported, reconciled or changed.
    #[model(readonly)]
    pub only_channel: Option<String>,
    /// Channels and categories made in Tilo after the server was adopted
    /// are made in Discord too, and what Tilo does to them follows there.
    /// What Discord had stays Discord's either way.
    #[model(readonly)]
    pub mirror_channels: bool,
    /// Tilo's roles are kept as roles the bridge makes and owns in Discord,
    /// held by linked members. Off, the bridge changes no role or permission.
    #[model(readonly)]
    pub map_roles: bool,
    pub ownership: Ownership,
    /// The secret item holding the bot token in the host's store; the token
    /// itself never enters the record.
    pub token_item: Option<String>,
    /// The bot user's snowflake: its own messages and reactions are echoes.
    pub bot_user: String,
    /// Discord's channels as the bot last observed them.
    pub channels: Keyed<DChannel>,
    pub roles: Keyed<DRole>,
    /// The server's custom emoji, name → id, as the bot last observed them:
    /// a Tilo reaction with a workspace emoji of one of these names is
    /// mirrored as it.
    pub emojis: BTreeMap<String, String>,
    /// Tilo id ⇄ Discord id, keyed `kind:tilo`.
    pub map: Keyed<Mapping>,
    pub links: Keyed<Link>,
    /// Discord accounts offered for linking, each to whoever holds its
    /// one-time token (§5), by the token's check.
    pub link_offers: Keyed<LinkOffer>,
    /// People who went to Discord's consent page to link their account,
    /// by person: the OAuth 2 round trip's state, hashed.
    pub connects: Keyed<Connect>,
    /// Discord's consent page for signing in through the room's host,
    /// without its state, when the host offers it: then members are
    /// offered "Connect Discord".
    pub sign_in_page: Option<String>,
    /// Discord avatar hash → content-store blob, so an avatar is fetched once.
    pub avatars: BTreeMap<String, String>,
    /// Discord users whose messages are not mirrored.
    pub blocked_personas: Set<String>,
    /// Mapped Discord channels that were observed once and are gone from
    /// the latest layout: deleted in Discord, so the reconcile recreates
    /// them. A mapped channel never observed yet is still being created and
    /// is left alone, or every reconcile before Discord's echo would create
    /// it again.
    pub vanished: Set<String>,
    /// Tilo channels and categories that stay in Tilo only: unmatched when
    /// the server was adopted, or their Discord side was deleted.
    pub tilo_only: Set<String>,
    /// Discord channels and categories that stay in Discord only: their
    /// Tilo side was deleted, so they are not imported again.
    pub discord_only: Set<String>,
    pub outbox: Outbox<Queued, DiscordResult>,
    /// Why the bridge cannot carry a voice channel's call now, as Discord
    /// refused it (a permission the bot lacks there); none while it can.
    pub voice_problem: Option<String>,
}

#[derive(
    Model,
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Ownership {
    /// The server's categories and channels are being imported into Tilo,
    /// matched by name to what Tilo already has. Nothing in Discord changes.
    #[default]
    Importing,
    /// The server is adopted: what it had stays its own, and channels made
    /// on either side later are carried to the other (into Discord only when
    /// the bridge mirrors channels).
    Adopted,
}

#[derive(
    Model,
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum MappedKind {
    #[default]
    Channel,
    Category,
    Thread,
    Message,
    /// A Tilo channel's webhook (`discord` is the webhook id).
    Webhook,
    /// A Tilo role's Discord role (`tilo` is the role name).
    Role,
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
        }
    }
}

#[derive(
    Model,
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    #[default]
    Tilo,
    Discord,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DChannel {
    #[model(readonly)]
    pub id: String,
    pub kind: ChannelKind,
    pub name: String,
    pub parent: Option<String>,
    pub position: i64,
    pub topic: String,
    pub overwrites: Vec<OverwriteInfo>,
}

impl Identified for DChannel {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DRole {
    #[model(readonly)]
    pub id: String,
    pub name: String,
    pub permissions: String,
    pub color: u32,
    pub position: i64,
    pub managed: bool,
}

impl Identified for DRole {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Mapping {
    #[model(readonly)]
    pub key: String,
    pub kind: MappedKind,
    pub tilo: String,
    pub discord: String,
    pub origin: Origin,
    /// For a Discord-origin message: its author's Discord id, so blocking
    /// them can find what was mirrored.
    pub persona: Option<String>,
    /// For a message posted as a reply in Tilo: its thread root, because Tilo
    /// threads are one level deep.
    pub root: Option<String>,
}

impl Identified for Mapping {
    fn id(&self) -> Id {
        Id::from_raw(self.key.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Link {
    #[model(readonly)]
    pub discord_user: String,
    pub person: String,
    pub linked_at: i64,
    /// The account's unique Discord username, as the bridge last saw it.
    pub user_name: String,
    /// The name it shows when that is another (a nickname, a global name).
    pub display_name: Option<String>,
    /// The account's Discord avatar hash, as the bridge last saw it.
    pub avatar: Option<String>,
    /// The `/link` whose reply says what came of it, when the link was
    /// made from Discord.
    pub interaction: Option<String>,
}

impl Identified for Link {
    fn id(&self) -> Id {
        Id::from_raw(self.discord_user.clone())
    }
}

/// A Discord account offered for linking, to whoever holds the one-time
/// token it was offered with: the button `/link` answered with, or the
/// browser that came back from Discord's consent page. The record keeps
/// only the token's check ([`discord_wire::event::link_check`]).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct LinkOffer {
    #[model(readonly)]
    pub check: String,
    pub discord_user: String,
    pub user_name: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    /// The person who started it from Tilo: only they may take it.
    pub person: Option<String>,
    /// The `/link` it answers, whose reply the bridge edits.
    pub interaction: Option<String>,
    pub expires_ms: i64,
}

impl Identified for LinkOffer {
    fn id(&self) -> Id {
        Id::from_raw(self.check.clone())
    }
}

/// Someone on their way through Discord's consent page: the round trip's
/// state, hashed, binds what comes back to them.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Connect {
    /// One round trip per person: starting again replaces it.
    #[model(readonly)]
    pub person: String,
    pub state_sha256: String,
    pub expires_ms: i64,
}

impl Identified for Connect {
    fn id(&self) -> Id {
        Id::from_raw(self.person.clone())
    }
}

/// The map key of `tilo` as a `kind`.
#[must_use]
pub fn map_key(kind: MappedKind, tilo: &str) -> String {
    format!("{}:{tilo}", kind.as_str())
}

/// A display name as the bridge passes it, empty for none.
fn shown(display_name: String) -> Option<String> {
    (!display_name.is_empty()).then_some(display_name)
}

/// Insert `item`, or replace the item with its id.
pub(crate) fn upsert<T: Identified>(items: &mut Keyed<T>, item: T) {
    let id = item.id();
    match items.find_mut(&id) {
        Some(slot) => *slot = item,
        None => items.push(item),
    }
}

impl Guild {
    #[must_use]
    pub fn mapping(&self, kind: MappedKind, tilo: &str) -> Option<&Mapping> {
        self.map.find(&Id::from_raw(map_key(kind, tilo)))
    }

    /// The reverse index, derived on read (discord-bridge.md §6).
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
}

impl Guild {
    /// The layout Discord reported, with what vanished since the last report
    /// remembered so the reconcile can tell a deletion from a creation still
    /// in flight. The bridge compares against this to see whether a layout
    /// event says anything new.
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
        // An overwrite the bridge asked for and saw done is answered once a
        // layout reports any change to it: Discord's echo, or someone
        // editing it since, which the reconcile then reverts.
        let answered: Vec<Id> = self
            .outbox
            .entries()
            .filter(|(_, entry)| entry.settled.as_ref().is_some_and(Settled::is_done))
            .filter_map(|(id, entry)| match &entry.value.op {
                DiscordOp::SetOverwrite(op) => Some((id, op)),
                _ => None,
            })
            .filter(|(_, op)| {
                let before = self
                    .channels
                    .find(&Id::from_raw(op.channel.as_str()))
                    .map(|c| &c.overwrites);
                let after = channels
                    .iter()
                    .find(|c| c.id == op.channel)
                    .map(|c| &c.overwrites);
                overwrite_for(before, &op.target) != overwrite_for(after, &op.target)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &answered {
            self.outbox.forget(id);
        }
        self.channels = channels
            .into_iter()
            .map(|c| DChannel {
                id: c.id,
                kind: c.kind,
                name: c.name,
                parent: c.parent,
                position: c.position,
                topic: c.topic,
                overwrites: c.overwrites,
            })
            .collect();
        self.roles = roles
            .into_iter()
            .map(|r| DRole {
                id: r.id,
                name: r.name,
                permissions: r.permissions,
                color: r.color,
                position: r.position,
                managed: r.managed,
            })
            .collect();
    }

    /// The server's custom emoji as Discord reported them. Two of one name
    /// (Discord allows it) are one: the first reported.
    pub fn observe_emojis(&mut self, emojis: Vec<EmojiInfo>) {
        let mut by_name = BTreeMap::new();
        for emoji in emojis {
            by_name.entry(emoji.name).or_insert(emoji.id);
        }
        self.emojis = by_name;
    }

    /// The id of the server's custom emoji `name`, when it has one.
    #[must_use]
    pub fn emoji(&self, name: &str) -> Option<&str> {
        self.emojis.get(name).map(String::as_str)
    }
}

impl Guild {
    /// `op`, when this bridge may queue it: one confined to a channel acts
    /// only in that channel and the threads it mapped there.
    fn admit(&self, op: DiscordOp) -> Result<DiscordOp, Refusal> {
        let Some(only) = &self.only_channel else {
            return Ok(op);
        };
        if op.reaches_server() {
            return Err(Refusal::new(
                "a bridge of one channel changes nothing else in the server",
            ));
        }
        match op.channel() {
            Some(channel)
                if channel != only && self.by_discord(MappedKind::Thread, channel).is_none() =>
            {
                Err(Refusal::new(
                    "this bridge reaches only its one Discord channel",
                ))
            }
            _ => Ok(op),
        }
    }
}

fn overwrite_for<'a>(
    overwrites: Option<&'a Vec<OverwriteInfo>>,
    target: &str,
) -> Option<&'a OverwriteInfo> {
    overwrites?.iter().find(|o| o.id == target)
}

/// Bring a guild record of version `from` up to this one. Version 1 named
/// the Chat workspace the server mirrored; Chat runs in its room and keeps
/// no workspaces now, so the server mirrors the room's chat and the name is
/// dropped. Up to version 2 a channel's kind and permission overwrites were
/// each one JSON string; they are rewritten as the values they held. Up
/// to version 3 a link began with a code the person sent from Discord;
/// those codes and the count of wrong ones are dropped, as linking starts
/// from a one-time token now and a pending code could no longer be used.
///
/// # Errors
/// When `from` is not a version this record was, or a stored kind or
/// overwrite list does not read.
fn migrate(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    use ui::automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
    use ui::tilo_resource::evolution::{map_values, retype_json};
    if !(1..=3).contains(&from) {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no Discord server migration from {from}"
        )));
    }
    if doc.get(ROOT, "workspace")?.is_some() {
        doc.delete(ROOT, "workspace")?;
    }
    if from <= 2 {
        for (_, channel) in map_values(doc, &ROOT, "channels")? {
            retype_json::<ChannelKind>(doc, &channel, "kind")?;
            retype_json::<Vec<OverwriteInfo>>(doc, &channel, "overwrites")?;
        }
    }
    for dropped in ["pending_links", "link_failures"] {
        if doc.get(ROOT, dropped)?.is_some() {
            doc.delete(ROOT, dropped)?;
        }
    }
    Ok(())
}

#[resource(
    kind = "discord.guild",
    label = "Discord server",
    params = guild,
    version = 4,
    migrate = migrate,
)]
impl Guild {
    /// Connect a Discord server.
    ///
    /// Binds this record to one server, mirrored in the room's chat.
    /// The server's layout stays the truth until the import has run.
    #[action(housekeeping)]
    pub fn connect(
        &mut self,
        cx: &mut ActionCx,
        #[param("The server's id")] guild_id: String,
        #[param("The server's name")] name: String,
        #[param("Bridge only this channel of the server, by its id")] only_channel: Option<String>,
        #[param("Make channels created in Tilo later in Discord too")] mirror_channels: Option<
            bool,
        >,
        #[param("Keep Tilo's roles as roles the bridge owns in Discord")] map_roles: Option<bool>,
    ) -> Result<(), Refusal> {
        guard::person(cx)?;
        if !self.guild_id.is_empty() {
            return Err(Refusal::new("this record is already connected to a server"));
        }
        let snowflake = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
        if !snowflake(&guild_id) || !only_channel.as_deref().is_none_or(snowflake) {
            return Err(Refusal::new("a server or channel id is a number"));
        }
        self.guild_id = guild_id;
        self.only_channel = only_channel;
        self.mirror_channels = mirror_channels.unwrap_or(false);
        self.map_roles = map_roles.unwrap_or(false);
        self.name = name;
        self.ownership = Ownership::Importing;
        Ok(())
    }

    /// Record Discord's layout as the latest layout event reported it.
    #[action]
    pub fn observe_layout(
        &mut self,
        cx: &mut ActionCx,
        #[param("The bot user's id")] bot_user: String,
        #[param("Every channel")] channels: Vec<ChannelInfo>,
        #[param("Every role")] roles: Vec<RoleInfo>,
        #[param("Every custom emoji")] emojis: Vec<EmojiInfo>,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.observe(bot_user, channels, roles);
        self.observe_emojis(emojis);
        Ok(())
    }

    /// Map a Tilo object to its Discord counterpart.
    #[action]
    pub fn map(
        &mut self,
        cx: &mut ActionCx,
        kind: MappedKind,
        #[param("The Tilo id, or the item resource a Chat verb answered")] tilo: String,
        #[param("The Discord id")] discord: String,
        origin: Origin,
        #[param("The Discord author of a Discord-origin message")] persona: Option<String>,
        #[param("The Tilo thread root of a reply")] root: Option<String>,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        // A verb answers the items it created as their resources
        // (`chat.room/messages/<id>`); the map keys on the item's id.
        let tilo = tilo.rsplit('/').next().unwrap_or_default().to_owned();
        if tilo.is_empty() || discord.is_empty() {
            return Err(Refusal::new("a mapping joins two ids"));
        }
        if matches!(kind, MappedKind::Channel | MappedKind::Category) {
            self.tilo_only.remove(&tilo);
            self.discord_only.remove(&discord);
        }
        upsert(
            &mut self.map,
            Mapping {
                key: map_key(kind, &tilo),
                kind,
                tilo,
                discord,
                origin,
                persona,
                root,
            },
        );
        Ok(())
    }

    /// Forget a mapping.
    #[action]
    pub fn unmap(
        &mut self,
        cx: &mut ActionCx,
        kind: MappedKind,
        #[param("The Tilo id")] tilo: String,
        #[param("Keep both sides of a channel or category apart from now on")] keep_apart: Option<
            bool,
        >,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        let gone = self.map.take(&Id::from_raw(map_key(kind, &tilo)));
        if keep_apart == Some(true) && matches!(kind, MappedKind::Channel | MappedKind::Category) {
            if let Some(mapping) = gone {
                self.discord_only.insert(mapping.discord);
            }
            self.tilo_only.insert(tilo);
        }
        Ok(())
    }

    /// Send a message through a channel's webhook.
    #[action]
    pub fn send(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        mut op: SendOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        // The duplicate check after a crash looks only at messages newer than this.
        op.created_ms = cx.now_ms();
        self.known(&op.channel)?;
        self.webhook(&op.webhook)?;
        content_fits(&op.content, !op.attachments.is_empty())?;
        if op.attachments.len() > 10 || op.attachments.iter().any(|a| a.size > MAX_UPLOAD_BYTES) {
            return Err(Refusal::new(
                "Discord takes at most ten files of 10 MiB from a webhook",
            ));
        }
        let publish = op.avatar_blob.clone().into_iter().collect();
        let attach = op.attachments.iter().map(|a| a.blob.clone()).collect();
        outbox.push(
            cx.id(),
            Queued {
                op: self.admit(DiscordOp::Send(op))?,
                publish,
                attach,
            },
        );
        Ok(())
    }

    /// Edit a message the bridge sent.
    #[action]
    pub fn edit(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: EditOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        self.webhook(&op.webhook)?;
        // Editing content leaves Discord's existing attachments in place;
        // an empty caption is valid even though an empty new post is not.
        content_fits(&op.content, true)?;
        queue(cx, outbox, self.admit(DiscordOp::Edit(op))?);
        Ok(())
    }

    /// Delete a message.
    #[action]
    pub fn delete(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: DeleteOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        if let Some(webhook) = &op.webhook {
            self.webhook(webhook)?;
        }
        queue(cx, outbox, self.admit(DiscordOp::Delete(op))?);
        Ok(())
    }

    /// Add the bot's reaction.
    #[action]
    pub fn react(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: ReactOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        queue(cx, outbox, self.admit(DiscordOp::React(op))?);
        Ok(())
    }

    /// Remove the bot's reaction.
    #[action]
    pub fn unreact(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: ReactOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        queue(cx, outbox, self.admit(DiscordOp::Unreact(op))?);
        Ok(())
    }

    /// Start a thread on a message.
    #[action]
    pub fn start_thread(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: StartThreadOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        queue(cx, outbox, self.admit(DiscordOp::StartThread(op))?);
        Ok(())
    }

    /// Create a channel's webhook.
    #[action]
    pub fn create_webhook(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: CreateWebhookOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.known(&op.channel)?;
        queue(cx, outbox, self.admit(DiscordOp::CreateWebhook(op))?);
        Ok(())
    }

    /// Create a channel or bring it back in line.
    #[action]
    pub fn apply_channel(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: ChannelSpec,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.mirroring()?;
        if let Some(id) = &op.id {
            self.made_by_bridge(id)?;
        }
        if let Some(parent) = &op.parent {
            self.known(parent)?;
        }
        if op.name.is_empty() || op.name.chars().count() > 100 {
            return Err(Refusal::new(
                "a Discord channel name is 1 to 100 characters",
            ));
        }
        queue(cx, outbox, self.admit(DiscordOp::ApplyChannel(op))?);
        Ok(())
    }

    /// Delete a channel.
    #[action]
    pub fn delete_channel(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: DeleteChannelOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.mirroring()?;
        self.made_by_bridge(&op.channel)?;
        queue(cx, outbox, self.admit(DiscordOp::DeleteChannel(op))?);
        Ok(())
    }

    /// Create a role or bring it back in line.
    #[action]
    pub fn apply_role(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: RoleSpec,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.mapping_roles()?;
        if let Some(id) = &op.id
            && self.by_discord(MappedKind::Role, id).is_none()
        {
            return Err(Refusal::new(
                "the bridge changes only the roles it made, never @everyone or the server's own",
            ));
        }
        queue(cx, outbox, self.admit(DiscordOp::ApplyRole(op))?);
        Ok(())
    }

    /// Delete a role.
    #[action]
    pub fn delete_role(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: DeleteRoleOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        if self.by_discord(MappedKind::Role, &op.role).is_none() {
            return Err(Refusal::new("the bridge deletes only roles it manages"));
        }
        queue(cx, outbox, self.admit(DiscordOp::DeleteRole(op))?);
        Ok(())
    }

    /// Set a linked member's roles.
    #[action]
    pub fn set_member_roles(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: MemberRolesOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.mapping_roles()?;
        self.linked(&op.user)?;
        if op
            .add
            .iter()
            .chain(&op.remove)
            .any(|role| self.by_discord(MappedKind::Role, role).is_none())
        {
            return Err(Refusal::new("the bridge assigns only roles it manages"));
        }
        queue(cx, outbox, self.admit(DiscordOp::SetMemberRoles(op))?);
        Ok(())
    }

    /// Kick a linked member.
    #[action]
    pub fn kick(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: MemberOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.linked(&op.user)?;
        queue(cx, outbox, self.admit(DiscordOp::Kick(op))?);
        Ok(())
    }

    /// Ban a linked member.
    #[action]
    pub fn ban(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: MemberOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.linked(&op.user)?;
        queue(cx, outbox, self.admit(DiscordOp::Ban(op))?);
        Ok(())
    }

    /// Unban a linked member.
    #[action]
    pub fn unban(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: MemberOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.linked(&op.user)?;
        queue(cx, outbox, self.admit(DiscordOp::Unban(op))?);
        Ok(())
    }

    /// Set a channel's permission overwrite.
    #[action]
    pub fn set_overwrite(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: OverwriteOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.mapping_roles()?;
        self.known(&op.channel)?;
        if op.target_kind != OverwriteKind::Role
            || self.by_discord(MappedKind::Role, &op.target).is_none()
        {
            return Err(Refusal::new(
                "the bridge sets overwrites only for the roles it made",
            ));
        }
        queue(cx, outbox, self.admit(DiscordOp::SetOverwrite(op))?);
        Ok(())
    }

    /// Copy a file from Discord's CDN into the room's content store. The
    /// worker that performs it fetches only from the CDN it is configured
    /// with, so a URL elsewhere is refused there.
    #[action]
    pub fn fetch_asset(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: FetchAssetOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        if op.max_bytes > MAX_FETCH_BYTES {
            return Err(Refusal::new("Chat holds files of at most 32 MiB"));
        }
        queue(cx, outbox, self.admit(DiscordOp::FetchAsset(op))?);
        Ok(())
    }

    /// Start linking your Discord account from Tilo.
    ///
    /// Records the hash of the state your trip through Discord's consent
    /// page carries, so what comes back is linked to you alone. Good for
    /// ten minutes.
    #[action(housekeeping)]
    pub fn begin_connect(
        &mut self,
        cx: &mut ActionCx,
        #[param("SHA-256 of the OAuth2 state")] state_sha256: String,
    ) -> Result<(), Refusal> {
        let person = guard::person(cx)?;
        if self.guild_id.is_empty() {
            return Err(Refusal::new("this room is not bridged to a Discord server"));
        }
        hex_digest(&state_sha256)?;
        upsert(
            &mut self.connects,
            Connect {
                person,
                state_sha256,
                expires_ms: cx.now_ms().saturating_add(LINK_TTL_MS),
            },
        );
        Ok(())
    }

    /// Offer a Discord account for linking, from Discord.
    ///
    /// The account ran `/link`. Whoever holds the token whose check this
    /// is links it to themselves with `complete-link` within ten minutes;
    /// the reply to `interaction` then says what came of it.
    #[action]
    pub fn offer_link(
        &mut self,
        cx: &mut ActionCx,
        #[param("The token's check")] check: String,
        #[param("The Discord account")] discord_user: String,
        #[param("Its Discord username")] user_name: String,
        #[param("The name it shows when another; empty when none")] display_name: Option<String>,
        #[param("Its Discord avatar hash; empty when none")] avatar: Option<String>,
        #[param("The /link it answers")] interaction: Option<String>,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        self.offer(
            cx.now_ms(),
            LinkOffer {
                check,
                discord_user,
                user_name,
                display_name: display_name.and_then(shown),
                avatar: avatar.and_then(shown),
                person: None,
                interaction: interaction.and_then(shown),
                expires_ms: 0,
            },
        )
    }

    /// Offer a Discord account for linking, from a sign-in in Tilo.
    ///
    /// The account signed in on Discord's consent page in the round trip
    /// whose state hashes to `state_sha256`: only the person who started
    /// it (`begin-connect`) may take the offer, with `complete-link`
    /// within ten minutes.
    #[action]
    pub fn offer_sign_in(
        &mut self,
        cx: &mut ActionCx,
        #[param("The token's check")] check: String,
        #[param("The Discord account")] discord_user: String,
        #[param("Its Discord username")] user_name: String,
        #[param("The name it shows when another; empty when none")] display_name: Option<String>,
        #[param("Its Discord avatar hash; empty when none")] avatar: Option<String>,
        #[param("SHA-256 of the round trip's OAuth 2 state")] state_sha256: String,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        hex_digest(&check)?;
        let now = cx.now_ms();
        let person = self
            .connects
            .iter()
            .find(|connect| connect.state_sha256 == state_sha256 && now <= connect.expires_ms)
            .map(|connect| connect.person.clone())
            .ok_or_else(|| {
                Refusal::new("that sign-in with Discord was not started here, or took too long")
            })?;
        self.connects.take(&Id::from_raw(person.as_str()));
        self.offer(
            now,
            LinkOffer {
                check,
                discord_user,
                user_name,
                display_name: display_name.and_then(shown),
                avatar: avatar.and_then(shown),
                person: Some(person),
                interaction: None,
                expires_ms: 0,
            },
        )
    }

    /// Keep `offer` for ten minutes from `now`, dropping those run out.
    fn offer(&mut self, now: i64, offer: LinkOffer) -> Result<(), Refusal> {
        hex_digest(&offer.check)?;
        let expired: Vec<Id> = self
            .link_offers
            .iter()
            .filter(|offer| offer.expires_ms < now)
            .map(Identified::id)
            .collect();
        for id in expired {
            self.link_offers.take(&id);
        }
        upsert(
            &mut self.link_offers,
            LinkOffer {
                expires_ms: now.saturating_add(LINK_TTL_MS),
                ..offer
            },
        );
        Ok(())
    }

    /// Link the Discord account offered to you.
    ///
    /// `proof` is the SHA-256 of the one-time token you were given; the
    /// offer it opens is used up. Your messages in Discord then show as
    /// you, and a link you had before is replaced.
    #[action(housekeeping)]
    pub fn complete_link(
        &mut self,
        cx: &mut ActionCx,
        #[param("SHA-256 of the link token")] proof: String,
    ) -> Result<(), Refusal> {
        let person = guard::person(cx)?;
        let id = Id::from_raw(link_check(&proof));
        let Some(offer) = self.link_offers.find(&id).cloned() else {
            return Err(Refusal::new(
                "this link was already used or has run out. Start again from Discord with /link, or with Connect Discord in Tilo",
            ));
        };
        if cx.now_ms() > offer.expires_ms {
            self.link_offers.take(&id);
            return Err(Refusal::new(
                "this link ran out after ten minutes. Start again from Discord with /link, or with Connect Discord in Tilo",
            ));
        }
        if offer
            .person
            .as_deref()
            .is_some_and(|starter| starter != person)
        {
            return Err(Refusal::new(
                "this link was started by another Tilo account. Sign in as that account, or start again",
            ));
        }
        self.link_offers.take(&id);
        let replaced: Vec<Id> = self
            .links
            .iter()
            .filter(|link| link.person == person)
            .map(Identified::id)
            .collect();
        for old in replaced {
            self.links.take(&old);
        }
        upsert(
            &mut self.links,
            Link {
                discord_user: offer.discord_user,
                person,
                linked_at: cx.now_ms(),
                user_name: offer.user_name,
                display_name: offer.display_name,
                avatar: offer.avatar,
                interaction: offer.interaction,
            },
        );
        Ok(())
    }

    /// Offer signing in with Discord.
    ///
    /// The room's host offers it, through this consent page (empty for
    /// none): members are then offered "Connect Discord".
    #[action]
    pub fn set_sign_in(
        &mut self,
        cx: &mut ActionCx,
        #[param("Discord's consent page without its state; empty for none")] page: String,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        let page = shown(page);
        if let Some(page) = &page
            && (!(page.starts_with("https://") || page.starts_with("http://"))
                || !page.contains('?'))
        {
            return Err(Refusal::new(
                "a consent page is an http(s) address with its query",
            ));
        }
        self.sign_in_page = page;
        Ok(())
    }

    /// Edit the reply to a slash command to what came of it.
    #[action]
    pub fn answer_interaction(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Queued, DiscordResult>,
        op: AnswerInteractionOp,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        if op.content.is_empty() || op.content.chars().count() > MAX_CONTENT_CHARS {
            return Err(Refusal::new("a reply holds 1 to 2000 characters"));
        }
        queue(cx, outbox, self.admit(DiscordOp::AnswerInteraction(op))?);
        Ok(())
    }

    /// Take a linked account's newer names and avatar.
    ///
    /// The bridge saw the account under another username, display name or
    /// avatar than the link holds.
    #[action(housekeeping)]
    pub fn rename_link(
        &mut self,
        cx: &mut ActionCx,
        #[param("The Discord account")] discord_user: String,
        #[param("Its Discord username")] user_name: String,
        #[param("The name it shows when another; empty when none")] display_name: Option<String>,
        #[param("Its newer avatar hash, empty for none; unchanged when absent")] avatar: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        if let Some(link) = self.links.find_mut(&Id::from_raw(discord_user.as_str())) {
            link.user_name = user_name;
            link.display_name = display_name.and_then(shown);
            if let Some(avatar) = avatar {
                link.avatar = shown(avatar);
            }
        }
        Ok(())
    }

    /// Unlink a Discord account.
    #[action]
    pub fn unlink(
        &mut self,
        cx: &mut ActionCx,
        #[param("The Discord account")] discord_user: String,
    ) -> Result<(), Refusal> {
        let id = Id::from_raw(discord_user.as_str());
        let owner = self.links.find(&id).map(|l| l.person.clone());
        if guard::bot(cx).is_err() {
            let person = guard::person(cx)?;
            if owner.as_deref() != Some(person.as_str()) {
                return Err(Refusal::new(
                    "only its person or the bridge unlinks an account",
                ));
            }
        }
        self.links.take(&id);
        Ok(())
    }

    /// Stop mirroring a Discord user.
    ///
    /// Their Discord messages are no longer mirrored, and the bridge removes
    /// the ones already mirrored. They are not banned in Discord.
    #[action(moderation)]
    pub fn block_persona(
        &mut self,
        cx: &mut ActionCx,
        #[param("The Discord user")] discord_user: String,
    ) -> Result<(), Refusal> {
        bot_or_person(cx)?;
        self.blocked_personas.insert(discord_user);
        Ok(())
    }

    /// Mirror a Discord user again.
    #[action(moderation)]
    pub fn unblock_persona(
        &mut self,
        cx: &mut ActionCx,
        #[param("The Discord user")] discord_user: String,
    ) -> Result<(), Refusal> {
        bot_or_person(cx)?;
        self.blocked_personas.remove(&discord_user);
        Ok(())
    }

    /// Remember where an avatar's image is stored.
    #[action]
    pub fn remember_avatar(
        &mut self,
        cx: &mut ActionCx,
        hash: String,
        blob: String,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        let hash_ok = !hash.is_empty()
            && hash.len() <= 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        let blob_ok = blob.len() == 64 && blob.bytes().all(|b| b.is_ascii_hexdigit());
        if !hash_ok || !blob_ok {
            return Err(Refusal::new("an avatar hash and a content address"));
        }
        self.avatars.insert(hash, blob);
        Ok(())
    }

    /// Finish adopting the server: everything in it is in Tilo now.
    #[action]
    pub fn finish_import(
        &mut self,
        cx: &mut ActionCx,
        #[param("Tilo channels and categories the server had no match for")] tilo_only: Vec<String>,
    ) -> Result<(), Refusal> {
        guard::bot(cx)?;
        if self.ownership == Ownership::Adopted {
            return Err(Refusal::new("the server was already adopted"));
        }
        self.ownership = Ownership::Adopted;
        for id in tilo_only {
            self.tilo_only.insert(id);
        }
        Ok(())
    }

    /// Say why the bridge cannot carry voice now, or that it can again.
    ///
    /// The room's host reports what Discord answered when the bridge
    /// joined a voice channel, so people see why a call is not carried.
    #[action(housekeeping)]
    pub fn report_voice(
        &mut self,
        cx: &mut ActionCx,
        #[param("What Discord refused, as it said it; none once voice is carried")] problem: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        bot_or_person(cx)?;
        if problem.as_deref().is_some_and(|problem| {
            problem.is_empty() || problem.chars().count() > MAX_PROBLEM_CHARS
        }) {
            return Err(Refusal::new("a voice problem is 1 to 300 characters"));
        }
        self.voice_problem = problem;
        Ok(())
    }
}

/// The longest voice problem a guild keeps, in characters.
pub const MAX_PROBLEM_CHARS: usize = 300;

/// Discord's limit for a bot's or webhook's upload.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024;
/// Chat's attachment limit (`chat::record::MAX_ATTACHMENT_BYTES`).
pub const MAX_FETCH_BYTES: u64 = 32 * 1024 * 1024;
/// Discord's message length limit, in characters.
pub const MAX_CONTENT_CHARS: usize = 2000;

fn queue(cx: &mut ActionCx, outbox: &mut Outbox<Queued, DiscordResult>, op: DiscordOp) {
    outbox.push(
        cx.id(),
        Queued {
            op,
            publish: Vec::new(),
            attach: Vec::new(),
        },
    );
}

/// `text` is a SHA-256 digest in hex.
fn hex_digest(text: &str) -> Result<(), Refusal> {
    if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(Refusal::new("a digest is 64 hex digits"))
    }
}

fn content_fits(content: &str, has_files: bool) -> Result<(), Refusal> {
    let chars = content.chars().count();
    if chars > MAX_CONTENT_CHARS || (chars == 0 && !has_files) {
        return Err(Refusal::new("a Discord message is 1 to 2000 characters"));
    }
    Ok(())
}

impl Guild {
    fn known(&self, channel: &str) -> Result<(), Refusal> {
        if self.knows_channel(channel) {
            Ok(())
        } else {
            Err(Refusal::new("there is no such channel in the server"))
        }
    }

    fn webhook(&self, webhook: &str) -> Result<(), Refusal> {
        if self.is_bridge_webhook(webhook) {
            Ok(())
        } else {
            Err(Refusal::new("that is not one of the bridge's webhooks"))
        }
    }

    fn mirroring(&self) -> Result<(), Refusal> {
        if !self.mirror_channels {
            Err(Refusal::new(
                "this bridge does not make channels in Discord",
            ))
        } else if self.ownership != Ownership::Adopted {
            Err(Refusal::new("the server is adopted first"))
        } else {
            Ok(())
        }
    }

    /// A channel or category the bridge made in Discord for a Tilo one.
    fn made_by_bridge(&self, discord: &str) -> Result<(), Refusal> {
        let ours = self.map.iter().any(|m| {
            matches!(m.kind, MappedKind::Channel | MappedKind::Category)
                && m.discord == discord
                && m.origin == Origin::Tilo
        });
        if ours {
            Ok(())
        } else {
            Err(Refusal::new(
                "the bridge changes only channels it made, never the server's own",
            ))
        }
    }

    fn mapping_roles(&self) -> Result<(), Refusal> {
        if self.map_roles {
            Ok(())
        } else {
            Err(Refusal::new("this bridge leaves the server's roles alone"))
        }
    }

    fn linked(&self, user: &str) -> Result<(), Refusal> {
        if self.links.find(&Id::from_raw(user)).is_some() {
            Ok(())
        } else {
            Err(Refusal::new(
                "that Discord account is not linked to anyone in the room",
            ))
        }
    }
}

fn bot_or_person(cx: &ActionCx) -> Result<(), Refusal> {
    guard::bot(cx).or_else(|_| guard::person(cx).map(drop))
}

#[cfg(test)]
pub(crate) mod tests;
