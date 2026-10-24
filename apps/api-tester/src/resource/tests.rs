//! The workspace on its own (verbs, field edits, the invariants, merging,
//! the published tree and search) through the door the host uses, and an
//! agent and history through the hosted app.

use super::*;
use crate::model::BodyMode;
use crate::record::{AuthScheme, Row};
use ui::framework::testing::TestApp;
use ui::tilo_actions::Resource as Published;
use ui::tilo_resource::{Edit, Replica, Resource as _};

fn edit(workspace: &mut Replica<Workspace>, edit: &Edit<Workspace>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    workspace
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A workspace with one collection holding one request and one environment
/// holding one variable, built through the verbs.
fn seeded() -> Replica<Workspace> {
    let mut workspace = Replica::<Workspace>::new();
    workspace
        .apply(Workspace::NEW_COLLECTION.with("Users API"))
        .unwrap();
    let collection = collection_id(&workspace);
    workspace
        .apply(Workspace::ADD_REQUEST.with(workspace::AddRequest {
            collection,
            name: "List users".into(),
        }))
        .unwrap();
    workspace
        .apply(Workspace::NEW_ENVIRONMENT.with("Production"))
        .unwrap();
    let environment = environment_id(&workspace);
    workspace
        .apply(Workspace::ADD_VARIABLE.with(workspace::AddVariable {
            environment,
            key: "base_url".into(),
            value: Some("https://api.example.com".into()),
            secret: None,
        }))
        .unwrap();
    workspace
}

fn collection_id(workspace: &Replica<Workspace>) -> Id {
    workspace
        .state()
        .unwrap()
        .collections
        .ids()
        .next()
        .unwrap()
        .clone()
}

fn request_id(workspace: &Replica<Workspace>) -> Id {
    let state = workspace.state().unwrap();
    let (_, collection) = state.collections.iter().next().unwrap();
    collection.requests.ids().next().unwrap().clone()
}

fn environment_id(workspace: &Replica<Workspace>) -> Id {
    workspace
        .state()
        .unwrap()
        .environments
        .ids()
        .next()
        .unwrap()
        .clone()
}

fn child<'a>(tree: &'a Published, id: &str) -> &'a Published {
    tree.children
        .iter()
        .find(|child| child.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is published"))
}

fn offers(resource: &Published, id: &str) -> bool {
    resource.actions.iter().any(|spec| spec.id == id)
}

/// Collections, saved requests and environments are made, opened, saved
/// and removed by verbs; names change by field edits.
#[test]
fn verbs_make_open_save_and_remove_records() {
    let mut workspace = seeded();
    let collection = collection_id(&workspace);
    let request = request_id(&workspace);
    let paths = Workspace::paths();

    edit(
        &mut workspace,
        &paths
            .collections
            .item(&collection)
            .name
            .replace("Accounts API"),
    )
    .unwrap();
    workspace
        .apply(Workspace::OPEN_REQUEST.with(workspace::OpenRequest {
            collection: collection.clone(),
            request: request.clone(),
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    assert_eq!(state.request.name, "List users");
    assert_eq!(
        state.opened(),
        request,
        "the builder holds the saved request"
    );

    // Saving a request the builder opened from the collection saves over it.
    edit(&mut workspace, &paths.request.method.set(HttpMethod::Put)).unwrap();
    workspace
        .apply(Workspace::SAVE_REQUEST.with(workspace::SaveRequest {
            collection: collection.clone(),
            name: None,
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    let saved = &state.collections.get(&collection).unwrap().requests;
    assert_eq!(saved.len(), 1, "same request replaces, not appends");
    assert_eq!(saved.get(&request).unwrap().method, HttpMethod::Put);

    // A new request is saved under its own id, with the name given.
    workspace.apply(Workspace::NEW_REQUEST.with(())).unwrap();
    edit(&mut workspace, &paths.request.url.replace("https://x.test")).unwrap();
    workspace
        .apply(Workspace::SAVE_REQUEST.with(workspace::SaveRequest {
            collection: collection.clone(),
            name: Some(" Saved copy ".into()),
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    let saved = &state.collections.get(&collection).unwrap().requests;
    assert_eq!(saved.len(), 2);
    let copy = saved
        .get(&state.opened())
        .expect("the builder holds the copy");
    assert_eq!(copy.name, "Saved copy");
    assert_eq!(copy.url, "https://x.test");

    let delete = || {
        Workspace::DELETE_REQUEST.with(workspace::DeleteRequest {
            collection: collection.clone(),
            request: request.clone(),
        })
    };
    // Undo takes a deletion back, so it runs at once.
    workspace
        .apply(delete())
        .expect("an undoable delete runs at once");
    assert!(
        workspace
            .apply(Workspace::OPEN_REQUEST.with(workspace::OpenRequest {
                collection: collection.clone(),
                request,
            }))
            .is_err(),
        "a deleted request is not opened"
    );

    let environment = environment_id(&workspace);
    workspace
        .apply(Workspace::USE_ENVIRONMENT.with(environment.clone()))
        .unwrap();
    assert_eq!(
        workspace.state().unwrap().active_environment,
        Some(environment.clone())
    );
    workspace
        .apply_confirmed(Workspace::DELETE_ENVIRONMENT.with(environment))
        .unwrap();
    assert_eq!(
        workspace.state().unwrap().active_environment,
        None,
        "deleting the active environment leaves none active"
    );
    workspace
        .apply_confirmed(Workspace::DELETE_COLLECTION.with(collection))
        .unwrap();
    assert!(workspace.state().unwrap().collections.is_empty());
}

#[test]
fn field_edits_change_the_builder_and_rows_in_place() {
    let mut workspace = seeded();
    let request = Workspace::paths().request;
    edit(&mut workspace, &request.method.set(HttpMethod::Delete)).unwrap();
    edit(&mut workspace, &request.url.splice(0, 0, "https://x.test")).unwrap();
    edit(&mut workspace, &request.body_mode.set(BodyMode::Json)).unwrap();
    let row = Id::from_raw("page");
    edit(
        &mut workspace,
        &request.params.insert_as(&row, 0, &Row::new("page", "1")),
    )
    .unwrap();
    let params = request.params.item(&row);
    edit(&mut workspace, &params.key.replace("limit")).unwrap();
    assert_eq!(
        workspace
            .state()
            .unwrap()
            .request
            .params
            .get(&row)
            .unwrap()
            .value,
        "1",
        "editing the key keeps the value"
    );
    edit(&mut workspace, &params.value.replace("50")).unwrap();
    edit(&mut workspace, &params.enabled.set(false)).unwrap();
    let state = workspace.state().unwrap();
    assert_eq!(state.request.method, HttpMethod::Delete);
    assert_eq!(state.request.url, "https://x.test");
    assert_eq!(state.request.body_mode, BodyMode::Json);
    assert_eq!(
        state.request.params.get(&row),
        Some(&Row {
            key: "limit".into(),
            value: "50".into(),
            enabled: false,
        })
    );
    edit(&mut workspace, &request.params.remove(&row)).unwrap();
    assert!(workspace.state().unwrap().request.params.is_empty());

    // A variable's key and value are separate fields.
    let environment = environment_id(&workspace);
    let state = workspace.state().unwrap();
    let variable = state
        .environments
        .get(&environment)
        .unwrap()
        .variables
        .ids()
        .next()
        .unwrap()
        .clone();
    let variable_path = Workspace::paths()
        .environments
        .item(&environment)
        .variables
        .item(&variable);
    edit(&mut workspace, &variable_path.key.replace("api_url")).unwrap();
    let state = workspace.state().unwrap();
    let renamed = state
        .environments
        .get(&environment)
        .unwrap()
        .variables
        .get(&variable)
        .unwrap();
    assert_eq!(renamed.key, "api_url");
    assert_eq!(renamed.value, "https://api.example.com");
}

/// What was sent is history, newest first, bounded, and opens again as a
/// new request; only sending adds to it.
#[test]
fn sent_requests_are_history_that_opens_again() {
    let mut workspace = seeded();
    for status in 0..(HISTORY_LIMIT as u16 + 3) {
        workspace
            .apply(Workspace::RECORD_SENT.with(workspace::RecordSent {
                method: "PATCH".into(),
                url: format!("https://user:pw@api.example.com/users/{status}?token=x"),
                status,
                duration_ms: 3,
            }))
            .unwrap();
    }
    let state = workspace.state().unwrap();
    assert_eq!(state.history.len(), HISTORY_LIMIT, "the history is bounded");
    let (newest, sent) = state.history.iter().next().unwrap();
    assert_eq!(sent.status, HISTORY_LIMIT as u16 + 2, "newest first");
    assert_eq!(
        sent.route.0,
        format!(
            "PATCH https://api.example.com/users/{} returned status {}",
            sent.status, sent.status
        ),
        "the route leaves out the credential and the query"
    );
    assert!(
        edit(
            &mut workspace,
            &Workspace::paths().history.remove(&newest.clone())
        )
        .is_err(),
        "only sending changes the history"
    );
    workspace
        .apply(Workspace::OPEN_SENT.with(newest.clone()))
        .unwrap();
    let state = workspace.state().unwrap();
    assert_eq!(state.request.method, HttpMethod::Patch);
    assert_eq!(state.request.url, sent.url.0);
    assert!(
        state.saved(&state.opened()).is_none(),
        "it opens as a new request"
    );
}

/// A credential never reaches the record: a request's auth keeps its scheme
/// and public fields, a secret variable carries no value.
#[test]
fn credentials_never_enter_the_record() {
    let mut workspace = seeded();
    let auth = Workspace::paths().request.auth;
    edit(&mut workspace, &auth.scheme.set(AuthScheme::Basic)).unwrap();
    edit(&mut workspace, &auth.username.replace("ada")).unwrap();
    let environment = environment_id(&workspace);
    workspace
        .apply(Workspace::ADD_VARIABLE.with(workspace::AddVariable {
            environment: environment.clone(),
            key: "token".into(),
            value: Some("sk-live-variable".into()),
            secret: Some(true),
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    let (token, variable) = state
        .environments
        .get(&environment)
        .unwrap()
        .variables
        .iter()
        .find(|(_, variable)| variable.key == "token")
        .map(|(id, variable)| (id.clone(), variable.clone()))
        .unwrap();
    assert!(variable.secret && variable.value.is_empty());
    let value = Workspace::paths()
        .environments
        .item(&environment)
        .variables
        .item(&token)
        .value;
    assert!(
        edit(&mut workspace, &value.replace("sk-live-edit")).is_err(),
        "a secret variable's value is refused whole"
    );
    let saved = String::from_utf8_lossy(&workspace.save()).to_string();
    for secret in ["sk-live-variable", "sk-live-edit"] {
        assert!(!saved.contains(secret), "{secret} reached the record");
    }
    assert_eq!(
        workspace.state().unwrap().request.auth.shown(),
        crate::model::Auth::Basic {
            username: "ada".into(),
            password: String::new()
        }
    );
}

#[test]
fn the_active_environment_names_an_environment() {
    let mut workspace = seeded();
    assert!(
        edit(
            &mut workspace,
            &Workspace::paths()
                .active_environment
                .set(Some(Id::from_raw("nowhere")))
        )
        .is_err()
    );
    let environment = environment_id(&workspace);
    edit(
        &mut workspace,
        &Workspace::paths().active_environment.set(Some(environment)),
    )
    .unwrap();
}

/// Two people editing one workspace converge: the URL merges per
/// character, rows per row.
#[test]
fn concurrent_edits_converge() {
    let mut ours = seeded();
    let request = Workspace::paths().request;
    edit(&mut ours, &request.url.replace("https://api.test/users")).unwrap();
    let mut theirs = ours.fork();
    edit(&mut theirs, &request.url.splice(22, 0, "/7")).unwrap();
    edit(
        &mut theirs,
        &request
            .headers
            .insert_as(&Id::from_raw("accept"), 0, &Row::new("Accept", "*/*")),
    )
    .unwrap();
    edit(&mut ours, &request.url.splice(8, 3, "API")).unwrap();
    edit(
        &mut ours,
        &request
            .params
            .insert_as(&Id::from_raw("page"), 0, &Row::new("page", "2")),
    )
    .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let state = ours.state().unwrap();
    assert_eq!(state, theirs.state().unwrap());
    assert_eq!(state.request.url, "https://API.test/users/7");
    assert_eq!(state.request.headers.len(), 1);
    assert_eq!(state.request.params.len(), 1);
}

/// Each collection, environment and sent request is published with the
/// verbs that act on it; search finds records by name, never a value.
#[test]
fn records_offer_their_verbs_and_search_names_them() {
    let mut workspace = seeded();
    workspace
        .apply(Workspace::RECORD_SENT.with(workspace::RecordSent {
            method: "GET".into(),
            url: "https://api.example.com/users?token=do-not-index".into(),
            status: 200,
            duration_ms: 5,
        }))
        .unwrap();
    let tree = workspace.published().unwrap();
    for verb in [
        "new-request",
        "new-collection",
        "new-environment",
        "record-sent",
        "field-edit",
    ] {
        assert!(offers(&tree, verb), "the workspace offers {verb}");
    }
    let state = workspace.state().unwrap();
    let collection = child(
        &tree,
        &item_resource_id::<Workspace>("collections", &collection_id(&workspace)),
    );
    assert_eq!(collection.label, "Users API");
    for verb in [
        "add-request",
        "save-request",
        "open-request",
        "delete-request",
        "delete-collection",
    ] {
        assert!(offers(collection, verb), "a collection offers {verb}");
    }
    let environment = child(
        &tree,
        &item_resource_id::<Workspace>("environments", &environment_id(&workspace)),
    );
    for verb in ["use-environment", "add-variable", "delete-environment"] {
        assert!(offers(environment, verb), "an environment offers {verb}");
    }
    let (sent, _) = state.history.iter().next().unwrap();
    let sent = child(&tree, &item_resource_id::<Workspace>("history", sent));
    assert!(offers(sent, "open-sent"));
    assert!(!sent.label.contains("do-not-index"));

    let entries = Workspace::search(&state);
    for title in [
        "Users API",
        "GET List users",
        "Production",
        "Variable base_url",
        "GET https://api.example.com/users returned status 200",
    ] {
        assert!(
            entries.iter().any(|entry| entry.title == title),
            "{title} in {entries:?}"
        );
    }
    assert!(entries.iter().all(|entry| {
        !entry.title.contains("do-not-index")
            && !entry.text.contains("do-not-index")
            && !entry.text.contains("https://api.example.com")
    }));
}

/// An agent reaches the workspace through the one published tree: each
/// change is one call, a verb on the record it acts on or a field edit.
#[test]
fn an_agent_builds_requests_one_call_each() {
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    app.call("new-collection", serde_json::json!({ "name": "Billing" }))
        .unwrap();
    let state = app.state();
    let collection = state.collections.ids().next().unwrap().clone();
    let request = Workspace::paths().request;
    for edit in [
        request.url.replace("https://billing.test/invoices"),
        request.method.set(HttpMethod::Post),
        request.name.replace("Create invoice"),
    ] {
        app.call("field-edit", serde_json::json!({ "edit": edit }))
            .unwrap();
    }
    let item = item_resource_id::<Workspace>("collections", &collection);
    app.call_on(&item, "save-request", serde_json::json!({}))
        .unwrap();
    let state = app.state();
    let saved = &state.collections.get(&collection).unwrap().requests;
    assert_eq!(saved.len(), 1);
    let (_, invoice) = saved.iter().next().unwrap();
    assert_eq!(
        (invoice.name.as_str(), invoice.method, invoice.url.as_str()),
        (
            "Create invoice",
            HttpMethod::Post,
            "https://billing.test/invoices"
        )
    );
    assert!(app.undo(), "saving is one undo step");
    assert!(
        app.state()
            .collections
            .get(&collection)
            .unwrap()
            .requests
            .is_empty()
    );
}

/// What an agent does for a person is history the person reads: the app's
/// sentence for each field and verb.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Workspace::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "new-collection",
        serde_json::json!({ "name": "Billing" }),
    )
    .unwrap();
    let collection = app.state().collections.ids().next().unwrap().clone();
    let item = item_resource_id::<Workspace>("collections", &collection);
    let request = Workspace::paths().request;
    for edit in [
        request.url.replace("https://billing.test"),
        request.method.set(HttpMethod::Post),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": edit }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        &item,
        "save-request",
        serde_json::json!({ "name": "Invoices" }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "new-environment",
        serde_json::json!({ "name": "Staging" }),
    )
    .unwrap();
    let environment = app.state().environments.ids().next().unwrap().clone();
    app.call_by(
        agent.clone(),
        &item_resource_id::<Workspace>("environments", &environment),
        "use-environment",
        serde_json::json!({}),
    )
    .unwrap();
    // Undo takes a deletion back, so it runs at once.
    app.call_by(agent, &item, "delete-collection", serde_json::json!({}))
        .expect("an undoable delete runs at once");

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Created the collection Billing",
            "Edited the URL of a request",
            "Set the method to POST",
            "Saved a request to Billing",
            "Created the environment Staging",
            "Switched to the environment Staging",
            "Deleted the collection Billing",
        ]
    );
}

/// Deleting from the window asks nothing, of its own or through the host:
/// Undo takes the deletion back, so it runs at once and is one undo step.
#[test]
fn deleting_from_the_window_runs_at_once_and_undo_brings_it_back() {
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    for invoke in crate::seed::workspace() {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    let count = |app: &TestApp<ApiTesterShell>| -> usize {
        app.state()
            .collections
            .values()
            .map(|collection| collection.requests.len())
            .sum()
    };
    let before = count(&app);
    app.effect(&crate::actions::ApiTesterEffect::RequestMenu {
        action: "Delete request\u{2026}".into(),
    })
    .unwrap();
    app.settle();
    assert!(
        !app.harness()
            .inspection_json()
            .unwrap()
            .contains("Delete this request?"),
        "the window asks nothing of its own"
    );
    assert!(app.asking().is_none(), "nor does the host");
    assert_eq!(
        count(&app),
        before - 1,
        "the delete applied: {:?}",
        app.harness().control_failures()
    );
    assert!(app.undo(), "the delete is one undo step");
    assert_eq!(count(&app), before);
}
