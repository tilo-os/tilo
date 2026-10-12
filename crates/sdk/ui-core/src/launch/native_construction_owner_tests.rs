mod construction_owner {
    use super::*;
    use crate::application::{
        HostContext, HostContextInputs, LaunchTarget, PreparedComponentInputs, RestoreInput,
    };
    use crate::application_test_support as probe;
    use automerge::{ReadDoc, transaction::Transactable};
    thread_local! {
        static REFRESH: std::cell::RefCell<Option<Box<dyn FnMut() -> usize>>> = const { std::cell::RefCell::new(None) };
        static FACTORIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static RETIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn prepare(
        definition: &crate::application::ApplicationDefinition,
        args: &LaunchArgs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let root = std::path::PathBuf::from(args.value("--fixture-root").unwrap());
        let room = args.value("--fixture-room").unwrap();
        let id = crate::record::RecordId::parse(args.value("--fixture-record").unwrap()).unwrap();
        let mut owners = crate::record::RecordOwners::new(root.clone());
        let records = owners
            .open(definition.app_id, room)
            .map_err(LaunchError::Record)?;
        let views = std::rc::Rc::new(
            crate::application_view::InstanceViewStore::new(root, probe::DEVICE, room)
                .map_err(LaunchError::Restore)?,
        );
        let target = LaunchTarget::Record {
            checkout: crate::record::RecordCheckout::new(id, definition.record_kind.clone()),
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views,
                device_id: probe::DEVICE.into(),
                instance_id: 11,
                permissions: probe::runner_permissions(),
                author: probe::author(),
                action_context: probe::provenance(),
            },
            &target,
        )?;
        context
            .read_record(|doc| {
                assert_eq!(
                    doc.get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("original before factory")
                )
            })
            .unwrap();
        let inputs = probe::runner_inputs();
        refresh_prepared_owner(&context);
        if RETIRED.get() {
            context.callback_binding().closing.set(true);
        }
        Ok(PreparedComponentInputs {
            context,
            target,
            restore: RestoreInput::None,
            runner_inputs: inputs,
        })
    }

    fn refresh_prepared_owner(context: &HostContext) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while crate::collaboration::native_initial::component_history_phase_for_test(
            &context.callback_binding(),
        )
        .is_some_and(|phase| phase.1)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "original applied capture must leave staging"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut refresh =
            REFRESH.with_borrow_mut(|refresh| refresh.take().expect("original socket fixture"));
        let before = refresh();
        let read = crate::collaboration::native_initial::component_history_read_for_test(
            &context.callback_binding(),
        )
        .unwrap_or_else(|error| panic!("actual owner requests the newer committed capture: {error}, reads={before}->{}, phase={:?}", refresh(), crate::collaboration::native_initial::component_history_phase_for_test(&context.callback_binding())));
        read.with_mutation(|mutation| {
            let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                panic!("full committed owner capture required")
            };
            let doc = automerge::AutoCommit::load(bytes).unwrap();
            assert_eq!(
                doc.get(automerge::ROOT, "body")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("newer authoritative history")
            );
        })
        .unwrap();
        assert_eq!(
            crate::collaboration::native_initial::component_history_phase_for_test(
                &context.callback_binding()
            ),
            Some((true, true, false))
        );
        assert_eq!(FACTORIES.get(), 0);
    }

    fn create(
        context: HostContext,
        target: LaunchTarget,
        restore: RestoreInput,
    ) -> Result<crate::application::AppComponent, LaunchError> {
        FACTORIES.set(FACTORIES.get() + 1);
        context
            .read_record(|doc| {
                assert_eq!(
                    doc.get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("newer authoritative history")
                )
            })
            .unwrap();
        probe::action_probe_factory(context, target, restore)
    }

    #[test]
    #[serial_test::serial]
    fn native_constructor_applies_newer_owner_capture_before_factory() {
        run_case(false, false);
    }

    #[test]
    #[serial_test::serial]
    fn native_constructor_refuses_foreign_capture_without_factory() {
        run_case(true, false);
    }

    #[test]
    #[serial_test::serial]
    fn native_constructor_refuses_retired_owner_without_factory() {
        run_case(false, true);
    }

    fn fixture_args(
        environment: &LaunchFixtureEnv,
        room: uuid::Uuid,
        id: &crate::record::RecordId,
    ) -> LaunchArgs {
        let raw = vec![
            "app".into(),
            "--fixture-root".into(),
            environment
                ._directory
                .path()
                .join("component")
                .display()
                .to_string(),
            "--fixture-room".into(),
            room.to_string(),
            "--fixture-record".into(),
            id.as_str().into(),
        ];
        parse_args_extended(
            &raw,
            &[],
            &[],
            &[],
            &["--fixture-root", "--fixture-room", "--fixture-record"],
            0,
        )
        .unwrap()
    }

    fn run_case(foreign: bool, retired: bool) {
        FACTORIES.set(0);
        RETIRED.set(retired);
        let environment = LaunchFixtureEnv::new();
        let room = uuid::Uuid::new_v4();
        let id = crate::record::RecordId::minted();
        let mut history = automerge::AutoCommit::new();
        assert!(crate::record::init_scoped(
            &mut history,
            &id,
            &crate::record::RecordKind::new("probe"),
            &room.to_string(),
            Some(&room.to_string())
        ));
        history
            .put(automerge::ROOT, "body", "original before factory")
            .unwrap();
        history.commit();
        let original = history.save();
        if foreign {
            history = automerge::AutoCommit::new();
            crate::record::init_scoped(
                &mut history,
                &crate::record::RecordId::minted(),
                &crate::record::RecordKind::new("probe"),
                &room.to_string(),
                Some(&room.to_string()),
            );
        }
        history
            .put(automerge::ROOT, "body", "newer authoritative history")
            .unwrap();
        history.commit();
        let (manager, probe) =
            crate::collaboration::CollaborationManager::native_refresh_fixture_for_test(
                probe::APP,
                room,
                original,
            );
        let mut changed = Some(history.save());
        REFRESH.with_borrow_mut(|slot| {
            *slot = Some(Box::new(move || {
                if let Some(changed) = changed.take() {
                    probe.replace_history(changed);
                }
                probe.committed_read_count()
            }))
        });
        static DEFINITION: std::sync::OnceLock<crate::application::ApplicationDefinition> =
            std::sync::OnceLock::new();
        let definition = DEFINITION.get_or_init(|| probe::definition(create));
        let spec = LaunchSpec::from_definition(definition)
            .component_host(prepare)
            .native_services()
            .allowed_live_options(&["--fixture-root", "--fixture-room", "--fixture-record"]);
        let args = fixture_args(&environment, room, &id);
        let result = dispatch_definition_with_manager(
            &spec,
            &args,
            |component, inputs, native| {
                assert!(native);
                App::from_component_with_native_services(component, inputs)
            },
            || Ok(manager),
        );
        if foreign || retired {
            let Some(LaunchError::Operation(error)) = result.as_ref().err() else {
                panic!(
                    "expected concrete owner admission refusal: {:?}",
                    result.as_ref().err()
                );
            };
            if foreign {
                assert_eq!(error, "record identity does not match its binding");
            } else {
                assert_eq!(
                    error,
                    "component owner history admission refused: ClosedLifetime"
                );
            }
            assert_eq!(FACTORIES.get(), 0, "refusal must precede the factory");
            return;
        }
        assert!(
            result.is_ok(),
            "current owner capture must be applied before factory: {:?}",
            result.as_ref().err()
        );
        assert_eq!(FACTORIES.get(), 1);
        let DefinitionDispatch::Native(app) = result.unwrap() else {
            panic!("native component expected")
        };
        assert!(app.collaboration_is_connected());
        drop(app);
    }
}
