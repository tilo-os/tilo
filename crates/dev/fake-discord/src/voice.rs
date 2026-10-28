//! The fake voice server: Discord's voice gateway (v8) and its UDP media
//! path on loopback, with DAVE end-to-end encryption for real.
//!
//! The fake is the MLS group's external sender, as Discord's voice server
//! is: it proposes adding each member's key package, takes one commit per
//! epoch, announces it as a transition and executes it once the bot is
//! ready. Each fake voice user is a `davey` session living in the fake, so
//! the bot's handshake, sending and receiving all run on real DAVE frames.
//! A member leaving starts a new group (`prepare_epoch` 1), which the
//! protocol allows and which exercises the bot's re-initialisation.

#![allow(
    clippy::indexing_slicing,
    reason = "the fake indexes JSON it parses and buffers it sized itself"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::sync::Arc;

use davey::{DaveSession, MediaType, ProposalsOperationType};
use futures_util::{SinkExt as _, StreamExt as _};
use openmls::prelude::tls_codec::{DeserializeBytes as _, Serialize as _, VLBytes};
use openmls::prelude::{
    BasicCredential, ExternalProposal, ExternalSender, GroupEpoch, GroupId, KeyPackageIn,
    MlsMessageIn, OpenMlsProvider as _, ProtocolVersion, SenderExtensionIndex, SignatureScheme,
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_websockets::{CloseCode, Message};

use crate::rtp::{self, Cipher, Stream};
use crate::{BOT_USER, GUILD, Shared, State, User};

/// Short, so a test sees heartbeats without waiting long.
const HEARTBEAT_INTERVAL_MS: u64 = 2_000;
/// What the fake offers in Ready, the deprecated mode first so a client
/// that just takes the first is caught.
const MODES: [&str; 3] = ["xsalsa20_poly1305", rtp::XCHACHA, rtp::AES_GCM];
/// Opus silence: the SFU's and every client's pause marker.
pub const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];

/// A frame for the bot's voice gateway connection.
#[derive(Debug)]
pub(crate) enum Out {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

/// One frame the bot sent on UDP, after transport (and DAVE) decryption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    pub opus: Vec<u8>,
    /// It arrived as a DAVE frame and decrypted with the group's keys.
    pub e2ee: bool,
    /// When it arrived, in Unix ms.
    pub at_ms: u64,
}

/// Someone in a voice channel of the guild.
#[derive(Clone, Debug)]
pub struct Present {
    pub channel: String,
    pub user: User,
    pub self_mute: bool,
}

/// A fake voice user in the bot's call, as a DAVE client.
struct Local {
    session: Option<DaveSession>,
    stream: Stream,
    announced: bool,
}

/// The bot's live voice gateway connection.
struct Bot {
    tx: mpsc::UnboundedSender<Out>,
    session_id: String,
    ssrc: u32,
    cipher: Option<Cipher>,
    address: Option<SocketAddr>,
    seq: u16,
    seq_ack: i64,
}

impl Bot {
    fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn json(&mut self, op: u64, data: Value) {
        let seq = self.next_seq();
        let mut frame = json!({"op": op, "seq": seq});
        frame["d"] = data;
        let _ = self.tx.send(Out::Text(frame.to_string()));
    }

    fn binary(&mut self, op: u8, payload: &[u8]) {
        let mut frame = self.next_seq().to_be_bytes().to_vec();
        frame.push(op);
        frame.extend_from_slice(payload);
        let _ = self.tx.send(Out::Binary(frame));
    }
}

/// The MLS group of the bot's call, from the external sender's seat.
struct Mls {
    signer: SignatureKeyPair,
    external: Vec<u8>,
    provider: OpenMlsRustCrypto,
    group: GroupId,
    epoch: u64,
    established: bool,
    members: BTreeSet<u64>,
    pending: BTreeMap<u64, Vec<u8>>,
    /// The commit being waited for (from the bot), and whom it adds.
    awaiting: Option<(u64, Vec<u64>)>,
    /// The transition announced to the bot, until it is ready.
    transition: Option<u16>,
    next_transition: u16,
}

/// Everything voice in the fake.
#[derive(Default)]
pub struct Voice {
    pub present: BTreeMap<String, Present>,
    pub endpoint: String,
    pub udp_port: u16,
    pub udp: Option<Arc<std::net::UdpSocket>>,
    pub bot_channel: Option<String>,
    bot_session: Option<String>,
    sessions: u32,
    pub token: String,
    bot: Option<Bot>,
    mls: Option<Mls>,
    locals: BTreeMap<u64, Local>,
    next_ssrc: u32,
    pub no_dave: bool,
    pub heard: Vec<Heard>,
    pub silence: usize,
    pub speaking: Vec<u64>,
    pub identifies: u32,
    pub resumes: u32,
    pub dave_version: u16,
    pub statuses: BTreeMap<String, Option<String>>,
    pub nick: Option<String>,
    pub bot_roles: Vec<String>,
}

impl Voice {
    pub fn new(
        endpoint: String,
        udp_port: u16,
        udp: Arc<std::net::UdpSocket>,
        bot_roles: Vec<String>,
    ) -> Self {
        Self {
            endpoint,
            udp_port,
            udp: Some(udp),
            bot_roles,
            ..Self::default()
        }
    }

    fn ssrc(&mut self) -> u32 {
        self.next_ssrc = self.next_ssrc.wrapping_add(1).max(100);
        self.next_ssrc
    }

    /// Whether the bot and every fake user in its channel share the
    /// group's current epoch with nothing left in flight.
    pub fn dave_ready(&self) -> bool {
        let Some(mls) = &self.mls else {
            return false;
        };
        let bot: u64 = BOT_USER.parse().unwrap_or(0);
        mls.established
            && mls.members.contains(&bot)
            && mls.transition.is_none()
            && mls.awaiting.is_none()
            && self.locals.keys().all(|id| mls.members.contains(id))
    }

    fn discovered(&mut self, from: SocketAddr) {
        if let Some(bot) = &mut self.bot {
            bot.address = Some(from);
        }
    }

    /// The last sequence the bot acknowledged on the voice gateway.
    pub fn seq_ack(&self) -> Option<i64> {
        self.bot.as_ref().map(|bot| bot.seq_ack)
    }

    pub fn connected(&self) -> bool {
        self.bot.as_ref().is_some_and(|bot| bot.cipher.is_some())
    }

    /// Close the bot's voice socket with `code`, keeping its session.
    pub fn drop_connection(&self, code: u16, reason: &str) {
        if let Some(bot) = &self.bot {
            let _ = bot.tx.send(Out::Close(code, reason.to_owned()));
        }
    }

    pub fn privacy_codes(&self) -> Vec<String> {
        self.locals
            .values()
            .filter_map(|l| l.session.as_ref()?.voice_privacy_code().map(str::to_owned))
            .collect()
    }
}

impl State {
    fn voice_member(user: &User) -> Value {
        json!({"user": user.json(), "nick": user.name, "roles": []})
    }

    pub(crate) fn voice_state_json(&self, user: &str) -> Value {
        match self.voice.present.get(user) {
            Some(present) => json!({
                "guild_id": GUILD, "channel_id": present.channel, "user_id": user,
                "session_id": format!("vs-{user}"), "deaf": false, "mute": false,
                "self_deaf": false, "self_mute": present.self_mute, "suppress": false,
                "member": Self::voice_member(&present.user),
            }),
            None => json!({
                "guild_id": GUILD, "channel_id": null, "user_id": user,
                "session_id": format!("vs-{user}"), "deaf": false, "mute": false,
                "self_deaf": false, "self_mute": false, "suppress": false,
            }),
        }
    }

    /// `GUILD_CREATE`'s voice states and the members they name, the bot's
    /// own member first.
    pub(crate) fn voice_guild_parts(&self) -> (Vec<Value>, Vec<Value>) {
        let mut states: Vec<Value> = self
            .voice
            .present
            .keys()
            .map(|user| {
                let mut state = self.voice_state_json(user);
                if let Some(object) = state.as_object_mut() {
                    object.remove("member");
                }
                state
            })
            .collect();
        if let Some(channel) = &self.voice.bot_channel {
            states.push(json!({
                "guild_id": GUILD, "channel_id": channel, "user_id": BOT_USER,
                "session_id": self.voice.bot_session, "self_mute": false, "mute": false,
                "self_deaf": false, "deaf": false, "suppress": false,
            }));
        }
        let mut members = vec![json!({
            "user": {"id": BOT_USER, "username": "tilo-bridge", "bot": true},
            "roles": self.voice.bot_roles,
            "nick": self.voice.nick,
        })];
        members.extend(
            self.voice
                .present
                .values()
                .map(|present| Self::voice_member(&present.user)),
        );
        (states, members)
    }

    /// Main gateway op 4 from the bot: join `channel`, or leave.
    pub(crate) fn bot_voice_state(&mut self, channel: Option<&str>) {
        let Some(channel) = channel else {
            self.end_bot_voice(true);
            return;
        };
        if self.voice.bot_channel.as_deref() != Some(channel) {
            self.end_bot_voice(false);
        }
        self.voice.sessions = self.voice.sessions.saturating_add(1);
        let session = format!("voice-session-{}", self.voice.sessions);
        self.voice.bot_channel = Some(channel.to_owned());
        self.voice.bot_session = Some(session.clone());
        self.voice.token = format!("voice-token-{}", self.voice.sessions);
        self.dispatch(
            "VOICE_STATE_UPDATE",
            json!({
                "guild_id": GUILD, "channel_id": channel, "user_id": BOT_USER,
                "session_id": session, "self_mute": false, "self_deaf": false,
                "mute": false, "deaf": false, "suppress": false,
                "member": {"user": {"id": BOT_USER, "username": "tilo-bridge", "bot": true}, "roles": self.voice.bot_roles},
            }),
        );
        let (token, endpoint) = (self.voice.token.clone(), self.voice.endpoint.clone());
        self.dispatch(
            "VOICE_SERVER_UPDATE",
            json!({"token": token, "guild_id": GUILD, "endpoint": endpoint}),
        );
    }

    /// The bot leaves its voice channel (`announce`: and says so on the
    /// gateway): its voice connection is closed as Discord closes it.
    pub(crate) fn end_bot_voice(&mut self, announce: bool) {
        if let Some(bot) = self.voice.bot.take() {
            let _ = bot.tx.send(Out::Close(4014, "Disconnected.".into()));
        }
        self.voice.mls = None;
        self.voice.locals.clear();
        let was = self.voice.bot_channel.take();
        self.voice.bot_session = None;
        for status in self.voice.statuses.values_mut() {
            *status = None;
        }
        if announce && was.is_some() {
            self.dispatch(
                "VOICE_STATE_UPDATE",
                json!({
                    "guild_id": GUILD, "channel_id": null, "user_id": BOT_USER,
                    "session_id": "", "self_mute": false, "self_deaf": false,
                    "mute": false, "deaf": false, "suppress": false,
                }),
            );
        }
    }

    pub(crate) fn user_joins_voice(&mut self, user: &User, channel: &str) {
        let left_call = self.leave_call(&user.id);
        self.voice.present.insert(
            user.id.clone(),
            Present {
                channel: channel.to_owned(),
                user: user.clone(),
                self_mute: false,
            },
        );
        let state = self.voice_state_json(&user.id);
        self.dispatch("VOICE_STATE_UPDATE", state);
        if left_call {
            self.restart_group();
        }
        if self.voice.bot_channel.as_deref() == Some(channel) {
            self.join_call(&user.id);
        }
    }

    pub(crate) fn user_leaves_voice(&mut self, user: &str) {
        let left_call = self.leave_call(user);
        if self.voice.present.remove(user).is_some() {
            let state = self.voice_state_json(user);
            self.dispatch("VOICE_STATE_UPDATE", state);
        }
        if left_call {
            self.restart_group();
        }
    }

    pub(crate) fn user_mutes(&mut self, user: &str, muted: bool) {
        if let Some(present) = self.voice.present.get_mut(user) {
            present.self_mute = muted;
            let state = self.voice_state_json(user);
            self.dispatch("VOICE_STATE_UPDATE", state);
        }
    }

    /// A fake user enters the bot's call: the bot hears who they are, and
    /// they become a DAVE client offering a key package.
    fn join_call(&mut self, user: &str) {
        let Ok(id) = user.parse::<u64>() else { return };
        let ssrc = self.voice.ssrc();
        let start = random::<10>();
        self.voice.locals.insert(
            id,
            Local {
                session: None,
                stream: Stream::new(ssrc, start),
                announced: false,
            },
        );
        if let Some(bot) = &mut self.voice.bot {
            bot.json(11, json!({"user_ids": [user]}));
        }
        self.local_key_package(id);
        self.advance();
    }

    /// Whether `user` was in the bot's call, now left.
    fn leave_call(&mut self, user: &str) -> bool {
        let Ok(id) = user.parse::<u64>() else {
            return false;
        };
        if self.voice.locals.remove(&id).is_none() {
            return false;
        }
        if let Some(bot) = &mut self.voice.bot {
            bot.json(13, json!({"user_id": user}));
        }
        true
    }

    fn local_key_package(&mut self, id: u64) {
        let (Some(mls), Some(channel)) = (&mut self.voice.mls, &self.voice.bot_channel) else {
            return;
        };
        let (Some(version), Ok(channel)) = (NonZeroU16::new(1), channel.parse::<u64>()) else {
            return;
        };
        let Some(local) = self.voice.locals.get_mut(&id) else {
            return;
        };
        let Ok(mut session) = DaveSession::new(version, id, channel, None) else {
            return;
        };
        if session.set_external_sender(&mls.external).is_err() {
            return;
        }
        if let Ok(package) = session.create_key_package() {
            mls.pending.insert(id, package);
        }
        local.session = Some(session);
    }

    /// A member left: everyone starts a new group (epoch 1), the bot by
    /// `prepare_epoch`, the fake's users at once.
    fn restart_group(&mut self) {
        let Some(mls) = &mut self.voice.mls else {
            return;
        };
        mls.established = false;
        mls.epoch = 0;
        mls.members.clear();
        mls.pending.clear();
        mls.awaiting = None;
        mls.transition = None;
        if let Some(bot) = &mut self.voice.bot {
            bot.json(24, json!({"protocol_version": 1, "epoch": 1}));
        }
        let ids: Vec<u64> = self.voice.locals.keys().copied().collect();
        for id in ids {
            self.local_key_package(id);
        }
        self.advance();
    }

    /// Propose adding whoever offered a key package, when nothing else is
    /// in flight. A new group is founded by a fake user when there is one,
    /// so the bot joins by welcome; once the bot is a member it is the one
    /// that commits, so its commits are exercised too.
    fn advance(&mut self) {
        let bot_id: u64 = BOT_USER.parse().unwrap_or(0);
        let Some(mls) = &mut self.voice.mls else {
            return;
        };
        if mls.awaiting.is_some() || mls.transition.is_some() || mls.pending.is_empty() {
            return;
        }
        let (committer, epoch, receivers): (u64, u64, Vec<u64>) = if mls.established {
            let committer = if mls.members.contains(&bot_id) {
                bot_id
            } else {
                let Some(first) = mls.members.iter().next() else {
                    return;
                };
                *first
            };
            (committer, mls.epoch, mls.members.iter().copied().collect())
        } else {
            if mls.pending.len() < 2 {
                return;
            }
            let founder = mls
                .pending
                .keys()
                .find(|id| **id != bot_id)
                .copied()
                .unwrap_or(bot_id);
            (founder, 0, vec![founder])
        };
        let adds: Vec<u64> = mls
            .pending
            .keys()
            .filter(|id| **id != committer)
            .copied()
            .collect();
        if adds.is_empty() {
            return;
        }
        let mut proposals = Vec::new();
        for id in &adds {
            let Some(package) = mls.pending.get(id) else {
                continue;
            };
            let Ok(package) = KeyPackageIn::tls_deserialize_exact_bytes(package) else {
                continue;
            };
            let Ok(package) = package.validate(mls.provider.crypto(), ProtocolVersion::Mls10)
            else {
                continue;
            };
            let Ok(message) = ExternalProposal::new_add::<OpenMlsRustCrypto>(
                package,
                mls.group.clone(),
                GroupEpoch::from(epoch),
                &mls.signer,
                SenderExtensionIndex::new(0),
            ) else {
                continue;
            };
            if let Ok(bytes) = message.tls_serialize_detached() {
                proposals.extend(bytes);
            }
        }
        let Ok(payload) = VLBytes::new(proposals).tls_serialize_detached() else {
            return;
        };
        let mut local_commit = None;
        for receiver in receivers {
            if receiver == bot_id {
                if let Some(bot) = &mut self.voice.bot {
                    let mut frame = vec![0_u8];
                    frame.extend_from_slice(&payload);
                    bot.binary(27, &frame);
                }
                continue;
            }
            let Some(session) = self
                .voice
                .locals
                .get_mut(&receiver)
                .and_then(|l| l.session.as_mut())
            else {
                continue;
            };
            if let Ok(Some(commit)) =
                session.process_proposals(ProposalsOperationType::APPEND, &payload, None)
                && receiver == committer
            {
                local_commit = Some((commit.commit, commit.welcome));
            }
        }
        if committer == bot_id {
            if let Some(mls) = &mut self.voice.mls {
                mls.awaiting = Some((bot_id, adds));
            }
        } else if let Some((commit, welcome)) = local_commit {
            self.commit(committer, &adds, &commit, welcome.as_deref());
        }
    }

    /// Op 28 from the bot: its commit, and the welcome for whom it adds.
    pub(crate) fn bot_commit(&mut self, payload: &[u8]) {
        let bot_id: u64 = BOT_USER.parse().unwrap_or(0);
        let Some(mls) = &mut self.voice.mls else {
            return;
        };
        let Some((committer, adds)) = mls.awaiting.take() else {
            return;
        };
        if committer != bot_id {
            return;
        }
        let Ok((_, rest)) = MlsMessageIn::tls_deserialize_bytes(payload) else {
            return;
        };
        let commit_len = payload.len().saturating_sub(rest.len());
        let commit = payload[..commit_len].to_vec();
        let welcome = (!rest.is_empty()).then(|| rest.to_vec());
        self.commit(committer, &adds, &commit, welcome.as_deref());
    }

    /// Announce `commit` to the members and welcome the ones it adds.
    fn commit(&mut self, committer: u64, adds: &[u64], commit: &[u8], welcome: Option<&[u8]>) {
        let bot_id: u64 = BOT_USER.parse().unwrap_or(0);
        let Some(mls) = &mut self.voice.mls else {
            return;
        };
        mls.next_transition = mls.next_transition.wrapping_add(1).max(1);
        let transition = mls.next_transition;
        let mut members: Vec<u64> = mls.members.iter().copied().collect();
        if !mls.established {
            members = vec![committer];
        }
        mls.established = true;
        mls.epoch = mls.epoch.saturating_add(1);
        mls.members.extend(members.iter().copied());
        mls.members.extend(adds.iter().copied());
        for id in adds {
            mls.pending.remove(id);
        }
        mls.pending.remove(&committer);
        let includes_bot = members.contains(&bot_id) || adds.contains(&bot_id);
        mls.transition = includes_bot.then_some(transition);
        let mut announce = transition.to_be_bytes().to_vec();
        announce.extend_from_slice(commit);
        for member in &members {
            if *member == bot_id {
                if let Some(bot) = &mut self.voice.bot {
                    bot.binary(29, &announce);
                }
            } else if let Some(session) = self
                .voice
                .locals
                .get_mut(member)
                .and_then(|l| l.session.as_mut())
            {
                let _ = session.process_commit(commit);
            }
        }
        if let Some(welcome) = welcome {
            let mut framed = transition.to_be_bytes().to_vec();
            framed.extend_from_slice(welcome);
            for id in adds {
                if *id == bot_id {
                    if let Some(bot) = &mut self.voice.bot {
                        bot.binary(30, &framed);
                    }
                } else if let Some(session) = self
                    .voice
                    .locals
                    .get_mut(id)
                    .and_then(|l| l.session.as_mut())
                {
                    let _ = session.process_welcome(welcome);
                }
            }
        }
        self.advance();
    }

    /// Op 23 from the bot.
    pub(crate) fn bot_ready(&mut self, transition: u16) {
        let Some(mls) = &mut self.voice.mls else {
            return;
        };
        if mls.transition != Some(transition) {
            return;
        }
        mls.transition = None;
        if let Some(bot) = &mut self.voice.bot {
            bot.json(22, json!({"transition_id": transition}));
        }
        self.advance();
    }

    /// A fake user says one Opus frame to the bot.
    pub(crate) fn say(&mut self, user: &str, opus: &[u8]) {
        let Ok(id) = user.parse::<u64>() else { return };
        let dave = self.voice.dave_version != 0;
        let (Some(bot), Some(local), Some(udp)) = (
            self.voice.bot.as_mut(),
            self.voice.locals.get_mut(&id),
            self.voice.udp.as_ref(),
        ) else {
            return;
        };
        if bot.cipher.is_none() || bot.address.is_none() {
            return;
        }
        if !local.announced {
            local.announced = true;
            let ssrc = local.stream.ssrc();
            let seq = bot.next_seq();
            let frame =
                json!({"op": 5, "d": {"user_id": user, "ssrc": ssrc, "speaking": 1}, "seq": seq});
            let _ = bot.tx.send(Out::Text(frame.to_string()));
        }
        let frame = if dave {
            let Some(session) = local.session.as_mut().filter(|s| s.is_ready()) else {
                return;
            };
            let Ok(sealed) = session.encrypt_opus(opus) else {
                return;
            };
            sealed.into_owned()
        } else {
            opus.to_vec()
        };
        let (Some(cipher), Some(address)) = (&bot.cipher, bot.address) else {
            return;
        };
        if let Some(packet) = local.stream.packet(cipher, &frame) {
            let _ = udp.send_to(&packet, address);
        }
    }

    /// A packet from the bot on UDP.
    pub(crate) fn heard_packet(&mut self, packet: &[u8], from: SocketAddr) {
        let bot_id: u64 = BOT_USER.parse().unwrap_or(0);
        let Some(bot) = &mut self.voice.bot else {
            return;
        };
        bot.address = Some(from);
        let Some(cipher) = &bot.cipher else { return };
        let Some((ssrc, frame)) = rtp::open(cipher, packet) else {
            return;
        };
        if ssrc != bot.ssrc {
            return;
        }
        if frame == SILENCE {
            self.voice.silence = self.voice.silence.saturating_add(1);
            return;
        }
        if !frame.ends_with(&[0xfa, 0xfa]) {
            self.voice.heard.push(Heard {
                opus: frame,
                e2ee: false,
                at_ms: crate::time::now_ms(),
            });
            return;
        }
        let listener = self
            .voice
            .locals
            .values_mut()
            .filter_map(|l| l.session.as_mut())
            .find(|s| s.is_ready());
        if let Some(session) = listener
            && let Ok(opus) = session.decrypt(bot_id, MediaType::AUDIO, &frame)
        {
            self.voice.heard.push(Heard {
                opus,
                e2ee: true,
                at_ms: crate::time::now_ms(),
            });
        }
    }
}

/// Serve the voice gateway on `listener` and the media path on `udp`.
pub(crate) async fn serve(
    shared: Arc<Shared>,
    listener: tokio::net::TcpListener,
    udp: tokio::net::UdpSocket,
) {
    let udp_task = tokio::spawn(udp_loop(Arc::clone(&shared), udp));
    while let Ok((socket, _)) = listener.accept().await {
        tokio::spawn(connection(Arc::clone(&shared), socket));
    }
    udp_task.abort();
}

async fn udp_loop(shared: Arc<Shared>, udp: tokio::net::UdpSocket) {
    let mut buffer = vec![0_u8; 2048];
    while let Ok((length, from)) = udp.recv_from(&mut buffer).await {
        let packet = &buffer[..length];
        // IP discovery: type 1, length 70, then the SSRC.
        if length == 74 && packet[..2] == [0, 1] {
            let mut answer = [0_u8; 74];
            answer[..2].copy_from_slice(&2_u16.to_be_bytes());
            answer[2..4].copy_from_slice(&70_u16.to_be_bytes());
            answer[4..8].copy_from_slice(&packet[4..8]);
            let address = from.ip().to_string();
            if let Some(field) = answer[8..72].get_mut(..address.len()) {
                field.copy_from_slice(address.as_bytes());
            }
            answer[72..74].copy_from_slice(&from.port().to_be_bytes());
            let _ = udp.send_to(&answer, from).await;
            shared.lock().voice.discovered(from);
            continue;
        }
        shared.lock().heard_packet(packet, from);
    }
}

async fn connection(shared: Arc<Shared>, socket: tokio::net::TcpStream) {
    let Ok((_, mut ws)) = tokio_websockets::ServerBuilder::new().accept(socket).await else {
        return;
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let hello = json!({"op": 8, "d": {"heartbeat_interval": HEARTBEAT_INTERVAL_MS}});
    if ws.send(Message::text(hello.to_string())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            frame = ws.next() => {
                let Some(Ok(frame)) = frame else { break };
                if frame.is_close() {
                    break;
                }
                if frame.is_binary() {
                    handle_binary(&mut shared.lock(), &tx, frame.as_payload());
                } else if let Some(text) = frame.as_text() {
                    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                    handle(&mut shared.lock(), &tx, &parsed);
                }
            }
            outgoing = rx.recv() => match outgoing {
                Some(Out::Text(text)) => {
                    if ws.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                Some(Out::Binary(bytes)) => {
                    if ws.send(Message::binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close(code, reason)) => {
                    let _ = ws.send(Message::close(CloseCode::try_from(code).ok(), &reason)).await;
                    let _ = ws.close().await;
                    break;
                }
                None => break,
            },
        }
    }
}

fn is_live(state: &State, tx: &mpsc::UnboundedSender<Out>) -> bool {
    state
        .voice
        .bot
        .as_ref()
        .is_some_and(|bot| bot.tx.same_channel(tx))
}

fn handle(state: &mut State, tx: &mpsc::UnboundedSender<Out>, frame: &Value) {
    let data = &frame["d"];
    match frame["op"].as_u64() {
        Some(0) => identify(state, tx, data),
        Some(7) => {
            state.voice.resumes = state.voice.resumes.saturating_add(1);
            let known = state.voice.bot.as_ref().is_some_and(|bot| {
                data["session_id"] == bot.session_id.as_str()
                    && data["token"] == state.voice.token.as_str()
            });
            if !known {
                let _ = tx.send(Out::Close(4006, "Session is no longer valid.".into()));
                return;
            }
            if let Some(bot) = &mut state.voice.bot {
                bot.tx = tx.clone();
                bot.seq_ack = data["seq_ack"].as_i64().unwrap_or(-1);
                bot.json(9, Value::Null);
            }
        }
        Some(3) => {
            if let Some(bot) = state.voice.bot.as_mut().filter(|b| b.tx.same_channel(tx)) {
                bot.seq_ack = data["seq_ack"].as_i64().unwrap_or(bot.seq_ack);
            }
            let _ = tx.send(Out::Text(
                json!({"op": 6, "d": {"t": data["t"]}}).to_string(),
            ));
        }
        Some(1) if is_live(state, tx) => select_protocol(state, data),
        Some(5) if is_live(state, tx) => {
            state
                .voice
                .speaking
                .push(data["speaking"].as_u64().unwrap_or(0));
        }
        Some(23) if is_live(state, tx) => {
            if let Some(transition) = data["transition_id"]
                .as_u64()
                .and_then(|t| u16::try_from(t).ok())
            {
                state.bot_ready(transition);
            }
        }
        Some(31) if is_live(state, tx) => state.restart_group(),
        _ => {}
    }
}

fn handle_binary(state: &mut State, tx: &mpsc::UnboundedSender<Out>, frame: &[u8]) {
    if !is_live(state, tx) {
        return;
    }
    let bot_id: u64 = BOT_USER.parse().unwrap_or(0);
    match frame.split_first() {
        Some((26, package)) => {
            if let Some(mls) = &mut state.voice.mls {
                mls.pending.insert(bot_id, package.to_vec());
            }
            state.advance();
        }
        Some((28, payload)) => state.bot_commit(payload),
        _ => {}
    }
}

fn identify(state: &mut State, tx: &mpsc::UnboundedSender<Out>, data: &Value) {
    state.voice.identifies = state.voice.identifies.saturating_add(1);
    let valid = data["server_id"] == GUILD
        && data["user_id"] == BOT_USER
        && state
            .voice
            .bot_session
            .as_deref()
            .is_some_and(|s| data["session_id"] == s)
        && data["token"] == state.voice.token.as_str();
    if !valid {
        let _ = tx.send(Out::Close(4004, "Authentication failed.".into()));
        return;
    }
    let dave = data["max_dave_protocol_version"].as_u64().unwrap_or(0);
    if dave == 0 && !state.voice.no_dave {
        let _ = tx.send(Out::Close(4017, "E2EE/DAVE protocol required.".into()));
        return;
    }
    if let Some(old) = state.voice.bot.take() {
        let _ = old
            .tx
            .send(Out::Close(4006, "Session is no longer valid.".into()));
    }
    state.voice.mls = None;
    let ssrc = state.voice.ssrc();
    let mut bot = Bot {
        tx: tx.clone(),
        session_id: data["session_id"].as_str().unwrap_or_default().to_owned(),
        ssrc,
        cipher: None,
        address: None,
        seq: 0,
        seq_ack: -1,
    };
    bot.json(
        2,
        json!({
            "ssrc": ssrc, "ip": "127.0.0.1", "port": state.voice.udp_port,
            "modes": MODES, "heartbeat_interval": 1,
        }),
    );
    let in_call: Vec<String> = state
        .voice
        .present
        .iter()
        .filter(|(_, p)| state.voice.bot_channel.as_deref() == Some(p.channel.as_str()))
        .map(|(id, _)| id.clone())
        .collect();
    if !in_call.is_empty() {
        bot.json(11, json!({"user_ids": in_call}));
    }
    state.voice.bot = Some(bot);
}

fn select_protocol(state: &mut State, data: &Value) {
    let mode = data["data"]["mode"].as_str().unwrap_or_default().to_owned();
    if mode != rtp::AES_GCM && mode != rtp::XCHACHA {
        if let Some(bot) = &state.voice.bot {
            let _ = bot
                .tx
                .send(Out::Close(4016, "Unknown encryption mode.".into()));
        }
        return;
    }
    let key = random::<32>();
    let Ok(cipher) = Cipher::new(&mode, &key) else {
        return;
    };
    let version = u16::from(!state.voice.no_dave);
    state.voice.dave_version = version;
    let Some(bot) = &mut state.voice.bot else {
        return;
    };
    bot.cipher = Some(cipher);
    bot.json(
        4,
        json!({"mode": mode, "secret_key": key.to_vec(), "dave_protocol_version": version}),
    );
    if version == 0 {
        return;
    }
    let Some(mls) = new_mls(state.voice.bot_channel.as_deref()) else {
        return;
    };
    bot.binary(25, &mls.external);
    state.voice.mls = Some(mls);
    let in_call: Vec<(String, u64)> = state
        .voice
        .present
        .iter()
        .filter(|(_, p)| state.voice.bot_channel.as_deref() == Some(p.channel.as_str()))
        .filter_map(|(id, _)| Some((id.clone(), id.parse().ok()?)))
        .collect();
    state.voice.locals.clear();
    for (_, id) in &in_call {
        let ssrc = state.voice.ssrc();
        state.voice.locals.insert(
            *id,
            Local {
                session: None,
                stream: Stream::new(ssrc, random::<10>()),
                announced: false,
            },
        );
        state.local_key_package(*id);
    }
    state.advance();
}

/// A new group for the bot's call, the fake as its external sender.
fn new_mls(channel: Option<&str>) -> Option<Mls> {
    let channel: u64 = channel?.parse().ok()?;
    let signer = SignatureKeyPair::new(SignatureScheme::ECDSA_SECP256R1_SHA256).ok()?;
    let credential = BasicCredential::new(b"discord-voice".to_vec());
    let sender = ExternalSender::new(signer.public().to_vec().into(), credential.into());
    let external = sender.tls_serialize_detached().ok()?;
    Some(Mls {
        signer,
        external,
        provider: OpenMlsRustCrypto::default(),
        group: GroupId::from_slice(&channel.to_be_bytes()),
        epoch: 0,
        established: false,
        members: BTreeSet::new(),
        pending: BTreeMap::new(),
        awaiting: None,
        transition: None,
        next_transition: 0,
    })
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    let _ = getrandom::fill(&mut bytes);
    bytes
}
