#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

//! The worker's voice side against the fake Discord, through the connector
//! protocol: occupancy, joining and leaving, audio both ways with DAVE, no
//! echo, mute and speaking, and refusals that name what is missing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fake_discord::{CHANNEL_VOICE, CONNECT_AND_SPEAK, FakeDiscord, TOKEN, User, tone};
use tilo_connector::framing::{read_frame_async, write_frame_async};
use tilo_connector::protocol::{Bytes, CallLink, FromWorker, OutsideCall, ToWorker};
use tokio::sync::mpsc;

/// What the worker said that these tests look at.
#[derive(Clone, Debug)]
enum Seen {
    Calls(Vec<OutsideCall>, CallLink),
    Audio(String, Vec<u8>),
    Status(bool, String),
}

struct Host {
    fake: FakeDiscord,
    to_worker: mpsc::UnboundedSender<ToWorker>,
    seen: Arc<Mutex<Vec<Seen>>>,
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
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let (seen, to_worker) = (seen.clone(), to_worker.clone());
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
                        FromWorker::Calls { calls, link } => Seen::Calls(calls, link),
                        FromWorker::Audio { speaker, opus } => Seen::Audio(speaker, opus.0.clone()),
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
        let deadline = tokio::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            if let Some(found) = check() {
                return found;
            }
            assert!(tokio::time::Instant::now() < deadline, "never saw {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
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

    /// Send `frames` as `speaker` in real time.
    async fn say(&self, speaker: &str, frames: &[Vec<u8>]) {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
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
fn tone_share(frames: &[Vec<u8>], hz: f32) -> f32 {
    let samples = tone::decode(frames);
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
    let opus: Vec<Vec<u8>> = heard.into_iter().map(|h| h.opus).collect();
    assert!(
        tone_share(&opus, 440.0) > 0.5,
        "440 Hz arrived: {}",
        tone_share(&opus, 440.0)
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
