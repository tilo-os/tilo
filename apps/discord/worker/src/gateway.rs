//! One gateway session: identify or resume, heartbeats, dispatches turned
//! into events, and a backfill when Discord refuses to resume.
//!
//! The resume state (session, sequence, the last message seen per channel)
//! is the host's to keep: the worker has no writable path, so it reports
//! the state as a `Checkpoint` and gets it back in `Start`.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_connector::protocol::FromWorker;
use tilo_net::Method;
use tokio::time::Instant;
use tokio_websockets::{ClientBuilder, Connector, MaybeTlsStream, Message, WebSocketStream};
use url::Url;
use zeroize::Zeroizing;

use crate::Ctx;
use crate::egress::Purpose;
use crate::events::{Emit, Guild, snowflake_ms};
use crate::rest::{Auth, Body, RestError, segment};
use crate::secrets::{BOT_TOKEN, TOKEN_ITEM};

/// `GUILDS`, `GUILD_MEMBERS`, `GUILD_MODERATION` (bans), `GUILD_MESSAGES`,
/// `GUILD_MESSAGE_REACTIONS` and `MESSAGE_CONTENT`.
pub const INTENTS: u64 = 1 | (1 << 1) | (1 << 2) | (1 << 9) | (1 << 10) | (1 << 15);
/// Messages read back per channel when a session could not be resumed.
pub const BACKFILL_LIMIT: u32 = 100;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A rejected token is asked for again this often, so a new one set with
/// `set-bot-token` is picked up without restarting the worker.
const TOKEN_RETRY: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const CHECKPOINT_EVERY: Duration = Duration::from_secs(1);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// The resume state the host keeps for the worker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub resume_url: Option<String>,
    #[serde(default)]
    pub seq: u64,
    /// When the session was identified, in Unix seconds; part of every
    /// sequence-keyed event id (see [`Gateway::sequence`]).
    #[serde(default)]
    pub session_started_s: u64,
    /// Channel (or thread) → the last message seen in it.
    #[serde(default)]
    pub last_seen: BTreeMap<String, Seen>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub message: String,
    /// The parent text channel, when the channel is a thread.
    #[serde(default)]
    pub parent: Option<String>,
}

enum End {
    /// Connect again, resuming if the session allows it.
    Reconnect,
    /// Discord refused the token or the session's settings.
    Rejected(String),
    /// The host has no token.
    NoToken,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one session, not a state machine"
)]
pub struct Gateway {
    ctx: Arc<Ctx>,
    state: Checkpoint,
    guild: Option<Guild>,
    /// The guild to bridge, once known.
    guild_id: Option<String>,
    application: Option<String>,
    bot_user: String,
    backfill_pending: bool,
    commands_registered: bool,
    dirty: bool,
    last_checkpoint: Instant,
    /// Whether the last connection got as far as READY or RESUMED.
    established: bool,
}

impl Gateway {
    #[must_use]
    pub fn new(ctx: Arc<Ctx>, checkpoint: Option<Value>) -> Self {
        let state: Checkpoint = checkpoint
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let guild_id = ctx.config.guild_id.clone();
        Self {
            ctx,
            state,
            guild: None,
            guild_id,
            application: None,
            bot_user: String::new(),
            backfill_pending: false,
            commands_registered: false,
            dirty: false,
            last_checkpoint: Instant::now(),
            established: false,
        }
    }

    /// Keep a session open until the task is dropped.
    pub async fn run(mut self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            self.established = false;
            let end = self.session().await;
            self.flush_checkpoint();
            let wait = match end {
                End::Reconnect => {
                    if self.established {
                        backoff = Duration::from_secs(1);
                        Duration::from_millis(250)
                    } else {
                        let wait = backoff;
                        backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
                        wait
                    }
                }
                End::Rejected(message) => {
                    self.ctx.secrets.forget(BOT_TOKEN, TOKEN_ITEM);
                    self.ctx.status(false, &message);
                    TOKEN_RETRY
                }
                End::NoToken => {
                    self.ctx
                        .status(false, "no bot token is set for the connector");
                    TOKEN_RETRY
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Gateway sequence numbers restart with every session, and the host's
    /// log deduplicates by event id for a day, so a sequence-keyed id
    /// carries the session's start second in its upper bits: a replay in the
    /// same session repeats its id, a new session never reuses one.
    fn sequence(&self, seq: u64) -> u64 {
        self.state.session_started_s.wrapping_shl(32) | (seq & 0xffff_ffff)
    }

    fn emit(&self, emit: Emit) {
        let _ = self.ctx.out.send(FromWorker::Event {
            source: emit.source.to_owned(),
            id: emit.id,
            at_ms: emit.at_ms,
            payload: emit.payload,
        });
    }

    fn flush_checkpoint(&mut self) {
        if let Ok(state) = serde_json::to_value(&self.state) {
            let _ = self.ctx.out.send(FromWorker::Checkpoint { state });
        }
        self.dirty = false;
        self.last_checkpoint = Instant::now();
    }

    async fn gateway_url(&mut self) -> Result<Url, End> {
        if let (Some(_), Some(resume)) = (&self.state.session_id, &self.state.resume_url) {
            match Url::parse(resume) {
                Ok(url) if self.ctx.config.egress.check(&url, Purpose::Gateway).is_ok() => {
                    return Ok(url);
                }
                _ => {
                    self.state.session_id = None;
                    self.state.resume_url = None;
                }
            }
        }
        let answer = self
            .ctx
            .rest
            .call(
                Method::GET,
                "/gateway/bot",
                &[],
                Body::Empty,
                Auth::Bot,
                None,
            )
            .await;
        let url = match answer {
            Ok(answer) => answer["url"].as_str().unwrap_or_default().to_owned(),
            Err(RestError::Unauthorized(message)) => return Err(End::Rejected(message)),
            Err(error) => {
                self.ctx.status(false, &error.to_string());
                return Err(End::Reconnect);
            }
        };
        let url = Url::parse(&url).map_err(|_| End::Reconnect)?;
        if let Err(error) = self.ctx.config.egress.check(&url, Purpose::Gateway) {
            self.ctx.status(false, &format!("gateway refused: {error}"));
            return Err(End::Reconnect);
        }
        Ok(url)
    }

    async fn connect(url: &Url) -> Option<Socket> {
        let mut tls = tilo_net::client_config().ok()?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let connector = Connector::Rustls(tokio_rustls::TlsConnector::from(Arc::new(tls)));
        let builder = ClientBuilder::new()
            .uri(url.as_str())
            .ok()?
            .connector(&connector);
        let (socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, builder.connect())
            .await
            .ok()?
            .ok()?;
        Some(socket)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one connection from token to close, read top to bottom"
    )]
    async fn session(&mut self) -> End {
        let Some(token) = self.ctx.secrets.get(BOT_TOKEN, TOKEN_ITEM).await else {
            return End::NoToken;
        };
        let mut url = match self.gateway_url().await {
            Ok(url) => url,
            Err(end) => return end,
        };
        url.set_query(Some("v=10&encoding=json"));
        let Some(mut socket) = Self::connect(&url).await else {
            self.ctx
                .status(false, "could not connect to Discord's gateway");
            return End::Reconnect;
        };
        let hello = match tokio::time::timeout(CONNECT_TIMEOUT, read(&mut socket)).await {
            Ok(Ok(frame)) if frame["op"] == 10 => frame,
            _ => return End::Reconnect,
        };
        let interval = Duration::from_millis(
            hello["d"]["heartbeat_interval"]
                .as_u64()
                .unwrap_or(41_250)
                .max(100),
        );
        let opening = match (&self.state.session_id, self.state.seq) {
            (Some(session), seq) => {
                json!({"op": 6, "d": {"token": token.as_str(), "session_id": session, "seq": seq}})
            }
            (None, _) => json!({"op": 2, "d": {
                "token": token.as_str(),
                "intents": INTENTS,
                "properties": {"os": std::env::consts::OS, "browser": "tilo", "device": "tilo"},
            }}),
        };
        let opening = Zeroizing::new(opening.to_string());
        drop(token);
        if socket
            .send(Message::text(opening.as_str().to_owned()))
            .await
            .is_err()
        {
            return End::Reconnect;
        }
        drop(opening);
        let start = Instant::now()
            .checked_add(interval.checked_div(2).unwrap_or_default())
            .unwrap_or_else(Instant::now);
        let mut beat = tokio::time::interval_at(start, interval);
        let mut acked = true;
        loop {
            tokio::select! {
                _ = beat.tick() => {
                    if !acked {
                        // No ack since the last beat: the connection is a zombie.
                        return End::Reconnect;
                    }
                    acked = false;
                    let seq = if self.state.seq == 0 { Value::Null } else { json!(self.state.seq) };
                    if socket.send(Message::text(json!({"op": 1, "d": seq}).to_string())).await.is_err() {
                        return End::Reconnect;
                    }
                    if self.dirty && self.last_checkpoint.elapsed() >= CHECKPOINT_EVERY {
                        self.flush_checkpoint();
                    }
                }
                frame = socket.next() => {
                    let Some(Ok(frame)) = frame else { return End::Reconnect };
                    if let Some((code, reason)) = frame.as_close() {
                        return self.closed(u16::from(code), reason);
                    }
                    let Some(text) = frame.as_text() else { continue };
                    let Ok(frame) = serde_json::from_str::<Value>(text) else { continue };
                    match frame["op"].as_u64() {
                        Some(0) => {
                            let seq = frame["s"].as_u64().unwrap_or(self.state.seq);
                            self.state.seq = self.state.seq.max(seq);
                            let kind = frame["t"].as_str().unwrap_or_default().to_owned();
                            self.dispatch(&kind, &frame["d"], seq).await;
                            if self.dirty && self.last_checkpoint.elapsed() >= CHECKPOINT_EVERY {
                                self.flush_checkpoint();
                            }
                        }
                        Some(1) => {
                            let seq = json!(self.state.seq);
                            if socket.send(Message::text(json!({"op": 1, "d": seq}).to_string())).await.is_err() {
                                return End::Reconnect;
                            }
                        }
                        Some(7) => return End::Reconnect,
                        Some(9) => {
                            if frame["d"].as_bool() != Some(true) {
                                self.forget_session();
                            }
                            return End::Reconnect;
                        }
                        Some(11) => acked = true,
                        _ => {}
                    }
                }
            }
        }
    }

    fn forget_session(&mut self) {
        self.state.session_id = None;
        self.state.resume_url = None;
        self.state.seq = 0;
        self.dirty = true;
    }

    fn closed(&mut self, code: u16, reason: &str) -> End {
        match code {
            4004 => End::Rejected(crate::rest::REJECTED_TOKEN.into()),
            4010..=4014 => End::Rejected(format!("Discord closed the gateway: {reason} ({code})")),
            4007 | 4009 => {
                self.forget_session();
                End::Reconnect
            }
            _ => End::Reconnect,
        }
    }

    /// Whether an event is about the bridged guild, and about the bridged
    /// channel when there is only one.
    fn ours(&self, data: &Value) -> bool {
        self.guild.as_ref().is_some_and(|guild| {
            data["guild_id"] == guild.id.as_str()
                && data["channel_id"]
                    .as_str()
                    .is_none_or(|channel| self.ctx.in_scope(&guild.place(channel).0))
        })
    }

    /// Tell the context which threads grew in the one channel bridged.
    fn note_threads(&self) {
        if let Some(guild) = &self.guild {
            self.ctx.threads_in(
                guild
                    .threads
                    .iter()
                    .map(|(thread, parent)| (thread.clone(), parent.clone())),
            );
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per gateway event")]
    async fn dispatch(&mut self, kind: &str, data: &Value, seq: u64) {
        match kind {
            "READY" => {
                self.state.session_id = data["session_id"].as_str().map(str::to_owned);
                self.state.resume_url = data["resume_gateway_url"].as_str().map(str::to_owned);
                self.state.session_started_s = tilo_clock::unix_now();
                data["user"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut self.bot_user);
                self.application = data["application"]["id"].as_str().map(str::to_owned);
                if self.guild_id.is_none() {
                    let guilds: Vec<&str> = data["guilds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|g| g["id"].as_str())
                        .collect();
                    match guilds.as_slice() {
                        [only] => self.guild_id = Some((*only).to_owned()),
                        [] => self
                            .ctx
                            .status(false, "the bot is in no Discord server yet"),
                        _ => self.ctx.status(
                            false,
                            "the bot is in several Discord servers; set DISCORD_GUILD_ID",
                        ),
                    }
                }
                self.ctx.set_guild(self.guild_id.clone());
                self.backfill_pending = !self.state.last_seen.is_empty();
                self.established = true;
                self.ctx.connected();
                self.flush_checkpoint();
            }
            "RESUMED" => {
                self.established = true;
                self.ctx.connected();
                self.flush_checkpoint();
            }
            "GUILD_CREATE" => {
                if self.guild_id.as_deref() != data["id"].as_str() {
                    return;
                }
                let guild =
                    Guild::from_create(data, &self.bot_user, self.ctx.config.cdn_base.as_str());
                self.emit(guild.layout(self.sequence(seq)));
                self.guild = Some(guild);
                self.note_threads();
                self.register_commands();
                if std::mem::take(&mut self.backfill_pending) {
                    self.backfill().await;
                    self.flush_checkpoint();
                }
            }
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "CHANNEL_DELETE" | "THREAD_CREATE"
            | "THREAD_UPDATE" | "THREAD_DELETE" | "THREAD_LIST_SYNC" | "GUILD_ROLE_CREATE"
            | "GUILD_ROLE_UPDATE" | "GUILD_ROLE_DELETE" => {
                if !self.ours(data) {
                    return;
                }
                let sequence = self.sequence(seq);
                let changed = self
                    .guild
                    .as_mut()
                    .is_some_and(|guild| guild.apply(kind, data));
                self.note_threads();
                if changed && let Some(guild) = &self.guild {
                    let layout = guild.layout(sequence);
                    self.emit(layout);
                }
            }
            "MESSAGE_CREATE" => {
                if !self.ours(data) {
                    return;
                }
                self.message(data);
            }
            "MESSAGE_UPDATE" => {
                if let Some(emit) = self
                    .guild
                    .as_ref()
                    .filter(|_| self.ours(data))
                    .and_then(|g| g.message_updated(data))
                {
                    self.emit(emit);
                }
            }
            "MESSAGE_DELETE" => {
                if !self.ours(data) {
                    return;
                }
                let channel = data["channel_id"].as_str().unwrap_or_default();
                let executor = self.delete_executor(channel).await;
                if let Some(guild) = &self.guild {
                    let emit = guild.message_deleted(
                        data["id"].as_str().unwrap_or_default(),
                        channel,
                        executor,
                    );
                    self.emit(emit);
                }
            }
            "MESSAGE_DELETE_BULK" => {
                if !self.ours(data) {
                    return;
                }
                let channel = data["channel_id"].as_str().unwrap_or_default();
                if let Some(guild) = &self.guild {
                    for id in data["ids"].as_array().into_iter().flatten() {
                        let emit =
                            guild.message_deleted(id.as_str().unwrap_or_default(), channel, None);
                        self.emit(emit);
                    }
                }
            }
            "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
                let sequence = self.sequence(seq);
                if let Some(emit) = self
                    .guild
                    .as_ref()
                    .filter(|_| self.ours(data))
                    .and_then(|g| g.reaction(data, kind == "MESSAGE_REACTION_ADD", sequence))
                {
                    self.emit(emit);
                }
            }
            "GUILD_MEMBER_UPDATE"
            | "GUILD_MEMBER_REMOVE"
            | "GUILD_BAN_ADD"
            | "GUILD_BAN_REMOVE" => {
                let sequence = self.sequence(seq);
                if let Some(emit) = self
                    .guild
                    .as_ref()
                    .filter(|_| self.ours(data))
                    .and_then(|g| g.member_changed(kind, data, sequence))
                {
                    self.emit(emit);
                }
            }
            "INTERACTION_CREATE" => {
                if let Some(emit) = self
                    .guild
                    .as_ref()
                    .filter(|_| self.ours(data))
                    .and_then(|g| g.command(data))
                {
                    let name = emit.payload["name"].as_str().unwrap_or_default().to_owned();
                    self.emit(emit);
                    self.answer_interaction(data, &name);
                }
            }
            _ => {}
        }
    }

    fn message(&mut self, data: &Value) {
        let Some(guild) = &self.guild else { return };
        let Some(emit) = guild.message_created(data) else {
            return;
        };
        let channel = data["channel_id"].as_str().unwrap_or_default().to_owned();
        let parent = guild.threads.get(&channel).cloned();
        self.emit(emit);
        let id = data["id"].as_str().unwrap_or_default().to_owned();
        let newer = self.state.last_seen.get(&channel).is_none_or(|seen| {
            snowflake_ms(&seen.message) <= snowflake_ms(&id) && seen.message != id
        });
        if newer {
            self.state.last_seen.insert(
                channel,
                Seen {
                    message: id,
                    parent,
                },
            );
            self.dirty = true;
        }
    }

    /// Read every channel seen before back from its last seen message, at
    /// most [`BACKFILL_LIMIT`] messages each. What was delivered already
    /// has the same event id and is dropped by the host's log.
    async fn backfill(&mut self) {
        let seen: Vec<(String, Seen)> = self
            .state
            .last_seen
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (channel, last) in seen {
            if let (Some(parent), Some(guild)) = (&last.parent, self.guild.as_mut()) {
                guild
                    .threads
                    .entry(channel.clone())
                    .or_insert_with(|| parent.clone());
            }
            let answer = self
                .ctx
                .rest
                .call(
                    Method::GET,
                    &format!("/channels/{}/messages", segment(&channel)),
                    &[
                        ("after", last.message.clone()),
                        ("limit", BACKFILL_LIMIT.to_string()),
                    ],
                    Body::Empty,
                    Auth::Bot,
                    None,
                )
                .await;
            match answer {
                Ok(Value::Array(mut messages)) => {
                    messages.sort_by_key(|m| {
                        m["id"]
                            .as_str()
                            .and_then(|id| id.parse::<u64>().ok())
                            .unwrap_or(0)
                    });
                    for message in &messages {
                        self.message(message);
                    }
                }
                Err(RestError::Refused {
                    status: 403 | 404, ..
                }) => {
                    self.state.last_seen.remove(&channel);
                    self.dirty = true;
                }
                _ => {}
            }
        }
    }

    /// Who deleted a message, when the audit log says a moderator just
    /// deleted one in `channel`. Discord logs only deletions of someone
    /// else's message, and folds repeated ones into one entry, so this is
    /// best effort.
    async fn delete_executor(&self, channel: &str) -> Option<String> {
        let guild = self.guild.as_ref()?;
        let answer = self
            .ctx
            .rest
            .call(
                Method::GET,
                &format!("/guilds/{}/audit-logs", segment(&guild.id)),
                &[("action_type", "72".into()), ("limit", "5".into())],
                Body::Empty,
                Auth::Bot,
                None,
            )
            .await
            .ok()?;
        let recent = tilo_clock::unix_now_ms_i64().saturating_sub(15_000);
        answer["audit_log_entries"]
            .as_array()?
            .iter()
            .find(|entry| {
                entry["options"]["channel_id"] == channel
                    && snowflake_ms(entry["id"].as_str().unwrap_or_default()) >= recent
            })
            .and_then(|entry| entry["user_id"].as_str().map(str::to_owned))
    }

    /// Register `/link` and `/unlink` in the guild, once per process.
    fn register_commands(&mut self) {
        if self.ctx.config.read_only
            || self.ctx.config.only_channel.is_some()
            || self.commands_registered
        {
            return;
        }
        let (Some(application), Some(guild)) = (self.application.clone(), self.guild_id.clone())
        else {
            return;
        };
        self.commands_registered = true;
        let rest = Arc::clone(&self.ctx.rest);
        tokio::spawn(async move {
            let commands = json!([
                {"name": "link", "type": 1, "description": "Link your Discord account to your Tilo account",
                 "options": [{"type": 3, "name": "code", "description": "The code Tilo shows you", "required": true}]},
                {"name": "unlink", "type": 1, "description": "Unlink your Discord account from Tilo"},
            ]);
            let _ = rest
                .call(
                    Method::PUT,
                    &format!(
                        "/applications/{}/guilds/{}/commands",
                        segment(&application),
                        segment(&guild)
                    ),
                    &[],
                    Body::Json(commands),
                    Auth::Bot,
                    None,
                )
                .await;
        });
    }

    /// Answer a slash command ephemerally. Whether a link succeeds is the
    /// bridge's decision, made later, so the answer only confirms receipt.
    fn answer_interaction(&self, data: &Value, name: &str) {
        if self.ctx.config.read_only {
            return;
        }
        let (Some(id), Some(token)) = (data["id"].as_str(), data["token"].as_str()) else {
            return;
        };
        let content = if name == "link" {
            "Got it. If the code is right and has not expired, your Discord account is linked to your Tilo account in a moment."
        } else {
            "Got it. Your Discord account will be unlinked from your Tilo account in a moment."
        };
        let path = format!("/interactions/{}/{}/callback", segment(id), segment(token));
        let rest = Arc::clone(&self.ctx.rest);
        tokio::spawn(async move {
            let _ = rest
                .call(
                    Method::POST,
                    &path,
                    &[],
                    Body::Json(json!({"type": 4, "data": {"content": content, "flags": 64}})),
                    Auth::InPath,
                    None,
                )
                .await;
        });
    }
}

async fn read(socket: &mut Socket) -> Result<Value, ()> {
    loop {
        let frame = socket.next().await.ok_or(())?.map_err(|_| ())?;
        if frame.is_close() {
            return Err(());
        }
        if let Some(text) = frame.as_text() {
            return serde_json::from_str(text).map_err(|_| ());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intents_are_the_six_the_bridge_needs() {
        assert_eq!(INTENTS, 34_311);
    }

    #[test]
    fn checkpoints_round_trip_and_tolerate_an_empty_start() {
        let checkpoint = Checkpoint {
            session_id: Some("s".into()),
            resume_url: Some("wss://gateway-us-east1-b.discord.gg".into()),
            seq: 9,
            session_started_s: 1_791_801_000,
            last_seen: BTreeMap::from([(
                "1500".into(),
                Seen {
                    message: "9".into(),
                    parent: Some("1101".into()),
                },
            )]),
        };
        let back: Checkpoint =
            serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
        assert_eq!(back, checkpoint);
        let empty: Checkpoint = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, Checkpoint::default());
    }
}
