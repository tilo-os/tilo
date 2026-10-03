//! Tests of the API tester root widget: the model it keeps, the commands it
//! applies and the views it rebuilds. The widget tree itself is exercised
//! through a headless driver in `tests/a11y.rs`.

use super::*;
use automerge::ROOT;
use ui::headless::Driver;

/// A demo app whose workspace has been seeded into a fresh document.
fn demo() -> (ApiTesterApp, AutoCommit) {
    let mut app = ApiTesterApp::new_demo();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &ROOT);
    (app, doc)
}

fn push(app: &ApiTesterApp, cmd: Cmd) {
    app.sink.borrow_mut().push(cmd);
}

/// The live constructor fabricates nothing, the demo constructor seeds a full
/// workspace, and a seeded workspace survives a CRDT round trip.
#[test]
fn constructors_start_live_empty_demo_seeded_and_persist_through_crdt() {
    let a = ApiTesterApp::new_demo();
    assert!(!a.collections.is_empty());
    assert!(!a.environments.is_empty());
    assert!(a.active_env.is_some());
    assert!(a.response.is_some());

    let live = ApiTesterApp::new();
    assert!(live.collections.is_empty(), "no fabricated collections");
    assert!(live.environments.is_empty(), "no fabricated environments");
    assert!(live.history.is_empty(), "no fabricated history");
    assert!(
        live.response.is_none(),
        "no canned response before a real Send"
    );

    let mut doc = AutoCommit::new();
    a.seed_workspace(&mut doc, &ROOT);
    let mut b = ApiTesterApp::new();
    b.adopt(&doc, &ROOT);
    assert_eq!(b.collections.len(), a.collections.len());
    assert_eq!(b.environments.len(), a.environments.len());
}

/// The request builder's derived state: the active environment resolves, the
/// outgoing URL is built from the document's text, and loading a saved request
/// swaps the builder and the document's method, URL and body.
#[test]
fn request_builder_resolves_environment_and_loads_saved_requests() {
    let (mut a, mut doc) = demo();
    assert_eq!(
        a.env_vars().get("base_url").map(String::as_str),
        Some("https://api.example.com/v2")
    );
    let out = a.outgoing(&doc, &ROOT);
    assert!(out.url.starts_with("https://api.example.com/v2/users"));
    assert!(out.url.contains("page=1"));
    assert!(out.url.contains("per_page=25"));
    assert!(!out.url.contains("sort="), "a disabled row is not sent");

    // The URL the person typed is what is sent, not the model's copy.
    ApiDoc::set_text(&mut doc, &ROOT, "url", "{{base_url}}/typed");
    assert!(a.outgoing(&doc, &ROOT).url.contains("/v2/typed"));

    push(&a, Cmd::LoadRequest(0, 2));
    assert!(a.drain_commands(&mut doc, &ROOT));
    assert_eq!(a.request.method, HttpMethod::Post);
    assert_eq!(a.request.body_mode, BodyMode::Json);
    assert_eq!(a.selected, Some((0, 2)));
    assert_eq!(ApiDoc::read_text(&doc, &ROOT, "url"), "{{base_url}}/users");
    assert_eq!(ApiDoc::read_text(&doc, &ROOT, METHOD_PATH), "POST");
    assert!(ApiDoc::read_text(&doc, &ROOT, "body").contains("Grace Hopper"));
    assert_eq!(a.active_tab, RequestTab::Params);
}

/// Replaying a history entry opens it in the builder without sending it.
#[test]
fn replaying_history_fills_the_builder_without_sending() {
    let (mut a, mut doc) = demo();
    push(&a, Cmd::ReplayHistory(1));
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(a.request.method, HttpMethod::Post);
    assert_eq!(
        ApiDoc::read_text(&doc, &ROOT, "url"),
        "https://api.example.com/v2/users"
    );
    assert!(!a.sending);
    assert_eq!(a.selected, None);
}

/// The environment picker is wired through the action host: selecting a new
/// environment dispatches exactly the [`SetActiveEnvironment`] the Ctrl+K
/// "activate" spec would build (affordance == `ActionSpec`), and the choice
/// lands in the shared document. Its commands open the sheets that manage
/// environments and are not kept as a value.
#[test]
fn selecting_environment_dispatches_the_set_active_action() {
    let (mut a, mut doc) = demo();
    assert!(a.environments.len() >= 2, "demo seeds several environments");
    let target = a.environments[1].clone();
    assert_ne!(a.active_env, Some(target.id), "target isn't already active");

    push(&a, Cmd::PickEnvironment(target.name.clone()));
    a.drain_commands(&mut doc, &ROOT);

    assert_eq!(
        a.active_env,
        Some(target.id),
        "the active environment switched"
    );
    let dispatched = a
        .actions
        .last_dispatched()
        .expect("the picker dispatched an action");
    let expected = actions::encode(&actions::set_active_environment_action(Some(target.id)));
    assert_eq!(dispatched.type_id, expected.type_id);
    assert_eq!(
        dispatched.payload, expected.payload,
        "affordance == ActionSpec"
    );
    assert_eq!(
        ApiDoc::active_env(&doc, &ROOT),
        Some(target.id.to_string()),
        "the selection is recorded in the shared document"
    );
    assert!(
        a.env_vars
            .borrow()
            .get("base_url")
            .is_some_and(|url| url.contains("staging")),
        "the variable hint follows the environment"
    );

    push(
        &a,
        Cmd::PickEnvironment(ENVIRONMENT_COMMANDS[2].0.to_owned()),
    );
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(
        a.form.as_ref().map(|form| form.mode),
        Some(FormMode::NewVariable)
    );
    assert_eq!(a.env_state.get(), target.name, "a command is not a value");
}

/// Renaming or deleting the active environment updates the picker to match
/// instead of the stale picker switching the environment away.
#[test]
fn renaming_the_active_environment_keeps_it_active() {
    let (mut a, mut doc) = demo();
    let active = a.active_env.expect("demo has an active environment");
    a.open_form(FormMode::RenameEnvironment, FormTarget::default());
    a.form
        .as_ref()
        .expect("sheet open")
        .fields
        .name
        .set("Live".into());
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    a.sync(&mut doc, &ROOT);
    assert_eq!(a.active_env, Some(active));
    assert_eq!(a.env_state.get(), "Live");

    a.open_form(FormMode::DeleteEnvironment, FormTarget::default());
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    a.sync(&mut doc, &ROOT);
    assert!(a.environments.iter().all(|e| e.id != active));
    assert_eq!(a.active_env, None);
    assert_eq!(
        a.environments.len(),
        2,
        "only the environment itself was deleted"
    );
    assert_eq!(a.env_state.get(), NO_ENV);
}

/// Each sheet collects what its action needs and dispatches the canonical
/// action; a sheet opened from a row acts on that row.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario walks every sheet so each reads against the same workspace"
)]
#[test]
fn sheets_dispatch_canonical_actions_for_the_row_they_were_opened_on() {
    let (mut a, mut doc) = demo();

    a.open_form(FormMode::NewCollection, FormTarget::default());
    a.form
        .as_ref()
        .expect("sheet open")
        .fields
        .name
        .set("Billing API".into());
    push(&a, Cmd::SubmitForm);
    assert!(a.drain_commands(&mut doc, &ROOT));
    assert!(
        ApiDoc::collections(&doc, &ROOT)
            .iter()
            .any(|collection| collection.name == "Billing API"),
        "New collection dispatched its canonical action"
    );
    assert_eq!(
        a.actions.last_dispatched().expect("recorded").type_id,
        API_TESTER_ACTION_TYPE
    );
    assert!(a.form.is_none(), "the sheet closes on submit");

    a.open_form(FormMode::NewVariable, FormTarget::default());
    let fields = a.form.as_ref().expect("sheet open").fields.clone();
    fields.key.set("region".into());
    fields.value.set("eu-west-1".into());
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    let active = a.active_env.expect("demo has an active environment");
    assert!(
        ApiDoc::environments(&doc, &ROOT)
            .iter()
            .find(|environment| environment.id == active)
            .is_some_and(|environment| environment
                .variables
                .iter()
                .any(|variable| variable.key == "region" && variable.value == "eu-west-1")),
        "Add variable dispatched its canonical action"
    );

    // A hover action on the second saved request renames that request, not
    // the one open in the builder.
    let (collection, request) = {
        let collection = a
            .collections
            .iter()
            .find(|collection| {
                collection
                    .requests
                    .iter()
                    .any(|r| r.name == "Get user by id")
            })
            .expect("demo collection");
        let request = collection
            .requests
            .iter()
            .find(|r| r.name == "Get user by id")
            .expect("demo request");
        (collection.id, request.id)
    };
    a.open_form(
        FormMode::RenameRequest,
        FormTarget {
            collection: Some(collection),
            request: Some(request),
        },
    );
    assert_eq!(
        a.form.as_ref().expect("sheet open").fields.name.get(),
        "Get user by id",
        "the sheet starts from the row's current name"
    );
    a.form
        .as_ref()
        .expect("sheet open")
        .fields
        .name
        .set("Fetch user".into());
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    let names: Vec<&str> = a
        .collections
        .iter()
        .flat_map(|collection| collection.requests.iter().map(|r| r.name.as_str()))
        .collect();
    assert!(names.contains(&"Fetch user") && names.contains(&"List users"));
    assert!(!names.contains(&"Get user by id"));

    // Cancelling changes nothing.
    let heads = doc.get_heads();
    a.open_form(FormMode::DeleteRequest, FormTarget::default());
    push(&a, Cmd::CancelForm);
    a.drain_commands(&mut doc, &ROOT);
    assert!(a.form.is_none());
    assert_eq!(doc.get_heads(), heads);

    // Deleting from a row removes it and keeps the builder's selection.
    a.open_form(
        FormMode::DeleteRequest,
        FormTarget {
            collection: Some(collection),
            request: Some(request),
        },
    );
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    let remaining: usize = a.collections.iter().map(|c| c.requests.len()).sum();
    assert_eq!(remaining, 6, "one of the seven demo requests is gone");
    let (ci, ri) = a.selected.expect("the builder's request is still selected");
    assert_eq!(a.collections[ci].requests[ri].name, "List users");
}

/// Saving names the request, files it in a collection and selects it.
#[test]
fn saving_a_request_files_it_and_selects_it() {
    let (mut a, mut doc) = demo();
    ApiDoc::set_text(&mut doc, &ROOT, "url", "{{base_url}}/health");
    a.request = Request::new("Health");
    a.selected = None;
    a.open_form(FormMode::SaveRequest, FormTarget::default());
    push(&a, Cmd::SubmitForm);
    a.drain_commands(&mut doc, &ROOT);
    let (ci, ri) = a.selected.expect("the saved request is selected");
    let saved = &a.collections[ci].requests[ri];
    assert_eq!(saved.name, "Health");
    assert_eq!(saved.url, "{{base_url}}/health");
    assert_eq!(a.request.name, "Health");
}

/// View commands choose what shows and stay off the collaborative document.
#[test]
fn view_commands_stay_device_local() {
    let (mut a, mut doc) = demo();
    let heads = doc.get_heads();
    push(&a, Cmd::SetTab(1));
    push(&a, Cmd::SetView(2));
    push(&a, Cmd::SetBodyMode(1));
    push(&a, Cmd::SetAuthKind(2));
    assert!(a.drain_commands(&mut doc, &ROOT));
    assert_eq!(a.active_tab, RequestTab::Headers);
    assert_eq!(a.response_view, ResponseView::Headers);
    assert_eq!(a.request.body_mode, BodyMode::Json);
    assert!(matches!(a.request.auth, Auth::Basic { .. }));
    assert_eq!(doc.get_heads(), heads, "no document change");
    assert!(a.stale.tab_panel && a.stale.response_body && a.stale.body_inner);
    a.sync(&mut doc, &ROOT);
    assert!(!a.stale.tab_panel && !a.stale.response_body && !a.stale.body_inner);
}

/// Typing in the blank row makes it a pair and gives the table a new blank
/// row; edits, the checkbox and remove act on the pair by id.
#[test]
fn key_value_tables_edit_the_request() {
    let (mut a, mut doc) = demo();
    let (table, queue) = a.table.clone().expect("the Params table is showing");
    assert_eq!(table, Table::Params);
    let count = a.request.params.len();
    let blank = a.blanks.params.clone();

    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: blank.clone(),
            edit: RowEdit::Key("limit".into()),
        },
    );
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(a.request.params.len(), count + 1);
    let pair = a.request.params.last().expect("the new pair");
    assert_eq!(
        (pair.id.as_str(), pair.key.as_str()),
        (blank.as_str(), "limit")
    );
    assert_ne!(a.blanks.params, blank, "the table gets a fresh blank row");
    assert_eq!(
        queue.pending(),
        2,
        "the pair is activated and a blank appended"
    );

    // Further typing edits the same pair; it does not append again.
    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: blank.clone(),
            edit: RowEdit::Value("10".into()),
        },
    );
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(a.request.params.len(), count + 1);
    assert_eq!(a.request.params[count].value, "10");

    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: blank.clone(),
            edit: RowEdit::Enabled(false),
        },
    );
    a.drain_commands(&mut doc, &ROOT);
    assert!(!a.request.params[count].enabled);
    assert!(
        !a.outgoing(&doc, &ROOT).url.contains("limit="),
        "an unchecked row is not sent"
    );

    // Emptying the blank row's text does not add a pair.
    let fresh = a.blanks.params.clone();
    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: fresh,
            edit: RowEdit::Key(String::new()),
        },
    );
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(a.request.params.len(), count + 1);

    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: blank,
            edit: RowEdit::Remove,
        },
    );
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(a.request.params.len(), count);
}

/// The auth tab edits the scheme the request is composed with.
#[test]
fn auth_fields_edit_the_request() {
    let (mut a, mut doc) = demo();
    push(&a, Cmd::SetAuthKind(2));
    push(&a, Cmd::EditAuth(AuthField::Username, "ada".into()));
    push(&a, Cmd::EditAuth(AuthField::Password, "secret".into()));
    push(&a, Cmd::EditAuth(AuthField::Token, "ignored".into()));
    a.drain_commands(&mut doc, &ROOT);
    assert_eq!(
        a.request.auth,
        Auth::Basic {
            username: "ada".into(),
            password: "secret".into()
        }
    );
    push(&a, Cmd::SetAuthKind(3));
    a.drain_commands(&mut doc, &ROOT);
    assert!(matches!(&a.request.auth, Auth::ApiKey { header, .. } if header == "X-Api-Key"));
}

/// Every named `--shots` scenario stages, renders, and leaves an accessibility
/// tree with no findings.
#[test]
fn every_scenario_stages_and_renders_an_accessible_frame() {
    for scenario in ApiTesterApp::SCENARIOS {
        let mut app = ApiTesterApp::new_demo();
        assert!(app.stage(scenario), "{scenario} is a known scenario");
        let size = if scenario.contains("drawer") {
            (520, 800)
        } else {
            (1280, 800)
        };
        let d = Driver::new(ui::State::new(), Box::new(app), size);
        assert!(
            d.render_rgba().iter().any(|byte| *byte != 0),
            "{scenario} renders"
        );
        let audit = d.native_app_audit();
        assert!(
            audit.accessibility.is_clean(),
            "{scenario}: {:#?}",
            audit.accessibility
        );
    }
    let mut app = ApiTesterApp::new_demo();
    assert!(!app.stage("no-such-scenario"));
}

/// The resource surface: Send performs a real HTTP round trip and reports
/// worker completion exactly once, and a late record resolves by one indexed
/// lookup.
#[test]
fn resource_provider_sends_for_real_and_resolves_exact_records() {
    resource_send_performs_a_real_http_round_trip();
    resource_send_reports_worker_completion_instead_of_queue_success();
    resource_provider_resolves_a_late_record_without_scanning_the_workspace();
}

/// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end scenario that reads top-to-bottom"
)]
fn resource_send_performs_a_real_http_round_trip() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use ui::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = tilo_test_env::hang_deadline();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        ui::time::now() < deadline,
                        "the app's HTTP request never reached the probe server"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept request: {error}"),
            }
        };
        // Windows inherits the listener's nonblocking mode on accept.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        stream
            .set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192, "unbounded request headers");
        }
        let request = String::from_utf8(request).unwrap();
        assert!(
            request.starts_with("GET /probe?name=Gr%C3%BCn HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-tilo-probe: native\r\n"),
            "{request}"
        );
        let body = "{\"result\":\"Grün\"}";
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });

    let mut app = ApiTesterApp::new();
    app.request.params.push(KeyValue::new("name", "Grün"));
    app.request
        .headers
        .push(KeyValue::new("X-Tilo-Probe", "native"));
    let invocation = tilo_actions::InvocationId::new();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &ROOT);
    ApiDoc::set_text(&mut doc, &ROOT, "url", &format!("http://{address}/probe"));
    assert!(ApiDoc::history(&doc, &ROOT).is_empty());
    let payload =
        serde_json::to_vec(&crate::actions::ApiTesterOperation::SendActiveRequest).unwrap();
    assert!(matches!(
        app.start_operation(
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::API_TESTER_OPERATION_TYPE,
                &payload
            ),
            &mut doc,
            &ROOT
        )
        .unwrap(),
        Some(ui::widget::ResourceOperationStart::Accepted)
    ));
    assert!(app.sending, "the pane shows the request is on its way");
    // The builder remains editable while a request is in flight. History
    // must describe the request sent, not these later edits.
    ApiDoc::set_text(&mut doc, &ROOT, "url", "http://127.0.0.1/edited-after-send");
    app.request.method = HttpMethod::Post;
    let update = tilo_test_env::wait_for("the HTTP worker to complete the request", || {
        app.poll_resource_operation(invocation, false, &doc, &ROOT)
    });
    assert!(matches!(
        update,
        tilo_actions::InvocationUpdate::Succeeded { .. }
    ));
    server.join().unwrap();
    let response = app.response.as_ref().expect("the response is displayed");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "{\"result\":\"Grün\"}");
    assert_eq!(app.history.len(), 1);
    assert_eq!(app.history[0].status, 200);
    assert_eq!(app.history[0].method, HttpMethod::Get);
    assert_eq!(
        app.history[0].url,
        format!("http://{address}/probe?name=Gr%C3%BCn")
    );
    assert!(!app.sending);
    app.sync(&mut doc, &ROOT);
    assert_eq!(ApiDoc::history(&doc, &ROOT), app.history);
    let mut restarted = ApiTesterApp::new();
    restarted.sync(&mut doc, &ROOT);
    assert_eq!(
        restarted.history, app.history,
        "completed requests survive restart"
    );
    let entry = app.history[0].clone();
    app.perform(&mut doc, &ROOT, &ApiTesterAction::RecordHistory { entry });
    assert_eq!(
        ApiDoc::history(&doc, &ROOT).len(),
        1,
        "replay does not duplicate history"
    );
}

/// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
fn resource_send_reports_worker_completion_instead_of_queue_success() {
    let (mut a, doc) = demo();
    let invocation = tilo_actions::InvocationId::new();
    a.resource_send_invocation = Some(invocation);
    a.tx.send(SendOutcome::Ok {
        response: http::sample_response(),
        method: HttpMethod::Get,
        url: "http://localhost/test".into(),
    })
    .unwrap();
    assert!(matches!(
        a.poll_resource_operation(invocation, false, &doc, &ROOT),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));
    assert!(
        a.poll_resource_operation(invocation, false, &doc, &ROOT)
            .is_none(),
        "terminal delivery is at most once"
    );
}

/// A failed request shows the failure once, in the response pane.
#[test]
fn a_failed_request_is_shown_in_the_response_pane() {
    let (mut a, mut doc) = demo();
    a.tx.send(SendOutcome::Err("Could not reach the host.".into()))
        .unwrap();
    a.sync(&mut doc, &ROOT);
    assert!(a.response.is_none());
    assert_eq!(a.error.as_deref(), Some("Could not reach the host."));
    assert!(!a.stale.response, "the pane was rebuilt");
}

/// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
fn resource_provider_resolves_a_late_record_without_scanning_the_workspace() {
    let (mut a, _) = demo();
    let mut collection = Collection::new("Load test");
    collection.requests = (0..5_000)
        .map(|index| Request::new(format!("Request {index}")))
        .collect();
    let request = collection.requests.last().expect("late request").id;
    a.collections = vec![collection];
    a.environments.clear();
    a.refresh_search();

    let doc = AutoCommit::new();
    let id = format!("api_tester.request:{request}");
    let resource =
        ApiTesterApp::publish_resource(&a, &doc, &id).expect("late exact address resolves");
    assert_eq!(resource.id.0, id);
    assert!(resource.actions.iter().any(|action| action.id == "send"));
    assert_eq!(
        a.search_provider.last_exact_inspected(),
        1,
        "the performs one indexed address lookup"
    );
}

/// The document returns records in the order of their ids, so a pick or a
/// rename that re-reads it must not shuffle the lists a person arranged.
#[test]
fn rereading_the_document_keeps_the_order_on_screen() {
    let (mut app, mut doc) = demo();
    let names = |app: &ApiTesterApp| -> Vec<String> {
        app.environments.iter().map(|e| e.name.clone()).collect()
    };
    let collections = |app: &ApiTesterApp| -> Vec<String> {
        app.collections.iter().map(|c| c.name.clone()).collect()
    };
    let before = (names(&app), collections(&app));
    for _ in 0..8 {
        app.adopt_workspace(&doc, &ROOT);
        assert_eq!((names(&app), collections(&app)), before);
    }
    let first = app.environments[0].id;
    let second = app.environments[1].id;
    app.pick_environment(&mut doc, &ROOT, "Staging");
    assert_eq!(app.active_env, Some(second));
    assert_ne!(app.active_env, Some(first));
    assert_eq!((names(&app), collections(&app)), before);
}
