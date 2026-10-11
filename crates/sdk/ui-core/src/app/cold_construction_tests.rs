//! Factory ordering and worker-custody regressions only; native authority
//! must additionally be proved through the real enrolled-owner broker fixture.
use super::*;
use std::cell::Cell;

#[test]
fn failed_host_preparation_never_calls_the_component_factory() {
    let factories = Cell::new(0_usize);
    let result = App::from_prepared_component_with_collaboration(
        CollaborationManager::disabled(),
        || Err(crate::launch::LaunchError::Operation("read refused".into())),
        |_, _, _| {
            factories.set(factories.get().saturating_add(1));
            Err(crate::launch::LaunchError::Operation("factory ran".into()))
        },
    );
    assert!(result.is_err());
    assert_eq!(factories.get(), 0);
}

#[test]
fn nested_construction_cannot_replace_the_original_worker() {
    let original = crate::collaboration::native_initial::NativeConstruction::enter(
        CollaborationManager::disabled(),
    )
    .unwrap();
    let preparations = Cell::new(0_usize);
    let result = App::from_prepared_component_with_collaboration(
        CollaborationManager::disabled(),
        || {
            preparations.set(preparations.get().saturating_add(1));
            Err(crate::launch::LaunchError::Operation("prepare ran".into()))
        },
        |_, _, _| Err(crate::launch::LaunchError::Operation("factory ran".into())),
    );
    assert!(result.is_err());
    assert_eq!(preparations.get(), 0);
    assert!(original.check().is_ok());
}

#[test]
fn absent_existing_record_cannot_reach_a_default_new_factory() {
    use crate::application::{ApplicationDefinition, ApplicationHost, ComponentRunnerInputs};
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use std::{cell::RefCell, rc::Rc};
    let directory = tempfile::tempdir().unwrap();
    let definition = ApplicationDefinition {
        app_id: "dev.tilo.cold-construction",
        title: "Cold construction probe",
        icon: crate::widget::IconDescriptor::Glyph("C"),
        record_kind: RecordKind::new("cold.probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: |_, _, _| {
            Err(crate::launch::LaunchError::Operation(
                "definition factory ran".into(),
            ))
        },
    };
    let checkout = RecordCheckout::new(RecordId::for_tab(55), definition.record_kind.clone());
    let factories = Cell::new(0_usize);
    let result = App::from_prepared_component_with_collaboration(
        CollaborationManager::disabled(),
        || {
            let mut host = ApplicationHost::new(
                directory.path().into(),
                "device".into(),
                "room",
                crate::permissions::UserId(crate::permissions::LOCAL_USER.into()),
                Rc::new(RefCell::new(
                    crate::permissions::PermissionContext::permissive(),
                )),
                tilo_actions::ActionContext::default(),
            )?;
            host.prepare_record(
                &definition,
                55,
                checkout.clone(),
                ComponentRunnerInputs {
                    theme: Theme::dark(),
                    text_renderer: crate::font_catalog::fixture_renderer(),
                },
            )
        },
        |_, _, _| {
            factories.set(factories.get().saturating_add(1));
            Err(crate::launch::LaunchError::Operation(
                "default factory ran".into(),
            ))
        },
    );
    let Err(crate::launch::LaunchError::ComponentSetup { record, .. }) = result else {
        panic!("absence must preserve the original record and fail before creation");
    };
    assert_eq!(*record, checkout);
    assert_eq!(factories.get(), 0);
}

#[test]
fn required_native_custody_binds_provenance_before_the_factory() {
    use crate::application::{ApplicationDefinition, ApplicationHost, ComponentRunnerInputs};
    use crate::record::{RecordCheckout, RecordId, RecordKind};
    use std::{cell::RefCell, rc::Rc};
    let directory = tempfile::tempdir().unwrap();
    let definition = ApplicationDefinition {
        app_id: "dev.tilo.cold-custody",
        title: "Cold custody probe",
        icon: crate::widget::IconDescriptor::Glyph("C"),
        record_kind: RecordKind::new("cold.probe"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: |_, _, _| {
            Err(crate::launch::LaunchError::Operation(
                "unused factory".into(),
            ))
        },
    };
    let checkout = RecordCheckout::new(RecordId::for_tab(55), definition.record_kind.clone());
    let mut manager = CollaborationManager::disabled();
    manager.require_document_authorization();
    let factories = Cell::new(0_usize);
    let result = App::from_prepared_component_with_collaboration(
        manager,
        || {
            let mut host = ApplicationHost::new(
                directory.path().into(),
                "device".into(),
                "db6424c5-d68b-4cbe-b4f5-2a9739938fb1",
                crate::permissions::UserId(crate::permissions::LOCAL_USER.into()),
                Rc::new(RefCell::new(
                    crate::permissions::PermissionContext::permissive(),
                )),
                tilo_actions::ActionContext::default(),
            )?;
            host.records_for(&definition)?
                .create_record(&checkout)
                .map_err(crate::launch::LaunchError::Record)?;
            host.prepare_record(
                &definition,
                55,
                checkout.clone(),
                ComponentRunnerInputs {
                    theme: Theme::dark(),
                    text_renderer: crate::font_catalog::fixture_renderer(),
                },
            )
        },
        |context, target, restore| {
            factories.set(factories.get().saturating_add(1));
            let binding = context.callback_binding();
            let provenance = binding
                .records
                .invocation_provenance(&binding.checkout.id)
                .unwrap();
            assert!(
                provenance.borrow().is_required(),
                "a required native publication cannot reach a factory with optional provenance"
            );
            binding
                .records
                .with_action_state(&binding.checkout.id, |document, _, _| {
                    use automerge::transaction::Transactable;
                    let baseline = document.base_heads();
                    let clean =
                        collab_types::DocumentMutation::Incremental(document.save_after(&baseline));
                    assert!(
                        provenance
                            .borrow()
                            .capture(document, &baseline, &clean)
                            .unwrap()
                            .is_some()
                    );
                    document
                        .put(automerge::ROOT, "body", "unscoped factory edit")
                        .unwrap();
                    document.commit();
                    let mutation =
                        collab_types::DocumentMutation::Incremental(document.save_after(&baseline));
                    assert_eq!(
                        provenance
                            .borrow()
                            .capture(document, &baseline, &mutation)
                            .unwrap_err(),
                        "canonical changes have no original invocation contributor"
                    );
                })
                .unwrap();
            crate::application_test_support::probe_factory(context, target, restore)
        },
    );
    let Err(crate::launch::LaunchError::ComponentSetup { source, record, .. }) = result else {
        panic!("an unscoped factory edit must preserve recovery and refuse the mount");
    };
    assert_eq!(*record, checkout);
    assert_eq!(
        source.to_string(),
        "canonical changes have no original invocation contributor"
    );
    assert_eq!(factories.get(), 1);
}
