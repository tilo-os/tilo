//! Explicit CPU fixtures: no App, renderer, ambient identity or service discovery.
use crate::support::application::*;
use automerge::{ROOT, transaction::Transactable};
use std::rc::Rc;
use tilo_ui::application::{
    ApplicationCatalog, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use tilo_ui::application_view::{InstanceViewStore, SavedInstanceView};
use tilo_ui::launch::LaunchError;
use tilo_ui::record::RecordId;
use tilo_ui::view::{FocusView, ThawError, View};

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn instance_creation_enforces_permissions_namespaces_and_distinct_ids() {
    // Case `new_instances_are_distinct_and_catalogue_never_fabricates_apps`.
    {
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

    // Case `required_denial_prevents_factory_and_optional_denial_is_visible`.
    {
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

    // Case `permission_revocation_between_prepare_and_factory_is_enforced`.
    {
        use tilo_ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
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
            HostContextInputs {
                records: host.records.clone(),
                views: host.views.clone(),
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: authority.clone(),
                author: author(),
                action_context: provenance(),
            },
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

    // Case `wrong_store_namespace_fails_before_factory`.
    {
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
                    HostContextInputs {
                        records: host.records.clone(),
                        views,
                        device_id: DEVICE.into(),
                        instance_id: 11,
                        permissions: permissions(),
                        author: author(),
                        action_context: provenance()
                    },
                    &LaunchTarget::New,
                ),
                Err(LaunchError::BindingMismatch)
            ));
        }
        assert_eq!(calls(), 0);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn restore_paths_validate_bindings_and_never_roll_back_documents() {
    // Case `old_local_view_restores_without_rolling_back_sibling_document_changes`.
    {
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
        let a =
            instantiate_probe(&host, 11, existing(&checkout), RestoreInput::Local(saved)).unwrap();
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

    // Case `foreign_restore_bindings_never_call_factory_or_replace_persisted_view`.
    {
        let host = ProbeHost::new();
        let mut original =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
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
                instantiate_probe(&host, 11, existing(&checkout), RestoreInput::Local(bad))
                    .is_err()
            );
            assert_eq!(std::fs::read(host.view_path(11)).unwrap(), original_bytes);
        }
        assert_eq!(calls(), 1);
        assert_eq!(body(&host.records, &checkout), "initialized");
    }

    // Case `new_restore_hook_failure_keeps_ready_record_and_old_view_bytes`.
    {
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
            recovery,
        } = error
        else {
            panic!("missing restore failure owner")
        };
        let records = recovery.into_record_store().unwrap().unwrap();
        assert!(matches!(*source, LaunchError::Restore(_)));
        assert_eq!(record.id, minted.id);
        assert_eq!(body(&records, &record), "initialized");
        let target = existing(&record);
        let context = HostContext::prepare(
            probe_definition(),
            HostContextInputs {
                records,
                views: host.views,
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: permissions(),
                author: author(),
                action_context: provenance(),
            },
            &target,
        )
        .unwrap();
        let recovered = probe_definition()
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        assert_eq!(recovered.record().id, minted.id);
        assert_eq!(initializations(), 2);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn local_envelopes_are_scoped_versioned_and_never_replaced_by_invalid_saves() {
    // Case `instance_store_scopes_device_and_workspace_and_preserves_bad_bytes`.
    {
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
                "version" => bad.version = 3,
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

    // Case `v2_application_envelope_roundtrips_and_default_hook_refuses_payload`.
    {
        use tilo_ui::application_view::ApplicationLocalView;
        let host = ProbeHost::new();
        let mut first =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let mut saved = first.close().recovery.unwrap();
        assert_eq!(saved.version, 2);
        assert!(saved.application.is_none());
        saved.application = Some(ApplicationLocalView {
            app_id: APP.into(),
            version: 17,
            payload: vec![0, 1, 255],
        });
        host.views.save(&saved).unwrap();
        let original = std::fs::read(host.view_path(11)).unwrap();
        let decoded = host.views.load(11).unwrap().unwrap();
        let local = decoded.application.as_ref().unwrap();
        assert_eq!(
            local.version, 17,
            "the store leaves app schema validation to its owner"
        );
        assert_eq!(local.payload, [0, 1, 255]);
        let error = instantiate_probe(
            &host,
            11,
            existing(first.record()),
            RestoreInput::Local(decoded),
        )
        .err()
        .unwrap();
        assert!(matches!(error, LaunchError::ComponentSetup { .. }));
        assert_eq!(std::fs::read(host.view_path(11)).unwrap(), original);
        let mut foreign = saved;
        foreign.application.as_mut().unwrap().app_id = "foreign.app".into();
        assert!(host.views.save(&foreign).is_err());
        assert_eq!(std::fs::read(host.view_path(11)).unwrap(), original);
        for invalid in [
            original[..original.len() - 1].to_vec(),
            [original.as_slice(), &[0]].concat(),
        ] {
            std::fs::write(host.view_path(11), &invalid).unwrap();
            assert!(host.views.load(11).is_err());
            assert_eq!(std::fs::read(host.view_path(11)).unwrap(), invalid);
        }
    }

    // Case `invalid_save_never_replaces_a_valid_local_envelope`.
    {
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
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn factory_failures_keep_records_and_files_and_reject_unvalidated_components() {
    // Case `successful_initialization_survives_factory_failure_with_only_error_owner`.
    {
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
            recovery,
        } = error
        else {
            panic!("missing retained binding")
        };
        let records = recovery.into_record_store().unwrap().unwrap();
        assert!(matches!(*source, LaunchError::Operation(_)));
        assert_eq!(record.id, minted.id);
        assert_eq!(body(&records, &record), "initialized");
        let target = existing(&record);
        let context = HostContext::prepare(
            probe_definition(),
            HostContextInputs {
                records,
                views,
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: permissions(),
                author: author(),
                action_context: provenance(),
            },
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

    // Case `uninitialized_factory_is_rejected_and_reservation_is_released`.
    {
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

    // Case `factory_cannot_mount_an_unvalidated_component`.
    {
        let host = ProbeHost::new();
        let definition = definition(attempt_premature_mount);
        let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        component.mount(|_| {}).unwrap();
    }

    // Case `invalid_factory_view_retains_new_ready_record_without_saving_view`.
    {
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
            recovery,
        } = error
        else {
            panic!("missing Ready owner")
        };
        let records = recovery.into_record_store().unwrap().unwrap();
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

    // Case `existing_factory_failure_preserves_document_and_view_files`.
    {
        let host = ProbeHost::new();
        let mut original =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let record = original.record().clone();
        let saved = original.close().recovery.unwrap();
        let view_bytes = std::fs::read(host.view_path(11)).unwrap();
        let path = app_state::DocStore::room_record_dir_at(
            &host.root(),
            APP,
            ROOM,
            &record.id.storage_key(),
        )
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

    // Case `returned_component_must_be_the_exact_prepared_context`.
    {
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
            recovery,
        } = error
        else {
            panic!("missing original binding")
        };
        let records = recovery.into_record_store().unwrap().unwrap();
        assert!(matches!(*source, LaunchError::BindingMismatch));
        assert_eq!(record.id, expected.id);
        assert_ne!(record.id, foreign_record.id);
        assert_eq!(body(&records, &record), "initialized");
        assert!(captured_handles().iter().all(|handle| !handle.is_active()));
    }

    // Case `failed_initializer_returns_original_typed_error_without_publishing_empty_record`.
    {
        let host = ProbeHost::new();
        let definition = definition(initializer_failure);
        let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
        let record = context.record().clone();
        let error = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .err()
            .unwrap();
        assert!(
            matches!(error, LaunchError::Record(tilo_ui::record::RecordOpenError::Initialization(reason)) if reason == "schema initialization refused")
        );
        assert!(!host.records.is_ready(&record.id));
        assert!(host.records.reserve_record(&record).is_ok());
        assert!(captured_handles().iter().all(|handle| !handle.is_active()));
        let path = app_state::DocStore::room_record_dir_at(
            &host.root(),
            APP,
            ROOM,
            &record.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE);
        assert!(!path.exists());
        assert!(!host.view_path(11).exists());
    }
}

#[test]
fn instance_lifetimes_wake_once_suspend_and_isolate_history() {
    // Case `construction_wakes_mount_once_and_close_cancels_local_publication`.
    {
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

    // Case `suspension_saves_and_reconstruction_uses_a_fresh_lifetime`.
    {
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

    // Case `second_instance_does_not_replace_history_or_commit_a_typing_transaction`.
    {
        let host = ProbeHost::new();
        let mut first =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
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
        let mut second =
            instantiate_probe(&host, 12, existing(&checkout), RestoreInput::None).unwrap();
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
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn save_and_close_failures_retain_owners_and_retry_persistence() {
    // Case `failed_view_save_retries_exact_original_store_after_component_drop`.
    {
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

    // Case `failed_close_retains_dirty_record_owner_and_recovery_independently`.
    {
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

    // Case `native_close_refusal_preserves_live_owner_and_retry_persists_later_edits`.
    {
        use automerge::ReadDoc;

        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        host.records.try_flush().unwrap();
        let document_path = app_state::DocStore::room_record_dir_at(
            &host.root(),
            APP,
            ROOM,
            &checkout.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE);
        let retained = document_path.with_extension("retained");
        std::fs::rename(&document_path, &retained).unwrap();
        std::fs::create_dir(&document_path).unwrap();
        let view_path = host.view_path(11);
        std::fs::create_dir_all(&view_path).unwrap();
        let mut app = tilo_ui::App::from_component(component, runner_inputs()).unwrap();
        host.records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "before refused close").unwrap();
            })
            .unwrap();
        assert!(!app.request_close());
        assert!(
            captured_handles()
                .iter()
                .all(tilo_ui::WakeHandle::is_active)
        );
        assert!(local_state().try_set("editing after close refusal".into()));
        host.records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "edited after refusal").unwrap();
            })
            .unwrap();
        assert!(
            !app.request_close(),
            "both original stores still refuse persistence"
        );
        std::fs::remove_dir(&document_path).unwrap();
        std::fs::remove_dir(&view_path).unwrap();
        app.refresh_semantic_tree();
        let retry = app
            .semantic_tree()
            .iter()
            .find(|node| node.action("retry").is_some())
            .map(|node| node.id.clone())
            .expect("failed close publishes an actionable Retry decision");
        assert!(
            app.invoke_semantic_action(&retry, "retry", None).unwrap(),
            "the public semantic action must retry the retained owners"
        );
        assert!(
            app.request_close(),
            "successful retry leaves the same live owners ready for native close"
        );
        assert_eq!(body(&host.records, &checkout), "edited after refusal");
        let saved = automerge::AutoCommit::load(&std::fs::read(&document_path).unwrap()).unwrap();
        assert_eq!(
            saved.get(ROOT, "body").unwrap().unwrap().0.as_str(),
            Some("edited after refusal")
        );
        assert!(view_path.is_file());
        // Admission alone is non-destructive; only the event-loop exit boundary
        // finalizes. Existing forced close remains available to test teardown.
        assert!(
            captured_handles()
                .iter()
                .all(tilo_ui::WakeHandle::is_active)
        );
        app.close_components();
        assert!(captured_handles().iter().all(|handle| !handle.is_active()));
    }

    // Case `record_save_status_is_delivered_after_document_loan`.
    // Real AppWidget callbacks sample the same store during sync and after release.
    {
        let mut fixture = save_status_fixture::Fixture::new();
        fixture.pending_and_visible_checkpoint();
        fixture.sticky_failure_and_recovery();
        fixture.close();
    }
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
        version: 2,
        app_id: format!("test.{room}.{device}"),
        device_id: device.into(),
        workspace_id: room.into(),
        instance_id: 11,
        record_id: format!("record:{room}:{device}"),
        record_kind: "test".into(),
        view: local_view(scroll, "caret=1"),
        application: None,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "fault matrix keeps last-owner transfer, pending-history comparison and repair in one auditable scenario"
)]
fn ready_validation_failures_retain_the_last_owner_and_pending_history() {
    use tilo_ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    for case in ["definition", "target", "foreign", "malformed", "revoked"] {
        let host = ProbeHost::new();
        let mut component =
            instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
        let checkout = component.record().clone();
        let saved = component.close().recovery.unwrap();
        drop(component);
        host.records
            .with_history(&checkout.id, |doc, history| {
                doc.put(ROOT, "body", "history edit").unwrap();
                assert!(history.commit_action_authored(
                    doc,
                    Some(0),
                    Some("caret=7".into()),
                    Some(author())
                ));
            })
            .unwrap();
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
                doc.put(ROOT, "body", "failed save").unwrap();
            })
            .unwrap();
        assert!(host.records.try_flush().is_err());
        host.records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "pending edit").unwrap();
            })
            .unwrap();
        let snapshot = |doc: &mut automerge::AutoCommit, history: &mut tilo_ui::state::UndoLog| {
            let last = history.peek_last_undo().unwrap();
            (
                doc.pending_ops(),
                doc.base_heads(),
                history.revision(),
                std::ptr::from_ref(history),
                last.before.clone(),
                last.after.clone(),
                last.widget_metadata.clone(),
                last.author.clone(),
            )
        };
        let before = host.records.with_history(&checkout.id, snapshot).unwrap();
        assert!(before.0 > 0);
        assert_eq!(before.2, 1);
        let mut definition = definition(probe_factory);
        let authority = permissions();
        if case == "revoked" {
            definition.required.push(denied_scope());
            *authority.borrow_mut() = PermissionContext::from_capabilities(
                Actor::person(None, "probe-author"),
                vec![CapOverride::grant(
                    Subject::person("probe-author"),
                    Scope::Workspace,
                    Verb::Admin,
                )],
            );
        }
        let target = existing(&checkout);
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records: host.records,
                views: host.views.clone(),
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: authority.clone(),
                author: author(),
                action_context: provenance(),
            },
            &target,
        )
        .unwrap();
        let wake = context.wake_handle();
        let mut bad = saved;
        match case {
            "definition" => definition.app_id = "foreign.app",
            "target" => {}
            "foreign" => bad.app_id = "foreign.app".into(),
            "malformed" => bad.view.viewport.height = f64::NAN,
            "revoked" => *authority.borrow_mut() = permissions().borrow().clone(),
            _ => unreachable!(),
        }
        let target = if case == "target" {
            LaunchTarget::New
        } else {
            target
        };
        // Only the context owns the record now; RecordOwners entries are weak.
        let error = definition
            .instantiate(context, target, RestoreInput::Local(bad))
            .err()
            .unwrap();
        assert!(!wake.is_active());
        let LaunchError::ComponentSetup {
            source,
            record,
            recovery,
        } = error
        else {
            panic!("Ready validation must retain its last owner: {case}")
        };
        let records = recovery.into_record_store().unwrap().unwrap();
        match case {
            "definition" | "target" | "foreign" => {
                assert!(matches!(*source, LaunchError::BindingMismatch));
            }
            "malformed" => assert!(matches!(
                *source,
                LaunchError::Restore(ThawError::InvalidView(_))
            )),
            "revoked" => assert!(
                matches!(*source, LaunchError::CapabilityDenied(scope) if scope == denied_scope())
            ),
            _ => unreachable!(),
        }
        assert_eq!(record.id, checkout.id);
        assert_eq!(body(&records, &record), "pending edit");
        assert_eq!(records.with_history(&record.id, snapshot).unwrap(), before);
        assert!(records.durability_error(&record.id).is_some());
        assert_eq!(calls(), 1);
        definition.app_id = APP;
        *authority.borrow_mut() = PermissionContext::from_capabilities(
            Actor::person(None, "probe-author"),
            vec![CapOverride::grant(
                Subject::person("probe-author"),
                Scope::Workspace,
                Verb::Admin,
            )],
        );
        let target = existing(&record);
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records: records.clone(),
                views: host.views,
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: authority,
                author: author(),
                action_context: provenance(),
            },
            &target,
        )
        .unwrap();
        let mut retry = definition
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        assert_eq!(initializations(), 1);
        assert_eq!(records.with_history(&record.id, snapshot).unwrap(), before);
        std::fs::remove_dir(&path).unwrap();
        assert!(retry.close().record_error.is_none());
    }
}

#[test]
fn destructor_panics_propagate_unconverted() {
    // Case `component_destructor_panics_reach_the_outer_caller_after_scope_closes`.
    {
        for closing in [false, true] {
            let host = ProbeHost::new();
            let mut component =
                instantiate_probe(&host, 11, LaunchTarget::New, RestoreInput::None).unwrap();
            panic_on_next_root_drop();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if closing {
                    component.close();
                } else {
                    drop(component);
                }
            }));
            let panic = result.expect_err("component teardown must not suppress an app panic");
            assert_eq!(
                panic.downcast_ref::<&str>(),
                Some(&"probe destructor panic")
            );
            assert!(drop_was_closed());
        }
    }

    // Case `failed_setup_destructor_panic_is_not_converted_to_a_launch_error`.
    {
        let host = ProbeHost::new();
        let definition = definition(invalid_contribution);
        let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
        panic_on_next_root_drop();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = definition.instantiate(context, LaunchTarget::New, RestoreInput::None);
        }));
        let panic = result.expect_err("failed construction must preserve its destructor panic");
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"probe destructor panic")
        );
        assert!(drop_was_closed());
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn prepared_drivers_use_canonical_documents_and_keep_close_recovery_on_failure() {
    // Case `prepared_driver_semantic_outputs_use_the_canonical_document`.
    {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let checkout = component.record().clone();
        let dump = host.temp.path().join("output/document.json");
        let inspect = host.temp.path().join("output/inspect.json");
        let targets = host.temp.path().join("output/targets.json");
        let spec = tilo_ui::headless::Spec {
            dump: Some(dump.clone()),
            inspect: Some(inspect.clone()),
            targets: Some(targets.clone()),
            ..Default::default()
        };
        assert_eq!(
            tilo_ui::headless::run_component(&spec, component, runner_inputs()),
            0
        );
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dump).unwrap()).unwrap();
        assert_eq!(document["body"], "initialized");
        assert!(std::fs::metadata(inspect).unwrap().len() > 0);
        assert!(std::fs::metadata(targets).unwrap().len() > 0);
        assert_eq!(body(&host.records, &checkout), "initialized");
        assert!(host.view_path(11).is_file());
        assert_eq!(calls(), 1);
    }

    // Case `prepared_runner_mount_failure_retains_close_recovery`.
    {
        for native in [false, true] {
            let host = ProbeHost::new();
            let mut component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let checkout = component.record().clone();
            component.mount(|_| {}).unwrap();
            let path = host.view_path(11);
            std::fs::create_dir_all(&path).unwrap();
            drop(host.views);
            let error = if native {
                tilo_ui::App::from_component(component, runner_inputs())
                    .err()
                    .unwrap()
            } else {
                tilo_ui::headless::Driver::from_component(component, (320, 180), runner_inputs())
                    .err()
                    .unwrap()
            };
            let LaunchError::ComponentSetup {
                record, recovery, ..
            } = error
            else {
                panic!("runner setup lost the canonical recovery owner");
            };
            let (records, mut reports) = recovery.into_parts().unwrap();
            let records = records.unwrap();
            assert_eq!(*record, checkout);
            assert_eq!(reports.len(), 1);
            assert!(reports[0].view_error.is_some());
            assert_eq!(body(&records, &record), "initialized");
            std::fs::remove_dir(&path).unwrap();
            reports[0].retry_view().unwrap();
            assert!(path.is_file());
        }
    }

    // Case `prepared_headless_action_and_output_failure_keep_exit_one_and_close`.
    {
        for output_failure in [false, true] {
            let host = ProbeHost::new();
            let component = runner_probe_definition()
                .instantiate(
                    host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                        .unwrap(),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let writer = local_state().writer();
            let output = host.root().join("output-is-directory");
            std::fs::create_dir_all(&output).unwrap();
            let spec = if output_failure {
                tilo_ui::headless::Spec {
                    dump: Some(output),
                    ..Default::default()
                }
            } else {
                tilo_ui::headless::Spec {
                    actions: vec![tilo_ui::headless::Action::ClickNamed("absent".into())],
                    ..Default::default()
                }
            };
            assert_eq!(
                tilo_ui::headless::run_component(&spec, component, runner_inputs()),
                1
            );
            assert!(!writer.try_set("after runner close".into()));
            assert!(host.view_path(11).is_file());
        }
    }

    // Case `prepared_headless_flush_failure_is_durability_exit_with_original_bytes`.
    {
        let host = ProbeHost::new();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare(runner_probe_definition(), 11, &LaunchTarget::New)
                    .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
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
        let original = std::fs::read(&path).unwrap();
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained).unwrap();
        std::fs::create_dir(&path).unwrap();
        host.records
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "body", "pending durability").unwrap();
            })
            .unwrap();
        assert_eq!(
            tilo_ui::headless::run_component(
                &tilo_ui::headless::Spec::default(),
                component,
                runner_inputs()
            ),
            3
        );
        assert_eq!(std::fs::read(&retained).unwrap(), original);
        assert_eq!(body(&host.records, &checkout), "pending durability");
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(&retained, &path).unwrap();
        host.records.try_flush().unwrap();
        assert_eq!(
            host.records.save_status(&checkout.id).unwrap(),
            tilo_ui::record::RecordSaveStatus::Saved
        );
    }
}

mod save_status_fixture {
    use super::*;
    use std::cell::RefCell;
    use tilo_ui::{
        Widget,
        record::{RecordCheckout, RecordOpenError, RecordSaveStatus, RecordStore},
        widget::{AppKind, AppWidget, IconDescriptor, TileHost},
    };
    thread_local! {
        static STORE: RefCell<Option<RecordStore>> = const { RefCell::new(None) };
        static DELIVERED: RefCell<Vec<RecordSaveStatus>> = const { RefCell::new(Vec::new()) };
        static TYPE_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    struct StatusRoot {
        records: RecordStore,
        checkout: RecordCheckout,
        edits: u64,
    }
    impl Widget for StatusRoot {
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &tilo_ui::Theme,
            _: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            tree.new_leaf(taffy::Style::default()).unwrap()
        }
        fn apply_layout(
            &mut self,
            _: &taffy::TaffyTree<()>,
            _: taffy::NodeId,
            _: vello::kurbo::Point,
        ) {
        }
        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut tilo_ui::TextRenderer,
            _: &tilo_ui::Theme,
            _: &tilo_ui::presence::PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Status"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn sync(&mut self, doc: &mut tilo_ui::widget::AppModel, path: &tilo_ui::widget::ModelPath) {
            let (doc, _) = tilo_ui::doc::document(doc, path);
            assert!(matches!(
                self.records.save_status(&self.checkout.id),
                Err(RecordOpenError::Busy)
            ));
            if TYPE_PENDING.replace(false) {
                self.edits = self.edits.checked_add(1).unwrap();
                doc.put(ROOT, "pending", self.edits).unwrap();
            }
        }
    }
    impl AppWidget for StatusRoot {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Status"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Named("status".into())
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
        fn record_save_status(&mut self, status: RecordSaveStatus) {
            assert_eq!(self.records.save_status(&self.checkout.id).unwrap(), status);
            DELIVERED.with(|seen| seen.borrow_mut().push(status));
        }
    }
    fn factory(
        mut context: HostContext,
        target: &LaunchTarget,
        _: RestoreInput,
    ) -> Result<tilo_ui::application::AppComponent, LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_record(|_| Ok(()))
                .map_err(LaunchError::Record)?;
        }
        let root = StatusRoot {
            records: STORE.with(|store| store.borrow().as_ref().unwrap().clone()),
            checkout: context.record().clone(),
            edits: 0,
        };
        Ok(tilo_ui::application::AppComponent::new(
            context,
            Box::new(root),
            tilo_actions::ActionRegistry::new(),
        ))
    }
    struct ClearStatusFixture;
    impl Drop for ClearStatusFixture {
        fn drop(&mut self) {
            STORE.with(|store| *store.borrow_mut() = None);
            DELIVERED.with(|seen| seen.borrow_mut().clear());
            TYPE_PENDING.set(false);
        }
    }
    struct Obstruction {
        path: std::path::PathBuf,
        retained: std::path::PathBuf,
        active: bool,
    }
    impl Obstruction {
        fn restore(&mut self) {
            std::fs::remove_dir(&self.path).unwrap();
            std::fs::rename(&self.retained, &self.path).unwrap();
            self.active = false;
        }
    }
    impl Drop for Obstruction {
        fn drop(&mut self) {
            if self.active {
                let _ = std::fs::remove_dir(&self.path);
                let _ = std::fs::rename(&self.retained, &self.path);
            }
        }
    }
    pub(super) struct Fixture {
        tile: TileHost,
        outer: automerge::AutoCommit,
        checkout: RecordCheckout,
        _clear: ClearStatusFixture,
        host: ProbeHost,
    }
    impl Fixture {
        pub(super) fn new() -> Self {
            let host = ProbeHost::new();
            let clear = ClearStatusFixture;
            STORE.with(|store| *store.borrow_mut() = Some(host.records.clone()));
            let definition =
                definition(|context, target, restore| factory(context, &target, restore));
            let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
            let component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            let checkout = component.record().clone();
            host.records.try_flush().unwrap();
            let mut tile = TileHost::from_component(component);
            let mut outer = automerge::AutoCommit::new();
            tile.sync(&mut outer, &ROOT);
            assert_eq!(
                DELIVERED.with(|seen| seen.borrow().clone()),
                vec![RecordSaveStatus::Saved]
            );
            Self {
                tile,
                outer,
                checkout,
                _clear: clear,
                host,
            }
        }
        fn path(&self) -> std::path::PathBuf {
            app_state::DocStore::room_record_dir_at(
                &self.host.root(),
                APP,
                ROOM,
                &self.checkout.id.storage_key(),
            )
            .unwrap()
            .join(app_state::DOCUMENT_FILE)
        }
        pub(super) fn pending_and_visible_checkpoint(&mut self) {
            TYPE_PENDING.set(true);
            self.tile.sync(&mut self.outer, &ROOT);
            let before = self
                .host
                .records
                .read(&self.checkout.id, |doc| {
                    (doc.base_heads(), doc.pending_ops())
                })
                .unwrap();
            for _ in 0..3 {
                self.tile.sync(&mut self.outer, &ROOT);
            }
            assert_eq!(
                self.host
                    .records
                    .read(&self.checkout.id, |doc| (
                        doc.base_heads(),
                        doc.pending_ops()
                    ))
                    .unwrap(),
                before
            );
            assert_eq!(
                DELIVERED.with(|seen| seen.borrow().last().cloned()),
                Some(RecordSaveStatus::Saving)
            );
            // Queue the older revision, then leave a newer transaction open while the
            // real writer makes that older checkpoint visible. The private RecordStore
            // predicate test separately observes the producer acknowledgement.
            self.host.records.tick();
            let older = self
                .host
                .records
                .read(&self.checkout.id, Transactable::base_heads)
                .unwrap();
            TYPE_PENDING.set(true);
            self.tile.sync(&mut self.outer, &ROOT);
            let pending = self
                .host
                .records
                .read(&self.checkout.id, |doc| {
                    (doc.base_heads(), doc.pending_ops())
                })
                .unwrap();
            assert!(pending.1 > 0);
            let path = self.path();
            let deadline = tilo_test_env::hang_deadline();
            loop {
                let visible = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                    .is_some_and(|mut doc| doc.get_heads() == older);
                if visible {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "older checkpoint was not visible"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            self.tile.sync(&mut self.outer, &ROOT);
            assert_eq!(
                DELIVERED.with(|seen| seen.borrow().last().cloned()),
                Some(RecordSaveStatus::Saving)
            );
            assert_eq!(
                self.host
                    .records
                    .read(&self.checkout.id, |doc| (
                        doc.base_heads(),
                        doc.pending_ops()
                    ))
                    .unwrap(),
                pending
            );
            self.host.records.try_flush().unwrap();
            self.tile.sync(&mut self.outer, &ROOT);
            assert_eq!(
                DELIVERED.with(|seen| seen.borrow().last().cloned()),
                Some(RecordSaveStatus::Saved)
            );
        }
        pub(super) fn sticky_failure_and_recovery(&mut self) {
            let path = self.path();
            let original = std::fs::read(&path).unwrap();
            let retained = path.with_extension("retained-status");
            std::fs::rename(&path, &retained).unwrap();
            let mut obstruction = Obstruction {
                path,
                retained,
                active: true,
            };
            std::fs::create_dir(&obstruction.path).unwrap();
            TYPE_PENDING.set(true);
            self.tile.sync(&mut self.outer, &ROOT);
            assert!(self.host.records.try_flush().is_err());
            for _ in 0..3 {
                self.tile.sync(&mut self.outer, &ROOT);
                assert!(matches!(
                    DELIVERED.with(|seen| seen.borrow().last().cloned()),
                    Some(RecordSaveStatus::Failed(_))
                ));
            }
            assert_eq!(std::fs::read(&obstruction.retained).unwrap(), original);
            obstruction.restore();
            self.tile.sync(&mut self.outer, &ROOT);
            assert!(matches!(
                DELIVERED.with(|seen| seen.borrow().last().cloned()),
                Some(RecordSaveStatus::Failed(_))
            ));
            self.host.records.try_flush().unwrap();
            self.tile.sync(&mut self.outer, &ROOT);
            assert_eq!(
                DELIVERED.with(|seen| seen.borrow().last().cloned()),
                Some(RecordSaveStatus::Saved)
            );
        }
        pub(super) fn close(&mut self) {
            let reports = tilo_ui::component_lifecycle::close_components(&mut self.tile);
            assert!(!reports.is_empty());
            assert!(reports.iter().all(|report| report.record_error.is_none()
                && report.view_error.is_none()
                && report.teardown_error.is_none()));
            let delivered = DELIVERED.with(|seen| seen.borrow().len());
            self.tile.sync(&mut self.outer, &ROOT);
            assert_eq!(DELIVERED.with(|seen| seen.borrow().len()), delivered);
            STORE.with(|store| *store.borrow_mut() = None);
        }
    }
}

mod paired_local {
    use super::*;
    use automerge::{AutoCommit, ReadDoc};
    use std::cell::RefCell;
    use tilo_ui::application::{AppComponent, ApplicationDefinition};
    use tilo_ui::application_view::{ApplicationFocusRestore, ApplicationLocalView};
    use tilo_ui::view::ViewContribution;
    use tilo_ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

    #[derive(Default)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "independent capture/finalizer faults and focus policy/state in a contract fixture"
    )]
    struct Presentation {
        scroll: f64,
        payload: u8,
        fail_capture: bool,
        fail_finish: bool,
        own_focus: bool,
        focused: bool,
        events: Vec<String>,
    }
    thread_local! { static STATE: RefCell<Rc<RefCell<Presentation>>> = RefCell::default(); }
    struct LocalRoot(Rc<RefCell<Presentation>>);
    impl Widget for LocalRoot {
        tilo_ui::provides!(
            dyn tilo_ui::view::capability::ViewState,
            dyn tilo_ui::undo::UndoPoints
        );

        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &tilo_ui::Theme,
            _: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            tree.new_leaf(taffy::Style::default()).unwrap()
        }
        fn apply_layout(
            &mut self,
            _: &taffy::TaffyTree<()>,
            _: taffy::NodeId,
            _: vello::kurbo::Point,
        ) {
        }
        fn draw(
            &mut self,
            _: &mut vello::Scene,
            _: vello::kurbo::Affine,
            _: &mut tilo_ui::TextRenderer,
            _: &tilo_ui::Theme,
            _: &tilo_ui::presence::PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Local pair"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn is_focusable(&self) -> bool {
            true
        }
        fn is_focused(&self) -> bool {
            self.0.borrow().focused
        }
        fn set_focus(&mut self, focused: bool) {
            self.0.borrow_mut().focused = focused;
        }
    }

    impl tilo_ui::undo::UndoPoints for LocalRoot {
        fn get_state(&self) -> Option<String> {
            Some("current caret".into())
        }
        fn set_state(&mut self, state: &str) {
            self.0
                .borrow_mut()
                .events
                .push(format!("ephemeral:{state}"));
        }
    }

    impl tilo_ui::view::capability::ViewState for LocalRoot {
        fn view_state(&self) -> Option<ViewContribution> {
            Some(ViewContribution {
                scroll: vec![self.0.borrow().scroll],
                view_mode: None,
            })
        }
        fn restore_view_state(&mut self, view: &ViewContribution) {
            let mut state = self.0.borrow_mut();
            state.events.push("generic".into());
            state.scroll = view.scroll[0];
        }
    }
    impl AppWidget for LocalRoot {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Local pair"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
        fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
            let mut state = self.0.borrow_mut();
            state.events.push("capture".into());
            if state.fail_capture {
                return Err(ThawError::State("second contribution refused".into()));
            }
            Ok(Some(ApplicationLocalView {
                app_id: APP.into(),
                version: 1,
                payload: vec![state.payload],
            }))
        }
        #[allow(
            clippy::panic_in_result_fn,
            reason = "assert the immutable record loan at the application contract boundary"
        )]
        fn restore_application_view(
            &mut self,
            local: &ApplicationLocalView,
            doc: &AutoCommit,
        ) -> Result<ApplicationFocusRestore, ThawError> {
            assert_eq!(
                doc.get(ROOT, "body").unwrap().unwrap().0.as_str(),
                Some("current canonical body")
            );
            let mut state = self.0.borrow_mut();
            state.events.push("prepare".into());
            state.payload = local.payload[0];
            Ok(if state.own_focus {
                ApplicationFocusRestore::Application
            } else {
                ApplicationFocusRestore::Generic
            })
        }
        fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
            let mut state = self.0.borrow_mut();
            state.events.push("finish".into());
            if state.fail_finish {
                return Err(ThawError::State("finish refused".into()));
            }
            Ok(())
        }
    }
    #[allow(
        clippy::needless_pass_by_value,
        reason = "ComponentFactory has an owned target signature"
    )]
    fn factory(
        mut context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_record(|doc| {
                    doc.put(ROOT, "body", "current canonical body")
                        .map_err(|e| e.to_string())
                })
                .map_err(LaunchError::Record)?;
        }
        Ok(AppComponent::new(
            context,
            Box::new(LocalRoot(STATE.with(|state| state.borrow().clone()))),
            tilo_actions::ActionRegistry::new(),
        ))
    }
    fn fixture() -> (ProbeHost, ApplicationDefinition, Rc<RefCell<Presentation>>) {
        let host = ProbeHost::new();
        let state = Rc::new(RefCell::new(Presentation {
            scroll: 12.0,
            payload: 7,
            ..Default::default()
        }));
        STATE.with(|slot| *slot.borrow_mut() = state.clone());
        (host, definition(factory), state)
    }
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
    )]
    #[test]
    fn paired_local_views_bracket_restore_and_keep_complete_pairs_on_failure() {
        // Case `second_capture_failure_retains_complete_previous_pair_during_owned_close`.
        {
            let (host, definition, state) = fixture();
            let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
            let mut component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            let mut saved = envelope(DEVICE, ROOM, 12.0);
            saved.app_id = APP.into();
            saved.record_id = component.record().id.as_str().into();
            saved.record_kind = component.record().kind.as_str().into();
            saved.application = Some(ApplicationLocalView {
                app_id: APP.into(),
                version: 1,
                payload: vec![7],
            });
            host.views.save(&saved).unwrap();
            let bytes = std::fs::read(host.view_path(11)).unwrap();
            state.borrow_mut().scroll = 90.0;
            state.borrow_mut().payload = 99;
            state.borrow_mut().fail_capture = true;
            assert!(component.capture_local_view().is_err());
            let report = component.close();
            assert!(report.view_error.is_some());
            assert!(report.view_retry.is_none());
            let retained = report.recovery.unwrap();
            assert_eq!(retained.view.scroll, vec![12.0]);
            assert_eq!(retained.application.unwrap().payload, vec![7]);
            assert_eq!(std::fs::read(host.view_path(11)).unwrap(), bytes);
            assert!(component.capture_local_view().is_err());
        }

        // Case `local_hooks_bracket_generic_restore_and_clear_only_uncorrelated_ephemeral`.
        {
            for own_focus in [false, true] {
                let (host, definition, state) = fixture();
                let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
                let mut first = definition
                    .instantiate(context, LaunchTarget::New, RestoreInput::None)
                    .unwrap();
                assert_eq!(state.borrow().events, ["capture"]);
                let mut saved = first.close().recovery.unwrap();
                saved.view = local_view(44.0, "old caret");
                saved.view.viewport.width = 333.0;
                saved.view.viewport.height = 222.0;
                state.borrow_mut().events.clear();
                state.borrow_mut().own_focus = own_focus;
                let target = existing(first.record());
                let context = host.prepare(&definition, 11, &target).unwrap();
                let before = host
                    .records
                    .read(&first.record().id, |doc| {
                        (doc.base_heads(), doc.pending_ops())
                    })
                    .unwrap();
                let mut restored = definition
                    .instantiate(context, target, RestoreInput::Local(saved))
                    .unwrap();
                let expected = if own_focus {
                    vec!["prepare", "generic", "finish", "capture"]
                } else {
                    vec![
                        "prepare",
                        "generic",
                        "ephemeral:old caret",
                        "finish",
                        "capture",
                    ]
                };
                assert_eq!(state.borrow().events, expected);
                assert_eq!(restored.capture_local_view().unwrap().viewport.width, 333.0);
                assert_eq!(
                    host.records
                        .read(&first.record().id, |doc| (
                            doc.base_heads(),
                            doc.pending_ops()
                        ))
                        .unwrap(),
                    before
                );
                assert!(restored.close().view_error.is_none());
            }
        }

        // Case `failed_finalizer_retains_setup_record_and_original_view_bytes`.
        {
            let (host, definition, state) = fixture();
            let context = host.prepare(&definition, 11, &LaunchTarget::New).unwrap();
            let mut first = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            let saved = first.close().recovery.unwrap();
            let bytes = std::fs::read(host.view_path(11)).unwrap();
            state.borrow_mut().fail_finish = true;
            let target = existing(first.record());
            let context = host.prepare(&definition, 11, &target).unwrap();
            let error = definition
                .instantiate(context, target, RestoreInput::Local(saved))
                .err()
                .unwrap();
            assert!(matches!(error, LaunchError::ComponentSetup { .. }));
            assert_eq!(std::fs::read(host.view_path(11)).unwrap(), bytes);
        }
    }
}
