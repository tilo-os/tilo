#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

//! The worker's voice side against the fake Discord, through the connector
//! protocol: occupancy, joining and leaving, audio both ways with DAVE, no
//! echo, mute and speaking, and refusals that name what is missing.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::presence::CallPresence;
use fake_discord::{CHANNEL_VOICE, CONNECT_AND_SPEAK, FakeDiscord, Heard, TOKEN, User, tone};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_connector::framing::{read_frame_async, write_frame_async};
use tilo_connector::protocol::{Bytes, CallLink, FromWorker, OutsideCall, ToWorker};
use tilo_live::app_call::{BridgedCall, CallsReport, ConnectorCall, Lane, OutsideAudio};
use tilo_live::calls::CallRule;
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{Board, Call, Own};
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::speaker::Speaker;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tokio::sync::{broadcast, mpsc, watch};

/// What the worker said that these tests look at.
#[derive(Clone, Debug)]
enum Seen {
    Calls(Vec<OutsideCall>, CallLink),
    Audio(String, Vec<u8>),
    Status(bool, String),
}

pub(super) struct Host {
    fake: FakeDiscord,
    to_worker: mpsc::UnboundedSender<ToWorker>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// The worker's reports and audio, as a bridge reads them.
    calls: watch::Sender<CallsReport>,
    audio: broadcast::Sender<OutsideAudio>,
}

impl Host {
    async fn start() -> Self {
        Self::with(FakeDiscord::start().await.expect("fake discord")).await
    }

    async fn with(fake: FakeDiscord) -> Self {
        let vars: HashMap<&str, String> = [
            ("DISCORD_API_BASE", fake.api_base()),
            ("DISCORD_CDN_BASE", fake.cdn_base()),
            ("DISCORD_ALLOW_LOOPBACK", "1".to_owned()),
        ]
        .into_iter()
        .collect();
        let config =
            discord_worker::Config::from_vars(|name| vars.get(name).cloned()).expect("config");
        let (host_end, worker_end) = tokio::io::duplex(1 << 22);
        let (worker_read, worker_write) = tokio::io::split(worker_end);
        tokio::spawn(discord_worker::run(
            config,
            tokio::io::BufReader::new(worker_read),
            worker_write,
        ));
        let (host_read, mut host_write) = tokio::io::split(host_end);
        let (to_worker, mut outgoing) = mpsc::unbounded_channel::<ToWorker>();
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if write_frame_async(&mut host_write, &message).await.is_err() {
                    return;
                }
            }
        });
        let (calls, _) = watch::channel(CallsReport::default());
        let (audio, _) = broadcast::channel(256);
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let (seen, to_worker) = (seen.clone(), to_worker.clone());
            let (reports, audio) = (calls.clone(), audio.clone());
            tokio::spawn(async move {
                let mut input = tokio::io::BufReader::new(host_read);
                while let Ok(Some(message)) = read_frame_async::<_, FromWorker>(&mut input).await {
                    let entry = match message {
                        FromWorker::NeedSecret { purpose, item } => {
                            let _ = to_worker.send(ToWorker::Secret {
                                purpose,
                                item,
                                bytes: Some(Bytes(TOKEN.as_bytes().to_vec())),
                            });
                            continue;
                        }
                        FromWorker::Calls { calls, link } => {
                            let _ = reports.send(CallsReport {
                                calls: calls.clone(),
                                link: link.clone(),
                            });
                            Seen::Calls(calls, link)
                        }
                        FromWorker::Audio { speaker, opus } => {
                            let _ = audio.send(OutsideAudio {
                                speaker: speaker.clone(),
                                opus: opus.0.clone(),
                            });
                            Seen::Audio(speaker, opus.0.clone())
                        }
                        FromWorker::Status { healthy, message } => Seen::Status(healthy, message),
                        _ => continue,
                    };
                    seen.lock().unwrap().push(entry);
                }
            });
        }
        to_worker
            .send(ToWorker::Start {
                version: tilo_connector::protocol::PROTOCOL_VERSION,
                checkpoint: None,
            })
            .unwrap();
        let host = Self {
            fake,
            to_worker,
            seen,
            calls,
            audio,
        };
        assert!(
            host.fake
                .eventually(tilo_test_env::hang_bound(), |f| f.connected().then_some(()))
                .await
                .is_some(),
            "the worker never connected"
        );
        // The guild arrived once the first occupancy report is out.
        host.wait("the first Calls report", |seen| {
            matches!(seen, Seen::Calls(..)).then_some(())
        })
        .await;
        host
    }

    async fn wait<T>(&self, what: &str, mut find: impl FnMut(&Seen) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let found = self.seen.lock().unwrap().iter().rev().find_map(&mut find);
            if let Some(found) = found {
                return found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never saw {what}; saw {:#?}",
                self.seen.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Poll `check` until it answers.
    async fn until<T>(&self, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
        tilo_test_env::within(what, async {
            loop {
                if let Some(found) = check() {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
    }

    /// The latest Calls report.
    fn calls(&self) -> Option<(Vec<OutsideCall>, CallLink)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|seen| match seen {
                Seen::Calls(calls, link) => Some((calls.clone(), link.clone())),
                _ => None,
            })
    }

    /// The bridge's lane: the worker's reports and audio, and the asks and
    /// frames it hears.
    fn lane(&self) -> Lane {
        let (say, mut says) = mpsc::unbounded_channel();
        let (speak, mut speaks) = mpsc::unbounded_channel();
        let asked = self.to_worker.clone();
        tokio::spawn(async move {
            while let Some((call, status)) = says.recv().await {
                let _ = asked.send(ToWorker::Call { call, status });
            }
        });
        let spoke = self.to_worker.clone();
        tokio::spawn(async move {
            while let Some((speaker, opus)) = speaks.recv().await {
                let _ = spoke.send(ToWorker::Audio {
                    speaker,
                    opus: Bytes(opus),
                });
            }
        });
        Lane {
            calls: self.calls.subscribe(),
            audio: self.audio.subscribe(),
            say,
            speak,
        }
    }

    /// Wait until the latest Calls report satisfies `check`.
    async fn calls_until(&self, what: &str, check: impl Fn(&[OutsideCall], &CallLink) -> bool) {
        let deadline = tokio::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            if let Some((calls, link)) = self.calls()
                && check(&calls, &link)
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never saw {what}; last report {:#?}",
                self.calls()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn call(&self, call: Option<&str>, status: Option<&str>) {
        self.to_worker
            .send(ToWorker::Call {
                call: call.map(str::to_owned),
                status: status.map(str::to_owned),
            })
            .unwrap();
    }

    async fn joined(&self) {
        self.calls_until(
            "the bot joined",
            |_, link| matches!(link, CallLink::Joined { call } if call == CHANNEL_VOICE),
        )
        .await;
    }

    async fn dave_ready(&self) {
        assert!(
            self.fake
                .eventually(tilo_test_env::hang_bound(), |f| f
                    .voice_dave_ready()
                    .then_some(()))
                .await
                .is_some(),
            "DAVE never got established"
        );
    }

    /// Send `frames` as `speaker` in real time. A late tick delays the rest
    /// rather than replaying the cadence it missed: audio arrives at the
    /// wall clock's pace, never as a burst the jitter buffer would shed.
    async fn say(&self, speaker: &str, frames: &[Vec<u8>]) {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        for frame in frames {
            tick.tick().await;
            self.to_worker
                .send(ToWorker::Audio {
                    speaker: speaker.to_owned(),
                    opus: Bytes(frame.clone()),
                })
                .unwrap();
        }
    }

    fn audio_from(&self, speaker: &str) -> Vec<Vec<u8>> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|seen| match seen {
                Seen::Audio(who, opus) if who == speaker => Some(opus.clone()),
                _ => None,
            })
            .collect()
    }
}

fn ada() -> User {
    User::new("7001", "Ada").with_avatar("a1")
}

fn bob() -> User {
    User::new("7002", "Bob")
}

/// The share of the energy at `hz` in what `frames` decode to, past the
/// codec's warm-up.
pub(super) fn tone_share(frames: &[Vec<u8>], hz: f32) -> f32 {
    let samples = tone::decode(frames);
    let skip = (tone::FRAME_SAMPLES * 5).min(samples.len());
    tone::share(&samples[skip..], hz)
}

/// The share of the energy at `hz` in what a receiver of `frames` hears, past
/// the codec's warm-up: a sequence number the sender spent without a packet
/// is concealed, as a receiver does when the sender drops audio.
pub(super) fn tone_share_received(frames: &[Heard], hz: f32) -> f32 {
    let samples = tone::decode_received(frames);
    let skip = (tone::FRAME_SAMPLES * 5).min(samples.len());
    tone::share(&samples[skip..], hz)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn occupancy_and_the_bots_own_place_follow_discord() {
    let host = Host::start().await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.calls_until("Ada in voice", |calls, link| {
        *link == CallLink::Idle
            && calls.len() == 1
            && calls[0].call == CHANNEL_VOICE
            && calls[0].people.len() == 1
            && calls[0].people[0].account == "7001"
            && calls[0].people[0].name == "Ada"
            && calls[0].people[0]
                .avatar_url
                .as_deref()
                .is_some_and(|url| url.ends_with("/avatars/7001/a1.png?size=128"))
            && !calls[0].people[0].muted
    })
    .await;

    host.call(Some(CHANNEL_VOICE), Some("Tilo: Grace"));
    host.calls_until("Joining", |_, link| {
        matches!(link, CallLink::Joining { .. } | CallLink::Joined { .. })
    })
    .await;
    host.joined().await;
    let (calls, _) = host.calls().unwrap();
    assert!(
        calls[0]
            .people
            .iter()
            .all(|p| p.account != fake_discord::BOT_USER),
        "the bot is never one of the people"
    );
    assert_eq!(
        host.fake.voice_bot_channel().as_deref(),
        Some(CHANNEL_VOICE)
    );
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_status(CHANNEL_VOICE)
                .filter(|s| s == "Tilo: Grace"))
            .await
            .is_some(),
        "the voice channel shows who is on the Tilo side"
    );

    host.fake.voice_leave("7001");
    host.calls_until("Ada gone", |calls, _| calls.is_empty())
        .await;

    host.call(None, None);
    host.calls_until("Idle", |_, link| *link == CallLink::Idle)
        .await;
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_bot_channel()
                .is_none()
                .then_some(()))
            .await
            .is_some(),
        "the bot left the voice channel"
    );
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_status(CHANNEL_VOICE)
                .is_none()
                .then_some(()))
            .await
            .is_some(),
        "the status is cleared"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_goes_both_ways_end_to_end_encrypted_without_echo() {
    let host = Host::start().await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.call(Some(CHANNEL_VOICE), None);
    host.joined().await;
    host.dave_ready().await;
    assert_eq!(host.fake.voice_dave_version(), 1);
    let codes = host.fake.voice_privacy_codes();
    assert!(!codes.is_empty() && codes.iter().all(|c| c == &codes[0]));

    // Tilo to Discord: a 440 Hz tone.
    host.say("tilo-grace", &tone::sine(440.0, 50)).await;
    let heard = host
        .fake
        .eventually(tilo_test_env::hang_bound(), |f| {
            let heard = f.voice_heard_frames();
            (heard.len() >= 40).then_some(heard)
        })
        .await
        .expect("the fake heard the bot");
    assert!(
        heard.iter().all(|h| h.e2ee),
        "every frame came through DAVE"
    );
    assert!(
        tone_share_received(&heard, 440.0) > 0.5,
        "440 Hz arrived: {}",
        tone_share_received(&heard, 440.0)
    );

    // Discord to Tilo: Ada says 660 Hz.
    host.fake.voice_say("7001", &tone::sine(660.0, 50)).await;
    let from_ada = host
        .until("Ada's audio", || {
            let frames = host.audio_from("7001");
            (frames.len() >= 40).then_some(frames)
        })
        .await;
    assert!(
        tone_share(&from_ada, 660.0) > 0.5,
        "660 Hz arrived: {}",
        tone_share(&from_ada, 660.0)
    );

    // Nothing Ada said went back to Discord.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let back: Vec<Vec<u8>> = host.fake.voice_heard();
    assert!(
        tone_share(&back, 660.0) < 0.05,
        "Ada's tone never comes back from the bot"
    );
    assert!(
        back.len() <= 50,
        "the bot sent only the Tilo tone ({} frames)",
        back.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_tilo_speakers_are_mixed_and_speaking_follows_the_audio() {
    let host = Host::start().await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.call(Some(CHANNEL_VOICE), None);
    host.joined().await;
    host.dave_ready().await;
    // Bob joins later: the bot commits him into the group.
    host.fake.voice_join(&bob(), CHANNEL_VOICE);
    host.dave_ready().await;

    let (a, b) = (tone::sine(440.0, 50), tone::sine(660.0, 50));
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for (x, y) in a.iter().zip(&b) {
        tick.tick().await;
        for (speaker, frame) in [("tilo-grace", x), ("tilo-linus", y)] {
            host.to_worker
                .send(ToWorker::Audio {
                    speaker: speaker.to_owned(),
                    opus: Bytes(frame.clone()),
                })
                .unwrap();
        }
    }
    let heard = host
        .fake
        .eventually(tilo_test_env::hang_bound(), |f| {
            let heard = f.voice_heard();
            (heard.len() >= 40).then_some(heard)
        })
        .await
        .expect("the fake heard the mix");
    assert!(tone_share(&heard, 440.0) > 0.2, "440 Hz in the mix");
    assert!(tone_share(&heard, 660.0) > 0.2, "660 Hz in the mix");

    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| (f.voice_speaking()
                == [1, 0]
                && f.voice_silence() >= 5)
                .then_some(()))
            .await
            .is_some(),
        "Speaking went on for the audio and off after five silence frames: {:?}, {} silent",
        host.fake.voice_speaking(),
        host.fake.voice_silence()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_muted_user_shows_as_muted() {
    let host = Host::start().await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.fake.voice_mute("7001", true);
    host.calls_until("Ada muted", |calls, _| {
        calls
            .first()
            .is_some_and(|c| c.people.first().is_some_and(|p| p.muted))
    })
    .await;
    host.fake.voice_mute("7001", false);
    host.calls_until("Ada unmuted", |calls, _| {
        calls
            .first()
            .is_some_and(|c| c.people.first().is_some_and(|p| !p.muted))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_connect_and_speak_refuse_with_the_permissions_named() {
    let fake = FakeDiscord::start().await.unwrap();
    // A bot without Administrator, in a channel @everyone may not use.
    fake.set_bot_roles(&[]);
    fake.deny_everyone(CHANNEL_VOICE, CONNECT_AND_SPEAK);
    let host = Host::with(fake).await;
    host.call(Some(CHANNEL_VOICE), None);
    host.calls_until("Refused", |_, link| {
        matches!(link, CallLink::Refused { call, reason }
            if call == CHANNEL_VOICE && reason == "the bot lacks Connect and Speak in #voice")
    })
    .await;
    host.wait("the refusal on the status", |seen| match seen {
        Seen::Status(false, message) if message.contains("lacks Connect and Speak") => Some(()),
        _ => None,
    })
    .await;
    assert!(
        host.fake.voice_bot_channel().is_none(),
        "the bot never tried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_voice_socket_resumes_and_audio_keeps_flowing() {
    let host = Host::start().await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.call(Some(CHANNEL_VOICE), None);
    host.joined().await;
    host.dave_ready().await;
    host.fake.drop_voice();
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| (f.voice_handshakes().1
                >= 1)
                .then_some(()))
            .await
            .is_some(),
        "the worker resumed"
    );
    host.say("tilo-grace", &tone::sine(440.0, 30)).await;
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| (f.voice_heard().len()
                >= 20)
                .then_some(()))
            .await
            .is_some(),
        "audio flows after the resume"
    );
    assert_eq!(
        host.fake.voice_handshakes().0,
        1,
        "one identify, no fresh join"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_dave_audio_is_transport_encrypted_only() {
    let fake = FakeDiscord::start().await.unwrap();
    fake.voice_without_dave();
    let host = Host::with(fake).await;
    host.fake.voice_join(&ada(), CHANNEL_VOICE);
    host.call(Some(CHANNEL_VOICE), None);
    host.joined().await;
    host.say("tilo-grace", &tone::sine(440.0, 30)).await;
    let heard = host
        .fake
        .eventually(tilo_test_env::hang_bound(), |f| {
            let heard = f.voice_heard_frames();
            (heard.len() >= 20).then_some(heard)
        })
        .await
        .expect("heard without DAVE");
    assert!(heard.iter().all(|h| !h.e2ee));
    assert_eq!(host.fake.voice_dave_version(), 0);
}

/// One audio object a device opened: the device that sealed it, the bridged participant it
/// names, and the Opus payload.
type Voice = ([u8; 32], Option<[u8; 32]>, Vec<u8>);

/// A Tilo device in the workspace's call: the live session it hears it on and
/// the board it reads everyone's presence from.
struct Member {
    live: LiveSession,
    board: Board,
    /// Every audio object it opened, as [`Voice`].
    voices: Vec<Voice>,
    sequence: u32,
}

impl Member {
    fn new(relay: &Relay, device: [u8; 32], keys: &ContentKeys, roster: &Roster) -> Self {
        let live = LiveSession::local(
            relay,
            &Join {
                scope: Scope::workspace(BRIDGE_WORKSPACE),
                device,
                keys: keys.clone().into(),
                roster: roster.clone(),
                publish: true,
                presence: true,
            },
        )
        .unwrap();
        let mut board = Board::new(BRIDGE_WORKSPACE, device);
        board.say(&Own {
            label: "Grace".into(),
            device: "laptop".into(),
            ..Own::default()
        });
        Self {
            live,
            board,
            voices: Vec::new(),
            sequence: 0,
        }
    }

    /// Read what the live session heard and say what is due.
    fn pump(&mut self) {
        let now = tilo_clock::unix_now_ms() as f64;
        while let Some(heard) = self.live.try_recv() {
            match heard.track {
                Track::Presence => {
                    self.board.heard(heard.sender, &heard.body, now);
                }
                Track::Audio => {
                    if let Ok(object) = VoiceObject::decode(&heard.body) {
                        self.voices
                            .push((heard.sender, object.speaker, object.payload));
                    }
                }
                _ => {}
            }
        }
        if let Some(frames) = self.board.due(now) {
            let _ = tilo_live::presence::say(&self.live, &frames);
        }
    }

    /// Be in `channel`'s call, or in none.
    fn in_call(&mut self, channel: Option<&str>) {
        self.board.say(&Own {
            label: "Grace".into(),
            device: "laptop".into(),
            call: channel.map(|channel| Call {
                call: CallPresence {
                    channel: channel.into(),
                    name: "voice".into(),
                    bridge: None,
                },
                camera: false,
                screen: false,
                muted: false,
                deafened: false,
            }),
            ..Own::default()
        });
    }

    /// Say one 20 ms frame of `opus` in the call.
    fn speak(&mut self, opus: &[u8]) {
        let object = VoiceObject {
            sequence: u64::from(self.sequence),
            timestamp: u64::from(self.sequence) * u64::from(FRAME_SAMPLES),
            duration: FRAME_SAMPLES,
            speaker: None,
            payload: opus.to_vec(),
        };
        self.live
            .publish(Track::Audio, self.sequence, &object.encode().unwrap())
            .unwrap();
        self.sequence = self.sequence.wrapping_add(1);
    }

    /// The callers in `channel`'s call, as this device's board says.
    fn callers(&self, channel: &str) -> Vec<tilo_live::presence::LiveCaller> {
        self.board
            .calls()
            .into_iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| call.people)
            .collect()
    }

    /// The bridge in `channel`'s call, as this device's call views see it.
    fn bridge(&self, channel: &str) -> Option<tilo_live::presence::LiveCaller> {
        self.callers(channel)
            .into_iter()
            .find(|caller| caller.bridge.is_some())
    }
}

const BRIDGE_WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const TILO_CALL: &str = "voice-tl";

/// The workspace's content keys, as its group hands them to its members.
fn content_keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(BRIDGE_WORKSPACE).unwrap();
    let owner = MemberKey::generate("owner").unwrap();
    KeyGroup::create(workspace, owner)
        .unwrap()
        .content_keys()
        .unwrap()
}

/// The relay the test's local live sessions meet on: in-process, with nobody
/// to admit, since a local live session is its own relay's member.
fn local_relay() -> Relay {
    struct Nobody;
    impl RelayAuthority for Nobody {
        fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
            Box::pin(std::future::ready(Err(Denied)))
        }
    }
    Relay::new(Arc::new(Nobody))
}

/// The bridge's device's word in the workspace: the live session its daemon would hold, held to
/// `rules` and to the media keys of the generation [`TILO_CALL`] names, and the speaker that says
/// the device's presence and carries the call's voice there.
fn bridge_speaker(
    relay: &Relay,
    device: [u8; 32],
    keys: &ContentKeys,
    roster: &Roster,
    rules: BTreeMap<String, CallRule>,
) -> Arc<Speaker> {
    let live = Arc::new(
        LiveSession::local(
            relay,
            &Join {
                scope: Scope::workspace(BRIDGE_WORKSPACE),
                device,
                keys: keys.clone().into(),
                roster: roster.clone(),
                publish: true,
                presence: true,
            },
        )
        .unwrap(),
    );
    live.set_calls(rules, BTreeMap::new());
    live.set_call_keys(TILO_CALL, 0, keys.clone());
    Speaker::start(
        &live,
        BRIDGE_WORKSPACE,
        device,
        Own {
            label: "Bridge".into(),
            device: "home-server".into(),
            ..Own::default()
        },
    )
}

/// The bridge's join, as the device's daemon makes it: the device says it is in the call, or in
/// none, and holds the keys of the call's media it already has. A device nobody is at says
/// nothing outside a call.
fn daemon_join(speaker: &Arc<Speaker>) -> tilo_live::app_call::JoinCall {
    let speaker = Arc::clone(speaker);
    Arc::new(move |call| {
        let Some(call) = call else {
            speaker.set_joined(None);
            return Ok(None);
        };
        let channel = call.channel.clone();
        speaker.set_joined(Some(Call {
            call,
            camera: false,
            screen: false,
            muted: false,
            deafened: false,
        }));
        Ok(Some((channel, 0)))
    })
}

/// Poll `check` until it answers, pumping `grace` and doing `each` every turn.
async fn until<T>(
    grace: &mut Member,
    what: &str,
    mut each: impl FnMut(&mut Member),
    mut check: impl FnMut(&Member) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        grace.pump();
        if let Some(found) = check(grace) {
            return found;
        }
        each(grace);
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} did not happen"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A Discord voice channel and the Tilo voice channel it is mapped to are one
/// call: Dana, in Discord, is a bridged participant of the Tilo call, heard by
/// her own name; what Grace says on Tilo reaches Discord; mute stops the
/// voice; and when both sides empty the bridge leaves both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outside_call_is_carried_into_the_tilo_call_as_a_bridged_person() {
    let host = Host::start().await;
    // The worker joins the voice channel only when the bridge asks it to.
    host.fake
        .voice_join(&User::new("7001", "Dana"), CHANNEL_VOICE);
    host.calls_until("Dana in the Discord channel", |calls, link| {
        *link == CallLink::Idle && calls.iter().any(|call| call.call == CHANNEL_VOICE)
    })
    .await;

    let relay = local_relay();
    let keys = content_keys();
    let grace_device = [9; 32];
    let bridge_device = [7; 32];
    let roster = Roster::new([grace_device, bridge_device]);

    let speaker = bridge_speaker(&relay, bridge_device, &keys, &roster, BTreeMap::new());
    let mut bridge = ConnectorCall::new(
        Arc::clone(&speaker),
        "discord",
        host.lane(),
        daemon_join(&speaker),
    );
    bridge.carry(Some(BridgedCall {
        outside: CHANNEL_VOICE.to_owned(),
        tilo: TILO_CALL.to_owned(),
    }));
    let carrier = tokio::spawn(bridge.run());

    let mut grace = Member::new(&relay, grace_device, &keys, &roster);
    grace.live.set_calls(BTreeMap::new(), BTreeMap::new());
    grace.live.set_call_keys(TILO_CALL, 0, keys.clone());
    grace.in_call(Some(TILO_CALL));

    // The bridge joins the Tilo call as nobody's face, and carries Dana there
    // as a participant of her own, by the name Discord shows.
    let bridge_in_call = until(
        &mut grace,
        "the bridge carries the call",
        |_| {},
        |grace| {
            let caller = grace.bridge(TILO_CALL)?;
            let service = caller.bridge.clone()?;
            Some((caller, service))
        },
    )
    .await;
    assert_eq!(bridge_in_call.1, "discord");
    let dana = until(
        &mut grace,
        "Dana is a bridged participant",
        |_| {},
        |grace| {
            grace
                .callers(TILO_CALL)
                .into_iter()
                .find(|caller| caller.bridged.is_some())
        },
    )
    .await;
    assert_eq!(
        dana.bridged.as_ref().map(|b| b.account.as_str()),
        Some("7001")
    );
    assert_eq!(dana.label, "Dana");
    assert!(
        dana.user.is_none(),
        "a bridged participant is never the bridge's person"
    );
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_bot_channel()
                .is_some_and(|channel| channel == CHANNEL_VOICE)
                .then_some(()))
            .await
            .is_some(),
        "the worker joined the Discord channel"
    );
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_dave_ready()
                .then_some(()))
            .await
            .is_some(),
        "the call is end-to-end encrypted on Discord's side"
    );
    // Tilo asks the bridge for its voice a moment after the bridge joins the
    // call, so a first tone primes the subscription before the one counted.
    let dana_key = collab_types::presence::stand_in(bridge_device, "discord:7001");
    let mut primed = false;
    for _ in 0..100 {
        host.fake.voice_say("7001", &tone::sine(500.0, 1)).await;
        grace.pump();
        if grace
            .voices
            .iter()
            .any(|(_, who, _)| *who == Some(dana_key))
        {
            primed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(primed, "Tilo asked the bridge for its voice");
    let carried_before = grace.voices.len();

    // Dana says 660 Hz. The worker forwards it, and the bridge carries it into
    // Tilo named as Dana's own voice.
    let said = tone::sine(660.0, 50);
    host.fake.voice_say("7001", &said).await;
    assert!(
        host.audio_from("7001").len() >= 40,
        "the worker forwarded {} frames of Dana's voice",
        host.audio_from("7001").len()
    );
    until(
        &mut grace,
        "Dana is heard in Tilo",
        |_| {},
        |grace| {
            (grace.voices[carried_before..]
                .iter()
                .filter(|(_, who, _)| *who == Some(dana_key))
                .count()
                >= 40)
                .then_some(())
        },
    )
    .await;
    let from_dana: Vec<Vec<u8>> = grace.voices[carried_before..]
        .iter()
        .filter(|(_, who, _)| *who == Some(dana_key))
        .map(|(_, _, opus)| opus.clone())
        .collect();
    assert!(
        tone_share(&from_dana, 660.0) > 0.5,
        "660 Hz arrived in Tilo: {}",
        tone_share(&from_dana, 660.0)
    );

    // Grace says 440 Hz: Discord hears it, and nothing Dana said comes back.
    let mut said = tone::sine(440.0, 50).into_iter();
    until(
        &mut grace,
        "Grace is heard in Discord",
        |grace| {
            if let Some(frame) = said.next() {
                grace.speak(&frame);
            }
        },
        |_| (host.fake.voice_heard_frames().len() >= 40).then_some(()),
    )
    .await;
    let heard = host.fake.voice_heard_frames();
    assert!(
        heard.iter().all(|frame| frame.e2ee),
        "every frame came through DAVE"
    );
    assert!(
        tone_share_received(&heard, 440.0) > 0.5,
        "440 Hz arrived in Discord: {}",
        tone_share_received(&heard, 440.0)
    );
    assert!(
        tone_share_received(&heard, 660.0) < 0.05,
        "nothing Dana said comes back to Discord"
    );

    // Muted in Discord, Dana stops being heard on Tilo.
    host.fake.voice_mute("7001", true);
    host.calls_until("Dana muted", |calls, _| {
        calls
            .iter()
            .flat_map(|call| &call.people)
            .any(|person| person.account == "7001" && person.muted)
    })
    .await;
    let before_mute = grace
        .voices
        .iter()
        .filter(|(_, who, _)| *who == Some(dana_key))
        .count();
    let told = tone::sine(880.0, 10);
    host.fake.voice_say("7001", &told).await;
    for _ in 0..15 {
        grace.pump();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        grace
            .voices
            .iter()
            .filter(|(_, who, _)| *who == Some(dana_key))
            .count(),
        before_mute,
        "a muted participant's frames are not carried"
    );

    // Both sides empty: the bridge leaves Discord and no call view shows the
    // call bridged any more.
    host.fake.voice_leave("7001");
    grace.in_call(None);
    until(
        &mut grace,
        "the bridge leaves the Tilo call",
        |_| {},
        |grace| grace.bridge(TILO_CALL).is_none().then_some(()),
    )
    .await;
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_bot_channel()
                .is_none()
                .then_some(()))
            .await
            .is_some(),
        "the bridge left Discord"
    );
    carrier.abort();
}

/// A bridged person the workspace's call denies speaking is not carried: the
/// bridge holds the call's rule, so a denied Discord speaker's frames never
/// reach Tilo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridged_person_the_call_denies_is_not_carried() {
    let host = Host::start().await;
    host.fake
        .voice_join(&User::new("7001", "Dana"), CHANNEL_VOICE);
    let relay = local_relay();
    let keys = content_keys();
    let grace_device = [9; 32];
    let bridge_device = [7; 32];
    let roster = Roster::new([grace_device, bridge_device]);
    let dana_key = collab_types::presence::stand_in(bridge_device, "discord:7001");
    let mut rule = CallRule::default();
    rule.denied.insert(
        hex_of(dana_key),
        tilo_live::calls::Directions {
            publish: true,
            subscribe: false,
        },
    );
    let rules = BTreeMap::from([(TILO_CALL.to_owned(), rule)]);
    let speaker = bridge_speaker(&relay, bridge_device, &keys, &roster, rules.clone());
    let mut bridge = ConnectorCall::new(
        Arc::clone(&speaker),
        "discord",
        host.lane(),
        daemon_join(&speaker),
    );
    bridge.carry(Some(BridgedCall {
        outside: CHANNEL_VOICE.to_owned(),
        tilo: TILO_CALL.to_owned(),
    }));
    let carrier = tokio::spawn(bridge.run());
    let mut grace = Member::new(&relay, grace_device, &keys, &roster);
    grace.live.set_calls(rules, BTreeMap::new());
    grace.live.set_call_keys(TILO_CALL, 0, keys.clone());
    grace.in_call(Some(TILO_CALL));

    until(
        &mut grace,
        "the bridge carries the call",
        |_| {},
        |grace| grace.bridge(TILO_CALL).map(|_| ()),
    )
    .await;
    assert!(
        host.fake
            .eventually(tilo_test_env::hang_bound(), |f| f
                .voice_dave_ready()
                .then_some(()))
            .await
            .is_some(),
        "the call is end-to-end encrypted on Discord's side"
    );
    host.fake.voice_say("7001", &tone::sine(660.0, 50)).await;
    for _ in 0..20 {
        grace.pump();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        grace
            .voices
            .iter()
            .all(|(_, who, _)| *who != Some(dana_key)),
        "a denied participant's voice was carried"
    );
    carrier.abort();
}

/// A key as presence and call rules name it.
fn hex_of(key: [u8; 32]) -> String {
    use std::fmt::Write as _;
    key.iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}
