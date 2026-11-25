#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

//! An app's call lane on the connection it holds: the frames a bridge program speaks carry a
//! person from an outside service into a workspace call as a participant of their own, the
//! workspace's frames go back out, and a lane the workspace did not grant is refused with the
//! reason. The app here speaks through the line wire's client, the one a bun program's SDK
//! reaches the device's daemon with.

use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::presence::CallPresence;
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_actions::remote::call::{
    BridgedCall, CallAudio, CallLink as SaidLink, CallReport as SaidReport, CallStatus,
    OutsideCall, OutsidePerson,
};
use tilo_actions::remote::{ActionCallError, LineClient, RemoteActionFrame, TestProvider};
use tilo_live::app_call::{AppLane, CallSetup, MIXED};
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{Board, Call, Own};
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::speaker::Speaker;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const CALL: &str = "voice-tl";
const OUTSIDE: &str = "458";
const BRIDGE: [u8; 32] = [7; 32];
const GRACE: [u8; 32] = [9; 32];
const ADA: [u8; 32] = [11; 32];
/// The BLAKE3 hex of the picture the app kept for its outside person.
const AVATAR: &str = "4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a4f8a";

fn keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let owner = MemberKey::generate("owner").unwrap();
    KeyGroup::create(workspace, owner)
        .unwrap()
        .content_keys()
        .unwrap()
}

/// The relay the test's local live sessions meet on: in-process, with nobody to admit, since a
/// local live session is its own relay's member.
fn local_relay() -> Relay {
    struct Nobody;
    impl RelayAuthority for Nobody {
        fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
            Box::pin(std::future::ready(Err(Denied)))
        }
    }
    Relay::new(Arc::new(Nobody))
}

fn setup(relay: &Relay, keys: ContentKeys) -> CallSetup {
    // The session the device's daemon holds, as a local relay serves it here, and the device's
    // one word in it: a device with no desktop session says who it is from its certificate.
    let session = LiveSession::local(
        relay,
        &Join {
            scope: Scope::workspace(WORKSPACE.to_owned()),
            device: BRIDGE,
            keys: keys.clone().into(),
            roster: Roster::new([GRACE, ADA, BRIDGE]),
            publish: true,
            presence: true,
        },
    )
    .expect("a local live session");
    session.set_calls(BTreeMap::new(), BTreeMap::new());
    let session = Arc::new(session);
    let speaker = Speaker::start(
        &session,
        WORKSPACE,
        BRIDGE,
        Own {
            label: "home-server".to_owned(),
            device: "home-server".to_owned(),
            user: Some("aa".repeat(32)),
            ..Own::default()
        },
    );
    // Joining is the daemon's: it says the device is in the call and holds the call's keys,
    // which never reach the app.
    let joining = Arc::clone(&speaker);
    let join: tilo_live::app_call::JoinCall = Arc::new(move |call| {
        let Some(call) = call else {
            joining.set_joined(None);
            return Ok(None);
        };
        let channel = call.channel.clone();
        joining.set_joined(Some(Call {
            call,
            camera: false,
            screen: false,
            muted: false,
            deafened: false,
        }));
        joining.session().set_call_keys(&channel, 0, keys.clone());
        Ok(Some((channel, 0)))
    });
    CallSetup {
        service: "discord".to_owned(),
        speaker,
        join,
    }
}

/// The app's side of the connection: the line wire's one client, as the SDKs a bridge program
/// is written against reach the device with it.
fn app(stream: UnixStream) -> LineClient {
    LineClient::over(stream.try_clone().unwrap(), stream).unwrap()
}

/// Serve one app connection over `stream` with a lane and a plain action provider beside it.
/// `written` is the same socket the app's pushes go to, so one lock orders whole lines either way.
fn serve(stream: UnixStream, lane: Arc<AppLane>, written: Arc<Mutex<UnixStream>>) {
    let output = Shared(written);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let provider = Arc::new(TestProvider::new());
        let uploads = tilo_actions::remote::HostedContentPuts::default();
        tilo_actions::remote::serve_lines_with(
            |_: &RemoteActionFrame| {
                Ok((
                    Arc::clone(&provider),
                    tilo_actions::remote::Caller::local_person("tester").in_workspace(WORKSPACE),
                ))
            },
            &uploads,
            None,
            BufReader::new(stream),
            output,
            Duration::from_secs(5),
            Some(lane.as_ref() as &dyn tilo_actions::remote::CallLanes),
        );
    });
}

/// A writer over one socket that several threads write whole lines to.
struct Shared(Arc<Mutex<UnixStream>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

/// One device in the workspace's call: its live session and what its presence says.
struct Member {
    live: LiveSession,
    board: Board,
    voices: Vec<(Option<[u8; 32]>, Vec<u8>)>,
    sequence: u32,
}

impl Member {
    /// `device` in the call, shown as `label`.
    fn new(
        relay: &Relay,
        keys: &ContentKeys,
        roster: &Roster,
        device: [u8; 32],
        label: &str,
    ) -> Self {
        let live = LiveSession::local(
            relay,
            &Join {
                scope: Scope::workspace(WORKSPACE),
                device,
                keys: keys.clone().into(),
                roster: roster.clone(),
                publish: true,
                presence: true,
            },
        )
        .unwrap();
        live.set_calls(BTreeMap::new(), BTreeMap::new());
        live.set_call_keys(CALL, 0, keys.clone());
        let mut board = Board::new(WORKSPACE, device);
        board.say(&Own {
            label: label.into(),
            device: "laptop".into(),
            call: Some(Call {
                call: CallPresence {
                    channel: CALL.into(),
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
        Self {
            live,
            board,
            voices: Vec::new(),
            sequence: 0,
        }
    }

    fn pump(&mut self) {
        let now = tilo_clock::unix_now_ms() as f64;
        while let Some(heard) = self.live.try_recv() {
            match heard.track {
                Track::Presence => {
                    self.board.heard(heard.sender, &heard.body, now);
                }
                Track::Audio => {
                    if let Ok(object) = VoiceObject::decode(&heard.body) {
                        self.voices.push((object.speaker, object.payload.clone()));
                    }
                }
                _ => {}
            }
        }
        if let Some(frames) = self.board.due(now) {
            let _ = tilo_live::presence::say(&self.live, &frames);
        }
    }

    /// Say one 20 ms frame of `opus` in the call. The bridge asks for this device's voice a
    /// moment after its own session joins the call, so a first attempt nobody asks for is not a
    /// failure: it is the call still settling.
    fn speak(&mut self, opus: &[u8]) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let object = VoiceObject {
                sequence: u64::from(self.sequence),
                timestamp: u64::from(self.sequence) * u64::from(FRAME_SAMPLES),
                duration: FRAME_SAMPLES,
                speaker: None,
                payload: opus.to_vec(),
            };
            match self
                .live
                .publish(Track::Audio, self.sequence, &object.encode().unwrap())
            {
                Ok(()) => {
                    self.sequence = self.sequence.wrapping_add(1);
                    return;
                }
                Err(tilo_live::live_session::LiveError::Unwanted) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the call never asked for this device's voice (wanted: {}, callers: {:?})",
                        self.live.voice_wanted(),
                        self.callers()
                    );
                    self.pump();
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(other) => panic!("publishing a voice object failed: {other}"),
            }
        }
    }

    fn callers(&self) -> Vec<tilo_live::presence::LiveCaller> {
        self.board
            .calls()
            .into_iter()
            .filter(|call| call.channel == CALL)
            .flat_map(|call| call.people)
            .collect()
    }
}

/// Pump `grace` until `check` answers, or fail after ten seconds.
fn until<T>(grace: &mut Member, what: &str, mut check: impl FnMut(&Member) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        grace.pump();
        if let Some(found) = check(grace) {
            return found;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The runtime the test's live sessions run their origins on, entered so the test body may
/// block while they do.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn an_apps_connection_carries_an_outside_speaker_into_the_call_and_back() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let relay = local_relay();
    let keys = keys();
    let roster = Roster::new([GRACE, ADA, BRIDGE]);
    let (server, client) = UnixStream::pair().unwrap();
    let written = Arc::new(Mutex::new(server.try_clone().unwrap()));
    let out: tilo_live::app_call::Out = {
        let written = Arc::clone(&written);
        Arc::new(move |line: String| {
            let mut stream = written.lock().unwrap();
            let _ = writeln!(stream, "{line}");
            let _ = stream.flush();
        })
    };
    let lane = AppLane::new(Ok(setup(&relay, keys.clone())), out);
    assert!(lane.granted().is_ok());
    serve(server, lane, written);

    let app = app(client);
    let mut grace = Member::new(&relay, &keys, &roster, GRACE, "Grace");

    // The app asks for the pair, and says who is in its outside call: Dana, by her name and the
    // picture it kept in the workspace's content.
    app.carry_call(Some(BridgedCall {
        outside: OUTSIDE.into(),
        tilo: CALL.into(),
    }))
    .expect("the pair was carried");
    let dana = |muted: bool| SaidReport {
        calls: vec![OutsideCall {
            call: OUTSIDE.into(),
            people: vec![OutsidePerson {
                account: "7001".into(),
                name: "Dana".into(),
                avatar_url: Some("https://cdn.discordapp.com/avatars/7001/9f3.png".into()),
                avatar: Some(AVATAR.into()),
                muted,
                deafened: false,
            }],
        }],
        link: SaidLink::Joined {
            call: OUTSIDE.into(),
        },
        status: String::new(),
    };
    app.report_call(dana(false)).expect("the report was taken");

    // Dana and Grace are each in their call: the device asks the app into the outside call,
    // saying who is here once it sees Grace.
    let asked = until(&mut grace, "the app asked into the outside call", |_| {
        app.asked_call()
            .filter(|asked| asked.status.as_deref() == Some("On Tilo: Grace"))
    });
    assert_eq!(
        asked,
        CallStatus {
            call: Some(OUTSIDE.into()),
            status: Some("On Tilo: Grace".into()),
        }
    );

    // Dana is a participant of the Tilo call of her own, by the name and picture her service
    // shows.
    let shown = until(&mut grace, "Dana is a bridged participant", |grace| {
        grace
            .callers()
            .into_iter()
            .find(|caller| caller.bridged.is_some())
    });
    let bridged = shown.bridged.as_ref().unwrap();
    assert_eq!(bridged.account, "7001");
    assert_eq!(bridged.avatar.as_deref(), Some(AVATAR));
    assert_eq!(shown.label, "Dana");
    assert!(!shown.muted);

    // What she says reaches the call, named as her own voice. Her service sends a frame every
    // 20 ms; the call asks for the bridge's voice a moment after it joins, and the frames before
    // that have nobody to go to.
    let heard = until(&mut grace, "Dana is heard", |grace| {
        app.hear_call(CallAudio {
            speaker: "7001".into(),
            opus: vec![1, 2, 3],
            skipped: 0,
        })
        .unwrap();
        grace
            .voices
            .iter()
            .find(|(_, opus)| opus == &[1, 2, 3])
            .cloned()
    });
    let dana_key = collab_types::presence::stand_in(BRIDGE, "discord:7001");
    assert_eq!(heard.0, Some(dana_key), "the voice is Dana's own");

    // What Grace says comes back to the app untouched, as hers: the device waits for her second
    // frame before playing the first, so a late frame is waited for, not a gap.
    grace.speak(&[7, 8, 9]);
    grace.speak(&[7, 8, 9]);
    let said = until(&mut grace, "the workspace's frame reached the app", |_| {
        app.said_call()
            .into_iter()
            .find(|frame| frame.opus == [7, 8, 9])
    });
    assert_eq!(said.speaker, hex::encode(GRACE));

    // Dana mutes herself there: the call shows her muted, and nothing she sends is carried.
    app.report_call(dana(true)).expect("the report was taken");
    until(&mut grace, "Dana is shown muted", |grace| {
        grace
            .callers()
            .into_iter()
            .find(|caller| caller.bridged.is_some() && caller.muted)
    });
    for _ in 0..10 {
        app.hear_call(CallAudio {
            speaker: "7001".into(),
            opus: vec![4, 4],
            skipped: 0,
        })
        .unwrap();
        grace.pump();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !grace.voices.iter().any(|(_, opus)| opus == &[4, 4]),
        "a muted person is not heard"
    );

    // Letting go ends the bridge's place in the call, and the people it carried leave it.
    app.carry_call(None).expect("the call was let go");
    until(&mut grace, "the bridged participants left", |grace| {
        (!grace
            .callers()
            .iter()
            .any(|caller| caller.bridged.is_some() || caller.bridge.is_some()))
        .then_some(())
    });
}

#[test]
fn an_app_without_the_grant_is_refused_its_call() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let (server, client) = UnixStream::pair().unwrap();
    let lane = AppLane::new(
        Err("this workspace grants the bridge no call".to_owned()),
        Arc::new(|_| {}),
    );
    assert_eq!(
        lane.granted(),
        Err("this workspace grants the bridge no call".to_owned())
    );
    let written = Arc::new(Mutex::new(server.try_clone().unwrap()));
    serve(server, lane, written);
    let app = app(client);
    let refused = app.carry_call(Some(BridgedCall {
        outside: OUTSIDE.into(),
        tilo: CALL.into(),
    }));
    assert_eq!(
        refused,
        Err(ActionCallError::Refused {
            reason: "this workspace grants the bridge no call".into()
        })
    );
    // A spoken frame is not a request: nothing is answered, and nothing is carried.
    app.hear_call(CallAudio {
        speaker: "7001".into(),
        opus: vec![1],
        skipped: 0,
    })
    .unwrap();
    app.report_call(SaidReport::default())
        .expect("a report is still taken");
    assert!(
        app.said_call().is_empty(),
        "nothing of a call reaches the app"
    );
}

#[test]
fn a_pair_for_another_call_than_the_one_admitted_is_refused() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let relay = local_relay();
    let keys = keys();
    let (server, client) = UnixStream::pair().unwrap();
    let lane = AppLane::new(Ok(setup(&relay, keys)), Arc::new(|_| {}));
    let written = Arc::new(Mutex::new(server.try_clone().unwrap()));
    serve(server, lane, written);
    let app = app(client);
    // The app's first carry joins its call: that is the call it was admitted to.
    app.carry_call(Some(BridgedCall {
        outside: OUTSIDE.into(),
        tilo: CALL.into(),
    }))
    .expect("the first pair was carried");
    let refused = app.carry_call(Some(BridgedCall {
        outside: OUTSIDE.into(),
        tilo: "another-call".into(),
    }));
    let Err(ActionCallError::Refused { reason }) = refused else {
        panic!("the call was refused: {refused:?}");
    };
    assert!(
        reason.contains(CALL) && reason.contains("another-call"),
        "the refusal names both calls: {reason}"
    );
}

/// A 20 ms Opus frame of a `hz` tone, the `frame`th of it, as a microphone says it.
fn tone(hz: f32, frame: usize) -> Vec<u8> {
    let mut encoder =
        opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio).unwrap();
    let samples = FRAME_SAMPLES as usize;
    let pcm: Vec<i16> = (0..samples)
        .map(|i| {
            let t = (frame * samples + i) as f32 / 48_000.0;
            (f32::sin(t * hz * std::f32::consts::TAU) * 8000.0) as i16
        })
        .collect();
    encoder.encode_vec(&pcm, 1275).unwrap()
}

#[test]
fn two_people_speaking_at_once_reach_the_app_as_one_mixed_stream() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let relay = local_relay();
    let keys = keys();
    let roster = Roster::new([GRACE, ADA, BRIDGE]);
    let (server, client) = UnixStream::pair().unwrap();
    let written = Arc::new(Mutex::new(server.try_clone().unwrap()));
    let out: tilo_live::app_call::Out = {
        let written = Arc::clone(&written);
        Arc::new(move |line: String| {
            let mut stream = written.lock().unwrap();
            let _ = writeln!(stream, "{line}");
            let _ = stream.flush();
        })
    };
    let lane = AppLane::new(Ok(setup(&relay, keys.clone())), out);
    serve(server, lane, written);
    let app = app(client);
    let mut grace = Member::new(&relay, &keys, &roster, GRACE, "Grace");
    let mut ada = Member::new(&relay, &keys, &roster, ADA, "Ada");
    app.carry_call(Some(BridgedCall {
        outside: OUTSIDE.into(),
        tilo: CALL.into(),
    }))
    .expect("the pair was carried");
    app.report_call(SaidReport {
        calls: vec![OutsideCall {
            call: OUTSIDE.into(),
            people: Vec::new(),
        }],
        link: SaidLink::Joined {
            call: OUTSIDE.into(),
        },
        status: String::new(),
    })
    .expect("the report was taken");

    // Both are in the call once the device says so to the app.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        grace.pump();
        ada.pump();
        let status = app
            .asked_call()
            .and_then(|asked| asked.status)
            .unwrap_or_default();
        if status.contains("Grace") && status.contains("Ada") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the device never saw both in the call"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Grace and Ada talk at once: the app is handed one stream that is both of them, a frame
    // every 20 ms, never their frames one after the other.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut said = Vec::new();
    let mut frame = 0;
    let mixed = loop {
        grace.speak(&tone(440.0, frame));
        ada.speak(&tone(660.0, frame));
        frame += 1;
        grace.pump();
        ada.pump();
        said.extend(app.said_call());
        if let Some(mixed) = said.iter().find(|said| said.speaker == MIXED) {
            break mixed.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no mixed frame reached the app; it was handed {:?}",
            said.iter().map(|said| &said.speaker).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        said.iter().all(|said| said.speaker == MIXED
            || said.speaker == hex::encode(GRACE)
            || said.speaker == hex::encode(ADA)),
        "every frame is one of theirs or both"
    );
    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
    let mut pcm = vec![0_i16; FRAME_SAMPLES as usize * 2];
    assert_eq!(
        decoder.decode(&mixed.opus, &mut pcm, false).unwrap(),
        FRAME_SAMPLES as usize,
        "the mix is one 20 ms frame"
    );
}
