use super::*;

fn history_preview(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("history-preview") {
        return Some(node);
    }
    node.children.iter().find_map(history_preview)
}

fn restore_review(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("standalone-history-restore-review") {
        return Some(node);
    }
    node.children.iter().find_map(restore_review)
}

#[test]
fn native_event_edits_record_the_actual_resource_title_in_platform_history() {
    let app = CalendarApp::new_demo();
    let mut doc = demo_document(&app);
    doc.commit();
    let event = app.model.events[0].clone();
    assert!(!event.title.trim().is_empty());
    for (label, action) in [
        (
            "Update event",
            ui::tilo_resource::Invoke::from(
                Calendar::paths()
                    .events
                    .item(&crate::model::key(event.id))
                    .location
                    .set("Meeting room".into()),
            ),
        ),
        (
            "Delete event",
            Calendar::DELETE_EVENT.with(crate::model::key(event.id)),
        ),
    ] {
        let before = doc.get_changes(&[]).len();
        app.try_submit(&mut Edits::lend(&mut doc), action)
            .expect("the actual calendar dispatcher admits the edit");
        assert_eq!(doc.get_changes(&[]).len(), before + 1);
        let heads = doc.get_heads();
        let context = doc
            .get_change_by_hash(&heads[0])
            .and_then(|change| {
                change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            })
            .expect("the committed edit carries platform History provenance");
        assert!(
            matches!(
                &context.what,
                Some(tilo_actions::Did::Verb(_) | tilo_actions::Did::Edit(_))
            ),
            "{label}: {context:?}"
        );
        let resource = context.resource.expect("the edit names the calendar");
        assert_eq!(resource.id, "calendar.calendar");
        let item = context.item.expect("and the event it changed");
        assert_eq!(item.id, event.id.to_string(), "{label}");
        assert_eq!(item.label, event.title, "{label}");
    }
    assert!(crate::resource::read_doc(&doc).event(event.id).is_none());
}

#[test]
fn platform_history_restore_returns_the_calendar_to_a_chosen_version() {
    let mut app = CalendarApp::new_detached();
    let mut state = ui::State::new();
    state.doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
    let mut first = None;
    for title in ["Alpha review", "Beta sync"] {
        present(&mut app, &mut state.doc, &state.root);
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state.doc));
        present(&mut app, &mut state.doc, &state.root);
        let event = crate::resource::read_doc(&state.doc)
            .events
            .into_iter()
            .find(|event| event.title.is_empty())
            .expect("the new event");
        apply_to(
            &mut state.doc,
            Calendar::paths()
                .events
                .item(&crate::model::key(event.id))
                .title
                .set(title.into()),
        );
        present(&mut app, &mut state.doc, &state.root);
        state.commit_action(None, None);
        first.get_or_insert_with(|| state.doc.get_heads()[0].to_string());
    }
    assert_eq!(crate::resource::read_doc(&state.doc).events.len(), 2);
    let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (1200, 800));
    // The launcher hands History the app's own root factory; a preview is a
    // second, read-only instance of it.
    harness.set_history_preview_root(|| Ok(Box::new(CalendarApp::new_detached())));
    harness.enable_document_history_chrome();
    open_platform_history(&mut harness);

    let session = harness
        .local_history_resources()
        .unwrap()
        .into_iter()
        .find(|resource| resource.kind == "tilo.host.history.session")
        .expect("an open History session");
    let entry = first.unwrap();
    harness
        .invoke_local_history_action(
            &session
                .action_address("select")
                .expect("a History session publishes `select`"),
            Some(&format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "entries": [&entry[..32]] })
            )),
        )
        .unwrap();
    harness.relayout();
    let changes = harness.state_mut().doc.get_changes(&[]).len();

    // The selected version is shown by the calendar itself, beside the pane.
    let inspection = harness.presentation_inspection();
    let shown = history_preview(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| semantics.name.clone())
        .expect("History previews the selected version");
    assert!(shown.contains("Alpha review"), "{shown}");
    assert!(
        !shown.contains("Beta sync"),
        "the preview is the past: {shown}"
    );
    assert_eq!(
        crate::resource::read_doc(&harness.state_mut().doc)
            .events
            .len(),
        2,
        "previewing writes nothing"
    );

    harness.click_named("standalone-history-restore").unwrap();
    let inspection = harness.presentation_inspection();
    // The confirm action is the review's footer button.
    let (owner, confirm) = restore_review(&inspection.root)
        .and_then(|dialog| {
            dialog.children.iter().find_map(|child| {
                let target = child.semantics.as_ref()?.action_target.clone()?;
                let owner = child.test_id.clone()?;
                target.ends_with(":primary").then_some((owner, target))
            })
        })
        .expect("the review publishes its confirm action");
    harness
        .perform_semantic_named(&owner, Some(&confirm), &ui::a11y::Action::Activate)
        .unwrap();
    harness.relayout();

    let doc = &mut harness.state_mut().doc;
    assert_eq!(
        crate::resource::read_doc(doc).events.len(),
        1,
        "the calendar is back at the version after its first event"
    );
    assert_eq!(doc.get_changes(&[]).len(), changes + 1);
}

/// Open the actual published platform door without depending on app chrome.
fn open_platform_history(harness: &mut ui::headless::FrameHarness) {
    let resources = harness.local_history_resources().unwrap();
    let history = resources
        .iter()
        .find(|resource| resource.id.0 == "tilo.host:history")
        .expect("the platform publishes History independently of app chrome");
    let open = history
        .actions
        .iter()
        .find(|action| action.id == "open")
        .expect("the platform History resource has its stable open action");
    assert_eq!(open.kind, tilo_actions::OperationKind::View);
    assert!(!open.label.trim().is_empty());
    harness
        .invoke_local_history_action(
            &history
                .action_address(&open.id)
                .expect("History publishes `open`"),
            None,
        )
        .unwrap();
}
