//! Which voice channel the bot is in, who is in the guild's voice
//! channels, and the line the bot shows there about the Tilo side.
//!
//! The gateway task owns this and feeds it the voice events of the bridged
//! guild and the host's `Call`s; what it wants sent on the main gateway
//! (op 4, Voice State Update) it hands back. A bot can be in one voice
//! channel per guild, so the newest `Call` wins. Joining asks Discord with
//! op 4, collects the bot's own `VOICE_STATE_UPDATE` (its voice session)
//! and `VOICE_SERVER_UPDATE` (token and endpoint), and then opens the voice
//! connection. Whatever keeps the bot out is said as `Refused`, with the
//! reason, and on the connector's status: never a silent failure.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_connector::protocol::{CallLink, FromWorker, OutsideCall, OutsidePerson};
use tilo_net::Method;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use super::connection::{self, Join};
use super::permissions::{self, CHANGE_NICKNAME, SET_VOICE_CHANNEL_STATUS};
use crate::Ctx;
use crate::events::{Guild, display_name};
use crate::rest::{Auth, Body, segment};
use discord_wire::op::ChannelKind;

/// How long Discord gets to let the bot in before the join is given up:
/// it ignores a join it refuses (a full channel) rather than saying so.
const JOIN_TIMEOUT: Duration = Duration::from_secs(20);
/// Discord's longest nickname.
const NICK_MAX_CHARS: usize = 32;
/// Discord's longest voice channel status.
const STATUS_MAX_CHARS: usize = 500;

/// What a voice connection (or a join's timer) tells the call.
#[derive(Debug)]
pub enum LinkEvent {
    /// Connection `attempt` carries audio both ways.
    Joined { attempt: u64 },
    /// Connection `attempt` ended for good, for `reason`.
    Refused { attempt: u64, reason: String },
    /// Join `attempt` has waited [`JOIN_TIMEOUT`].
    Timeout { attempt: u64 },
}

struct Want {
    channel: String,
    status: Option<String>,
}

struct Present {
    channel: String,
    muted: bool,
}

#[derive(Clone)]
struct Named {
    name: String,
    avatar_url: Option<String>,
}

/// Where the status line went, so leaving clears it there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Shown {
    #[default]
    Nowhere,
    VoiceStatus(String),
    Nickname,
}

pub struct Calls {
    ctx: Arc<Ctx>,
    links: mpsc::UnboundedSender<LinkEvent>,
    people: BTreeMap<String, Present>,
    names: HashMap<String, Named>,
    want: Option<Want>,
    /// A `Call` that came before the guild was known.
    early: Option<(Option<String>, Option<String>)>,
    link: CallLink,
    attempt: u64,
    voice_session: Option<String>,
    server: Option<(Zeroizing<String>, String)>,
    connection: Option<tokio::task::JoinHandle<()>>,
    reported: Option<(Vec<OutsideCall>, CallLink)>,
    shown: Arc<tokio::sync::Mutex<Shown>>,
}

impl Drop for Calls {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.abort();
        }
    }
}

impl Calls {
    #[must_use]
    pub fn new(ctx: Arc<Ctx>, links: mpsc::UnboundedSender<LinkEvent>) -> Self {
        Self {
            ctx,
            links,
            people: BTreeMap::new(),
            names: HashMap::new(),
            want: None,
            early: None,
            link: CallLink::Idle,
            attempt: 0,
            voice_session: None,
            server: None,
            connection: None,
            reported: None,
            shown: Arc::new(tokio::sync::Mutex::new(Shown::Nowhere)),
        }
    }

    /// `GUILD_CREATE`: who is in voice now, and what they are called. A
    /// `Call` the host sent earlier is acted on now.
    pub fn guild_created(&mut self, data: &Value, guild: &Guild) -> Vec<Value> {
        for member in data["members"].as_array().into_iter().flatten() {
            self.name(guild, &member["user"], member);
        }
        self.people.clear();
        for state in data["voice_states"].as_array().into_iter().flatten() {
            self.take_state(guild, state);
        }
        let mut send = Vec::new();
        if let Some((call, status)) = self.early.take() {
            send = self.command(call, status, Some(guild));
        } else if let Some(want) = self.want.take() {
            // A new session: Discord may have dropped the bot from voice.
            send = self.join(guild, want);
        }
        self.report(false);
        send
    }

    /// A gateway dispatch about the bridged guild.
    pub fn dispatch(&mut self, kind: &str, data: &Value, guild: &Guild) -> Vec<Value> {
        match kind {
            "VOICE_STATE_UPDATE" => {
                let user = data["user_id"].as_str().unwrap_or_default();
                if user == guild.bot_user {
                    return self.own_state(data, guild);
                }
                self.take_state(guild, data);
                self.report(false);
                Vec::new()
            }
            "VOICE_SERVER_UPDATE" => {
                let (Some(token), Some(endpoint)) =
                    (data["token"].as_str(), data["endpoint"].as_str())
                else {
                    // No endpoint: Discord is moving the call to another
                    // server and sends another update when it has one.
                    return Vec::new();
                };
                self.server = Some((Zeroizing::new(token.to_owned()), endpoint.to_owned()));
                if let Some(old) = self.connection.take() {
                    old.abort();
                }
                self.connect(guild);
                Vec::new()
            }
            "GUILD_MEMBER_UPDATE" => {
                if self
                    .people
                    .contains_key(data["user"]["id"].as_str().unwrap_or_default())
                {
                    self.name(guild, &data["user"], data);
                    self.report(false);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The host's `Call`: be in `call` (a voice channel id) showing
    /// `status`, or in none.
    pub fn command(
        &mut self,
        call: Option<String>,
        status: Option<String>,
        guild: Option<&Guild>,
    ) -> Vec<Value> {
        let Some(guild) = guild else {
            self.early = Some((call.clone(), status));
            self.link = call.map_or(CallLink::Idle, |call| CallLink::Joining { call });
            self.report(true);
            return Vec::new();
        };
        let Some(channel) = call else {
            let leave = self.leave(guild);
            self.link = CallLink::Idle;
            self.report(true);
            return leave;
        };
        if let Some(reason) = self.refusal(guild, &channel) {
            let mut send = self.leave(guild);
            send.extend(self.refuse(&channel, reason));
            return send;
        }
        let same = self.want.as_ref().is_some_and(|w| w.channel == channel)
            && matches!(
                &self.link,
                CallLink::Joining { .. } | CallLink::Joined { .. }
            );
        if same {
            if let Some(want) = &mut self.want {
                want.status.clone_from(&status);
            }
            if matches!(self.link, CallLink::Joined { .. }) {
                self.show(guild, &channel, status);
            }
            self.report(true);
            return Vec::new();
        }
        let send = self.join(guild, Want { channel, status });
        self.report(true);
        send
    }

    /// What a voice connection or a join's timer said.
    pub fn link_event(&mut self, event: LinkEvent, guild: Option<&Guild>) -> Vec<Value> {
        let now = self.attempt;
        let current = |attempt: u64| attempt == now;
        match event {
            LinkEvent::Joined { attempt } if current(attempt) => {
                let Some(want) = &self.want else {
                    return Vec::new();
                };
                let (channel, status) = (want.channel.clone(), want.status.clone());
                self.link = CallLink::Joined {
                    call: channel.clone(),
                };
                if let Some(guild) = guild {
                    self.show(guild, &channel, status);
                }
                self.report(true);
                Vec::new()
            }
            LinkEvent::Refused { attempt, reason } if current(attempt) => {
                let Some(guild) = guild else {
                    return Vec::new();
                };
                let channel = self.want.as_ref().map(|w| w.channel.clone());
                let mut send = self.leave(guild);
                if let Some(channel) = channel {
                    send.extend(self.refuse(&channel, reason));
                }
                send
            }
            LinkEvent::Timeout { attempt }
                if current(attempt) && matches!(self.link, CallLink::Joining { .. }) =>
            {
                let (Some(guild), Some(want)) = (guild, &self.want) else {
                    return Vec::new();
                };
                let channel = want.channel.clone();
                let reason = format!(
                    "Discord did not let the bot into {} (is the channel full?)",
                    channel_name(guild, &channel)
                );
                let mut send = self.leave(guild);
                send.extend(self.refuse(&channel, reason));
                send
            }
            _ => Vec::new(),
        }
    }

    /// Why the bot may not join `channel`, if it may not.
    fn refusal(&self, guild: &Guild, channel: &str) -> Option<String> {
        let name = channel_name(guild, channel);
        if self.ctx.config.read_only {
            return Some(format!(
                "the connector is read-only, so the bot stays out of {name}"
            ));
        }
        if !self.ctx.in_scope(channel) {
            return Some(format!("{name} is not bridged"));
        }
        if guild
            .channels
            .get(channel)
            .is_none_or(|c| c.kind != ChannelKind::Voice)
        {
            return Some(format!("{name} is not a voice channel of the server"));
        }
        let missing = permissions::missing_to_speak(guild, channel);
        (!missing.is_empty()).then(|| {
            format!(
                "the bot lacks {} in {name}",
                permissions::and_list(&missing)
            )
        })
    }

    fn refuse(&mut self, channel: &str, reason: String) -> Vec<Value> {
        self.ctx.status(false, &reason);
        self.link = CallLink::Refused {
            call: channel.to_owned(),
            reason,
        };
        self.report(true);
        Vec::new()
    }

    /// Ask Discord to put the bot in `want`'s channel.
    fn join(&mut self, guild: &Guild, want: Want) -> Vec<Value> {
        if let Some(old) = self.connection.take() {
            old.abort();
        }
        self.attempt = self.attempt.wrapping_add(1);
        self.voice_session = None;
        self.server = None;
        self.link = CallLink::Joining {
            call: want.channel.clone(),
        };
        let channel = want.channel.clone();
        self.want = Some(want);
        let (links, attempt) = (self.links.clone(), self.attempt);
        tokio::spawn(async move {
            tokio::time::sleep(JOIN_TIMEOUT).await;
            let _ = links.send(LinkEvent::Timeout { attempt });
        });
        vec![voice_state(&guild.id, Some(&channel))]
    }

    /// Leave the voice channel, clearing the status line first.
    fn leave(&mut self, guild: &Guild) -> Vec<Value> {
        if let Some(old) = self.connection.take() {
            old.abort();
        }
        self.attempt = self.attempt.wrapping_add(1);
        self.want = None;
        self.voice_session = None;
        self.server = None;
        self.clear_shown(guild);
        vec![voice_state(&guild.id, None)]
    }

    fn own_state(&mut self, data: &Value, guild: &Guild) -> Vec<Value> {
        let channel = data["channel_id"].as_str();
        let Some(want) = &self.want else {
            return Vec::new();
        };
        if channel == Some(want.channel.as_str()) {
            self.voice_session = data["session_id"].as_str().map(str::to_owned);
            self.connect(guild);
            return Vec::new();
        }
        if !matches!(self.link, CallLink::Joined { .. }) {
            // Leaving the last channel on the way to this one.
            return Vec::new();
        }
        let wanted = want.channel.clone();
        let name = channel_name(guild, &wanted);
        let reason = match channel {
            Some(_) => format!("someone moved the bot out of {name}"),
            None => format!("someone disconnected the bot from {name}"),
        };
        if let Some(old) = self.connection.take() {
            old.abort();
        }
        self.want = None;
        self.attempt = self.attempt.wrapping_add(1);
        self.clear_shown(guild);
        let mut send = Vec::new();
        if channel.is_some() {
            send.push(voice_state(&guild.id, None));
        }
        send.extend(self.refuse(&wanted, reason));
        send
    }

    /// Open the voice connection once the session and the server are known.
    fn connect(&mut self, guild: &Guild) {
        if self.connection.is_some() {
            return;
        }
        let (Some(want), Some(session), Some((token, endpoint))) =
            (&self.want, &self.voice_session, &self.server)
        else {
            return;
        };
        let join = Join {
            attempt: self.attempt,
            guild: guild.id.clone(),
            channel: want.channel.clone(),
            user: guild.bot_user.clone(),
            session: session.clone(),
            token: token.clone(),
            endpoint: endpoint.clone(),
        };
        self.connection = Some(tokio::spawn(connection::run(
            Arc::clone(&self.ctx),
            join,
            self.links.clone(),
        )));
    }

    fn name(&mut self, guild: &Guild, user: &Value, member: &Value) {
        let Some(id) = user["id"].as_str() else {
            return;
        };
        self.names.insert(
            id.to_owned(),
            Named {
                name: display_name(user, member),
                avatar_url: guild.avatar_url(user),
            },
        );
    }

    fn take_state(&mut self, guild: &Guild, state: &Value) {
        let Some(user) = state["user_id"].as_str() else {
            return;
        };
        if user == guild.bot_user {
            return;
        }
        if state["member"]["user"].is_object() {
            self.name(guild, &state["member"]["user"], &state["member"]);
        }
        match state["channel_id"].as_str() {
            Some(channel) => {
                let flag = |key: &str| state[key].as_bool().unwrap_or(false);
                self.people.insert(
                    user.to_owned(),
                    Present {
                        channel: channel.to_owned(),
                        muted: flag("self_mute") || flag("mute") || flag("suppress"),
                    },
                );
            }
            None => {
                self.people.remove(user);
            }
        }
    }

    fn calls(&self) -> Vec<OutsideCall> {
        let mut calls: BTreeMap<&str, Vec<OutsidePerson>> = BTreeMap::new();
        for (user, present) in &self.people {
            if !self.ctx.in_scope(&present.channel) {
                continue;
            }
            let named = self.names.get(user);
            calls
                .entry(present.channel.as_str())
                .or_default()
                .push(OutsidePerson {
                    account: user.clone(),
                    name: named.map_or_else(|| user.clone(), |n| n.name.clone()),
                    avatar_url: named.and_then(|n| n.avatar_url.clone()),
                    muted: present.muted,
                });
        }
        calls
            .into_iter()
            .map(|(call, people)| OutsideCall {
                call: call.to_owned(),
                people,
            })
            .collect()
    }

    /// Tell the host who is in voice and where the bot is, when that
    /// changed or `always`.
    fn report(&mut self, always: bool) {
        let now = (self.calls(), self.link.clone());
        if !always && self.reported.as_ref() == Some(&now) {
            return;
        }
        let _ = self.ctx.out.send(FromWorker::Calls {
            calls: now.0.clone(),
            link: now.1.clone(),
        });
        self.reported = Some(now);
    }

    /// Show `status` about the Tilo side: as the voice channel's status
    /// when the bot may set it, else as the bot's nickname in the server.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "held throughout so a clear queued behind this waits for it"
    )]
    fn show(&self, guild: &Guild, channel: &str, status: Option<String>) {
        let have = permissions::in_channel(guild, channel);
        let (guild_id, channel) = (guild.id.clone(), channel.to_owned());
        let (ctx, shown) = (Arc::clone(&self.ctx), Arc::clone(&self.shown));
        tokio::spawn(async move {
            let mut shown = shown.lock().await;
            let text = status.unwrap_or_default();
            if have & SET_VOICE_CHANNEL_STATUS != 0 {
                let set = set_voice_status(&ctx, &channel, &text).await;
                if set {
                    if *shown == Shown::Nickname {
                        set_nickname(&ctx, &guild_id, None).await;
                    }
                    *shown = Shown::VoiceStatus(channel);
                    return;
                }
            }
            if have & CHANGE_NICKNAME != 0 {
                let nick = (!text.is_empty()).then(|| truncate(&text, NICK_MAX_CHARS));
                if set_nickname(&ctx, &guild_id, nick).await {
                    *shown = Shown::Nickname;
                    return;
                }
            }
            if !text.is_empty() {
                ctx.status(
                    true,
                    "the bot may neither set the voice channel's status nor its nickname, so Discord does not see who is on the Tilo side",
                );
            }
        });
    }

    fn clear_shown(&self, guild: &Guild) {
        let guild_id = guild.id.clone();
        let (ctx, shown) = (Arc::clone(&self.ctx), Arc::clone(&self.shown));
        tokio::spawn(async move {
            let mut shown = shown.lock().await;
            match std::mem::take(&mut *shown) {
                Shown::VoiceStatus(channel) => {
                    set_voice_status(&ctx, &channel, "").await;
                }
                Shown::Nickname => {
                    set_nickname(&ctx, &guild_id, None).await;
                }
                Shown::Nowhere => {}
            }
        });
    }
}

/// Op 4: be in `channel`, or in no voice channel of `guild`.
fn voice_state(guild: &str, channel: Option<&str>) -> Value {
    json!({"op": 4, "d": {
        "guild_id": guild,
        "channel_id": channel,
        "self_mute": false,
        "self_deaf": false,
    }})
}

fn channel_name(guild: &Guild, channel: &str) -> String {
    guild
        .channels
        .get(channel)
        .map_or_else(|| format!("channel {channel}"), |c| format!("#{}", c.name))
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// `PUT /channels/{id}/voice-status`; an empty text clears it.
async fn set_voice_status(ctx: &Ctx, channel: &str, text: &str) -> bool {
    let status = if text.is_empty() {
        Value::Null
    } else {
        json!(truncate(text, STATUS_MAX_CHARS))
    };
    ctx.rest
        .call(
            Method::PUT,
            &format!("/channels/{}/voice-status", segment(channel)),
            &[],
            Body::Json(json!({ "status": status })),
            Auth::Bot,
            None,
        )
        .await
        .is_ok()
}

/// The bot's nickname in `guild`; `None` restores its username.
async fn set_nickname(ctx: &Ctx, guild: &str, nick: Option<String>) -> bool {
    ctx.rest
        .call(
            Method::PATCH,
            &format!("/guilds/{}/members/@me", segment(guild)),
            &[],
            Body::Json(json!({ "nick": nick })),
            Auth::Bot,
            None,
        )
        .await
        .is_ok()
}
