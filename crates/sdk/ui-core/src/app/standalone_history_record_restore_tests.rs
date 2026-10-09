// Restore of a record-backed document from the standalone History host: the
// write goes through the record's own action frame and undo log.

fn record_body(fixture: &ActionFixture) -> String {
    fixture
        .records
        .read(&fixture.bindings[0].checkout.id, |doc| {
            doc.get(ROOT, "body")
                .unwrap()
                .and_then(|(value, _)| value.into_string().ok())
                .unwrap_or_default()
        })
        .unwrap()
}

fn record_heads(fixture: &ActionFixture) -> Vec<automerge::ChangeHash> {
    fixture
        .records
        .read(&fixture.bindings[0].checkout.id, Transactable::base_heads)
        .unwrap()
}

/// Commit `value` as one authored step of the first record's own history.
fn record_commit(fixture: &ActionFixture, value: &str) -> automerge::ChangeHash {
    let binding = &fixture.bindings[0];
    fixture
        .records
        .with_action_state(&binding.checkout.id, |doc, history, _| {
            doc.put(ROOT, "body", value).unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            doc.get_heads()[0]
        })
        .unwrap()
}

/// Three versions of the first record, History open on it.
fn record_history_fixture() -> (ActionFixture, Vec<automerge::ChangeHash>) {
    let mut fixture = ActionFixture::new();
    let versions = ["v1", "v2", "v3"].map(|value| record_commit(&fixture, value));
    let instance = fixture.bindings[0].instance_id;
    fixture.app.focused_node = fixture.app.layout.node_map.keys().copied().find(|node| {
        fixture
            .app
            .bindings
            .binding_for(*node)
            .is_some_and(|binding| binding.instance_id == instance)
    });
    fixture.app.open_document_history().unwrap();
    (fixture, versions.to_vec())
}

fn history_session(app: &mut App) -> uuid::Uuid {
    let resources = app
        .with_history_host(crate::standalone_history_host::StandaloneHistoryHost::local_resources)
        .unwrap();
    let id = resources
        .iter()
        .find(|resource| resource.kind == "tilo.host.history.session")
        .expect("History is open")
        .id
        .clone();
    id.as_str()
        .strip_prefix("tilo.host.history:")
        .and_then(|session| uuid::Uuid::parse_str(session).ok())
        .unwrap()
}

fn history_command(
    app: &mut App,
    command: impl FnOnce(uuid::Uuid) -> crate::standalone_history_host::HistoryCommand,
) -> Result<(), String> {
    let session = history_session(app);
    let encoded = tilo_actions::EncodedAction::encode_operation(
        crate::standalone_history_host::HISTORY_OPERATION,
        &command(session),
    )
    .unwrap();
    app.invoke_history_local(&encoded)
}

fn review_record_restore(app: &mut App, version: automerge::ChangeHash) -> Result<(), String> {
    let entry = crate::history_source::entry_id_from_hash(&version.to_string());
    app.select_document_history(&[entry])?;
    history_command(app, |session| {
        crate::standalone_history_host::HistoryCommand::ReviewRestore {
            session,
            entry: crate::widget::history_group::hex16(&entry),
        }
    })
}

fn confirm_record_restore(app: &mut App) -> Result<(), String> {
    history_command(app, |session| {
        crate::standalone_history_host::HistoryCommand::Restore { session }
    })
}

fn can_restore(app: &mut App) -> bool {
    app.with_history_host(|host, context| {
        host.session(context)
            .unwrap()
            .is_some_and(|session| session.can_restore(context))
    })
}

fn read_only_record_authority() -> crate::permissions::PermissionContext {
    crate::permissions::PermissionContext::granted(
        "fixture-user",
        &crate::permissions::TreePath::root().app("so.tilo.binding-probe"),
        crate::permissions::policy::KindSet::READ,
        None,
    )
    .at_unix_time(100)
}

#[test]
fn standalone_history_restores_a_record_as_one_undoable_change() {
    let (mut fixture, versions) = record_history_fixture();
    let binding = fixture.bindings[0].clone();
    let outer = fixture.app.state.doc.get_heads();
    let entries = fixture
        .app
        .document_history_snapshot()
        .unwrap()
        .unwrap()
        .entries
        .len();
    assert!(
        can_restore(&mut fixture.app),
        "an editor may restore a record"
    );

    review_record_restore(&mut fixture.app, versions[0]).unwrap();
    let reviewed = record_heads(&fixture);
    confirm_record_restore(&mut fixture.app).unwrap();

    // The record returns to v1 as exactly one new, attributed change.
    assert_eq!(record_body(&fixture), "v1");
    fixture
        .records
        .with_action_state(&binding.checkout.id, |doc, history, _| {
            let added = doc.get_changes(&reviewed);
            assert_eq!(added.len(), 1, "restore appends exactly one change");
            let context = added[0]
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)
                .expect("restore carries its action context");
            assert!(matches!(
                context.what,
                Some(tilo_actions::Did::History(tilo_actions::HistoryStep {
                    op: tilo_actions::HistoryOp::Restore,
                    ..
                }))
            ));
            assert_eq!(context.app.unwrap().id, binding.app_id);
            let entry = history.peek_last_undo().unwrap();
            assert_eq!(entry.before, reviewed);
            assert_eq!(entry.author, Some(binding.author.clone()));
            assert_eq!(entry.instance_id, Some(binding.instance_id));
            // Later changes stay in history, like a revert.
            for version in &versions {
                assert!(doc.get_change_by_hash(version).is_some());
            }
        })
        .unwrap();
    assert_eq!(
        fixture
            .app
            .document_history_snapshot()
            .unwrap()
            .unwrap()
            .entries
            .len(),
        entries + 1
    );
    assert_eq!(
        fixture.app.state.doc.get_heads(),
        outer,
        "the app's own document is untouched"
    );

    // Ctrl+Z on a focused bound record is this call in the native runner.
    assert!(
        binding
            .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .is_some()
    );
    assert_eq!(record_body(&fixture), "v3");
}

#[test]
fn standalone_history_record_restore_refuses_stale_reviews() {
    let (mut fixture, versions) = record_history_fixture();
    review_record_restore(&mut fixture.app, versions[0]).unwrap();
    record_commit(&fixture, "concurrent");
    let heads = record_heads(&fixture);
    let error = confirm_record_restore(&mut fixture.app).unwrap_err();
    assert!(error.contains("StaleReview"), "{error}");
    assert_eq!(record_body(&fixture), "concurrent");
    assert_eq!(
        record_heads(&fixture),
        heads,
        "a stale review writes nothing"
    );

    // The frontier is checked again inside the record frame itself.
    let binding = &fixture.bindings[0];
    let target = vec![versions[0]];
    let stale = vec![versions[2]];
    let result = crate::state::owned_restore::restore_reviewed_record(
        &mut fixture.app.state.doc,
        binding,
        &stale,
        &target,
    );
    assert!(matches!(
        result,
        Err(crate::state::owned_restore::OwnedRestoreError::StaleReview)
    ));
    assert_eq!(record_heads(&fixture), heads);
}

#[test]
fn standalone_history_record_restore_requires_write() {
    // A reader cannot open a restore review.
    {
        let (mut fixture, versions) = record_history_fixture();
        *fixture.permissions.borrow_mut() = read_only_record_authority();
        let heads = record_heads(&fixture);
        assert!(!can_restore(&mut fixture.app));
        let error = review_record_restore(&mut fixture.app, versions[0]).unwrap_err();
        assert!(error.contains("Denied"), "{error}");
        assert_eq!(record_body(&fixture), "v3");
        assert_eq!(record_heads(&fixture), heads);
    }

    // Write revoked between review and confirmation is refused at confirmation.
    {
        let (mut fixture, versions) = record_history_fixture();
        review_record_restore(&mut fixture.app, versions[0]).unwrap();
        *fixture.permissions.borrow_mut() = read_only_record_authority();
        let heads = record_heads(&fixture);
        let error = confirm_record_restore(&mut fixture.app).unwrap_err();
        assert!(error.contains("Denied"), "{error}");
        assert_eq!(record_body(&fixture), "v3");
        assert_eq!(record_heads(&fixture), heads);
    }

    // The record door itself refuses a reader, whatever its caller checked.
    {
        let (mut fixture, versions) = record_history_fixture();
        *fixture.permissions.borrow_mut() = read_only_record_authority();
        let heads = record_heads(&fixture);
        let result = crate::state::owned_restore::restore_reviewed_record(
            &mut fixture.app.state.doc,
            &fixture.bindings[0],
            &heads,
            &[versions[0]],
        );
        assert!(matches!(
            result,
            Err(crate::state::owned_restore::OwnedRestoreError::Denied)
        ));
        assert_eq!(record_body(&fixture), "v3");
        assert_eq!(record_heads(&fixture), heads);
    }
}
