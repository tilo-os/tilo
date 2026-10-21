//! Tests of the API tester root widget: the model it keeps, the commands it
//! applies and the views it rebuilds. The widget tree itself is exercised
//! through a headless driver in `tests/a11y.rs`.

use super::*;
use crate::resource::ApiTesterShell;
use automerge::{AutoCommit, ROOT};
use ui::Theme;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Edit, Invoke, Operation as _, Replica, Resource as _};

/// The sample workspace, built through its edits and verbs.
fn sample() -> AutoCommit {
    let mut replica = Replica::<Workspace>::new();
    for invoke in crate::seed::workspace() {
        replica.apply(invoke).unwrap();
    }
    AutoCommit::load(&replica.save()).unwrap()
}

/// The record `doc` holds.
fn record(doc: &AutoCommit) -> Rc<Workspace> {
    Rc::new(Workspace::read(doc).unwrap())
}

/// A demo app whose workspace has been seeded into a fresh document.
fn demo() -> (ApiTesterApp, AutoCommit) {
    let dispatcher = ui::framework::testing::dispatcher::<Workspace>("so.tilo.api-tester");
    let doc = sample();
    let mut app = ApiTesterApp::new_demo();
    app.dispatcher = Some(dispatcher.clone());
    *app.sink.dispatcher.borrow_mut() = Some(dispatcher);
    app.present_workspace(&record(&doc));
    app.sync(&doc, &ROOT);
    (app, doc)
}

thread_local! { static LAST: std::cell::RefCell<Option<serde_json::Value>> = const { std::cell::RefCell::new(None) }; }

fn resync(app: &mut ApiTesterApp, doc: &AutoCommit) {
    app.present_workspace(&record(doc));
    app.sync(doc, &ROOT);
}

/// Apply what the window submitted, as its host does, and present the
/// result. Remembers the last field edit applied.
fn drain(app: &mut ApiTesterApp, doc: &mut AutoCommit) -> bool {
    let changed = app.drain_commands();
    apply(app, doc);
    changed
}

fn apply(app: &mut ApiTesterApp, doc: &mut AutoCommit) {
    let applied = ui::framework::testing::resolve(app.dispatcher.as_ref().unwrap(), doc).unwrap();
    if let Some(action) = applied.last().and_then(|group| group.last()) {
        let edit = action
            .operation
            .edit()
            .map(|edit| serde_json::to_value(edit).unwrap());
        LAST.with(|last| *last.borrow_mut() = edit);
    }
    app.present_workspace(&record(doc));
}

/// The field edit `edit` as the last drain records one.
fn as_applied(edit: &Edit<Workspace>) -> serde_json::Value {
    serde_json::to_value(edit).unwrap()
}

fn outgoing(app: &mut ApiTesterApp, doc: &AutoCommit) -> Outgoing {
    app.present_workspace(&record(doc));
    app.outgoing()
}

/// Apply `submit` to `doc` as the window would submit it.
fn submit(
    app: &mut ApiTesterApp,
    doc: &mut AutoCommit,
    submit: impl ui::framework::Submit<Workspace>,
) {
    app.submit(submit);
    apply(app, doc);
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

    let doc = sample();
    let mut b = ApiTesterApp::new();
    b.present_workspace(&record(&doc));
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
    let out = outgoing(&mut a, &doc);
    assert!(out.url.starts_with("https://api.example.com/v2/users"));
    assert!(out.url.contains("page=1"));
    assert!(out.url.contains("per_page=25"));
    assert!(!out.url.contains("sort="), "a disabled row is not sent");

    // The URL the person typed is what is sent, not the model's copy.
    let url = Workspace::paths().request.url;
    submit(&mut a, &mut doc, url.replace("{{base_url}}/typed"));
    assert!(outgoing(&mut a, &doc).url.contains("/v2/typed"));

    push(&a, Cmd::LoadRequest(0, 2));
    assert!(drain(&mut a, &mut doc));
    assert_eq!(a.request.method, HttpMethod::Post);
    assert_eq!(a.request.body_mode, BodyMode::Json);
    assert_eq!(a.selected, Some((0, 2)));
    let stored = record(&doc);
    assert_eq!(stored.request.url, "{{base_url}}/users");
    assert_eq!(stored.request.method, HttpMethod::Post);
    assert!(stored.request.body.contains("Grace Hopper"));
    assert_eq!(a.active_tab, RequestTab::Params);
}

/// Replaying a history entry opens it in the builder without sending it.
#[test]
fn replaying_history_fills_the_builder_without_sending() {
    let (mut a, mut doc) = demo();
    push(&a, Cmd::ReplayHistory(1));
    drain(&mut a, &mut doc);
    assert_eq!(a.request.method, HttpMethod::Post);
    assert_eq!(record(&doc).request.url, "https://api.example.com/v2/users");
    assert!(!a.sending);
    assert_eq!(a.selected, None);
}

/// The environment picker is wired through the action host: selecting a new
/// environment submits exactly the field edit of the active environment an
/// agent would, and the choice lands in the shared document. Its commands
/// open the sheets that manage environments and are not kept as a value.
#[test]
fn selecting_environment_dispatches_the_set_active_action() {
    let (mut a, mut doc) = demo();
    assert!(a.environments.len() >= 2, "demo seeds several environments");
    let target = a.environments[1].clone();
    assert_ne!(
        a.active_env.as_ref(),
        Some(&target.id),
        "target isn't already active"
    );

    push(&a, Cmd::PickEnvironment(target.name.clone()));
    drain(&mut a, &mut doc);

    assert_eq!(
        a.active_env.as_ref(),
        Some(&target.id),
        "the active environment switched"
    );
    let dispatched = LAST
        .with(|last| last.borrow().clone())
        .expect("the picker submitted a field edit");
    assert_eq!(
        dispatched,
        as_applied(
            &Workspace::paths()
                .active_environment
                .set(Some(target.id.clone()))
        ),
        "the picker submits what an agent would"
    );
    assert_eq!(
        record(&doc).active_environment.as_ref(),
        Some(&target.id),
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
    drain(&mut a, &mut doc);
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
    let active = a
        .active_env
        .clone()
        .expect("demo has an active environment");
    a.open_form(FormMode::RenameEnvironment, FormTarget::default());
    a.form
        .as_ref()
        .expect("sheet open")
        .fields
        .name
        .set("Live".into());
    push(&a, Cmd::SubmitForm);
    drain(&mut a, &mut doc);
    resync(&mut a, &doc);
    assert_eq!(a.active_env.as_ref(), Some(&active));
    assert_eq!(a.env_state.get(), "Live");

    push(
        &a,
        Cmd::PickEnvironment("Delete environment\u{2026}".into()),
    );
    drain(&mut a, &mut doc);
    resync(&mut a, &doc);
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
    assert!(drain(&mut a, &mut doc));
    assert!(
        record(&doc)
            .collections
            .values()
            .any(|collection| collection.name == "Billing API"),
        "New collection submitted its verb"
    );
    assert!(a.form.is_none(), "the sheet closes on submit");

    a.open_form(FormMode::NewVariable, FormTarget::default());
    let fields = a.form.as_ref().expect("sheet open").fields.clone();
    fields.key.set("region".into());
    fields.value.set("eu-west-1".into());
    push(&a, Cmd::SubmitForm);
    drain(&mut a, &mut doc);
    let active = a
        .active_env
        .clone()
        .expect("demo has an active environment");
    assert!(
        record(&doc)
            .environments
            .get(&active)
            .is_some_and(|environment| environment
                .variables
                .values()
                .any(|variable| variable.key == "region" && variable.value == "eu-west-1")),
        "Add variable submitted its edit"
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
        (collection.id.clone(), request.id.clone())
    };
    a.open_form(
        FormMode::RenameRequest,
        FormTarget {
            collection: Some(collection.clone()),
            request: Some(request.clone()),
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
    drain(&mut a, &mut doc);
    let names: Vec<&str> = a
        .collections
        .iter()
        .flat_map(|collection| collection.requests.iter().map(|r| r.name.as_str()))
        .collect();
    assert!(names.contains(&"Fetch user") && names.contains(&"List users"));
    assert!(!names.contains(&"Get user by id"));

    // Cancelling changes nothing.
    let heads = doc.get_heads();
    a.open_form(FormMode::RenameRequest, FormTarget::default());
    push(&a, Cmd::CancelForm);
    drain(&mut a, &mut doc);
    assert!(a.form.is_none());
    assert_eq!(doc.get_heads(), heads);

    // Deleting from a row removes it (once the host has the person's
    // consent, which resolving here stands for) and keeps the builder's
    // selection. No sheet of the window's own asks first.
    push(
        &a,
        Cmd::DeleteRequest(FormTarget {
            collection: Some(collection),
            request: Some(request),
        }),
    );
    drain(&mut a, &mut doc);
    assert!(a.form.is_none(), "the host asks, not the window");
    let remaining: usize = a.collections.iter().map(|c| c.requests.len()).sum();
    assert_eq!(remaining, 6, "one of the seven demo requests is gone");
    let (ci, ri) = a.selected.expect("the builder's request is still selected");
    assert_eq!(a.collections[ci].requests[ri].name, "List users");
}

/// Saving names the request, files it in a collection and selects it.
#[test]
fn saving_a_request_files_it_and_selects_it() {
    let (mut a, mut doc) = demo();
    submit(&mut a, &mut doc, Workspace::NEW_REQUEST.with(()));
    let paths = Workspace::paths().request;
    submit(&mut a, &mut doc, paths.name.replace("Health"));
    submit(&mut a, &mut doc, paths.url.replace("{{base_url}}/health"));
    resync(&mut a, &doc);
    a.open_form(FormMode::SaveRequest, FormTarget::default());
    push(&a, Cmd::SubmitForm);
    drain(&mut a, &mut doc);
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
    assert!(drain(&mut a, &mut doc));
    assert_eq!(a.active_tab, RequestTab::Headers);
    assert_eq!(a.response_view, ResponseView::Headers);
    assert_eq!(
        doc.get_heads(),
        heads,
        "view navigation changes no document"
    );
    push(&a, Cmd::SetBodyMode(1));
    push(&a, Cmd::SetAuthKind(2));
    assert!(drain(&mut a, &mut doc));
    assert_eq!(a.request.body_mode, BodyMode::Json);
    assert!(matches!(a.request.auth, Auth::Basic { .. }));
    let stored = record(&doc);
    assert_eq!(stored.request.body_mode, BodyMode::Json);
    assert_eq!(stored.request.auth.scheme, crate::record::AuthScheme::Basic);
    assert_ne!(
        doc.get_heads(),
        heads,
        "body and authentication modes are shared draft edits"
    );
    assert!(a.stale.tab_panel && a.stale.response_body && a.stale.body_inner);
    resync(&mut a, &doc);
    assert!(!a.stale.tab_panel && !a.stale.response_body && !a.stale.body_inner);
}

/// Typing in the blank row makes it a pair and gives the table a new blank
/// row; edits, the checkbox and remove act on the pair by id.
#[test]
fn key_value_tables_edit_the_request() {
    let (mut a, mut doc) = demo();
    let (table, _) = a.table.clone().expect("the Params table is showing");
    assert_eq!(table, Table::Params);
    let count = a.request.params.len();
    let blank = a.blanks.params.clone();

    let params = Table::Params.rows();
    let blank_id = Id::from_raw(blank.clone());
    a.submit(params.insert_as(&blank_id, count, &crate::record::Row::new("limit", "")));
    drain(&mut a, &mut doc);
    assert_eq!(a.request.params.len(), count + 1);
    let pair = a.request.params.last().expect("the new pair");
    assert_eq!(
        (pair.id.as_str(), pair.key.as_str()),
        (blank.as_str(), "limit")
    );
    assert_ne!(a.blanks.params, blank, "the table gets a fresh blank row");
    resync(&mut a, &doc);
    assert_eq!(a.request.params[count].id, blank);
    assert!(
        a.request.params.iter().all(|row| row.id != a.blanks.params),
        "the activated pair and appended blank have distinct identities"
    );

    // Further typing edits the same pair; it does not append again.
    a.submit(params.item(&blank_id).value.replace("10"));
    drain(&mut a, &mut doc);
    assert_eq!(a.request.params.len(), count + 1);
    assert_eq!(a.request.params[count].value, "10");

    a.submit(params.item(&blank_id).enabled.set(false));
    drain(&mut a, &mut doc);
    assert!(!a.request.params[count].enabled);
    assert!(
        !outgoing(&mut a, &doc).url.contains("limit="),
        "an unchecked row is not sent"
    );

    // A field of a row that is not there yet changes nothing.
    let fresh = Id::from_raw(a.blanks.params.clone());
    a.submit(params.item(&fresh).key.replace(""));
    drain(&mut a, &mut doc);
    assert_eq!(a.request.params.len(), count + 1);

    push(
        &a,
        Cmd::Edit {
            table: Table::Params,
            row: blank,
            edit: RowEdit::Remove,
        },
    );
    drain(&mut a, &mut doc);
    assert_eq!(a.request.params.len(), count);
}

/// The auth tab edits the scheme the request is composed with; its
/// credential stays on the device.
#[test]
fn auth_fields_edit_the_request() {
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    let auth = Workspace::paths().request.auth;
    app.invoke(Invoke::edit(&auth.scheme.set(crate::record::AuthScheme::Basic)).unwrap())
        .unwrap();
    app.invoke(Invoke::edit(&auth.username.replace("ada")).unwrap())
        .unwrap();
    app.effect(&ApiTesterEffect::SetCredential {
        secret: "secret".into(),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        app.with_surface::<ApiTesterApp, _>(|surface| surface.request.auth.clone())
            .unwrap(),
        Auth::Basic {
            username: "ada".into(),
            password: "secret".into()
        }
    );
    assert_eq!(
        app.state().request.auth.shown(),
        Auth::Basic {
            username: "ada".into(),
            password: String::new()
        },
        "the record holds the auth scheme and username, with no credential"
    );
    let first_reference = app.state().request.auth.reference;
    assert!(first_reference.starts_with("request-auth/"));
    let saved = app.read_document(|doc| doc.clone().save());
    assert!(!saved.windows(6).any(|window| window == b"secret"));
    app.effect(&ApiTesterEffect::SetCredential {
        secret: "replacement-credential".into(),
    })
    .unwrap();
    app.settle();
    assert_ne!(app.state().request.auth.reference, first_reference);
    let saved = app.read_document(|doc| doc.clone().save());
    assert!(
        !saved
            .windows(22)
            .any(|window| window == b"replacement-credential")
    );
    app.invoke(Invoke::edit(&auth.scheme.set(crate::record::AuthScheme::ApiKey)).unwrap())
        .unwrap();
    app.invoke(Invoke::edit(&auth.header.replace("X-Api-Key")).unwrap())
        .unwrap();
    app.settle();
    assert!(
        matches!(app.state().request.auth.shown(), Auth::ApiKey { ref header, .. } if header == "X-Api-Key")
    );
}

#[test]
fn actual_form_capture_settles_on_the_executor_clock_without_wall_time() {
    let pumped = ui::background::Pumped::new();
    let _entered = pumped.background().enter();
    let mut app =
        TestApp::<ApiTesterShell>::capture_at(Some("form-new-variable"), (1280, 800)).unwrap();
    assert!(
        !app.harness().frame_schedule().needs_animation_frame,
        "the actual form capture must settle even when CPU rendering consumes no toolkit time: {:?}",
        app.harness().frame_requesters()
    );
}

/// Every named `--shots` scenario stages, settles, renders, and leaves an
/// accessibility tree with no findings.
#[test]
fn every_scenario_stages_and_renders_an_accessible_frame() {
    let mut default = TestApp::<ApiTesterShell>::capture(None).unwrap();
    assert!(
        default
            .harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the default gallery capture settles its populated navigation; requesting frames: {:?}",
        default.harness().frame_requesters()
    );
    for scenario in ApiTesterApp::SCENARIOS {
        let size = if scenario.contains("drawer") {
            (520, 800)
        } else {
            (1280, 800)
        };
        let mut app = TestApp::<ApiTesterShell>::capture_at(Some(scenario), size).unwrap();
        let d = app.harness();
        assert!(
            d.settle_for_capture(tilo_test_env::hang_bound()),
            "{scenario}: the actual capture settles its navigation headings and rows; requesting frames: {:?}",
            d.frame_requesters()
        );
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

/// Open `request` in the builder of `app`, as a new request.
fn open(app: &mut TestApp<ApiTesterShell>, request: &Request) {
    app.invoke(Workspace::NEW_REQUEST.with(())).unwrap();
    let edits = Workspace::paths().request.path().edits(
        &app.state().request,
        &crate::record::RequestRecord::of(request),
    );
    for edit in edits {
        app.invoke(Invoke::edit(&edit).unwrap()).unwrap();
    }
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
    let (release_response, response_ready) = std::sync::mpsc::channel();
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
        response_ready
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the admitted pending job is released by its caller");
        let body = "{\"result\":\"Grün\"}";
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });

    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    let mut request = Request::new("HTTP proof");
    request.url = format!("http://{address}/probe");
    request.params.push(KeyValue::new("name", "Grün"));
    request
        .headers
        .push(KeyValue::new("X-Tilo-Probe", "native"));
    open(&mut app, &request);
    assert!(app.state().history.is_empty());
    let admitted = app.start_effect(&ApiTesterEffect::Send).unwrap();
    assert!(
        !app.effect_progress().iter().any(|update| matches!(
            update,
            ui::tilo_actions::InvocationUpdate::Succeeded { .. }
                | ui::tilo_actions::InvocationUpdate::Failed { .. }
                | ui::tilo_actions::InvocationUpdate::Cancelled { .. }
        )),
        "admitted HTTP invocation {admitted:?} has not completed at queue time"
    );
    assert!(
        app.with_surface::<ApiTesterApp, _>(|surface| surface.sending)
            .unwrap(),
        "the backend has not answered the displayed HTTP job"
    );
    assert!(
        app.state().history.is_empty(),
        "admission is not a completed request record"
    );
    // A later edit cannot change the history record of the request already sent.
    let builder = Workspace::paths().request;
    app.invoke(Invoke::edit(&builder.url.replace("http://127.0.0.1/edited-after-send")).unwrap())
        .unwrap();
    app.invoke(Invoke::edit(&builder.method.set(HttpMethod::Post)).unwrap())
        .unwrap();
    release_response
        .send(())
        .expect("release the held HTTP backend");
    tilo_test_env::wait_until("the HTTP worker to answer the request", || {
        app.settle();
        app.effect_progress().iter().any(|update| {
            matches!(
                update,
                ui::tilo_actions::InvocationUpdate::Succeeded { .. }
                    | ui::tilo_actions::InvocationUpdate::Failed { .. }
                    | ui::tilo_actions::InvocationUpdate::Cancelled { .. }
            )
        })
    });
    let updates = app.effect_progress();
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. })),
        "admitted HTTP invocation {admitted:?} must succeed after the held backend replies: {updates:?}"
    );
    server.join().unwrap();
    let response = app
        .with_surface::<ApiTesterApp, _>(|surface| surface.response.clone())
        .unwrap()
        .expect("the response is displayed");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "{\"result\":\"Grün\"}");
    let state = app.state();
    let history = state.history_shown();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, 200);
    assert_eq!(history[0].method, HttpMethod::Get);
    assert_eq!(
        history[0].url,
        format!("http://{address}/probe?name=Gr%C3%BCn")
    );
    assert!(
        !app.with_surface::<ApiTesterApp, _>(|surface| surface.sending)
            .unwrap()
    );
    for _ in 0..3 {
        app.settle();
    }
    assert_eq!(
        app.effect_progress()
            .iter()
            .filter(|update| matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. }))
            .count(),
        1,
        "terminal delivery is at most once"
    );
    let restarted = app.reopen().unwrap();
    assert_eq!(
        restarted.state().history,
        state.history,
        "completed requests survive restart"
    );
    assert!(
        restarted
            .state()
            .history
            .values()
            .all(|sent| !sent.route.0.contains("Gr%C3%BCn")),
        "a sent request is named without its query values"
    );
}

/// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
fn resource_send_reports_worker_completion_instead_of_queue_success() {
    // The real hosted HTTP scenario asserts admission, worker completion and exactly one terminal delivery.
    resource_send_performs_a_real_http_round_trip();
}

/// A sheet that asks for one name is as short as that name's field.
#[test]
fn the_new_environment_sheet_is_as_tall_as_its_one_field() {
    let mut app =
        TestApp::<ApiTesterShell>::capture_at(Some("form-new-environment"), (1280, 800)).unwrap();
    app.settle();
    let targets = app.harness().targets();
    let name = targets["api.form-name"];
    let sheet = targets
        .get("api.form")
        .copied()
        .expect("the sheet is a named target");
    assert!(
        sheet.height() < 320.0,
        "the sheet {sheet:?} around the name field {name:?} does not fill the window"
    );
}

/// On a wide screen a parameter's value stays beside its key.
#[test]
fn a_wide_window_keeps_each_value_beside_its_key() {
    let mut app = TestApp::<ApiTesterShell>::capture_at(None, (2560, 1440)).unwrap();
    app.settle();
    let targets = app.harness().targets();
    let field = |suffix: &str| {
        targets
            .iter()
            .find(|(id, _)| id.starts_with("api.kv.Params.") && id.ends_with(suffix))
            .map_or_else(
                || panic!("the blank parameter row's {suffix} field"),
                |(_, rect)| *rect,
            )
    };
    let (key, value) = (field(".key"), field(".value"));
    let readable = f64::from(Theme::dark().tokens.metrics.readable_width);
    assert!(
        value.x1 - key.x0 <= readable + 1.0,
        "key {key:?} and value {value:?} span no more than the readable measure"
    );
}

/// Send with no URL says what is missing instead of doing nothing (or, in a
/// browser, fetching the page itself).
#[test]
fn send_without_a_url_asks_for_one() {
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    let mut request = Request::new("No address");
    request.url = "  ".into();
    request.params.push(KeyValue::new("q", "1"));
    open(&mut app, &request);
    let _ = app.start_effect(&ApiTesterEffect::Send);
    app.settle();
    app.with_surface::<ApiTesterApp, _>(|surface| {
        assert!(!surface.sending, "nothing was sent");
        assert!(surface.response.is_none());
        assert_eq!(
            surface.error.as_deref(),
            Some("Enter a URL to send the request to.")
        );
        assert!(!surface.stale.response, "the pane shows it");
    })
    .unwrap();
    assert!(app.state().history.is_empty());
}

/// A failed request shows the failure once, in the response pane.
#[test]

fn a_failed_request_is_shown_in_the_response_pane() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    let mut request = Request::new("Failure proof");
    request.url = format!("http://{address}/unreachable");
    open(&mut app, &request);
    app.start_effect(&ApiTesterEffect::Send).unwrap();
    tilo_test_env::wait_until("the failed HTTP worker", || {
        app.settle();
        app.effect_progress()
            .iter()
            .any(|update| matches!(update, ui::tilo_actions::InvocationUpdate::Failed { .. }))
    });
    let failure = app
        .effect_progress()
        .into_iter()
        .find_map(|update| match update {
            ui::tilo_actions::InvocationUpdate::Failed { error, .. } => Some(error.message),
            _ => None,
        })
        .expect("the worker reported why it failed");
    app.with_surface::<ApiTesterApp, _>(|surface| {
        assert!(surface.response.is_none());
        assert_eq!(surface.error.as_deref(), Some(failure.as_str()));
        assert!(!surface.stale.response, "the pane was rebuilt");
    })
    .unwrap();
}

/// Case of `resource_provider_sends_for_real_and_resolves_exact_records`.
fn resource_provider_resolves_a_late_record_without_scanning_the_workspace() {
    let mut workspace = Replica::<Workspace>::new();
    let mut requests = ui::tilo_resource::Collection::new();
    for index in 0..5_000 {
        requests.push(
            crate::model::fresh_id(),
            crate::record::RequestRecord::named(format!("Request {index}")),
        );
    }
    let collection = crate::model::fresh_id();
    let saved = crate::record::SavedCollection {
        name: "Load test".into(),
        requests,
    };
    workspace
        .apply(
            Invoke::edit(
                &Workspace::paths()
                    .collections
                    .insert_as(&collection, 0, &saved),
            )
            .unwrap(),
        )
        .unwrap();
    let state = workspace.state().unwrap();
    assert_eq!(state.collections.len(), 1);
    let id = ui::tilo_resource::items::item_resource_id::<Workspace>("collections", &collection);
    let resource = Workspace::child(&state, &id).expect("the collection resolves by its id");
    assert_eq!(resource.id.0, id);
    assert!(
        resource
            .actions
            .iter()
            .any(|action| action.id == "save-request"),
        "the collection offers its verbs"
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
        resync(&mut app, &doc);
        assert_eq!((names(&app), collections(&app)), before);
    }
    let first = app.environments[0].id.clone();
    let second = app.environments[1].id.clone();
    app.pick_environment("Staging");
    drain(&mut app, &mut doc);
    assert_eq!(app.active_env.as_ref(), Some(&second));
    assert_ne!(app.active_env.as_ref(), Some(&first));
    assert_eq!((names(&app), collections(&app)), before);
}

/// The app's retained shared navigation paints long saved-request and history
/// labels within its measured row, including at twice the text size.
#[test]
fn long_navigation_labels_fit_the_shared_rows() {
    fn find(widget: &mut dyn Widget, theme: &Theme, text: &mut ui::TextRenderer) -> usize {
        if widget.test_id() == Some("api.nav") {
            let rect = widget.get_rect();
            let mut scene = vello::Scene::new();
            widget.draw(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                text,
                theme,
                &ui::presence::PresenceCtx::default(),
            );
            let mut glyphs = 0;
            for run in &scene.encoding().resources.glyph_runs {
                for glyph in scene
                    .encoding()
                    .resources
                    .glyphs
                    .get(run.glyphs.clone())
                    .expect("painted run has valid glyph range")
                {
                    let y = f64::from(run.transform.translation[1] + glyph.y);
                    assert!(
                        y >= rect.y0 && y < rect.y1,
                        "navigation glyph {y} escapes {rect:?}"
                    );
                    glyphs += 1;
                }
            }
            assert!(glyphs > 0, "the actual retained list paints text");
            return 1;
        }
        widget
            .children_mut()
            .into_iter()
            .map(|child| find(child, theme, text))
            .sum()
    }
    let long = "a very long saved request or history path with Unicode café and many more words";
    for scale in [1.0, 2.0] {
        let mut app = TestApp::<ApiTesterShell>::capture(None).unwrap();
        let workspace = app.state();
        let (collection, saved) = workspace
            .collections
            .iter()
            .next()
            .expect("sample collection");
        let request = saved.requests.ids().next().expect("sample request");
        app.invoke(
            Invoke::edit(
                &Workspace::paths()
                    .collections
                    .item(collection)
                    .requests
                    .item(request)
                    .name
                    .replace(long),
            )
            .unwrap(),
        )
        .unwrap();
        app.invoke(Workspace::RECORD_SENT.with(verbs::RecordSent {
            method: "GET".into(),
            url: format!("https://example.invalid/{long}"),
            status: 200,
            duration_ms: 12,
        }))
        .unwrap();
        let theme = Theme::dark().with_text_scale(scale);
        app.harness().set_theme(theme.clone());
        app.with_surface::<ApiTesterApp, _>(|surface| {
            // At large text this viewport uses a push sidebar. Review the
            // actual visible navigation pane rather than hidden content.
            surface.nav.show(ui::widget::NavPane::Sidebar);
            surface.nav.open_sidebar();
            surface.nav.snap();
        })
        .unwrap();
        app.harness().relayout();
        app.settle();
        assert!(
            app.harness().inspection_json().unwrap().contains(long),
            "long labels remain semantically complete: {}",
            app.harness().inspection_json().unwrap()
        );
        let mut text = ui::TextRenderer::new();
        text.set_text_scale(scale);
        let lists = app
            .with_surface::<ApiTesterApp, _>(|surface| find(surface, &theme, &mut text))
            .unwrap();
        assert!(lists > 0, "the production view contains shared navigation");
    }
}
