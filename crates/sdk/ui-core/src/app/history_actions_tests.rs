use super::*;
use crate::collaboration::CollabEvent;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use serde_json::{Value, json};
use tilo_actions::{ActionAddress, DoorError};

use super::history_actions::{CLOSE, COMPARE, LIST, NAME, OPEN, RESOURCE, RESTORE, VIEW_AT};

fn document(values: &[&str]) -> App {
    let mut app = App::new_with_controlled_inputs(
        crate::State::new(),
        Box::new(crate::widget::Button::new("document", "Document")),
    );
    for value in values {
        commit_title(&mut app, value);
    }
    app
}

fn present(app: &mut App) {
    app.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
    app.init_layout();
    assert!(app.history_chrome_enabled);
}

fn commit_title(app: &mut App, value: &str) {
    app.state.doc.put(ROOT, "title", value).unwrap();
    app.state.commit_action(None, None);
}

fn title(app: &App) -> Option<String> {
    app.state
        .doc
        .get(ROOT, "title")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

fn invoke(app: &mut App, action: &str, args: Option<Value>) -> Result<Value, DoorError> {
    let address = ActionAddress::new(RESOURCE, action);
    let actor = app.collaboration.local_author.0.clone();
    let argument = args.map(|args| format!("{}{args}", tilo_actions::ARGUMENTS_JSON_PREFIX));
    match app.invoke_resource_action(
        ResourceInvocation::new(&address, &actor).argument(argument.as_deref()),
    )? {
        crate::widget::ResourceOperationStart::Completed(output) => {
            Ok(output.value.unwrap_or(Value::Null))
        }
        crate::widget::ResourceOperationStart::Accepted => panic!("History completes inline"),
    }
}

fn local_actions(app: &App) -> Vec<String> {
    let mut actions: Vec<String> = app
        .collect_local_resources()
        .iter()
        .filter(|resource| resource.id.as_str() == RESOURCE)
        .flat_map(|resource| resource.actions.iter().map(|action| action.id.clone()))
        .collect();
    actions.sort();
    actions
}

fn entries(listing: &Value) -> Vec<String> {
    listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["entry"].as_str().unwrap().to_owned())
        .collect()
}

fn reader(app: &mut App) {
    let app_id = crate::collaboration::local_app_id();
    app.permission_ctx = crate::permissions::PermissionContext::agent_granted(
        "reader",
        &crate::permissions::TreePath::root().app(app_id),
        crate::permissions::policy::KindSet::READ,
    );
}

#[test]
fn write_holder_lists_previews_compares_and_restores_through_the_registry() {
    let mut app = document(&["v1", "v2", "v3"]);
    present(&mut app);
    assert_eq!(
        local_actions(&app),
        [CLOSE, COMPARE, LIST, NAME, OPEN, RESTORE, VIEW_AT]
    );

    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let ids = entries(&listing);
    assert_eq!(ids.len(), 3, "{listing}");
    assert_eq!(listing["next_cursor"], Value::Null);
    let (v3, v2, v1) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    // Previewing reads the version without touching the live document.
    let changes = app.state.doc.get_changes(&[]).len();
    let preview = invoke(&mut app, VIEW_AT, Some(json!({"entry": v1}))).unwrap();
    assert_eq!(preview["document"]["title"], "v1", "{preview}");
    assert_eq!(preview["reviewed_live_heads"], listing["live_heads"]);
    assert_eq!(title(&app).as_deref(), Some("v3"));
    assert_eq!(app.state.doc.get_changes(&[]).len(), changes);

    let comparison = invoke(&mut app, COMPARE, Some(json!({"entries": [v2]}))).unwrap();
    assert_eq!(comparison["before"]["title"], "v1", "{comparison}");
    assert_eq!(comparison["after"]["title"], "v2", "{comparison}");
    assert!(comparison["changes"].as_u64().unwrap() > 0);
    assert_eq!(app.state.doc.get_changes(&[]).len(), changes);

    // Restore takes the pane's path: one attributed, undoable change.
    let restored = invoke(
        &mut app,
        RESTORE,
        Some(json!({"entry": v1, "reviewed_live_heads": preview["reviewed_live_heads"]})),
    )
    .unwrap();
    assert_eq!(restored["restored"], true);
    assert_eq!(title(&app).as_deref(), Some("v1"));
    let all = app.state.doc.get_changes(&[]);
    assert_eq!(all.len(), changes + 1, "restore appends exactly one change");
    let context = all
        .last()
        .and_then(|change| change.message())
        .and_then(tilo_actions::ActionContext::from_change_message)
        .expect("restore carries its action context");
    assert!(matches!(
        context.what,
        Some(tilo_actions::Did::History(tilo_actions::HistoryStep {
            op: tilo_actions::HistoryOp::Restore,
            ..
        }))
    ));
    let author = app.local_author().clone();
    assert!(app.state.undo_as(Some(&author), None, None, None).is_some());
    assert_eq!(title(&app).as_deref(), Some("v3"));

    let session = preview["session"].as_str().unwrap();
    assert_eq!(
        invoke(&mut app, CLOSE, Some(json!({"session": session}))).unwrap()["closed"],
        true
    );
    assert!(matches!(
        invoke(&mut app, CLOSE, Some(json!({"session": session}))),
        Err(DoorError::InvalidArguments(_))
    ));
    assert!(ids.contains(&v3));
}

#[test]
fn naming_a_run_writes_one_change_that_lists_and_undoes_as_no_step() {
    let mut app = document(&["v1", "v2", "v3"]);
    present(&mut app);
    let ids = entries(&invoke(&mut app, LIST, Some(json!({}))).unwrap());
    let (v3, v2) = (ids[0].clone(), ids[1].clone());
    let changes = app.state.doc.get_changes(&[]).len();
    let names = |app: &App| {
        crate::history_source::groups::read_named_ranges(
            &app.state.doc,
            &crate::history_source::AuthorDirectory::default(),
        )
    };

    // Newest first or oldest first, the run is the same.
    let named = invoke(
        &mut app,
        NAME,
        Some(json!({"first": v3, "last": v2, "label": " Second draft "})),
    )
    .unwrap();
    assert_eq!(named["named"], true, "{named}");
    assert_eq!(app.state.doc.get_changes(&[]).len(), changes + 1);
    let [range] = names(&app).try_into().unwrap();
    assert_eq!(range.label, "Second draft");
    assert_eq!(
        crate::standalone_history_host::encode_entry(range.start),
        v2
    );
    assert_eq!(
        range.end.map(crate::standalone_history_host::encode_entry),
        Some(v3.clone())
    );
    assert_eq!(
        title(&app).as_deref(),
        Some("v3"),
        "naming moves no content"
    );
    assert_eq!(
        entries(&invoke(&mut app, LIST, Some(json!({}))).unwrap()),
        ids,
        "a name is not a change of its own"
    );

    // Undo walks past a name to the last edit, and the name stays: naming
    // is undone by naming again.
    let author = app.local_author().clone();
    assert!(app.state.undo_as(Some(&author), None, None, None).is_some());
    assert_eq!(title(&app).as_deref(), Some("v2"));
    assert_eq!(names(&app).len(), 1);

    // Renaming replaces the name; an empty name removes it.
    invoke(
        &mut app,
        NAME,
        Some(json!({"first": v2, "last": v3, "label": "Draft"})),
    )
    .unwrap();
    invoke(
        &mut app,
        NAME,
        Some(json!({"first": v2, "last": v3, "label": "Final"})),
    )
    .unwrap();
    let [range] = names(&app).try_into().unwrap();
    assert_eq!(range.label, "Final");
    invoke(
        &mut app,
        NAME,
        Some(json!({"first": v2, "last": v3, "label": ""})),
    )
    .unwrap();
    assert!(names(&app).is_empty());
    let unnamed = invoke(
        &mut app,
        NAME,
        Some(json!({"first": v2, "last": v3, "label": ""})),
    );
    assert_eq!(unnamed.unwrap()["named"], false, "nothing to remove");

    assert!(matches!(
        invoke(
            &mut app,
            NAME,
            Some(json!({"first": "00".repeat(16), "last": v3, "label": "x"}))
        ),
        Err(DoorError::InvalidArguments(_))
    ));
}

#[test]
fn restore_against_outgrown_live_heads_is_refused_unwritten() {
    let mut app = document(&["v1", "v2"]);
    present(&mut app);
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let v1 = entries(&listing)[1].clone();
    let preview = invoke(&mut app, VIEW_AT, Some(json!({"entry": v1}))).unwrap();

    commit_title(&mut app, "concurrent");
    let changes = app.state.doc.get_changes(&[]).len();
    let refused = invoke(
        &mut app,
        RESTORE,
        Some(json!({"entry": v1, "reviewed_live_heads": preview["reviewed_live_heads"]})),
    );
    assert!(
        matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "history_stale_review"),
        "{refused:?}"
    );
    assert_eq!(title(&app).as_deref(), Some("concurrent"));
    assert_eq!(app.state.doc.get_changes(&[]).len(), changes);
}

#[test]
fn listing_pages_newest_first_and_refuses_a_cursor_the_history_outgrew() {
    let values: Vec<String> = (0..60).map(|index| format!("v{index}")).collect();
    let values: Vec<&str> = values.iter().map(String::as_str).collect();
    let mut app = document(&values);
    present(&mut app);

    let first = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    assert_eq!(entries(&first).len(), 50);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let second = invoke(&mut app, LIST, Some(json!({"cursor": cursor}))).unwrap();
    assert_eq!(entries(&second).len(), 10);
    assert_eq!(second["next_cursor"], Value::Null);
    let mut all = entries(&first);
    all.extend(entries(&second));
    all.dedup();
    assert_eq!(all.len(), 60, "pages neither overlap nor skip");

    commit_title(&mut app, "later");
    assert!(matches!(
        invoke(&mut app, LIST, Some(json!({"cursor": cursor}))),
        Err(DoorError::Unavailable { code, .. }) if code == "history_stale_cursor"
    ));
}

#[test]
fn reader_neither_sees_nor_invokes_write_gated_history() {
    let mut app = document(&["v1", "v2"]);
    reader(&mut app);
    present(&mut app);
    assert_eq!(local_actions(&app), [LIST, OPEN]);

    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let v1 = entries(&listing)[1].clone();
    let changes = app.state.doc.get_changes(&[]).len();
    for (action, args) in [
        (VIEW_AT, json!({"entry": v1})),
        (COMPARE, json!({"entries": [v1]})),
        (
            RESTORE,
            json!({"entry": v1, "reviewed_live_heads": listing["live_heads"]}),
        ),
    ] {
        let refused = invoke(&mut app, action, Some(args));
        assert!(
            matches!(refused, Err(DoorError::Unknown(_))),
            "{action}: {refused:?}"
        );
    }
    assert_eq!(title(&app).as_deref(), Some("v2"));
    assert_eq!(app.state.doc.get_changes(&[]).len(), changes);
}

#[test]
fn published_catalogue_declares_write_so_the_broker_withholds_it() {
    let mut app = document(&["v1"]);
    reader(&mut app);
    present(&mut app);
    let history: Vec<_> = tilo_actions::door::survey(&app.publication_resources())
        .into_iter()
        .filter(|descriptor| descriptor.address.resource == RESOURCE)
        .collect();
    assert_eq!(history.len(), 7);
    for descriptor in history {
        let expected = if matches!(descriptor.address.action.as_str(), OPEN | LIST) {
            tilo_actions::ActionKind::Read
        } else {
            tilo_actions::ActionKind::Undoable
        };
        assert_eq!(
            descriptor.contract.kind, expected,
            "{}",
            descriptor.address.action
        );
    }
}

#[test]
fn a_broker_delivered_restore_runs_and_is_attributed_to_the_invoker() {
    let mut app = document(&["v1", "v2"]);
    present(&mut app);
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let v1 = entries(&listing)[1].clone();
    let argument = format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        json!({"entry": v1, "reviewed_live_heads": listing["live_heads"]})
    );
    let changes = app.state.doc.get_changes(&[]).len();
    app.collaboration.manager = super::semantic_tree_app_tests::admitted_delivery_manager();
    app.apply_collab_event_for_test(CollabEvent::Invocation {
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(7)),
        id: 7,
        resource: RESOURCE.into(),
        action: RESTORE.into(),
        argument: Some(argument),
        actor: "agent:Scribe".into(),
        context: None,
    });
    assert_eq!(title(&app).as_deref(), Some("v1"));
    let all = app.state.doc.get_changes(&[]);
    assert_eq!(all.len(), changes + 1);
    let agent = crate::permissions::UserId("agent:Scribe".into());
    assert!(
        app.state.undo_as(Some(&agent), None, None, None).is_some(),
        "the restore is the invoking agent's to undo"
    );
    assert_eq!(title(&app).as_deref(), Some("v2"));
}

/// What an agent changed for its person reads as the agent's, on behalf of
/// that person, through `history.list` too: the door scripts, agents and
/// assistive tech read history by returns who acted and for whom as data.
#[test]
fn history_list_returns_an_agents_change_as_the_agents_for_its_person() {
    use tilo_actions::{ActionContext, ActionInvoker, ActorKind};
    let mut app = document(&["v1", "v2"]);
    present(&mut app);
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let v1 = entries(&listing)[1].clone();
    let argument = format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        json!({"entry": v1, "reviewed_live_heads": listing["live_heads"]})
    );
    let local = std::mem::replace(
        &mut app.collaboration.manager,
        super::semantic_tree_app_tests::admitted_delivery_manager(),
    );
    // The broker stamps the authenticated agent and the person it acts for.
    let context = ActionContext::for_resource(RESOURCE, RESOURCE, "History").with_invoker(
        ActionInvoker::actor("agent:Scribe")
            .with_kind(ActorKind::Agent)
            .acting_for("ada"),
    );
    app.apply_collab_event_for_test(CollabEvent::Invocation {
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(8)),
        id: 8,
        resource: RESOURCE.into(),
        action: RESTORE.into(),
        argument: Some(argument),
        actor: "agent:Scribe".into(),
        context: Some(Box::new(context)),
    });
    assert_eq!(title(&app).as_deref(), Some("v1"));

    // The person reads the history back on their own device.
    app.collaboration.manager = local;
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let newest = &listing["entries"][0];
    assert_eq!(newest["author"], "Scribe · Software, for ada", "{listing}");
    assert_eq!(
        newest["attribution"],
        json!({
            "actor": "agent:Scribe",
            "kind": "agent",
            "actor_name": "Scribe",
            "on_behalf_of": "ada",
            "on_behalf_of_name": "ada",
        }),
        "{listing}"
    );
    assert_eq!(
        listing["entries"][1]["attribution"],
        Value::Null,
        "a person's own change acted for nobody: {listing}"
    );
}

/// A change an agent's own device key signed reads in the app's own
/// history as the agent's, on behalf of its person, once the session named
/// the room's verified agents; a device it did not verify stays unknown.
#[test]
fn history_list_names_a_verified_agents_own_key_change_and_no_other() {
    let mut app = document(&["v1"]);
    present(&mut app);
    // Two collaborators' changes arrive, each signed by its own device.
    let (scribe, stranger) = ([7_u8; 32], [8_u8; 32]);
    for (device, value) in [(scribe, "by scribe"), (stranger, "by stranger")] {
        let mut remote = app
            .state
            .doc
            .fork()
            .with_actor(automerge::ActorId::from(device.as_slice()));
        remote.put(ROOT, "title", value).unwrap();
        remote.commit();
        app.state.doc.merge(&mut remote).unwrap();
    }
    // Newest first: the stranger's change came after the agent's.
    let (stranger_row, scribe_row) = (0, 1);
    let unknown = crate::history_source::UNKNOWN_AUTHOR_NAME;
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    assert_eq!(
        listing["entries"][scribe_row]["author"], unknown,
        "{listing}"
    );

    // The session tells the app which agents of its room it verified.
    let room = uuid::Uuid::from_u128(5).to_string();
    let person = [9_u8; 32];
    app.apply_collab_event_for_test(CollabEvent::HistoryAgents {
        cluster_id: room.clone(),
        agents: vec![crate::history_source::RosterAgent {
            device: scribe,
            person,
            person_name: "Ada".into(),
            agent_id: "scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        }],
    });
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    let agent = &listing["entries"][scribe_row];
    assert_eq!(agent["author"], "Scribe · Software, for Ada", "{listing}");
    assert_eq!(
        agent["attribution"],
        json!({
            "actor": "agent:scribe",
            "kind": "agent",
            "actor_name": "Scribe",
            "on_behalf_of": hex::encode(person),
            "on_behalf_of_name": "Ada",
        }),
        "{listing}"
    );
    let other = &listing["entries"][stranger_row];
    assert_eq!(other["author"], unknown, "an unverified device: {listing}");
    assert_eq!(other["attribution"], Value::Null, "{listing}");

    // The session took the agent back: its changes are nobody's again.
    app.apply_collab_event_for_test(CollabEvent::HistoryAgents {
        cluster_id: room,
        agents: Vec::new(),
    });
    let listing = invoke(&mut app, LIST, Some(json!({}))).unwrap();
    assert_eq!(
        listing["entries"][scribe_row]["author"], unknown,
        "{listing}"
    );
}

#[test]
fn the_in_app_palette_lists_one_history_launcher() {
    let mut app = document(&["v1"]);
    present(&mut app);
    app.open_action_palette();
    let palette = app.actions.action_palette.as_mut().unwrap();
    palette.set_query("");
    let labels = app.action_palette().unwrap().result_labels();
    let chrome = crate::i18n::I18nManager::lookup_plain("standalone-history-title");
    assert!(
        labels.iter().any(|label| label.contains(&chrome)),
        "the palette offers History: {labels:?}"
    );
    let published = crate::i18n::I18nManager::lookup_plain("history-action-open");
    assert!(
        !labels.iter().any(|label| label.contains(&published)),
        "the chrome's own launcher stands for History here: {labels:?}"
    );
    for hidden in ["history-action-list", "history-action-restore"] {
        let hidden = crate::i18n::I18nManager::lookup_plain(hidden);
        assert!(
            !labels.iter().any(|label| label.contains(&hidden)),
            "programmatic History actions stay out of the palette: {labels:?}"
        );
    }
    assert!(
        survey_has(&app.publication_resources(), OPEN),
        "the launcher is still published for the shell palette"
    );
}

fn survey_has(resources: &[tilo_actions::Resource], action: &str) -> bool {
    tilo_actions::door::survey(resources)
        .iter()
        .any(|descriptor| {
            descriptor.address.resource == RESOURCE && descriptor.address.action == action
        })
}
