use std::sync::{Arc, Mutex};

use tilo_actions::remote::call::BridgedCall;
use tilo_actions::remote::{CallLanes as _, RemoteActionMessage, Take};
use tilo_desktop_model::WorkspaceId;

use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;

const NO_WINDOW_TEST: &str = "device::live_speaking::tests::an_app_with_no_window_reaches_the_daemons_call_and_is_refused_what_it_is_not_granted";
const SHELL_ENDED_TEST: &str =
    "device::live_speaking::tests::a_shell_that_ends_leaves_the_person_out_of_the_call";

/// The call the app carries, and the outside one it carries it from.
const CALL: &str = "voice-tl";
const OUTSIDE: &str = "458";

/// What `take`, one answer of the lane, says of the carry it answers: `Ok` or the refusal.
fn result_of(take: Take) -> serde_json::Value {
    let Take::Answer(line) = take else {
        panic!("the lane answered nothing");
    };
    let line: serde_json::Value = serde_json::from_str(&line).expect("an answer is a line of JSON");
    line.pointer("/message/CallCarryResponse/result")
        .cloned()
        .unwrap_or_else(|| panic!("no call answer: {line}"))
}

/// A device's daemon with nobody signed in at it, no desktop and no window: it takes part in its
/// workspace's live session itself, and an app it runs reaches the workspace's call through it.
/// The app's join goes through the device's one dispatch, so an app the workspace grants nothing
/// of the device's calls is refused it, and the device says it is in no call.
#[test]
fn an_app_with_no_window_reaches_the_daemons_call_and_is_refused_what_it_is_not_granted() {
    let Some(root) = isolated_root(NO_WINDOW_TEST) else {
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
        app: "so.tilo.discord".to_owned(),
        workspace,
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

    let refused = result_of(lane.take(&RemoteActionMessage::CallCarryRequest {
        correlation: 1,
        pair: Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        }),
    }));
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

    // No line the app was handed carries a key.
    for line in lines.lock().unwrap().iter() {
        assert!(!line.contains("key"), "{line}");
    }

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
    app.report_call(tilo_actions::remote::call::CallReport::default())
        .expect("a report is taken from an app the device carries no call for");
    assert!(app.said_call().is_empty());
    assert_eq!(app.asked_call(), None);
    assert_eq!(
        session.speaker().call(),
        None,
        "a refused app joins nothing"
    );
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
