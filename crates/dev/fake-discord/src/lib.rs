//! A stand-in for Discord on loopback: the REST routes, the CDN and the
//! gateway the connector's outside worker uses, with one guild in memory.
//!
//! Tests drive it from both sides. They act as Discord users through the
//! methods below ([`FakeDiscord::post`], [`FakeDiscord::react`],
//! [`FakeDiscord::slash`], ...), each of which changes the guild and
//! dispatches the matching gateway event, and they read what the worker did
//! through the recorded requests and the guild's state. Scripts make the
//! awkward cases happen on purpose: a 429 on a route, a refused call, a
//! dropped gateway, a refused resume, a revoked token.
//!
//! The REST side mutates the guild the way Discord does and dispatches the
//! events Discord would (a webhook message comes back as `MESSAGE_CREATE`),
//! so echo suppression is exercised for real.
//!
//! Voice is faked too: a voice gateway and a UDP media path on loopback,
//! with transport encryption and DAVE for real (see [`voice`]'s notes in
//! the source). Fake users join, mute, leave and say Opus frames through
//! [`FakeDiscord::voice_join`] and friends; what the bot said is decrypted
//! and kept for [`FakeDiscord::voice_heard`]. [`tone`] makes and tells
//! tones.

#![allow(
    clippy::indexing_slicing,
    reason = "the fake indexes and assigns into JSON objects it built itself"
)]

mod gateway;
mod rest;
mod rtp;
mod time;
pub mod tone;
mod voice;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

pub use rest::Recorded;
pub use time::now_ms;
pub use voice::Heard;

/// The bot token the fake accepts until [`FakeDiscord::revoke_token`].
pub const TOKEN: &str = "fake.bot.token";
/// The seeded guild, which is also the id of its `@everyone` role.
pub const GUILD: &str = "1000";
/// The bot user, and the application it belongs to.
pub const BOT_USER: &str = "2000";
/// A category `chat` holding the text channel `general` and the voice
/// channel `voice`, and a text channel `goal` outside any category.
pub const CATEGORY_CHAT: &str = "1100";
pub const CHANNEL_GENERAL: &str = "1101";
pub const CHANNEL_VOICE: &str = "1102";
pub const CHANNEL_GOAL: &str = "1103";

/// Discord's epoch, the zero of a snowflake's timestamp.
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// A Discord user the test acts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
}

/// Discord's permission bits for joining a voice channel and speaking.
pub const CONNECT_AND_SPEAK: u64 = (1 << 20) | (1 << 21);

impl User {
    #[must_use]
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            avatar: None,
        }
    }

    #[must_use]
    pub fn with_avatar(mut self, hash: &str) -> Self {
        self.avatar = Some(hash.to_owned());
        self
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "username": self.name.to_lowercase(),
            "global_name": self.name,
            "avatar": self.avatar,
            "bot": false,
        })
    }
}

/// What a scripted route answers instead of doing its work.
#[derive(Clone, Debug, PartialEq)]
pub enum Scripted {
    /// HTTP 429 with `retry_after` and the rate-limit headers.
    RateLimited { retry_after_ms: u64, global: bool },
    /// An error status with Discord's error body.
    Status {
        status: u16,
        code: u32,
        message: String,
    },
}

#[derive(Clone, Debug)]
struct Script {
    method: String,
    path_prefix: String,
    times: u32,
    answer: Scripted,
}

/// A rate-limit window: at most `limit` matching requests per `window`,
/// counted down in the headers the way Discord does.
#[derive(Clone, Debug)]
struct Window {
    method: String,
    path_prefix: String,
    limit: u32,
    length: Duration,
    started: std::time::Instant,
    used: u32,
}

#[derive(Clone, Debug)]
struct Webhook {
    id: String,
    channel: String,
    token: String,
    name: String,
}

/// A frame for the live gateway connection.
#[derive(Clone, Debug)]
enum Outgoing {
    Text(String),
    Close(u16, String),
}

#[derive(Debug, Default)]
struct Session {
    id: String,
    seq: u64,
    /// Every dispatch of the session, for a resume to replay.
    log: Vec<(u64, String)>,
}

#[derive(Debug, Default)]
struct Gateway {
    session: Option<Session>,
    live: Option<mpsc::UnboundedSender<Outgoing>>,
    refuse_next_resume: bool,
    identifies: u32,
    resumes: u32,
    sessions_started: u32,
    intents: Option<u64>,
}

#[derive(Default)]
struct State {
    token: String,
    rest_origin: String,
    gateway_url: String,
    channels: Vec<Value>,
    roles: Vec<Value>,
    messages: Vec<Value>,
    webhooks: Vec<Webhook>,
    member_roles: HashMap<String, Vec<String>>,
    bans: Vec<String>,
    /// The guild's custom emoji, as Discord's emoji objects.
    emojis: Vec<Value>,
    bot_reactions: Vec<(String, String)>,
    audit: Vec<Value>,
    cdn: HashMap<String, (String, Vec<u8>)>,
    requests: Vec<Recorded>,
    scripts: Vec<Script>,
    windows: Vec<Window>,
    commands: Value,
    interaction_replies: Vec<Value>,
    /// Edits of interaction replies (`PATCH .../messages/@original`), as
    /// `{"interaction", "edit"}`.
    interaction_edits: Vec<Value>,
    /// Who is signed in to Discord in the browser: the account the consent
    /// page authorizes.
    oauth_user: Option<User>,
    /// Authorization codes the consent page handed out, each to its
    /// account and redirect address, good once.
    oauth_codes: HashMap<String, (User, String)>,
    /// Access tokens the token endpoint issued, by token.
    oauth_tokens: HashMap<String, User>,
    last_id: u64,
    gateway: Gateway,
    voice: voice::Voice,
}

impl State {
    /// A fresh snowflake carrying the current time, so the worker's
    /// duplicate check (which compares creation times) sees real ones.
    fn snowflake(&mut self) -> String {
        let since = time::now_ms().saturating_sub(DISCORD_EPOCH_MS);
        let candidate = since << 22;
        self.last_id = candidate.max(self.last_id.saturating_add(1));
        self.last_id.to_string()
    }

    /// Send a dispatch on the current session, if there is one, and keep it
    /// for a resume. Events while no worker is connected wait in the log.
    fn dispatch(&mut self, kind: &str, data: Value) {
        let Some(session) = self.gateway.session.as_mut() else {
            return;
        };
        session.seq = session.seq.saturating_add(1);
        let mut frame = json!({"op": 0, "t": kind, "s": session.seq});
        frame["d"] = data;
        let frame = frame.to_string();
        session.log.push((session.seq, frame.clone()));
        if let Some(live) = &self.gateway.live {
            let _ = live.send(Outgoing::Text(frame));
        }
    }

    fn channel(&self, id: &str) -> Option<&Value> {
        self.channels.iter().find(|c| c["id"] == id)
    }

    fn channel_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.channels.iter_mut().find(|c| c["id"] == id)
    }

    fn message_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.messages.iter_mut().find(|m| m["id"] == id)
    }

    fn guild_json(&self) -> Value {
        let (threads, channels): (Vec<&Value>, Vec<&Value>) = self
            .channels
            .iter()
            .partition(|c| matches!(c["type"].as_u64(), Some(10..=12)));
        let (voice_states, members) = self.voice_guild_parts();
        json!({
            "id": GUILD,
            "name": "Fake Guild",
            "owner_id": "3000",
            "channels": channels,
            "threads": threads,
            "roles": self.roles,
            "emojis": self.emojis,
            "members": members,
            "voice_states": voice_states,
            "member_count": 3,
            "unavailable": false,
        })
    }

    fn message_json(
        &mut self,
        channel: &str,
        author: &User,
        content: &str,
        reply_to: Option<&str>,
    ) -> Value {
        let id = self.snowflake();
        let mut message = json!({
            "id": id,
            "channel_id": channel,
            "guild_id": GUILD,
            "type": 0,
            "content": content,
            "author": author.json(),
            "timestamp": time::rfc3339(time::now_ms()),
            "edited_timestamp": null,
            "attachments": [],
            "mentions": [],
        });
        if let Some(parent) = reply_to {
            message["type"] = json!(19);
            message["message_reference"] =
                json!({"message_id": parent, "channel_id": channel, "guild_id": GUILD});
            if let Some(original) = self.messages.iter().find(|m| m["id"] == parent) {
                message["referenced_message"] = original.clone();
            }
        }
        message
    }

    /// `emoji` as a reaction event carries it: a Unicode emoji by itself,
    /// a custom one (`:name:` as a person picks it, `name:id` as the
    /// reaction routes name it) with its id and animation. A name the guild
    /// has no emoji of gets a made-up id, as an emoji of another server
    /// would have.
    fn emoji_json(&self, emoji: &str) -> Value {
        let named = emoji
            .strip_prefix(':')
            .and_then(|e| e.strip_suffix(':'))
            .map(|name| (name, None))
            .or_else(|| emoji.split_once(':').map(|(name, id)| (name, Some(id))));
        let Some((name, id)) = named else {
            return json!({"id": null, "name": emoji});
        };
        self.emojis
            .iter()
            .find(|e| e["name"] == name && id.is_none_or(|id| e["id"] == id))
            .map_or_else(
                || json!({"id": id.unwrap_or("5000"), "name": name, "animated": false}),
                |e| json!({"id": e["id"], "name": e["name"], "animated": e["animated"]}),
            )
    }

    fn with_member(message: &Value, author: &User) -> Value {
        let mut event = message.clone();
        event["member"] = json!({"nick": author.name, "roles": []});
        event
    }
}

struct Shared {
    state: Mutex<State>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock_recover()
    }
}

/// A running fake Discord. Dropping it stops its servers.
pub struct FakeDiscord {
    shared: Arc<Shared>,
    rest: SocketAddr,
    gateway: SocketAddr,
    voice: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FakeDiscord {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FakeDiscord {
    /// Start the REST and gateway servers on loopback, with the seeded guild.
    ///
    /// # Errors
    /// A loopback port could not be bound.
    pub async fn start() -> std::io::Result<Self> {
        Self::start_with_channels(seed_channels()).await
    }

    /// [`Self::start`] with the guild holding `channels` (Discord's channel
    /// objects, `guild_id` filled in when absent) instead of the seeded ones.
    ///
    /// # Errors
    /// A loopback port could not be bound.
    pub async fn start_with_channels(channels: Vec<Value>) -> std::io::Result<Self> {
        let fake = Self::start_on(
            "127.0.0.1:0".parse().map_err(std::io::Error::other)?,
            "127.0.0.1:0".parse().map_err(std::io::Error::other)?,
        )
        .await?;
        fake.shared.lock().channels = channels
            .into_iter()
            .map(|mut channel| {
                if channel.get("guild_id").is_none() {
                    channel["guild_id"] = json!(GUILD);
                }
                if channel.get("permission_overwrites").is_none() {
                    channel["permission_overwrites"] = json!([]);
                }
                channel
            })
            .collect();
        Ok(fake)
    }

    /// [`Self::start`] on the given addresses.
    ///
    /// # Errors
    /// An address could not be bound.
    pub async fn start_on(rest: SocketAddr, gateway: SocketAddr) -> std::io::Result<Self> {
        let rest_listener = tokio::net::TcpListener::bind(rest).await?;
        let gateway_listener = tokio::net::TcpListener::bind(gateway).await?;
        let rest = rest_listener.local_addr()?;
        let gateway = gateway_listener.local_addr()?;
        let voice_listener = tokio::net::TcpListener::bind((gateway.ip(), 0)).await?;
        let voice = voice_listener.local_addr()?;
        let udp = std::net::UdpSocket::bind((gateway.ip(), 0))?;
        udp.set_nonblocking(true)?;
        let udp_port = udp.local_addr()?.port();
        let udp_send = Arc::new(udp.try_clone()?);
        let udp = tokio::net::UdpSocket::from_std(udp)?;
        let state = State {
            token: TOKEN.to_owned(),
            rest_origin: format!("http://{rest}"),
            gateway_url: format!("ws://{gateway}"),
            channels: seed_channels(),
            roles: vec![
                json!({"id": GUILD, "name": "@everyone", "permissions": "3263553", "color": 0, "position": 0, "managed": false}),
                json!({"id": "1200", "name": "Tilo Bridge", "permissions": "8", "color": 0, "position": 1, "managed": true}),
            ],
            commands: json!([]),
            // The bot has the bridge's managed role, which has Administrator.
            voice: voice::Voice::new(
                voice.to_string(),
                udp_port,
                udp_send,
                vec!["1200".to_owned()],
            ),
            ..State::default()
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
        });
        let tasks = vec![
            tokio::spawn(rest::serve(Arc::clone(&shared), rest_listener)),
            tokio::spawn(gateway::serve(Arc::clone(&shared), gateway_listener)),
            tokio::spawn(voice::serve(Arc::clone(&shared), voice_listener, udp)),
        ];
        Ok(Self {
            shared,
            rest,
            gateway,
            voice,
            tasks,
        })
    }

    /// `http://127.0.0.1:<port>/api/v10`, the worker's REST base.
    #[must_use]
    pub fn api_base(&self) -> String {
        format!("http://{}/api/v10", self.rest)
    }

    /// `http://127.0.0.1:<port>/cdn`, the worker's CDN base.
    #[must_use]
    pub fn cdn_base(&self) -> String {
        format!("http://{}/cdn", self.rest)
    }

    /// The gateway URL `/gateway/bot` names.
    #[must_use]
    pub fn gateway_url(&self) -> String {
        format!("ws://{}", self.gateway)
    }

    /// The voice server's endpoint, as `VOICE_SERVER_UPDATE` names it.
    #[must_use]
    pub fn voice_endpoint(&self) -> String {
        self.voice.to_string()
    }

    /// `author` posts `content` in `channel`; returns the message id.
    pub fn post(&self, channel: &str, author: &User, content: &str) -> String {
        self.post_message(channel, author, content, None, true)
    }

    pub fn reply(&self, channel: &str, author: &User, content: &str, reply_to: &str) -> String {
        self.post_message(channel, author, content, Some(reply_to), true)
    }

    /// A message that lands in the channel's history without the gateway
    /// telling anyone: what a worker misses while it is disconnected and its
    /// session cannot be resumed.
    pub fn post_unseen(&self, channel: &str, author: &User, content: &str) -> String {
        self.post_message(channel, author, content, None, false)
    }

    /// A message `author` wrote at `at_ms` (Unix ms), long before anyone
    /// connected: in the channel's history, its id carrying that time, and
    /// never announced on the gateway.
    pub fn post_written(&self, channel: &str, author: &User, content: &str, at_ms: u64) -> String {
        let mut state = self.shared.lock();
        let mut message = state.message_json(channel, author, content, None);
        let since = at_ms.saturating_sub(DISCORD_EPOCH_MS);
        // Ids differ by their low bits, as Discord's do within one millisecond.
        let sequence = u64::try_from(state.messages.len()).unwrap_or(0) & 0xfff;
        let id = ((since << 22) | sequence).to_string();
        message["id"] = json!(id);
        message["timestamp"] = json!(time::rfc3339(at_ms));
        state.messages.push(message);
        id
    }

    /// A message posted through `webhook` as `name` at `at_ms`, before
    /// anyone connected, as a bridge posts: in the channel's history like
    /// [`Self::post_written`]'s.
    pub fn post_written_through(
        &self,
        webhook: &str,
        channel: &str,
        name: &str,
        content: &str,
        at_ms: u64,
    ) -> String {
        let id = self.post_written(channel, &User::new(webhook, name), content, at_ms);
        if let Some(message) = self
            .shared
            .lock()
            .messages
            .iter_mut()
            .find(|m| m["id"] == id.as_str())
        {
            message["webhook_id"] = json!(webhook);
            message["author"]["bot"] = json!(true);
        }
        id
    }

    fn post_message(
        &self,
        channel: &str,
        author: &User,
        content: &str,
        reply_to: Option<&str>,
        dispatch: bool,
    ) -> String {
        let mut state = self.shared.lock();
        let message = state.message_json(channel, author, content, reply_to);
        let id = message["id"].as_str().unwrap_or_default().to_owned();
        state.messages.push(message.clone());
        if dispatch {
            state.dispatch("MESSAGE_CREATE", State::with_member(&message, author));
        }
        id
    }

    /// `author` posts a file; its bytes are served from the fake CDN.
    pub fn post_file(
        &self,
        channel: &str,
        author: &User,
        content: &str,
        file: (&str, &str, &[u8]),
    ) -> String {
        let (name, content_type, bytes) = file;
        let mut state = self.shared.lock();
        let mut message = state.message_json(channel, author, content, None);
        let id = message["id"].as_str().unwrap_or_default().to_owned();
        let path = format!("attachments/{channel}/{id}/{name}");
        let url = format!("{}/cdn/{path}", state.rest_origin);
        state
            .cdn
            .insert(path, (content_type.to_owned(), bytes.to_vec()));
        let mut attachment = json!({
            "id": id, "filename": name, "size": bytes.len(), "content_type": content_type,
            "url": url, "proxy_url": url,
        });
        // Discord sizes the images it stores; a bridge lays them out by it.
        if let Some((width, height)) = png_size(bytes) {
            attachment["width"] = json!(width);
            attachment["height"] = json!(height);
        }
        message["attachments"] = json!([attachment]);
        state.messages.push(message.clone());
        state.dispatch("MESSAGE_CREATE", State::with_member(&message, author));
        id
    }

    /// Serve `bytes` at `<cdn base>/<path>`; returns the URL.
    pub fn put_cdn(&self, path: &str, content_type: &str, bytes: &[u8]) -> String {
        let mut state = self.shared.lock();
        state
            .cdn
            .insert(path.to_owned(), (content_type.to_owned(), bytes.to_vec()));
        let url = format!("{}/cdn/{path}", state.rest_origin);
        drop(state);
        url
    }

    pub fn edit(&self, message: &str, content: &str) {
        let mut state = self.shared.lock();
        let Some(stored) = state.message_mut(message) else {
            return;
        };
        stored["content"] = json!(content);
        stored["edited_timestamp"] = json!(time::rfc3339(time::now_ms()));
        let event = stored.clone();
        state.dispatch("MESSAGE_UPDATE", event);
        drop(state);
    }

    /// The message is deleted, by `executor` when that is a moderator
    /// deleting someone else's message (which Discord writes to the audit
    /// log).
    pub fn delete(&self, message: &str, executor: Option<&str>) {
        let mut state = self.shared.lock();
        let Some(index) = state.messages.iter().position(|m| m["id"] == message) else {
            return;
        };
        let removed = state.messages.remove(index);
        if let Some(executor) = executor {
            let id = state.snowflake();
            state.audit.insert(
                0,
                json!({
                    "id": id, "action_type": 72, "user_id": executor,
                    "target_id": removed["author"]["id"],
                    "options": {"channel_id": removed["channel_id"], "count": "1"},
                }),
            );
        }
        state.dispatch(
            "MESSAGE_DELETE",
            json!({"id": message, "channel_id": removed["channel_id"], "guild_id": GUILD}),
        );
        drop(state);
    }

    /// `user` adds `emoji` (unicode, or a custom emoji as `:name:`, the
    /// guild's of that name when it has one) to `message`.
    pub fn react(&self, channel: &str, message: &str, user: &User, emoji: &str) {
        let mut state = self.shared.lock();
        let event = json!({
            "user_id": user.id, "channel_id": channel, "message_id": message, "guild_id": GUILD,
            "member": {"nick": user.name, "roles": [], "user": user.json()},
            "emoji": state.emoji_json(emoji),
        });
        state.dispatch("MESSAGE_REACTION_ADD", event);
    }

    /// `user` takes `emoji` off `message`.
    pub fn unreact(&self, channel: &str, message: &str, user: &User, emoji: &str) {
        let mut state = self.shared.lock();
        let event = json!({
            "user_id": user.id, "channel_id": channel, "message_id": message, "guild_id": GUILD,
            "emoji": state.emoji_json(emoji),
        });
        state.dispatch("MESSAGE_REACTION_REMOVE", event);
    }

    /// The guild's custom emoji become `emojis`, `(name, id, animated)`,
    /// as a server's moderator uploads or deletes them: in the guild a
    /// worker connecting later is sent, and as `GUILD_EMOJIS_UPDATE` to one
    /// connected now.
    pub fn set_emojis(&self, emojis: &[(&str, &str, bool)]) {
        let mut state = self.shared.lock();
        state.emojis = emojis
            .iter()
            .map(|(name, id, animated)| {
                json!({"id": id, "name": name, "animated": animated, "available": true, "require_colons": true, "managed": false})
            })
            .collect();
        let update = json!({"guild_id": GUILD, "emojis": state.emojis});
        state.dispatch("GUILD_EMOJIS_UPDATE", update);
    }

    /// The guild's custom emoji, as Discord's emoji objects.
    #[must_use]
    pub fn emojis(&self) -> Vec<Value> {
        self.shared.lock().emojis.clone()
    }

    /// A user starts a thread on `message` in `parent`; returns the thread
    /// id, which is the message's id as on Discord.
    pub fn start_thread(&self, parent: &str, message: &str, name: &str) -> String {
        let thread = json!({"id": message, "type": 11, "name": name, "parent_id": parent, "guild_id": GUILD, "position": 0});
        let mut state = self.shared.lock();
        state.channels.push(thread.clone());
        state.dispatch("THREAD_CREATE", thread);
        drop(state);
        message.to_owned()
    }

    /// `user` runs `/name`; returns the interaction id.
    pub fn slash(&self, user: &User, name: &str) -> String {
        let mut state = self.shared.lock();
        let id = state.snowflake();
        let options: Vec<Value> = Vec::new();
        state.dispatch(
            "INTERACTION_CREATE",
            json!({
                "id": id, "application_id": BOT_USER, "type": 2, "token": format!("itoken-{id}"),
                "guild_id": GUILD, "channel_id": CHANNEL_GENERAL,
                "member": {"user": user.json(), "nick": user.name, "roles": []},
                "data": {"id": "cmd", "name": name, "type": 1, "options": options},
            }),
        );
        drop(state);
        id
    }

    /// Someone deletes a channel in Discord.
    pub fn delete_channel(&self, channel: &str) {
        let mut state = self.shared.lock();
        let Some(index) = state.channels.iter().position(|c| c["id"] == channel) else {
            return;
        };
        let removed = state.channels.remove(index);
        state.dispatch("CHANNEL_DELETE", removed);
        drop(state);
    }

    pub fn ban(&self, user: &User) {
        let event = json!({"guild_id": GUILD, "user": user.json()});
        let mut state = self.shared.lock();
        state.bans.push(user.id.clone());
        state.dispatch("GUILD_BAN_ADD", event);
        drop(state);
    }

    /// Dispatch any gateway event.
    pub fn dispatch(&self, kind: &str, data: Value) {
        self.shared.lock().dispatch(kind, data);
    }

    /// The next `times` requests whose method is `method` and whose path
    /// (after `/api/v10`) starts with `path_prefix` get a 429.
    pub fn rate_limit(&self, method: &str, path_prefix: &str, times: u32, retry_after_ms: u64) {
        self.script(
            method,
            path_prefix,
            times,
            Scripted::RateLimited {
                retry_after_ms,
                global: false,
            },
        );
    }

    /// The next `times` matching requests fail with `status` and Discord's
    /// error body.
    pub fn fail(
        &self,
        method: &str,
        path_prefix: &str,
        times: u32,
        status: u16,
        code: u32,
        message: &str,
    ) {
        self.script(
            method,
            path_prefix,
            times,
            Scripted::Status {
                status,
                code,
                message: message.to_owned(),
            },
        );
    }

    /// Matching requests share one bucket of `limit` calls per `window_ms`,
    /// as Discord's real buckets do: the headers count it down and a call
    /// beyond it gets a 429 until the window ends.
    pub fn limit(&self, method: &str, path_prefix: &str, limit: u32, window_ms: u64) {
        self.shared.lock().windows.push(Window {
            method: method.to_owned(),
            path_prefix: path_prefix.to_owned(),
            limit,
            length: Duration::from_millis(window_ms),
            started: std::time::Instant::now(),
            used: 0,
        });
    }

    fn script(&self, method: &str, path_prefix: &str, times: u32, answer: Scripted) {
        self.shared.lock().scripts.push(Script {
            method: method.to_owned(),
            path_prefix: path_prefix.to_owned(),
            times,
            answer,
        });
    }

    /// Close the live gateway connection as a network failure would; the
    /// session stays resumable.
    pub fn drop_gateway(&self) {
        self.close_gateway(4000, "unknown error");
    }

    /// Close the live gateway connection with `code`.
    pub fn close_gateway(&self, code: u16, reason: &str) {
        let mut state = self.shared.lock();
        if let Some(live) = state.gateway.live.take() {
            let _ = live.send(Outgoing::Close(code, reason.to_owned()));
        }
    }

    /// The next resume is answered with Invalid Session, as when Discord has
    /// dropped the session.
    pub fn refuse_next_resume(&self) {
        self.shared.lock().gateway.refuse_next_resume = true;
    }

    /// The bot token stops working, as when it is reset in the developer
    /// portal: REST answers 401, an identify is closed with 4004, and the
    /// live connection is closed with 4004.
    pub fn revoke_token(&self, new_token: &str) {
        let mut state = self.shared.lock();
        new_token.clone_into(&mut state.token);
        state.gateway.session = None;
        if let Some(live) = state.gateway.live.take() {
            let _ = live.send(Outgoing::Close(4004, "Authentication failed.".into()));
        }
    }

    /// Every REST request so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.lock().requests.clone()
    }

    /// The requests with `method` whose path starts with `path_prefix`.
    #[must_use]
    pub fn requests_to(&self, method: &str, path_prefix: &str) -> Vec<Recorded> {
        self.shared
            .lock()
            .requests
            .iter()
            .filter(|r| r.method == method && r.path.starts_with(path_prefix))
            .cloned()
            .collect()
    }

    /// The messages in `channel` (or thread), oldest first.
    #[must_use]
    pub fn messages_in(&self, channel: &str) -> Vec<Value> {
        self.shared
            .lock()
            .messages
            .iter()
            .filter(|m| m["channel_id"] == channel)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn message(&self, id: &str) -> Option<Value> {
        self.shared
            .lock()
            .messages
            .iter()
            .find(|m| m["id"] == id)
            .cloned()
    }

    #[must_use]
    pub fn channels(&self) -> Vec<Value> {
        self.shared.lock().channels.clone()
    }

    #[must_use]
    pub fn channel(&self, id: &str) -> Option<Value> {
        self.shared.lock().channel(id).cloned()
    }

    #[must_use]
    pub fn roles(&self) -> Vec<Value> {
        self.shared.lock().roles.clone()
    }

    #[must_use]
    pub fn member_roles(&self, user: &str) -> Vec<String> {
        self.shared
            .lock()
            .member_roles
            .get(user)
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn bans(&self) -> Vec<String> {
        self.shared.lock().bans.clone()
    }

    /// `(message, emoji)` the bot user has reacted with, the emoji as the
    /// reaction route named it: Unicode, or `name:id` for a custom one.
    #[must_use]
    pub fn bot_reactions(&self) -> Vec<(String, String)> {
        self.shared.lock().bot_reactions.clone()
    }

    /// The emoji the bot user has reacted to `message` with, oldest first.
    #[must_use]
    pub fn bot_reactions_on(&self, message: &str) -> Vec<String> {
        self.bot_reactions()
            .into_iter()
            .filter(|(on, _)| on == message)
            .map(|(_, emoji)| emoji)
            .collect()
    }

    /// `(webhook id, channel)` of every webhook.
    #[must_use]
    pub fn webhooks(&self) -> Vec<(String, String)> {
        self.shared
            .lock()
            .webhooks
            .iter()
            .map(|w| (w.id.clone(), w.channel.clone()))
            .collect()
    }

    /// A webhook's token, as the test's stand-in for the host's secret store.
    #[must_use]
    pub fn webhook_token(&self, webhook: &str) -> Option<String> {
        self.shared
            .lock()
            .webhooks
            .iter()
            .find(|w| w.id == webhook)
            .map(|w| w.token.clone())
    }

    /// Create a webhook as if the bridge had made it earlier; returns
    /// `(id, token)`.
    pub fn create_webhook(&self, channel: &str) -> (String, String) {
        let mut state = self.shared.lock();
        let id = state.snowflake();
        let token = format!("whtoken-{id}");
        state.webhooks.push(Webhook {
            id: id.clone(),
            channel: channel.to_owned(),
            token: token.clone(),
            name: "Tilo".into(),
        });
        drop(state);
        (id, token)
    }

    /// The ephemeral answers the bot gave to interactions.
    #[must_use]
    pub fn interaction_replies(&self) -> Vec<Value> {
        self.shared.lock().interaction_replies.clone()
    }

    /// The bot's edits of its interaction replies, oldest first, as
    /// `{"interaction", "edit"}`.
    #[must_use]
    pub fn interaction_edits(&self) -> Vec<Value> {
        self.shared.lock().interaction_edits.clone()
    }

    /// Sign `user` in to Discord: the consent page (`/oauth2/authorize`)
    /// authorizes their account.
    pub fn sign_in(&self, user: User) {
        self.shared.lock().oauth_user = Some(user);
    }

    /// The guild's registered slash commands.
    #[must_use]
    pub fn commands(&self) -> Value {
        self.shared.lock().commands.clone()
    }

    #[must_use]
    pub fn identifies(&self) -> u32 {
        self.shared.lock().gateway.identifies
    }

    #[must_use]
    pub fn resumes(&self) -> u32 {
        self.shared.lock().gateway.resumes
    }

    /// The intents of the last identify.
    #[must_use]
    pub fn intents(&self) -> Option<u64> {
        self.shared.lock().gateway.intents
    }

    /// Whether a worker holds a live, identified gateway connection.
    #[must_use]
    pub fn connected(&self) -> bool {
        let state = self.shared.lock();
        state.gateway.live.is_some() && state.gateway.session.is_some()
    }

    /// `user` enters voice channel `channel` (dispatching their voice
    /// state); in the bot's channel they join its call as a DAVE client.
    pub fn voice_join(&self, user: &User, channel: &str) {
        self.shared.lock().user_joins_voice(user, channel);
    }

    /// `user` leaves voice.
    pub fn voice_leave(&self, user: &str) {
        self.shared.lock().user_leaves_voice(user);
    }

    /// `user` mutes or unmutes themselves.
    pub fn voice_mute(&self, user: &str, muted: bool) {
        self.shared.lock().user_mutes(user, muted);
    }

    /// `user`, in the bot's call, says `frames` (20 ms Opus frames) in real
    /// time, end-to-end and transport encrypted to the bot.
    pub async fn voice_say(&self, user: &str, frames: &[Vec<u8>]) {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for frame in frames {
            tick.tick().await;
            self.shared.lock().say(user, frame);
        }
    }

    /// The Opus frames the bot sent (silence left out), decrypted.
    #[must_use]
    pub fn voice_heard(&self) -> Vec<Vec<u8>> {
        self.voice_heard_frames()
            .into_iter()
            .map(|heard| heard.opus)
            .collect()
    }

    /// [`Self::voice_heard`] with whether each came end-to-end encrypted
    /// and when.
    #[must_use]
    pub fn voice_heard_frames(&self) -> Vec<Heard> {
        self.shared.lock().voice.heard.clone()
    }

    /// Opus silence frames the bot sent.
    #[must_use]
    pub fn voice_silence(&self) -> usize {
        self.shared.lock().voice.silence
    }

    /// The speaking flags the bot sent (op 5), oldest first.
    #[must_use]
    pub fn voice_speaking(&self) -> Vec<u64> {
        self.shared.lock().voice.speaking.clone()
    }

    /// The voice channel the bot is in.
    #[must_use]
    pub fn voice_bot_channel(&self) -> Option<String> {
        self.shared.lock().voice.bot_channel.clone()
    }

    /// Whether the bot holds a live voice connection.
    #[must_use]
    pub fn voice_connected(&self) -> bool {
        self.shared.lock().voice.connected()
    }

    /// Whether DAVE is established: the bot and every fake user in its
    /// call share the group's epoch, no transition in flight.
    #[must_use]
    pub fn voice_dave_ready(&self) -> bool {
        self.shared.lock().voice.dave_ready()
    }

    /// The DAVE protocol version of the bot's call (0: transport only).
    #[must_use]
    pub fn voice_dave_version(&self) -> u16 {
        self.shared.lock().voice.dave_version
    }

    /// The voice privacy codes the fake users see, one per user in the
    /// group: all equal once DAVE is established.
    #[must_use]
    pub fn voice_privacy_codes(&self) -> Vec<String> {
        self.shared.lock().voice.privacy_codes()
    }

    /// Identifies and resumes on the voice gateway.
    #[must_use]
    pub fn voice_handshakes(&self) -> (u32, u32) {
        let state = self.shared.lock();
        (state.voice.identifies, state.voice.resumes)
    }

    /// The status the bot set on `channel` (`PUT .../voice-status`).
    #[must_use]
    pub fn voice_status(&self, channel: &str) -> Option<String> {
        self.shared
            .lock()
            .voice
            .statuses
            .get(channel)
            .cloned()
            .flatten()
    }

    /// The bot's nickname in the guild.
    #[must_use]
    pub fn bot_nick(&self) -> Option<String> {
        self.shared.lock().voice.nick.clone()
    }

    /// The bot's voice side as JSON, for the control endpoint.
    #[must_use]
    pub fn voice_state_json(&self) -> Value {
        let state = self.shared.lock();
        let voice = &state.voice;
        let people: Vec<Value> = voice
            .present
            .iter()
            .map(|(id, p)| json!({"user": id, "name": p.user.name, "channel": p.channel, "muted": p.self_mute}))
            .collect();
        let state_json = json!({
            "bot_channel": voice.bot_channel,
            "connected": voice.connected(),
            "dave_version": voice.dave_version,
            "dave_ready": voice.dave_ready(),
            "speaking": voice.speaking.last(),
            "speaking_flags": voice.speaking,
            "statuses": voice.statuses,
            "nick": voice.nick,
            "heard_frames": voice.heard.len(),
            "identifies": voice.identifies,
            "resumes": voice.resumes,
            "seq_ack": voice.seq_ack(),
            "people": people,
        });
        drop(state);
        state_json
    }

    /// From now on the voice server negotiates no DAVE (version 0), as for
    /// a call with a client that lacks it.
    pub fn voice_without_dave(&self) {
        self.shared.lock().voice.no_dave = true;
    }

    /// Close the bot's voice connection as a network failure would; its
    /// session stays resumable.
    pub fn drop_voice(&self) {
        self.shared
            .lock()
            .voice
            .drop_connection(4015, "Voice server crashed.");
    }

    /// The bot's roles become `roles` (dispatching `GUILD_MEMBER_UPDATE`).
    pub fn set_bot_roles(&self, roles: &[&str]) {
        let mut state = self.shared.lock();
        state.voice.bot_roles = roles.iter().map(|r| (*r).to_owned()).collect();
        let event = json!({
            "guild_id": GUILD, "roles": roles, "nick": state.voice.nick,
            "user": {"id": BOT_USER, "username": "tilo-bridge", "bot": true},
        });
        state.dispatch("GUILD_MEMBER_UPDATE", event);
        drop(state);
    }

    /// `@everyone` loses `perms` in `channel` through an overwrite
    /// (dispatching `CHANNEL_UPDATE`).
    pub fn deny_everyone(&self, channel: &str, perms: u64) {
        let mut state = self.shared.lock();
        let Some(stored) = state.channel_mut(channel) else {
            return;
        };
        let overwrite = json!({"id": GUILD, "type": 0, "allow": "0", "deny": perms.to_string()});
        match stored["permission_overwrites"].as_array_mut() {
            Some(list) => list.push(overwrite),
            None => stored["permission_overwrites"] = json!([overwrite]),
        }
        let event = stored.clone();
        state.dispatch("CHANNEL_UPDATE", event);
        drop(state);
    }

    /// Poll `check` every 10 ms until it answers, for at most `timeout`.
    pub async fn eventually<T>(
        &self,
        timeout: Duration,
        mut check: impl FnMut(&Self) -> Option<T>,
    ) -> Option<T> {
        let deadline = tokio::time::Instant::now().checked_add(timeout)?;
        loop {
            if let Some(found) = check(self) {
                return Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn seed_channels() -> Vec<Value> {
    vec![
        json!({"id": CHANNEL_GOAL, "type": 0, "name": "goal", "parent_id": null, "position": 0, "topic": "What we are building", "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CATEGORY_CHAT, "type": 4, "name": "chat", "parent_id": null, "position": 1, "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CHANNEL_GENERAL, "type": 0, "name": "general", "parent_id": CATEGORY_CHAT, "position": 2, "topic": null, "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CHANNEL_VOICE, "type": 2, "name": "voice", "parent_id": CATEGORY_CHAT, "position": 3, "permission_overwrites": [], "guild_id": GUILD}),
    ]
}

/// The width and height a PNG's header gives, when `bytes` is one.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) {
        return None;
    }
    let width = bytes.get(16..20)?.try_into().ok()?;
    let height = bytes.get(20..24)?.try_into().ok()?;
    Some((u32::from_be_bytes(width), u32::from_be_bytes(height)))
}

#[cfg(test)]
mod png_tests {
    #[test]
    fn a_png_header_gives_its_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&3_u32.to_be_bytes());
        png.extend_from_slice(&2_u32.to_be_bytes());
        assert_eq!(super::png_size(&png), Some((3, 2)));
        assert_eq!(super::png_size(b"plain text"), None);
        assert_eq!(super::png_size(&png[..18]), None);
    }
}
