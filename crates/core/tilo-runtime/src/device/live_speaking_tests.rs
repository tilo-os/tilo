use std::sync::{Arc, Mutex};

use tilo_actions::remote::call::BridgedCall;
use tilo_actions::remote::{CallLanes as _, RemoteActionMessage, Take};
use tilo_desktop_model::WorkspaceId;

use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;

const NO_WINDOW_TEST: &str = "device::live_speaking::tests::an_app_with_no_window_reaches_the_daemons_call_and_is_refused_what_it_is_not_granted";

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
}
