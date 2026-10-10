// Included in `semantic_tree_app_tests` beside hosted_model_tests.rs: the
// dirty-tracked hosted worker pump keeps idle passes cheap while every real
// model, notification, invocation, commit, inbound and policy change still
// refreshes resources and the published action surface.

/// Widget state (label) arrives asynchronously and is applied in `sync`,
/// document state (`more`) is read by `resources`, exactly like app workers.
struct HostedPumpProbe {
    calls: std::rc::Rc<Cell<usize>>,
    syncs: std::rc::Rc<Cell<usize>>,
    pending_label: std::rc::Rc<RefCell<Option<String>>>,
    label: String,
    actions: usize,
}
impl Widget for HostedPumpProbe {
    crate::provides!(dyn crate::resources::PublishesResources);

    crate::testing::inert_widget_methods!(
        unregistered_leaf_layout,
        apply_layout,
        draw,
        get_rect,
        name = "hosted-pump-probe"
    );
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {
        self.syncs.set(self.syncs.get() + 1);
        if let Some(label) = self.pending_label.borrow_mut().take() {
            self.label = label;
        }
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[COUNTER_TYPE]
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        assert_eq!(request.kind, OperationKind::View);
        assert_eq!(request.type_id, COUNTER_TYPE);
        *self.pending_label.borrow_mut() = Some("After".into());
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::resources::PublishesResources for HostedPumpProbe {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
        self.calls.set(self.calls.get() + 1);
        let count = if doc.get(ROOT, "more").unwrap().is_some() {
            3
        } else {
            1
        };
        (0..count)
            .map(|index| {
                let mut resource = Resource::new(
                    format!("probe:{index}"),
                    "probe",
                    format!("{} {index}", self.label),
                );
                for action in 0..self.actions {
                    resource = resource.with_action(
                        ActionSpec::operation(
                            format!("inspect:{action}"),
                            "Inspect",
                            OperationKind::View,
                            COUNTER_TYPE,
                            &CounterAction::Bump,
                        )
                        .unwrap()
                        .with_contract(tilo_actions::ActionContract::view()),
                    );
                }
                resource
            })
            .collect()
    }
}

struct HostedPumpFixture {
    app: App,
    wake: crate::wake::WakeHandle,
    calls: std::rc::Rc<Cell<usize>>,
    syncs: std::rc::Rc<Cell<usize>>,
    pending_label: std::rc::Rc<RefCell<Option<String>>>,
    sent: std::rc::Rc<RefCell<Vec<CollabRequest>>>,
    events: std::rc::Rc<RefCell<Vec<CollabEvent>>>,
    directory: std::rc::Rc<RefCell<Vec<crate::collaboration::ActionDirectoryRequest>>>,
}

struct HostedPumpTransport {
    sent: std::rc::Rc<RefCell<Vec<CollabRequest>>>,
    events: std::rc::Rc<RefCell<Vec<CollabEvent>>>,
    directory: std::rc::Rc<RefCell<Vec<crate::collaboration::ActionDirectoryRequest>>>,
}
impl crate::collaboration::PeerTransport for HostedPumpTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        self.sent.borrow_mut().push(request);
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
    fn take_action_directory_requests(
        &mut self,
    ) -> Vec<crate::collaboration::ActionDirectoryRequest> {
        std::mem::take(&mut *self.directory.borrow_mut())
    }
}

impl HostedPumpFixture {
    /// An application worker: unbound instance wake scope, no event loop.
    fn new(actions: usize) -> Self {
        let scope = crate::wake::WakeScope::new();
        let wake = scope.handle();
        let calls = std::rc::Rc::new(Cell::new(0));
        let syncs = std::rc::Rc::new(Cell::new(0));
        let pending_label = std::rc::Rc::new(RefCell::new(None));
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            scope,
            State::new(),
            Box::new(HostedPumpProbe {
                calls: calls.clone(),
                syncs: syncs.clone(),
                pending_label: pending_label.clone(),
                label: "Before".into(),
                actions,
            }),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        let sent = std::rc::Rc::new(RefCell::new(vec![]));
        let events = std::rc::Rc::new(RefCell::new(vec![]));
        let directory = std::rc::Rc::new(RefCell::new(vec![]));
        app.collaboration.manager =
            CollaborationManager::with_peer_transport(HostedPumpTransport {
                sent: sent.clone(),
                events: events.clone(),
                directory: directory.clone(),
            });
        app.init_layout();
        let mut fixture = Self {
            app,
            wake,
            calls,
            syncs,
            pending_label,
            sent,
            events,
            directory,
        };
        // Construction wakes and the first publication settle here.
        fixture.app.pump_hosted_model_as(false);
        fixture.app.pump_hosted_model_as(false);
        fixture
    }

    fn pump(&mut self) -> model_lifecycle::HostedPumpReport {
        self.app.pump_hosted_model_as(false)
    }

    fn labels(&self) -> Vec<String> {
        self.app
            .actions
            .last_published_actions
            .as_ref()
            .unwrap()
            .iter()
            .map(|descriptor| descriptor.resource_label.clone())
            .collect()
    }

    fn assert_idle(&mut self) {
        let generation = self.app.actions.resource_generation.get();
        let calls = self.calls.get();
        let syncs = self.syncs.get();
        let attempts = self.app.actions.action_surface_publication_attempts;
        let revision = self.app.action_directory_revision();
        let report = self.pump();
        assert_eq!(report, model_lifecycle::HostedPumpReport::default());
        assert_eq!(self.app.actions.resource_generation.get(), generation);
        assert_eq!(self.calls.get(), calls, "idle pump must not rematerialize");
        assert_eq!(self.syncs.get(), syncs, "idle pump must not sync");
        assert_eq!(
            self.app.actions.action_surface_publication_attempts, attempts,
            "idle pump must not rebuild the action surface"
        );
        assert_eq!(self.app.action_directory_revision(), revision);
    }
}

#[test]
fn hosted_pump_repairs_invalidated_snapshot_before_queued_directory_reply() {
    let mut fixture = HostedPumpFixture::new(1);
    let requests = Rc::new(RefCell::new(Vec::new()));
    fixture.app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedPumpTransport {
            sent: fixture.sent.clone(),
            events: fixture.events.clone(),
            directory: requests.clone(),
        });
    fixture.pump();
    let first = fixture
        .app
        .action_directory_page(directory_query("", "", 8, 8192, 8));
    assert_eq!(first.rows.len(), 1);
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.app.invalidate_widget_resource_cache();
    let (reply, answer) = tokio::sync::oneshot::channel();
    requests
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query {
            query: directory_query("", "", 8, 8192, 8),
            reply,
        });
    fixture.app.pump_collab_presence();
    let page = answer.blocking_recv().unwrap();
    assert_eq!(
        page.rows.len(),
        3,
        "queued pull sees the current host publication"
    );
    assert!(page.exhausted && !page.budget_exhausted);
    assert_ne!(page.revision, first.revision);
    let calls = fixture.calls.get();
    let attempts = fixture.app.actions.action_surface_publication_attempts;
    fixture.app.pump_collab_presence();
    assert_eq!(
        fixture.calls.get(),
        calls,
        "idle does not redeclare the tree"
    );
    assert_eq!(
        fixture.app.actions.action_surface_publication_attempts,
        attempts
    );
}

#[test]
fn hosted_pump_idle_keeps_resource_cache_and_published_surface() {
    let mut fixture = HostedPumpFixture::new(1);
    assert_eq!(fixture.labels(), vec!["Before 0".to_string()]);
    for _ in 0..3 {
        fixture.assert_idle();
    }
    assert!(fixture.app.actions.resource_cache.borrow().is_some());
}

#[test]
fn completed_view_syncs_its_local_projection_before_returning_to_idle() {
    let mut fixture = HostedPumpFixture::new(1);
    fixture.assert_idle();
    let before = fixture.app.state.doc.save();
    let address = tilo_actions::ActionAddress::new("probe:0", "inspect:0");
    let actor = fixture.app.collaboration.local_author.0.clone();
    let started = fixture
        .app
        .invoke_resource_action(ResourceInvocation::new(&address, &actor))
        .unwrap();
    assert!(matches!(
        started,
        crate::widget::ResourceOperationStart::Completed(_)
    ));
    assert_eq!(
        fixture.app.state.doc.save(),
        before,
        "navigation writes no document state"
    );
    fixture.pump();
    assert_eq!(
        fixture.labels(),
        ["After 0"],
        "the native worker applies the completed view's queued local projection without another input"
    );
    fixture.assert_idle();
}

#[test]
fn hosted_pump_idle_keeps_directory_cursor() {
    let mut fixture = HostedPumpFixture::new(1);
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.wake.wake();
    fixture.pump();
    fixture.pump();
    let page = fixture
        .app
        .action_directory_page(directory_query("", "", 1, 8192, 8));
    assert_eq!(page.rows.len(), 1);
    assert!(!page.next_cursor.is_empty());
    fixture.assert_idle();
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    let next = fixture.app.action_directory_page(query);
    assert!(
        !next.stale_cursor,
        "idle pump must retain the issued cursor"
    );
    assert_eq!(next.rows.len(), 1);
}

#[test]
fn hosted_pump_async_notification_wake_refreshes_widget_resources() {
    let mut fixture = HostedPumpFixture::new(1);
    *fixture.pending_label.borrow_mut() = Some("After".into());
    // Without a wake the pump cannot know (same contract as the window
    // runner, whose redraw syncs only on a model-dirty wake).
    fixture.assert_idle();
    fixture.wake.wake();
    let generation = fixture.app.actions.resource_generation.get();
    let report = fixture.pump();
    assert!(report.synced && report.published);
    assert_ne!(fixture.app.actions.resource_generation.get(), generation);
    assert_eq!(fixture.labels(), vec!["After 0".to_string()]);
    fixture.assert_idle();
}

#[test]
fn hosted_pump_queue_only_notification_updates_without_forced_sync() {
    let mut fixture = HostedPumpFixture::new(1);
    let syncs = fixture.syncs.get();
    fixture.wake.notify_events();
    let report = fixture.pump();
    assert!(
        report.updated,
        "queue notification drains update-driven work"
    );
    assert!(!report.synced, "a queue wake alone is not model-dirty");
    assert_eq!(fixture.syncs.get(), syncs);
    fixture.assert_idle();
}

#[test]
fn hosted_pump_inbound_event_refreshes_remote_document_state() {
    let mut fixture = HostedPumpFixture::new(1);
    // Remote document bytes enter only with an inbound collaboration event.
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.events.borrow_mut().push(CollabEvent::Context {
        collaborators: vec![],
        permissions: vec![],
    });
    let report = fixture.pump();
    assert!(report.synced && report.published);
    assert_eq!(fixture.labels().len(), 3);
    fixture.assert_idle();
}

#[test]
fn idle_nearby_snapshot_reply_does_not_request_a_hosted_pump() {
    let mut fixture = HostedPumpFixture::new(1);
    fixture
        .events
        .borrow_mut()
        .push(CollabEvent::NearbyTransferCommandResult {
            request_id: 1,
            result: Ok(crate::collaboration::NearbyTransferReply::Snapshot {
                snapshot: crate::collaboration::NearbyTransferSnapshot::default(),
            }),
        });
    assert!(
        !fixture.app.pump_collab_presence(),
        "an unchanged Nearby snapshot must not keep requesting frames"
    );
    fixture.assert_idle();
}

#[test]
fn hosted_pump_invocation_advances_and_publishes_then_idles() {
    let scope = crate::wake::WakeScope::new();
    let mut app = App::from_scoped_parts_with_controlled_inputs(
        scope,
        State::new(),
        Box::new(CounterCard::with_async_post_bump_reset()),
    );
    app.action_registry_mut()
        .register::<CounterAction>(COUNTER_TYPE);
    app.init_layout();
    let sent = std::rc::Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    app.pump_hosted_model_as(false);
    app.pump_hosted_model_as(false);
    let invocation = tilo_actions::InvocationId::new();
    app.invoke_published_action(
        872,
        invocation,
        "test.counter".into(),
        "async-bump".into(),
        None,
        "original-caller",
        None,
    );
    assert!(app.pump_hosted_model_as(false).synced);
    assert_eq!(counter(&app), 1, "accepted work advances without a wake");
    app.pump_hosted_model_as(false);
    assert!(!app.actions.directed_invocations.contains_key(&invocation));
    assert_eq!(sent.borrow().iter().filter(|request| matches!(request,
        CollabRequest::UpdateInvocation { update: tilo_actions::InvocationUpdate::Succeeded { invocation: observed, .. }, .. } if *observed == invocation
    )).count(), 1);
    app.pump_hosted_model_as(false);
    let generation = app.actions.resource_generation.get();
    assert_eq!(
        app.pump_hosted_model_as(false),
        model_lifecycle::HostedPumpReport::default(),
        "after completion the worker returns to idle"
    );
    assert_eq!(app.actions.resource_generation.get(), generation);
    assert_eq!(counter(&app), 1);
}

#[test]
fn hosted_pump_policy_change_and_expiry_refresh() {
    use crate::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let mut fixture = HostedPumpFixture::new(1);
    let time = Arc::new(AtomicU64::new(100));
    let sample = time.clone();
    fixture.app.permission_ctx = PermissionContext::from_capabilities(
        Actor::person(None, "fixture-user"),
        vec![
            CapOverride::grant(
                Subject::person("fixture-user"),
                Scope::Workspace,
                Verb::Admin,
            )
            .with_expiry(110),
        ],
    )
    .with_policy_time_sampler(Arc::new(move || Ok(sample.load(Ordering::Acquire))));
    let generation = fixture.app.actions.resource_generation.get();
    assert!(fixture.pump().synced, "a replaced policy refreshes");
    assert_ne!(fixture.app.actions.resource_generation.get(), generation);
    fixture.assert_idle();
    time.store(105, Ordering::Release);
    fixture.assert_idle();
    time.store(110, Ordering::Release);
    assert!(
        fixture.pump().synced,
        "capability expiry is a policy change"
    );
    fixture.assert_idle();
}

struct OwnerAuthorityFixture {
    permissions: Rc<RefCell<crate::permissions::PermissionContext>>,
    current: Arc<std::sync::Mutex<bool>>,
    deadline: Arc<std::sync::atomic::AtomicU64>,
}

thread_local! {
    static HOSTED_COUNTER_DECLARATION: RefCell<Option<Rc<Cell<bool>>>> = const {
        RefCell::new(None)
    };
}

fn owned_counter_component(
    mut context: crate::application::HostContext,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|doc| doc.put(ROOT, "seed", true).map_err(|e| e.to_string()))
        .map_err(crate::launch::LaunchError::Record)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<CounterAction>(COUNTER_TYPE);
    let declaration = HOSTED_COUNTER_DECLARATION.with_borrow(Clone::clone);
    let mut counter = CounterCard::new();
    if let Some(declaration) = declaration {
        counter.declare_bump = declaration;
    }
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(counter),
        registry,
    ))
}

fn owned_counter_factory(
    context: crate::application::HostContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    owned_counter_component(context)
}

struct HostedOwnedCounter {
    _temp: tempfile::TempDir,
    app: App,
    permissions: Rc<RefCell<crate::permissions::PermissionContext>>,
    declaration: Rc<Cell<bool>>,
}

fn hosted_owned_counter() -> HostedOwnedCounter {
    use crate::application::{
        ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners
        .open("test.directory", "00000000-0000-0000-0000-000000000006")
        .unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "D",
            "00000000-0000-0000-0000-000000000006",
        )
        .unwrap(),
    );
    let permissions = Rc::new(RefCell::new(
        crate::permissions::PermissionContext::permissive(),
    ));
    let definition = ApplicationDefinition {
        app_id: "test.directory",
        title: "Directory",
        icon: crate::widget::IconDescriptor::Glyph("D"),
        record_kind: crate::record::RecordKind::new("directory"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: owned_counter_factory,
    };
    let declaration = Rc::new(Cell::new(true));
    HOSTED_COUNTER_DECLARATION.with_borrow_mut(|slot| {
        assert!(slot.replace(Rc::clone(&declaration)).is_none());
    });
    let context = HostContext::prepare(
        &definition,
        HostContextInputs {
            records,
            views,
            device_id: "D".into(),
            instance_id: 83,
            permissions: Rc::clone(&permissions),
            author: crate::permissions::UserId("fixture-user".into()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    )
    .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    HOSTED_COUNTER_DECLARATION.with_borrow_mut(|slot| {
        assert!(slot.take().is_some());
    });
    let binding = component.callback_binding();
    let inputs = crate::application::ComponentRunnerInputs {
        theme: Theme::dark(),
        text_renderer: crate::font_catalog::fixture_renderer(),
    };
    let mut app = App::from_component_using(component, inputs, |_, _| {
        AppConstructionInputs::controlled()
    })
    .unwrap();
    app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: binding.broker_route().unwrap(),
        epoch: uuid::Uuid::new_v4(),
    });
    app.init_layout();
    let mut settled = false;
    for _ in 0..8 {
        app.collaboration.manager.wait_for_commands_for_test();
        if app.pump_hosted_model_as(false) == model_lifecycle::HostedPumpReport::default() {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "hosted counter never reached its publication-ready idle state"
    );
    app.collaboration.manager.wait_for_commands_for_test();
    HostedOwnedCounter {
        _temp: temp,
        app,
        permissions,
        declaration,
    }
}

fn restrict_counter_owner_to_read(fixture: &HostedOwnedCounter) {
    use crate::permissions::{Actor, CapOverride, Effect, Scope, Subject, Verb};
    let mut ceiling = CapOverride::grant(
        Subject::person("fixture-user"),
        Scope::App("test.directory".into()),
        Verb::Read,
    );
    ceiling.effect = Effect::Restrict;
    *fixture.permissions.borrow_mut() = crate::permissions::PermissionContext::from_capabilities(
        Actor::person(Some(crate::permissions::Role::Owner), "fixture-user"),
        vec![ceiling],
    );
}

#[test]
fn hosted_owner_read_ceiling_keeps_read_publication_and_disables_write_control() {
    let mut fixture = hosted_owned_counter();
    restrict_counter_owner_to_read(&fixture);
    let report = fixture.app.pump_hosted_model_as(false);
    assert!(report.synced && report.published);
    fixture.app.refresh_control_action_availability();
    let button = fixture
        .app
        .layout
        .node_map
        .values()
        .copied()
        .find(|widget| unsafe { (**widget).test_id() == Some("bump") })
        .expect("the retained bound Write button remains in the mounted view");
    assert!(!unsafe { (*button).is_enabled() });
    let published = fixture
        .app
        .actions
        .last_published_actions
        .as_ref()
        .expect("the restricted owner still publishes its readable surface");
    assert!(
        published
            .iter()
            .any(|action| action.address.action == "inspect")
    );
    assert!(
        published
            .iter()
            .all(|action| action.address.action != "bump")
    );
}

#[test]
fn hosted_undeclared_bound_control_still_withdraws_the_surface() {
    let mut fixture = hosted_owned_counter();
    assert!(
        fixture
            .app
            .actions
            .last_published_actions
            .as_ref()
            .is_some_and(|actions| !actions.is_empty())
    );
    fixture.declaration.set(false);
    fixture.app.invalidate_widget_resource_cache();
    fixture.app.pump_hosted_model_as(false);
    assert!(
        fixture
            .app
            .actions
            .last_published_actions
            .as_ref()
            .is_some_and(Vec::is_empty)
    );
}

fn live_owner_authority() -> OwnerAuthorityFixture {
    let current = Arc::new(std::sync::Mutex::new(true));
    let deadline = Arc::new(std::sync::atomic::AtomicU64::new(
        crate::time::try_unix_now_ms().unwrap() + 60_000,
    ));
    let sampled_current = Arc::clone(&current);
    let sampled_deadline = Arc::clone(&deadline);
    let permissions = crate::permissions::PermissionContext::permissive()
        .with_live_action_authority(
            "test.directory".into(),
            Arc::new(move || {
                if *sampled_current.lock().unwrap() {
                    Ok(crate::permissions::PermissionContext::permissive())
                } else {
                    sampled_deadline.store(0, Ordering::Release);
                    Err("fixture owner authority revoked".into())
                }
            }),
            Arc::clone(&deadline),
        );
    OwnerAuthorityFixture {
        permissions: Rc::new(RefCell::new(permissions)),
        current,
        deadline,
    }
}

fn assert_bound_owner_authority_sources(
    app: &mut App,
    owner_deadline: &Arc<std::sync::atomic::AtomicU64>,
) {
    assert_eq!(
        app.permission_ctx,
        crate::permissions::PermissionContext::permissive()
    );
    assert!(!app.permission_ctx.has_live_action_authority());
    assert!(app.has_live_hosted_action_authority());
    assert_eq!(
        app.hosted_action_authority_deadline_unix_ms(),
        Some(owner_deadline.load(Ordering::Acquire))
    );
    let outer_deadline = Arc::new(std::sync::atomic::AtomicU64::new(
        owner_deadline.load(Ordering::Acquire) - 1_000,
    ));
    app.permission_ctx = crate::permissions::PermissionContext::permissive()
        .with_live_action_authority(
            "outer.test".into(),
            Arc::new(|| Ok(crate::permissions::PermissionContext::permissive())),
            Arc::clone(&outer_deadline),
        );
    assert_eq!(
        app.hosted_action_authority_deadline_unix_ms(),
        Some(outer_deadline.load(Ordering::Acquire)),
        "the event loop arms the earliest independent authority boundary"
    );
    app.permission_ctx = crate::permissions::PermissionContext::permissive();
}

#[test]
fn hosted_pump_host_owner_permission_replacement_refreshes() {
    use crate::application::{
        ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
    };
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners
        .open("test.directory", "00000000-0000-0000-0000-000000000005")
        .unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "D",
            "00000000-0000-0000-0000-000000000005",
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
    let authority = live_owner_authority();
    let context = HostContext::prepare(
        &definition,
        HostContextInputs {
            records,
            views,
            device_id: "D".into(),
            instance_id: 82,
            permissions: authority.permissions.clone(),
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
    assert_bound_owner_authority_sources(&mut app, &authority.deadline);
    app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: binding.broker_route().unwrap(),
        epoch: uuid::Uuid::new_v4(),
    });
    app.init_layout();
    // The first pass joins the record route, whose initial catalogue arrives
    // only once the broker worker has processed the join: settle on the
    // worker, not on a count of passes.
    let mut settled = false;
    for _ in 0..8 {
        app.collaboration.manager.wait_for_commands_for_test();
        if app.pump_hosted_model_as(false) == model_lifecycle::HostedPumpReport::default() {
            settled = true;
            break;
        }
    }
    assert!(settled, "hosted pump never went idle");
    app.collaboration.manager.wait_for_commands_for_test();
    let before = tilo_actions::door::survey(&app.publication_resources()).len();
    assert!(before > 0);
    let generation = app.actions.resource_generation.get();
    assert_eq!(
        app.pump_hosted_model_as(false),
        model_lifecycle::HostedPumpReport::default()
    );
    assert_eq!(app.actions.resource_generation.get(), generation);
    // What the retained native owner sampler observes after host revocation.
    let retained_scope = binding.lifetime_id;
    let document = app.state.doc.save();
    *authority.current.lock().unwrap() = false;
    let report = app.pump_hosted_model_as(false);
    assert!(report.synced && report.published);
    assert_eq!(app.hosted_action_authority_deadline_unix_ms(), None);
    assert!(app.has_live_hosted_action_authority());
    assert_eq!(
        app.root_widget.callback_binding().unwrap().lifetime_id,
        retained_scope
    );
    assert_eq!(app.state.doc.save(), document);
    assert_ne!(app.actions.resource_generation.get(), generation);
    assert!(
        tilo_actions::door::survey(&app.publication_resources()).len() < before,
        "revoked authority must filter the published surface"
    );
}

#[test]
fn hosted_pump_baseline_always_refreshes() {
    let mut fixture = HostedPumpFixture::new(1);
    let generation = fixture.app.actions.resource_generation.get();
    let report = fixture.app.pump_hosted_model_as(true);
    assert!(report.synced && report.published);
    assert_ne!(fixture.app.actions.resource_generation.get(), generation);
    assert!(!fixture.sent.borrow().is_empty());
}

/// `cargo test -p tilo-ui --lib hosted_pump_cost -- --ignored --nocapture`
#[test]
#[ignore = "timing measurement"]
fn hosted_pump_cost_idle_before_after() {
    const PUMPS: u32 = 200;
    let mut fixture = HostedPumpFixture::new(40);
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.wake.wake();
    fixture.pump();
    fixture.pump();
    for (name, always) in [("baseline", true), ("dirty-tracked", false)] {
        let started = std::time::Instant::now();
        for _ in 0..PUMPS {
            fixture.app.pump_hosted_model_as(always);
        }
        let per = started.elapsed() / PUMPS;
        eprintln!("hosted pump {name}: {per:?} per idle pump ({PUMPS} pumps, 3x40 actions)");
    }
}

#[test]
fn hosted_pump_action_authority_revoke_refreshes_without_forced_sync() {
    use crate::permissions::{Actor, PermissionContext, Role};
    use std::sync::{Arc, Mutex};
    let current = Arc::new(Mutex::new(Some(Role::Owner)));
    let sample = current.clone();
    let deadline = Arc::new(std::sync::atomic::AtomicU64::new(
        crate::time::try_unix_now_ms().unwrap() + 90_000,
    ));
    let sampled_deadline = Arc::clone(&deadline);
    let mut fixture = HostedPumpFixture::new(1);
    fixture.app.permission_ctx = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Editor), "fixture-user"),
        vec![],
    )
    .with_live_action_authority(
        "test.directory".into(),
        Arc::new(move || {
            let role = *sample.lock().unwrap();
            if role.is_none() {
                sampled_deadline.store(0, Ordering::Release);
            }
            Ok(PermissionContext::from_capabilities(
                Actor::person(role, "fixture-user"),
                vec![],
            ))
        }),
        Arc::clone(&deadline),
    );
    assert!(fixture.app.has_live_hosted_action_authority());
    assert_eq!(
        fixture.app.hosted_action_authority_deadline_unix_ms(),
        Some(deadline.load(Ordering::Acquire))
    );
    assert!(fixture.pump().synced);
    fixture.assert_idle();
    *current.lock().unwrap() = None;
    let generation = fixture.app.actions.resource_generation.get();
    assert!(
        fixture.pump().synced,
        "ordinary worker observes revoked owner authority"
    );
    assert_ne!(fixture.app.actions.resource_generation.get(), generation);
    assert_eq!(fixture.app.hosted_action_authority_deadline_unix_ms(), None);
    fixture.assert_idle();
}

/// The broker uses an event-only wake for pull-directory requests. Route the
/// real queue through the owner pump, rather than calling the page helper alone.
#[test]
fn hosted_directory_rpc_wake_preserves_no_change_cursor_and_rejects_model_change() {
    let mut fixture = HostedPumpFixture::new(1);
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.wake.wake();
    fixture.pump();
    fixture.pump();
    let before = fixture.app.state.doc.save();
    let mut query = directory_query("", "", 1, 8192, 8);
    let mut revision = None;
    let mut rows = Vec::new();
    for _ in 0..4 {
        let (reply, mut response) = tokio::sync::oneshot::channel();
        fixture
            .directory
            .borrow_mut()
            .push(crate::collaboration::ActionDirectoryRequest::Query {
                query: query.clone(),
                reply,
            });
        fixture.wake.notify_events();
        let report = fixture.pump();
        let page = response
            .try_recv()
            .expect("owner pump answers the actual queued request");
        assert!(
            !page.stale_cursor,
            "metadata-only owner wakes keep consecutive pages valid"
        );
        assert!(
            !report.synced,
            "directory event wake alone must not force model sync"
        );
        assert!(
            !report.published,
            "directory reads must not republish declarations"
        );
        if let Some(revision) = &revision {
            assert_eq!(&page.revision, revision);
        } else {
            revision = Some(page.revision.clone());
        }
        rows.extend(page.rows);
        if page.exhausted {
            break;
        }
        query.revision = page.revision;
        query.cursor = page.next_cursor;
    }
    assert_eq!(
        rows.len(),
        3,
        "all declared roots appear once across real queue turns"
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.resource.as_str())
            .collect::<Vec<_>>(),
        ["probe:0", "probe:1", "probe:2"]
    );
    assert_eq!(fixture.app.state.doc.save(), before);
    let page = fixture
        .app
        .action_directory_page(directory_query("", "", 1, 8192, 8));
    assert!(!page.next_cursor.is_empty());
    *fixture.pending_label.borrow_mut() = Some("Changed".into());
    fixture.wake.wake();
    assert!(fixture.pump().synced);
    let (reply, mut response) = tokio::sync::oneshot::channel();
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    fixture
        .directory
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query { query, reply });
    fixture.wake.notify_events();
    fixture.pump();
    assert!(
        response.try_recv().unwrap().stale_cursor,
        "a real changed model still invalidates the old cursor"
    );
}

#[test]
fn cancelled_directory_rpc_does_not_consume_undelivered_cursor() {
    let mut fixture = HostedPumpFixture::new(1);
    fixture.app.state.doc.put(ROOT, "more", true).unwrap();
    fixture.wake.wake();
    fixture.pump();
    fixture.pump();
    let cursors = fixture.app.actions.action_directory_cursors.len();
    let (reply, response) = tokio::sync::oneshot::channel();
    drop(response);
    fixture
        .directory
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query {
            query: directory_query("", "", 1, 8192, 8),
            reply,
        });
    fixture.wake.notify_events();
    let report = fixture.pump();
    assert!(!report.synced && !report.published);
    assert_eq!(
        fixture.app.actions.action_directory_cursors.len(),
        cursors,
        "a cancelled root request must not allocate an undeliverable cursor"
    );
    let page = fixture
        .app
        .action_directory_page(directory_query("", "", 1, 8192, 8));
    assert!(!page.next_cursor.is_empty());
    let mut query = directory_query(&page.revision, "", 1, 8192, 8);
    query.cursor = page.next_cursor;
    let (reply, response) = tokio::sync::oneshot::channel();
    drop(response);
    fixture
        .directory
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query {
            query: query.clone(),
            reply,
        });
    fixture.wake.notify_events();
    let report = fixture.pump();
    assert!(!report.synced && !report.published);
    let (reply, mut response) = tokio::sync::oneshot::channel();
    fixture
        .directory
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query {
            query: query.clone(),
            reply,
        });
    fixture.wake.notify_events();
    fixture.pump();
    let next = response.try_recv().unwrap();
    assert!(
        !next.stale_cursor,
        "a request cancelled before owner admission must not consume the offered continuation"
    );
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows.first().unwrap().resource, "probe:1");
    let (reply, mut response) = tokio::sync::oneshot::channel();
    fixture
        .directory
        .borrow_mut()
        .push(crate::collaboration::ActionDirectoryRequest::Query { query, reply });
    fixture.wake.notify_events();
    fixture.pump();
    assert!(
        response.try_recv().unwrap().stale_cursor,
        "a delivered continuation remains strictly one-use"
    );
}

/// Bare pointer motion keeps the published resource tree; every change an
/// event can make to it still republishes within that event (TIL-628).
#[cfg(feature = "native")]
mod input_publication {
    use super::*;
    use std::rc::Rc;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, MouseButton};

    /// Publishes one resource whose label shows every piece of state an
    /// input can move: a press switches its tab without marking the event
    /// handled; hovering past x = 500 opens a menu, reported as a
    /// presentation change; motion with a button held drags, unreported;
    /// hovering below y = 500 writes the document. Releasing `t` switches
    /// its tab and marks the release handled; releasing `w` writes the
    /// document; releasing any other key changes nothing.
    struct InputProbe {
        calls: Rc<Cell<usize>>,
        tab: usize,
        menu_open: bool,
        pressed: bool,
        drag_x: Option<f64>,
    }

    impl Widget for InputProbe {
        crate::provides!(dyn crate::resources::PublishesResources);

        crate::testing::inert_widget_methods!(
            unregistered_leaf_layout,
            apply_layout,
            draw,
            get_rect,
            name = "input-publication-probe"
        );

        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &[COUNTER_TYPE]
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            match event {
                WindowEvent::MouseInput { state, .. } => {
                    self.pressed = *state == ElementState::Pressed;
                    if self.pressed {
                        self.tab += 1;
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    if self.pressed {
                        self.drag_x = Some(position.x);
                    }
                    if position.x > 500.0 && !self.menu_open {
                        self.menu_open = true;
                        crate::widget::note_resource_presentation_changed();
                    }
                    if position.y > 500.0 {
                        ctx.state.document_mut().put(ROOT, "hovered", true).unwrap();
                    }
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Released =>
                {
                    if event.logical_key == winit::keyboard::Key::Character("t".into()) {
                        self.tab += 1;
                        *ctx.handled = true;
                    } else if event.logical_key == winit::keyboard::Key::Character("w".into()) {
                        ctx.state.document_mut().put(ROOT, "hovered", true).unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    impl crate::resources::PublishesResources for InputProbe {
        fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource> {
            self.calls.set(self.calls.get() + 1);
            let hovered = doc.get(ROOT, "hovered").unwrap().is_some();
            let label = format!(
                "tab {} menu {} drag {:?} hovered {hovered}",
                self.tab, self.menu_open, self.drag_x
            );
            vec![
                Resource::new("probe", "probe", label).with_action(
                    ActionSpec::operation(
                        "inspect",
                        "Inspect",
                        OperationKind::View,
                        COUNTER_TYPE,
                        &CounterAction::Bump,
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
                ),
            ]
        }
    }

    struct Surface;

    impl AppSurface for Surface {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(1000, 1000)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _: CursorIcon) {}
    }

    fn app() -> (App, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let mut app = App::from_scoped_parts_with_controlled_inputs(
            crate::wake::WakeScope::new(),
            State::new(),
            Box::new(InputProbe {
                calls: calls.clone(),
                tab: 0,
                menu_open: false,
                pressed: false,
                drag_x: None,
            }),
        );
        app.action_registry_mut()
            .register::<CounterAction>(COUNTER_TYPE);
        app.init_layout();
        (app, calls)
    }

    fn move_to(app: &mut App, x: f64, y: f64) {
        app.surface_input(
            &Surface,
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(x, y),
            },
            false,
        );
    }

    fn button(app: &mut App, state: ElementState) {
        app.surface_input(
            &Surface,
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
            false,
        );
    }

    fn release(app: &mut App, key: &str) {
        app.surface_input(
            &Surface,
            crate::synthetic_input::key_input(
                &winit::keyboard::Key::Character(key.into()),
                ElementState::Released,
            ),
            false,
        );
    }

    /// The probe's label in the tree an agent reads now, with no frame or
    /// sync in between.
    fn published(app: &App) -> String {
        app.collect_resources()
            .into_iter()
            .find(|resource| resource.id.as_str() == "probe")
            .expect("the probe stays published")
            .label
    }

    #[test]
    fn pointer_motion_without_state_change_does_not_regenerate_declarations() {
        let (mut app, calls) = app();
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        let generation = app.actions.resource_generation.get();
        let before = calls.get();
        for step in 0..100 {
            move_to(&mut app, f64::from(step), 20.0);
        }
        let regenerated = calls.get() - before;
        assert!(
            regenerated <= 1,
            "100 hover moves regenerated declarations {regenerated} times"
        );
        assert_eq!(app.actions.resource_generation.get(), generation);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        assert_eq!(
            calls.get() - before,
            regenerated,
            "reading the kept tree regenerates nothing"
        );
    }

    #[test]
    fn presentation_only_change_still_republishes() {
        let (mut app, _) = app();
        move_to(&mut app, 10.0, 20.0);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        // The probe switches tabs on a press it does not mark handled.
        button(&mut app, ElementState::Pressed);
        button(&mut app, ElementState::Released);
        assert_eq!(published(&app), "tab 1 menu false drag None hovered false");
    }

    #[test]
    fn hover_that_reports_a_presentation_change_republishes() {
        let (mut app, _) = app();
        move_to(&mut app, 10.0, 20.0);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        move_to(&mut app, 600.0, 20.0);
        assert_eq!(published(&app), "tab 0 menu true drag None hovered false");
    }

    #[test]
    fn hover_that_edits_the_document_republishes() {
        let (mut app, _) = app();
        move_to(&mut app, 10.0, 20.0);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        move_to(&mut app, 10.0, 600.0);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered true");
    }

    #[test]
    fn drag_motion_republishes_without_being_reported() {
        let (mut app, _) = app();
        move_to(&mut app, 10.0, 20.0);
        button(&mut app, ElementState::Pressed);
        assert_eq!(published(&app), "tab 1 menu false drag None hovered false");
        move_to(&mut app, 50.0, 20.0);
        assert_eq!(
            published(&app),
            "tab 1 menu false drag Some(50.0) hovered false"
        );
        button(&mut app, ElementState::Released);
        move_to(&mut app, 70.0, 20.0);
        assert_eq!(
            published(&app),
            "tab 1 menu false drag Some(50.0) hovered false",
            "released, motion is hover again"
        );
    }

    /// Every keystroke ends in a release. One that nothing consumed and that
    /// moved nothing keeps the tree its press published (TIL-751).
    #[test]
    fn key_release_without_state_change_does_not_regenerate_declarations() {
        let (mut app, calls) = app();
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        let generation = app.actions.resource_generation.get();
        let before = calls.get();
        for _ in 0..100 {
            release(&mut app, "k");
        }
        let regenerated = calls.get() - before;
        assert!(
            regenerated <= 1,
            "100 key releases regenerated declarations {regenerated} times"
        );
        assert_eq!(app.actions.resource_generation.get(), generation);
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
    }

    #[test]
    fn consumed_key_release_still_republishes() {
        let (mut app, _) = app();
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        release(&mut app, "t");
        assert_eq!(published(&app), "tab 1 menu false drag None hovered false");
    }

    #[test]
    fn key_release_that_edits_the_document_republishes() {
        let (mut app, _) = app();
        assert_eq!(published(&app), "tab 0 menu false drag None hovered false");
        release(&mut app, "w");
        assert_eq!(published(&app), "tab 0 menu false drag None hovered true");
    }
}
