use super::*;
use crate::bound_dispatch::ToolkitPart as _;
use crate::history_mode::*;
use crate::widget::{AppModel, ModelPath};
use automerge::{ROOT, transaction::Transactable};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

#[path = "peer_delivery_tests.rs"]
mod peer_delivery;

static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.native-history-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Native notebook",
        presentation: HistoryPresentation::Domain {
            create: crate::history_mode::test_preview,
        },
    }],
};
/// An app whose registered framework History has two committed changes.
fn app() -> App {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    app_on(state)
}
/// An app on `state` whose History is the framework's registered History.
fn app_on(state: State) -> App {
    app_with_root(state, Box::new(crate::widget::Column::new()))
}
/// An app on `state` showing `root` under the framework's registered History.
pub(crate) fn app_with_root(state: State, root: Box<dyn crate::widget::Widget>) -> App {
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "native-fixture",
                "lifetime-904",
                HISTORY.app_id,
                crate::DocumentId::for_tab(904),
            ),
        }],
    )
    .unwrap();
    App::from_managed_root_with_controlled_inputs(state, ManagedRoot::new(root, registration))
        .unwrap()
}
/// Lay out and paint one 900×650 frame, as the native runner does.
pub(crate) fn layout(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(app, root, 900, 650, 1.0, true, None).is_some());
    let mut scene = vello::Scene::new();
    draw_frame_content(app, &mut scene, 900, 650, 1.0, None);
    complete_native_frame_content(app, |_| {});
}
/// [`app`] where the host's own chrome presents History, laid out.
/// An app whose platform menus list its History, served the history of its document.
pub(crate) fn native_chrome_app() -> App {
    let mut app = app();
    app.prepare_native_presentation(crate::host_chrome::HostChrome::Native);
    layout(&mut app);
    app
}

fn named(app: &mut App, label: &str) -> accesskit::NodeId {
    app.build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .map(|(id, _)| *id)
        .unwrap()
}
fn node_for_test_id(app: &App, id: &str) -> NodeId {
    app.layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.inspect_node(*node, |widget| widget.test_id() == Some(id))
                .unwrap_or(false)
        })
        .unwrap()
}
fn native_key(app: &mut App, key: &str) {
    for state in [ElementState::Pressed, ElementState::Released] {
        let logical = winit::keyboard::Key::Named(match key {
            "Escape" => winit::keyboard::NamedKey::Escape,
            "Enter" => winit::keyboard::NamedKey::Enter,
            "Tab" => winit::keyboard::NamedKey::Tab,
            _ => panic!("unsupported fixture key"),
        });
        let event = crate::synthetic_input::key_input(&logical, state);
        let framework = app.history_controls_event(&event, 1.0);
        assert!(
            framework,
            "the complete History key frame is intercepted: {key} {state:?}"
        );
        app.dispatch_framework_history_event(&event, 1.0);
        let root = app.layout.root_node.unwrap();
        let _ = run_layout_pass(app, root, 900, 650, 1.0, app.layout.structure_dirty, None);
    }
}

/// `mutation` of `route`'s record, as the device's store sends it.
fn routed_document(
    route: &crate::collaboration::RouteSpec,
    mutation: collab_types::DocumentMutation,
) -> CollabEvent {
    CollabEvent::Document {
        route: route.clone(),
        mutation,
    }
}
#[test]
fn owned_routes_isolate_documents_and_admit_only_owned_bytes() {
    {
        use automerge::ReadDoc;
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::Text::new("root")),
        );
        let installed = crate::collaboration::RouteSpec::app("installed-app", uuid::Uuid::new_v4());
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::AcceptedRoute {
            route: installed.clone(),
            epoch: uuid::Uuid::new_v4(),
        });
        let mut foreign = installed;
        foreign.document = Some("foreign-record".into());
        let mut incoming = automerge::AutoCommit::new();
        incoming.put(ROOT, "foreign-body", "must stay out").unwrap();
        app.apply_document_event(&routed_document(
            &foreign,
            collab_types::DocumentMutation::Snapshot(incoming.save()),
        ));
        assert!(
            app.state.doc.get(ROOT, "foreign-body").unwrap().is_none(),
            "route-foreign envelope must not fall into outer State"
        );
    }

    {
        use automerge::ReadDoc;
        let mut fixture = RoutedOwners::new();
        let outer = fixture.app.state.doc.base_heads();
        let envelopes = [
            fixture.remote(0, "first-remote", false),
            fixture.remote(1, "second-remote", true),
        ];
        for envelope in &envelopes {
            fixture.app.apply_document_event(envelope);
        }
        assert_eq!(fixture.value(0, "first-remote").as_deref(), Some("remote"));
        assert_eq!(fixture.value(1, "second-remote").as_deref(), Some("remote"));
        assert_eq!(fixture.value(1, "first-remote"), None);
        assert_eq!(fixture.value(0, "second-remote"), None);
        assert_eq!(fixture.app.state.doc.base_heads(), outer);
        fixture.app.publish_owned_documents();
        assert_eq!(
            fixture.app.document_routes.sent.len(),
            2,
            "initial installed owners synchronize independently"
        );
        for (route, request) in &fixture.app.document_routes.sent {
            let CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(data),
                ..
            } = request
            else {
                panic!("initial publication is a snapshot")
            };
            let doc = automerge::AutoCommit::load(data).unwrap();
            let index = fixture
                .bindings
                .iter()
                .position(|binding| binding.broker_route().as_ref() == Some(route))
                .unwrap();
            assert!(
                doc.get(
                    ROOT,
                    if index == 0 {
                        "first-remote"
                    } else {
                        "second-remote"
                    }
                )
                .unwrap()
                .is_some()
            );
            assert!(doc.get(ROOT, "outer").unwrap().is_none());
        }
        fixture.app.document_routes.sent.clear();
        fixture
            .records
            .with_history(&fixture.bindings[0].checkout.id, |doc, history| {
                doc.put(ROOT, "local", "first owner only").unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
        let cursor_before = fixture
            .records
            .read_checked(&fixture.bindings[0].checkout.id, |doc| {
                doc.clone().save_incremental()
            })
            .unwrap();
        let changed_clones = crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get();
        fixture.app.publish_owned_documents();
        let cursor_after = fixture
            .records
            .read_checked(&fixture.bindings[0].checkout.id, |doc| {
                doc.clone().save_incremental()
            })
            .unwrap();
        assert_eq!(
            cursor_after, cursor_before,
            "actual delta publication preserves the owner's exact incremental cursor"
        );
        assert_eq!(fixture.app.document_routes.sent.len(), 1);
        assert_eq!(
            crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get(),
            changed_clones + 1,
            "only the changed owner is cloned for publication"
        );
        assert_eq!(
            fixture.app.document_routes.sent[0].0,
            fixture.bindings[0].broker_route().unwrap()
        );
        let clones = crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get();
        fixture.app.publish_owned_documents();
        assert_eq!(
            fixture.app.document_routes.sent.len(),
            1,
            "unchanged heads do not enqueue again"
        );
        assert_eq!(
            crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get(),
            clones,
            "unchanged owned documents must not clone their full Automerge history"
        );

        let reconciles = super::document_routes::DOCUMENT_ROUTE_REFRESHES.get();
        fixture.app.pump_collab_presence();
        assert_eq!(
            super::document_routes::DOCUMENT_ROUTE_REFRESHES.get(),
            reconciles + 1,
            "one app pump reconciles document ownership exactly once"
        );
        assert_eq!(
            crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get(),
            clones,
            "the unchanged pump path does not clone owned documents"
        );
    }

    {
        let mut fixture = RoutedOwners::new();
        let refusal = crate::collaboration::CollabSendError::TooLarge {
            actual: collab_types::MAX_DOCUMENT_TRANSFER_BYTES + 1,
            max: collab_types::MAX_DOCUMENT_TRANSFER_BYTES,
        };
        fixture.app.document_routes.refuse_for_test = Some(refusal);
        fixture.app.publish_owned_documents();
        assert!(fixture.app.document_routes.sent.is_empty());
        let status = fixture
            .app
            .document_sync_status()
            .expect("a refused change is reported")
            .into_owned();
        assert!(status.contains("no longer shared"), "{status}");
        fixture.app.publish_owned_documents();
        assert!(
            fixture.app.document_routes.sent.is_empty(),
            "still refused, and still not marked as sent"
        );

        fixture.app.document_routes.refuse_for_test =
            Some(crate::collaboration::CollabSendError::Busy);
        fixture.app.publish_owned_documents();
        assert!(fixture.app.document_routes.sent.is_empty());

        fixture.app.document_routes.refuse_for_test = None;
        fixture.app.publish_owned_documents();
        assert_eq!(
            fixture.app.document_routes.sent.len(),
            2,
            "the snapshots that were refused are offered again once accepted"
        );
        assert!(
            fixture
                .app
                .document_routes
                .sent
                .iter()
                .all(|(_, request)| matches!(
                    request,
                    CollabRequest::Document {
                        mutation: collab_types::DocumentMutation::Snapshot(_),
                        ..
                    }
                ))
        );
        assert_eq!(
            fixture.app.document_sync_status(),
            None,
            "an accepted change clears the report"
        );
    }

    {
        let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
            "observer",
            "fixture.observer",
        );
        let mut app = App::new_with_controlled_inputs(State::new(), probe.root);
        let foreign =
            crate::collaboration::RouteSpec::document("foreign", uuid::Uuid::new_v4(), "other");
        let mut doc = automerge::AutoCommit::new();
        doc.put(ROOT, "body", "foreign").unwrap();
        let envelope = routed_document(
            &foreign,
            collab_types::DocumentMutation::Snapshot(doc.save()),
        );
        app.apply_collab_event_for_test(envelope);
        assert_eq!(
            probe.collab_events.get(),
            0,
            "generic mutable widgets never receive unadmitted document traffic"
        );
        app.apply_collab_event_for_test(CollabEvent::FollowChanged { peer_id: None });
        assert_eq!(
            probe.collab_events.get(),
            1,
            "normal session notifications remain available"
        );
    }

    {
        let mut fixture = RoutedOwners::new();
        let incoming = fixture.remote(0, "old-epoch", false);
        let binding = fixture.bindings[0].clone();
        fixture
            .records
            .with_action_state(&binding.checkout.id, |doc, _, _| {
                doc.put(ROOT, "draft", "local").unwrap();
            })
            .unwrap();
        fixture.app.apply_document_event(&incoming);
        fixture
            .app
            .actions
            .bound_broker_route_for_test
            .as_mut()
            .unwrap()
            .epoch = uuid::Uuid::new_v4();
        fixture.app.refresh_document_routes();
        fixture
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            })
            .unwrap();
        fixture.app.drain_document_routes();
        assert_eq!(fixture.value(0, "old-epoch"), None);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("connection changed")
        );
        assert_eq!(fixture.value(0, "draft").as_deref(), Some("local"));
    }

    {
        use automerge::ReadDoc;
        let mut fixture = RoutedOwners::new();
        let first = fixture.remote(0, "scrollback-first", false);
        let second = fixture.remote(1, "scrollback-second", true);
        let primary = fixture
            .app
            .actions
            .bound_broker_route_for_test
            .as_ref()
            .unwrap()
            .route
            .clone();
        let mut outer_peer = fixture.app.state.doc.clone();
        outer_peer
            .put(ROOT, "primary-remote", "outer peer")
            .unwrap();
        outer_peer.commit();
        let primary_op = routed_document(
            &primary,
            collab_types::DocumentMutation::Snapshot(outer_peer.save()),
        );
        let mut foreign = primary.clone();
        foreign.app_id = "foreign".into();
        outer_peer
            .put(ROOT, "foreign-remote", "must not enter")
            .unwrap();
        outer_peer.commit();
        let foreign_op = routed_document(
            &foreign,
            collab_types::DocumentMutation::Snapshot(outer_peer.save()),
        );
        for event in [first, foreign_op, second, primary_op] {
            fixture.app.apply_collab_event_for_test(event);
        }
        assert_eq!(
            fixture.value(0, "scrollback-first").as_deref(),
            Some("remote")
        );
        assert_eq!(
            fixture.value(1, "scrollback-second").as_deref(),
            Some("remote")
        );
        assert_eq!(fixture.value(0, "scrollback-second"), None);
        assert_eq!(fixture.value(1, "primary-remote"), None);
        assert_eq!(
            fixture
                .app
                .state
                .doc
                .get(ROOT, "primary-remote")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("outer peer")
        );
        assert!(
            fixture
                .app
                .state
                .doc
                .get(ROOT, "foreign-remote")
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .app
                .state
                .doc
                .get(ROOT, "scrollback-first")
                .unwrap()
                .is_none()
        );
    }
}

pub(crate) struct RoutedOwners {
    pub(crate) app: App,
    pub(crate) records: crate::ResourceStore,
    pub(crate) bindings: Vec<crate::bound_dispatch::CallbackBinding>,
    /// Holds the broker's next route join until the returned sender sends.
    pub(crate) hold_join: Box<dyn Fn() -> tokio::sync::oneshot::Sender<()>>,
    _owners: crate::resource::ResourceOwners,
}
impl RoutedOwners {
    pub(crate) fn new() -> Self {
        use crate::application::{
            LaunchTarget, ProcessContext, ProcessContextInputs, RestoreInput,
        };
        use crate::application_test_support::{
            APP, author, provenance, runner_permissions, runner_probe_definition,
        };
        // A window joined to its workspace reads its records through the store and publishes
        // what it changes on their routes, which these tests watch.
        let mut owners = crate::resource::ResourceOwners::reading(Rc::new(
            crate::resource::MemoryWorkspace::default(),
        ));
        let workspace = "00000000-0000-0000-0000-000000000077";
        let records = owners.open(APP, workspace).unwrap();
        let views = Rc::new(
            crate::application_view::InstanceViewStore::in_memory(APP, "D", workspace).unwrap(),
        );
        let definition = runner_probe_definition();
        let mut root = crate::widget::Column::new();
        let mut bindings = vec![];
        for instance in [10, 11] {
            let context = ProcessContext::prepare(
                definition,
                ProcessContextInputs {
                    records: records.clone(),
                    views: views.clone(),
                    device_id: "D".into(),
                    instance_id: instance,
                    permissions: runner_permissions(),
                    author: author(),
                    action_context: provenance(),
                },
                &LaunchTarget::New,
            )
            .unwrap();
            let mut component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            component.mount(|_| {}).unwrap();
            bindings.push(component.callback_binding());
            root = root.push(Box::new(crate::widget::BoundAppWidget::new(component)));
        }
        let mut state = State::new();
        state.doc.put(ROOT, "outer", "outer only").unwrap();
        state.commit_action(None, None);
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        let (manager, hold_join) = CollaborationManager::connected_holding_joins_for_test();
        app.collaboration.manager = manager;
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::AcceptedRoute {
            route: crate::collaboration::RouteSpec::app(
                "outer-shell",
                uuid::Uuid::parse_str(workspace).unwrap(),
            ),
            epoch: uuid::Uuid::new_v4(),
        });
        app.refresh_document_routes();
        // The fake broker admits each route join asynchronously, and a route
        // refuses documents until its document mode is resolved.
        let deadline = tilo_test_env::hang_deadline();
        while !app.document_routes.owners.iter().all(|entry| {
            app.collaboration
                .manager
                .document_route_publishable(&entry.route)
        }) {
            assert!(
                crate::time::now() < deadline,
                "owned routes never became publishable"
            );
            std::thread::sleep(crate::time::Duration::from_millis(5));
        }
        Self {
            app,
            records,
            bindings,
            hold_join,
            _owners: owners,
        }
    }
    fn remote(&self, index: usize, name: &str, incremental: bool) -> CollabEvent {
        let binding = &self.bindings[index];
        let mut doc = self
            .records
            .read_checked(&binding.checkout.id, Clone::clone)
            .unwrap();
        let baseline = doc.base_heads();
        doc.set_actor(automerge::ActorId::from(format!("peer-{index}").as_bytes()));
        doc.put(ROOT, name, "remote").unwrap();
        doc.commit();
        let mutation = if incremental {
            collab_types::DocumentMutation::Incremental(doc.save_after(&baseline))
        } else {
            collab_types::DocumentMutation::Snapshot(doc.save())
        };
        routed_document(&binding.broker_route().unwrap(), mutation)
    }
    fn value(&self, index: usize, key: &str) -> Option<String> {
        use automerge::ReadDoc;
        self.records
            .read_checked(&self.bindings[index].checkout.id, |doc| {
                doc.get(ROOT, key)
                    .unwrap()
                    .and_then(|(v, _)| v.as_str().map(str::to_owned))
            })
            .unwrap()
    }
    fn observation(
        &self,
        index: usize,
    ) -> (
        Vec<automerge::ChangeHash>,
        usize,
        u64,
        Option<u64>,
        Vec<(
            tilo_actions::MemberId,
            crate::application::InstanceId,
            crate::widget::undo_access::UndoStatus,
        )>,
    ) {
        let checkout = &self.bindings[index].checkout;
        self.records
            .with_action_state(&checkout.id, |doc, history, actions| {
                (
                    doc.base_heads(),
                    doc.pending_ops(),
                    history.revision(),
                    actions.generation,
                    actions.part().statuses_for_test(),
                )
            })
            .unwrap()
    }
}

/// A record is who its store and route say it is. A peer's change can name another record's id,
/// kind, workspace or home in the record's document, but the store holds every record as it was
/// opened, the route still names the same record, and the other record never changes.
#[test]
fn a_peers_change_cannot_make_a_record_another() {
    use automerge::ReadDoc;
    for incremental in [false, true] {
        for field in ["id", "kind", "workspace", "home"] {
            let mut fixture = RoutedOwners::new();
            let (first, second) = (&fixture.bindings[0], &fixture.bindings[1]);
            assert_ne!(first.checkout.id, second.checkout.id);
            let opened = fixture.records.open_resources();
            let held = fixture.records.app_and_home();
            let (route, other_route) = (first.broker_route(), second.broker_route());
            let other_heads = fixture
                .records
                .read_checked(&second.checkout.id, |doc| doc.base_heads())
                .unwrap();
            let mut remote = fixture
                .records
                .read_checked(&first.checkout.id, Clone::clone)
                .unwrap();
            let baseline = remote.base_heads();
            remote.set_actor(automerge::ActorId::from(
                format!("peer-renames-{field}").as_bytes(),
            ));
            let claimed = match field {
                "id" => second.checkout.id.to_string(),
                "kind" => "foreign-kind".to_owned(),
                "workspace" | "home" => "00000000-0000-4000-8000-000000000099".to_owned(),
                _ => unreachable!(),
            };
            let meta = remote
                .put_object(
                    ROOT,
                    collab_types::document::fields::META,
                    automerge::ObjType::Map,
                )
                .unwrap();
            remote.put(&meta, field, claimed.as_str()).unwrap();
            remote.commit();
            let incoming = routed_document(
                &route.clone().unwrap(),
                if incremental {
                    collab_types::DocumentMutation::Incremental(remote.save_after(&baseline))
                } else {
                    collab_types::DocumentMutation::Snapshot(remote.save())
                },
            );
            fixture.app.apply_document_event(&incoming);

            let (first, second) = (&fixture.bindings[0], &fixture.bindings[1]);
            assert_eq!(fixture.records.open_resources(), opened, "{field}");
            assert_eq!(fixture.records.app_and_home(), held, "{field}");
            assert_eq!(first.broker_route(), route, "{field}");
            assert_eq!(second.broker_route(), other_route, "{field}");
            assert_eq!(
                fixture
                    .records
                    .read_checked(&second.checkout.id, |doc| doc.base_heads())
                    .unwrap(),
                other_heads,
                "{field}: the record it named is untouched"
            );
            assert!(
                fixture
                    .records
                    .read_checked(&first.checkout.id, |doc| doc.get(ROOT, "missing").is_ok())
                    .unwrap(),
                "{field}: the record still opens as itself"
            );
        }
    }
}

tilo_test_env::scenario! {
    fn owned_records_refuse_a_corrupt_change;

    phase signed_owned_corrupt_incremental_preserves_document_history_generation_and_statuses;
    {
        let mut fixture = RoutedOwners::new();
        let binding = &fixture.bindings[0];
        let mut remote = fixture
            .records
            .read_checked(&binding.checkout.id, Clone::clone)
            .unwrap();
        let baseline = remote.base_heads();
        remote.put(ROOT, "valid-prefix", "must not enter").unwrap();
        remote.commit();
        let mut bytes = remote.save_after(&baseline);
        bytes.extend_from_slice(b"corrupt trailing chunk");
        let incoming = routed_document(
            &binding.broker_route().unwrap(),
            collab_types::DocumentMutation::Incremental(bytes),
        );
        let before = fixture.observation(0);
        fixture.app.apply_document_event(&incoming);
        assert_eq!(fixture.observation(0), before);
        assert_eq!(fixture.value(0, "valid-prefix"), None);
    }
}

#[derive(Clone, Copy)]
enum PrimaryMetadataShape {
    None,
    Tilo(bool),
}
impl PrimaryMetadataShape {
    fn family(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Tilo(_) => Some("tilo:record"),
        }
    }
    fn scoped(self) -> bool {
        matches!(self, Self::Tilo(true))
    }
}

fn primary_routed_factory(
    mut context: crate::application::ProcessContext,
    target: crate::application::LaunchTarget,
    restore: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    if matches!(target, crate::application::LaunchTarget::PrimaryNew) {
        context
            .initialize_new_resource(|doc| {
                doc.put(ROOT, "body", "initialized")
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .map_err(crate::launch::LaunchError::Record)?;
    }
    crate::application_test_support::probe_factory(context, target, restore)
}

struct RoutedPrimary {
    app: App,
    records: crate::ResourceStore,
    binding: crate::bound_dispatch::CallbackBinding,
    _owners: crate::resource::ResourceOwners,
}
impl RoutedPrimary {
    fn new(shape: PrimaryMetadataShape) -> Self {
        use crate::application::{
            ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs, RestoreInput,
        };
        // As a window joined to its workspace: its routes publish its records.
        let mut owners = crate::resource::ResourceOwners::reading(Rc::new(
            crate::resource::MemoryWorkspace::default(),
        ));
        let workspace = "00000000-0000-4000-8000-000000000078";
        let records = owners.open("dev.tilo.primary-probe", workspace).unwrap();
        let views = Rc::new(
            crate::application_view::InstanceViewStore::in_memory(
                "dev.tilo.primary-probe",
                "D",
                workspace,
            )
            .unwrap(),
        );
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.primary-probe".into(),
            title: "Primary Probe".into(),
            icon: crate::widget::IconDescriptor::Glyph("P"),
            resource_type: crate::ResourceType::new("primary.probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: primary_routed_factory,
        };
        let target = LaunchTarget::PrimaryNew;
        let context = ProcessContext::prepare(
            &definition,
            ProcessContextInputs {
                records: records.clone(),
                views,
                device_id: "D".into(),
                instance_id: 70,
                permissions: crate::permissions::SharedPermissions::new(
                    crate::permissions::PermissionContext::permissive(),
                ),
                author: crate::permissions::MemberId::person("primary-author"),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        if let Some(family) = shape.family() {
            records
                .with_action_state(&binding.checkout.id, |doc, history, _| {
                    let meta = doc
                        .put_object(ROOT, family, automerge::ObjType::Map)
                        .unwrap();
                    if shape.scoped() {
                        doc.put(&meta, "home", workspace).unwrap();
                        doc.put(&meta, "workspace", workspace).unwrap();
                    }
                    doc.put_object(&meta, "refs", automerge::ObjType::List)
                        .unwrap();
                    doc.commit();
                    history.rebase(doc);
                })
                .unwrap();
        }
        component.mount(|_| {}).unwrap();
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(crate::widget::BoundAppWidget::new(component)),
        );
        app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::AcceptedRoute {
            route: binding.broker_route().unwrap(),
            epoch: uuid::Uuid::new_v4(),
        });
        app.refresh_document_routes();
        Self {
            app,
            records,
            binding,
            _owners: owners,
        }
    }
    fn observation(
        &self,
    ) -> (
        Vec<automerge::ChangeHash>,
        usize,
        u64,
        Option<u64>,
        Vec<(
            tilo_actions::MemberId,
            crate::application::InstanceId,
            crate::widget::undo_access::UndoStatus,
        )>,
    ) {
        self.records
            .with_action_state(&self.binding.checkout.id, |doc, history, actions| {
                (
                    doc.base_heads(),
                    doc.pending_ops(),
                    history.revision(),
                    actions.generation,
                    actions.part().statuses_for_test(),
                )
            })
            .unwrap()
    }
    fn value(&self, key: &str) -> Option<String> {
        use automerge::ReadDoc;
        self.records
            .read_checked(&self.binding.checkout.id, |doc| {
                doc.get(ROOT, key)
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned))
            })
            .unwrap()
    }
    fn envelope(
        &self,
        incremental: bool,
        mutate: impl FnOnce(&mut automerge::AutoCommit),
    ) -> CollabEvent {
        let mut remote = self
            .records
            .read_checked(&self.binding.checkout.id, Clone::clone)
            .unwrap();
        let baseline = remote.base_heads();
        remote.set_actor(automerge::ActorId::from(b"primary-peer".as_slice()));
        mutate(&mut remote);
        remote.commit();
        routed_document(
            &self.binding.broker_route().unwrap(),
            if incremental {
                collab_types::DocumentMutation::Incremental(remote.save_after(&baseline))
            } else {
                collab_types::DocumentMutation::Snapshot(remote.save())
            },
        )
    }
}

tilo_test_env::scenario! {
    fn signed_package_primary_keeps_each_metadata_shape;

    phase signed_package_primary_preserves_each_existing_metadata_shape;
    {
        for shape in [
            PrimaryMetadataShape::None,
            PrimaryMetadataShape::Tilo(false),
            PrimaryMetadataShape::Tilo(true),
        ] {
            for incremental in [false, true] {
                let mut fixture = RoutedPrimary::new(shape);
                let before = fixture.observation();
                let valid = fixture.envelope(incremental, |doc| {
                    doc.put(ROOT, "valid-primary", "remote").unwrap();
                });
                fixture.app.apply_document_event(&valid);
                assert_eq!(fixture.value("valid-primary").as_deref(), Some("remote"));
                let after = fixture.observation();
                assert_eq!(after.2, before.2);
                assert_eq!(after.3, before.3.and_then(|value| value.checked_add(1)));
                assert!(after.4.is_empty());
            }
        }
    }
}

static OWNED_HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: crate::application_test_support::APP,
    scopes: &[
        HistoryScopeDefinition {
            key: "first",
            title: "First record",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
        },
        HistoryScopeDefinition {
            key: "second",
            title: "Second record",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
        },
    ],
};
impl RoutedOwners {
    fn install_history(&mut self) {
        let sources = self
            .bindings
            .iter()
            .zip(["first", "second"])
            .map(|(binding, scope)| {
                HistorySourceRegistration::owned(
                    scope,
                    OWNED_HISTORY.app_id,
                    HistoryOwner::component(binding.clone()),
                )
                .unwrap()
            })
            .collect();
        self.app
            .install_history(HistoryRegistration::new(&OWNED_HISTORY, sources).unwrap())
            .unwrap();
        layout(&mut self.app);
    }
}
fn select_native_version(app: &mut App, hash: automerge::ChangeHash) {
    let target = format!(
        "timeline-entry:{}",
        crate::HistoryEntryId::hex(&crate::history_source::entry_id_from_hash(
            &hash.to_string()
        ))
    );
    fn find(node: &crate::a11y::AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
        if node.action_target.as_deref() == Some(target) {
            return node.bounds.map(|bounds| bounds.to_rect());
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    let timeline = node_for_test_id(app, "activity_timeline");
    let access = app
        .inspect_node(timeline, |widget| widget.accessibility())
        .unwrap()
        .unwrap();
    let rect = find(&access, &target)
        .expect("selected immutable version has a measured actual timeline row");
    app.dispatch_semantic_click(rect.center().x, rect.center().y);
    layout(app);
}

tilo_test_env::scenario! {
    fn a_live_update_leaves_the_historical_view_where_it_is;

    phase native_routed_live_update_keeps_selected_historical_canvas_pinned;
    {
        let mut fixture = RoutedOwners::new();
        let binding = fixture.bindings[0].clone();
        let selected = fixture
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "selected old notebook").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let selected = doc.base_heads()[0];
                doc.put(ROOT, "body", "live notebook").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                selected
            })
            .unwrap();
        fixture.install_history();
        let target = named(&mut fixture.app, "History");
        fixture
            .app
            .dispatch_a11y_action(target, &crate::a11y::Action::Activate);
        layout(&mut fixture.app);
        select_native_version(&mut fixture.app, selected);
        let preview_before = fixture
            .app
            .build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label())
            .find(|label| label.starts_with("Notebook:"))
            .unwrap()
            .to_owned();
        let mut peer = fixture
            .records
            .read_checked(&binding.checkout.id, Clone::clone)
            .unwrap();
        peer.put(ROOT, "body", "remote latest notebook").unwrap();
        peer.commit();
        fixture
            .app
            .apply_collab_event_for_test(routed_document(
                &binding.broker_route().unwrap(),
                collab_types::DocumentMutation::Snapshot(peer.save()),
            ));
        layout(&mut fixture.app);
        assert_eq!(
            fixture.value(0, "body").as_deref(),
            Some("remote latest notebook")
        );
        let labels = fixture
            .app
            .build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(labels.contains(&preview_before));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Live content has updates")),
            "live update notice accompanies pinned historical state: {labels:?}"
        );
    }

    phase round1_top_level_wide_activity_detaches_focused_native_view_until_live_return;
    {
        use crate::widget::native_view::{
            NativeView, NativeViewBackend, NativeViewGeometry, NativeViewHost,
        };
        use std::{cell::Cell, rc::Rc};
        #[derive(Default)]
        struct NativeState {
            mounts: Cell<usize>,
            attached: Cell<bool>,
            visible: Cell<bool>,
            enabled: Cell<bool>,
            focused: Cell<bool>,
        }
        struct Backend(Rc<NativeState>);
        impl NativeViewBackend for Backend {
            fn mount(&mut self, _: &NativeViewHost<'_>) -> Result<(), String> {
                self.0.mounts.set(self.0.mounts.get() + 1);
                self.0.attached.set(true);
                Ok(())
            }
            fn unmount(&mut self) {
                self.0.attached.set(false);
            }
            fn set_geometry(&mut self, _: NativeViewGeometry) {}
            fn set_visible(&mut self, value: bool) {
                self.0.visible.set(value);
            }
            fn set_enabled(&mut self, value: bool) {
                self.0.enabled.set(value);
            }
            fn set_focus(&mut self, value: bool) {
                self.0.focused.set(value);
            }
        }
        let native = Rc::new(NativeState::default());
        let mut view = NativeView::new("Top-level native editor", Backend(native.clone()));
        view.set_focus(true);
        let mut app = app();
        app.root_widget = Box::new(view);
        let host = NativeViewHost::headless(
            crate::platform::PlatformConventions::new(crate::platform::Platform::MacOs),
            1.0,
        );
        app.init_layout();
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 900, 650, 1.0, true, Some(&host)).unwrap();
        draw_frame_content(&mut app, &mut Scene::new(), 900, 650, 1.0, None);
        assert!(
            native.attached.get()
                && native.visible.get()
                && native.enabled.get()
                && native.focused.get()
        );
        let target = named(&mut app, "History");
        app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 900, 650, 1.0, true, Some(&host)).unwrap();
        assert!(
            app.history.as_ref().unwrap().live_visible(),
            "wide activity retains the rendered live backdrop"
        );
        assert!(!app.history.as_ref().unwrap().live_interactive());
        assert!(
            !native.attached.get(),
            "wide activity must detach independent native input even without a selected preview"
        );
        draw_frame_content(&mut app, &mut Scene::new(), 900, 650, 1.0, None);
        assert!(
            !native.attached.get() && !native.visible.get(),
            "drawing the live backdrop must not reactivate native controls"
        );
        let target = named(&mut app, "History");
        app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
        let root = app.layout.root_node.unwrap();
        run_layout_pass(&mut app, root, 900, 650, 1.0, true, Some(&host)).unwrap();
        draw_frame_content(&mut app, &mut Scene::new(), 900, 650, 1.0, None);
        assert_eq!(native.mounts.get(), 2);
        assert!(native.attached.get() && native.visible.get() && native.enabled.get());
    }
}

fn round1_malformed_mutation(
    incremental: bool,
    mut document: automerge::AutoCommit,
) -> collab_types::DocumentMutation {
    let baseline = document.base_heads();
    document.set_actor(automerge::ActorId::from(b"malformed-peer".as_slice()));
    document
        .put(ROOT, "invalid-peer-field", "must never enter")
        .unwrap();
    document.commit();
    let mut bytes = if incremental {
        document.save_after(&baseline)
    } else {
        document.save()
    };
    bytes[4] ^= 0xff; // Corrupt an unknown complete chunk, not a known snapshot or incomplete tail.
    assert!(automerge::AutoCommit::load(&bytes).is_err());
    if incremental {
        collab_types::DocumentMutation::Incremental(bytes)
    } else {
        collab_types::DocumentMutation::Snapshot(bytes)
    }
}

tilo_test_env::scenario! {
    fn remote_recovery_quarantines_bad_bytes_and_asks_for_a_snapshot;

    phase malformed_remote_bytes_quarantine_and_recover_with_a_snapshot;
    {
        {
            for incremental in [false, true] {
                let mut fixture = RoutedOwners::new();
                let binding = fixture.bindings[0].clone();
                let route = binding.broker_route().unwrap();
                let before = fixture
                    .records
                    .read_checked(&binding.checkout.id, Clone::clone)
                    .unwrap();
                let bad = routed_document(
                    &route,
                    round1_malformed_mutation(incremental, before.clone()),
                );
                fixture.app.apply_document_event(&bad);
                assert!(
                    fixture
                        .app
                        .document_sync_status()
                        .unwrap()
                        .contains("could not be applied")
                );
                let failure = fixture
                    .app
                    .document_sync_status()
                    .map(std::borrow::Cow::into_owned);
                let invalid_recovery =
                    routed_document(&route, round1_malformed_mutation(false, before.clone()));
                fixture
                    .app
                    .apply_document_event(&invalid_recovery);
                assert_eq!(
                    fixture.app.document_sync_status().as_deref(),
                    failure.as_deref()
                );
                fixture.app.publish_owned_documents();
                assert!(
                    !fixture
                        .app
                        .document_routes
                        .sent
                        .iter()
                        .any(|(sent, _)| sent == &route)
                );
                assert_eq!(
                    fixture
                        .records
                        .read_checked(&binding.checkout.id, |doc| doc.base_heads())
                        .unwrap(),
                    before.base_heads()
                );
                let recovery = fixture.remote(0, "recovered", false);
                fixture
                    .records
                    .with_action_state(&binding.checkout.id, |doc, _, _| {
                        doc.put(ROOT, "local-draft", "preserved").unwrap();
                    })
                    .unwrap();
                let pending = fixture
                    .records
                    .read_history(&binding.checkout.id, |doc, history| {
                        (doc.base_heads(), doc.pending_ops(), history.revision())
                    })
                    .unwrap();
                fixture.app.apply_document_event(&recovery);
                assert_eq!(
                    fixture.app.document_sync_status().as_deref(),
                    failure.as_deref(),
                    "recovery remains visible until the snapshot actually merges"
                );
                assert_eq!(
                    fixture
                        .records
                        .read_history(&binding.checkout.id, |doc, history| (
                            doc.base_heads(),
                            doc.pending_ops(),
                            history.revision()
                        ))
                        .unwrap(),
                    pending
                );
                assert_eq!(fixture.value(0, "recovered"), None);
                fixture
                    .records
                    .with_history(&binding.checkout.id, |doc, history| {
                        history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                    })
                    .unwrap();
                let revision = fixture
                    .records
                    .read_history(&binding.checkout.id, |_, history| history.revision())
                    .unwrap();
                fixture.app.drain_document_routes();
                assert_eq!(
                    fixture.value(0, "recovered").as_deref(),
                    Some("remote"),
                    "a malformed front must not permanently block a fresh snapshot"
                );
                assert_eq!(
                    fixture.value(0, "local-draft").as_deref(),
                    Some("preserved")
                );
                assert_eq!(
                    fixture
                        .records
                        .read_history(&binding.checkout.id, |_, history| history.revision())
                        .unwrap(),
                    revision
                );
                assert_eq!(fixture.app.document_sync_status(), None);
                fixture.app.publish_owned_documents();
                assert_eq!(
                    fixture
                        .app
                        .document_routes
                        .sent
                        .iter()
                        .filter(|(sent, _)| sent == &route)
                        .count(),
                    1,
                    "recovered exact owner resumes publication"
                );
            }
        }

        {
            use automerge::ReadDoc;
            for incremental in [false, true] {
                let mut fixture = RoutedOwners::new();
                let route = fixture
                    .app
                    .actions
                    .bound_broker_route_for_test
                    .as_ref()
                    .unwrap()
                    .route
                    .clone();
                let before = fixture.app.state.doc.base_heads();
                let bad = routed_document(
                    &route,
                    round1_malformed_mutation(incremental, fixture.app.state.doc.clone()),
                );
                fixture.app.apply_document_event(&bad);
                assert!(
                    fixture
                        .app
                        .document_sync_status()
                        .unwrap()
                        .contains("could not be applied")
                );
                let failure = fixture
                    .app
                    .document_sync_status()
                    .map(std::borrow::Cow::into_owned);
                let invalid_recovery = routed_document(
                    &route,
                    round1_malformed_mutation(false, fixture.app.state.doc.clone()),
                );
                fixture
                    .app
                    .apply_document_event(&invalid_recovery);
                assert_eq!(
                    fixture.app.document_sync_status().as_deref(),
                    failure.as_deref()
                );
                assert_eq!(fixture.app.state.doc.base_heads(), before);
                let mut peer = fixture.app.state.doc.clone();
                peer.set_actor(automerge::ActorId::from(b"recovery-peer".as_slice()));
                peer.put(ROOT, "recovered", "remote").unwrap();
                peer.commit();
                let recovery = routed_document(
                    &route,
                    collab_types::DocumentMutation::Snapshot(peer.save()),
                );
                fixture
                    .app
                    .state
                    .doc
                    .put(ROOT, "local-draft", "preserved")
                    .unwrap();
                let pending = (
                    fixture.app.state.doc.base_heads(),
                    fixture.app.state.doc.pending_ops(),
                    fixture.app.state.history_revision(),
                );
                fixture.app.apply_document_event(&recovery);
                assert_eq!(
                    fixture.app.document_sync_status().as_deref(),
                    failure.as_deref(),
                    "recovery remains visible until the snapshot actually merges"
                );
                assert_eq!(
                    (
                        fixture.app.state.doc.base_heads(),
                        fixture.app.state.doc.pending_ops(),
                        fixture.app.state.history_revision()
                    ),
                    pending
                );
                assert!(
                    fixture
                        .app
                        .state
                        .doc
                        .get(ROOT, "recovered")
                        .unwrap()
                        .is_none()
                );
                fixture.app.state.commit_action(None, None);
                let revision = fixture.app.state.history_revision();
                fixture.app.drain_document_routes();
                assert_eq!(
                    fixture
                        .app
                        .state
                        .doc
                        .get(ROOT, "recovered")
                        .unwrap()
                        .map(|(value, _)| value.as_str().unwrap().to_owned())
                        .as_deref(),
                    Some("remote"),
                    "primary malformed front must not block fresh snapshot recovery"
                );
                assert_eq!(
                    fixture
                        .app
                        .state
                        .doc
                        .get(ROOT, "local-draft")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("preserved")
                );
                assert_eq!(fixture.app.state.history_revision(), revision);
                assert_eq!(fixture.app.document_sync_status(), None);
                assert_eq!(fixture.value(0, "recovered"), None);
                assert_eq!(fixture.value(1, "recovered"), None);
            }
        }

        {
            let mut fixture = RoutedOwners::new();
            let valid = fixture.remote(0, "must-not-partly-apply", true);
            let collab_types::DocumentMutation::Incremental(mut bytes) =
                valid.document_mutation().unwrap().clone()
            else {
                panic!("fixture delta");
            };
            bytes.extend_from_slice(b"invalid trailing chunk");
            assert!(automerge::AutoCommit::load(&bytes).is_err());
            let envelope = routed_document(
                &fixture.bindings[0].broker_route().unwrap(),
                collab_types::DocumentMutation::Incremental(bytes),
            );
            fixture.app.apply_document_event(&envelope);
            assert_eq!(
                fixture.value(0, "must-not-partly-apply"),
                None,
                "partial incremental parsing must not admit the valid prefix"
            );
            assert!(
                fixture
                    .app
                    .document_sync_status()
                    .unwrap()
                    .contains("could not be applied")
            );
        }

        {
            let mut fixture = RoutedOwners::new();
            let binding = fixture.bindings[0].clone();
            let baseline = fixture
                .records
                .read_checked(&binding.checkout.id, Clone::clone)
                .unwrap();
            let bad = routed_document(
                &binding.broker_route().unwrap(),
                round1_malformed_mutation(false, baseline.clone()),
            );
            let delta = fixture.remote(0, "valid-queued-delta", true);
            let collab_types::DocumentMutation::Incremental(delta_bytes) =
                delta.document_mutation().unwrap().clone()
            else {
                panic!("fixture delta");
            };
            assert!(
                matches!(
                    automerge::AutoCommit::load(&delta_bytes),
                    Err(automerge::AutomergeError::MissingDeps)
                ),
                "valid incremental requires the actual receiver's dependencies"
            );
            let mut snapshot_peer = baseline;
            snapshot_peer.set_actor(automerge::ActorId::from(
                b"independent-snapshot-peer".as_slice(),
            ));
            snapshot_peer
                .put(ROOT, "valid-recovery-snapshot", "remote")
                .unwrap();
            snapshot_peer.commit();
            let snapshot = routed_document(
                &binding.broker_route().unwrap(),
                collab_types::DocumentMutation::Snapshot(snapshot_peer.save()),
            );
            fixture
                .records
                .with_action_state(&binding.checkout.id, |doc, _, _| {
                    doc.put(ROOT, "local-draft", "preserved").unwrap();
                })
                .unwrap();
            let pending = fixture
                .records
                .read_history(&binding.checkout.id, |doc, history| {
                    (doc.base_heads(), doc.pending_ops(), history.revision())
                })
                .unwrap();
            for message in [&bad, &delta, &snapshot] {
                fixture.app.apply_document_event(message);
            }
            assert_eq!(
                fixture
                    .records
                    .read_history(&binding.checkout.id, |doc, history| (
                        doc.base_heads(),
                        doc.pending_ops(),
                        history.revision()
                    ))
                    .unwrap(),
                pending
            );
            fixture
                .records
                .with_history(&binding.checkout.id, |doc, history| {
                    history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                })
                .unwrap();
            let revision = fixture
                .records
                .read_history(&binding.checkout.id, |_, history| history.revision())
                .unwrap();
            fixture.app.drain_document_routes();
            assert_eq!(
                fixture.value(0, "valid-queued-delta").as_deref(),
                Some("remote")
            );
            assert_eq!(
                fixture.value(0, "valid-recovery-snapshot").as_deref(),
                Some("remote")
            );
            assert_eq!(
                fixture.value(0, "local-draft").as_deref(),
                Some("preserved")
            );
            assert_eq!(
                fixture
                    .records
                    .read_history(&binding.checkout.id, |_, history| history.revision())
                    .unwrap(),
                revision
            );
            assert_eq!(fixture.app.document_sync_status(), None);
        }
    }

    phase deferred_remote_recovery_retains_later_deltas_and_requires_a_new_snapshot_on_overflow;
    {
        {
            for snapshot in [false, true] {
                round2_deferred_recovery_keeps_later_traffic(true, snapshot, false);
            }
        }
        {
            for snapshot in [false, true] {
                round2_deferred_recovery_keeps_later_traffic(false, snapshot, false);
            }
        }
        {
            for owned in [false, true] {
                round2_deferred_recovery_keeps_later_traffic(owned, false, true);
            }
        }
    }
}

fn round2_deferred_recovery_keeps_later_traffic(owned: bool, later_snapshot: bool, overflow: bool) {
    use automerge::ReadDoc;
    let mut fixture = RoutedOwners::new();
    let binding = fixture.bindings[0].clone();
    let route = if owned {
        binding.broker_route().unwrap()
    } else {
        fixture
            .app
            .actions
            .bound_broker_route_for_test
            .as_ref()
            .unwrap()
            .route
            .clone()
    };
    let baseline = if owned {
        fixture
            .records
            .read_checked(&binding.checkout.id, Clone::clone)
            .unwrap()
    } else {
        fixture.app.state.doc.clone()
    };
    let bad = routed_document(&route, round1_malformed_mutation(false, baseline.clone()));
    fixture.app.apply_document_event(&bad);
    assert!(fixture.app.document_sync_status().is_some());
    let mut peer = baseline;
    peer.set_actor(automerge::ActorId::from(
        b"deferred-recovery-peer".as_slice(),
    ));
    peer.put(ROOT, "recovery-S", "first").unwrap();
    peer.commit();
    let s_heads = peer.base_heads();
    let s = routed_document(
        &route,
        collab_types::DocumentMutation::Snapshot(peer.save()),
    );
    peer.put(ROOT, "later-D", "later").unwrap();
    peer.commit();
    let d = routed_document(
        &route,
        if later_snapshot {
            collab_types::DocumentMutation::Snapshot(peer.save())
        } else {
            collab_types::DocumentMutation::Incremental(peer.save_after(&s_heads))
        },
    );
    if owned {
        fixture
            .records
            .with_action_state(&binding.checkout.id, |doc, _, _| {
                doc.put(ROOT, "local-draft", "preserved").unwrap();
            })
            .unwrap();
    } else {
        fixture
            .app
            .state
            .doc
            .put(ROOT, "local-draft", "preserved")
            .unwrap();
    }
    let observation = |fixture: &RoutedOwners| {
        if owned {
            fixture
                .records
                .read_history(&binding.checkout.id, |doc, history| {
                    (doc.base_heads(), doc.pending_ops(), history.revision())
                })
                .unwrap()
        } else {
            (
                fixture.app.state.doc.base_heads(),
                fixture.app.state.doc.pending_ops(),
                fixture.app.state.history_revision(),
            )
        }
    };
    let pending = observation(&fixture);
    fixture.app.apply_document_event(&s);
    assert!(
        fixture.app.document_sync_status().is_some(),
        "S must remain visibly pending while local editing continues"
    );
    fixture.app.apply_document_event(&d);
    if overflow {
        // S plus D and62 duplicate D packets fill the bounded64-item queue.
        for _ in 0..62 {
            fixture.app.apply_document_event(&d);
        }
        peer.put(ROOT, "refused-newer-change", "must recover later")
            .unwrap();
        peer.commit();
        let refused = routed_document(
            &route,
            collab_types::DocumentMutation::Snapshot(peer.save()),
        );
        fixture.app.apply_document_event(&refused);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("Too many changes from others")
        );
    }
    assert_eq!(
        observation(&fixture),
        pending,
        "remote admission must not finish local edits"
    );
    if owned {
        fixture.app.publish_owned_documents();
        assert!(
            !fixture
                .app
                .document_routes
                .sent
                .iter()
                .any(|(sent, _)| sent == &route)
        );
        fixture
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            })
            .unwrap();
    } else {
        fixture.app.state.commit_action(None, None);
    }
    let committed_revision = observation(&fixture).2;
    fixture.app.drain_document_routes();
    let value = |fixture: &RoutedOwners, key: &str| {
        if owned {
            fixture.value(0, key)
        } else {
            fixture
                .app
                .state
                .doc
                .get(ROOT, key)
                .unwrap()
                .map(|(value, _)| value.as_str().unwrap().to_owned())
        }
    };
    assert_eq!(value(&fixture, "recovery-S").as_deref(), Some("first"));
    assert_eq!(
        value(&fixture, "later-D").as_deref(),
        Some("later"),
        "traffic arriving after deferred recovery S must not be lost"
    );
    assert_eq!(value(&fixture, "local-draft").as_deref(), Some("preserved"));
    assert_eq!(
        observation(&fixture).2,
        committed_revision,
        "remote recovery has no authored undo boundary"
    );
    if overflow {
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("Too many changes from others"),
            "older S cannot clear the obligation created by refused later traffic"
        );
        assert_eq!(value(&fixture, "refused-newer-change"), None);
        if owned {
            fixture.app.publish_owned_documents();
            assert!(
                !fixture
                    .app
                    .document_routes
                    .sent
                    .iter()
                    .any(|(sent, _)| sent == &route)
            );
        }
        let fresh = routed_document(
            &route,
            collab_types::DocumentMutation::Snapshot(peer.save()),
        );
        fixture.app.apply_document_event(&fresh);
        assert_eq!(
            value(&fixture, "refused-newer-change").as_deref(),
            Some("must recover later")
        );
    }
    assert_eq!(fixture.app.document_sync_status(), None);
    assert_eq!(observation(&fixture).2, committed_revision);
    assert_eq!(fixture.value(1, "later-D"), None);
    if owned {
        assert!(
            fixture
                .app
                .state
                .doc
                .get(ROOT, "later-D")
                .unwrap()
                .is_none()
        );
        fixture.app.publish_owned_documents();
        let sent = fixture
            .app
            .document_routes
            .sent
            .iter()
            .filter(|(sent, _)| sent == &route)
            .collect::<Vec<_>>();
        assert_eq!(
            sent.len(),
            1,
            "only recovered exact owner resumes publication"
        );
        let CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(data),
            ..
        } = &sent[0].1
        else {
            panic!("first owned publication is snapshot");
        };
        let published = automerge::AutoCommit::load(data).unwrap();
        assert_eq!(
            published.get(ROOT, "later-D").unwrap().unwrap().0.as_str(),
            Some("later")
        );
    } else {
        assert_eq!(fixture.value(0, "later-D"), None);
    }
}

tilo_test_env::scenario! {
    fn native_history_keeps_editor_state_pointer_capture_and_live_controls;

    phase native_history_surface_composes_installs_and_keeps_editor_state;
    {
        {
            let mut app = app();
            layout(&mut app);
            let tree = app.build_a11y_tree_update(1.0);
            let history = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("History"))
                .map(|(id, _)| *id)
                .expect("native managed App must expose shared History");
            app.dispatch_a11y_action(history, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == accesskit::Role::List)
            );
        }

        {
            let mut app = app();
            app.root_widget = Box::new(crate::widget::Column::new().push(Box::new(
                crate::widget::TextInput::local().with_id("draft-editor"),
            )));
            app.root_widget.sync(&app.state.doc, &ROOT);
            layout(&mut app);
            let editor = node_for_test_id(&app, "draft-editor");
            app.set_focused_node(Some(editor));
            app.perform_node_action(editor, &crate::a11y::Action::SetValue("unfinished".into()))
                .unwrap();
            app.perform_node_action(
                editor,
                &crate::a11y::Action::SetSelection {
                    anchor: 2,
                    focus: 7,
                },
            )
            .unwrap();
            app.state.doc.put(ROOT, "in-progress", "pending").unwrap();
            let heads = app.state.doc.base_heads();
            let ops = app.state.doc.pending_ops();
            let editor_before = app
                .inspect_node(editor, |widget| widget.accessibility())
                .unwrap()
                .unwrap();
            let open = node_for_test_id(&app, "history-open");
            let bounds = app.inspect_node(open, |widget| widget.get_rect()).unwrap();
            app.dispatch_semantic_click(bounds.center().x, bounds.center().y);
            layout(&mut app);
            assert_eq!(app.state.doc.base_heads(), heads);
            assert_eq!(app.state.doc.pending_ops(), ops);
            let close = named(&mut app, "History");
            app.dispatch_a11y_action(close, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert_eq!(app.state.doc.base_heads(), heads);
            assert_eq!(app.state.doc.pending_ops(), ops);
            let restored = app.focused_node.expect("stable editor focus restored");
            let after = app
                .inspect_node(restored, |widget| widget.accessibility())
                .unwrap()
                .unwrap();
            assert_eq!(editor_before.value, after.value);
            assert_eq!(editor_before.text_selection, after.text_selection);
        }

        {
            let mut app = app();
            layout(&mut app);
            let before = app.state.doc.base_heads();
            let target = named(&mut app, "History");
            app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
            layout(&mut app);
            let node = app
                .build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .find(|(_, node)| node.role() == accesskit::Role::ListItem)
                .unwrap()
                .1
                .clone();
            let rect = node.bounds().unwrap();
            app.dispatch_semantic_click((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0);
            layout(&mut app);
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(_, node)| node
                        .label()
                        .is_some_and(|label| label.starts_with("Notebook:")))
            );
            let target = named(&mut app, "Restore…");
            app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Restore version"))
            );
            native_key(&mut app, "Escape");
            assert_eq!(app.state.doc.base_heads(), before);
            let target = named(&mut app, "Back to live");
            app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert!(app.history.as_ref().unwrap().live_visible());
        }

        {
            let (_host, component) = crate::history_mode::integration_tests::component_with_history();
            let mut app = App::from_component(
                component,
                crate::application::ComponentRunnerInputs {
                    theme: crate::Theme::dark(),
                    text_renderer: crate::font_catalog::fixture_renderer(),
                },
            )
            .unwrap();
            let handle = {
                let _scope = app.enter_wake_scope();
                crate::WakeHandle::new()
            };
            layout(&mut app);
            assert!(app.validate_history_registration().is_ok());
            let history = named(&mut app, "History");
            app.dispatch_a11y_action(history, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == accesskit::Role::List)
            );
            assert!(handle.is_active());
            drop(app);
            assert!(!handle.is_active());
        }

        {
            let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
                "live",
                "fixture.export",
            );
            let starts = probe.starts;
            let mut app = app();
            app.root_widget = probe.root;
            layout(&mut app);
            let declared = app.collect_declared_resources();
            let violations = tilo_actions::validate_surface(
                &declared,
                &app.actions.action_registry,
                &crate::widget::resource_operation_handlers(&*app.root_widget),
            );
            assert!(violations.is_empty(), "{violations:?}");
            let export = crate::accesskit_bridge::resource_action_node_id("live", "export");
            assert!(
                app.build_a11y_tree_update(1.0)
                    .nodes
                    .iter()
                    .any(|(id, _)| *id == export),
                "the live resource action remains exported beside closed History chrome"
            );
            app.dispatch_a11y_action(export, &crate::a11y::Action::Activate);
            assert_eq!(starts.get(), 1, "live positive control must execute");
            probe.finish.set(true);
            assert!(app.poll_directed_invocations());
            assert_eq!(probe.completions.get(), 1);
            assert!(
                app.actions.local_invocations.is_empty(),
                "hidden retry cannot be rejected merely because first invocation is in flight"
            );
            let target = named(&mut app, "History");
            app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
            layout(&mut app);
            app.dispatch_a11y_action(export, &crate::a11y::Action::Activate);
            assert_eq!(
                starts.get(),
                1,
                "hidden local resource action cannot start during History"
            );
        }

        {
            let mut app = app();
            app.root_widget = Box::new(crate::widget::TextInput::local().with_id("pending-editor"));
            layout(&mut app);
            let editor = node_for_test_id(&app, "pending-editor");
            app.set_focused_node(Some(editor));
            app.perform_node_action(editor, &crate::a11y::Action::SetValue("draft".into()))
                .unwrap();
            app.state.doc.put(ROOT, "unrelated", "pending").unwrap();
            let before = (app.state.doc.base_heads(), app.state.doc.pending_ops());
            let target = named(&mut app, "History");
            app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
            layout(&mut app);
            native_key(&mut app, "Tab");
            let tabbed = app.focused_node;
            assert!(tabbed.is_some_and(|node| {
                app.history
                    .as_ref()
                    .unwrap()
                    .owns_node(node, &app.layout.taffy)
            }));
            app.input.modifiers = winit::keyboard::ModifiersState::SHIFT.into();
            native_key(&mut app, "Tab");
            app.input.modifiers = winit::event::Modifiers::default();
            native_key(&mut app, "Tab");
            assert_eq!(
                app.focused_node, tabbed,
                "Shift+Tab reverses the same chrome focus order"
            );
            for ime in [
                winit::event::Ime::Preedit("候補".into(), Some((0, 6))),
                winit::event::Ime::Commit("界".into()),
            ] {
                let event = WindowEvent::Ime(ime);
                assert!(app.history_controls_event(&event, 1.0));
                app.dispatch_framework_history_event(&event, 1.0);
            }
            assert_eq!(
                (app.state.doc.base_heads(), app.state.doc.pending_ops()),
                before
            );
            assert!(!app.framework_input.get());
            assert!(!crate::history_mode::frame::FrameworkInputFrame::active());
        }
    }

    phase native_history_entry_pointer_captures_before_discard;
    {
        let probe =
            crate::history_mode::isolation_tests::native_effect_probe_parts("live", "fixture.export");
        let mut app = app();
        app.root_widget = probe.root;
        layout(&mut app);
        probe.emitter.borrow_mut().emit(()).unwrap();

        let open = node_for_test_id(&app, "history-open");
        let bounds = app.inspect_node(open, |widget| widget.get_rect()).unwrap();
        app.dispatch_semantic_click(bounds.center().x, bounds.center().y);

        let retained = app.pending_history_controls();
        assert_eq!(retained.len(), 1, "entry must seal the pre-entry request");
        assert_eq!(retained[0].invocation.address.resource, "live");
        assert_eq!(probe.starts.get(), 0, "entry cannot admit retained work");
    }

    phase native_history_focus_does_not_drain_live_controls;
    {
        let probe =
            crate::history_mode::isolation_tests::native_effect_probe_parts("live", "fixture.export");
        let mut app = app();
        app.root_widget = probe.root;
        layout(&mut app);
        probe.emitter.borrow_mut().emit(()).unwrap();

        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Focus);

        assert!(
            probe.emitter.borrow_mut().take().is_some(),
            "ordinary framework focus must leave the live request unfinished"
        );
        assert_eq!(probe.starts.get(), 0);
        assert!(app.pending_history_controls().is_empty());
    }
}

const PALETTE_TEXT_OPERATION: &str = "fixture.history-palette-text";

const PALETTE_JSON_OPERATION: &str = "fixture.history-palette-json";

const PALETTE_CONFIRMATION: &str = "RUN PALETTE";

#[derive(Default)]
struct PaletteContinuationState {
    authority: std::sync::atomic::AtomicU8,
    draft_epoch: std::sync::atomic::AtomicU64,
    target_epoch: std::sync::atomic::AtomicU64,
    handler_epoch: std::sync::atomic::AtomicU64,
    draft_supported: std::sync::atomic::AtomicBool,
    exact_supported: std::sync::atomic::AtomicBool,
    starts: Mutex<Vec<(String, Vec<u8>)>>,
    queries: Mutex<Vec<String>>,
}

impl PaletteContinuationState {
    fn stable() -> Arc<Self> {
        Arc::new(Self {
            authority: std::sync::atomic::AtomicU8::new(0),
            draft_epoch: std::sync::atomic::AtomicU64::new(1),
            target_epoch: std::sync::atomic::AtomicU64::new(1),
            handler_epoch: std::sync::atomic::AtomicU64::new(1),
            draft_supported: std::sync::atomic::AtomicBool::new(true),
            exact_supported: std::sync::atomic::AtomicBool::new(true),
            starts: Mutex::new(Vec::new()),
            queries: Mutex::new(Vec::new()),
        })
    }
}

struct PaletteHistoryOwner {
    text: crate::widget::Text,
    state: Arc<PaletteContinuationState>,
}

impl PaletteHistoryOwner {
    fn action(
        &self,
        action: &'static str,
        operation: &'static str,
        typed: bool,
    ) -> tilo_actions::ActionSpec {
        let mut spec = if action == "named" {
            tilo_actions::ActionSpec::typed_operation(
                action,
                "Named text",
                "Target",
                tilo_actions::OperationKind::Effect,
                operation,
                vec![tilo_actions::ActionParam::required(
                    "target",
                    tilo_actions::ParamType::Text,
                    "Target",
                )],
                move |arguments| {
                    tilo_actions::EncodedAction::encode_operation(operation, &arguments.to_json())
                        .unwrap()
                },
            )
        } else if typed {
            tilo_actions::ActionSpec::typed_operation(
                action,
                "Nested JSON alias",
                "Strict JSON arguments",
                tilo_actions::OperationKind::Effect,
                operation,
                vec![
                    tilo_actions::ActionParam::required(
                        "alias",
                        tilo_actions::ParamType::Text,
                        "Mutable alias",
                    ),
                    tilo_actions::ActionParam::required(
                        "nested",
                        tilo_actions::ParamType::Object,
                        "Nonempty nested object",
                    ),
                ],
                move |arguments| {
                    tilo_actions::EncodedAction::encode_operation(operation, &arguments.to_json())
                        .unwrap()
                },
            )
        } else {
            tilo_actions::ActionSpec::prompted_operation(
                action,
                "Text alias",
                "Alias",
                "alias",
                tilo_actions::OperationKind::Effect,
                operation,
                move |value| {
                    tilo_actions::EncodedAction::encode_operation(operation, &value).unwrap()
                },
            )
        };
        spec.type_mut().contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase {
            phrase: PALETTE_CONFIRMATION.into(),
        };
        if self.state.authority.load(Ordering::SeqCst) == 2 {
            spec.type_mut().contract.availability = tilo_actions::Availability::Unavailable {
                code: "fixture-unavailable".into(),
                reason: "Current action unavailable".into(),
            };
        }
        let draft_epoch = self.state.draft_epoch.load(Ordering::SeqCst);
        let target_epoch = self.state.target_epoch.load(Ordering::SeqCst);
        let handler_epoch = self.state.handler_epoch.load(Ordering::SeqCst);
        let draft_supported = self.state.draft_supported.load(Ordering::SeqCst);
        let exact_supported = self.state.exact_supported.load(Ordering::SeqCst);
        let state = self.state.clone();
        spec.with_continuation_identity(move |query| match query {
            tilo_actions::ContinuationQuery::Exact(arguments) if exact_supported => {
                let (kind, input) = match arguments {
                    tilo_actions::ContinuationArguments::Immediate => ("immediate", Vec::new()),
                    tilo_actions::ContinuationArguments::Text(value) => {
                        ("text", value.as_bytes().to_vec())
                    }
                    tilo_actions::ContinuationArguments::Json(value) => {
                        ("json", serde_json::to_vec(value).unwrap())
                    }
                };
                state
                    .queries
                    .lock()
                    .unwrap()
                    .push(format!("exact:{action}:{kind}"));
                Some(tilo_actions::ContinuationEvidence::Exact {
                    target: tilo_actions::ContinuationKey::new(
                        "ui.history.palette.target",
                        vec![
                            action.as_bytes().to_vec(),
                            target_epoch.to_be_bytes().to_vec(),
                            input,
                        ],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.palette.handler",
                        vec![
                            action.as_bytes().to_vec(),
                            handler_epoch.to_be_bytes().to_vec(),
                        ],
                    ),
                })
            }
            tilo_actions::ContinuationQuery::Draft if draft_supported => {
                state
                    .queries
                    .lock()
                    .unwrap()
                    .push(format!("draft:{action}"));
                Some(tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: tilo_actions::ContinuationKey::new(
                        "ui.history.palette.domain",
                        vec![
                            action.as_bytes().to_vec(),
                            draft_epoch.to_be_bytes().to_vec(),
                        ],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.palette.handler",
                        vec![
                            action.as_bytes().to_vec(),
                            handler_epoch.to_be_bytes().to_vec(),
                        ],
                    ),
                })
            }
            tilo_actions::ContinuationQuery::Exact(_) | tilo_actions::ContinuationQuery::Draft => {
                None
            }
        })
    }
}

impl crate::widget::Widget for PaletteHistoryOwner {
    crate::provides!(
        dyn crate::widget::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn name(&self) -> &str {
        "Palette history owner"
    }
    fn test_id(&self) -> Option<&str> {
        Some("palette-history-owner")
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.text.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
        node
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.text.apply_layout(tree, node, origin);
    }
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.text.draw(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.text.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.text.handle_event(event, ctx);
    }
}

impl crate::widget::ResourceOperations for PaletteHistoryOwner {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [PALETTE_TEXT_OPERATION.into(), PALETTE_JSON_OPERATION.into()]
            .into_iter()
            .collect()
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let crate::host_operation::OperationRequest {
            type_id, payload, ..
        } = op.request();
        if !crate::widget::resource_operation_handlers(self).contains(type_id) {
            return Ok(None);
        }
        self.state
            .starts
            .lock()
            .unwrap()
            .push((type_id.to_owned(), payload.to_vec()));
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::widget::resources::PublishesResources for PaletteHistoryOwner {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        if self.state.authority.load(Ordering::SeqCst) == 1 {
            return vec![];
        }
        vec![
            tilo_actions::ResourceSpec::new("palette-owner", "fixture.palette", "Palette owner")
                .with_action(self.action("text", PALETTE_TEXT_OPERATION, false))
                .with_action(self.action("json", PALETTE_JSON_OPERATION, true))
                .with_action(self.action("named", PALETTE_TEXT_OPERATION, false)),
        ]
    }
}

fn palette_history_app(state: Arc<PaletteContinuationState>) -> App {
    let mut app = app();
    app.root_widget = Box::new(PaletteHistoryOwner {
        text: crate::widget::Text::new("Palette history owner"),
        state,
    });
    layout(&mut app);
    app
}

fn palette_activate(app: &mut App, action: &str, value: &str) {
    app.open_action_palette();
    assert!(
        app.actions
            .action_palette
            .as_deref_mut()
            .unwrap()
            .activate_address("palette-owner", action)
    );
    app.actions
        .action_palette
        .as_deref_mut()
        .unwrap()
        .set_query(value);
}

fn palette_enter(app: &mut App) {
    let event = crate::synthetic_input::key_input(
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        winit::event::ElementState::Pressed,
    );
    assert!(app.action_palette_handle_key(&event));
}

/// Go ahead with the typed confirmation through its own button, past
/// Cancel: Return in the field for the name stays the safe answer.
fn palette_confirm(app: &mut App) {
    for key in [
        winit::keyboard::NamedKey::Tab,
        winit::keyboard::NamedKey::Tab,
        winit::keyboard::NamedKey::Enter,
    ] {
        let event = crate::synthetic_input::key_input(
            &winit::keyboard::Key::Named(key),
            winit::event::ElementState::Pressed,
        );
        assert!(app.action_palette_handle_key(&event));
    }
}

fn palette_history_round_trip(app: &mut App) {
    let named_action_target = named(app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(app);
    let named_action_target = named(app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(app);
}

fn editor_snapshot(app: &App) -> (String, Option<(usize, usize)>, &'static str) {
    crate::action_palette::observed_history_editor(app.action_palette().unwrap())
}

tilo_test_env::scenario! {
    fn managed_palette_drafts_keep_identity_through_history;

    phase managed_palette_draft_identity_preserves_text_and_json_editor_and_refuses_changed_or_absent_domain;
    {
        for (action, partial, complete, mutation, should_start) in [
            ("text", "ali", "alias-a", "stable", true),
            ("text", "ali", "alias-a", "changed", false),
            (
                "json",
                r#"{"alias":"a","nested":{"path":"#,
                r#"{"alias":"a","nested":{"path":{"leaf":1}}}"#,
                "stable",
                true,
            ),
            (
                "json",
                r#"{"alias":"a","nested":{"path":"#,
                r#"{"alias":"a","nested":{"path":{"leaf":1}}}"#,
                "absent",
                false,
            ),
        ] {
            let state = PaletteContinuationState::stable();
            if mutation == "absent" {
                state.draft_supported.store(false, Ordering::SeqCst);
            }
            let mut app = palette_history_app(state.clone());
            palette_activate(&mut app, action, partial);
            let before = editor_snapshot(&app);
            assert_eq!(before.0, partial);
            assert!(before.1.is_some(), "the actual editor exposes its cursor");
            assert_eq!(before.2, "argument");

            palette_history_round_trip(&mut app);
            assert_eq!(editor_snapshot(&app), before, "{action}:{mutation}");
            if mutation == "changed" {
                state.draft_epoch.store(2, Ordering::SeqCst);
            }
            app.actions
                .action_palette
                .as_deref_mut()
                .unwrap()
                .set_query(complete);
            palette_enter(&mut app);
            assert_eq!(editor_snapshot(&app).2, "confirmation");
            app.actions
                .action_palette
                .as_deref_mut()
                .unwrap()
                .set_query(PALETTE_CONFIRMATION);
            palette_confirm(&mut app);

            let starts = state.starts.lock().unwrap().len();
            assert_eq!(starts, usize::from(should_start), "{action}:{mutation}");
            if should_start {
                assert!(
                    app.action_palette().is_none(),
                    "successful dispatch closes palette"
                );
            } else {
                let snapshot = editor_snapshot(&app);
                // The name was typed in the confirmation's own plain field,
                // never in the search field.
                assert_eq!(snapshot.0, "");
                assert_eq!(snapshot.2, "ready");
                assert!(
                    app.actions
                        .action_palette
                        .as_deref_mut()
                        .unwrap()
                        .take_search_announcement()
                        .is_some_and(
                            |message| message.contains("stale") || message.contains("identity")
                        ),
                    "refusal remains visibly announced"
                );
            }
        }
    }

    phase managed_palette_exact_text_and_nested_json_recheck_alias_and_handler_around_fresh_phrase;
    {
        for (action, complete, mutation, should_start) in [
            ("text", "production", "unrelated", true),
            (
                "json",
                r#"{"alias":"production","nested":{"route":{"shard":7}}}"#,
                "unrelated",
                true,
            ),
            ("text", "production", "target-before-return", false),
            (
                "json",
                r#"{"alias":"production","nested":{"route":{"shard":7}}}"#,
                "handler-changed-before-going-ahead",
                false,
            ),
        ] {
            let state = PaletteContinuationState::stable();
            let mut app = palette_history_app(state.clone());
            palette_activate(&mut app, action, complete);
            palette_enter(&mut app);
            assert_eq!(editor_snapshot(&app).2, "confirmation");
            app.actions
                .action_palette
                .as_deref_mut()
                .unwrap()
                .set_query(PALETTE_CONFIRMATION);

            let named_action_target = named(&mut app, "History");
            app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
            layout(&mut app);
            if mutation == "target-before-return" {
                state.target_epoch.store(2, Ordering::SeqCst);
            } else {
                app.state
                    .doc
                    .put(ROOT, "unrelated-round3", mutation)
                    .unwrap();
            }
            let named_action_target = named(&mut app, "History");
            app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
            layout(&mut app);
            assert_eq!(
                editor_snapshot(&app),
                (String::new(), Some((0, 0)), "confirmation"),
                "History return requires a fresh phrase: {action}:{mutation}"
            );

            if mutation == "handler-changed-before-going-ahead" {
                assert_eq!(state.starts.lock().unwrap().len(), 0);
                state.handler_epoch.store(2, Ordering::SeqCst);
            }
            app.actions
                .action_palette
                .as_deref_mut()
                .unwrap()
                .set_query(PALETTE_CONFIRMATION);
            palette_confirm(&mut app);
            assert_eq!(
                state.starts.lock().unwrap().len(),
                usize::from(should_start),
                "{action}:{mutation}"
            );
            let queries = state.queries.lock().unwrap().clone();
            assert!(
                queries
                    .iter()
                    .any(|query| query == &format!("exact:{action}:{action}")),
                "exact Text/JSON representation is passed to the resolver: {queries:?}"
            );
        }
    }
}

#[test]
fn host_owned_chrome_opens_framework_history_without_its_toolbar_button() {
    use crate::host_chrome::model::CommandGroup;
    let has_open_button = |app: &App| {
        app.layout.node_map.keys().any(|node| {
            app.inspect_node(*node, |widget| widget.test_id() == Some("history-open"))
                .unwrap_or(false)
        })
    };
    let mut in_window = app();
    in_window.prepare_native_presentation(crate::host_chrome::HostChrome::InWindow);
    layout(&mut in_window);
    assert!(
        !has_open_button(&in_window),
        "an app without a header must not gain a duplicate top bar"
    );
    let api = in_window
        .host_commands(false)
        .into_iter()
        .find(|command| command.group == CommandGroup::History)
        .expect("History stays available through the platform action");
    assert!(api.enabled);
    assert!(in_window.invoke_host_command(&api.invocation().unwrap()));
    layout(&mut in_window);
    assert!(in_window.history.as_ref().unwrap().is_open());

    let mut app = app();
    app.prepare_native_presentation(crate::host_chrome::HostChrome::Native);
    layout(&mut app);
    assert!(!has_open_button(&app));
    let show = app
        .host_commands(false)
        .into_iter()
        .find(|command| command.group == CommandGroup::History)
        .expect("the menu bar presents framework History");
    assert_eq!(show.label, "Show History");
    assert!(show.enabled);
    assert!(app.invoke_host_command(&show.invocation().unwrap()));
    layout(&mut app);
    assert!(app.history.as_ref().unwrap().is_open());
    assert!(
        has_open_button(&app),
        "open History keeps its way back to the live app"
    );

    let hide = app
        .host_commands(false)
        .into_iter()
        .find(|command| command.group == CommandGroup::History)
        .unwrap();
    assert_eq!(hide.label, "Hide History");
    assert!(app.invoke_host_command(&hide.invocation().unwrap()));
    layout(&mut app);
    assert!(!app.history.as_ref().unwrap().is_open());
    assert!(!has_open_button(&app));
}

/// The History control in a `NavShell` app's header row opens and closes
/// the app's registered History, as that History's own button does.
#[test]
fn header_history_control_opens_framework_history() {
    let history_buttons = |app: &mut App| -> Vec<Option<accesskit::Toggled>> {
        app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.label() == Some("History") && node.role() == accesskit::Role::Button
            })
            .map(|(_, node)| node.toggled())
            .collect()
    };
    let mut state = State::new();
    for body in ["old", "live"] {
        state.doc.put(ROOT, "body", body).unwrap();
        state.commit_action(None, None);
    }
    let shell = crate::widget::NavShell::new()
        .content_header("Notes", vec![])
        .content(Box::new(crate::widget::Text::new("Body")));
    let mut app = app_with_root(state, Box::new(shell));
    layout(&mut app);
    assert_eq!(history_buttons(&mut app), [Some(accesskit::Toggled::False)]);

    let header = node_for_test_id(&app, "nav-shell-history");
    let bounds = app
        .inspect_node(header, |widget| widget.get_rect())
        .unwrap();
    assert!(app.dispatch_semantic_click(bounds.center().x, bounds.center().y));
    layout(&mut app);
    assert!(app.history.as_ref().unwrap().is_open());
    assert_eq!(
        history_buttons(&mut app),
        [Some(accesskit::Toggled::True)],
        "the one History control reads as on while History shows"
    );

    let close = named(&mut app, "History");
    app.dispatch_a11y_action(close, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(!app.history.as_ref().unwrap().is_open());
    assert_eq!(history_buttons(&mut app), [Some(accesskit::Toggled::False)]);
}

#[test]
fn host_chrome_lists_framework_history_changes_and_opens_at_the_chosen_one() {
    use crate::host_chrome::model::CommandGroup;
    let mut app = native_chrome_app();
    let recent: Vec<_> = app
        .host_commands(true)
        .into_iter()
        .filter(|command| command.group == CommandGroup::RecentChanges)
        .collect();
    assert_eq!(recent.len(), 2, "both committed changes are listed");
    assert!(
        recent
            .iter()
            .all(|command| command.enabled && command.label.contains(" by "))
    );
    let entry = |command: &crate::host_chrome::model::HostCommand| {
        command.invocation().unwrap().arguments["entry"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_ne!(entry(&recent[0]), entry(&recent[1]));

    // The older change, so opening at it is not the timeline's default.
    let chosen = &recent[1];
    assert!(app.invoke_host_command(&chosen.invocation().unwrap()));
    layout(&mut app);
    let history = app.history.as_ref().unwrap();
    assert!(history.is_open());
    let selected: Vec<_> = history
        .timeline_selection()
        .into_iter()
        .map(crate::history_mode::api::encode_entry)
        .collect();
    assert_eq!(selected, [entry(chosen)]);
    assert!(
        app.layout.node_map.keys().any(|node| {
            app.inspect_node(*node, |widget| widget.test_id() == Some("history-before"))
                .unwrap_or(false)
        }),
        "the chosen change shows its before and after"
    );
    assert_eq!(
        app.host_commands(true)
            .iter()
            .filter(|command| command.group == CommandGroup::RecentChanges)
            .count(),
        2,
        "open History still lists its changes"
    );
}

#[cfg(unix)]
include!("initial_history_action_tests.rs");

/// A press on the side panel's Restore… opens the restore review: the move from the row a person
/// chose to the panel's foot leaves the timeline's hovered row behind, and that must not keep the
/// footer's controls from learning the pointer is over them.
#[test]
fn a_pointer_press_on_restore_opens_the_review() {
    let mut app = app();
    layout(&mut app);
    let target = named(&mut app, "History");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let row = app
        .build_a11y_tree_update(1.0)
        .nodes
        .into_iter()
        .find(|(_, node)| node.role() == accesskit::Role::ListItem)
        .and_then(|(_, node)| node.bounds())
        .expect("History lists the changes");
    app.dispatch_semantic_click(f64::midpoint(row.x0, row.x1), f64::midpoint(row.y0, row.y1));
    layout(&mut app);
    let restore = app
        .build_a11y_tree_update(1.0)
        .nodes
        .into_iter()
        .find(|(_, node)| node.label() == Some("Restore…"))
        .and_then(|(_, node)| node.bounds())
        .expect("the panel's foot offers to restore the chosen version");
    app.dispatch_semantic_click(
        f64::midpoint(restore.x0, restore.x1),
        f64::midpoint(restore.y0, restore.y1),
    );
    layout(&mut app);
    assert!(
        app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Restore version")),
        "the press on Restore… opens the review"
    );
}
