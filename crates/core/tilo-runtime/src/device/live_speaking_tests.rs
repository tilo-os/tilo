use std::sync::{Arc, Mutex};

use tilo_actions::remote::call::{
    BridgedCall, CallAudio, CallLink, CallReport, OutsideCall, OutsidePerson,
};
use tilo_actions::remote::{CallLanes as _, RemoteActionMessage, Take};
use tilo_desktop_model::WorkspaceId;
use tilo_live::live::Track;
use tilo_live::live_session::LiveError;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};

use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;

const GRANTED_JOIN_TEST: &str = "device::live_speaking::tests::an_app_granted_the_call_joins_it_with_no_window_and_carries_audio_without_a_key";
const SHELL_ENDED_TEST: &str =
    "device::live_speaking::tests::a_shell_that_ends_leaves_the_person_out_of_the_call";

/// The app that carries the call, the call it carries, and the outside one it carries it from.
const APP: &str = "so.tilo.discord";
const CALL: &str = "voice-tl";
const OUTSIDE: &str = "458";
/// The outside person the app carries in.
const DANA: &str = "7001";

/// What `take`, one answer of the lane, says of the carry or report it answers: `Ok` or the
/// refusal.
fn result_of(take: Take) -> serde_json::Value {
    let Take::Answer(line) = take else {
        panic!("the lane answered nothing");
    };
    let line: serde_json::Value = serde_json::from_str(&line).expect("an answer is a line of JSON");
    line.pointer("/message/CallCarryResponse/result")
        .or_else(|| line.pointer("/message/CallReportResponse/result"))
        .cloned()
        .unwrap_or_else(|| panic!("no call answer: {line}"))
}

fn carry(correlation: u64) -> RemoteActionMessage {
    RemoteActionMessage::CallCarryRequest {
        correlation,
        pair: Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        }),
    }
}

/// Wait until `check` holds, or fail saying `what` did not happen.
fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !check() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}

/// A device's daemon with nobody signed in at it, no desktop and no window: it takes part in its
/// workspace's live session itself, and an app it runs reaches the workspace's call through it,
/// by the typed join of the device's live sessions, decided as the app acting by itself. Before
/// the workspace grants the app anything there, its join is refused and the device is in no
/// call; once a grant names the app on the device's live sessions, the same carry joins the call,
/// the device holds the call's media keys and may speak in it, and the outside person the app
/// reports is carried in as a participant of their own. No line the app is handed carries a key.
#[test]
fn an_app_granted_the_call_joins_it_with_no_window_and_carries_audio_without_a_key() {
    let Some(root) = isolated_root(GRANTED_JOIN_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let id = WorkspaceId::parse(&workspace).expect("a workspace id");
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let session = loop {
        if let Some(session) = live.call_session(id) {
            break session;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon took no part in its workspace's live session"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    assert_eq!(
        session.speaker().call(),
        None,
        "the device is in no call yet"
    );

    let _entered = tilo_rt::app().handle().enter();
    let at = crate::seats::AppAt {
        app: APP.to_owned(),
        workspace: workspace.clone(),
        pid: std::process::id(),
    };
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let out = {
        let lines = Arc::clone(&lines);
        Arc::new(move |line: String| lines.lock().unwrap().push(line)) as tilo_live::app_call::Out
    };
    let lane = tilo_live::app_call::AppLane::new(daemon.app_call_setup(&at), out);
    assert_eq!(
        lane.granted(),
        Ok(()),
        "the app's workspace has a live session here"
    );

    // Granted nothing, the app is refused the join, and nothing of the call is the device's.
    let refused = result_of(lane.take(&carry(1)));
    assert_eq!(
        refused
            .pointer("/Err/Refused/reason")
            .and_then(serde_json::Value::as_str),
        Some("the caller may not invoke this action"),
        "{refused}"
    );
    assert_eq!(
        session.speaker().call(),
        None,
        "a refused app joins nothing"
    );
    assert_eq!(
        session.session().call_keys_wanted(),
        None,
        "the device is held to no call's media"
    );

    // The same over the app's own connection, as the daemon serves the process it started and
    // the app's SDK reaches it: the carry is refused with the reason, and the device joins
    // nothing.
    let app = app_connection(&daemon, &at);
    assert_eq!(
        app.carry_call(Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        })),
        Err(tilo_actions::remote::ActionCallError::Refused {
            reason: "the caller may not invoke this action".into()
        })
    );
    app.report_call(CallReport::default())
        .expect("a report is taken from an app the device carries no call for");
    assert!(app.said_call().is_empty());
    assert_eq!(app.asked_call(), None);
    assert_eq!(
        session.speaker().call(),
        None,
        "a refused app joins nothing"
    );
    drop(app);

    // The person grants the app the device's live sessions: the grant names the app, never
    // them, so only the app acting by itself holds it.
    let person = hex::encode(live.person());
    let grant = tilo_action_policy::Grant::new(
        "discord-joins-calls",
        tilo_action_policy::Grantee::app(APP),
        tilo_action_policy::TreePath::workspace(&workspace).app(super::PROVIDER),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    );
    tilo_rt::app()
        .block_on(live.workspaces().copies().commit_own_change(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(grant),
            },
            None,
        ))
        .expect("the runtime runs the change")
        .expect("the owner's grant is committed");

    let joined = result_of(lane.take(&carry(2)));
    assert_eq!(joined, serde_json::json!({ "Ok": null }), "{joined}");
    assert_eq!(
        session.speaker().call().map(|call| call.call.channel),
        Some(CALL.to_owned()),
        "the device is in the call the app carries"
    );
    let (wanted, _generation) = session
        .session()
        .call_keys_wanted()
        .expect("the device holds the call's media");
    assert_eq!(wanted, CALL);

    // The app is in the outside call with Dana in it: the bridge carries her into the call as
    // a participant of her own, and what she says is sealed under the call's keys the daemon
    // holds. Nobody here asks for the device's voice yet, so the frame is unwanted rather than
    // withheld: the device may speak in the call and seals for it.
    let reported = result_of(lane.take(&RemoteActionMessage::CallReport {
        correlation: 3,
        report: CallReport {
            calls: vec![OutsideCall {
                call: OUTSIDE.into(),
                people: vec![OutsidePerson {
                    account: DANA.into(),
                    name: "Dana".into(),
                    ..OutsidePerson::default()
                }],
            }],
            link: CallLink::Joined {
                call: OUTSIDE.into(),
            },
            status: String::new(),
        },
    }));
    assert_eq!(reported, serde_json::json!({ "Ok": null }), "{reported}");
    until("Dana carried into the call", || {
        session
            .speaker()
            .bridged()
            .iter()
            .any(|member| member.bridged.account == DANA && member.call.call.channel == CALL)
    });
    let frame = vec![0xF8, 0xFF, 0xFE];
    assert!(matches!(
        lane.take(&RemoteActionMessage::CallHeard(CallAudio {
            speaker: DANA.into(),
            opus: frame.clone(),
            skipped: 0,
        })),
        Take::Silent
    ));
    let voice = VoiceObject {
        sequence: 0,
        timestamp: 0,
        duration: FRAME_SAMPLES,
        speaker: None,
        payload: frame,
    };
    assert!(
        matches!(
            session
                .session()
                .publish(Track::Audio, 0, &voice.encode().unwrap()),
            Err(LiveError::Unwanted)
        ),
        "the device speaks in the call with the keys it holds"
    );

    // No line the app was handed carries a key.
    until("the bridge saying its place to the app", || {
        !lines.lock().unwrap().is_empty()
    });
    for line in lines.lock().unwrap().iter() {
        assert!(!line.contains("key"), "{line}");
    }
}

/// The connection the daemon serves an app it started at `at`, as the line wire's client the
/// app's SDK holds.
fn app_connection(
    daemon: &Arc<Daemon>,
    at: &crate::seats::AppAt,
) -> tilo_actions::remote::LineClient {
    use std::io::{BufRead as _, Read as _};
    let (served, held) = std::os::unix::net::UnixStream::pair().unwrap();
    let frames = crate::device::local::Frames::of_an_app(daemon, at);
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(served.try_clone().unwrap());
        let mut first = Vec::new();
        (&mut reader)
            .take(64 * 1024)
            .read_until(b'\n', &mut first)
            .unwrap();
        let line = first.strip_suffix(b"\n").unwrap_or(&first).to_vec();
        frames.serve(served, &line, reader);
    });
    tilo_actions::remote::LineClient::of_stream(held).unwrap()
}

/// The person's shell ends the way a desktop quits: the word it said and the call it was in
/// stop being said in the workspace, and the device says only what it is there. A person gone
/// from their computer is in no call, as their roster row on everyone else's device shows.
#[test]
fn a_shell_that_ends_leaves_the_person_out_of_the_call() {
    use collab_types::presence::CallPresence;
    use tilo_live::presence::Call;
    use tilo_workspace_service::apps::shell::ShellSpeaking as _;

    let Some(root) = isolated_root(SHELL_ENDED_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let id = WorkspaceId::parse(&workspace).expect("a workspace id");
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let session = loop {
        if let Some(session) = live.call_session(id) {
            break session;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon took no part in its workspace's live session"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    let _entered = tilo_rt::app().handle().enter();

    live.say(
        &workspace,
        Some(runtime_collab::wire::shell::ShellWord {
            label: "Ada".into(),
            device: "laptop".into(),
            status: Some(("Writing the brief".into(), false)),
            ..runtime_collab::wire::shell::ShellWord::default()
        }),
        Vec::new(),
    )
    .expect("the daemon takes the person's word");
    session.speaker().set_joined(Some(Call {
        call: CallPresence {
            channel: CALL.into(),
            name: "voice".into(),
            bridge: None,
        },
        camera: false,
        screen: false,
        muted: false,
        deafened: false,
    }));
    assert!(
        session.speaker().word().status.is_some(),
        "the person says what they are doing while their shell runs"
    );
    assert!(
        session.speaker().call().is_some(),
        "the person is in the call while their shell runs"
    );

    live.gone();
    loop {
        let word = session.speaker().word();
        if word.status.is_none() && session.speaker().call().is_none() {
            assert!(
                !word.label.is_empty(),
                "the device still says what it is after the shell ends"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the word or the call outlived the shell"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}
