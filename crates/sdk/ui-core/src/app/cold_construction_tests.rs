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
