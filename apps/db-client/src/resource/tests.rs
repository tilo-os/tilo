//! The workspace on its own (verbs, field edits, merging, the published
//! tree and search) through the door the host uses, and an agent and
//! history through the hosted app.

use super::*;
use crate::record::uuid_of;
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

fn add_analytics(workspace: &mut Replica<Workspace>) -> Id {
    workspace
        .apply(Workspace::ADD_CONNECTION.with(workspace::AddConnection {
            name: "Analytics".into(),
            engine: "postgres".into(),
            location: Some("db.internal".into()),
            database: Some("analytics".into()),
            user: Some("reader".into()),
        }))
        .unwrap();
    workspace
        .state()
        .unwrap()
        .connections
        .ids()
        .next_back()
        .unwrap()
        .clone()
}

fn child<'a>(tree: &'a Published, id: &str) -> &'a Published {
    tree.children
        .iter()
        .find(|child| child.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is published"))
}

/// Connections are added and removed by verbs and edited field by field;
/// a password never enters the record.
#[test]
fn connections_are_saved_without_passwords() {
    let mut workspace = Replica::<Workspace>::new();
    let id = add_analytics(&mut workspace);
    let profile = workspace.state().unwrap().profiles().remove(0);
    assert_eq!(
        profile.id,
        uuid_of(&id),
        "minted as a UUID the window keeps"
    );
    assert_eq!(
        (
            profile.name.as_str(),
            profile.engine,
            profile.host.as_str(),
            profile.port,
            profile.database.as_str(),
            profile.user.as_str()
        ),
        (
            "Analytics",
            Engine::Postgres,
            "db.internal",
            5432,
            "analytics",
            "reader"
        ),
        "a network engine keeps its default port"
    );
    assert!(
        workspace
            .apply(Workspace::ADD_CONNECTION.with(workspace::AddConnection {
                name: "Odd".into(),
                engine: "oracle".into(),
                location: None,
                database: None,
                user: None,
            }))
            .is_err(),
        "an engine the client cannot open is refused"
    );

    let paths = Workspace::paths().connections.item(&id);
    edit(&mut workspace, &paths.name.replace("Prod analytics")).unwrap();
    edit(&mut workspace, &paths.port.set(6543)).unwrap();
    let profile = workspace.state().unwrap().profiles().remove(0);
    assert_eq!(
        (profile.name.as_str(), profile.port),
        ("Prod analytics", 6543)
    );

    // The window adds a profile it typed a password for: only the shape
    // reaches the record.
    let mut typed = ConnectionProfile::new("Shop", Engine::MySql);
    typed.password = "s3cret".into();
    workspace.apply(crate::record::adding(&typed, 1)).unwrap();
    let saved = workspace.save();
    assert!(
        !saved.windows(6).any(|window| window == b"s3cret"),
        "the saved record does not contain the password"
    );
    assert!(
        workspace
            .state()
            .unwrap()
            .profiles()
            .iter()
            .all(|profile| profile.password.is_empty())
    );

    assert!(
        workspace
            .apply(Workspace::REMOVE_CONNECTION.with(id.clone()))
            .is_err(),
        "removing asks for confirmation"
    );
    workspace
        .apply_confirmed(Workspace::REMOVE_CONNECTION.with(id))
        .unwrap();
    let left = workspace.state().unwrap().profiles();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, typed.id, "the other profile survives removal");
}

/// Saved queries keep the SQL given or the query being written; an empty
/// buffer saves nothing.
#[test]
fn queries_are_saved_from_sql_or_the_buffer() {
    let mut workspace = Replica::<Workspace>::new();
    assert!(
        workspace
            .apply(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
                name: "Nothing".into(),
                sql: None,
            }))
            .is_err(),
        "there is no SQL to save"
    );
    edit(&mut workspace, &Workspace::paths().sql.replace("SELECT 1")).unwrap();
    workspace
        .apply(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
            name: " Current ".into(),
            sql: None,
        }))
        .unwrap();
    workspace
        .apply(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
            name: "Totals".into(),
            sql: Some("SELECT count(*) FROM orders".into()),
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    let saved: Vec<(&str, &str)> = state
        .saved_queries
        .values()
        .map(|query| (query.name.as_str(), query.sql.as_str()))
        .collect();
    assert_eq!(
        saved,
        [
            ("Current", "SELECT 1"),
            ("Totals", "SELECT count(*) FROM orders")
        ]
    );
    let (first, _) = state.saved_queries.iter().next().unwrap();
    workspace
        .apply_confirmed(Workspace::REMOVE_QUERY.with(first.clone()))
        .unwrap();
    assert_eq!(workspace.state().unwrap().saved_queries.len(), 1);
}

/// Two replicas of one workspace that each type SQL and save a query before
/// syncing keep both.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Workspace>::new();
    let mut theirs = ours.fork();
    edit(&mut ours, &Workspace::paths().sql.splice(0, 0, "left")).unwrap();
    edit(&mut theirs, &Workspace::paths().sql.splice(0, 0, "right")).unwrap();
    for (replica, name) in [(&mut ours, "L"), (&mut theirs, "R")] {
        replica
            .apply(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
                name: name.into(),
                sql: Some("SELECT 1".into()),
            }))
            .unwrap();
    }
    let id = add_analytics(&mut ours);
    theirs.sync_with(&mut ours).unwrap();
    edit(
        &mut theirs,
        &Workspace::paths()
            .connections
            .item(&id)
            .name
            .replace("Theirs"),
    )
    .unwrap();
    edit(
        &mut ours,
        &Workspace::paths().connections.item(&id).port.set(1),
    )
    .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let state = ours.state().unwrap();
    assert_eq!(state, theirs.state().unwrap());
    assert!(state.sql.contains("left") && state.sql.contains("right"));
    assert_eq!(state.saved_queries.len(), 2, "both saved queries survive");
    let profile = state.profiles().remove(0);
    assert_eq!((profile.name.as_str(), profile.port), ("Theirs", 1));
}

/// Each saved connection and query is published with the verbs that act
/// on it; search finds them by name, never by their SQL.
#[test]
fn records_offer_their_verbs_and_search_names_them() {
    let mut workspace = Replica::<Workspace>::new();
    let connection = add_analytics(&mut workspace);
    workspace
        .apply(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
            name: "Daily totals".into(),
            sql: Some("SELECT 'sk-token'".into()),
        }))
        .unwrap();
    let state = workspace.state().unwrap();
    let tree = workspace.published().unwrap();
    for verb in ["add-connection", "save-query", "field-edit"] {
        assert!(tree.actions.iter().any(|spec| spec.id == verb), "{verb}");
    }
    let item = child(
        &tree,
        &item_resource_id::<Workspace>("connections", &connection),
    );
    assert_eq!(item.label, "Analytics");
    for id in ["remove-connection", "connect"] {
        let spec = item
            .actions
            .iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("a connection offers {id}"));
        assert!(
            spec.params().iter().all(|param| param.name != "connection"),
            "{id} is bound to the connection"
        );
    }
    let (query, _) = state.saved_queries.iter().next().unwrap();
    let query = child(
        &tree,
        &item_resource_id::<Workspace>("saved_queries", query),
    );
    for id in ["remove-query", "run"] {
        assert!(
            query.actions.iter().any(|spec| spec.id == id),
            "a saved query offers {id}"
        );
    }

    let entries = Workspace::search(&state);
    assert!(
        entries
            .iter()
            .any(|entry| entry.title == "Analytics" && entry.text.contains("db.internal"))
    );
    assert!(entries.iter().any(|entry| entry.title == "Daily totals"));
    assert!(
        entries
            .iter()
            .all(|entry| !entry.text.contains("sk-token") && !entry.title.contains("sk-token")),
        "saved SQL is never indexed"
    );
}

#[test]
fn the_workspace_publishes_its_items_with_their_actions_and_live_rows_beside_them() {
    let mut app = TestApp::<DbClientShell>::new().unwrap();
    let mut profile = ConnectionProfile::new("Analytics", Engine::Postgres);
    profile.host = "db.internal".into();
    profile.password = "s3cret".into();
    app.invoke(crate::record::adding(&profile, 0)).unwrap();
    app.invoke(Workspace::SAVE_QUERY.with(workspace::SaveQuery {
        name: "Daily totals".into(),
        sql: Some("SELECT 1".into()),
    }))
    .unwrap();
    app.settle();

    let surface = app.agent_surface().to_string();
    for needle in [
        "add-connection",
        "save-query",
        "remove-connection",
        "remove-query",
        "connect",
        "\"query\"",
        "Run SQL",
        &item_resource_id::<Workspace>("connections", &id_of(profile.id)),
    ] {
        assert!(surface.contains(needle), "missing {needle} in {surface}");
    }
    assert!(
        !surface.contains("database.query-result"),
        "no query ran, so no live result is published"
    );
    assert!(
        !surface.contains("s3cret"),
        "the published tree never holds a password"
    );
}

/// An agent works on the workspace through the one published tree, one
/// call per change, and opening a saved query puts it in the editor.
#[test]
fn an_agent_edits_the_workspace_one_call_each() {
    let mut app = TestApp::<DbClientShell>::new().unwrap();
    app.call(
        "add-connection",
        serde_json::json!({ "name": "Local", "engine": "sqlite", "location": ":memory:" }),
    )
    .unwrap();
    let state = app.state();
    let (connection, saved) = state.connections.iter().next().unwrap();
    assert_eq!(saved.file_path.0, ":memory:");
    let renamed = Workspace::paths()
        .connections
        .item(connection)
        .name
        .replace("Scratch");
    app.call("field-edit", serde_json::json!({ "edit": renamed }))
        .unwrap();
    app.call(
        "save-query",
        serde_json::json!({ "name": "Albums", "sql": "SELECT * FROM albums" }),
    )
    .unwrap();
    let state = app.state();
    assert_eq!(state.profiles()[0].name, "Scratch");
    let (query, _) = state.saved_queries.iter().next().unwrap();
    app.call(
        "open",
        serde_json::json!({ "anchor": item_resource_id::<Workspace>("saved_queries", query) }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.state().sql,
        "SELECT * FROM albums",
        "opening a saved query loads it"
    );
    assert!(app.undo(), "loading it is one undo step");
    assert_eq!(app.state().sql, "");
}

/// What an agent does for a person is history the person reads: the app's
/// sentence for each field and verb.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<DbClientShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = Workspace::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "add-connection",
        serde_json::json!({ "name": "Analytics", "engine": "postgres" }),
    )
    .unwrap();
    let connection = app.state().connections.ids().next().unwrap().clone();
    let paths = Workspace::paths().connections.item(&connection);
    for edit in [
        paths.port.set(6543),
        paths.engine.set(Engine::MySql),
        paths.name.replace("Shop"),
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
        kind,
        "field-edit",
        serde_json::json!({ "edit": Workspace::paths().sql.replace("SELECT 1") }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "save-query",
        serde_json::json!({ "name": "One" }),
    )
    .unwrap();
    app.call_by(
        agent,
        &item_resource_id::<Workspace>("connections", &connection),
        "remove-connection",
        serde_json::json!({}),
    )
    .expect_err("removing asks for confirmation");

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Added the connection Analytics",
            "Set the port of Analytics to 6543",
            "Set the engine of Analytics to MySQL",
            "Renamed the connection Analytics",
            "Edited the SQL",
            "Saved the query One",
        ]
    );
}
