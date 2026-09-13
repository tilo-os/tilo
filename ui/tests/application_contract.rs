//! Explicit CPU fixtures: no App, renderer, ambient identity or service discovery.
use crate::support::application::*;
use automerge::{ROOT, transaction::Transactable};
use std::rc::Rc;
use ui::application::{ApplicationCatalog, HostContext, LaunchTarget, RestoreInput};
use ui::application_view::{InstanceViewStore, SavedInstanceView};
use ui::launch::LaunchError;
use ui::record::RecordId;
use ui::view::{FocusView, FrozenApp, ThawError, View};

#[test]
fn new_instances_are_distinct_and_catalogue_never_fabricates_apps() {
    let host = ProbeHost::new();
    let a = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let b = instantiate_probe(&host, 12, LaunchTarget::New, RestoreInput::None).unwrap();
    assert_ne!(a.record().id, b.record().id);
    assert_eq!(calls(), 2);
    let mut catalogue = ApplicationCatalog::new();
    catalogue.register(probe_definition()).unwrap();
    assert!(matches!(
        catalogue.register(probe_definition()),
        Err(LaunchError::DuplicateApplication(_))
    ));
    assert!(std::ptr::eq(
        catalogue.resolve(APP).unwrap(),
        probe_definition()
    ));
    assert!(matches!(
        catalogue.resolve("unavailable"),
        Err(LaunchError::UnknownApplication(_))
    ));
}

#[test]
fn required_denial_prevents_factory_and_optional_denial_is_visible() {
    let host = ProbeHost::new();
    let mut definition = definition(probe_factory);
    definition.required.push(denied_scope());
    assert!(matches!(
        host.prepare(&definition, 11, &LaunchTarget::New),
        Err(LaunchError::CapabilityDenied(_))
    ));
    assert_eq!(calls(), 0);
    definition.required.clear();
    definition.optional.push(denied_scope());
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    assert!(!context.permissions().check(&definition.optional[0]));
    definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    assert_eq!(calls(), 1);
}

#[test]
fn old_local_view_restores_without_rolling_back_sibling_document_changes() {
    let host = ProbeHost::new();
    let mut a = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let checkout = a.record().clone();
    let mut b = instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap();
    let view = local_view(37.0, "caret=9");
    a.restore_local_view(&view).unwrap();
    let report = a.close();
    let saved = report.recovery.unwrap();
    host.records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "body", "sibling edit").unwrap();
        })
        .unwrap();
    b.restore_local_view(&local_view(81.0, "caret=2")).unwrap();
    drop(a);
    let a = instantiate_probe(&host, 11, existing(&checkout), RestoreInput::Local(saved)).unwrap();
    assert_eq!(a.record().id, b.record().id);
    assert_eq!(a.capture_local_view().unwrap().scroll, vec![37.0]);
    assert_eq!(
        a.capture_local_view().unwrap().focus.unwrap().ephemeral,
        "caret=9"
    );
    assert_eq!(b.capture_local_view().unwrap().scroll, vec![81.0]);
    assert_eq!(body(&host.records, &checkout), "sibling edit");
    assert_eq!(initializations(), 1);
}

#[test]
fn instance_store_scopes_device_and_workspace_and_preserves_bad_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("state");
    for (device, room, scroll) in [("D", "A", 10.0), ("D", "B", 20.0), ("E", "A", 30.0)] {
        let store = InstanceViewStore::new(root.clone(), device, room).unwrap();
        store.save(&envelope(device, room, scroll)).unwrap();
    }
    for (device, room, scroll) in [("D", "A", 10.0), ("D", "B", 20.0), ("E", "A", 30.0)] {
        let store = InstanceViewStore::new(root.clone(), device, room).unwrap();
        assert_eq!(store.load(11).unwrap().unwrap().view.scroll, vec![scroll]);
    }
    let store = InstanceViewStore::new(root.clone(), "D", "A").unwrap();
    let path = root.join("instance-views/44/41/11.postcard");
    let good = envelope("D", "A", 10.0);
    let mut cases = vec![vec![], b"malformed".to_vec()];
    for field in [
        "device",
        "workspace",
        "instance",
        "version",
        "geometry",
        "record",
        "kind",
        "app",
    ] {
        let mut bad = good.clone();
        match field {
            "device" => bad.device_id = "E".into(),
            "workspace" => bad.workspace_id = "B".into(),
            "instance" => bad.instance_id = 12,
            "version" => bad.version = 2,
            "geometry" => bad.view.viewport.width = f64::NAN,
            "record" => bad.record_id.clear(),
            "kind" => bad.record_kind.clear(),
            "app" => bad.app_id.clear(),
            _ => unreachable!(),
        }
        cases.push(postcard::to_stdvec(&bad).unwrap());
    }
    for bad in cases {
        std::fs::write(&path, &bad).unwrap();
        assert!(store.load(11).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bad);
    }
    assert!(store.load(99).unwrap().is_none());
    assert!(InstanceViewStore::new(root.clone(), "", "A").is_err());
    assert!(InstanceViewStore::new(root.clone(), "D", "\0").is_err());
    let escaped = InstanceViewStore::new(root.clone(), "../D", "A/B").unwrap();
    escaped.save(&envelope("../D", "A/B", 7.0)).unwrap();
    assert!(
        root.join("instance-views/2e2e2f44/412f42/11.postcard")
            .exists()
    );
}

#[test]
fn foreign_restore_bindings_never_call_factory_or_replace_persisted_view() {
    let host = ProbeHost::new();
    let mut original = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let checkout = original.record().clone();
    let saved = original.close().recovery.unwrap();
    let original_bytes = std::fs::read(host.view_path(11)).unwrap();
    for field in ["app", "device", "workspace", "record", "kind", "instance"] {
        let mut bad = saved.clone();
        match field {
            "app" => bad.app_id = "foreign.app".into(),
            "device" => bad.device_id = "other-device".into(),
            "workspace" => bad.workspace_id = "other-room".into(),
            "record" => bad.record_id = RecordId::minted().as_str().into(),
            "kind" => bad.record_kind = "other-kind".into(),
            "instance" => bad.instance_id = 99,
            _ => unreachable!(),
        }
        assert!(
            instantiate_probe(&host, 11, existing(&checkout), RestoreInput::Local(bad)).is_err()
        );
        assert_eq!(std::fs::read(host.view_path(11)).unwrap(), original_bytes);
    }
    assert_eq!(calls(), 1);
    assert_eq!(body(&host.records, &checkout), "initialized");
}

#[test]
fn wrong_store_namespace_fails_before_factory() {
    let host = ProbeHost::new();
    for (root, device, room) in [
        (host.root().join("other-profile"), DEVICE, ROOM),
        (host.root(), "other-device", ROOM),
        (host.root(), DEVICE, "other-room"),
    ] {
        let views = Rc::new(InstanceViewStore::new(root, device, room).unwrap());
        assert!(matches!(
            HostContext::prepare(
                probe_definition(),
                host.records.clone(),
                views,
                DEVICE.into(),
                11,
                permissions(),
                author(),
                provenance(),
                &LaunchTarget::New
            ),
            Err(LaunchError::BindingMismatch)
        ));
    }
    assert_eq!(calls(), 0);
}

#[test]
fn legacy_recovery_is_validated_but_never_adopted() {
    let host = ProbeHost::new();
    let a = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let checkout = a.record().clone();
    let frozen = FrozenApp {
        version: 1,
        state: automerge::AutoCommit::new().save(),
        view: View::default(),
    };
    let error = instantiate_probe(
        &host,
        12,
        existing(&checkout),
        RestoreInput::LegacyRecovery(frozen.clone()),
    )
    .err()
    .unwrap();
    assert!(matches!(error, LaunchError::RecoveryRequired));
    let mut bad = frozen.clone();
    bad.version = 9;
    assert!(matches!(
        instantiate_probe(
            &host,
            12,
            existing(&checkout),
            RestoreInput::LegacyRecovery(bad)
        ),
        Err(LaunchError::Restore(ThawError::UnsupportedVersion { .. }))
    ));
    let mut bad = frozen;
    bad.state = b"bad".to_vec();
    assert!(matches!(
        instantiate_probe(
            &host,
            12,
            existing(&checkout),
            RestoreInput::LegacyRecovery(bad)
        ),
        Err(LaunchError::Restore(_))
    ));
    assert_eq!(body(&host.records, &checkout), "initialized");
    assert_eq!(calls(), 1);
}

#[test]
fn successful_initialization_survives_factory_failure_with_only_error_owner() {
    let host = ProbeHost::new();
    let definition = definition(fail_after_initialization);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let minted = context.record().clone();
    let error = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .err()
        .unwrap();
    let root = host.root();
    let views = host.views.clone();
    drop(host.records);
    drop(host.owners);
    let LaunchError::ComponentSetup {
        source,
        record,
        records,
    } = error
    else {
        panic!("missing retained binding")
    };
    assert!(matches!(*source, LaunchError::Operation(_)));
    assert_eq!(record.id, minted.id);
    assert_eq!(body(&records, &record), "initialized");
    let target = existing(&record);
    let context = HostContext::prepare(
        probe_definition(),
        records,
        views,
        DEVICE.into(),
        11,
        permissions(),
        author(),
        provenance(),
        &target,
    )
    .unwrap();
    let mut recovered = probe_definition()
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    assert_eq!(initializations(), 1);
    recovered.close();
    assert!(root.exists());
    assert!(captured_handles().iter().take(1).all(|h| !h.is_active()));
}

#[test]
fn uninitialized_factory_is_rejected_and_reservation_is_released() {
    let host = ProbeHost::new();
    let definition = definition(omit_initialization);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let record = context.record().clone();
    assert!(
        definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .is_err()
    );
    assert!(!host.records.is_ready(&record.id));
    assert!(host.records.reserve_record(&record).is_ok());
    assert!(captured_handles().iter().all(|h| !h.is_active()));
    assert!(!host.view_path(11).exists());
}

#[test]
fn construction_wakes_mount_once_and_close_cancels_local_publication() {
    let host = ProbeHost::new();
    let mut component =
        instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    component
        .mount(move |event| tx.send(event).unwrap())
        .unwrap();
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
    assert!(component.mount(|_| {}).is_err());
    component.close();
    assert!(component.mount(|_| {}).is_err());
    assert!(captured_handles().iter().all(|h| !h.is_active()));
    assert!(!local_state().try_set("late".into()));
    assert!(component.close_report().is_some());
    assert!(drop_was_closed());
}

#[test]
fn failed_view_save_retries_exact_original_store_after_component_drop() {
    let host = ProbeHost::new();
    let mut component =
        instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    component
        .restore_local_view(&local_view(42.0, "caret=6"))
        .unwrap();
    let path = host.view_path(11);
    std::fs::create_dir_all(&path).unwrap();
    drop(host.views);
    let mut report = component.close();
    assert!(report.view_error.is_some());
    assert!(report.view_retry.is_some());
    assert_eq!(report.recovery.as_ref().unwrap().view.scroll, vec![42.0]);
    drop(component);
    assert!(report.retry_view().is_err());
    assert!(report.view_retry.is_some());
    std::fs::remove_dir(&path).unwrap();
    report.retry_view().unwrap();
    assert!(report.view_error.is_none());
    assert!(report.view_retry.is_none());
    let saved: SavedInstanceView = postcard::from_bytes(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved.view.scroll, vec![42.0]);
    assert_eq!(saved.device_id, DEVICE);
    assert_eq!(saved.workspace_id, ROOM);
}

fn local_view(scroll: f64, caret: &str) -> View {
    View {
        scroll: vec![scroll],
        focus: Some(FocusView {
            ordinal: 0,
            ephemeral: caret.into(),
        }),
        ..View::default()
    }
}
fn envelope(device: &str, room: &str, scroll: f64) -> SavedInstanceView {
    SavedInstanceView {
        version: 1,
        app_id: format!("test.{room}.{device}"),
        device_id: device.into(),
        workspace_id: room.into(),
        instance_id: 11,
        record_id: format!("record:{room}:{device}"),
        record_kind: "test".into(),
        view: local_view(scroll, "caret=1"),
    }
}

#[test]
fn factory_cannot_mount_an_unvalidated_component() {
    let host = ProbeHost::new();
    let definition = definition(attempt_premature_mount);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.mount(|_| {}).unwrap();
}

#[test]
fn invalid_factory_view_retains_new_ready_record_without_saving_view() {
    let host = ProbeHost::new();
    let definition = definition(invalid_contribution);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let checkout = context.record().clone();
    let error = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .err()
        .unwrap();
    drop(host.records);
    drop(host.owners);
    let LaunchError::ComponentSetup {
        source,
        record,
        records,
    } = error
    else {
        panic!("missing Ready owner")
    };
    assert!(matches!(*source, LaunchError::Restore(_)));
    assert_eq!(record.id, checkout.id);
    assert_eq!(body(&records, &record), "initialized");
    assert!(
        !host
            .temp
            .path()
            .join("state/instance-views/44/41/11.postcard")
            .exists()
    );
    assert_eq!(initializations(), 1);
    assert!(captured_handles().iter().all(|h| !h.is_active()));
}

#[test]
fn permission_revocation_between_prepare_and_factory_is_enforced() {
    use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    let host = ProbeHost::new();
    let mut definition = definition(probe_factory);
    definition.required.push(denied_scope());
    let authority = permissions();
    *authority.borrow_mut() = PermissionContext::from_capabilities(
        Actor::person(None, "probe-author"),
        vec![CapOverride::grant(
            Subject::person("probe-author"),
            Scope::Workspace,
            Verb::Admin,
        )],
    );
    let context = HostContext::prepare(
        &definition,
        host.records.clone(),
        host.views.clone(),
        DEVICE.into(),
        11,
        authority.clone(),
        author(),
        provenance(),
        &LaunchTarget::New,
    )
    .unwrap();
    let checkout = context.record().clone();
    *authority.borrow_mut() = permissions().borrow().clone();
    assert!(matches!(
        definition.instantiate(context, LaunchTarget::New, RestoreInput::None),
        Err(LaunchError::CapabilityDenied(_))
    ));
    assert_eq!(calls(), 0);
    assert!(host.records.reserve_record(&checkout).is_ok());
}

#[test]
fn failed_close_retains_dirty_record_owner_and_recovery_independently() {
    let host = ProbeHost::new();
    let mut component =
        instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let checkout = component.record().clone();
    host.records.try_flush().unwrap();
    let path = app_state::DocStore::room_record_dir_at(
        &host.root(),
        APP,
        ROOM,
        &checkout.id.storage_key(),
    )
    .unwrap()
    .join(app_state::DOCUMENT_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    host.records
        .with(&checkout.id, |doc| {
            doc.put(ROOT, "body", "unsaved edit").unwrap();
        })
        .unwrap();
    let report = component.close();
    assert!(report.record_error.is_some());
    assert!(report.recovery.is_some());
    assert!(report.view_error.is_none());
    drop(component);
    drop(host.records);
    drop(host.owners);
    let records = report.record_retry.unwrap();
    assert_eq!(body(&records, &checkout), "unsaved edit");
    std::fs::remove_dir(path).unwrap();
    records.try_flush().unwrap();
}

#[test]
fn second_instance_does_not_replace_history_or_commit_a_typing_transaction() {
    let host = ProbeHost::new();
    let mut first = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let checkout = first.record().clone();
    host.records
        .with(&checkout.id, |doc| doc.put(ROOT, "body", "typing").unwrap())
        .unwrap();
    let before = host
        .records
        .with_history(&checkout.id, |doc, history| {
            (
                doc.pending_ops(),
                doc.base_heads(),
                history.revision(),
                std::ptr::from_ref(history),
            )
        })
        .unwrap();
    assert!(before.0 > 0);
    let mut second = instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap();
    first.mount(|_| {}).unwrap();
    second.mount(|_| {}).unwrap();
    let after = host
        .records
        .with_history(&checkout.id, |doc, history| {
            (
                doc.pending_ops(),
                doc.base_heads(),
                history.revision(),
                std::ptr::from_ref(history),
            )
        })
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(initializations(), 1);
    assert_eq!(host.records.open_records().len(), 1);
    let directory = app_state::DocStore::room_record_dir_at(
        &host.root(),
        APP,
        ROOM,
        &checkout.id.storage_key(),
    )
    .unwrap();
    assert!(app_state::DocStore::open_canonical_at(APP, &directory).is_err());
}

#[test]
fn new_restore_hook_failure_keeps_ready_record_and_old_view_bytes() {
    let host = ProbeHost::new();
    let mut old = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let old_saved = old.close().recovery.unwrap();
    let original_bytes = std::fs::read(host.view_path(11)).unwrap();
    let definition = definition(restore_breaks_contribution);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let minted = context.record().clone();
    let saved = SavedInstanceView {
        record_id: minted.id.as_str().into(),
        ..old_saved
    };
    let error = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::Local(saved))
        .err()
        .unwrap();
    assert_eq!(std::fs::read(host.view_path(11)).unwrap(), original_bytes);
    assert!(drop_was_closed());
    drop((host.records, host.owners, old));
    let LaunchError::ComponentSetup {
        source,
        record,
        records,
    } = error
    else {
        panic!("missing restore failure owner")
    };
    assert!(matches!(*source, LaunchError::Restore(_)));
    assert_eq!(record.id, minted.id);
    assert_eq!(body(&records, &record), "initialized");
    let target = existing(&record);
    let context = HostContext::prepare(
        probe_definition(),
        records,
        host.views,
        DEVICE.into(),
        11,
        permissions(),
        author(),
        provenance(),
        &target,
    )
    .unwrap();
    let recovered = probe_definition()
        .instantiate(context, target, RestoreInput::None)
        .unwrap();
    assert_eq!(recovered.record().id, minted.id);
    assert_eq!(initializations(), 2);
}

#[test]
fn existing_factory_failure_preserves_document_and_view_files() {
    let host = ProbeHost::new();
    let mut original = instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let record = original.record().clone();
    let saved = original.close().recovery.unwrap();
    let view_bytes = std::fs::read(host.view_path(11)).unwrap();
    let path =
        app_state::DocStore::room_record_dir_at(&host.root(), APP, ROOM, &record.id.storage_key())
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
    let doc_bytes = std::fs::read(&path).unwrap();
    let definition = definition(fail_after_initialization);
    let target = existing(&record);
    let context = host.prepare(&definition, 11, &target).unwrap();
    let error = definition
        .instantiate(context, target, RestoreInput::Local(saved))
        .err()
        .unwrap();
    assert!(matches!(error, LaunchError::ComponentSetup { .. }));
    assert_eq!(std::fs::read(host.view_path(11)).unwrap(), view_bytes);
    assert_eq!(std::fs::read(path).unwrap(), doc_bytes);
    assert_eq!(initializations(), 1);
    assert!(captured_handles().iter().all(|h| !h.is_active()));
}

#[test]
fn suspension_saves_and_reconstruction_uses_a_fresh_lifetime() {
    let host = ProbeHost::new();
    let mut component =
        instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    component
        .restore_local_view(&local_view(18.0, "caret=3"))
        .unwrap();
    let record = component.record().clone();
    let saved = component.suspend().recovery.unwrap();
    let old_handle = captured_handles().pop().unwrap();
    assert!(!old_handle.is_active());
    let mut replacement =
        instantiate_probe(&host, 11, existing(&record), RestoreInput::Local(saved)).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    replacement
        .mount(move |event| tx.send(event).unwrap())
        .unwrap();
    assert!(rx.try_recv().is_ok());
    old_handle.wake();
    assert!(rx.try_recv().is_err());
    assert_eq!(replacement.capture_local_view().unwrap().scroll, vec![18.0]);
}

#[test]
fn returned_component_must_be_the_exact_prepared_context() {
    let host = ProbeHost::new();
    let definition = definition(foreign_component);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let expected = context.record().clone();
    let foreign = host
        .prepare(probe_definition(), 12, &LaunchTarget::New)
        .unwrap();
    let foreign_record = foreign.record().clone();
    set_foreign_context(foreign);
    let error = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .err()
        .unwrap();
    let LaunchError::ComponentSetup {
        source,
        record,
        records,
    } = error
    else {
        panic!("missing original binding")
    };
    assert!(matches!(*source, LaunchError::BindingMismatch));
    assert_eq!(record.id, expected.id);
    assert_ne!(record.id, foreign_record.id);
    assert_eq!(body(&records, &record), "initialized");
    assert!(captured_handles().iter().all(|handle| !handle.is_active()));
}

#[test]
fn failed_initializer_returns_original_typed_error_without_publishing_empty_record() {
    let host = ProbeHost::new();
    let definition = definition(initializer_failure);
    let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
    let record = context.record().clone();
    let error = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .err()
        .unwrap();
    assert!(
        matches!(error, LaunchError::Record(ui::record::RecordOpenError::Initialization(reason)) if reason == "schema initialization refused")
    );
    assert!(!host.records.is_ready(&record.id));
    assert!(host.records.reserve_record(&record).is_ok());
    assert!(captured_handles().iter().all(|handle| !handle.is_active()));
    let path =
        app_state::DocStore::room_record_dir_at(&host.root(), APP, ROOM, &record.id.storage_key())
            .unwrap()
            .join(app_state::DOCUMENT_FILE);
    assert!(!path.exists());
    assert!(!host.view_path(11).exists());
}

#[test]
fn invalid_save_never_replaces_a_valid_local_envelope() {
    let host = ProbeHost::new();
    let original = envelope(DEVICE, ROOM, 23.0);
    host.views.save(&original).unwrap();
    let bytes = std::fs::read(host.view_path(11)).unwrap();
    let mut foreign = original.clone();
    foreign.device_id = "E".into();
    assert!(matches!(
        host.views.save(&foreign),
        Err(ThawError::BindingMismatch)
    ));
    let mut invalid = original;
    invalid.view.scroll = vec![f64::INFINITY];
    assert!(matches!(
        host.views.save(&invalid),
        Err(ThawError::InvalidView(_))
    ));
    assert_eq!(std::fs::read(host.view_path(11)).unwrap(), bytes);
}
