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

#![allow(
    clippy::indexing_slicing,
    reason = "the fake indexes and assigns into JSON objects it built itself"
)]

mod gateway;
mod rest;
mod time;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

pub use rest::Recorded;

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

#[derive(Debug, Default)]
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
    bot_reactions: Vec<(String, String)>,
    audit: Vec<Value>,
    cdn: HashMap<String, (String, Vec<u8>)>,
    requests: Vec<Recorded>,
    scripts: Vec<Script>,
    windows: Vec<Window>,
    commands: Value,
    interaction_replies: Vec<Value>,
    last_id: u64,
    gateway: Gateway,
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
        json!({
            "id": GUILD,
            "name": "Fake Guild",
            "owner_id": "3000",
            "channels": channels,
            "threads": threads,
            "roles": self.roles,
            "members": [],
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
            ..State::default()
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
        });
        let tasks = vec![
            tokio::spawn(rest::serve(Arc::clone(&shared), rest_listener)),
            tokio::spawn(gateway::serve(Arc::clone(&shared), gateway_listener)),
        ];
        Ok(Self {
            shared,
            rest,
            gateway,
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

    // ----- acting as Discord users -------------------------------------

    /// `author` posts `content` in `channel`; returns the message id.
    pub fn post(&self, channel: &str, author: &User, content: &str) -> String {
        self.post_message(channel, author, content, None, true)
    }

    /// `author` answers `reply_to` in `channel`.
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
        message["attachments"] = json!([{
            "id": id, "filename": name, "size": bytes.len(), "content_type": content_type,
            "url": url, "proxy_url": url,
        }]);
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

    /// The message's author edits it.
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

    /// `user` adds `emoji` (unicode, or a custom emoji's name) to `message`.
    pub fn react(&self, channel: &str, message: &str, user: &User, emoji: &str) {
        let event = json!({
            "user_id": user.id, "channel_id": channel, "message_id": message, "guild_id": GUILD,
            "member": {"nick": user.name, "roles": [], "user": user.json()},
            "emoji": emoji_json(emoji),
        });
        self.shared.lock().dispatch("MESSAGE_REACTION_ADD", event);
    }

    /// `user` takes `emoji` off `message`.
    pub fn unreact(&self, channel: &str, message: &str, user: &User, emoji: &str) {
        let event = json!({
            "user_id": user.id, "channel_id": channel, "message_id": message, "guild_id": GUILD,
            "emoji": emoji_json(emoji),
        });
        self.shared
            .lock()
            .dispatch("MESSAGE_REACTION_REMOVE", event);
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

    /// `user` runs `/name` (with `code` for `/link`); returns the
    /// interaction id.
    pub fn slash(&self, user: &User, name: &str, code: Option<&str>) -> String {
        let mut state = self.shared.lock();
        let id = state.snowflake();
        let options: Vec<Value> = code
            .map(|code| json!({"type": 3, "name": "code", "value": code}))
            .into_iter()
            .collect();
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

    /// A Discord moderator bans `user`.
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

    // ----- scripting failures ------------------------------------------

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

    // ----- reading what happened ----------------------------------------

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

    /// `(message, emoji)` the bot user has reacted with.
    #[must_use]
    pub fn bot_reactions(&self) -> Vec<(String, String)> {
        self.shared.lock().bot_reactions.clone()
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

fn emoji_json(emoji: &str) -> Value {
    emoji
        .strip_prefix(':')
        .and_then(|e| e.strip_suffix(':'))
        .map_or_else(
            || json!({"id": null, "name": emoji}),
            |name| json!({"id": "5000", "name": name}),
        )
}

fn seed_channels() -> Vec<Value> {
    vec![
        json!({"id": CHANNEL_GOAL, "type": 0, "name": "goal", "parent_id": null, "position": 0, "topic": "What we are building", "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CATEGORY_CHAT, "type": 4, "name": "chat", "parent_id": null, "position": 1, "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CHANNEL_GENERAL, "type": 0, "name": "general", "parent_id": CATEGORY_CHAT, "position": 2, "topic": null, "permission_overwrites": [], "guild_id": GUILD}),
        json!({"id": CHANNEL_VOICE, "type": 2, "name": "voice", "parent_id": CATEGORY_CHAT, "position": 3, "permission_overwrites": [], "guild_id": GUILD}),
    ]
}
