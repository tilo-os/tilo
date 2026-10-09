// Included in the real action-door fixture module.
use std::rc::Rc;

struct HostedModelTransport(Rc<RefCell<Vec<CollabRequest>>>);
impl crate::collaboration::PeerTransport for HostedModelTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.0.borrow_mut().push(request);
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        vec![]
    }
    // Legacy owner-route delivery is explicit; no native capture is synthesized.
    fn join_route(&mut self, _: crate::collaboration::RouteSpec) -> Result<(), String> {
        Ok(())
    }
    fn send_to_route(
        &mut self,
        _: crate::collaboration::RouteSpec,
        request: CollabRequest,
    ) -> Result<(), String> {
        self.send(request)
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn hosted_model_broker_alone_emits_acceptance() {
    let request = remote_request(870, "async-bump");
    let (mut app, _, _, _temp) = owned_counter(CounterCard::with_async_post_bump_reset());
    let sent = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    app.require_published_invocation_authorizer();
    app.install_published_invocation_authorizer(original_remote_authorizer(
        request.clone(),
        original_remote_grant(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(99))),
    ))
    .unwrap();
    deliver_remote(&mut app, &request);
    assert!(
        app.actions
            .directed_invocations
            .contains_key(&request.invocation)
    );
    assert!(
        !sent.borrow().iter().any(|request| matches!(
            request,
            CollabRequest::UpdateInvocation {
                update: tilo_actions::InvocationUpdate::Accepted { .. },
                ..
            }
        )),
        "the broker already owns acceptance before delivering this invocation"
    );
}

#[test]
fn hosted_model_advances_retained_job_once_without_draw_or_private_fixture_drain() {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(CounterCard::with_async_post_bump_reset()),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    let sent = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    let invocation = tilo_actions::InvocationId::new();
    app.invoke_published_action(
        871,
        invocation,
        "test.counter".into(),
        "async-bump".into(),
        None,
        "original-caller",
        None,
    );
    assert_eq!(counter(&app), 0);
    app.pump_hosted_model();
    assert_eq!(
        counter(&app),
        1,
        "a hosted model pump must advance accepted work"
    );
    // Poll runs before model work, so cancellation wins before a continuation.
    // A completion produced by this frame is emitted on the next bounded pass.
    assert!(app.actions.directed_invocations.contains_key(&invocation));
    app.pump_hosted_model();
    assert!(!app.actions.directed_invocations.contains_key(&invocation));
    app.pump_hosted_model();
    assert_eq!(counter(&app), 1, "the completed job must not run again");
    assert_eq!(sent.borrow().iter().filter(|request| matches!(request,
        CollabRequest::UpdateInvocation { update: tilo_actions::InvocationUpdate::Succeeded { invocation: observed, .. }, .. } if *observed == invocation
    )).count(), 1, "one actual completion emits exactly one terminal update");
}

fn hosted_directory_fixture() -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(DeclaredDirectory {
            domain_provider: None,
            calls: Rc::new(Cell::new(0)),
            semantic_calls: Rc::new(Cell::new(0)),
            count: 2,
        }),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.collaboration.manager = admitted_delivery_manager();
    app.init_layout();
    app.pump_collab_presence();
    app.publish_action_surface();
    app
}

#[test]
fn hosted_directory_noop_model_keeps_revision_and_actual_cursor() {
    let mut app = hosted_directory_fixture();
    let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
    assert_eq!(page.rows.len(), 1);
    assert!(!page.next_cursor.is_empty());
    app.pump_hosted_model();
    assert_eq!(
        app.action_directory_revision(),
        page.revision,
        "a real no-op hosted sync must preserve its published directory revision"
    );
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    let next = app.action_directory_page(query);
    assert!(!next.stale_cursor);
    assert_eq!(next.rows.len(), 1);
}

#[test]
fn hosted_directory_sync_contract_and_hierarchy_changes_invalidate_revision() {
    for change in ["changed-contract", "hierarchy"] {
        let mut app = hosted_directory_fixture();
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        // The change arrives from elsewhere; the hosted pass syncs it.
        app.state.doc.put(ROOT, change, true).unwrap();
        app.pump_hosted_model();
        assert_ne!(
            app.action_directory_revision(),
            page.revision,
            "fresh changed projection must never inherit old revision"
        );
        let mut query = directory_query(&page.revision, "", 1, 8192, 8);
        query.cursor = page.next_cursor;
        assert!(app.action_directory_page(query).stale_cursor);
        let current = app.action_directory_page(directory_query("", "declared:0", 8, 8192, 8));
        if change == "hierarchy" {
            assert_eq!(current.rows.len(), 2);
        } else {
            let contract = &current.rows[0].visibility.as_ref().unwrap().contract;
            assert_eq!(
                contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
        }
    }
}

#[test]
fn hosted_directory_provider_generation_change_remains_stale() {
    let provider = LargeDirectoryProvider::new(1);
    let catalog = provider.catalog.clone();
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(CounterCard::with_directory_provider(provider)),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.collaboration.manager = admitted_delivery_manager();
    app.init_layout();
    app.pump_collab_presence();
    app.publish_action_surface();
    let old = app.action_directory_revision();
    catalog
        .upsert(
            "test.item:1",
            "New record",
            crate::search::SearchResult::for_action(
                "Large directory",
                "1",
                tilo_actions::ActionAddress::new("test.item:1", "open"),
            ),
        )
        .unwrap();
    app.pump_hosted_model();
    assert_ne!(app.action_directory_revision(), old);
}

#[test]
fn hosted_directory_hidden_immediate_target_change_stays_stale() {
    let mut app = hosted_directory_fixture();
    let old = app.action_directory_revision();
    let before = tilo_actions::door::survey(&app.publication_resources());
    app.state.doc.put(ROOT, "changed-payload", true).unwrap();
    app.pump_hosted_model();
    assert_eq!(
        tilo_actions::door::survey(&app.publication_resources()),
        before,
        "public descriptors deliberately omit the changed encoded target"
    );
    assert_ne!(
        app.action_directory_revision(),
        old,
        "an identical public descriptor cannot preserve a changed actual invocation"
    );
}

crate::testing::test_app_widget!(
    DeclaredDirectory,
    "Declared directory",
    icon = crate::widget::IconDescriptor::Glyph("D")
);

fn owned_declared_directory_factory(
    mut context: crate::application::HostContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| doc.put(ROOT, "seed", true).map_err(|e| e.to_string()))
        .map_err(crate::launch::LaunchError::Record)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<CounterAction>(COUNTER_TYPE);
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(DeclaredDirectory {
            domain_provider: None,
            calls: Rc::new(Cell::new(0)),
            semantic_calls: Rc::new(Cell::new(0)),
            count: 2,
        }),
        registry,
    ))
}

#[test]
fn hosted_directory_known_actual_owner_noop_keeps_revision_and_current_counter() {
    use crate::application::{
        ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners
        .open("test.directory", "00000000-0000-0000-0000-000000000004")
        .unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "D",
            "00000000-0000-0000-0000-000000000004",
        )
        .unwrap(),
    );
    let definition = ApplicationDefinition {
        app_id: "test.directory",
        title: "Directory",
        icon: crate::widget::IconDescriptor::Glyph("D"),
        record_kind: crate::record::RecordKind::new("directory"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: owned_declared_directory_factory,
    };
    let context = HostContext::prepare(
        &definition,
        HostContextInputs {
            records,
            views,
            device_id: "D".into(),
            instance_id: 81,
            permissions: Rc::new(RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            author: crate::permissions::UserId("fixture-user".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding();
    let inputs = crate::application::ComponentRunnerInputs {
        theme: Theme::dark(),
        text_renderer: crate::font_catalog::fixture_renderer(),
    };
    let mut app = App::from_component_using(component, inputs, |_, _| {
        AppConstructionInputs::controlled()
    })
    .unwrap();
    app.collaboration.manager = admitted_delivery_manager();
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: binding.broker_route().unwrap(),
        epoch: uuid::Uuid::new_v4(),
    });
    app.init_layout();
    app.pump_collab_presence();
    app.publish_action_surface();
    let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
    assert_eq!(page.rows.len(), 1);
    let before = crate::bound_dispatch::directory_witness(app.root_widget.as_ref(), &app.state.doc)
        .unwrap()[0]
        .generation
        .unwrap();
    app.pump_hosted_model();
    let after = crate::bound_dispatch::directory_witness(app.root_widget.as_ref(), &app.state.doc)
        .unwrap()[0]
        .generation
        .unwrap();
    assert!(
        after > before,
        "actual mutable no-op loans advance the bookkeeping counter"
    );
    assert_eq!(
        app.action_directory_revision(),
        page.revision,
        "same actual owner with complete fresh invocation evidence remains usable"
    );
    assert_eq!(
        app.actions
            .action_directory_stamp
            .as_ref()
            .unwrap()
            .0
            .owners[0]
            .generation,
        Some(after),
        "restore must retain the new owner counter"
    );
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    let next = app.action_directory_page(query);
    assert!(!next.stale_cursor);
    assert_eq!(next.rows.len(), 1);
    let current = &app
        .actions
        .action_directory_stamp
        .as_ref()
        .unwrap()
        .0
        .owners[0];
    for generation in [None, Some(0), Some(u64::MAX)] {
        let mut invalid = current.clone();
        invalid.generation = generation;
        assert!(
            !invalid.same_model_owner_after(current),
            "unknown/regressing/exhausted counters refuse"
        );
    }
}

#[derive(Clone)]
struct DirectoryDomainProvider {
    target: Rc<Cell<u8>>,
    supported: Rc<Cell<bool>>,
}
impl crate::search::SearchProvider for DirectoryDomainProvider {
    fn name(&self) -> &'static str {
        "domain-provider"
    }
    fn search(&self, _: &str) -> Vec<crate::search::SearchResult> {
        vec![crate::search::SearchResult::for_action(
            "lazy",
            "Lazy",
            tilo_actions::ActionAddress::new("lazy:target", "inspect"),
        )]
    }
    fn action_directory_generation(&self) -> Option<u64> {
        Some(0)
    }
    fn action_directory_invocation_domain(&self) -> crate::widget::ActionDirectoryInvocationDomain {
        if !self.supported.get() {
            return crate::widget::ActionDirectoryInvocationDomain::Unsupported;
        }
        crate::widget::ActionDirectoryInvocationDomain::versioned(
            &tilo_actions::ContinuationEvidence::Draft {
                resolution_domain: tilo_actions::ContinuationKey::new(
                    "lazy-domain",
                    [vec![self.target.get()]],
                ),
                handler: tilo_actions::ContinuationKey::new("lazy-handler", [b"v1".as_slice()]),
            },
        )
    }
    fn resolve_action_resource(&self, address: &tilo_actions::ActionAddress) -> Option<Resource> {
        if address.resource != "lazy:target" || address.action != "inspect" {
            return None;
        }
        Some(
            Resource::new("lazy:target", "lazy", "Lazy").with_action(
                ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    OperationKind::View,
                    COUNTER_TYPE,
                    &if self.target.get() == 0 {
                        CounterAction::Bump
                    } else {
                        CounterAction::Reset
                    },
                )
                .unwrap()
                .with_contract(tilo_actions::ActionContract::view()),
            ),
        )
    }
}

#[test]
fn hosted_directory_lazy_target_domain_versions_even_with_unchanged_search_generation() {
    let provider = DirectoryDomainProvider {
        target: Rc::new(Cell::new(0)),
        supported: Rc::new(Cell::new(true)),
    };
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(DeclaredDirectory {
            calls: Rc::new(Cell::new(0)),
            semantic_calls: Rc::new(Cell::new(0)),
            count: 2,
            domain_provider: Some(provider.clone()),
        }),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.collaboration.manager = admitted_delivery_manager();
    app.init_layout();
    app.pump_collab_presence();
    app.publish_action_surface();
    let old = app.action_directory_revision();
    app.pump_hosted_model();
    assert_eq!(
        app.action_directory_revision(),
        old,
        "truthful unchanged lazy domain remains usable"
    );
    let before = tilo_actions::door::survey(&app.publication_resources());
    provider.target.set(1);
    app.pump_hosted_model();
    assert_eq!(
        tilo_actions::door::survey(&app.publication_resources()),
        before
    );
    assert_ne!(
        app.action_directory_revision(),
        old,
        "same search generation cannot hide changed lazy target"
    );
    let changed = app.action_directory_revision();
    provider.supported.set(false);
    app.pump_hosted_model();
    assert_ne!(
        app.action_directory_revision(),
        changed,
        "lost domain proof refuses continuity"
    );
}

#[test]
fn hosted_directory_prompted_domain_handler_and_unknown_changes_stay_stale() {
    for change in [
        "changed-prompt-domain",
        "changed-prompt-handler",
        "lost-prompt-identity",
        "unknown-domain",
        "changed-domain",
    ] {
        let mut app = hosted_directory_fixture();
        app.state.doc.put(ROOT, "prompted", true).unwrap();
        app.pump_hosted_model();
        let old = app.action_directory_revision();
        app.pump_hosted_model();
        assert_eq!(
            app.action_directory_revision(),
            old,
            "explicit stable Draft identity must progress"
        );
        let before = tilo_actions::door::survey(&app.publication_resources());
        app.state.doc.put(ROOT, change, true).unwrap();
        app.pump_hosted_model();
        assert_eq!(
            tilo_actions::door::survey(&app.publication_resources()),
            before
        );
        assert_ne!(
            app.action_directory_revision(),
            old,
            "changed {change} cannot inherit old revision"
        );
    }
}

#[test]
fn hosted_directory_registry_epoch_and_supported_large_surface() {
    let mut app = hosted_directory_fixture();
    app.state.doc.put(ROOT, "wide", true).unwrap();
    app.pump_hosted_model();
    let count = tilo_actions::door::survey(&app.publication_resources()).len();
    assert!(count > 256);
    let old = app.action_directory_revision();
    app.pump_hosted_model();
    assert_eq!(app.action_directory_revision(), old);
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.pump_hosted_model();
    assert_ne!(
        app.action_directory_revision(),
        old,
        "mutable registry access is an independent epoch"
    );
}

struct HostedDirectoryNotificationTransport {
    events: Rc<RefCell<Vec<CollabEvent>>>,
    before_poll: Option<Box<dyn FnOnce()>>,
}
impl crate::collaboration::PeerTransport for HostedDirectoryNotificationTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        if let Some(change) = self.before_poll.take() {
            change();
        }
        std::mem::take(&mut *self.events.borrow_mut())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}
#[test]
fn hosted_directory_equivalent_notification_before_sync_preserves_offered_revision() {
    let mut app = hosted_directory_fixture();
    let events = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
            events: events.clone(),
            before_poll: None,
        });
    app.publish_action_surface();
    let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
    assert_eq!(page.rows.len(), 1);
    events.borrow_mut().push(CollabEvent::Context {
        collaborators: vec![],
        permissions: vec![],
    });
    app.pump_hosted_model();
    assert!(
        events.borrow().is_empty(),
        "actual inbound pump must drain the notification"
    );
    assert_eq!(
        app.action_directory_revision(),
        page.revision,
        "equivalent notifications must not evict the offer before model comparison begins"
    );
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    let next = app.action_directory_page(query);
    assert!(!next.stale_cursor);
    assert_eq!(next.rows.len(), 1);
}

#[test]
fn hosted_directory_permission_expiry_or_failure_inside_notification_cannot_restore() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    for clock_failure in [false, true] {
        let mut app = hosted_directory_fixture();
        let time = Arc::new(AtomicU64::new(100));
        let failed = Arc::new(AtomicBool::new(false));
        let sample_time = time.clone();
        let sample_failed = failed.clone();
        app.permission_ctx = crate::permissions::PermissionContext::granted(
            "fixture-user",
            &crate::permissions::TreePath::root(),
            crate::permissions::policy::KindSet::ALL,
            Some(110),
        )
        .at_unix_time(100)
        .with_policy_time_sampler(Arc::new(move || {
            if sample_failed.load(Ordering::Acquire) {
                Err("actual clock retired".into())
            } else {
                Ok(sample_time.load(Ordering::Acquire))
            }
        }));
        app.invalidate_widget_resource_cache();
        let events = Rc::new(RefCell::new(vec![CollabEvent::Context {
            collaborators: vec![],
            permissions: vec![],
        }]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
                events,
                before_poll: Some(Box::new(move || {
                    if clock_failure {
                        failed.store(true, Ordering::Release)
                    } else {
                        time.store(110, Ordering::Release)
                    }
                })),
            });
        app.publish_action_surface();
        let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
        assert_eq!(
            page.rows.len(),
            1,
            "the original sole grant admits the actual offer"
        );
        app.pump_hosted_model();
        assert_ne!(app.action_directory_revision(), page.revision);
        assert!(!matches!(
            app.resolve_action_directory(&page.revision, "declared:0", "inspect"),
            runtime_collab::ActionDirectoryResolution::Found(_)
        ));
    }
}

#[test]
fn hosted_directory_hidden_target_change_with_notification_cannot_restore() {
    let mut app = hosted_directory_fixture();
    let events = Rc::new(RefCell::new(vec![CollabEvent::Context {
        collaborators: vec![],
        permissions: vec![],
    }]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedDirectoryNotificationTransport {
            events,
            before_poll: None,
        });
    app.publish_action_surface();
    let page = app.action_directory_page(directory_query("", "", 1, 8192, 8));
    let before = tilo_actions::door::survey(&app.publication_resources());
    app.state.doc.put(ROOT, "changed-payload", true).unwrap();
    app.pump_hosted_model();
    assert_eq!(
        tilo_actions::door::survey(&app.publication_resources()),
        before
    );
    assert_ne!(app.action_directory_revision(), page.revision);
}
