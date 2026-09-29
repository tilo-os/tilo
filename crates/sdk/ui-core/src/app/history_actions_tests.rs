//! `tilo.history` through the same resolution, permission filter and dispatch
//! every published invocation takes.

use super::*;
use crate::collaboration::CollabEvent;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use serde_json::{Value, json};
use tilo_actions::{ActionAddress, DoorError};

use super::history_actions::{CLOSE, COMPARE, LIST, OPEN, RESOURCE, RESTORE, VIEW_AT};

fn document(values: &[&str]) -> App {
    let mut app = App::new_with_controlled_inputs(
        crate::State::new(),
        Box::new(crate::widget::Button::new("document", "Document", None)),
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
    app.permission_ctx = crate::permissions::PermissionContext::from_capabilities(
        crate::permissions::Actor::agent("reader"),
        vec![crate::permissions::CapOverride::grant(
            crate::permissions::Subject::agent("reader"),
            crate::permissions::Scope::App(app_id),
            crate::permissions::Verb::Read,
        )],
    );
}

#[test]
fn write_holder_lists_previews_compares_and_restores_through_the_registry() {
    let mut app = document(&["v1", "v2", "v3"]);
    present(&mut app);
    assert_eq!(
        local_actions(&app),
        [CLOSE, COMPARE, LIST, OPEN, RESTORE, VIEW_AT]
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
    assert_eq!(
        context.action_label,
        crate::i18n::I18nManager::lookup_plain("standalone-history-restore-action")
    );
    let author = app.local_author().clone();
    assert!(app.state.undo_as(Some(&author), None, None).is_some());
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
    assert_eq!(history.len(), 6);
    for descriptor in history {
        let expected = if matches!(descriptor.address.action.as_str(), OPEN | LIST) {
            tilo_actions::PermissionRequirement::Read
        } else {
            tilo_actions::PermissionRequirement::Write
        };
        assert_eq!(
            descriptor.contract.permission, expected,
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
        app.state.undo_as(Some(&agent), None, None).is_some(),
        "the restore is the invoking agent's to undo"
    );
    assert_eq!(title(&app).as_deref(), Some("v2"));
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
