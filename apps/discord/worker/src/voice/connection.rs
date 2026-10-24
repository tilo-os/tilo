//! One voice connection: the voice gateway (v8) and the UDP socket that
//! carries the call's audio both ways.
//!
//! Identify (with the DAVE version the worker speaks), Hello and Ready, IP
//! discovery on UDP, Select Protocol with the best transport mode, Session
//! Description with the key; then heartbeats acknowledging the last
//! sequence seen, DAVE's opcodes, who speaks on which SSRC, and resumption
//! when the socket drops. Received audio goes to the host as it arrives;
//! the host's audio is mixed and sent on a 20 ms tick. The two never meet,
//! so nothing a Discord user says is sent back to Discord.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tilo_connector::protocol::{Bytes, FromWorker};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_websockets::Message;
use url::Url;
use zeroize::Zeroizing;

use super::call::LinkEvent;
use super::dave::{self, Dave, Frame};
use super::mix::Mixer;
use super::rtp::{self, Cipher, Sender};
use crate::Ctx;
use crate::egress::Purpose;
use crate::gateway::{Socket, connect};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME: Duration = Duration::from_millis(20);
/// Socket drops in a row the connection rides out before giving up.
const MAX_DROPS: u32 = 5;
/// Opus silence, sent five times before a pause so the receivers'
/// decoders do not interpolate across it.
pub const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];
const SILENCE_FRAMES: u8 = 5;
/// Speaking flag: the microphone.
const MICROPHONE: u8 = 1;

/// Everything Discord handed out for one join.
pub struct Join {
    pub attempt: u64,
    pub guild: String,
    pub channel: String,
    pub user: String,
    pub session: String,
    pub token: Zeroizing<String>,
    pub endpoint: String,
}

enum End {
    /// Open the socket again and resume.
    Resume,
    /// Open the socket again and identify afresh.
    Identify,
    /// The call is over for the bot, for this reason.
    Fatal(String),
}

/// What outlives one socket: the UDP path, the keys, the MLS group, who
/// is on which SSRC, the outgoing stream's counters.
struct Link {
    ctx: Arc<Ctx>,
    join: Join,
    url: Url,
    seq_ack: i64,
    ssrc: u32,
    udp: Option<UdpSocket>,
    mode: Option<&'static str>,
    cipher: Option<Cipher>,
    sender: Option<Sender>,
    dave: Dave,
    users: HashMap<u32, u64>,
    mixer: Mixer,
    speaking: bool,
    silence_left: u8,
    joined: bool,
}

/// Run the voice connection for `join` until it is aborted or ends for
/// good, saying which on `links`.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the host's audio belongs to this call for as long as it runs"
)]
pub async fn run(ctx: Arc<Ctx>, join: Join, links: mpsc::UnboundedSender<LinkEvent>) {
    let attempt = join.attempt;
    let url = match voice_url(&ctx, &join.endpoint) {
        Ok(url) => url,
        Err(reason) => {
            let _ = links.send(LinkEvent::Refused { attempt, reason });
            return;
        }
    };
    let (Ok(user), Ok(channel)) = (join.user.parse::<u64>(), join.channel.parse::<u64>()) else {
        let reason = "Discord named a call by an id that is not a snowflake".to_owned();
        let _ = links.send(LinkEvent::Refused { attempt, reason });
        return;
    };
    let audio = Arc::clone(&ctx.audio);
    let mut audio = audio.lock().await;
    // What the host said while no call was up is stale now.
    while audio.try_recv().is_ok() {}
    let mut link = Link {
        ctx,
        join,
        url,
        seq_ack: -1,
        ssrc: 0,
        udp: None,
        mode: None,
        cipher: None,
        sender: None,
        dave: Dave::new(user, channel),
        users: HashMap::new(),
        mixer: Mixer::default(),
        speaking: false,
        silence_left: 0,
        joined: false,
    };
    let mut resume = false;
    let mut drops = 0_u32;
    loop {
        let established_before = link.joined;
        let end = link.socket(resume, &mut audio, &links).await;
        match end {
            End::Fatal(reason) => {
                let _ = links.send(LinkEvent::Refused { attempt, reason });
                return;
            }
            End::Resume => resume = link.cipher.is_some(),
            End::Identify => {
                resume = false;
                link.reset();
            }
        }
        drops = if link.joined && !established_before {
            0
        } else {
            drops.saturating_add(1)
        };
        if drops > MAX_DROPS {
            let reason = "lost the voice connection to Discord".to_owned();
            let _ = links.send(LinkEvent::Refused { attempt, reason });
            return;
        }
        tokio::time::sleep(Duration::from_millis(
            250u64.saturating_mul(u64::from(drops)),
        ))
        .await;
    }
}

/// `wss://<endpoint>/?v=8`, checked against the voice egress rule. A
/// loopback endpoint, allowed only for tests, is plain `ws://`.
fn voice_url(ctx: &Ctx, endpoint: &str) -> Result<Url, String> {
    let mut url = Url::parse(&format!("wss://{endpoint}"))
        .map_err(|_| format!("Discord named a voice server that is not a host: {endpoint}"))?;
    let loopback = url
        .host_str()
        .is_some_and(|h| h == "localhost" || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    if loopback && ctx.config.egress.allow_loopback {
        let _ = url.set_scheme("ws");
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/");
    }
    url.set_query(Some("v=8"));
    ctx.config
        .egress
        .check(&url, Purpose::Voice)
        .map_err(|error| format!("voice server refused: {error}"))?;
    Ok(url)
}

impl Link {
    /// Forget the UDP path and keys: a fresh identify negotiates them anew.
    fn reset(&mut self) {
        self.udp = None;
        self.cipher = None;
        self.sender = None;
        self.mode = None;
        self.seq_ack = -1;
        self.speaking = false;
        if let (Ok(user), Ok(channel)) = (self.join.user.parse(), self.join.channel.parse()) {
            self.dave = Dave::new(user, channel);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one socket from connect to close, read top to bottom"
    )]
    async fn socket(
        &mut self,
        resume: bool,
        audio: &mut mpsc::Receiver<(String, Bytes)>,
        links: &mpsc::UnboundedSender<LinkEvent>,
    ) -> End {
        let Some(mut socket) = connect(&self.url).await else {
            return End::Resume;
        };
        let opening = if resume {
            json!({"op": 7, "d": {
                "server_id": self.join.guild,
                "session_id": self.join.session,
                "token": self.join.token.as_str(),
                "seq_ack": self.seq_ack,
            }})
        } else {
            json!({"op": 0, "d": {
                "server_id": self.join.guild,
                "user_id": self.join.user,
                "session_id": self.join.session,
                "token": self.join.token.as_str(),
                "max_dave_protocol_version": dave::MAX_VERSION,
            }})
        };
        let opening = Zeroizing::new(opening.to_string());
        if socket
            .send(Message::text(opening.as_str().to_owned()))
            .await
            .is_err()
        {
            return End::Resume;
        }
        drop(opening);
        let far = Instant::now()
            .checked_add(Duration::from_secs(86_400))
            .unwrap_or_else(Instant::now);
        let mut beat = tokio::time::interval_at(far, Duration::from_secs(86_400));
        let mut acked = true;
        let mut tick = tokio::time::interval(FRAME);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let deadline = tokio::time::sleep(CONNECT_TIMEOUT);
        tokio::pin!(deadline);
        let mut live = false;
        let mut buffer = vec![0_u8; 2048];
        loop {
            tokio::select! {
                () = &mut deadline, if !live => return End::Resume,
                _ = beat.tick() => {
                    if !acked {
                        return End::Resume;
                    }
                    acked = false;
                    let beat = json!({"op": 3, "d": {"t": tilo_clock::unix_now_ms_i64(), "seq_ack": self.seq_ack}});
                    if socket.send(Message::text(beat.to_string())).await.is_err() {
                        return End::Resume;
                    }
                }
                frame = socket.next() => {
                    let Some(Ok(frame)) = frame else { return End::Resume };
                    if let Some((code, reason)) = frame.as_close() {
                        return closed(u16::from(code), reason);
                    }
                    let answers = if frame.is_binary() {
                        self.binary(frame.as_payload())
                    } else if let Some(text) = frame.as_text() {
                        let Ok(message) = serde_json::from_str::<Value>(text) else { continue };
                        if let Some(seq) = message["seq"].as_i64() {
                            self.seq_ack = self.seq_ack.max(seq);
                        }
                        let data = &message["d"];
                        match message["op"].as_u64() {
                            Some(8) => {
                                let every = data["heartbeat_interval"]
                                    .as_f64()
                                    .and_then(|ms| Duration::try_from_secs_f64(ms.clamp(100.0, 600_000.0) / 1000.0).ok())
                                    .unwrap_or(Duration::from_millis(13_750));
                                beat = tokio::time::interval(every);
                                beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
                                Vec::new()
                            }
                            Some(6) => {
                                acked = true;
                                Vec::new()
                            }
                            Some(2) => match self.ready(data).await {
                                Ok(select) => vec![Frame::Json(select)],
                                Err(end) => return end,
                            },
                            Some(4) => match self.session_description(data) {
                                Ok(frames) => {
                                    live = true;
                                    if !self.joined {
                                        self.joined = true;
                                        let _ = links.send(LinkEvent::Joined { attempt: self.join.attempt });
                                    }
                                    frames
                                }
                                Err(reason) => return End::Fatal(reason),
                            },
                            Some(9) => {
                                live = true;
                                Vec::new()
                            }
                            Some(5) => {
                                if let (Some(ssrc), Some(user)) = (
                                    data["ssrc"].as_u64().and_then(|s| u32::try_from(s).ok()),
                                    data["user_id"].as_str().and_then(|u| u.parse::<u64>().ok()),
                                ) {
                                    self.users.insert(ssrc, user);
                                }
                                Vec::new()
                            }
                            Some(op @ (11 | 13 | 21 | 22 | 24)) => {
                                if op == 13
                                    && let Some(gone) = data["user_id"].as_str().and_then(|u| u.parse::<u64>().ok())
                                {
                                    self.users.retain(|_, user| *user != gone);
                                }
                                self.dave.json(op, data)
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    for answer in answers {
                        let message = match answer {
                            Frame::Json(value) => Message::text(value.to_string()),
                            Frame::Binary(bytes) => Message::binary(bytes),
                        };
                        if socket.send(message).await.is_err() {
                            return End::Resume;
                        }
                    }
                }
                received = recv(self.udp.as_ref(), &mut buffer) => {
                    if let Some(length) = received {
                        self.heard(buffer.get(..length).unwrap_or_default());
                    }
                }
                frame = audio.recv(), if live => {
                    if let Some((speaker, mut opus)) = frame {
                        self.mixer.push(&speaker, std::mem::take(&mut opus.0));
                    }
                }
                _ = tick.tick(), if live => {
                    if self.send_tick(&mut socket).await.is_err() {
                        return End::Resume;
                    }
                }
            }
        }
    }

    /// The voice server's Ready: its UDP address, our SSRC and the modes.
    /// Find our public address on UDP and answer with Select Protocol.
    async fn ready(&mut self, data: &Value) -> Result<Value, End> {
        let ssrc = data["ssrc"].as_u64().and_then(|s| u32::try_from(s).ok());
        let ip = data["ip"].as_str().and_then(|ip| ip.parse::<IpAddr>().ok());
        let port = data["port"].as_u64().and_then(|p| u16::try_from(p).ok());
        let (Some(ssrc), Some(ip), Some(port)) = (ssrc, ip, port) else {
            return Err(End::Fatal("the voice server's Ready is incomplete".into()));
        };
        let modes = data["modes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let Some(mode) = rtp::choose_mode(modes) else {
            return Err(End::Fatal(
                "the voice server offers no encryption mode the worker supports".into(),
            ));
        };
        self.ctx
            .config
            .egress
            .check_voice_udp(ip)
            .map_err(|error| End::Fatal(format!("voice server refused: {error}")))?;
        let bind: SocketAddr = if ip.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            ([0_u16; 8], 0).into()
        };
        let udp = UdpSocket::bind(bind).await.map_err(|_| End::Resume)?;
        udp.connect((ip, port)).await.map_err(|_| End::Resume)?;
        let (address, public_port) = discover(&udp, ssrc).await.ok_or(End::Resume)?;
        self.ssrc = ssrc;
        self.mode = Some(mode);
        self.udp = Some(udp);
        Ok(json!({"op": 1, "d": {
            "protocol": "udp",
            "data": {"address": address, "port": public_port, "mode": mode},
        }}))
    }

    /// Session Description: the key, and the DAVE version the call starts
    /// with.
    fn session_description(&mut self, data: &Value) -> Result<Vec<Frame>, String> {
        let mode = data["mode"].as_str().unwrap_or_default();
        if Some(mode) != self.mode {
            return Err(format!(
                "the voice server chose another encryption mode ({mode})"
            ));
        }
        let key: Zeroizing<Vec<u8>> = Zeroizing::new(
            data["secret_key"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect(),
        );
        self.cipher = Some(
            Cipher::new(mode, &key)
                .map_err(|error| format!("the voice server's key does not fit {mode}: {error}"))?,
        );
        let mut start = [0_u8; 10];
        getrandom::fill(&mut start).map_err(|_| "no randomness for the RTP stream".to_owned())?;
        self.sender = Some(Sender::new(self.ssrc, start));
        let version = data["dave_protocol_version"]
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0);
        Ok(self.dave.start(version))
    }

    /// A binary opcode: a big-endian sequence number, then opcode and
    /// payload.
    fn binary(&mut self, frame: &[u8]) -> Vec<Frame> {
        let Some((seq, rest)) = frame.split_first_chunk::<2>() else {
            return Vec::new();
        };
        self.seq_ack = self.seq_ack.max(i64::from(u16::from_be_bytes(*seq)));
        self.dave.binary(rest)
    }

    /// A packet on the UDP socket: a Discord user's audio for the host.
    fn heard(&mut self, packet: &[u8]) {
        let Some(cipher) = &self.cipher else { return };
        let Some(received) = rtp::open(cipher, packet) else {
            return;
        };
        let Some(&user) = self.users.get(&received.ssrc) else {
            return;
        };
        let Some(opus) = self.dave.decrypt(user, &received.frame) else {
            return;
        };
        if opus == SILENCE {
            return;
        }
        let _ = self.ctx.out.send(FromWorker::Audio {
            speaker: user.to_string(),
            opus: Bytes(opus),
        });
    }

    /// Send the next 20 ms of the Tilo call, or the silence that ends it,
    /// with the Speaking op ahead of the first packet and after the last.
    async fn send_tick(&mut self, socket: &mut Socket) -> Result<(), ()> {
        let mixed = self.mixer.tick().filter(|_| self.dave.can_send());
        let frame = match mixed {
            Some(frame) => {
                self.silence_left = SILENCE_FRAMES;
                if !self.speaking {
                    self.speaking = true;
                    send_speaking(socket, self.ssrc, MICROPHONE).await?;
                }
                frame
            }
            None if self.speaking && self.silence_left > 0 => {
                self.silence_left = self.silence_left.saturating_sub(1);
                SILENCE.to_vec()
            }
            None if self.speaking => {
                self.speaking = false;
                return send_speaking(socket, self.ssrc, 0).await;
            }
            None => return Ok(()),
        };
        if let (Some(sealed), Some(cipher), Some(sender), Some(udp)) = (
            self.dave.encrypt(&frame),
            &self.cipher,
            &mut self.sender,
            &self.udp,
        ) && let Some(packet) = sender.packet(cipher, &sealed)
        {
            let _ = udp.send(&packet).await;
        }
        Ok(())
    }
}

/// Op 5: whether the worker is speaking on `ssrc`.
async fn send_speaking(socket: &mut Socket, ssrc: u32, flag: u8) -> Result<(), ()> {
    let op = json!({"op": 5, "d": {"speaking": flag, "delay": 0, "ssrc": ssrc}});
    socket
        .send(Message::text(op.to_string()))
        .await
        .map_err(|_| ())
}

/// Receive on `udp`, or wait forever when there is none yet.
async fn recv(udp: Option<&UdpSocket>, buffer: &mut [u8]) -> Option<usize> {
    match udp {
        Some(udp) => udp.recv(buffer).await.ok(),
        None => std::future::pending().await,
    }
}

/// Discord's IP discovery: our address and port as the voice server sees
/// them.
async fn discover(udp: &UdpSocket, ssrc: u32) -> Option<(String, u16)> {
    let mut request = [0_u8; 74];
    request[..2].copy_from_slice(&1_u16.to_be_bytes());
    request[2..4].copy_from_slice(&70_u16.to_be_bytes());
    request[4..8].copy_from_slice(&ssrc.to_be_bytes());
    udp.send(&request).await.ok()?;
    let mut answer = [0_u8; 74];
    let length = tokio::time::timeout(DISCOVERY_TIMEOUT, udp.recv(&mut answer))
        .await
        .ok()?
        .ok()?;
    if length < 74 || answer[..2] != 2_u16.to_be_bytes() {
        return None;
    }
    let address = answer.get(8..72)?;
    let end = address.iter().position(|&b| b == 0)?;
    let address = std::str::from_utf8(address.get(..end)?).ok()?;
    let address: IpAddr = address.parse().ok()?;
    let port = u16::from_be_bytes([answer[72], answer[73]]);
    Some((address.to_string(), port))
}

/// What a voice gateway close code means for the call.
fn closed(code: u16, reason: &str) -> End {
    match code {
        // Session gone or timed out: identify again.
        4006 | 4009 => End::Identify,
        4014 => End::Fatal("Discord disconnected the bot from the call".into()),
        4021 => End::Fatal("Discord rate-limited the bot's voice connection".into()),
        4022 => End::Fatal("Discord ended the call".into()),
        4001..=4005 | 4011 | 4012 | 4016 | 4017 | 4020 => End::Fatal(format!(
            "Discord's voice server closed the connection: {} ({code})",
            if reason.is_empty() {
                "no reason given"
            } else {
                reason
            }
        )),
        // 4015 (the voice server crashed) and anything unlisted.
        _ => End::Resume,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn close_codes_resume_identify_or_end() {
        assert!(matches!(closed(4015, ""), End::Resume));
        assert!(matches!(closed(1006, ""), End::Resume));
        assert!(matches!(closed(4006, ""), End::Identify));
        assert!(matches!(closed(4014, ""), End::Fatal(_)));
        let End::Fatal(reason) = closed(4017, "E2EE/DAVE protocol required") else {
            panic!("4017 ends the call");
        };
        assert!(reason.contains("E2EE/DAVE protocol required (4017)"));
    }
}
