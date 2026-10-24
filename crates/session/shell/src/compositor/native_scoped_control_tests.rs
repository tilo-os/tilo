//! A program running in an app, over the connection the agent door hands
//! over ([`super::scoped_control`]), driven through the real transport
//! ([`pump_test_control_conn`]).

use super::tests::socket_response;
use super::*;

/// `tilo action` run in a terminal app reaches the desktop through the
/// agent door, which hands the connection here with the terminal's
/// workspace scope: it lists and runs what works in that workspace; the
/// first action beyond it asks the person, a refusal is said, "this tab"
/// lets the next ones run until it is stopped; and it can do nothing
/// else — no input, no screenshots, nothing while the session is locked.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scoped connection's whole conversation, read top to bottom"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the desktop and its broker serve the conversation to its last request, through \
              the pump closure the lint does not see"
)]
fn a_program_in_an_app_lists_and_runs_actions_with_its_workspaces_scope() {
    use crate::compositor::workspace_door::{AppScope, Holder};
    use crate::shell::collab_broker_capnp::ActionDirectoryRoot;
    use crate::shell::search::providers::live_app_actions::fixture_with_directory_page;
    use std::io::Write as _;

    let mut frame_builder = FrameBuilder::new((800, 600));
    let (room, actor) = {
        let chrome = frame_builder.chrome();
        let room = chrome.tree().desktop.active_workspace().unwrap().room();
        let actor = chrome.local_user_for_room(&room);
        drop(chrome);
        (room.to_string(), actor)
    };
    frame_builder
        .chrome()
        .tree_handle()
        .write()
        .desktop
        .active_workspace_mut()
        .unwrap()
        .set_owner(actor.clone());
    let tree = frame_builder.chrome().tree_handle();
    let broker = crate::shell::broker_services::test_broker_with_membership(tree);
    broker.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
        peer_id: 4096,
        app_id: "dev.tilo.calculator".into(),
        cluster_id: room.clone(),
        record_id: "calculator-document".into(),
    }]);
    let page = tilo_actions::remote::CataloguePage {
        revision: "revision-3".into(),
        rows: vec![tilo_actions::remote::CatalogueRow {
            provider: "4096".into(),
            app_id: "dev.tilo.calculator".into(),
            parent_resource: String::new(),
            resource: tilo_actions::remote::CatalogueResource {
                id: "calculator.graph:curve2d:stable-17".into(),
                kind: "calculator.graph".into(),
                label: "Curve".into(),
                presentation: None,
            },
            action: Some(tilo_actions::ActionDescriptor {
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(
                    "calculator.graph:curve2d:stable-17",
                    "toggle-visibility",
                ),
                resource_kind: "calculator.graph".into(),
                resource_label: "Curve".into(),
                action_label: "Hide".into(),
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::View,
                contract: tilo_actions::ActionContract::view(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }),
            has_children: false,
            visibility: None,
        }],
        exhausted: true,
        ..tilo_actions::remote::CataloguePage::default()
    };
    let provider = fixture_with_directory_page(broker, 4096, actor, page);
    crate::shell::overlay_host::install_fixture_search_provider(frame_builder.host_mut(), provider);
    let mut ctx = TestControlCtx::new();
    let mut session = SessionManager::new(IdleConfig::from_env(), Instant::now());
    let mut lock = LockController::new(lock_auth::verifier_from_env(), "fixture");
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
    let display: Display<State> = Display::new().expect("display");
    let mut state = State::new(event_loop.handle(), display.handle());

    // The tab: this test process stands for its shell.
    let tab = {
        let pid = std::process::id();
        let (_, start) = crate::shell::host_runtime::proc_stat(pid).expect("own stat");
        Holder { pid, start }
    };
    let (server, client) = UnixStream::pair().expect("socket pair");
    let mut writer = client.try_clone().expect("client writer");
    let mut reader = BufReader::new(client);
    // The desktop's tab it was typed in.
    let typed_in = frame_builder
        .chrome()
        .tree()
        .desktop
        .active_workspace()
        .and_then(|workspace| workspace.strips.first())
        .and_then(|strip| strip.window_groups.first())
        .map_or(1, |group| group.active_tab);
    let mut conn = TestControlConn::scoped(
        server,
        r#"{"op":"actions_enumerate","query":""}"#.into(),
        AppScope {
            package: "so.tilo.terminal".into(),
            room: room.clone(),
            holder: tab,
            tabs: vec![typed_in],
        },
    )
    .expect("scoped connection");

    // The request the door read first is answered first.
    pump_test_control_conn(
        &mut conn,
        &mut frame_builder,
        &mut ctx,
        &session,
        &mut lock,
        &mut state,
    );
    let test_control::Response::Answer(listed) = socket_response(&mut reader) else {
        panic!("the first request is served");
    };
    let rows = listed["actions"].as_array().expect("rows");
    assert!(
        rows.iter()
            .any(|row| row["provider"] == "app-actions" && row["title"] == "Hide"),
        "the terminal's workspace's actions are listed: {listed}"
    );
    assert!(
        rows.iter()
            .all(|row| row["room"].is_null() || row["room"] == room.as_str()),
        "nothing of another workspace is listed: {listed}"
    );
    assert!(
        rows.iter()
            .all(|row| row["provider"] != "permission-prompts"),
        "a terminal never sees the person's own decisions: {listed}"
    );
    assert_eq!(listed["count"], rows.len());
    let hide = rows
        .iter()
        .find(|row| row["title"] == "Hide")
        .expect("the workspace's action")
        .clone();

    let chrome = frame_builder.chrome_handle();
    let mut send = |line: &str| {
        writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("write request");
    };
    let mut pump = |session: &SessionManager| {
        pump_test_control_conn(
            &mut conn,
            &mut frame_builder,
            &mut ctx,
            session,
            &mut lock,
            &mut state,
        );
    };
    macro_rules! ask {
        ($line:expr, $session:expr $(,)?) => {{
            send($line);
            pump($session);
            socket_response(&mut reader)
        }};
    }
    // The person's answer to the prompt in front, as a click gives it.
    let answer = |outcome: crate::portal::PromptOutcome| {
        let mut pending = chrome
            .borrow_mut()
            .pending_grants
            .pop_front()
            .expect("the person is asked");
        assert!(
            matches!(
                &pending.request,
                crate::shell::chrome_state::PromptRequest::DesktopControl(ask)
                    if !ask.sensitive && ask.holder == tab
            ),
            "{pending:?}"
        );
        pending.answer(outcome);
    };

    // What works in its workspace runs, unasked.
    let invoke = serde_json::json!({
        "op": "actions_invoke", "provider": hide["provider"], "id": hide["id"],
    })
    .to_string();
    let ran = ask!(&invoke, &session);
    assert!(
        !matches!(&ran, test_control::Response::Err { code, .. } if code == "not_allowed"),
        "an action in its own workspace is not refused for its scope: {ran:?}"
    );
    assert!(
        chrome.borrow().pending_grants.is_empty(),
        "nobody was asked"
    );

    // The first desktop-wide action asks the person, and waits.
    let overview = r#"{"op":"actions_invoke","provider":"actions","id":"overview.toggle"}"#;
    send(overview);
    pump(&session);
    assert_eq!(chrome.borrow().pending_grants.len(), 1, "one prompt");
    pump(&session);
    assert_eq!(
        chrome.borrow().pending_grants.len(),
        1,
        "asked once, not again"
    );
    answer(crate::portal::PromptOutcome::Deny);
    pump(&session);
    let refused = socket_response(&mut reader);
    assert!(
        matches!(&refused, test_control::Response::Err { code, .. } if code == "not_allowed"),
        "a refusal is said, not swallowed: {refused:?}"
    );

    // Asked again, the person allows this tab: it runs, and the next
    // desktop-wide action runs unasked.
    send(overview);
    pump(&session);
    answer(crate::portal::PromptOutcome::AllowSession);
    pump(&session);
    let allowed = socket_response(&mut reader);
    assert!(
        !matches!(&allowed, test_control::Response::Err { code, .. } if code == "not_allowed"),
        "{allowed:?}"
    );
    assert!(chrome.borrow().desktop_control.tab_allowed(tab));
    assert!(
        chrome
            .borrow()
            .desktop_control
            .tabs()
            .any(|grant| grant.tabs == [typed_in]),
        "the grant shows on the tab it was asked from"
    );
    let again = ask!(overview, &session);
    assert!(
        !matches!(&again, test_control::Response::Err { code, .. } if code == "not_allowed"),
        "{again:?}"
    );
    assert!(
        chrome.borrow().pending_grants.is_empty(),
        "allowed once for the tab"
    );

    // Stopped, the tab is asked again.
    assert!(chrome.borrow_mut().desktop_control.revoke_tab(tab));
    send(overview);
    pump(&session);
    assert_eq!(chrome.borrow().pending_grants.len(), 1, "asked again");
    answer(crate::portal::PromptOutcome::Deny);
    pump(&session);
    let _ = socket_response(&mut reader);

    // The person's decisions are not even there to address.
    let decide = ask!(
        r#"{"op":"actions_invoke","provider":"permission-prompts","id":"allow-once@x","args":{"confirm":true}}"#,
        &session,
    );
    assert!(
        matches!(&decide, test_control::Response::Err { code, .. } if code == "unknown_action"),
        "{decide:?}"
    );

    for refused in [
        r#"{"op":"click_at","x":10,"y":10}"#,
        r#"{"op":"type","text":"hello"}"#,
        r#"{"op":"snapshot","path":"scoped.png"}"#,
        r#"{"op":"state_dump"}"#,
        r#"{"op":"actions_status","invocation_id":1}"#,
        r#"{"op":"actions_enumerate","query":"","agent":"bot"}"#,
    ] {
        let test_control::Response::Err { code, .. } = ask!(refused, &session) else {
            panic!("{refused} must be refused from an app");
        };
        assert_eq!(code, "not_allowed", "{refused}");
    }

    let _ = session.request_lock(Instant::now());
    assert!(session.is_locked());
    let locked = ask!(r#"{"op":"actions_enumerate","query":""}"#, &session);
    assert!(
        matches!(&locked, test_control::Response::Err { code, .. } if code == "session_locked"),
        "{locked:?}"
    );
}
