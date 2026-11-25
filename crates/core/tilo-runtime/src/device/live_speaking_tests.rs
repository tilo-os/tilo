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
const ONCE_JOIN_TEST: &str = "device::live_speaking::tests::a_join_given_once_joins_the_call_once";
const ASKED_JOIN_TEST: &str =
    "device::live_speaking::tests::an_app_with_no_grant_asks_its_person_once_to_join_calls";
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
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("the hang bound fits the clock");
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
    assert!(
        session.speaker().word().is_none(),
        "a device nobody is at says no word in its workspace, so nobody lists it as here"
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
        Some(super::join_asked_first(APP).as_str()),
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
    the_apps_connection_joins_nothing(&daemon, &at, &session);

    // The person grants the app the device's live sessions: the grant names the app, never
    // them, so only the app acting by itself holds it.
    grant_the_app(&live, &workspace, "discord-joins-calls");

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

    dana_is_carried_into_the_call(&lane, &session);

    // No line the app was handed carries a key.
    until("the bridge saying its place to the app", || {
        !lines.lock().unwrap().is_empty()
    });
    for line in lines.lock().unwrap().iter() {
        assert!(!line.contains("key"), "{line}");
    }
}

/// A join given once ([`tilo_action_policy::Lifetime::Once`]) admits the app's join once: the
/// device's dispatch spends the grant as it admits the call, so of two joins at the same moment
/// only one is admitted, and the app's next join is decided afresh and refused.
#[test]
fn a_join_given_once_joins_the_call_once() {
    let Some(root) = isolated_root(ONCE_JOIN_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let id = WorkspaceId::parse(&workspace).expect("a workspace id");
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    until(
        "the daemon taking part in its workspace's live session",
        || live.call_session(id).is_some(),
    );
    let session = live.call_session(id).unwrap();
    let _entered = tilo_rt::app().handle().enter();
    let at = crate::seats::AppAt {
        app: APP.to_owned(),
        workspace: workspace.clone(),
        pid: std::process::id(),
    };
    let lane = || {
        let out = Arc::new(|_: String| {}) as tilo_live::app_call::Out;
        tilo_live::app_call::AppLane::new(daemon.app_call_setup(&at), out)
    };

    let person = hex::encode(live.person());
    let once = tilo_action_policy::Grant::new(
        "discord-joins-once",
        tilo_action_policy::Grantee::app(APP),
        tilo_action_policy::TreePath::workspace(&workspace).app(super::PROVIDER),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    )
    .with_lifetime(tilo_action_policy::Lifetime::Once);
    tilo_rt::app()
        .block_on(live.workspaces().copies().commit_own_change(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(once),
            },
            None,
        ))
        .expect("the runtime runs the change")
        .expect("the owner's grant is committed");
    let held = tilo_rt::app()
        .block_on(live.workspaces().copies().policy_of(&workspace))
        .unwrap()
        .unwrap();
    assert!(
        held.grants
            .iter()
            .any(|grant| grant.id.as_str() == "discord-joins-once"),
        "the grant given once is the workspace's: {:?}",
        held.grants
            .iter()
            .map(|grant| (grant.id.as_str().to_owned(), grant.lifetime))
            .collect::<Vec<_>>()
    );

    // Two joins decided at the same moment: the grant given once admits exactly one of them.
    let lanes = [lane(), lane()];
    let racing = std::sync::Barrier::new(lanes.len());
    let answers: Vec<serde_json::Value> = std::thread::scope(|scope| {
        let runs: Vec<_> = (1..)
            .zip(&lanes)
            .map(|(correlation, lane)| {
                let racing = &racing;
                scope.spawn(move || {
                    let _entered = tilo_rt::app().handle().enter();
                    racing.wait();
                    result_of(lane.take(&carry(correlation)))
                })
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("a join answers"))
            .collect()
    });
    let joined = serde_json::json!({ "Ok": null });
    assert_eq!(
        answers.iter().filter(|answer| **answer == joined).count(),
        1,
        "one of two joins at once is admitted: {answers:?}"
    );
    assert_eq!(
        session.speaker().call().map(|call| call.call.channel),
        Some(CALL.to_owned()),
        "the join given once is taken"
    );
    until("the grant given once spent", || {
        tilo_rt::app()
            .block_on(live.workspaces().copies().policy_of(&workspace))
            .expect("the runtime reads the policy")
            .is_some_and(|policy| {
                !policy
                    .grants
                    .iter()
                    .any(|grant| grant.id.as_str() == "discord-joins-once")
            })
    });
    drop(lanes);

    let refused = result_of(lane().take(&carry(3)));
    assert!(
        refused.pointer("/Err/Refused/reason").is_some(),
        "the next join is decided afresh and refused: {refused}"
    );
}

/// The person's answer `offer` to their asked record `record`, as the system's confirm view
/// gives it.
fn answer(
    record: &tilo_actions::remote::asked::RecordRef,
    offer: tilo_actions::remote::asked::Offer,
) -> tilo_actions::remote::InvokeRequest {
    let resolving = tilo_actions::remote::asked::Resolve {
        resolution: offer,
        ..tilo_actions::remote::asked::Resolve::default()
    };
    let mut request = super::request(
        tilo_actions::remote::asked::RESOLVE,
        serde_json::to_value(resolving).unwrap(),
    );
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: tilo_actions::remote::workspace_invocations::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            &record.record,
            tilo_actions::remote::asked::RESOLVE,
        ),
    };
    request
}

/// An app with no grant on the device's live sessions asks its person, once, whether it may join
/// the workspace's calls, and every answer is the grant its next join is decided by: approve lets
/// one join through and the next asks again; approve and grant lets every join through, unasked;
/// decline refuses every join, unasked, until they allow it where the app's permissions are.
#[test]
fn an_app_with_no_grant_asks_its_person_once_to_join_calls() {
    use tilo_actions::remote::asked::Offer;

    let Some(Asking {
        daemon,
        workspace,
        live,
        session,
        workspaces,
        store,
        person,
        at,
    }) = asking()
    else {
        return;
    };
    let _entered = tilo_rt::app().handle().enter();
    let in_call = || session.speaker().call().map(|call| call.call.channel);

    // Asked once: the second join finds the question waiting and writes no other.
    assert_eq!(refused(&a_join(&daemon, &at, 1)), Some(super::join_asked_first(APP)));
    assert_eq!(refused(&a_join(&daemon, &at, 2)), Some(super::join_asked_first(APP)));
    assert_eq!(store.waiting_in(&workspace).len(), 1);
    assert_eq!(in_call(), None, "an asked join joins nothing");

    // Approve lets one join through; the next is asked again.
    the_persons_answer(&workspaces, &store, &person, &workspace, Offer::Approve);
    let joined = a_join(&daemon, &at, 3);
    assert_eq!(joined, serde_json::json!({ "Ok": null }), "{joined}");
    assert_eq!(in_call(), Some(CALL.to_owned()));
    the_device_leaves(&live, &workspace);
    until("the join given once spent", || {
        tilo_rt::app()
            .block_on(workspaces.policy_of(&workspace))
            .unwrap()
            .is_some_and(|policy| {
                !policy
                    .grants
                    .iter()
                    .any(|grant| grant.grantee == tilo_action_policy::Grantee::app(APP))
            })
    });
    assert_eq!(refused(&a_join(&daemon, &at, 4)), Some(super::join_asked_first(APP)));

    // Approve and grant lets every join through, unasked.
    the_persons_answer(
        &workspaces,
        &store,
        &person,
        &workspace,
        Offer::ApproveAndGrant,
    );
    until("the granted join", || {
        a_join(&daemon, &at, 5) == serde_json::json!({ "Ok": null })
    });
    the_device_leaves(&live, &workspace);
    assert_eq!(a_join(&daemon, &at, 6), serde_json::json!({ "Ok": null }));
    assert!(store.waiting_in(&workspace).is_empty(), "nobody is asked");
    the_device_leaves(&live, &workspace);

    // Taken back where the app's permissions are, the next join asks; declined, every join is
    // refused saying so, and nobody is asked again; then they change their mind there, and the
    // same grant, allowed, takes its place.
    taken_back_and_declined(
        &daemon,
        &at,
        &session,
        &workspaces,
        &store,
        &person,
        &workspace,
    );
}

/// The reason `answer` refuses a join, when it refuses one.
fn refused(answer: &serde_json::Value) -> Option<String> {
    answer
        .pointer("/Err/Refused/reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The grant taken back where the app's permissions are: the next join asks; declined, every join
/// is refused saying so and nobody is asked again; then they change their mind there, the same
/// grant allowed takes its place.
fn taken_back_and_declined(
    daemon: &Arc<Daemon>,
    at: &crate::seats::AppAt,
    session: &crate::device::live_call::CallSession,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    store: &tilo_actions::remote::asked::AskedStore,
    person: &tilo_actions::remote::Caller,
    workspace: &str,
) {
    let policy = tilo_rt::app()
        .block_on(workspaces.policy_of(workspace))
        .unwrap()
        .unwrap();
    let granted = policy
        .grants
        .iter()
        .find(|grant| grant.grantee == tilo_action_policy::Grantee::app(APP))
        .expect("the grant names the app")
        .clone();
    tilo_rt::app()
        .block_on(workspaces.commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Revoke(granted.id.clone()),
            },
            None,
        ))
        .unwrap()
        .unwrap();
    until("the join asked again", || {
        refused(&a_join(daemon, at, 7)) == Some(super::join_asked_first(APP))
    });
    the_persons_answer(
        workspaces,
        store,
        person,
        workspace,
        tilo_actions::remote::asked::Offer::Decline,
    );
    let declined = tilo_workspace_service::workspaces::vault_release::declined_by_person(APP);
    until("the decline written", || {
        refused(&a_join(daemon, at, 8)) == Some(declined.clone())
    });
    assert_eq!(refused(&a_join(daemon, at, 9)), Some(declined));
    assert!(store.waiting_in(workspace).is_empty(), "nobody is asked again");
    assert_eq!(session.speaker().call(), None);
    // They change their mind where the app's permissions are: the refusal goes, and the same
    // grant, allowed, takes its place.
    for change in [
        tilo_action_policy::Change::Revoke(granted.id.clone()),
        tilo_action_policy::Change::Grant(granted),
    ] {
        tilo_rt::app()
            .block_on(workspaces.commit_own_change(
                workspace,
                tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                    change,
                },
                None,
            ))
            .unwrap()
            .unwrap();
    }
    until("the allowed join", || {
        a_join(daemon, at, 10) == serde_json::json!({ "Ok": null })
    });
    assert!(store.waiting_in(workspace).is_empty());
}

/// The device's daemon with its live session running, and what asks its person about an app's
/// join on it: the app's place, the asked store, and the person who answers.
struct Asking {
    daemon: Arc<Daemon>,
    workspace: String,
    live: Arc<crate::device::live_relay::Live>,
    session: Arc<crate::device::live_call::CallSession>,
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    store: Arc<tilo_actions::remote::asked::AskedStore>,
    person: tilo_actions::remote::Caller,
    at: crate::seats::AppAt,
}

/// [`Asking`], from a device started reporting with its live session up.
fn asking() -> Option<Asking> {
    let root = isolated_root(ASKED_JOIN_TEST)?;
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the person's workspace");
    let id = WorkspaceId::parse(&workspace).expect("a workspace id");
    let live = Arc::clone(daemon.live().expect("the daemon runs its live session"));
    until(
        "the daemon taking part in its workspace's live session",
        || live.call_session(id).is_some(),
    );
    let session = live.call_session(id).unwrap();
    let workspaces = live.workspaces().copies().clone();
    let store = workspaces
        .asked_store()
        .expect("the device keeps asked records");
    let cert = workspaces.device_cert();
    let person =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&workspace);
    let at = crate::seats::AppAt {
        app: APP.to_owned(),
        workspace: workspace.clone(),
        pid: std::process::id(),
    };
    Some(Asking {
        daemon,
        workspace,
        live,
        session,
        workspaces,
        store,
        person,
        at,
    })
}

/// The app's lane joining the call as `correlation`'s carry asks: the answer it was given.
fn a_join(daemon: &Arc<Daemon>, at: &crate::seats::AppAt, correlation: u64) -> serde_json::Value {
    let out = Arc::new(|_: String| {}) as tilo_live::app_call::Out;
    let lane = tilo_live::app_call::AppLane::new(daemon.app_call_setup(at), out);
    result_of(lane.take(&carry(correlation)))
}

/// The person's answer `offer` to the one question waiting in `workspace`, taken as `person`.
fn the_persons_answer(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    store: &tilo_actions::remote::asked::AskedStore,
    person: &tilo_actions::remote::Caller,
    workspace: &str,
    offer: tilo_actions::remote::asked::Offer,
) {
    let waiting = store.waiting_in(workspace);
    assert_eq!(waiting.len(), 1, "one question waits");
    assert_eq!(
        waiting[0].asked.offers,
        vec![
            tilo_actions::remote::asked::Offer::Approve,
            tilo_actions::remote::asked::Offer::ApproveAndGrant,
            tilo_actions::remote::asked::Offer::Decline
        ]
    );
    let answered = tilo_rt::app()
        .block_on(async {
            workspaces
                .resolve_asked(person, &answer(&waiting[0].asked.record, offer))
                .await
        })
        .expect("the runtime answers")
        .expect("the answer is answered")
        .expect("their answer is taken");
    assert!(
        matches!(
            answered.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "{answered:?}"
    );
}

/// The device leaves the call in `workspace`, as a person's answer or a session's end does.
fn the_device_leaves(live: &crate::device::live_relay::Live, workspace: &str) {
    tilo_rt::app()
        .block_on(live.join_in(workspace, None))
        .expect("the runtime runs the leave")
        .expect("the device leaves the call");
}

/// The same join over the app's own connection, as the daemon serves the process it started and
/// the app's SDK reaches it: the carry is refused with the reason, a report is taken although the
/// device carries no call for it, and the device joins nothing.
fn the_apps_connection_joins_nothing(
    daemon: &Arc<Daemon>,
    at: &crate::seats::AppAt,
    session: &crate::device::live_call::CallSession,
) {
    let app = app_connection(daemon, at);
    assert_eq!(
        app.carry_call(Some(BridgedCall {
            outside: OUTSIDE.into(),
            tilo: CALL.into(),
        })),
        Err(tilo_actions::remote::ActionCallError::Refused {
            reason: super::join_asked_first(APP)
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
}

/// The person's grant `id` naming [`APP`] on this device's live sessions in `workspace`: the app,
/// never the person, holds it.
fn grant_the_app(live: &crate::device::live_relay::Live, workspace: &str, id: &str) {
    let person = hex::encode(live.person());
    let grant = tilo_action_policy::Grant::new(
        id,
        tilo_action_policy::Grantee::app(APP),
        tilo_action_policy::TreePath::workspace(workspace).app(super::PROVIDER),
        tilo_action_policy::KindSet::ALL,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(person),
    );
    tilo_rt::app()
        .block_on(live.workspaces().copies().commit_own_change(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::Grant(grant),
            },
            None,
        ))
        .expect("the runtime runs the change")
        .expect("the owner's grant is committed");
}

/// The app reports its outside call with Dana in it: the bridge carries her into the device's call
/// as a participant of her own, and what she says is sealed under the call's keys the daemon holds.
/// Nobody here asks for the device's voice yet, so the frame is unwanted rather than withheld: the
/// device may speak in the call and seals for it.
fn dana_is_carried_into_the_call(
    lane: &tilo_live::app_call::AppLane,
    session: &crate::device::live_call::CallSession,
) {
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
/// stop being said in the workspace, and the device, which nobody is at now, says nothing there.
/// A person gone from their computer is in no call and not here, as their roster row on everyone
/// else's device shows.
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
        session
            .speaker()
            .word()
            .is_some_and(|word| word.status.is_some()),
        "the person says what they are doing while their shell runs"
    );
    assert!(
        session.speaker().call().is_some(),
        "the person is in the call while their shell runs"
    );

    live.gone();
    loop {
        if session.speaker().word().is_none() && session.speaker().call().is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the word or the call outlived the shell"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(20));
    }
}
