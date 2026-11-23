//! Factory ordering and worker-ownership regressions only; native authority
//! must additionally be proved through the real enrolled-owner broker fixture.
use super::*;
use std::cell::Cell;

tilo_test_env::scenario! {
    fn cold_construction_never_reaches_a_factory_it_must_not;

    phase failed_host_preparation_never_calls_the_component_factory;
    {
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

    phase nested_construction_cannot_replace_the_original_worker;
    {
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

    phase absent_existing_record_cannot_reach_a_default_new_factory;
    {
        use crate::application::{ApplicationDefinition, ApplicationOwner, ComponentRunnerInputs};
        use crate::resource::{DocumentCheckout, DocumentId, ResourceType};
        let directory = tempfile::tempdir().unwrap();
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.cold-construction".into(),
            title: "Cold construction probe".into(),
            icon: crate::widget::IconDescriptor::Glyph("C"),
            resource_type: ResourceType::new("cold.probe"),
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: |_, _, _| {
                Err(crate::launch::LaunchError::Operation(
                    "definition factory ran".into(),
                ))
            },
        };
        let checkout = DocumentCheckout::new(DocumentId::for_tab(55), definition.resource_type.clone());
        let factories = Cell::new(0_usize);
        let result = App::from_prepared_component_with_collaboration(
            CollaborationManager::disabled(),
            || {
                let mut host = ApplicationOwner::new(
                    directory.path().into(),
                    "device".into(),
                    "workspace",
                    crate::permissions::MemberId::person(crate::permissions::LOCAL_USER),
                    crate::permissions::SharedPermissions::new(
                        crate::permissions::PermissionContext::owner(),
                    ),
                    tilo_actions::ActionContext::default(),
                )?;
                host.prepare_resource(
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
}
