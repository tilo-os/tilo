use crate::component_support::*;
use notes::{doc::NotesDoc, hosting};
use ui::application::{ApplicationHost, LaunchTarget};
use ui::headless::{Driver, SemanticExpectation};
use ui::record::{RecordOpenError, RecordStore};

fn host(f: &LegacyFixture) -> ApplicationHost {
    ApplicationHost::new(
        f.state_root.clone(),
        "fixture-device".into(),
        canonical::ROOM,
        f.author.clone(),
        f.authority.clone(),
        tilo_actions::ActionContext::default(),
    )
    .unwrap()
}
fn open(
    f: &LegacyFixture,
    host: &mut ApplicationHost,
    instance: u64,
    import: bool,
) -> (Driver, RecordStore) {
    let prepared = hosting::prepare_default(
        host,
        instance,
        import.then_some(f.source.as_path()),
        runner_inputs(),
    )
    .unwrap();
    let records = host.records_for(notes::definition()).unwrap();
    let component = notes::definition()
        .instantiate(prepared.context, prepared.target, prepared.restore)
        .unwrap();
    (
        Driver::from_component(component, (1000, 700), prepared.runner_inputs).unwrap(),
        records,
    )
}

#[test]
fn fresh_default_is_empty_and_restart_keeps_identity_and_real_edits() {
    let f = LegacyFixture::rich();
    let source = f.source_bytes();
    let checkout = hosting::default_notebook();
    let mut owner = host(&f);
    let (mut live, records) = open(&f, &mut owner, 201, false);
    assert!(
        records
            .read(&checkout.id, |doc| notes::notebook::project(doc, None)
                .unwrap()
                .pages()
                .is_empty())
            .unwrap()
    );
    invoke(&mut live, 201, "notes.notebook", "new-page", Some("First"));
    let page = records
        .read(&checkout.id, |doc| {
            notes::notebook::project(doc, None).unwrap().pages()[0].id
        })
        .unwrap();
    live.type_text("A note that survives restart");
    let expected = records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, page)
        })
        .unwrap();
    assert_eq!(expected[0].1, "A note that survives restart");
    close_driver(&mut live);
    drop(live);
    drop(records);
    drop(owner);
    let mut owner = host(&f);
    // Existing canonical content wins even if a legacy workspace appears later.
    let (mut reopened, records) = open(&f, &mut owner, 201, true);
    reopened
        .expect(&SemanticExpectation::named("note_title").value("First"))
        .unwrap();
    records
        .read(&checkout.id, |doc| {
            assert_eq!(
                notes::notebook::project(doc, None).unwrap().pages().len(),
                1
            );
            assert_eq!(NotesDoc::page_block_snapshots(doc, page), expected);
        })
        .unwrap();
    assert_eq!(source, f.source_bytes());
    close_driver(&mut reopened);
}

#[test]
fn migrated_default_preserves_rich_content_selection_and_source_independence() {
    let f = LegacyFixture::rich();
    let source = f.source_bytes();
    let checkout = hosting::default_notebook();
    let mut owner = host(&f);
    let (mut live, records) = open(&f, &mut owner, 202, true);
    live.expect(&SemanticExpectation::named("note_title").value("Launch"))
        .unwrap();
    let expected = records
        .read(&checkout.id, |doc| {
            notes::notebook::validate(doc).unwrap();
            (
                NotesDoc::page_block_snapshots(doc, f.child),
                NotesDoc::page_block_marks(doc, f.child),
            )
        })
        .unwrap();
    assert!(expected.1.iter().any(|(_, marks)| !marks.is_empty()));
    assert_eq!(source, f.source_bytes());
    close_driver(&mut live);
    drop(live);
    drop(records);
    drop(owner);
    std::fs::rename(&f.source, f.temp.path().join("archived-source")).unwrap();
    let mut owner = host(&f);
    let (mut reopened, records) = open(&f, &mut owner, 202, true);
    reopened
        .expect(&SemanticExpectation::named("note_title").value("Launch"))
        .unwrap();
    assert_eq!(
        records
            .read(&checkout.id, |doc| (
                NotesDoc::page_block_snapshots(doc, f.child),
                NotesDoc::page_block_marks(doc, f.child)
            ))
            .unwrap(),
        expected
    );
    close_driver(&mut reopened);
}

#[test]
fn saved_default_with_missing_record_refuses_legacy_resurrection() {
    let f = LegacyFixture::rich();
    let checkout = hosting::default_notebook();
    let mut owner = host(&f);
    let (mut live, records) = open(&f, &mut owner, 203, true);
    close_driver(&mut live);
    drop(live);
    drop(records);
    drop(owner);
    let path = f.document_path(&checkout);
    std::fs::rename(&path, path.with_extension("retained-backup")).unwrap();
    let source = f.source_bytes();
    let result = hosting::prepare_default(&mut host(&f), 203, Some(&f.source), runner_inputs());
    assert!(matches!(
        result,
        Err(ui::launch::LaunchError::Record(
            RecordOpenError::MissingRecord
        ))
    ));
    assert!(!path.exists());
    assert_eq!(source, f.source_bytes());
}

#[test]
fn corrupt_default_refuses_without_falling_back_to_legacy() {
    let f = LegacyFixture::rich();
    let checkout = hosting::default_notebook();
    let mut owner = host(&f);
    let (mut live, records) = open(&f, &mut owner, 204, true);
    close_driver(&mut live);
    drop(live);
    drop(records);
    drop(owner);
    let path = f.document_path(&checkout);
    std::fs::write(&path, b"broken canonical bytes").unwrap();
    let source = f.source_bytes();
    assert!(
        hosting::prepare_default(&mut host(&f), 204, Some(&f.source), runner_inputs()).is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"broken canonical bytes");
    assert_eq!(source, f.source_bytes());
}

#[test]
fn one_host_shares_the_owner_but_competing_hosts_cannot_start_another_writer() {
    let f = LegacyFixture::rich();
    let mut owner = host(&f);
    let (mut first, records) = open(&f, &mut owner, 205, false);
    let (mut second, shared) = open(&f, &mut owner, 206, false);
    invoke(
        &mut first,
        205,
        "notes.notebook",
        "new-page",
        Some("Shared"),
    );
    let checkout = hosting::default_notebook();
    assert_eq!(
        records.read(
            &checkout.id,
            automerge::transaction::Transactable::base_heads
        ),
        shared.read(
            &checkout.id,
            automerge::transaction::Transactable::base_heads
        )
    );
    let result = hosting::prepare_default(&mut host(&f), 207, None, runner_inputs());
    assert!(matches!(
        result,
        Err(ui::launch::LaunchError::Record(RecordOpenError::Store(
            app_state::StoreError::WriterBusy { .. }
        )))
    ));
    close_driver(&mut first);
    close_driver(&mut second);
}

#[test]
fn named_new_record_uses_the_definition_factory_and_refuses_reinitialization() {
    let f = LegacyFixture::rich();
    let mut owner = host(&f);
    let prepared = hosting::prepare_default(&mut owner, 208, None, runner_inputs()).unwrap();
    assert!(matches!(prepared.target, LaunchTarget::New));
    assert_eq!(prepared.context.record().id, hosting::default_notebook().id);
    let mut component = notes::definition()
        .instantiate(prepared.context, prepared.target, prepared.restore)
        .unwrap();
    let records = owner.records_for(notes::definition()).unwrap();
    let before = records
        .read(
            &component.record().id,
            automerge::transaction::Transactable::base_heads,
        )
        .unwrap();
    let refused = ui::application::HostContext::prepare_new_record(
        notes::definition(),
        records.clone(),
        std::rc::Rc::new(
            ui::application_view::InstanceViewStore::new(
                f.state_root.clone(),
                "fixture-device",
                canonical::ROOM,
            )
            .unwrap(),
        ),
        "fixture-device".into(),
        209,
        f.authority.clone(),
        f.author,
        tilo_actions::ActionContext::default(),
        component.record().id.clone(),
    );
    assert!(matches!(
        refused,
        Err(ui::launch::LaunchError::Record(RecordOpenError::Busy))
    ));
    assert_eq!(
        records
            .read(
                &component.record().id,
                automerge::transaction::Transactable::base_heads,
            )
            .unwrap(),
        before
    );
    assert_closed(&component.close());
}

#[test]
fn invalid_host_record_identity_is_rejected_before_opening_storage() {
    let f = LegacyFixture::rich();
    let mut owner = host(&f);
    let invalid = ui::record::RecordId::derived("invalid@role", &ui::record::RecordId::for_tab(0));
    assert!(ui::record::RecordId::parse(invalid.as_str()).is_none());
    let checkout =
        ui::record::RecordCheckout::new(invalid.clone(), notes::definition().record_kind.clone());
    let result = owner.prepare_default(
        notes::definition(),
        210,
        checkout.clone(),
        None,
        runner_inputs(),
    );
    assert!(matches!(
        result,
        Err(ui::launch::LaunchError::BindingMismatch)
    ));
    let result = ui::application::HostContext::prepare_new_record(
        notes::definition(),
        owner.records_for(notes::definition()).unwrap(),
        std::rc::Rc::new(
            ui::application_view::InstanceViewStore::new(
                f.state_root.clone(),
                "fixture-device",
                canonical::ROOM,
            )
            .unwrap(),
        ),
        "fixture-device".into(),
        210,
        f.authority.clone(),
        f.author.clone(),
        tilo_actions::ActionContext::default(),
        invalid,
    );
    assert!(matches!(
        result,
        Err(ui::launch::LaunchError::BindingMismatch)
    ));
    let result =
        notes::legacy_import::import_legacy_into(&f.records, &f.source, f.key, checkout.clone());
    assert!(matches!(
        result,
        Err(notes::legacy_import::ImportError::Record(
            RecordOpenError::IdentityMismatch
        ))
    ));
    assert!(!f.document_path(&checkout).parent().unwrap().exists());
}

#[test]
fn migration_is_found_after_restart_before_any_window_view_was_saved() {
    let f = LegacyFixture::rich();
    let mut owner = host(&f);
    let prepared =
        hosting::prepare_default(&mut owner, 211, Some(&f.source), runner_inputs()).unwrap();
    let records = owner.records_for(notes::definition()).unwrap();
    let checkout = hosting::default_notebook();
    let expected = records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, f.child)
        })
        .unwrap();
    assert!(!expected.is_empty());
    // Preparation has durably imported the document, but no window has been
    // constructed or closed and no local presentation can locate the record.
    assert!(
        owner
            .saved_view_for(notes::definition(), 211, &checkout)
            .unwrap()
            .is_none()
    );
    drop(prepared);
    drop(records);
    drop(owner);
    std::fs::rename(&f.source, f.temp.path().join("archived-before-window")).unwrap();
    let mut owner = host(&f);
    let (mut reopened, records) = open(&f, &mut owner, 211, true);
    assert_eq!(
        records
            .read(&checkout.id, |doc| NotesDoc::page_block_snapshots(
                doc, f.child
            ))
            .unwrap(),
        expected
    );
    close_driver(&mut reopened);
}

fn temporary_host(f: &LegacyFixture) -> (ApplicationHost, std::path::PathBuf) {
    let directory = std::rc::Rc::new(tempfile::tempdir().unwrap());
    let root = directory.path().to_owned();
    let owner = ApplicationHost::new_with_profile_lifetime(
        root.clone(),
        "fixture-device".into(),
        canonical::ROOM,
        f.author.clone(),
        f.authority.clone(),
        tilo_actions::ActionContext::default(),
        directory,
    )
    .unwrap();
    (owner, root)
}

#[test]
fn temporary_profile_outlives_host_and_component_until_last_writer_is_released() {
    let f = LegacyFixture::rich();
    let (mut owner, root) = temporary_host(&f);
    let (mut live, records) = open(&f, &mut owner, 212, false);
    invoke(
        &mut live,
        212,
        "notes.notebook",
        "new-page",
        Some("Temporary"),
    );
    close_driver(&mut live);
    drop(owner);
    drop(live);
    assert!(root.is_dir());
    assert_eq!(
        records.read(&hosting::default_notebook().id, |doc| {
            notes::notebook::project(doc, None).unwrap().pages().len()
        }),
        Some(1)
    );
    records.try_flush().unwrap();
    drop(records);
    assert!(
        !root.exists(),
        "last canonical owner releases the temporary profile after flushing"
    );
}

#[test]
fn temporary_profile_survives_failed_view_save_and_is_released_after_retry() {
    let f = LegacyFixture::rich();
    let (mut owner, root) = temporary_host(&f);
    let (mut live, records) = open(&f, &mut owner, 213, false);
    std::fs::write(root.join("instance-views"), b"intentional file collision").unwrap();
    live.close_components();
    let mut failed = live
        .take_component_close_reports()
        .into_iter()
        .find(|report| report.view_error.is_some())
        .expect("view save must fail");
    assert!(failed.record_error.is_none());
    drop(live);
    drop(records);
    drop(owner);
    assert!(
        root.is_dir(),
        "presentation retry owns its original temporary profile"
    );
    std::fs::remove_file(root.join("instance-views")).unwrap();
    failed.retry_view().unwrap();
    assert!(failed.view_error.is_none());
    assert!(
        !root.exists(),
        "successful retry releases the final profile owner"
    );
}
