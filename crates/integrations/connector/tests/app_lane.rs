#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

//! An app's call lane on the connection it holds: the frames a bridge program speaks carry a
//! person from an outside service into a workspace call as a participant of their own, the
//! workspace's frames go back out, and a lane the workspace did not grant is refused with the
//! reason. The app here is a test process speaking the same frames a bun program does.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::presence::CallPresence;
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_actions::remote::call::{
    BridgedCall, CallAudio, CallLink as SaidLink, CallReport as SaidReport,
};
use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage, TestProvider, frame_line};
use tilo_connector::app_lane::{AppLane, CallSetup};
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{Board, Call, Own};
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const CALL: &str = "voice-tl";
const OUTSIDE: &str = "458";
const BRIDGE: [u8; 32] = [7; 32];
const GRACE: [u8; 32] = [9; 32];

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

fn setup(relay: Relay, keys: ContentKeys) -> CallSetup {
    CallSetup {
        relay,
        workspace: WORKSPACE.to_owned(),
        device: BRIDGE,
        device_name: "home-server".to_owned(),
        person: Some("aa".repeat(32)),
        service: "discord".to_owned(),
        roster: Roster::new([GRACE, BRIDGE]),
        keys: keys.clone().into(),
        rules: BTreeMap::new(),
        persons: BTreeMap::new(),
        call_keys: Some((CALL.to_owned(), 0, keys.into())),
    }
}

/// The app's side of the connection: frames out, and every line the device writes in.
struct App {
    writer: Mutex<UnixStream>,
    lines: Arc<Mutex<Vec<serde_json::Value>>>,
    next: AtomicU64,
}

impl App {
    fn of(stream: UnixStream) -> Self {
        let reader = BufReader::new(stream.try_clone().unwrap());
        let lines: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let held = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { return };
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    held.lock().unwrap().push(value);
                }
            }
        });
        Self {
            writer: Mutex::new(stream),
            lines,
            next: AtomicU64::new(1),
        }
    }

    /// Ask `message`, answering with its correlation, and wait for the answer.
    fn ask(&self, build: impl FnOnce(u64) -> RemoteActionMessage) -> serde_json::Value {
        let correlation = self.next.fetch_add(1, Ordering::Relaxed);
        self.send(build(correlation));
        self.until(|line| {
            line.get("message")
                .and_then(|m| {
                    m.get("CallCarryResponse")
                        .or_else(|| m.get("CallReportResponse"))
                })
                .and_then(|answer| answer.get("correlation"))
                .and_then(serde_json::Value::as_u64)
                == Some(correlation)
        })
    }

    fn send(&self, message: RemoteActionMessage) {
        let line = frame_line(message);
        let mut writer = self.writer.lock().unwrap();
        writeln!(writer, "{line}").unwrap();
        writer.flush().unwrap();
    }

    /// Wait for a line this app's connection was written, past the ones already read.
    fn until(&self, mut found: impl FnMut(&serde_json::Value) -> bool) -> serde_json::Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut seen = 0;
        loop {
            {
                let lines = self.lines.lock().unwrap();
                while seen < lines.len() {
                    if found(&lines[seen]) {
                        return lines[seen].clone();
                    }
                    seen += 1;
                }
            }
            assert!(std::time::Instant::now() < deadline, "no such line arrived");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether any line this app read is the device's push of `said`.
    fn said(&self, opus: &[u8]) -> bool {
        self.lines.lock().unwrap().iter().any(|line| {
            line.pointer("/message/CallSaid/opus")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|bytes| {
                    bytes
                        .iter()
                        .filter_map(serde_json::Value::as_u64)
                        .map(|b| u8::try_from(b).unwrap_or_default())
                        .eq(opus.iter().copied())
                })
        })
    }
}

/// Serve one app connection over `stream` with a lane and a plain action provider beside it.
fn serve(stream: UnixStream, lane: Arc<AppLane>) {
    let output = Shared(Arc::new(Mutex::new(stream.try_clone().unwrap())));
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
    fn new(relay: &Relay, keys: &ContentKeys, roster: &Roster) -> Self {
        let live = LiveSession::local(
            relay,
            &Join {
                scope: Scope::workspace(WORKSPACE),
                device: GRACE,
                keys: keys.clone().into(),
                roster: roster.clone(),
                publish: true,
                presence: true,
            },
        )
        .unwrap();
        live.set_calls(BTreeMap::new(), BTreeMap::new());
        live.set_call_keys(CALL, 0, keys.clone());
        let mut board = Board::new(WORKSPACE, GRACE);
        board.say(&Own {
            label: "Grace".into(),
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
    let roster = Roster::new([GRACE, BRIDGE]);
    let (server, client) = UnixStream::pair().unwrap();
    let written = Arc::new(Mutex::new(server.try_clone().unwrap()));
    let out: tilo_connector::app_lane::Out = {
        let written = Arc::clone(&written);
        Arc::new(move |line: String| {
            let mut stream = written.lock().unwrap();
            let _ = writeln!(stream, "{line}");
            let _ = stream.flush();
        })
    };
    let lane = AppLane::new(Ok(setup(relay.clone(), keys.clone())), out);
    assert!(lane.granted().is_ok());
    serve(server, lane);

    let app = App::of(client);
    let mut grace = Member::new(&relay, &keys, &roster);

    // The app asks for the pair, and says who is in its outside call.
    let answer = app.ask(|correlation| RemoteActionMessage::CallCarryRequest {
        correlation,
        pair: Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        }),
    });
    assert!(
        answer
            .pointer("/message/CallCarryResponse/result/Ok")
            .is_some(),
        "the pair was carried: {answer}"
    );
    let answer = app.ask(|correlation| RemoteActionMessage::CallReport {
        correlation,
        report: SaidReport {
            calls: vec![tilo_actions::remote::call::OutsideCall {
                call: OUTSIDE.into(),
                people: vec![tilo_actions::remote::call::OutsidePerson {
                    account: "7001".into(),
                    name: "Dana".into(),
                    muted: false,
                    deafened: true,
                    ..Default::default()
                }],
            }],
            link: SaidLink::Joined {
                call: OUTSIDE.into(),
            },
            status: String::new(),
        },
    });
    assert!(
        answer
            .pointer("/message/CallReportResponse/result/Ok")
            .is_some(),
        "the report was taken: {answer}"
    );

    // Dana is a participant of the Tilo call of her own, by the name her service shows,
    // deafened as her own service says.
    let dana = until(&mut grace, "Dana is a bridged participant", |grace| {
        grace
            .callers()
            .into_iter()
            .find(|caller| caller.bridged.is_some())
    });
    assert_eq!(
        dana.bridged
            .as_ref()
            .map(|bridged| bridged.account.as_str()),
        Some("7001")
    );
    assert_eq!(dana.label, "Dana");
    assert!(dana.deafened, "her service says her ear is off too");

    // One frame she says reaches the call, named as her own voice.
    app.send(RemoteActionMessage::CallHeard(CallAudio {
        speaker: "7001".into(),
        opus: vec![1, 2, 3],
    }));
    let heard = until(&mut grace, "Dana is heard", |grace| {
        grace
            .voices
            .iter()
            .find(|(_, opus)| opus == &[1, 2, 3])
            .cloned()
    });
    let dana_key = collab_types::presence::stand_in(BRIDGE, "discord:7001");
    assert_eq!(heard.0, Some(dana_key), "the voice is Dana's own");

    // What Grace says comes back to the app as one frame, untouched.
    grace.speak(&[7, 8, 9]);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !app.said(&[7, 8, 9]) {
        grace.pump();
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace's frame reached the app"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Letting go ends the bridge's place in the call.
    let answer = app.ask(|correlation| RemoteActionMessage::CallCarryRequest {
        correlation,
        pair: None,
    });
    assert!(
        answer
            .pointer("/message/CallCarryResponse/result/Ok")
            .is_some()
    );
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
    serve(server, lane);
    let app = App::of(client);
    let answer = app.ask(|correlation| RemoteActionMessage::CallCarryRequest {
        correlation,
        pair: Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        }),
    });
    let refusal = answer
        .pointer("/message/CallCarryResponse/result/Err")
        .expect("the call was refused");
    assert_eq!(
        refusal["Refused"]["reason"],
        "this workspace grants the bridge no call"
    );
    // A spoken frame is not a request: nothing is answered, and nothing is carried.
    app.send(RemoteActionMessage::CallHeard(CallAudio {
        speaker: "7001".into(),
        opus: vec![1],
    }));
    let answer = app.ask(|correlation| RemoteActionMessage::CallReport {
        correlation,
        report: SaidReport::default(),
    });
    assert!(
        answer
            .pointer("/message/CallReportResponse/result/Ok")
            .is_some(),
        "a report is still taken: {answer}"
    );
}

#[test]
fn a_pair_for_another_call_than_the_one_admitted_is_refused() {
    let runtime = runtime();
    let _entered = runtime.enter();
    let relay = local_relay();
    let keys = keys();
    let (server, client) = UnixStream::pair().unwrap();
    let lane = AppLane::new(Ok(setup(relay, keys)), Arc::new(|_| {}));
    serve(server, lane);
    let app = App::of(client);
    let answer = app.ask(|correlation| RemoteActionMessage::CallCarryRequest {
        correlation,
        pair: Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: "another-call".into(),
        }),
    });
    let refusal = answer
        .pointer("/message/CallCarryResponse/result/Err")
        .expect("the call was refused");
    assert!(
        refusal["Refused"]["reason"]
            .as_str()
            .is_some_and(|why| why.contains(CALL) && why.contains("another-call")),
        "the refusal names both calls: {refusal}"
    );
}
