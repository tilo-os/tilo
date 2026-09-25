use super::*;
use crate::history_mode::*;
use automerge::{ROOT, transaction::Transactable};
static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "dev.qos.native-history-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Native notebook",
        presentation: HistoryPresentation::Domain {
            create: crate::history_mode::test_preview,
        },
        content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
    }],
};
fn app() -> App {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "native-fixture",
                "lifetime-904",
                HISTORY.app_id,
                crate::RecordId::for_tab(904),
            ),
        }],
    )
    .unwrap();
    App::from_managed_root_with_controlled_inputs(
        state,
        ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
    )
    .unwrap()
}
fn layout(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(app, root, 900, 650, 1.0, true, None).is_some());
    let mut scene = vello::Scene::new();
    draw_frame_content(app, &mut scene, 900, 650, 1.0, None);
    complete_native_frame_content(app, |_| {});
}
#[test]
fn native_managed_app_automatically_composes_same_history_surface() {
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
        let event = crate::synthetic_input::key_event(&logical, state);
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
#[test]
fn native_dirty_editor_real_history_click_preserves_pending_heads_and_semantic_focus() {
    let mut app = app();
    app.root_widget = Box::new(crate::widget::Column::new().push(Box::new(
        crate::widget::TextInput::from_document_string("draft").with_id("draft-editor"),
    )));
    app.root_widget.sync(&mut app.state.doc, &ROOT);
    layout(&mut app);
    let editor = node_for_test_id(&app, "draft-editor");
    app.set_focused_node(Some(editor));
    app.dispatch_node(editor, |widget, doc, path| {
        widget.perform_action(
            &crate::a11y::Action::SetValue("unfinished".into()),
            doc,
            path,
        )
    })
    .unwrap();
    app.dispatch_node(editor, |widget, doc, path| {
        widget.perform_action(
            &crate::a11y::Action::SetSelection {
                anchor: 2,
                focus: 7,
            },
            doc,
            path,
        )
    })
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
    // Opening the panel then closing the actual entry preserves the editor.
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
#[test]
fn native_pointer_version_selection_restore_dialog_and_back_live_use_shared_surface() {
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

#[test]
fn native_component_bundle_installs_history_in_existing_component_wake_scope() {
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

#[test]
fn replacing_installed_autosave_invalidates_history_and_old_modal_action() {
    let mut app = app();
    layout(&mut app);
    let target = named(&mut app, "History");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let row = app
        .build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::ListItem)
        .map(|(id, _)| *id)
        .unwrap();
    app.dispatch_a11y_action(row, &crate::a11y::Action::Activate);
    layout(&mut app);
    let target = named(&mut app, "Restore…");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let old = named(&mut app, "Restore version");
    let heads = app.state.doc.base_heads();
    app = app.with_autosave(crate::persist::DocWriter::disabled("replacement"));
    layout(&mut app);
    assert!(app.validate_history_registration().is_err());
    app.dispatch_a11y_action(old, &crate::a11y::Action::Activate);
    assert_eq!(app.state.doc.base_heads(), heads);
}

fn routed_document(
    route: &crate::collaboration::RouteSpec,
    mutation: collab_types::DocumentMutation,
) -> collab_types::SignedOp {
    collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::document_attestation_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::OpClass::Splice,
        crate::time::SystemTime::UNIX_EPOCH,
        0,
    )
}
#[test]
fn native_document_envelope_refuses_foreign_route_without_touching_outer_state() {
    use automerge::ReadDoc;
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(crate::widget::Text::new("root")));
    let installed = crate::collaboration::RouteSpec::app("installed-app", uuid::Uuid::new_v4());
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: installed.clone(),
        epoch: uuid::Uuid::new_v4(),
    });
    let mut foreign = installed;
    foreign.record = Some("foreign-record".into());
    let mut incoming = automerge::AutoCommit::new();
    incoming.put(ROOT, "foreign-body", "must stay out").unwrap();
    app.apply_signed_document_envelope(
        &routed_document(
            &foreign,
            collab_types::DocumentMutation::Snapshot(incoming.save()),
        ),
        None,
    );
    assert!(
        app.state.doc.get(ROOT, "foreign-body").unwrap().is_none(),
        "route-foreign envelope must not fall into outer State"
    );
}

struct RoutedOwners {
    app: App,
    records: crate::RecordStore,
    bindings: Vec<crate::bound_dispatch::CallbackBinding>,
    _owners: crate::record::RecordOwners,
    _temp: tempfile::TempDir,
}
impl RoutedOwners {
    fn new() -> Self {
        use crate::application::{HostContext, LaunchTarget, RestoreInput};
        use crate::application_test_support::{
            APP, author, provenance, runner_permissions, runner_probe_definition,
        };
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().to_path_buf());
        let workspace = "00000000-0000-0000-0000-000000000077";
        let records = owners.open(APP, workspace).unwrap();
        let views = std::rc::Rc::new(
            crate::application_view::InstanceViewStore::new(
                temp.path().to_path_buf(),
                "D",
                workspace,
            )
            .unwrap(),
        );
        let definition = runner_probe_definition();
        let mut root = crate::widget::Column::new();
        let mut bindings = vec![];
        for instance in [10, 11] {
            let context = HostContext::prepare(
                definition,
                records.clone(),
                views.clone(),
                "D".into(),
                instance,
                runner_permissions(),
                author(),
                provenance(),
                &LaunchTarget::New,
            )
            .unwrap();
            let mut component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            component.mount(|_| {}).unwrap();
            bindings.push(component.callback_binding());
            root = root.push(Box::new(crate::widget::BoundAppWidget { component }));
        }
        let mut state = State::new();
        state.doc.put(ROOT, "outer", "outer only").unwrap();
        state.commit_action(None, None);
        let mut app = App::new_with_controlled_inputs(state, Box::new(root));
        app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
            route: crate::collaboration::RouteSpec::app(
                "outer-shell",
                uuid::Uuid::parse_str(workspace).unwrap(),
            ),
            epoch: uuid::Uuid::new_v4(),
        });
        app.refresh_document_routes();
        Self {
            app,
            records,
            bindings,
            _owners: owners,
            _temp: temp,
        }
    }
    fn remote(&self, index: usize, name: &str, incremental: bool) -> collab_types::SignedOp {
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
            String,
            crate::application::InstanceId,
            crate::undo::UndoStatus,
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
                    actions.statuses_for_test(),
                )
            })
            .unwrap()
    }
    fn convert_to_qos_metadata(&self, index: usize) {
        let checkout = &self.bindings[index].checkout;
        let workspace = self.bindings[index]
            .broker_route()
            .unwrap()
            .cluster_id
            .to_string();
        self.records
            .with_action_state(&checkout.id, |doc, history, _| {
                doc.delete(ROOT, "tilo:record").unwrap();
                let meta = doc
                    .put_object(ROOT, "qos:record", automerge::ObjType::Map)
                    .unwrap();
                doc.put(&meta, "id", checkout.id.as_str()).unwrap();
                doc.put(&meta, "kind", checkout.kind.as_str()).unwrap();
                doc.put(&meta, "home", &workspace).unwrap();
                doc.put(&meta, "room", &workspace).unwrap();
                doc.put_object(&meta, "refs", automerge::ObjType::List)
                    .unwrap();
                doc.commit();
                history.rebase(doc);
            })
            .unwrap();
    }
}

#[test]
fn signed_owned_foreign_identity_is_rejected_without_mutating_live_record() {
    use automerge::ReadDoc;

    let mut fixture = RoutedOwners::new();
    let binding = &fixture.bindings[0];
    let before = fixture
        .records
        .read_history(&binding.checkout.id, |doc, history| {
            (doc.base_heads(), doc.pending_ops(), history.revision())
        })
        .unwrap();
    let generation = fixture
        .records
        .action_generation(&binding.checkout.id)
        .unwrap();
    let mut remote = fixture
        .records
        .read_checked(&binding.checkout.id, Clone::clone)
        .unwrap();
    remote.set_actor(automerge::ActorId::from(b"foreign-identity".as_slice()));
    let metadata = remote.get(ROOT, "tilo:record").unwrap().unwrap().1;
    remote
        .put(&metadata, "id", "uuid:00000000-0000-4000-8000-000000000099")
        .unwrap();
    remote
        .put(ROOT, "foreign-content", "must not enter")
        .unwrap();
    remote.commit();
    let incoming = routed_document(
        &binding.broker_route().unwrap(),
        collab_types::DocumentMutation::Snapshot(remote.save()),
    );

    fixture.app.apply_signed_document_envelope(&incoming, None);

    assert_eq!(
        fixture
            .records
            .read_history(&binding.checkout.id, |doc, history| (
                doc.base_heads(),
                doc.pending_ops(),
                history.revision(),
            ))
            .unwrap(),
        before,
    );
    assert_eq!(
        fixture
            .records
            .action_generation(&binding.checkout.id)
            .unwrap(),
        generation,
    );
    assert_eq!(fixture.value(0, "foreign-content"), None);
    assert!(fixture.app.document_sync_status().is_some());
}

fn identity_mutation(
    fixture: &RoutedOwners,
    index: usize,
    family: &str,
    field: &str,
    incremental: bool,
) -> collab_types::SignedOp {
    use automerge::ReadDoc;

    let binding = &fixture.bindings[index];
    let mut remote = fixture
        .records
        .read_checked(&binding.checkout.id, Clone::clone)
        .unwrap();
    let baseline = remote.base_heads();
    remote.set_actor(automerge::ActorId::from(
        format!("foreign-{family}-{field}").as_bytes(),
    ));
    let metadata = remote.get(ROOT, family).unwrap().unwrap().1;
    let value = match field {
        "id" => "uuid:00000000-0000-4000-8000-000000000099",
        "kind" => "foreign-kind",
        "room" => "00000000-0000-4000-8000-000000000099",
        "home" => "foreign-origin",
        _ => unreachable!(),
    };
    remote.put(&metadata, field, value).unwrap();
    remote.put(ROOT, "foreign-content", field).unwrap();
    remote.commit();
    routed_document(
        &binding.broker_route().unwrap(),
        if incremental {
            collab_types::DocumentMutation::Incremental(remote.save_after(&baseline))
        } else {
            collab_types::DocumentMutation::Snapshot(remote.save())
        },
    )
}

#[test]
fn signed_owned_identity_mutations_are_atomic_invalid_and_recover_with_fresh_snapshot() {
    use automerge::ReadDoc;

    for qos in [false, true] {
        for incremental in [false, true] {
            for field in ["id", "kind", "room", "home"] {
                let mut fixture = RoutedOwners::new();
                if qos {
                    fixture.convert_to_qos_metadata(0);
                    fixture.app.refresh_document_routes();
                }
                let family = if qos { "qos:record" } else { "tilo:record" };
                let incoming = identity_mutation(&fixture, 0, family, field, incremental);
                let before = fixture.observation(0);
                fixture.app.apply_signed_document_envelope(&incoming, None);
                assert_eq!(fixture.observation(0), before, "{family} {field}");
                assert_eq!(fixture.value(0, "foreign-content"), None);
                assert!(
                    fixture
                        .app
                        .document_sync_status()
                        .is_some_and(|status| status.contains("could not be applied"))
                );
                let route = fixture.bindings[0].broker_route().unwrap();
                let pending = &fixture
                    .app
                    .document_routes
                    .owners
                    .iter()
                    .find(|entry| entry.route == route)
                    .unwrap()
                    .pending;
                assert!(pending.is_empty(), "invalid front must be quarantined");

                let mut recovery = fixture
                    .records
                    .read_checked(&fixture.bindings[0].checkout.id, Clone::clone)
                    .unwrap();
                recovery.set_actor(automerge::ActorId::from(b"valid-recovery".as_slice()));
                recovery.put(ROOT, "valid-after-recovery", field).unwrap();
                recovery.commit();
                let recovery = routed_document(
                    &route,
                    collab_types::DocumentMutation::Snapshot(recovery.save()),
                );
                fixture.app.apply_signed_document_envelope(&recovery, None);
                assert_eq!(
                    fixture.value(0, "valid-after-recovery").as_deref(),
                    Some(field)
                );
                assert_eq!(fixture.app.document_sync_status(), None);
                if qos {
                    fixture
                        .records
                        .read_checked(&fixture.bindings[0].checkout.id, |doc| {
                            assert!(doc.get(ROOT, "qos:record").unwrap().is_some());
                            assert!(doc.get(ROOT, "tilo:record").unwrap().is_none());
                        })
                        .unwrap();
                }
                let (_, _, revision, generation, statuses) = fixture.observation(0);
                assert_eq!(revision, before.2);
                assert_eq!(generation, before.3.and_then(|value| value.checked_add(1)));
                assert!(statuses.is_empty());
            }
        }
    }
}

#[test]
fn signed_owned_corrupt_incremental_preserves_document_history_generation_and_statuses() {
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
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(fixture.observation(0), before);
    assert_eq!(fixture.value(0, "valid-prefix"), None);
}

#[test]
fn signed_owned_concurrent_losing_identity_values_are_rejected() {
    use automerge::ReadDoc;
    for qos in [false, true] {
        for incremental in [false, true] {
            for field in ["id", "kind", "room", "home"] {
                let mut fixture = RoutedOwners::new();
                if qos {
                    fixture.convert_to_qos_metadata(0);
                    fixture.app.refresh_document_routes();
                }
                let binding = &fixture.bindings[0];
                let family = if qos { "qos:record" } else { "tilo:record" };
                let base = fixture
                    .records
                    .read_checked(&binding.checkout.id, Clone::clone)
                    .unwrap();
                let base_heads = base.base_heads();
                let metadata = base.get(ROOT, family).unwrap().unwrap().1;
                let expected = base
                    .get(&metadata, field)
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str()
                    .unwrap()
                    .to_string();
                let mut remote = base.clone();
                remote.set_actor(automerge::ActorId::from(b"foreign-concurrent".as_slice()));
                let remote_meta = remote.get(ROOT, family).unwrap().unwrap().1;
                remote
                    .put(
                        &remote_meta,
                        field,
                        if field == "id" {
                            "uuid:00000000-0000-4000-8000-000000000099"
                        } else {
                            "foreign-concurrent"
                        },
                    )
                    .unwrap();
                remote.commit();
                fixture
                    .records
                    .with_action_state(&binding.checkout.id, |doc, history, _| {
                        let local_meta = doc.get(ROOT, family).unwrap().unwrap().1;
                        doc.put(&local_meta, field, expected).unwrap();
                        doc.commit();
                        history.rebase(doc);
                    })
                    .unwrap();
                let incoming = routed_document(
                    &binding.broker_route().unwrap(),
                    if incremental {
                        collab_types::DocumentMutation::Incremental(remote.save_after(&base_heads))
                    } else {
                        collab_types::DocumentMutation::Snapshot(remote.save())
                    },
                );
                let before = fixture.observation(0);
                fixture.app.apply_signed_document_envelope(&incoming, None);
                assert_eq!(fixture.observation(0), before, "{family} {field}");
            }
        }
    }
}

#[test]
fn signed_owned_metadata_family_cannot_switch_or_coexist() {
    for qos in [false, true] {
        for incremental in [false, true] {
            let mut fixture = RoutedOwners::new();
            if qos {
                fixture.convert_to_qos_metadata(0);
                fixture.app.refresh_document_routes();
            }
            let binding = &fixture.bindings[0];
            let mut remote = fixture
                .records
                .read_checked(&binding.checkout.id, Clone::clone)
                .unwrap();
            let baseline = remote.base_heads();
            remote.set_actor(automerge::ActorId::from(b"family-switch".as_slice()));
            if qos {
                remote.delete(ROOT, "qos:record").unwrap();
            }
            let other = if qos { "tilo:record" } else { "qos:record" };
            let meta = remote
                .put_object(ROOT, other, automerge::ObjType::Map)
                .unwrap();
            remote
                .put(&meta, "id", binding.checkout.id.as_str())
                .unwrap();
            remote
                .put(&meta, "kind", binding.checkout.kind.as_str())
                .unwrap();
            let workspace = binding.broker_route().unwrap().cluster_id.to_string();
            remote.put(&meta, "home", &workspace).unwrap();
            remote.put(&meta, "room", &workspace).unwrap();
            remote
                .put_object(&meta, "refs", automerge::ObjType::List)
                .unwrap();
            remote.commit();
            let incoming = routed_document(
                &binding.broker_route().unwrap(),
                if incremental {
                    collab_types::DocumentMutation::Incremental(remote.save_after(&baseline))
                } else {
                    collab_types::DocumentMutation::Snapshot(remote.save())
                },
            );
            let before = fixture.observation(0);
            fixture.app.apply_signed_document_envelope(&incoming, None);
            assert_eq!(fixture.observation(0), before);
        }
    }
}

#[derive(Clone, Copy)]
enum PrimaryMetadataShape {
    None,
    Tilo(bool),
    Qos(bool),
}
impl PrimaryMetadataShape {
    fn family(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Tilo(_) => Some("tilo:record"),
            Self::Qos(_) => Some("qos:record"),
        }
    }
    fn scoped(self) -> bool {
        matches!(self, Self::Tilo(true) | Self::Qos(true))
    }
}

#[allow(clippy::needless_pass_by_value, reason = "factory contract")]
fn primary_routed_factory(
    mut context: crate::application::HostContext,
    target: crate::application::LaunchTarget,
    restore: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    if matches!(target, crate::application::LaunchTarget::PrimaryNew) {
        context
            .initialize_new_record(|doc| {
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
    records: crate::RecordStore,
    binding: crate::bound_dispatch::CallbackBinding,
    _owners: crate::record::RecordOwners,
    _temp: tempfile::TempDir,
}
impl RoutedPrimary {
    fn new(shape: PrimaryMetadataShape) -> Self {
        use crate::application::{ApplicationDefinition, HostContext, LaunchTarget, RestoreInput};
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().to_path_buf());
        let workspace = "00000000-0000-4000-8000-000000000078";
        let records = owners.open("dev.tilo.primary-probe", workspace).unwrap();
        let views = std::rc::Rc::new(
            crate::application_view::InstanceViewStore::new(
                temp.path().to_path_buf(),
                "D",
                workspace,
            )
            .unwrap(),
        );
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.primary-probe",
            title: "Primary Probe",
            icon: crate::widget::IconDescriptor::Glyph("P"),
            record_kind: crate::RecordKind::new("primary.probe"),
            required: vec![],
            optional: vec![],
            create: primary_routed_factory,
        };
        let target = LaunchTarget::PrimaryNew;
        let context = HostContext::prepare(
            &definition,
            records.clone(),
            views,
            "D".into(),
            70,
            std::rc::Rc::new(std::cell::RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            crate::permissions::UserId("primary-author".into()),
            tilo_actions::ActionContext::default(),
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
                        doc.put(&meta, "room", workspace).unwrap();
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
            Box::new(crate::widget::BoundAppWidget { component }),
        );
        app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
        app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
            route: binding.broker_route().unwrap(),
            epoch: uuid::Uuid::new_v4(),
        });
        app.refresh_document_routes();
        Self {
            app,
            records,
            binding,
            _owners: owners,
            _temp: temp,
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
            String,
            crate::application::InstanceId,
            crate::undo::UndoStatus,
        )>,
    ) {
        self.records
            .with_action_state(&self.binding.checkout.id, |doc, history, actions| {
                (
                    doc.base_heads(),
                    doc.pending_ops(),
                    history.revision(),
                    actions.generation,
                    actions.statuses_for_test(),
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
    ) -> collab_types::SignedOp {
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

#[test]
fn signed_package_primary_preserves_each_existing_metadata_shape() {
    for shape in [
        PrimaryMetadataShape::None,
        PrimaryMetadataShape::Tilo(false),
        PrimaryMetadataShape::Tilo(true),
        PrimaryMetadataShape::Qos(false),
        PrimaryMetadataShape::Qos(true),
    ] {
        for incremental in [false, true] {
            let mut fixture = RoutedPrimary::new(shape);
            let before = fixture.observation();
            let valid = fixture.envelope(incremental, |doc| {
                doc.put(ROOT, "valid-primary", "remote").unwrap();
            });
            fixture.app.apply_signed_document_envelope(&valid, None);
            assert_eq!(fixture.value("valid-primary").as_deref(), Some("remote"));
            let after = fixture.observation();
            assert_eq!(after.2, before.2);
            assert_eq!(after.3, before.3.and_then(|value| value.checked_add(1)));
            assert!(after.4.is_empty());
        }
    }
}

#[test]
fn signed_package_primary_refuses_identity_stamps_scope_changes_and_family_switches() {
    use automerge::ReadDoc;

    for shape in [
        PrimaryMetadataShape::None,
        PrimaryMetadataShape::Tilo(false),
        PrimaryMetadataShape::Tilo(true),
        PrimaryMetadataShape::Qos(false),
        PrimaryMetadataShape::Qos(true),
    ] {
        for incremental in [false, true] {
            let mut attacks = vec!["id", "kind", "home", "room", "family"];
            if shape.scoped() {
                attacks.extend(["delete-home", "delete-room"]);
            }
            for attack in attacks {
                let mut fixture = RoutedPrimary::new(shape);
                let before = fixture.observation();
                let incoming = fixture.envelope(incremental, |doc| {
                    let family = shape.family().unwrap_or("tilo:record");
                    let meta = if let Some((_, object)) = doc.get(ROOT, family).unwrap() {
                        object
                    } else {
                        doc.put_object(ROOT, family, automerge::ObjType::Map)
                            .unwrap()
                    };
                    match attack {
                        "id" => doc
                            .put(&meta, "id", "uuid:00000000-0000-4000-8000-000000000099")
                            .unwrap(),
                        "kind" => doc.put(&meta, "kind", "foreign-kind").unwrap(),
                        "home" => doc.put(&meta, "home", "foreign-home").unwrap(),
                        "room" => doc
                            .put(&meta, "room", "00000000-0000-4000-8000-000000000099")
                            .unwrap(),
                        "delete-home" => {
                            let _ = doc.delete(&meta, "home");
                        }
                        "delete-room" => {
                            let _ = doc.delete(&meta, "room");
                        }
                        "family" => {
                            let other = if family == "tilo:record" {
                                "qos:record"
                            } else {
                                "tilo:record"
                            };
                            doc.put_object(ROOT, other, automerge::ObjType::Map)
                                .unwrap();
                        }
                        _ => unreachable!(),
                    }
                    doc.put(ROOT, "invalid-primary-content", attack).unwrap();
                });
                fixture.app.apply_signed_document_envelope(&incoming, None);
                assert_eq!(fixture.observation(), before, "primary {attack}");
                assert_eq!(fixture.value("invalid-primary-content"), None);
                assert!(fixture.app.document_sync_status().is_some());
                let pending = &fixture.app.document_routes.owners[0].pending;
                assert!(pending.is_empty());

                let recovery = fixture.envelope(false, |doc| {
                    doc.put(ROOT, "primary-recovered", attack).unwrap();
                });
                fixture.app.apply_signed_document_envelope(&recovery, None);
                assert_eq!(fixture.value("primary-recovered").as_deref(), Some(attack));
                assert_eq!(fixture.app.document_sync_status(), None);
            }
        }
    }
}
#[test]
fn native_owned_routes_isolate_two_documents_and_outer_for_snapshot_incremental_and_publication() {
    use automerge::ReadDoc;
    let mut fixture = RoutedOwners::new();
    let outer = fixture.app.state.doc.base_heads();
    let envelopes = [
        fixture.remote(0, "first-remote", false),
        fixture.remote(1, "second-remote", true),
    ];
    for envelope in &envelopes {
        fixture.app.apply_signed_document_envelope(envelope, None);
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
        "one app pump reconciles document custody exactly once"
    );
    assert_eq!(
        crate::history_mode::owner::OWNED_DOCUMENT_CLONES.get(),
        clones,
        "the unchanged pump path does not clone owned documents"
    );
}
#[test]
fn native_owned_remote_merge_is_not_republished_as_an_echo() {
    let mut fixture = RoutedOwners::new();
    let id = fixture.bindings[0].checkout.id.clone();
    let route = fixture.bindings[0].broker_route().unwrap();
    // Only the first owner is under test; the second may publish its own
    // initial snapshot whenever its asynchronous route join completes.
    let sent = |fixture: &RoutedOwners| {
        fixture
            .app
            .document_routes
            .sent
            .iter()
            .filter(|(sent, _)| *sent == route)
            .map(|(_, request)| request.clone())
            .collect::<Vec<_>>()
    };
    let heads = |fixture: &RoutedOwners| {
        fixture
            .records
            .read_checked(&id, |doc| doc.base_heads())
            .unwrap()
    };
    let published = |fixture: &RoutedOwners| match &sent(fixture)[..] {
        [request] => match request {
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Incremental(bytes),
                ..
            } => bytes.clone(),
            other => panic!("expected an incremental publication, got {other:?}"),
        },
        other => panic!("expected exactly one publication, got {}", other.len()),
    };
    let commit_local = |fixture: &RoutedOwners, value: &str| {
        fixture
            .records
            .with_history(&id, |doc, history| {
                doc.put(ROOT, "local", value).unwrap();
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
    };

    // The initial snapshot carries an authored change (the bare factory
    // record has no publishable provenance).
    commit_local(&fixture, "initial");
    // The fake broker admits the route join asynchronously; publication is
    // refused (and retried) until the route is publishable.
    let deadline = crate::time::now() + crate::time::Duration::from_secs(5);
    while sent(&fixture).is_empty() {
        fixture.app.publish_owned_documents();
        assert!(
            crate::time::now() < deadline,
            "route never became publishable"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    assert_eq!(sent(&fixture).len(), 1, "initial snapshot");
    fixture.app.document_routes.sent.clear();

    // Fully published before the merge: the merged remote history is held by
    // its sender and owner, so it is not echoed back as our Incremental.
    for incremental in [true, false] {
        let incoming = fixture.remote(0, &format!("remote-{incremental}"), incremental);
        fixture.app.apply_signed_document_envelope(&incoming, None);
        assert_eq!(
            fixture
                .value(0, &format!("remote-{incremental}"))
                .as_deref(),
            Some("remote")
        );
        fixture.app.publish_owned_documents();
        assert!(
            sent(&fixture).is_empty(),
            "a merged remote change must not be republished"
        );
    }

    // A later local edit publishes exactly its own change.
    let before = heads(&fixture);
    commit_local(&fixture, "after merge");
    fixture.app.publish_owned_documents();
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    assert_eq!(published(&fixture), expected);
    fixture.app.document_routes.sent.clear();

    // Unsent local history interleaved with a merge is still published.
    let before = heads(&fixture);
    commit_local(&fixture, "unsent before merge");
    let incoming = fixture.remote(0, "interleaved-remote", true);
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(
        fixture.value(0, "interleaved-remote").as_deref(),
        Some("remote")
    );
    fixture.app.publish_owned_documents();
    let expected = fixture
        .records
        .read_checked(&id, |doc| doc.clone().save_after(&before))
        .unwrap();
    assert_eq!(
        published(&fixture),
        expected,
        "unsent local changes survive the merge into the next publication"
    );
}
#[test]
fn owned_route_frontier_moves_only_when_the_transport_accepts_the_change() {
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
        .to_owned();
    assert!(status.contains("no longer shared"), "{status}");
    fixture.app.publish_owned_documents();
    assert!(
        fixture.app.document_routes.sent.is_empty(),
        "still refused, and still not marked as sent"
    );

    fixture.app.document_routes.refuse_for_test = Some(crate::collaboration::CollabSendError::Busy);
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
#[test]
fn native_owned_remote_delivery_preserves_pending_edits_until_normal_boundary() {
    for incremental in [false, true] {
        let mut fixture = RoutedOwners::new();
        let incoming = fixture.remote(0, "remote-key", incremental);
        let checkout = &fixture.bindings[0].checkout;
        let before = fixture
            .records
            .read_checked(&checkout.id, |doc| doc.base_heads())
            .unwrap();
        fixture
            .records
            .with_action_state(&checkout.id, |doc, _, _| {
                doc.put(ROOT, "local-draft", "unfinished").unwrap();
            })
            .unwrap();
        fixture.app.apply_signed_document_envelope(&incoming, None);
        fixture
            .records
            .read_checked(&checkout.id, |doc| {
                assert_eq!(doc.base_heads(), before);
                assert!(doc.pending_ops() > 0);
            })
            .unwrap();
        assert_eq!(fixture.value(0, "remote-key"), None);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("current edit")
        );
        fixture
            .records
            .with_history(&checkout.id, |doc, history| {
                history.commit_action_authored(
                    doc,
                    None,
                    None,
                    Some(fixture.bindings[0].author.clone()),
                );
            })
            .unwrap();
        let undo_revision = fixture
            .records
            .read_history(&checkout.id, |_, history| history.revision())
            .unwrap();
        fixture.app.drain_document_routes();
        assert_eq!(fixture.value(0, "remote-key").as_deref(), Some("remote"));
        assert_eq!(
            fixture.value(0, "local-draft").as_deref(),
            Some("unfinished")
        );
        assert_eq!(
            fixture
                .records
                .read_history(&checkout.id, |_, history| history.revision())
                .unwrap(),
            undo_revision,
            "remote merge creates no local undo entry"
        );
    }
}
#[test]
fn native_owned_committed_offline_change_is_in_first_reconnected_snapshot() {
    use automerge::ReadDoc;
    let mut fixture = RoutedOwners::new();
    fixture.app.collaboration.manager = CollaborationManager::disabled();
    fixture.app.actions.bound_broker_route_for_test = None;
    fixture.app.refresh_document_routes();
    fixture
        .records
        .with_history(&fixture.bindings[0].checkout.id, |doc, history| {
            doc.put(ROOT, "offline", "restored locally").unwrap();
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(fixture.bindings[0].author.clone()),
            );
        })
        .unwrap();
    fixture.app.publish_owned_documents();
    assert!(fixture.app.document_routes.sent.is_empty());
    fixture.app.actions.bound_broker_route_for_test =
        Some(crate::collaboration::PrimaryRouteWitness {
            route: crate::collaboration::RouteSpec::app(
                "outer-shell",
                fixture.bindings[0].broker_route().unwrap().cluster_id,
            ),
            epoch: uuid::Uuid::new_v4(),
        });
    fixture.app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    // Resolve the broker's document mode on the new secondary route before
    // any document is eligible for publication. Production retries each frame.
    let deadline = crate::time::now() + crate::time::Duration::from_secs(2);
    loop {
        fixture.app.publish_owned_documents();
        if fixture
            .app
            .document_routes
            .sent
            .iter()
            .any(|(route, _)| Some(route) == fixture.bindings[0].broker_route().as_ref())
        {
            break;
        }
        assert!(
            crate::time::now() < deadline,
            "reconnected route did not become publishable"
        );
        std::thread::sleep(crate::time::Duration::from_millis(10));
    }
    let (_, request) = fixture
        .app
        .document_routes
        .sent
        .iter()
        .find(|(route, _)| Some(route) == fixture.bindings[0].broker_route().as_ref())
        .unwrap();
    let CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Snapshot(data),
        ..
    } = request
    else {
        panic!("reconnect needs fresh snapshot")
    };
    let doc = automerge::AutoCommit::load(data).unwrap();
    assert_eq!(
        doc.get(ROOT, "offline").unwrap().unwrap().0.as_str(),
        Some("restored locally")
    );
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
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
        },
        HistoryScopeDefinition {
            key: "second",
            title: "Second record",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
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
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
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
#[test]
fn native_owned_review_restore_enqueues_one_target_delta_and_preserves_other_owner_and_outer() {
    let mut fixture = RoutedOwners::new();
    let binding = &fixture.bindings[0];
    let old = fixture
        .records
        .with_history(&binding.checkout.id, |doc, history| {
            doc.put(ROOT, "body", "old version").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            let old = doc.base_heads()[0];
            doc.put(ROOT, "body", "live version").unwrap();
            history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
            old
        })
        .unwrap();
    fixture.app.publish_owned_documents();
    fixture.app.document_routes.sent.clear();
    fixture.install_history();
    let before = fixture
        .records
        .read_checked(&fixture.bindings[0].checkout.id, |doc| doc.base_heads())
        .unwrap();
    let other = fixture
        .records
        .read_checked(&fixture.bindings[1].checkout.id, |doc| doc.base_heads())
        .unwrap();
    let outer = fixture.app.state.doc.base_heads();
    let target = named(&mut fixture.app, "History");
    fixture
        .app
        .dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut fixture.app);
    select_native_version(&mut fixture.app, old);
    let target = named(&mut fixture.app, "Restore…");
    fixture
        .app
        .dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut fixture.app);
    native_key(&mut fixture.app, "Enter");
    assert_eq!(fixture.value(0, "body").as_deref(), Some("old version"));
    assert_eq!(fixture.app.state.doc.base_heads(), outer);
    assert_eq!(
        fixture
            .records
            .read_checked(&fixture.bindings[1].checkout.id, |doc| doc.base_heads())
            .unwrap(),
        other
    );
    assert_eq!(
        fixture
            .records
            .read_checked(&fixture.bindings[0].checkout.id, |doc| doc
                .clone()
                .get_changes(&before)
                .len())
            .unwrap(),
        1
    );
    assert_eq!(fixture.app.document_routes.sent.len(), 1);
    assert_eq!(
        fixture.app.document_routes.sent[0].0,
        fixture.bindings[0].broker_route().unwrap()
    );
    let CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Incremental(data),
        ..
    } = &fixture.app.document_routes.sent[0].1
    else {
        panic!("restore follows initial snapshot with exact-route delta")
    };
    assert!(!data.is_empty());
    // Compare exact bytes before/after an outbound traversal. Reading the
    // cursor through clones leaves the actual writer's cursor untouched.
    let cursor_before = fixture
        .records
        .read_checked(&fixture.bindings[0].checkout.id, |doc| {
            doc.clone().save_incremental()
        })
        .unwrap();
    fixture.app.publish_owned_documents();
    let cursor_after = fixture
        .records
        .read_checked(&fixture.bindings[0].checkout.id, |doc| {
            doc.clone().save_incremental()
        })
        .unwrap();
    assert_eq!(
        cursor_after, cursor_before,
        "publication cannot consume another writer's save cursor"
    );
    assert_eq!(fixture.app.document_routes.sent.len(), 1);
    // Restoring the same selected version again is a canonical no-op.
    let target = named(&mut fixture.app, "Restore…");
    fixture
        .app
        .dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut fixture.app);
    native_key(&mut fixture.app, "Enter");
    assert_eq!(
        fixture.app.document_routes.sent.len(),
        1,
        "no-op restore publishes nothing"
    );
}
#[test]
fn native_remote_queue_overflow_is_visible_and_recovers_only_with_fresh_snapshot() {
    let mut fixture = RoutedOwners::new();
    fixture.install_history();
    let incoming = fixture.remote(0, "remote-key", false);
    let checkout = &fixture.bindings[0].checkout;
    fixture
        .records
        .with_action_state(&checkout.id, |doc, _, _| {
            doc.put(ROOT, "draft", "pending").unwrap();
        })
        .unwrap();
    for _ in 0..65 {
        fixture.app.apply_signed_document_envelope(&incoming, None);
    }
    assert!(
        fixture
            .app
            .document_sync_status()
            .unwrap()
            .contains("queue is full")
    );
    layout(&mut fixture.app);
    assert!(
        fixture
            .app
            .build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node
                .label()
                .is_some_and(|label| label.contains("queue is full"))),
        "recovery is visible in shared History chrome"
    );
    fixture
        .records
        .with_history(&checkout.id, |doc, history| {
            history.commit_action_authored(
                doc,
                None,
                None,
                Some(fixture.bindings[0].author.clone()),
            );
        })
        .unwrap();
    fixture.app.drain_document_routes();
    assert_eq!(
        fixture.value(0, "remote-key").as_deref(),
        Some("remote"),
        "previously admitted entries were retained"
    );
    assert!(
        fixture
            .app
            .document_sync_status()
            .unwrap()
            .contains("queue is full")
    );
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(fixture.app.document_sync_status(), None);
}

#[test]
fn native_tiled_history_preserves_first_pending_and_routes_sibling_at_to_second_record() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().join("records"));
    let store = owners
        .open(
            crate::history_mode::integration_tests::RESTORABLE.app_id,
            "native-tile-history",
        )
        .unwrap();
    let first = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(701),
        crate::record::RecordKind::new("note"),
    );
    let second = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(702),
        crate::record::RecordKind::new("note"),
    );
    for checkout in [&first, &second] {
        store
            .reserve_record(checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let root = crate::history_mode::integration_tests::legacy_tiles_root(&store, &first, &second);
    let mut state = State::new();
    state.doc.put(ROOT, "outer", "stable").unwrap();
    state.commit_action(None, None);
    let mut app = App::new_with_controlled_inputs(state, root);
    layout(&mut app);
    let first_editor = node_for_test_id(&app, "first-editor");
    app.set_focused_node(Some(first_editor));
    store
        .with(&first.id, |doc| {
            doc.put(ROOT, "unfinished", "pending").unwrap()
        })
        .unwrap();
    let before = store
        .with(&first.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    app.state
        .doc
        .put(ROOT, "outer pending", "preserved")
        .unwrap();
    let outer = (app.state.doc.base_heads(), app.state.doc.pending_ops());
    let open = node_for_test_id(&app, "history-open");
    let rect = app.inspect_node(open, |widget| widget.get_rect()).unwrap();
    app.dispatch_semantic_click(rect.center().x, rect.center().y);
    layout(&mut app);
    assert!(
        app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.role() == accesskit::Role::List)
    );
    assert!(
        !app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("first-editor"))
    );
    let second_target = named(&mut app, "second-editor");
    app.dispatch_a11y_action(
        second_target,
        &crate::a11y::Action::SetValue("native second".into()),
    );
    layout(&mut app);
    assert_eq!(
        crate::history_mode::integration_tests::legacy_body(&store, &second),
        "native second"
    );
    assert_eq!(
        store
            .with(&first.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before
    );
    assert_eq!(
        (app.state.doc.base_heads(), app.state.doc.pending_ops()),
        outer
    );
}

#[test]
fn exported_legacy_at_id_cannot_edit_same_record_id_replacement_after_arena_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners_a = crate::record::RecordOwners::new(temp.path().join("a"));
    let mut owners_b = crate::record::RecordOwners::new(temp.path().join("b"));
    let a = owners_a
        .open(
            crate::history_mode::integration_tests::RESTORABLE.app_id,
            "same",
        )
        .unwrap();
    let b = owners_b
        .open(
            crate::history_mode::integration_tests::RESTORABLE.app_id,
            "same",
        )
        .unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(999),
        crate::record::RecordKind::new("note"),
    );
    for store in [&a, &b] {
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(Box::new(
            crate::history_mode::integration_tests::TileEditor(
                crate::widget::TextInput::from_document_string("body")
                    .with_accessible_name("replaceable editor"),
            ),
        )),
    ));
    tile.bind_record(a.clone(), checkout.clone());
    let tile_pointer = std::ptr::from_mut(tile.as_mut());
    let mut app = App::new_with_controlled_inputs(State::new(), tile);
    layout(&mut app);
    app.rebuild_structure();
    let exported = named(&mut app, "replaceable editor");
    let old_node = app
        .layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.inspect_node(*node, |widget| {
                widget
                    .accessibility()
                    .is_some_and(|node| node.name.as_deref() == Some("replaceable editor"))
            })
            .unwrap_or(false)
        })
        .unwrap();
    app.bindings.clear();
    app.layout.node_map.clear();
    app.focused_node = None;
    // App owns this stable boxed root exclusively, and raw descendant maps
    // were discarded before the supported owner rebind.
    unsafe {
        (*tile_pointer).bind_record(b.clone(), checkout.clone());
    }
    app.root_widget.sync(&mut app.state.doc, &ROOT);
    app.rebuild_structure();
    let new_node = app
        .layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.inspect_node(*node, |widget| {
                widget
                    .accessibility()
                    .is_some_and(|node| node.name.as_deref() == Some("replaceable editor"))
            })
            .unwrap_or(false)
        })
        .unwrap();
    assert_eq!(
        old_node, new_node,
        "full arena replacement reproduces the actual local layout id"
    );
    let before = b
        .with(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    app.dispatch_a11y_action(
        exported,
        &crate::a11y::Action::SetValue("stale edit".into()),
    );
    assert_eq!(
        b.with(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before,
        "an exported target from another owner must be rejected"
    );
    let fresh = named(&mut app, "replaceable editor");
    app.rebuild_structure();
    assert_eq!(
        named(&mut app, "replaceable editor"),
        fresh,
        "unchanged owner retains exported identity"
    );
    app.dispatch_a11y_action(fresh, &crate::a11y::Action::SetValue("fresh edit".into()));
    assert_ne!(
        b.with(&checkout.id, |doc| doc.base_heads()).unwrap(),
        before.0
    );
}
#[test]
fn taffy_same_arena_reinsert_has_generation_but_new_arena_reuses_initial_id() {
    let mut first = TaffyTree::<()>::new();
    let old = first.new_leaf(taffy::Style::default()).unwrap();
    first.remove(old).unwrap();
    assert_ne!(old, first.new_leaf(taffy::Style::default()).unwrap());
    let mut replacement = TaffyTree::<()>::new();
    assert_eq!(old, replacement.new_leaf(taffy::Style::default()).unwrap());
}

#[test]
fn native_hidden_resource_at_action_cannot_bypass_history_presentation() {
    let probe =
        crate::history_mode::isolation_tests::native_effect_probe_parts("live", "fixture.export");
    let starts = probe.starts;
    let mut app = app();
    app.root_widget = probe.root;
    layout(&mut app);
    let declared = app.collect_declared_resources();
    let violations = tilo_actions::validate_surface(
        &declared,
        &app.actions.action_registry,
        &app.root_widget.resource_operation_handlers(),
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

#[test]
fn round3_native_history_entry_pointer_captures_before_discard() {
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

#[test]
fn round3_native_history_focus_does_not_drain_live_controls() {
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

#[test]
fn native_tile_history_retains_own_queue_and_polls_admitted_hidden_effect() {
    native_tile_effect_isolation(false);
}
#[test]
fn native_tile_history_at_open_preserves_sibling_queue() {
    native_tile_effect_isolation(true);
}
fn native_tile_effect_isolation(semantic: bool) {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().to_path_buf());
    let store = owners
        .open(
            crate::history_mode::integration_tests::RESTORABLE.app_id,
            "effects",
        )
        .unwrap();
    let mut tiles = Vec::new();
    let mut probes = Vec::new();
    for (index, id, operation) in [
        (811, "first", "fixture.first"),
        (812, "second", "fixture.second"),
    ] {
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(index),
            crate::record::RecordKind::new("note"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
        let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(id, operation);
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(probe.root));
        tile.bind_record(store.clone(), checkout);
        if id == "first" {
            tile.register_document_history(&crate::history_mode::integration_tests::RESTORABLE)
                .unwrap();
        }
        probes.push((probe.emitter, probe.starts, probe.finish, probe.completions));
        tiles.push(tile);
    }
    let root = crate::widget::Row::new()
        .no_wrap()
        .push(Box::new(tiles.remove(0)))
        .push(Box::new(tiles.remove(0)));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    layout(&mut app);
    let actor = app.collaboration.local_author.0.clone();
    let address = tilo_actions::ActionAddress::new("first", "export");
    app.invoke_resource_action(ResourceInvocation::new(&address, &actor).confirmed(true))
        .unwrap();
    assert_eq!(probes[0].1.get(), 1);
    probes[0].0.borrow_mut().emit(()).unwrap();
    probes[1].0.borrow_mut().emit(()).unwrap();
    let open = node_for_test_id(&app, "history-open");
    let bounds = app.inspect_node(open, |widget| widget.get_rect()).unwrap();
    if semantic {
        let target = named(&mut app, "History");
        app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    } else {
        app.dispatch_semantic_click(bounds.center().x, bounds.center().y);
    }
    layout(&mut app);
    app.dispatch_control_invocations();
    assert_eq!(
        probes[0].1.get(),
        1,
        "A's retained local intent remains unadmitted"
    );
    assert_eq!(
        probes[1].1.get(),
        1,
        "B's queued live intent survives A's History input frame"
    );
    probes[0].0.borrow_mut().emit(()).unwrap();
    app.dispatch_control_invocations();
    assert_eq!(
        probes[0].1.get(),
        1,
        "output emitted while the embedded surface is hidden is discarded"
    );
    probes[0].2.set(true);
    app.poll_directed_invocations();
    assert_eq!(
        probes[0].3.get(),
        1,
        "A's already admitted effect still reaches terminal polling while hidden"
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert_eq!(probes[0].1.get(), 1, "return to live remains inert");
    let live_address = tilo_actions::ActionAddress::new("first", "export");
    assert!(
        matches!(
            crate::bound_dispatch::legacy_document::select_local_target(
                app.root_widget.as_ref(),
                &app.state.doc,
                &live_address,
            )
            .unwrap(),
            crate::bound_dispatch::legacy_document::LocalResourceTarget::Legacy(_)
        ),
        "returned owner must expose the exact retained action"
    );
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert!(
        app.layout.node_map.is_empty(),
        "continuation toolbar replacement must invalidate admitted raw maps first"
    );
    let continuation_labels = app
        .build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(
        probes[0].1.get(),
        2,
        "explicit embedded continuation enters once; visible tree: {continuation_labels:?}"
    );
}

#[test]
fn native_admitted_legacy_effect_refused_rebind_keeps_valid_old_owner_and_finishes_once() {
    check_admitted_legacy_effect_owner_retirement(false);
}

#[test]
fn native_admitted_legacy_effect_closed_owner_never_receives_completion() {
    check_admitted_legacy_effect_owner_retirement(true);
}

fn check_admitted_legacy_effect_owner_retirement(close: bool) {
    let temp = tempfile::tempdir().unwrap();
    let mut first_owners = crate::record::RecordOwners::new(temp.path().join("first"));
    let mut second_owners = crate::record::RecordOwners::new(temp.path().join("second"));
    let first = first_owners.open(HISTORY.app_id, "same-room").unwrap();
    let second = second_owners.open(HISTORY.app_id, "same-room").unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(910),
        crate::record::RecordKind::new("note"),
    );
    for store in [&first, &second] {
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "retained",
        "fixture.retained",
    );
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(probe.root),
    ));
    tile.bind_record(first.clone(), checkout.clone());
    let original_owner = tile.history_owner().unwrap();
    let candidate_before = second
        .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
        .unwrap();
    let pointer = std::ptr::from_mut(tile.as_mut());
    let mut app = App::new_with_controlled_inputs(State::new(), tile);
    layout(&mut app);
    let actor = app.collaboration.local_author.0.clone();
    app.invoke_resource_action(ResourceInvocation::new(&tilo_actions::ActionAddress::new("retained", "export"), &actor))
    .unwrap();
    assert_eq!(probe.starts.get(), 1);
    app.layout.node_map.clear();
    app.bindings.clear();
    // The retained allocation stays owned by App; no borrowed widget/map
    // references survive the supported close/rebind operation.
    unsafe {
        if close {
            (*pointer).close_components();
        } else {
            (*pointer).bind_record(second.clone(), checkout.clone());
        }
    }
    if close {
        assert!(
            original_owner.validate().is_err(),
            "actual close retires old custody"
        );
    } else {
        let retained = app.root_widget.history_owner().unwrap();
        retained.validate().unwrap();
        assert!(
            retained.same_owner(&original_owner),
            "default-refusing root keeps its valid old owner"
        );
        assert!(retained.store_checkout().0.same_owner(&first));
    }
    assert_eq!(
        second
            .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
            .unwrap(),
        candidate_before
    );
    probe.finish.set(true);
    assert!(app.poll_directed_invocations());
    assert_eq!(
        probe.completions.get(),
        usize::from(!close),
        "closed custody receives no callback; still-current old custody finishes once"
    );
    assert!(app.actions.local_invocations.is_empty());
    assert!(!app.poll_directed_invocations());
    assert_eq!(probe.starts.get(), 1);
    assert_eq!(probe.completions.get(), usize::from(!close));
    assert_eq!(
        second
            .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
            .unwrap(),
        candidate_before
    );
}

#[test]
fn native_tile_history_unmounts_live_native_view_and_remounts_on_return() {
    use crate::widget::native_view::{
        NativeView, NativeViewBackend, NativeViewGeometry, NativeViewHost,
    };
    use std::{cell::Cell, rc::Rc};
    struct Backend {
        mounts: Rc<Cell<usize>>,
        unmounts: Rc<Cell<usize>>,
    }
    impl NativeViewBackend for Backend {
        fn mount(&mut self, _: &NativeViewHost<'_>) -> Result<(), String> {
            self.mounts.set(self.mounts.get() + 1);
            Ok(())
        }
        fn unmount(&mut self) {
            self.unmounts.set(self.unmounts.get() + 1);
        }
        fn set_geometry(&mut self, _: NativeViewGeometry) {}
        fn set_visible(&mut self, _: bool) {}
        fn set_focus(&mut self, _: bool) {}
        fn set_enabled(&mut self, _: bool) {}
    }
    let mounts = Rc::new(Cell::new(0));
    let unmounts = Rc::new(Cell::new(0));
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().to_path_buf());
    let store = owners.open(HISTORY.app_id, "native-view").unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(911),
        crate::record::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "live").unwrap();
            Ok(())
        })
        .unwrap();
    let view = NativeView::new(
        "Live native surface",
        Backend {
            mounts: mounts.clone(),
            unmounts: unmounts.clone(),
        },
    );
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        crate::history_mode::integration_tests::TileEditor(view),
    )));
    tile.bind_record(store, checkout);
    tile.register_document_history(&HISTORY).unwrap();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(tile));
    layout(&mut app);
    let host = NativeViewHost::headless(
        crate::platform::PlatformConventions::new(crate::platform::Platform::MacOs),
        1.0,
    );
    crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
    assert_eq!(mounts.get(), 1);
    let target = named(&mut app, "History");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
    assert_eq!(unmounts.get(), 1);
    assert!(
        !app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Live native surface"))
    );
    let target = named(&mut app, "History");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    crate::widget::mount_native_views(app.root_widget.as_mut(), &host);
    assert_eq!(mounts.get(), 2);
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
    starts: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
    queries: std::sync::Mutex<Vec<String>>,
}

impl PaletteContinuationState {
    fn stable() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            authority: std::sync::atomic::AtomicU8::new(0),
            draft_epoch: std::sync::atomic::AtomicU64::new(1),
            target_epoch: std::sync::atomic::AtomicU64::new(1),
            handler_epoch: std::sync::atomic::AtomicU64::new(1),
            draft_supported: std::sync::atomic::AtomicBool::new(true),
            exact_supported: std::sync::atomic::AtomicBool::new(true),
            starts: std::sync::Mutex::new(Vec::new()),
            queries: std::sync::Mutex::new(Vec::new()),
        })
    }
}

struct PaletteHistoryOwner {
    text: crate::widget::Text,
    state: std::sync::Arc<PaletteContinuationState>,
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
        spec.contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase {
            phrase: PALETTE_CONFIRMATION.into(),
        };
        if self
            .state
            .authority
            .load(std::sync::atomic::Ordering::SeqCst)
            == 2
        {
            spec.contract.availability = tilo_actions::Availability::Unavailable {
                code: "fixture-unavailable".into(),
                reason: "Current action unavailable".into(),
            };
        }
        let draft_epoch = self
            .state
            .draft_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let target_epoch = self
            .state
            .target_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let handler_epoch = self
            .state
            .handler_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let draft_supported = self
            .state
            .draft_supported
            .load(std::sync::atomic::Ordering::SeqCst);
        let exact_supported = self
            .state
            .exact_supported
            .load(std::sync::atomic::Ordering::SeqCst);
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
    fn name(&self) -> &str {
        "Palette history owner"
    }
    fn test_id(&self) -> Option<&str> {
        Some("palette-history-owner")
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
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
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.text.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.text.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        if self
            .state
            .authority
            .load(std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            return vec![];
        }
        vec![
            tilo_actions::Resource::new("palette-owner", "fixture.palette", "Palette owner")
                .with_action(self.action("text", PALETTE_TEXT_OPERATION, false))
                .with_action(self.action("json", PALETTE_JSON_OPERATION, true))
                .with_action(self.action("named", PALETTE_TEXT_OPERATION, false)),
        ]
    }
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [PALETTE_TEXT_OPERATION.into(), PALETTE_JSON_OPERATION.into()]
            .into_iter()
            .collect()
    }
    fn start_resource_operation_with_document(
        &mut self,
        _: tilo_actions::InvocationId,
        _: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _: &mut automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if !self.resource_operation_handlers().contains(type_id) {
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

fn palette_history_app(state: std::sync::Arc<PaletteContinuationState>) -> App {
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
            .as_mut()
            .unwrap()
            .activate_address("palette-owner", action)
    );
    app.actions
        .action_palette
        .as_mut()
        .unwrap()
        .set_query(value);
}

fn palette_enter(app: &mut App) {
    let event = crate::synthetic_input::key_event(
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
        winit::event::ElementState::Pressed,
    );
    assert!(app.action_palette_handle_key(&event));
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

#[test]
fn managed_palette_draft_identity_preserves_text_and_json_editor_and_refuses_changed_or_absent_domain()
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
            state
                .draft_supported
                .store(false, std::sync::atomic::Ordering::SeqCst);
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
            state
                .draft_epoch
                .store(2, std::sync::atomic::Ordering::SeqCst);
        }
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(complete);
        palette_enter(&mut app);
        assert_eq!(editor_snapshot(&app).2, "confirmation");
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(PALETTE_CONFIRMATION);
        palette_enter(&mut app);

        let starts = state.starts.lock().unwrap().len();
        assert_eq!(starts, usize::from(should_start), "{action}:{mutation}");
        if should_start {
            assert!(
                app.action_palette().is_none(),
                "successful dispatch closes palette"
            );
        } else {
            let snapshot = editor_snapshot(&app);
            assert_eq!(snapshot.0, PALETTE_CONFIRMATION);
            assert_eq!(snapshot.2, "ready");
            assert!(
                app.actions
                    .action_palette
                    .as_mut()
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

#[test]
fn managed_palette_exact_text_and_nested_json_recheck_alias_and_handler_around_fresh_phrase() {
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
            "handler-after-wrong-phrase",
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
            .as_mut()
            .unwrap()
            .set_query(PALETTE_CONFIRMATION);

        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        if mutation == "target-before-return" {
            state
                .target_epoch
                .store(2, std::sync::atomic::Ordering::SeqCst);
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

        if mutation == "handler-after-wrong-phrase" {
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("wrong");
            palette_enter(&mut app);
            assert_eq!(state.starts.lock().unwrap().len(), 0);
            state
                .handler_epoch
                .store(2, std::sync::atomic::Ordering::SeqCst);
        }
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(PALETTE_CONFIRMATION);
        palette_enter(&mut app);
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

const COMPONENT_CONTROL_ACTION: &str = "run";

#[derive(Clone)]
struct ComponentControlHandle {
    identity_supported: std::rc::Rc<std::cell::Cell<bool>>,
    duplicate_depth: std::rc::Rc<std::cell::Cell<u8>>,
    binding: crate::bound_dispatch::CallbackBinding,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
    payloads: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
}

struct CustodyEnteredOperation {
    receiver: std::sync::mpsc::Receiver<()>,
    active: Option<tilo_actions::InvocationId>,
    completions: std::rc::Rc<std::cell::Cell<usize>>,
}

struct ComponentControlOwner {
    entered: Option<CustodyEnteredOperation>,
    emitter: Option<
        std::rc::Rc<
            std::cell::RefCell<
                crate::widget::ActionEmitter<serde_json::Map<String, serde_json::Value>>,
            >,
        >,
    >,
    identity_supported: std::rc::Rc<std::cell::Cell<bool>>,
    duplicate_depth: std::rc::Rc<std::cell::Cell<u8>>,
    id: String,
    operation: String,
    confirmation: tilo_actions::ConfirmationPolicy,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
    payloads: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    text: crate::widget::Text,
}

impl ComponentControlOwner {
    fn resource(&self) -> tilo_actions::Resource {
        let operation = self.operation.clone();
        let target = self.id.clone();
        let handler = operation.clone();
        let mut spec = tilo_actions::ActionSpec::typed_operation(
            COMPONENT_CONTROL_ACTION,
            "Run nested control",
            "Strict nonempty JSON",
            tilo_actions::OperationKind::Effect,
            operation.clone(),
            vec![
                tilo_actions::ActionParam::required(
                    "alias",
                    tilo_actions::ParamType::Text,
                    "Alias",
                ),
                tilo_actions::ActionParam::required(
                    "nested",
                    tilo_actions::ParamType::Object,
                    "Nested object",
                ),
            ],
            move |arguments| {
                tilo_actions::EncodedAction::encode_operation(&operation, &arguments.to_json())
                    .unwrap()
            },
        );
        spec.contract.confirmation = self.confirmation.clone();
        if self.entered.is_some() {
            spec = spec
                .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
                .returns(tilo_actions::ResultSpec::Job);
        }
        if !self.identity_supported.get() {
            return tilo_actions::Resource::new(&self.id, "fixture.component", &self.id)
                .with_action(spec);
        }
        tilo_actions::Resource::new(&self.id, "fixture.component", &self.id).with_action(
            spec.with_continuation_identity(move |query| match query {
                tilo_actions::ContinuationQuery::Exact(arguments) => {
                    Some(tilo_actions::ContinuationEvidence::Exact {
                        target: tilo_actions::ContinuationKey::new(
                            "ui.history.component.target",
                            [
                                target.as_bytes().to_vec(),
                                match arguments {
                                    tilo_actions::ContinuationArguments::Immediate => Vec::new(),
                                    tilo_actions::ContinuationArguments::Text(value) => {
                                        value.as_bytes().to_vec()
                                    }
                                    tilo_actions::ContinuationArguments::Json(value) => {
                                        serde_json::to_vec(value).unwrap()
                                    }
                                },
                            ],
                        ),
                        handler: tilo_actions::ContinuationKey::new(
                            "ui.history.component.handler",
                            [handler.as_bytes()],
                        ),
                    })
                }
                tilo_actions::ContinuationQuery::Draft => {
                    Some(tilo_actions::ContinuationEvidence::Draft {
                        resolution_domain: tilo_actions::ContinuationKey::new(
                            "ui.history.component.domain",
                            [target.as_bytes()],
                        ),
                        handler: tilo_actions::ContinuationKey::new(
                            "ui.history.component.handler",
                            [handler.as_bytes()],
                        ),
                    })
                }
            }),
        )
    }
}

impl crate::widget::AppWidget for ComponentControlOwner {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        &self.id
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("C")
    }
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        None
    }
}

impl crate::widget::Widget for ComponentControlOwner {
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.text.accessibility()
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.emitter
            .as_ref()
            .and_then(|emitter| emitter.borrow_mut().take())
    }
    fn name(&self) -> &str {
        &self.id
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
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
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.text.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.text.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources = vec![self.resource()];
        let empty = tilo_actions::Resource::new(&self.id, "fixture.component", "Empty duplicate");
        match self.duplicate_depth.get() {
            1 => resources.push(empty),
            2 => resources.push(
                tilo_actions::Resource::new("parent", "fixture.parent", "Parent").with_child(empty),
            ),
            _ => {}
        }
        resources
    }
    fn resource(&self, _: &automerge::AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        (id == self.id).then(|| self.resource())
    }
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [self.operation.clone()].into_iter().collect()
    }
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _: &mut automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if type_id != self.operation {
            return Ok(None);
        }
        self.starts.set(self.starts.get().saturating_add(1));
        self.payloads
            .borrow_mut()
            .push(serde_json::from_slice(payload).map_err(|error| error.to_string())?);
        if let Some(entered) = &mut self.entered {
            entered.active = Some(invocation);
            return Ok(Some(crate::widget::ResourceOperationStart::Accepted));
        }
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let entered = self.entered.as_mut()?;
        if entered.active != Some(invocation) || entered.receiver.try_recv().is_err() {
            return None;
        }
        entered.active = None;
        entered.completions.set(entered.completions.get() + 1);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

struct ComponentControlSlot {
    binding: crate::bound_dispatch::CallbackBinding,
    live: ComponentControlOwner,
    registry: tilo_actions::ActionRegistry,
    _scope: crate::WakeScope,
    _registry_owner: std::rc::Rc<()>,
}

impl crate::widget::Widget for ComponentControlSlot {
    fn name(&self) -> &str {
        "Controlled component boundary"
    }
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        Some(self.binding.clone())
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.live.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        self.live.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.live.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.live.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.live.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.live.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn children(&self) -> Vec<&dyn crate::widget::Widget> {
        vec![&self.live]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        vec![&mut self.live]
    }
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        if request
            .target
            .is_some_and(|target| !target.same_component(&self.binding))
        {
            return Ok(());
        }
        let _entry = self.binding.enter()?;
        let _ = (request.visit)(&self.binding, &mut self.live, &self.registry);
        request.found = true;
        Ok(())
    }
}

struct ComponentControlRoot {
    text: crate::widget::Text,
    slots: Vec<ComponentControlSlot>,
    fail_after_first: std::rc::Rc<std::cell::Cell<bool>>,
    _directory: tempfile::TempDir,
    _owners: crate::record::RecordOwners,
}

impl crate::widget::Widget for ComponentControlRoot {
    fn name(&self) -> &str {
        "Controlled component root"
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
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
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.text.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.text.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn crate::widget::Widget)) {
        for slot in &self.slots {
            visitor(slot);
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        self.slots
            .iter_mut()
            .map(|slot| slot as &mut dyn crate::widget::Widget)
            .collect()
    }
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if request
                .target
                .is_some_and(|target| !target.same_component(&slot.binding))
            {
                continue;
            }
            let _entry = slot.binding.enter()?;
            let _ = (request.visit)(&slot.binding, &mut slot.live, &slot.registry);
            request.found = true;
            if request.target.is_some() {
                return Ok(());
            }
            if index == 0 && self.fail_after_first.get() {
                return Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime);
            }
        }
        Ok(())
    }
}

fn component_control_root(
    specs: &[(&str, tilo_actions::ConfirmationPolicy)],
) -> (
    Box<ComponentControlRoot>,
    Vec<ComponentControlHandle>,
    std::rc::Rc<std::cell::Cell<bool>>,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let records = owners.open(HISTORY.app_id, "component-controls").unwrap();
    let failure = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut slots = Vec::new();
    let mut handles = Vec::new();
    for (index, (id, confirmation)) in specs.iter().enumerate() {
        let checkout = crate::record::RecordCheckout::new(
            crate::RecordId::for_tab(930 + index as u64),
            crate::record::RecordKind::new("component-control"),
        );
        records
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", *id).unwrap();
                Ok(())
            })
            .unwrap();
        let scope = crate::WakeScope::new();
        let registry_owner = std::rc::Rc::new(());
        let starts = std::rc::Rc::new(std::cell::Cell::new(0));
        let payloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let binding = crate::bound_dispatch::CallbackBinding {
            lifetime: scope.handle(),
            closing: std::rc::Rc::new(std::cell::Cell::new(false)),
            registry_owner: std::rc::Rc::downgrade(&registry_owner),
            lifetime_id: uuid::Uuid::new_v4(),
            control_domain: crate::control_custody::WeakControlDomain::default(),
            queued_controls: std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::VecDeque::new(),
            )),
            instance_id: 930 + index as u64,
            app_id: HISTORY.app_id.into(),
            records: records.clone(),
            checkout,
            permissions: std::rc::Rc::new(std::cell::RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            author: crate::permissions::UserId("component-control-author".into()),
            operation_context: tilo_actions::ActionContext::default(),
        };
        let identity_supported = std::rc::Rc::new(std::cell::Cell::new(true));
        let duplicate_depth = std::rc::Rc::new(std::cell::Cell::new(0));
        handles.push(ComponentControlHandle {
            identity_supported: identity_supported.clone(),
            duplicate_depth: duplicate_depth.clone(),
            binding: binding.clone(),
            starts: starts.clone(),
            payloads: payloads.clone(),
        });
        slots.push(ComponentControlSlot {
            binding,
            live: ComponentControlOwner {
                entered: None,
                emitter: None,
                identity_supported,
                duplicate_depth,
                id: (*id).into(),
                operation: format!("fixture.component.{id}"),
                confirmation: confirmation.clone(),
                starts,
                payloads,
                text: crate::widget::Text::new(*id),
            },
            registry: tilo_actions::ActionRegistry::new(),
            _scope: scope,
            _registry_owner: registry_owner,
        });
    }
    (
        Box::new(ComponentControlRoot {
            text: crate::widget::Text::new("Controlled component root"),
            slots,
            fail_after_first: failure.clone(),
            _directory: directory,
            _owners: owners,
        }),
        handles,
        failure,
    )
}

fn nested_control(resource: &str, alias: &str, leaf: i64) -> tilo_actions::ControlInvocation {
    tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new(resource, COMPONENT_CONTROL_ACTION),
        arguments: [
            ("alias".into(), serde_json::Value::String(alias.to_owned())),
            (
                "nested".into(),
                serde_json::json!({"route": {"leaf": leaf}}),
            ),
        ]
        .into_iter()
        .collect(),
    }
}

#[test]
fn managed_history_partial_component_capture_preserves_json_batches_without_double_capture() {
    let (root, handles, fail_after_first) = component_control_root(&[
        ("component-a", tilo_actions::ConfirmationPolicy::None),
        ("component-b", tilo_actions::ConfirmationPolicy::None),
    ]);
    handles[0]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("component-a", "first", 1));
    handles[1]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("component-b", "second", 2));
    fail_after_first.set(true);
    let mut app = app();
    app.root_widget = root;
    layout(&mut app);
    let resolved = crate::bound_dispatch::resolve_component_resource(
        app.root_widget.as_mut(),
        &app.state.doc,
        &handles[0].binding,
        "component-a",
        COMPONENT_CONTROL_ACTION,
    )
    .unwrap();
    assert!(
        resolved.is_some(),
        "controlled component resource must resolve"
    );

    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(app.pending_history_controls().len(), 1);
    assert!(
        format!("{:?}", app.build_a11y_tree_update(1.0).nodes)
            .contains("component capture refused")
    );
    assert!(handles[0].binding.queued_controls.borrow().is_empty());
    assert!(
        handles[1].binding.queued_controls.borrow().is_empty(),
        "the unvisited owner's original is sealed, with no live queued copy"
    );

    fail_after_first.set(false);
    layout(&mut app);
    assert_eq!(
        app.pending_history_controls().len(),
        2,
        "the next framework refresh resumes the interrupted capture"
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert_eq!(
        handles[0].starts.get(),
        0,
        "retained first batch stays inert"
    );
    assert_eq!(handles[1].starts.get(), 0, "retained sibling stays inert");

    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert_eq!(app.pending_history_controls().len(), 2, "no double capture");
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        handles[0].starts.get(),
        1,
        "first retained batch status: {:?}",
        app.pending_history_controls()
    );
    assert_eq!(handles[0].payloads.borrow()[0]["alias"], "first");
    assert_eq!(handles[1].starts.get(), 0);
    assert_eq!(app.pending_history_controls().len(), 1);
    layout(&mut app);
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(handles[1].starts.get(), 1);
    assert_eq!(handles[1].payloads.borrow()[0]["alias"], "second");
    assert!(app.pending_history_controls().is_empty());
}

#[test]
fn live_component_bridge_and_direct_take_first_emit_after_return_and_preview_remains_inert() {
    let (root, handles, _) =
        component_control_root(&[("component-live", tilo_actions::ConfirmationPolicy::None)]);
    let mut component_app = app();
    component_app.root_widget = root;
    layout(&mut component_app);
    palette_history_round_trip(&mut component_app);
    handles[0]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("component-live", "after-return", 3));
    component_app.dispatch_control_invocations();
    assert_eq!(handles[0].starts.get(), 1);
    assert!(component_app.pending_history_controls().is_empty());

    let named_action_target = named(&mut component_app, "History");
    component_app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut component_app);
    handles[0]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("component-live", "preview", 4));
    component_app.dispatch_control_invocations();
    assert_eq!(handles[0].starts.get(), 1, "preview bridge output is inert");

    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "direct-live",
        "fixture.direct-live",
    );
    let mut direct_app = app();
    direct_app.root_widget = probe.root;
    layout(&mut direct_app);
    palette_history_round_trip(&mut direct_app);
    probe.emitter.borrow_mut().emit(()).unwrap();
    direct_app.dispatch_control_invocations();
    assert_eq!(probe.starts.get(), 1);
    assert!(direct_app.pending_history_controls().is_empty());
    let named_action_target = named(&mut direct_app, "History");
    direct_app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut direct_app);
    probe.emitter.borrow_mut().emit(()).unwrap();
    direct_app.dispatch_control_invocations();
    assert_eq!(probe.starts.get(), 1, "preview direct-take output is inert");
}

fn emit_pending_control_command(
    app: &mut App,
    test_id: &str,
) -> crate::history_mode::pending_controls::ContinuationCommand {
    let node = node_for_test_id(app, test_id);
    app.dispatch_node(node, |widget, doc, path| {
        widget.perform_action(&crate::a11y::Action::Activate, doc, path)
    })
    .unwrap();
    let mut commands = app
        .history
        .as_ref()
        .unwrap()
        .drain_pending_control_commands();
    assert_eq!(commands.len(), 1, "actual framework callback emits once");
    commands.remove(0)
}

#[test]
fn delayed_framework_continue_confirm_and_cancel_callbacks_cannot_cross_reentry_or_replacement() {
    for command_kind in ["continue", "confirm", "cancel"] {
        let confirmation = if command_kind == "confirm" {
            tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "RUN COMPONENT".into(),
            }
        } else {
            tilo_actions::ConfirmationPolicy::None
        };
        let (root, original, _) =
            component_control_root(&[("component-replaced", confirmation.clone())]);
        original[0]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control("component-replaced", "original", 5));
        let mut app = app();
        app.root_widget = root;
        layout(&mut app);
        palette_history_round_trip(&mut app);

        let command = match command_kind {
            "continue" => emit_pending_control_command(&mut app, "history-pending-1"),
            "cancel" => emit_pending_control_command(&mut app, "history-pending-cancel-1"),
            "confirm" => {
                let named_action_target = named(&mut app, "Continue");
                app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
                layout(&mut app);
                let phrase = node_for_test_id(&app, "history-pending-1-phrase");
                app.dispatch_node(phrase, |widget, doc, path| {
                    widget.perform_action(
                        &crate::a11y::Action::SetValue("RUN COMPONENT".into()),
                        doc,
                        path,
                    )
                })
                .unwrap();
                emit_pending_control_command(&mut app, "history-pending-1")
            }
            _ => unreachable!(),
        };
        assert_eq!(original[0].starts.get(), 0);

        let (replacement, current, _) =
            component_control_root(&[("component-replaced", confirmation)]);
        app.root_widget = replacement;
        layout(&mut app);
        current[0]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control("component-replaced", "replacement", 6));
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        assert_eq!(
            app.pending_history_controls().len(),
            2,
            "replacement contributes one new same-address item"
        );

        app.history
            .as_ref()
            .unwrap()
            .queue_pending_control_command_for_test(command);
        app.service_pending_history_commands();
        assert_eq!(
            original[0].starts.get(),
            0,
            "delayed {command_kind} cannot reenter the retired owner"
        );
        assert_eq!(
            current[0].starts.get(),
            0,
            "delayed {command_kind} cannot affect the newer same-address owner"
        );
        assert_eq!(
            app.pending_history_controls().len(),
            2,
            "stale callback cannot consume either retained item"
        );
    }
}

#[test]
fn retained_framework_callback_objects_cannot_acquire_current_tokens_after_reentry_and_replacement()
{
    for callback_kind in ["continue", "confirm", "cancel"] {
        let confirmation = if callback_kind == "confirm" {
            tilo_actions::ConfirmationPolicy::Phrase {
                phrase: "RUN CALLBACK".into(),
            }
        } else {
            tilo_actions::ConfirmationPolicy::None
        };
        let (root, original, _) =
            component_control_root(&[("callback-owner", confirmation.clone())]);
        original[0]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control("callback-owner", "original", 7));
        let mut app = app();
        app.root_widget = root;
        layout(&mut app);
        palette_history_round_trip(&mut app);

        let callback_id = if callback_kind == "cancel" {
            "history-pending-cancel-1"
        } else {
            "history-pending-1"
        };
        if callback_kind == "confirm" {
            let named_action_target = named(&mut app, "Continue");
            app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
            layout(&mut app);
            let phrase = node_for_test_id(&app, "history-pending-1-phrase");
            app.dispatch_node(phrase, |widget, doc, path| {
                widget.perform_action(
                    &crate::a11y::Action::SetValue("RUN CALLBACK".into()),
                    doc,
                    path,
                )
            })
            .unwrap();
        }
        let callback = app
            .history
            .as_ref()
            .unwrap()
            .pending_control_callback_for_test(callback_id)
            .expect("the exact callback installed in the public framework toolbar");

        let (replacement, current, _) = component_control_root(&[("callback-owner", confirmation)]);
        app.root_widget = replacement;
        layout(&mut app);
        current[0]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control("callback-owner", "replacement", 8));
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        assert_eq!(app.pending_history_controls().len(), 2);

        callback.invoke_for_test();
        app.service_pending_history_commands();
        assert_eq!(original[0].starts.get(), 0);
        assert_eq!(current[0].starts.get(), 0);
        assert_eq!(
            app.pending_history_controls().len(),
            2,
            "old {callback_kind} callback cannot mint or consume a current token"
        );
    }
}

#[test]
fn live_legacy_callback_first_emits_after_return_and_preview_remains_inert() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners
        .open(HISTORY.app_id, "legacy-live-after-return")
        .unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(944),
        crate::record::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "live").unwrap();
            Ok(())
        })
        .unwrap();
    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "legacy-live",
        "fixture.legacy-live",
    );
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(probe.root));
    tile.bind_record(store, checkout);
    tile.register_document_history(&HISTORY).unwrap();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(tile));
    layout(&mut app);
    palette_history_round_trip(&mut app);

    probe.emitter.borrow_mut().emit(()).unwrap();
    app.dispatch_control_invocations();
    assert_eq!(probe.starts.get(), 1);
    assert!(
        !app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Continue")),
        "fresh legacy work uses its live door without stale continuation chrome"
    );

    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    probe.emitter.borrow_mut().emit(()).unwrap();
    app.dispatch_control_invocations();
    assert_eq!(probe.starts.get(), 1, "preview legacy output is inert");
}

#[test]
fn history_preview_keeps_session_notifications_on_the_live_owner_exactly_once() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners
        .open(HISTORY.app_id, "history-collaboration")
        .unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(945),
        crate::record::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "live").unwrap();
            Ok(())
        })
        .unwrap();
    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "history-live-session",
        "fixture.history-live-session",
    );
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(probe.root));
    tile.bind_record(store, checkout);
    tile.register_document_history(&HISTORY).unwrap();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(tile));
    layout(&mut app);
    let history = named(&mut app, "History");
    app.dispatch_a11y_action(history, &crate::a11y::Action::Activate);
    layout(&mut app);

    let visual_names = app
        .root_widget
        .children_mut()
        .into_iter()
        .map(|child| child.name().to_string())
        .collect::<Vec<_>>();
    assert!(
        !visual_names.iter().any(|name| name == "Live owner"),
        "active History presentation hides the live owner from visual traversal",
    );
    let session_names = app
        .root_widget
        .collaboration_children_mut()
        .into_iter()
        .map(|child| child.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(session_names, ["Live owner"]);

    app.apply_collab_event_for_test(CollabEvent::FollowChanged { peer_id: None });
    assert_eq!(
        probe.collab_events.get(),
        1,
        "History presentation must keep the live session owner notified once",
    );
}

#[test]
fn document_envelopes_never_enter_generic_widget_notification_before_custody() {
    let probe = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "observer",
        "fixture.observer",
    );
    let mut app = App::new_with_controlled_inputs(State::new(), probe.root);
    let foreign = crate::collaboration::RouteSpec::record("foreign", uuid::Uuid::new_v4(), "other");
    let mut doc = automerge::AutoCommit::new();
    doc.put(ROOT, "body", "foreign").unwrap();
    let envelope = routed_document(
        &foreign,
        collab_types::DocumentMutation::Snapshot(doc.save()),
    );
    app.apply_collab_event_for_test(CollabEvent::Document {
        envelope: Box::new(envelope.clone()),
        from_user_id: "peer".into(),
        decrypted_payload: None,
    });
    app.apply_collab_event_for_test(CollabEvent::ScrollbackBatch {
        artifact: envelope.artifact,
        ops: vec![Box::new(envelope)],
        decrypted_payloads: vec![],
        exhausted: true,
    });
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

#[test]
fn native_remote_waits_for_logical_owned_edit_and_drains_after_normal_undo_boundary() {
    let mut fixture = RoutedOwners::new();
    let incoming = fixture.remote(0, "remote-logical", true);
    let binding = fixture.bindings[0].clone();
    binding
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "logical-draft", "unfinished").unwrap();
            doc.commit(); // Physical persistence boundary, logical owner still open.
        })
        .unwrap();
    let physical = fixture
        .records
        .read_checked(&binding.checkout.id, |doc| {
            assert_eq!(doc.pending_ops(), 0);
            doc.base_heads()
        })
        .unwrap();
    fixture.app.apply_signed_document_envelope(&incoming, None);
    assert_eq!(fixture.value(0, "remote-logical"), None);
    assert_eq!(
        fixture
            .records
            .read_checked(&binding.checkout.id, |doc| doc.base_heads())
            .unwrap(),
        physical
    );
    assert!(
        binding
            .history_request(&mut fixture.app.state.doc, crate::undo::UndoRequest::Undo)
            .unwrap()
            .is_some()
    );
    let undo_revision = fixture
        .records
        .read_history(&binding.checkout.id, |_, history| history.revision())
        .unwrap();
    fixture.app.drain_document_routes();
    assert_eq!(
        fixture.value(0, "remote-logical").as_deref(),
        Some("remote")
    );
    assert_eq!(fixture.value(0, "logical-draft"), None);
    assert_eq!(
        fixture
            .records
            .read_history(&binding.checkout.id, |_, history| history.revision())
            .unwrap(),
        undo_revision,
        "network applies no new authored undo boundary"
    );
}

#[test]
fn native_route_epoch_change_retires_queued_bytes_before_later_drain() {
    let mut fixture = RoutedOwners::new();
    let incoming = fixture.remote(0, "old-epoch", false);
    let binding = fixture.bindings[0].clone();
    fixture
        .records
        .with_action_state(&binding.checkout.id, |doc, _, _| {
            doc.put(ROOT, "draft", "local").unwrap();
        })
        .unwrap();
    fixture.app.apply_signed_document_envelope(&incoming, None);
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
            .contains("connection or owner changed")
    );
    assert_eq!(fixture.value(0, "draft").as_deref(), Some("local"));
}

#[test]
fn native_scrollback_and_live_share_exact_primary_and_owned_route_admission() {
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
    fixture
        .app
        .apply_collab_event_for_test(CollabEvent::ScrollbackBatch {
            artifact: first.artifact,
            ops: vec![
                Box::new(first),
                Box::new(foreign_op),
                Box::new(second),
                Box::new(primary_op),
            ],
            decrypted_payloads: vec![],
            exhausted: true,
        });
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

#[test]
fn native_queued_remote_cannot_retarget_same_id_new_legacy_store() {
    use automerge::ReadDoc;
    let temp = tempfile::tempdir().unwrap();
    let room = "00000000-0000-0000-0000-000000000095";
    let mut owners_a = crate::record::RecordOwners::new(temp.path().join("a"));
    let mut owners_b = crate::record::RecordOwners::new(temp.path().join("b"));
    let first = owners_a.open(HISTORY.app_id, room).unwrap();
    let replacement = owners_b.open(HISTORY.app_id, room).unwrap();
    let checkout = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(930),
        crate::record::RecordKind::new("note"),
    );
    for store in [&first, &replacement] {
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let mut tile = Box::new(crate::widget::TileHost::new(
        crate::widget::TileContent::App(Box::new(
            crate::history_mode::integration_tests::TileEditor(crate::widget::Text::new("Live")),
        )),
    ));
    tile.bind_record(first.clone(), checkout.clone());
    let original_owner = tile.history_owner().unwrap();
    let old_heads = first
        .read_checked(&checkout.id, |doc| doc.base_heads())
        .unwrap();
    let pointer = std::ptr::from_mut(tile.as_mut());
    let mut app = App::new_with_controlled_inputs(State::new(), tile);
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::app("shell", uuid::Uuid::parse_str(room).unwrap()),
        epoch: uuid::Uuid::new_v4(),
    });
    app.refresh_document_routes();
    let route = app.document_routes.owners[0].route.clone();
    let mut peer = first.read_checked(&checkout.id, Clone::clone).unwrap();
    peer.put(ROOT, "remote", "old custody").unwrap();
    peer.commit();
    first
        .with_action_state(&checkout.id, |doc, _, _| {
            doc.put(ROOT, "draft", "pending").unwrap();
        })
        .unwrap();
    app.apply_signed_document_envelope(
        &routed_document(
            &route,
            collab_types::DocumentMutation::Snapshot(peer.save()),
        ),
        None,
    );
    assert!(!app.document_routes.owners[0].pending.is_empty());
    let before = replacement
        .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
        .unwrap();
    app.layout.node_map.clear();
    app.bindings.clear();
    unsafe {
        (*pointer).bind_record(replacement.clone(), checkout.clone());
    }
    let current = app.root_widget.history_owner().unwrap();
    current.validate().unwrap();
    assert!(!current.same_owner(&original_owner));
    assert!(current.store_checkout().0.same_owner(&replacement));
    app.drain_document_routes();
    assert_eq!(
        replacement
            .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
            .unwrap(),
        before
    );
    assert!(app.document_routes.owners.is_empty());
    assert!(
        app.document_sync_status()
            .unwrap()
            .contains("owner closed or changed")
    );
    first
        .read_checked(&checkout.id, |doc| {
            assert_eq!(
                doc.pending_ops(),
                0,
                "normal outgoing settlement commits the old draft"
            );
            assert_eq!(
                doc.get(ROOT, "draft").unwrap().unwrap().0.as_str(),
                Some("pending")
            );
            assert!(
                doc.get(ROOT, "remote").unwrap().is_none(),
                "queued remote bytes never merge into retired custody"
            );
            assert_ne!(doc.base_heads(), old_heads);
        })
        .unwrap();
    app.drain_document_routes();
    assert!(app.document_routes.owners.is_empty());
    assert_eq!(
        replacement
            .read_checked(&checkout.id, |doc| (doc.clone().save(), doc.base_heads()))
            .unwrap(),
        before
    );
}

#[test]
fn native_legacy_reported_bracket_and_directed_undo_stay_with_addressed_sibling() {
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().to_path_buf());
    let store = owners.open(HISTORY.app_id, "brackets").unwrap();
    let a = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(941),
        crate::record::RecordKind::new("note"),
    );
    let b = crate::record::RecordCheckout::new(
        crate::RecordId::for_tab(942),
        crate::record::RecordKind::new("note"),
    );
    for checkout in [&a, &b] {
        store
            .reserve_record(checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "old").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let probe_a = crate::history_mode::isolation_tests::native_effect_probe_parts("a", "fixture.a");
    let probe_b = crate::history_mode::isolation_tests::native_effect_probe_parts("b", "fixture.b");
    let mut tile_a = crate::widget::TileHost::new(crate::widget::TileContent::App(probe_a.root));
    tile_a.bind_record(store.clone(), a.clone());
    tile_a.register_document_history(&HISTORY).unwrap();
    let mut tile_b = crate::widget::TileHost::new(crate::widget::TileContent::App(probe_b.root));
    tile_b.bind_record(store.clone(), b.clone());
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(
            crate::widget::Row::new()
                .no_wrap()
                .push(Box::new(tile_a))
                .push(Box::new(tile_b)),
        ),
    );
    layout(&mut app);
    let target = named(&mut app, "History");
    app.dispatch_a11y_action(target, &crate::a11y::Action::Activate);
    layout(&mut app);
    store
        .with_action_state(&a.id, |doc, _, _| {
            doc.put(ROOT, "draft", "A unfinished").unwrap();
        })
        .unwrap();
    app.state
        .doc
        .put(ROOT, "outer-draft", "unfinished")
        .unwrap();
    let outer = (app.state.doc.base_heads(), app.state.doc.pending_ops());
    let first = store
        .read_checked(&a.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    let (before, revision) = store
        .read_history(&b.id, |doc, history| (doc.base_heads(), history.revision()))
        .unwrap();
    store
        .with_action_state(&b.id, |doc, _, _| {
            doc.put(ROOT, "body", "reported edit").unwrap();
            doc.commit();
        })
        .unwrap();
    *probe_b.bracket.borrow_mut() = Some(before);
    assert!(app.service_history());
    assert_eq!(
        store
            .read_history(&b.id, |_, history| history.revision())
            .unwrap(),
        revision + 1
    );
    assert!(probe_b.bracket.borrow().is_none());
    assert!(
        !app.service_history(),
        "the report is rising-edge, never duplicated"
    );
    probe_b
        .undo_request
        .set(Some(crate::undo::UndoRequest::Undo));
    assert!(
        app.service_history(),
        "directed sibling Undo ignores History focus in A"
    );
    assert_eq!(
        store
            .read_checked(&b.id, crate::state::owned_restore_tests::value)
            .unwrap(),
        "old"
    );
    assert_eq!(
        store
            .read_checked(&a.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        first
    );
    assert_eq!(
        (app.state.doc.base_heads(), app.state.doc.pending_ops()),
        outer
    );
}

#[test]
fn native_history_tab_shift_tab_release_and_ime_keep_pending_live_editor_isolated() {
    let mut app = app();
    app.root_widget =
        Box::new(crate::widget::TextInput::from_document_string("draft").with_id("pending-editor"));
    layout(&mut app);
    let editor = node_for_test_id(&app, "pending-editor");
    app.set_focused_node(Some(editor));
    app.dispatch_node(editor, |widget, doc, path| {
        widget.perform_action(&crate::a11y::Action::SetValue("draft".into()), doc, path)
    })
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

#[test]
fn native_routed_live_update_keeps_selected_historical_canvas_pinned() {
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
        .apply_collab_event_for_test(CollabEvent::Document {
            envelope: Box::new(routed_document(
                &binding.broker_route().unwrap(),
                collab_types::DocumentMutation::Snapshot(peer.save()),
            )),
            from_user_id: "peer".into(),
            decrypted_payload: None,
        });
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

#[test]
fn round1_top_level_native_history_preserves_legacy_custody_and_stale_at_refusal() {
    use crate::history_mode::integration_tests::{TopLegacyFixture, legacy_body};
    use crate::permissions::*;
    let mut fixture = TopLegacyFixture::new();
    let tile_pointer = std::ptr::from_mut(fixture.tile.as_mut());
    let mut app = App::from_managed_root(
        State::new(),
        ManagedRoot::new(fixture.tile, fixture.registration),
    )
    .unwrap();
    layout(&mut app);
    let allowed = app.permission_ctx().clone();
    assert!(
        !app.bindings
            .owns_document(node_for_test_id(&app, "history-open"))
    );
    app.state
        .doc
        .put(ROOT, "unfinished outer", "pending")
        .unwrap();
    let outer = (app.state.doc.base_heads(), app.state.doc.pending_ops());
    let before = fixture
        .store
        .read_history(&fixture.checkout.id, |_, history| history.revision())
        .unwrap();
    let target = named(&mut app, "owned editor");
    app.dispatch_a11y_action(target, &crate::a11y::Action::SetValue("owned edit".into()));
    assert_eq!(
        legacy_body(&fixture.store, &fixture.checkout),
        "owned edit",
        "top-level chrome must retain the actual legacy owner"
    );
    assert_eq!(
        (app.state.doc.base_heads(), app.state.doc.pending_ops()),
        outer
    );
    assert_eq!(
        fixture
            .store
            .read_history(&fixture.checkout.id, |_, history| history.revision())
            .unwrap(),
        before + 1
    );
    app.set_permission_ctx(PermissionContext::from_capabilities(
        Actor::person(None, LOCAL_USER),
        vec![],
    ));
    app.dispatch_a11y_action(target, &crate::a11y::Action::SetValue("denied".into()));
    assert_eq!(legacy_body(&fixture.store, &fixture.checkout), "owned edit");
    app.set_permission_ctx(allowed);
    app.layout.node_map.clear();
    app.bindings.clear();
    // Exclusive App retains the box; old maps cannot be dereferenced while rebinding.
    unsafe {
        (*tile_pointer).bind_record(fixture.replacement.clone(), fixture.checkout.clone());
    }
    app.rebuild_structure();
    layout(&mut app);
    assert!(
        !app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(id, _)| *id == target)
    );
    app.dispatch_a11y_action(target, &crate::a11y::Action::SetValue("stale".into()));
    assert_eq!(legacy_body(&fixture.replacement, &fixture.checkout), "live");
    let definition = &crate::history_mode::integration_tests::RESTORABLE;
    let owner = unsafe { (*tile_pointer).history_owner().unwrap() };
    app.install_history(
        HistoryRegistration::new(
            definition,
            vec![HistorySourceRegistration::owned("document", definition.app_id, owner).unwrap()],
        )
        .unwrap(),
    )
    .unwrap();
    layout(&mut app);
    let fresh = named(&mut app, "owned editor");
    assert_ne!(fresh, target);
    app.dispatch_a11y_action(
        target,
        &crate::a11y::Action::SetValue("stale allowed".into()),
    );
    assert_eq!(legacy_body(&fixture.replacement, &fixture.checkout), "live");
    let replacement_revision = fixture
        .replacement
        .read_history(&fixture.checkout.id, |_, history| history.revision())
        .unwrap();
    app.dispatch_a11y_action(
        fresh,
        &crate::a11y::Action::SetValue("replacement edit".into()),
    );
    assert_eq!(
        legacy_body(&fixture.replacement, &fixture.checkout),
        "replacement edit"
    );
    assert_eq!(legacy_body(&fixture.store, &fixture.checkout), "owned edit");
    assert_eq!(
        fixture
            .replacement
            .read_history(&fixture.checkout.id, |_, history| history.revision())
            .unwrap(),
        replacement_revision + 1
    );
    assert_eq!(
        (app.state.doc.base_heads(), app.state.doc.pending_ops()),
        outer
    );
}

#[test]
fn round1_top_level_wide_activity_detaches_focused_native_view_until_live_return() {
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

#[test]
fn round1_owned_malformed_remote_recovers_with_snapshot_after_local_edit_finishes() {
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
        fixture.app.apply_signed_document_envelope(&bad, None);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("could not be applied")
        );
        let failure = fixture.app.document_sync_status().map(str::to_owned);
        let invalid_recovery =
            routed_document(&route, round1_malformed_mutation(false, before.clone()));
        fixture
            .app
            .apply_signed_document_envelope(&invalid_recovery, None);
        assert_eq!(fixture.app.document_sync_status(), failure.as_deref());
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
        fixture.app.apply_signed_document_envelope(&recovery, None);
        assert_eq!(
            fixture.app.document_sync_status(),
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

#[test]
fn round1_primary_malformed_remote_recovers_with_snapshot_after_local_edit_finishes() {
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
        fixture.app.apply_signed_document_envelope(&bad, None);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("could not be applied")
        );
        let failure = fixture.app.document_sync_status().map(str::to_owned);
        let invalid_recovery = routed_document(
            &route,
            round1_malformed_mutation(false, fixture.app.state.doc.clone()),
        );
        fixture
            .app
            .apply_signed_document_envelope(&invalid_recovery, None);
        assert_eq!(fixture.app.document_sync_status(), failure.as_deref());
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
        fixture.app.apply_signed_document_envelope(&recovery, None);
        assert_eq!(
            fixture.app.document_sync_status(),
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

#[test]
fn round1_malformed_incremental_tail_never_partly_applies_valid_prefix() {
    let mut fixture = RoutedOwners::new();
    let valid = fixture.remote(0, "must-not-partly-apply", true);
    let collab_types::DocumentMutation::Incremental(mut bytes) =
        collab_types::DocumentMutation::decode(&valid.payload).unwrap()
    else {
        panic!("fixture delta");
    };
    bytes.extend_from_slice(b"invalid trailing chunk");
    assert!(automerge::AutoCommit::load(&bytes).is_err());
    let envelope = routed_document(
        &fixture.bindings[0].broker_route().unwrap(),
        collab_types::DocumentMutation::Incremental(bytes),
    );
    fixture.app.apply_signed_document_envelope(&envelope, None);
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

#[test]
fn round1_quarantining_bad_bytes_preserves_other_admitted_updates_and_recovers_in_order() {
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
        collab_types::DocumentMutation::decode(&delta.payload).unwrap()
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
        fixture.app.apply_signed_document_envelope(message, None);
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
    fixture.app.apply_signed_document_envelope(&bad, None);
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
    fixture.app.apply_signed_document_envelope(&s, None);
    assert!(
        fixture.app.document_sync_status().is_some(),
        "S must remain visibly pending while local editing continues"
    );
    fixture.app.apply_signed_document_envelope(&d, None);
    if overflow {
        // S plus D and62 duplicate D packets fill the bounded64-item queue.
        for _ in 0..62 {
            fixture.app.apply_signed_document_envelope(&d, None);
        }
        peer.put(ROOT, "refused-newer-change", "must recover later")
            .unwrap();
        peer.commit();
        let refused = routed_document(
            &route,
            collab_types::DocumentMutation::Snapshot(peer.save()),
        );
        fixture.app.apply_signed_document_envelope(&refused, None);
        assert!(
            fixture
                .app
                .document_sync_status()
                .unwrap()
                .contains("queue is full")
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
                .contains("queue is full"),
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
        fixture.app.apply_signed_document_envelope(&fresh, None);
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

#[test]
fn round2_owned_deferred_recovery_retains_later_delta_and_newer_snapshot() {
    for snapshot in [false, true] {
        round2_deferred_recovery_keeps_later_traffic(true, snapshot, false);
    }
}
#[test]
fn round2_primary_deferred_recovery_retains_later_delta_and_newer_snapshot() {
    for snapshot in [false, true] {
        round2_deferred_recovery_keeps_later_traffic(false, snapshot, false);
    }
}
#[test]
fn round2_deferred_recovery_overflow_requires_new_snapshot_on_both_routes() {
    for owned in [false, true] {
        round2_deferred_recovery_keeps_later_traffic(owned, false, true);
    }
}

#[test]
fn round4_palette_two_round_trips_preserve_original_exact_witness() {
    for changed in ["target", "handler", "unsupported"] {
        let state = PaletteContinuationState::stable();
        if changed == "unsupported" {
            state
                .exact_supported
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        let mut app = palette_history_app(state.clone());
        palette_activate(&mut app, "text", "production");
        palette_enter(&mut app);
        palette_history_round_trip(&mut app);
        let original =
            crate::action_palette::observed_history_pending(app.action_palette().unwrap()).unwrap();
        match changed {
            "target" => state
                .target_epoch
                .store(2, std::sync::atomic::Ordering::SeqCst),
            "handler" => state
                .handler_epoch
                .store(2, std::sync::atomic::Ordering::SeqCst),
            _ => state
                .exact_supported
                .store(true, std::sync::atomic::Ordering::SeqCst),
        }
        palette_history_round_trip(&mut app);
        let retained =
            crate::action_palette::observed_history_pending(app.action_palette().unwrap()).unwrap();
        assert_eq!(retained.wire_argument(), original.wire_argument());
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query(PALETTE_CONFIRMATION);
        palette_enter(&mut app);
        assert_eq!(state.starts.lock().unwrap().len(), 0, "{changed}");
        assert!(
            retained.history_witness == original.history_witness,
            "immutable witness: {changed}"
        );
        let refused =
            crate::action_palette::observed_history_pending(app.action_palette().unwrap()).unwrap();
        assert_eq!(refused.wire_argument().as_deref(), Some("production"));
        assert!(refused.history_witness == original.history_witness);
    }
}

#[test]
fn round4_palette_named_text_parameter_survives_history_and_confirmation() {
    let state = PaletteContinuationState::stable();
    let mut app = palette_history_app(state.clone());
    palette_activate(&mut app, "named", "  original target  ");
    palette_enter(&mut app);
    assert_eq!(editor_snapshot(&app).2, "confirmation");
    palette_history_round_trip(&mut app);
    app.actions
        .action_palette
        .as_mut()
        .unwrap()
        .set_query(PALETTE_CONFIRMATION);
    palette_enter(&mut app);
    let starts = state.starts.lock().unwrap();
    assert_eq!(starts.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&starts[0].1).unwrap(),
        serde_json::json!({"target":"  original target  "})
    );
    assert!(
        state
            .queries
            .lock()
            .unwrap()
            .iter()
            .any(|query| query == "exact:named:text")
    );
}

#[test]
fn round4_palette_missing_or_unavailable_draft_preserves_editor_and_reason() {
    for action in ["text", "json"] {
        for authority in [1, 2] {
            let state = PaletteContinuationState::stable();
            let mut app = palette_history_app(state.clone());
            let partial = if action == "text" {
                "unfinished alias"
            } else {
                r#"{"alias":"unfinished","nested":{"#
            };
            palette_activate(&mut app, action, partial);
            palette_history_round_trip(&mut app);
            let before = editor_snapshot(&app);
            state
                .authority
                .store(authority, std::sync::atomic::Ordering::SeqCst);
            palette_enter(&mut app);
            assert_eq!(editor_snapshot(&app), before, "{action}:{authority}");
            assert_eq!(state.starts.lock().unwrap().len(), 0);
            assert!(
                app.actions
                    .action_palette
                    .as_mut()
                    .unwrap()
                    .take_search_announcement()
                    .is_some_and(
                        |reason| reason.contains("unavailable") || reason.contains("removed")
                    )
            );
            palette_history_round_trip(&mut app);
            assert_eq!(editor_snapshot(&app), before);
        }
    }
}

fn activate_component_palette(
    app: &mut App,
    handle: &ComponentControlHandle,
    resource: &str,
    value: &str,
) {
    app.open_action_palette();
    let target = crate::bound_dispatch::BoundActionTarget {
        binding: handle.binding.clone(),
        address: crate::bound_dispatch::BoundActionAddress {
            instance_id: handle.binding.instance_id,
            address: tilo_actions::ActionAddress::new(resource, COMPONENT_CONTROL_ACTION),
        },
    };
    assert!(
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .activate_bound(&target)
    );
    app.actions
        .action_palette
        .as_mut()
        .unwrap()
        .set_query(value);
}

#[test]
fn round4_fresh_component_palette_does_not_require_history_identity_even_after_cancel() {
    let (root, handles, _) =
        component_control_root(&[("ordinary", tilo_actions::ConfirmationPolicy::None)]);
    handles[0].identity_supported.set(false);
    let mut app = app();
    app.root_widget = root;
    layout(&mut app);
    let input = r#"{"alias":"fresh","nested":{"leaf":1}}"#;
    activate_component_palette(&mut app, &handles[0], "ordinary", input);
    palette_enter(&mut app);
    assert_eq!(
        handles[0].starts.get(),
        1,
        "ordinary default ActionSpec executes without History opt-in"
    );
    activate_component_palette(&mut app, &handles[0], "ordinary", input);
    palette_history_round_trip(&mut app);
    let escape = crate::synthetic_input::key_event(
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
        winit::event::ElementState::Pressed,
    );
    assert!(app.action_palette_handle_key(&escape));
    activate_component_palette(&mut app, &handles[0], "ordinary", input);
    palette_enter(&mut app);
    assert_eq!(
        handles[0].starts.get(),
        2,
        "cancel clears retained status for the next fresh action"
    );
}

#[test]
fn round4_palette_reentry_cannot_replace_exact_component_owner() {
    let confirmation = tilo_actions::ConfirmationPolicy::Phrase {
        phrase: PALETTE_CONFIRMATION.into(),
    };
    let (root, old, _) = component_control_root(&[("same-address", confirmation.clone())]);
    let mut app = app();
    app.root_widget = root;
    layout(&mut app);
    let input = r#"{"alias":"original","nested":{"leaf":1}}"#;
    activate_component_palette(&mut app, &old[0], "same-address", input);
    palette_enter(&mut app);
    palette_history_round_trip(&mut app);
    let original =
        crate::action_palette::observed_history_pending(app.action_palette().unwrap()).unwrap();
    let (replacement, new, _) = component_control_root(&[("same-address", confirmation)]);
    app.layout.node_map.clear();
    app.bindings.clear();
    app.focused_node = None;
    app.root_widget = replacement;
    layout(&mut app);
    palette_history_round_trip(&mut app);
    app.actions
        .action_palette
        .as_mut()
        .unwrap()
        .set_query(PALETTE_CONFIRMATION);
    palette_enter(&mut app);
    assert_eq!(old[0].starts.get(), 0);
    assert_eq!(new[0].starts.get(), 0);
    let retained =
        crate::action_palette::observed_history_pending(app.action_palette().unwrap()).unwrap();
    assert_eq!(retained.wire_argument(), original.wire_argument());
    assert!(retained.history_witness == original.history_witness);
    let Some(crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(target)) =
        retained.history_target
    else {
        panic!("original component custody remains inspectable")
    };
    assert!(target.binding.same_component(&old[0].binding));
}

#[test]
fn round4_component_empty_duplicate_resource_refuses_saved_continuation() {
    for depth in [1, 2] {
        let (root, handles, _) =
            component_control_root(&[("duplicate", tilo_actions::ConfirmationPolicy::None)]);
        handles[0]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control("duplicate", "original", 1));
        let mut app = app();
        app.root_widget = root;
        layout(&mut app);
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        handles[0].duplicate_depth.set(depth);
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        let named_action_target = named(&mut app, "Continue");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        assert_eq!(handles[0].starts.get(), 0, "duplicate depth {depth}");
        assert!(
            matches!(&app.pending_history_controls()[0].status, crate::history_mode::pending_controls::PendingControlStatus::Refused(reason) if reason.contains("declaration"))
        );
        assert_eq!(
            app.pending_history_controls()[0].invocation,
            nested_control("duplicate", "original", 1)
        );
    }
}

#[test]
fn round4_partial_capture_seals_reachable_and_unvisited_queues_at_entry() {
    let (root, handles, failed) = component_control_root(&[
        ("sealed-a", tilo_actions::ConfirmationPolicy::None),
        ("sealed-b", tilo_actions::ConfirmationPolicy::None),
    ]);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        handles[i]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control(id, "original", i as i64));
    }
    failed.set(true);
    let mut app = app();
    app.root_widget = root;
    layout(&mut app);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(app.pending_history_controls().len(), 1);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        handles[i]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control(id, "post-entry", 10 + i as i64));
    }
    failed.set(false);
    layout(&mut app);
    assert_eq!(
        app.pending_history_controls().len(),
        2,
        "only the sealed pre-entry requests can recover"
    );
    assert!(
        app.pending_history_controls()
            .iter()
            .all(|item| item.invocation.arguments["alias"] == "original")
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    for i in 0..2 {
        let named_action_target = named(&mut app, "Continue");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        assert_eq!(handles[i].starts.get(), 1);
        assert_eq!(handles[i].payloads.borrow()[0]["alias"], "original");
        layout(&mut app);
    }
    app.dispatch_control_invocations();
    assert_eq!(handles[0].starts.get(), 1);
    assert_eq!(handles[1].starts.get(), 1);
    assert!(app.pending_history_controls().is_empty());
}

#[test]
fn round5_partial_capture_failed_through_return_never_automatically_dispatches_old_queues() {
    let (root, handles, failed) = component_control_root(&[
        ("sealed-a", tilo_actions::ConfirmationPolicy::None),
        ("sealed-b", tilo_actions::ConfirmationPolicy::None),
    ]);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        handles[i]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control(id, "original", i as i64));
    }
    failed.set(true);
    let mut app = app();
    app.root_widget = root;
    layout(&mut app);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(app.pending_history_controls().len(), 1);
    layout(&mut app);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        handles[i]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control(id, "suspended", 10 + i as i64));
    }
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(
        failed.get(),
        "custody remains unreachable throughout public return"
    );
    assert!(
        !app.framework_input.get(),
        "the framework input frame has ended"
    );
    assert!(!app.history.as_ref().unwrap().captures_input());
    let saved = app.pending_history_controls();
    assert_eq!(saved.len(), 2);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        assert_eq!(
            saved[i].invocation,
            nested_control(id, "original", i as i64)
        );
    }
    assert!(
        saved[1]
            .confirmation
            .as_ref()
            .unwrap_err()
            .contains("custody")
    );

    failed.set(false);
    app.dispatch_control_invocations();
    assert_eq!(handles[0].starts.get(), 0);
    assert_eq!(
        handles[1].starts.get(),
        0,
        "old copies and suspended output must never start without Continue"
    );
    assert_eq!(app.pending_history_controls(), saved);

    // A later History visit must not rebaseline the failed original or capture
    // an old queue copy. Both independent owners still accept genuinely fresh work.
    palette_history_round_trip(&mut app);
    assert_eq!(app.pending_history_controls(), saved);
    for (i, id) in ["sealed-a", "sealed-b"].iter().enumerate() {
        handles[i]
            .binding
            .queued_controls
            .borrow_mut()
            .push_back(nested_control(id, "fresh", 20 + i as i64));
    }
    app.dispatch_control_invocations();
    for handle in &handles {
        assert_eq!(handle.starts.get(), 1);
        assert_eq!(handle.payloads.borrow()[0]["alias"], "fresh");
    }
    assert_eq!(app.pending_history_controls(), saved);
}

#[test]
fn round5_driver_partial_capture_failed_through_return_discards_initially_empty_owner_queue() {
    let (root, handles, failed) = component_control_root(&[
        ("sealed-a", tilo_actions::ConfirmationPolicy::None),
        ("sealed-b", tilo_actions::ConfirmationPolicy::None),
    ]);
    handles[0]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("sealed-a", "original", 1));
    failed.set(true);
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "native-fixture",
                "lifetime-904",
                HISTORY.app_id,
                crate::RecordId::for_tab(904),
            ),
        }],
    )
    .unwrap();
    let mut state = State::new();
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let mut driver = crate::headless::Driver::from_managed_root(
        state,
        ManagedRoot::new(root, registration),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    assert_eq!(driver.pending_history_controls().len(), 1);
    handles[1]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("sealed-b", "suspended", 2));
    driver.click_named("history-open").unwrap();
    assert!(failed.get());
    let saved = driver.pending_history_controls();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].invocation,
        nested_control("sealed-a", "original", 1)
    );
    assert_eq!(
        saved[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Ready
    );
    failed.set(false);
    driver.send(&winit::event::WindowEvent::Focused(true));
    assert_eq!(handles[0].starts.get(), 0);
    assert_eq!(
        handles[1].starts.get(),
        0,
        "a queue empty at entry is still owned by suspension"
    );
    assert_eq!(driver.pending_history_controls(), saved);
    handles[1]
        .binding
        .queued_controls
        .borrow_mut()
        .push_back(nested_control("sealed-b", "fresh", 3));
    driver.send(&winit::event::WindowEvent::Focused(true));
    assert_eq!(handles[1].starts.get(), 1);
    assert_eq!(handles[1].payloads.borrow()[0]["alias"], "fresh");
    assert_eq!(driver.pending_history_controls(), saved);
}

#[test]
fn round4_actual_mounted_component_bridge_first_emits_after_return_and_discards_suspended_output() {
    use crate::application::{HostContext, LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        APP, author, provenance, runner_permissions, runner_probe_definition,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let records = owners.open(APP, "bridge-workspace").unwrap();
    let views = std::rc::Rc::new(
        crate::application_view::InstanceViewStore::new(
            directory.path().join("records"),
            "fixture-device",
            "bridge-workspace",
        )
        .unwrap(),
    );
    let definition = runner_probe_definition();
    let context = HostContext::prepare(
        definition,
        records,
        views,
        "fixture-device".into(),
        1971,
        runner_permissions(),
        author(),
        provenance(),
        &LaunchTarget::New,
    )
    .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let starts = std::rc::Rc::new(std::cell::Cell::new(0));
    let payloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut live = ComponentControlOwner {
        entered: None,
        emitter: None,
        identity_supported: std::rc::Rc::new(std::cell::Cell::new(true)),
        duplicate_depth: std::rc::Rc::new(std::cell::Cell::new(0)),
        id: "actual-bridge".into(),
        operation: "fixture.actual-bridge".into(),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        starts: starts.clone(),
        payloads: payloads.clone(),
        text: crate::widget::Text::new("Actual bridge"),
    };
    let resource = live.resource();
    let emitter = std::rc::Rc::new(std::cell::RefCell::new(crate::widget::ActionEmitter::new(
        tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |arguments| arguments)
            .unwrap(),
    )));
    live.emitter = Some(emitter.clone());
    component.root = Some(Box::new(live));
    component.mount(|_| {}).unwrap();
    let binding = component.callback_binding();
    static BRIDGE_HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: crate::application_test_support::APP,
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Bridge document",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
        }],
    };
    let registration = HistoryRegistration::new(
        &BRIDGE_HISTORY,
        vec![
            HistorySourceRegistration::owned(
                "document",
                APP,
                HistoryOwner::component(binding.clone()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let root = Box::new(
        crate::widget::Column::new().push(Box::new(crate::widget::BoundAppWidget { component })),
    );
    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(root, registration),
    )
    .unwrap();
    assert!(app.validate_history_registration().is_ok());
    layout(&mut app);
    // The callback retains only the ordinary live emitter across History.
    let emit_later = |alias: &str| {
        emitter
            .borrow_mut()
            .emit(nested_control("actual-bridge", alias, 7).arguments)
            .unwrap()
    };
    palette_history_round_trip(&mut app);
    assert!(app.validate_history_registration().is_ok());
    assert!(!app.history.as_ref().unwrap().captures_input());
    assert!(
        app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Actual bridge")),
        "public live content is exported after return"
    );
    assert!(!app.framework_input.get());
    let discovered = crate::bound_dispatch::discover_bound_resources(
        app.root_widget.as_ref(),
        &app.state.doc,
        None,
    )
    .unwrap();
    assert!(
        discovered
            .iter()
            .any(|snapshot| snapshot.binding.same_component(&binding)
                && tilo_actions::door::resolve(
                    &snapshot.resources,
                    &tilo_actions::ActionAddress::new("actual-bridge", COMPONENT_CONTROL_ACTION)
                )
                .is_some()),
        "mounted component exports current authorized action"
    );
    emit_later("after-return");
    assert!(binding.queued_controls.borrow().is_empty());
    assert!(
        app.root_widget.children_mut()[0]
            .take_control_invocation()
            .is_none()
    );
    assert_eq!(
        binding.queued_controls.borrow().len(),
        1,
        "production wrapper bridge transferred the root intent"
    );
    app.dispatch_control_invocations();
    assert_eq!(starts.get(), 1);
    assert_eq!(payloads.borrow()[0]["alias"], "after-return");
    assert!(app.pending_history_controls().is_empty());
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    emit_later("while-suspended");
    assert!(
        app.root_widget.children_mut()[0]
            .take_control_invocation()
            .is_none()
    );
    assert_eq!(binding.queued_controls.borrow().len(), 1);
    app.dispatch_control_invocations();
    assert!(binding.queued_controls.borrow().is_empty());
    assert_eq!(starts.get(), 1);
    assert!(app.pending_history_controls().is_empty());
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    app.dispatch_control_invocations();
    assert_eq!(starts.get(), 1);
    assert!(app.pending_history_controls().is_empty());
}

#[test]
fn round4_managed_constructor_rejects_invalid_owner_before_constructing_inputs() {
    let (_root, handles, _) =
        component_control_root(&[("invalid-owner", tilo_actions::ConfirmationPolicy::None)]);
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![
            HistorySourceRegistration::owned(
                "document",
                HISTORY.app_id,
                HistoryOwner::component(handles[0].binding.clone()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let called = std::cell::Cell::new(false);
    let result = App::from_managed_root_with_constructor(
        State::new(),
        ManagedRoot::new(
            Box::new(crate::widget::Text::new("No registered owner here")),
            registration,
        ),
        |state, widget| {
            called.set(true);
            App::new_with_controlled_inputs(state, widget)
        },
    );
    assert!(matches!(
        result,
        Err(HistoryRegistrationError::UnsupportedOwner)
    ));
    assert!(
        !called.get(),
        "invalid custody must refuse before input construction"
    );
}

#[test]
fn round4_controlled_managed_constructor_installs_real_history_composition() {
    let mut app = app();
    assert!(app.validate_history_registration().is_ok());
    layout(&mut app);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(app.history.as_ref().unwrap().captures_input());
    assert!(
        app.build_a11y_tree_update(1.0)
            .nodes
            .iter()
            .any(|(_, node)| node.role() == accesskit::Role::List)
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(!app.history.as_ref().unwrap().captures_input());
    assert!(app.validate_history_registration().is_ok());
}

struct CustodyDeniedRoot {
    child: Box<dyn crate::widget::Widget>,
    denied: std::rc::Rc<std::cell::Cell<bool>>,
    exit_callback: Option<Box<dyn FnMut()>>,
}
impl crate::widget::Widget for CustodyDeniedRoot {
    fn name(&self) -> &str {
        "Controlled component boundary"
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.child.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
        if let Some(callback) = &mut self.exit_callback {
            callback();
        }
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.child.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn children(&self) -> Vec<&dyn crate::widget::Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        if self.denied.get() {
            vec![]
        } else {
            vec![self.child.as_mut()]
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if self.denied.get() {
            None
        } else {
            self.child.take_control_invocation()
        }
    }
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        if self.denied.get() && request.target.is_none() {
            Err(crate::bound_dispatch::BoundDispatchError::CrossRecordReentry)
        } else {
            self.child.visit_components(request)
        }
    }
}

#[test]
fn control_custody_core_native_seals_and_discards_through_unreachable_private_routes() {
    use crate::application::{HostContext, LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        APP, author, provenance, runner_permissions, runner_probe_definition,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let records = owners.open(APP, "bridge-workspace").unwrap();
    let views = std::rc::Rc::new(
        crate::application_view::InstanceViewStore::new(
            directory.path().join("records"),
            "fixture-device",
            "bridge-workspace",
        )
        .unwrap(),
    );
    let definition = runner_probe_definition();
    let context = HostContext::prepare(
        definition,
        records,
        views,
        "fixture-device".into(),
        1971,
        runner_permissions(),
        author(),
        provenance(),
        &LaunchTarget::New,
    )
    .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let starts = std::rc::Rc::new(std::cell::Cell::new(0));
    let payloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut live = ComponentControlOwner {
        entered: None,
        emitter: None,
        identity_supported: std::rc::Rc::new(std::cell::Cell::new(true)),
        duplicate_depth: std::rc::Rc::new(std::cell::Cell::new(0)),
        id: "actual-bridge".into(),
        operation: "fixture.actual-bridge".into(),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        starts: starts.clone(),
        payloads: payloads.clone(),
        text: crate::widget::Text::new("Actual bridge"),
    };
    let resource = live.resource();
    let emitter = std::rc::Rc::new(std::cell::RefCell::new(
        crate::widget::ActionEmitter::new_in(
            tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |arguments| {
                arguments
            })
            .unwrap(),
            &component
                .callback_binding()
                .control_domain
                .upgrade()
                .unwrap(),
        ),
    ));
    live.emitter = Some(emitter.clone());
    component.root = Some(Box::new(live));
    component.mount(|_| {}).unwrap();
    let binding = component.callback_binding();
    static BRIDGE_HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: crate::application_test_support::APP,
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Bridge document",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
        }],
    };
    let registration = HistoryRegistration::new(
        &BRIDGE_HISTORY,
        vec![
            HistorySourceRegistration::owned(
                "document",
                APP,
                HistoryOwner::component(binding.clone()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let denied = std::rc::Rc::new(std::cell::Cell::new(false));
    let exit_armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let exit_calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let callback = {
        let armed = exit_armed.clone();
        let calls = exit_calls.clone();
        let emitter = emitter.clone();
        Box::new(move || {
            if armed.replace(false) {
                calls.set(calls.get() + 1);
                emitter
                    .borrow_mut()
                    .emit(nested_control("actual-bridge", "exit", 7).arguments)
                    .unwrap();
            }
        })
    };
    let root = Box::new(CustodyDeniedRoot {
        child: Box::new(
            crate::widget::Column::new()
                .push(Box::new(crate::widget::BoundAppWidget { component }))
                .push(Box::new(
                    crate::widget::Button::new("live-pulse", "Live pulse", None).on_click(|| {}),
                )),
        ),
        denied: denied.clone(),
        exit_callback: Some(callback),
    });
    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(root, registration),
    )
    .unwrap();
    assert!(app.validate_history_registration().is_ok());
    layout(&mut app);
    // The callback retains only the ordinary live emitter across History.
    let emit_later = |alias: &str| {
        emitter
            .borrow_mut()
            .emit(nested_control("actual-bridge", alias, 7).arguments)
            .unwrap()
    };
    emit_later("entry");
    denied.set(true);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    emit_later("hidden");
    exit_armed.set(true);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(!app.history.as_ref().unwrap().captures_input());
    assert_eq!(
        exit_calls.get(),
        1,
        "real live layout callback runs during return"
    );
    denied.set(false);
    let named_action_target = named(&mut app, "Live pulse");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        starts.get(),
        0,
        "hidden private output must never enter an ordinary action door after return"
    );
    assert_eq!(app.pending_history_controls().len(), 1);
    assert_eq!(
        app.pending_history_controls()[0].invocation,
        nested_control("actual-bridge", "entry", 7)
    );
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(starts.get(), 1);
    assert_eq!(payloads.borrow()[0]["alias"], "entry");
}

#[test]
fn control_custody_core_driver_seals_and_discards_through_unreachable_private_routes() {
    for entries in [0, 3] {
        use crate::application::{HostContext, LaunchTarget, RestoreInput};
        use crate::application_test_support::{
            APP, author, provenance, runner_permissions, runner_probe_definition,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
        let records = owners.open(APP, "bridge-workspace").unwrap();
        let views = std::rc::Rc::new(
            crate::application_view::InstanceViewStore::new(
                directory.path().join("records"),
                "fixture-device",
                "bridge-workspace",
            )
            .unwrap(),
        );
        let definition = runner_probe_definition();
        let context = HostContext::prepare(
            definition,
            records,
            views,
            "fixture-device".into(),
            1971,
            runner_permissions(),
            author(),
            provenance(),
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let starts = std::rc::Rc::new(std::cell::Cell::new(0));
        let payloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut live = ComponentControlOwner {
            entered: None,
            emitter: None,
            identity_supported: std::rc::Rc::new(std::cell::Cell::new(true)),
            duplicate_depth: std::rc::Rc::new(std::cell::Cell::new(0)),
            id: "actual-bridge".into(),
            operation: "fixture.actual-bridge".into(),
            confirmation: tilo_actions::ConfirmationPolicy::None,
            starts: starts.clone(),
            payloads: payloads.clone(),
            text: crate::widget::Text::new("Actual bridge"),
        };
        let resource = live.resource();
        let emitter = std::rc::Rc::new(std::cell::RefCell::new(
            crate::widget::ActionEmitter::new_in(
                tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |arguments| {
                    arguments
                })
                .unwrap(),
                &component
                    .callback_binding()
                    .control_domain
                    .upgrade()
                    .unwrap(),
            ),
        ));
        live.emitter = Some(emitter.clone());
        component.root = Some(Box::new(live));
        component.mount(|_| {}).unwrap();
        let binding = component.callback_binding();
        static BRIDGE_HISTORY: HistoryDefinition = HistoryDefinition {
            app_id: crate::application_test_support::APP,
            scopes: &[HistoryScopeDefinition {
                key: "document",
                title: "Bridge document",
                presentation: HistoryPresentation::Domain {
                    create: crate::history_mode::test_preview,
                },
                content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
            }],
        };
        let registration = HistoryRegistration::new(
            &BRIDGE_HISTORY,
            vec![
                HistorySourceRegistration::owned(
                    "document",
                    APP,
                    HistoryOwner::component(binding.clone()),
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let denied = std::rc::Rc::new(std::cell::Cell::new(false));
        let exit_armed = std::rc::Rc::new(std::cell::Cell::new(false));
        let exit_calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let callback = {
            let armed = exit_armed.clone();
            let calls = exit_calls.clone();
            let emitter = emitter.clone();
            Box::new(move || {
                if armed.replace(false) {
                    calls.set(calls.get() + 1);
                    emitter
                        .borrow_mut()
                        .emit(nested_control("actual-bridge", "exit", 7).arguments)
                        .unwrap();
                }
            })
        };
        let root = Box::new(CustodyDeniedRoot {
            child: Box::new(
                crate::widget::Column::new()
                    .push(Box::new(crate::widget::BoundAppWidget { component }))
                    .push(Box::new(
                        crate::widget::Button::new("live-pulse", "Live pulse", None)
                            .on_click(|| {}),
                    )),
            ),
            denied: denied.clone(),
            exit_callback: Some(callback),
        });
        let mut driver = crate::headless::Driver::from_managed_root_with_controlled_inputs(
            State::new(),
            ManagedRoot::new(root, registration),
            (900, 650),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            author(),
        )
        .unwrap();
        for i in 0..entries {
            emitter
                .borrow_mut()
                .emit(nested_control("actual-bridge", "entry", i).arguments)
                .unwrap();
        }
        denied.set(true);
        driver
            .perform_a11y_action(
                driver.a11y_node_named("History").unwrap(),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        emitter
            .borrow_mut()
            .emit(nested_control("actual-bridge", "hidden", 11).arguments)
            .unwrap();
        exit_armed.set(true);
        driver
            .perform_a11y_action(
                driver.a11y_node_named("History").unwrap(),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        assert!(denied.get());
        assert_eq!(exit_calls.get(), 1, "Driver exit layout callback");
        let saved = driver.pending_history_controls();
        assert_eq!(saved.len(), entries as usize);
        denied.set(false);
        driver.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(starts.get(), 0);
        for (i, item) in saved.iter().enumerate() {
            assert_eq!(
                item.invocation,
                nested_control("actual-bridge", "entry", i as i64)
            );
        }
        emitter
            .borrow_mut()
            .emit(nested_control("actual-bridge", "fresh", 12).arguments)
            .unwrap();
        driver.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(starts.get(), 1);
        assert_eq!(payloads.borrow()[0]["alias"], "fresh");
        assert_eq!(driver.pending_history_controls(), saved);
    }
}

struct CustodyControlsRoot {
    draw_callback: Option<Box<dyn FnMut()>>,
    overlay_callback: Option<Box<dyn FnMut()>>,
    mark_drawn_callback: Option<Box<dyn FnMut()>>,
    unsettled_layout: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    installed_output: Option<std::rc::Rc<std::cell::RefCell<Option<crate::ControlOutbox>>>>,
    record: Option<std::rc::Rc<std::cell::RefCell<Option<crate::RecordCheckout>>>>,
    child: Box<dyn crate::widget::Widget>,
    resources: Vec<tilo_actions::Resource>,
    ledger: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
}
impl crate::widget::Widget for CustodyControlsRoot {
    fn take_structure_dirty(&mut self) -> bool {
        self.child.take_structure_dirty()
            | self
                .unsettled_layout
                .as_ref()
                .is_some_and(|flag| flag.get())
    }
    fn mark_drawn(&mut self) {
        self.child.mark_drawn();
        if let Some(callback) = &mut self.mark_drawn_callback {
            callback();
        }
    }
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.child.install_control_custody(domain, attachment);
        if let Some(output) = &self.installed_output {
            if output.borrow().is_none() {
                *output.borrow_mut() =
                    Some(domain.producer(crate::ControlProducerDiscipline::Fifo { limit: 64 }));
            }
        }
    }
    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.child
            .draw_overlay(scene, transform, text, theme, presence);
        if let Some(callback) = &mut self.overlay_callback {
            callback();
        }
    }
    // This is the view hosted by TileHost; its requested checkout is owned by
    // that enclosing boundary, not a second independent document owner.
    fn has_separate_document_owner(&self) -> bool {
        false
    }
    fn record_intent(&self) -> Option<crate::RecordCheckout> {
        self.record
            .as_ref()
            .and_then(|record| record.borrow().clone())
    }
    fn name(&self) -> &str {
        "Controlled component boundary"
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.child.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
        if let Some(callback) = &mut self.draw_callback {
            callback();
        }
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.child.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            handled,
            focus,
            node,
            tree,
        );
    }
    fn children(&self) -> Vec<&dyn crate::widget::Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        self.resources.clone()
    }
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers = self.child.resource_operation_handlers();
        handlers.insert("fixture.control".into());
        handlers
    }
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        operation: &str,
        payload: &[u8],
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if operation != "fixture.control" {
            return self.child.start_resource_operation_with_document(
                invocation, kind, operation, payload, doc, path,
            );
        }
        self.ledger
            .borrow_mut()
            .push(serde_json::from_slice(payload).unwrap());
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

fn custody_matrix_spec(kind: &str) -> tilo_actions::Resource {
    use tilo_actions::*;
    let parameter = match kind {
        "Checkbox" | "Toggle" => Some(ParamType::Boolean),
        "Slider" | "NumberInput" => Some(ParamType::Number),
        "Dropdown" | "TextInput" => Some(ParamType::Text),
        "ToggleGroup" => Some(ParamType::Integer),
        _ => None,
    };
    let spec = if kind == "CommandPalette" {
        ActionSpec::prompted_operation(
            "run",
            "Run",
            "Value",
            "Value",
            OperationKind::Effect,
            "fixture.control",
            |value| {
                EncodedAction::encode_operation(
                    "fixture.control",
                    &serde_json::json!({"value":value}),
                )
                .unwrap()
            },
        )
    } else {
        ActionSpec::typed_operation(
            "run",
            "Run",
            "Run",
            OperationKind::Effect,
            "fixture.control",
            parameter
                .into_iter()
                .map(|ty| ActionParam::required("value", ty, "Value"))
                .collect(),
            |args| EncodedAction::encode_operation("fixture.control", &args.to_json()).unwrap(),
        )
    }
    .with_continuation_identity(|query| match query {
        ContinuationQuery::Exact(arguments) => Some(ContinuationEvidence::Exact {
            target: ContinuationKey::new(
                "fixture.control.target",
                [match arguments {
                    ContinuationArguments::Immediate => Vec::new(),
                    ContinuationArguments::Text(value) => value.as_bytes().to_vec(),
                    ContinuationArguments::Json(value) => serde_json::to_vec(value).unwrap(),
                }],
            ),
            handler: ContinuationKey::new("fixture.control.handler", [b"v1"]),
        }),
        _ => None,
    });
    Resource::new("fixture:control", "fixture", "Fixture").with_action(spec)
}
fn custody_matrix_binding<I: serde::Serialize + 'static>(
    resource: &tilo_actions::Resource,
) -> tilo_actions::ActionBinding<I> {
    tilo_actions::ActionBinding::new(resource, &resource.actions[0], |value| {
        serde_json::json!({"value":value})
            .as_object()
            .unwrap()
            .clone()
    })
    .unwrap()
}
fn custody_matrix_app(
    kind: &str,
) -> (App, std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>) {
    custody_matrix_app_with_confirmation(kind, tilo_actions::ConfirmationPolicy::None)
}
fn custody_matrix_app_with_confirmation(
    kind: &str,
    confirmation: tilo_actions::ConfirmationPolicy,
) -> (App, std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>) {
    use crate::widget::command_palette::{CommandEntry, CommandPalette};
    use crate::widget::toggle_group::Segment;
    use crate::widget::*;
    let mut resource = custody_matrix_spec(kind);
    resource.actions[0].contract.confirmation = confirmation;
    let unit = || {
        tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |()| {
            serde_json::Map::new()
        })
        .unwrap()
    };
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let control: Box<dyn Widget> = match kind {
        "Button" => Box::new(Button::new("control", "Control", None).on_action(unit())),
        "Checkbox" => Box::new(
            Checkbox::new("control", "unused")
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "Toggle" => Box::new(
            Toggle::new("control", "unused")
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "Slider" => Box::new(
            Slider::new("control", "unused")
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "NumberInput" => Box::new(
            NumberInput::new(0.0)
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "Dropdown" => Box::new(
            Dropdown::new(vec!["Alpha".into(), "Beta".into()], "unused")
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "TextInput" => Box::new(
            TextInput::local_with_language("en-US")
                .with_spellcheck(false)
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "ToggleGroup" => Box::new(
            ToggleGroup::new(vec![Segment::new("Zero"), Segment::new("One")], 0)
                .with_id("control")
                .on_action(custody_matrix_binding(&resource)),
        ),
        "ToggleGroupView" => {
            let ledger = ledger.clone();
            Box::new(
                ToggleGroup::new(vec![Segment::new("Zero"), Segment::new("One")], 0)
                    .on_view("control", move |index| {
                        ledger.borrow_mut().push(serde_json::json!({"value":index}))
                    }),
            )
        }
        "EmptyState" => Box::new(
            EmptyState::new("Empty")
                .with_action("Control")
                .with_action_id("control")
                .on_action(unit()),
        ),
        "CommandPalette" => {
            let mut palette = CommandPalette::commands();
            let flat = tilo_actions::flatten(std::slice::from_ref(&resource));
            palette.set_entries(vec![
                CommandEntry::from_flat_action(&flat[0]).with_argument_prefill("Prompted value"),
            ]);
            palette.open();
            Box::new(palette)
        }
        _ => panic!("unknown matrix control"),
    };
    let resources = if kind == "ToggleGroupView" {
        vec![]
    } else {
        vec![resource]
    };
    let root = Box::new(CustodyControlsRoot {
        draw_callback: None,
        overlay_callback: None,
        mark_drawn_callback: None,
        unsettled_layout: None,
        installed_output: None,
        record: None,
        child: Box::new(Column::new().push(control).push(Box::new(
            Button::new("pulse", "Live pulse", None).on_click(|| {}),
        ))),
        resources,
        ledger: ledger.clone(),
    });
    let registration = HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "matrix",
                HISTORY.app_id,
                crate::RecordId::for_tab(901),
            ),
        }],
    )
    .unwrap();
    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(root, registration),
    )
    .unwrap();
    layout(&mut app);
    assert_eq!(app.control_coverage().registered, 1, "{kind}");
    (app, ledger)
}
fn custody_matrix_activate(app: &mut App, kind: &str, second: bool) -> serde_json::Value {
    use crate::a11y::Action;
    let node = app
        .layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.inspect_node(*node, |widget| match kind {
                "EmptyState" => widget.name() == "EmptyState",
                "CommandPalette" => widget.test_id() == Some("command_palette_input"),
                _ => widget.test_id() == Some("control"),
            })
            .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("mounted real control {kind}"));
    if kind == "CommandPalette" {
        app.mutate_node(node, |widget| widget.set_focus(true))
            .unwrap();
        let event = crate::synthetic_input::key_event(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
            ElementState::Pressed,
        );
        for _ in 0..2 {
            let mut handled = false;
            let mut focus = None;
            app.dispatch_node(node, |widget, doc, path| {
                let mut tree = taffy::TaffyTree::new();
                let leaf =
                    crate::widget::TaffyTreeExt::leaf_node(&mut tree, taffy::Style::default());
                widget.handle_event(
                    &event,
                    1.0,
                    &winit::event::Modifiers::default(),
                    doc,
                    path,
                    &mut handled,
                    &mut focus,
                    leaf,
                    &tree,
                );
            })
            .unwrap();
            assert!(handled);
        }
        return serde_json::json!({"value":"Prompted value"});
    }
    let (action, expected) = match kind {
        "Checkbox" | "Toggle" => (Action::ToggleCheck, serde_json::json!({"value":true})),
        "Slider" => (
            Action::SetValue(if second { "0.75" } else { "0.25" }.into()),
            serde_json::json!({"value":if second {0.75} else {0.25}}),
        ),
        "NumberInput" => (
            Action::SetValue(if second { "43" } else { "42" }.into()),
            serde_json::json!({"value":if second {43.0} else {42.0}}),
        ),
        "Dropdown" => (
            Action::SetValue(if second { "Beta" } else { "Alpha" }.into()),
            serde_json::json!({"value":if second {"Beta"} else {"Alpha"}}),
        ),
        "TextInput" => (
            Action::SetValue(if second { "Second text" } else { "First text" }.into()),
            serde_json::json!({"value":if second {"Second text"} else {"First text"}}),
        ),
        "ToggleGroup" | "ToggleGroupView" => (
            Action::Activate,
            serde_json::json!({"value":if kind=="ToggleGroupView" && second {0} else {1}}),
        ),
        _ => (Action::Activate, serde_json::json!({})),
    };
    assert!(
        app.dispatch_node(node, |widget, doc, path| {
            if kind.starts_with("ToggleGroup") {
                widget.perform_semantic_action(
                    if kind == "ToggleGroupView" && second {
                        "control:segment:0"
                    } else {
                        "control:segment:1"
                    },
                    &action,
                    doc,
                    path,
                )
            } else {
                widget.perform_action(&action, doc, path)
            }
        })
        .unwrap(),
        "{kind}"
    );
    expected
}
const CUSTODY_MATRIX: &[&str] = &[
    "Button",
    "Checkbox",
    "Toggle",
    "Slider",
    "NumberInput",
    "Dropdown",
    "TextInput",
    "ToggleGroup",
    "ToggleGroupView",
    "EmptyState",
    "CommandPalette",
];
#[test]
fn control_custody_core_all_shared_controls_keep_ordinary_live_behavior() {
    for kind in CUSTODY_MATRIX {
        let (mut app, ledger) = custody_matrix_app(kind);
        let first = custody_matrix_activate(&mut app, kind, false);
        let named_action_target = named(&mut app, "Live pulse");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        assert_eq!(&*ledger.borrow(), &[first], "before History: {kind}");
        palette_history_round_trip(&mut app);
        let second = custody_matrix_activate(&mut app, kind, true);
        let named_action_target = named(&mut app, "Live pulse");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        assert_eq!(ledger.borrow().len(), 2, "after return: {kind}");
        assert_eq!(ledger.borrow()[1], second, "fresh typed value: {kind}");
        assert!(app.pending_history_controls().is_empty());
    }
}
#[test]
fn control_custody_core_shared_controls_seal_exact_pending_values() {
    for kind in CUSTODY_MATRIX {
        let (mut app, ledger) = custody_matrix_app(kind);
        let expected = custody_matrix_activate(&mut app, kind, false);
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        let saved = app.pending_history_controls();
        assert_eq!(saved.len(), 1, "{kind}");
        if *kind == "ToggleGroupView" {
            assert_eq!(saved[0].invocation.arguments["index"], 1);
        } else {
            assert_eq!(
                serde_json::Value::Object(saved[0].invocation.arguments.clone()),
                expected,
                "{kind}"
            );
        }
        assert!(saved[0].owner.starts_with("outer root "));
        let named_action_target = named(&mut app, "History");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        layout(&mut app);
        let named_action_target = named(&mut app, "Live pulse");
        app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
        assert!(ledger.borrow().is_empty(), "return cannot admit {kind}");
        assert_eq!(
            app.pending_history_controls()[0].invocation,
            saved[0].invocation
        );
    }
}

#[test]
fn control_custody_core_first_emission_after_return_keeps_live_semantics() {
    use crate::application::{HostContext, LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        APP, author, provenance, runner_permissions, runner_probe_definition,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let records = owners.open(APP, "bridge-workspace").unwrap();
    let views = std::rc::Rc::new(
        crate::application_view::InstanceViewStore::new(
            directory.path().join("records"),
            "fixture-device",
            "bridge-workspace",
        )
        .unwrap(),
    );
    let definition = runner_probe_definition();
    let context = HostContext::prepare(
        definition,
        records,
        views,
        "fixture-device".into(),
        1971,
        runner_permissions(),
        author(),
        provenance(),
        &LaunchTarget::New,
    )
    .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let starts = std::rc::Rc::new(std::cell::Cell::new(0));
    let payloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (complete_tx, complete_rx) = std::sync::mpsc::channel();
    let completions = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut live = ComponentControlOwner {
        entered: Some(CustodyEnteredOperation {
            receiver: complete_rx,
            active: None,
            completions: completions.clone(),
        }),
        emitter: None,
        identity_supported: std::rc::Rc::new(std::cell::Cell::new(true)),
        duplicate_depth: std::rc::Rc::new(std::cell::Cell::new(0)),
        id: "actual-bridge".into(),
        operation: "fixture.actual-bridge".into(),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        starts: starts.clone(),
        payloads: payloads.clone(),
        text: crate::widget::Text::new("Actual bridge"),
    };
    let resource = live.resource();
    let emitter = std::rc::Rc::new(std::cell::RefCell::new(
        crate::widget::ActionEmitter::new_in(
            tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |arguments| {
                arguments
            })
            .unwrap(),
            &component
                .callback_binding()
                .control_domain
                .upgrade()
                .unwrap(),
        ),
    ));
    live.emitter = Some(emitter.clone());
    component.root = Some(Box::new(live));
    component.mount(|_| {}).unwrap();
    let binding = component.callback_binding();
    static BRIDGE_HISTORY: HistoryDefinition = HistoryDefinition {
        app_id: crate::application_test_support::APP,
        scopes: &[HistoryScopeDefinition {
            key: "document",
            title: "Bridge document",
            presentation: HistoryPresentation::Domain {
                create: crate::history_mode::test_preview,
            },
            content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
        }],
    };
    let registration = HistoryRegistration::new(
        &BRIDGE_HISTORY,
        vec![
            HistorySourceRegistration::owned(
                "document",
                APP,
                HistoryOwner::component(binding.clone()),
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let denied = std::rc::Rc::new(std::cell::Cell::new(false));
    let root = Box::new(CustodyDeniedRoot {
        child: Box::new(
            crate::widget::Column::new()
                .push(Box::new(crate::widget::BoundAppWidget { component }))
                .push(Box::new(
                    crate::widget::Button::new("live-pulse", "Live pulse", None).on_click(|| {}),
                )),
        ),
        denied: denied.clone(),
        exit_callback: None,
    });
    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(root, registration),
    )
    .unwrap();
    assert!(app.validate_history_registration().is_ok());
    layout(&mut app);
    // The callback retains only the ordinary live emitter across History.
    let emit_later = |alias: &str| {
        emitter
            .borrow_mut()
            .emit(nested_control("actual-bridge", alias, 7).arguments)
            .unwrap()
    };
    emit_later("entered-before-history");
    let named_action_target = named(&mut app, "Live pulse");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(starts.get(), 1);
    emit_later("saved-E");
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    complete_tx.send(()).unwrap();
    app.dispatch_framework_history_event(&winit::event::WindowEvent::Focused(true), 1.0);
    app.dispatch_framework_history_event(&winit::event::WindowEvent::Focused(true), 1.0);
    assert_eq!(
        completions.get(),
        1,
        "entered operation settles exactly once while hidden"
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let saved = app.pending_history_controls();
    denied.set(true);
    emit_later("first-emission-F");
    let named_action_target = named(&mut app, "Live pulse");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        starts.get(),
        2,
        "registered dispatch does not need mutable queue traversal"
    );
    assert_eq!(
        payloads.borrow()[1],
        serde_json::json!({"alias":"first-emission-F","nested":{"route":{"leaf":7}}})
    );
    assert_eq!(app.pending_history_controls(), saved);
    binding.closing.set(true);
    emit_later("revoked-owner");
    let named_action_target = named(&mut app, "Live pulse");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        starts.get(),
        2,
        "current owner revocation remains canonical"
    );
    let (mut confirming, confirmed_ledger) =
        custody_matrix_app_with_confirmation("Button", tilo_actions::ConfirmationPolicy::Confirm);
    palette_history_round_trip(&mut confirming);
    custody_matrix_activate(&mut confirming, "Button", false);
    let named_action_target = named(&mut confirming, "Live pulse");
    confirming.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert!(
        confirmed_ledger.borrow().is_empty(),
        "fresh F still requires current confirmation"
    );
    assert_eq!(editor_snapshot(&confirming).2, "confirmation");
    confirming
        .actions
        .action_palette
        .as_mut()
        .unwrap()
        .set_query("CONFIRM");
    palette_enter(&mut confirming);
    assert_eq!(&*confirmed_ledger.borrow(), &[serde_json::json!({})]);
}

impl crate::widget::AppWidget for CustodyControlsRoot {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Custody controls"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("C")
    }
    fn permission_hint(&self) -> Option<crate::widget::AppKind> {
        None
    }
}
fn custody_owned_button(
    label: &str,
    ledger: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
) -> CustodyControlsRoot {
    let resource = custody_matrix_spec("Button");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "run").unwrap();
    CustodyControlsRoot {
        draw_callback: None,
        overlay_callback: None,
        mark_drawn_callback: None,
        unsettled_layout: None,
        installed_output: None,
        record: None,
        child: Box::new(crate::widget::Button::new(label, label, None).on_action(binding)),
        resources: vec![resource],
        ledger,
    }
}
#[test]
fn control_custody_core_outer_component_legacy_owners_and_sibling() {
    use crate::application::{HostContext, LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        APP, author, provenance, runner_permissions, runner_probe_definition,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let a_ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let b_ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let outer_ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let store = owners.open(HISTORY.app_id, "legacy").unwrap();
    let checkout = crate::RecordCheckout::new(
        crate::RecordId::for_tab(734),
        crate::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "live").unwrap();
            Ok(())
        })
        .unwrap();
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
        custody_owned_button("A", a_ledger.clone()),
    )));
    tile.bind_record(store, checkout);
    tile.register_document_history(&HISTORY).unwrap();
    let owner = tile.history_owner().unwrap();
    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner.0 else {
        panic!("real legacy owner");
    };
    let a_domain = lifetime.upgrade().unwrap().control_domain.clone();
    let a_output = a_domain.producer(crate::control_custody::ControlProducerDiscipline::ReplaceOne);
    let views = std::rc::Rc::new(
        crate::application_view::InstanceViewStore::new(
            directory.path().join("records"),
            "fixture-device",
            "component",
        )
        .unwrap(),
    );
    let definition = runner_probe_definition();
    let context = HostContext::prepare(
        definition,
        owners.open(APP, "component").unwrap(),
        views,
        "fixture-device".into(),
        735,
        runner_permissions(),
        author(),
        provenance(),
        &LaunchTarget::New,
    )
    .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.root = Some(Box::new(custody_owned_button("B", b_ledger.clone())));
    component.mount(|_| {}).unwrap();
    let b_binding = component.callback_binding();
    let mut outer = custody_owned_button("Outer", outer_ledger.clone());
    outer.child = Box::new(
        crate::widget::Row::new()
            .no_wrap()
            .push(outer.child)
            .push(Box::new(tile))
            .push(Box::new(crate::widget::BoundAppWidget { component })),
    );
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(outer));
    layout(&mut app);
    let a = node_for_test_id(&app, "A");
    assert!(
        app.dispatch_node(a, |widget, doc, path| widget.perform_action(
            &crate::a11y::Action::Activate,
            doc,
            path
        ))
        .unwrap()
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let hidden = tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("fixture:control", "run"),
        arguments: serde_json::Map::new(),
    };
    assert_eq!(
        a_output.emit(hidden),
        Ok(crate::control_custody::LocalEmission::Suppressed)
    );
    let named_action_target = named(&mut app, "B");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    let named_action_target = named(&mut app, "Outer");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        b_ledger.borrow().len(),
        1,
        "same-address component sibling stays live"
    );
    assert_eq!(outer_ledger.borrow().len(), 1, "outer owner stays live");
    assert!(a_ledger.borrow().is_empty());
    let mut saved = Vec::new();
    app.root_widget
        .visit_framework_history(&mut |history, _| saved.extend(history.pending_control_reviews()));
    assert_eq!(saved.len(), 1);
    assert!(saved[0].owner.starts_with("legacy owner "));
    assert!(
        saved[0].confirmation.is_ok(),
        "legacy entry witness: {:?}",
        saved[0]
    );
    assert!(!a_domain.same(&b_binding.control_domain.upgrade().unwrap()));
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(a_ledger.borrow().is_empty());
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        a_ledger.borrow().len(),
        1,
        "saved legacy E uses original owner"
    );
    assert_eq!(b_ledger.borrow().len(), 1);
    assert_eq!(outer_ledger.borrow().len(), 1);
}

fn custody_top_registration(owner: HistoryOwner) -> HistoryRegistration {
    HistoryRegistration::new(
        &HISTORY,
        vec![HistorySourceRegistration::owned("document", HISTORY.app_id, owner).unwrap()],
    )
    .unwrap()
}
fn custody_embedded_click(app: &mut App, id: &str) {
    app.layout.node_map.clear();
    app.bindings.clear();
    let mut clicked = false;
    app.root_widget
        .visit_framework_history_mut(&mut |history, _| {
            clicked |= history.activate_retained_control_for_test(id);
        });
    assert!(clicked, "real embedded control {id}");
    layout(app);
}
fn custody_outer_history(app: &mut App) {
    layout(app);
    let node = app
        .layout
        .node_map
        .keys()
        .copied()
        .find(|node| {
            app.history
                .as_ref()
                .unwrap()
                .owns_node(*node, &app.layout.taffy)
                && app
                    .inspect_node(*node, |widget| widget.test_id() == Some("history-open"))
                    .unwrap_or(false)
        })
        .unwrap();
    let id = crate::accesskit_bridge::widget_ak_id(&app.layout.node_map, node).unwrap();
    app.dispatch_a11y_action(id, &crate::a11y::Action::Activate);
    layout(app);
}
#[test]
fn control_custody_core_actual_nested_surfaces_finish_in_both_orders() {
    for outer_first in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut records = crate::record::RecordOwners::new(directory.path().join("records"));
        let store = records.open(HISTORY.app_id, "nested").unwrap();
        let checkout = crate::RecordCheckout::new(
            crate::RecordId::for_tab(884),
            crate::RecordKind::new("note"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
        let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(
            custody_owned_button("A", ledger.clone()),
        )));
        tile.bind_record(store, checkout);
        tile.register_document_history(&HISTORY).unwrap();
        let owner = tile.history_owner().unwrap();
        let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner.0 else {
            panic!("legacy owner")
        };
        let domain = lifetime.upgrade().unwrap().control_domain.clone();
        let raw = domain.producer(crate::ControlProducerDiscipline::ReplaceOne);
        let mut app = App::from_managed_root_with_controlled_inputs(
            State::new(),
            ManagedRoot::new(Box::new(tile), custody_top_registration(owner.clone())),
        )
        .unwrap();
        layout(&mut app);
        let a = node_for_test_id(&app, "A");
        assert!(
            app.dispatch_node(a, |widget, doc, path| widget.perform_action(
                &crate::a11y::Action::Activate,
                doc,
                path
            ))
            .unwrap()
        );
        custody_embedded_click(&mut app, "history-open");
        custody_outer_history(&mut app);
        assert!(
            app.pending_history_controls().is_empty(),
            "outer cannot recapture inner E"
        );
        let request = tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new("fixture:control", "run"),
            arguments: serde_json::Map::new(),
        };
        assert_eq!(
            raw.emit(request.clone()),
            Ok(crate::control_custody::LocalEmission::Suppressed)
        );
        if outer_first {
            custody_outer_history(&mut app);
        } else {
            custody_embedded_click(&mut app, "history-open");
        }
        assert_eq!(
            raw.emit(request.clone()),
            Ok(crate::control_custody::LocalEmission::Suppressed),
            "one covering surface still owns custody"
        );
        if !outer_first {
            custody_embedded_click(&mut app, "history-pending-1");
            app.service_pending_history_commands();
            assert!(
                ledger.borrow().is_empty(),
                "saved Continue cannot bypass covering outer lease"
            );
            custody_outer_history(&mut app);
        } else {
            custody_embedded_click(&mut app, "history-open");
        }
        assert!(ledger.borrow().is_empty());
        custody_embedded_click(&mut app, "history-pending-1");
        app.service_pending_history_commands();
        assert_eq!(
            ledger.borrow().len(),
            1,
            "original E continues once after both leases end"
        );
        assert!(matches!(
            raw.emit(request),
            Ok(crate::control_custody::LocalEmission::Queued(_))
        ));
    }
}
#[test]
fn control_custody_core_actual_retained_tile_rebind_closes_old_outboxes() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners.open(HISTORY.app_id, "rebind").unwrap();
    let a = crate::RecordCheckout::new(
        crate::RecordId::for_tab(881),
        crate::RecordKind::new("note"),
    );
    let b = crate::RecordCheckout::new(
        crate::RecordId::for_tab(882),
        crate::RecordKind::new("note"),
    );
    for checkout in [&a, &b] {
        store
            .reserve_record(checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }
    let record = std::rc::Rc::new(std::cell::RefCell::new(None));
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut live = custody_owned_button("A", ledger.clone());
    live.record = Some(record.clone());
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(live)));
    tile.bind_record(store.clone(), a.clone());
    let owner = tile.history_owner().unwrap();
    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner.0 else {
        panic!("legacy owner")
    };
    let old = lifetime.upgrade().unwrap().control_domain.clone();
    let raw = old.producer(crate::ControlProducerDiscipline::ReplaceOne);
    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(Box::new(tile), custody_top_registration(owner.clone())),
    )
    .unwrap();
    layout(&mut app);
    let node = node_for_test_id(&app, "A");
    assert!(
        app.dispatch_node(node, |widget, doc, path| widget.perform_action(
            &crate::a11y::Action::Activate,
            doc,
            path
        ))
        .unwrap()
    );
    custody_outer_history(&mut app);
    let saved = app.pending_history_controls();
    assert_eq!(saved.len(), 1);
    *record.borrow_mut() = Some(b.clone());
    custody_outer_history(&mut app);
    assert!(
        old.closed(),
        "real retained TileHost record transition closes prior domain"
    );
    let request = tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("fixture:control", "run"),
        arguments: serde_json::Map::new(),
    };
    assert_eq!(
        raw.emit(request),
        Err(crate::control_custody::EmitError::Closed)
    );
    assert_eq!(
        app.pending_history_controls()[0].invocation,
        saved[0].invocation
    );
    assert!(
        matches!(
            app.pending_history_controls()[0].status,
            crate::history_mode::pending_controls::PendingControlStatus::Refused(_)
        ),
        "closed saved E stays visibly refused"
    );
    assert!(
        ledger.borrow().is_empty(),
        "saved E cannot retarget replacement"
    );
    // The outer History declaration named the old exact record owner. Its
    // invalidation is intentional. Replace that surface through the real host
    // installation API, retaining the old host long enough to inspect refusal.
    let old_history = app.history.take().unwrap();
    assert_eq!(
        old_history.pending_control_reviews()[0].invocation,
        saved[0].invocation
    );
    assert!(matches!(
        old_history.pending_control_reviews()[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Refused(_)
    ));
    let current = app.root_widget.history_owner().unwrap();
    drop(old_history);
    app.install_history(custody_top_registration(current))
        .unwrap();
    layout(&mut app);
    let named_action_target = named(&mut app, "A");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        ledger.borrow().len(),
        1,
        "same retained shared control works for new owner"
    );
    let current = app
        .bindings
        .legacy_for(node_for_test_id(&app, "A"))
        .unwrap();
    assert_eq!(current.store_checkout().1, &b);
    assert!(!current.same_owner(&owner));
}

#[test]
fn control_custody_core_mounted_modelview_hidden_builder_and_fresh_button() {
    let scope = crate::WakeScope::new();
    let _entry = scope.enter();
    let domain = crate::control_custody::ControlDomain::outer();
    let key = std::rc::Rc::new(std::cell::Cell::new(false));
    let projection = key.clone();
    let retained = std::rc::Rc::new(std::cell::RefCell::new(None));
    let output = retained.clone();
    let resource = custody_matrix_spec("Button");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "run").unwrap();
    let view = crate::widget::ModelView::from_document_in(
        &domain,
        move |_, _| projection.get(),
        move |mounted, domain| {
            if !*mounted {
                return Box::new(crate::widget::Text::new("Waiting"));
            }
            let outbox = domain.producer(crate::ControlProducerDiscipline::ReplaceOne);
            assert_eq!(
                outbox.emit(binding.invoke(()).unwrap()),
                Ok(crate::control_custody::LocalEmission::Suppressed),
                "builder sees covering lease before emitting"
            );
            *output.borrow_mut() = Some(outbox);
            Box::new(
                crate::widget::Button::new("dynamic", "Dynamic", None).on_action(binding.clone()),
            )
        },
    );
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let root = CustodyControlsRoot {
        draw_callback: None,
        overlay_callback: None,
        mark_drawn_callback: None,
        unsettled_layout: None,
        installed_output: None,
        record: None,
        child: Box::new(view),
        resources: vec![resource],
        ledger: ledger.clone(),
    };
    let mut app =
        App::from_scoped_parts_with_controlled_custody(scope, State::new(), Box::new(root), domain);
    app.install_history(
        HistoryRegistration::new(
            &HISTORY,
            vec![HistorySourceRegistration::HostDocument {
                scope: "document",
                resource: HistoryResource::new(
                    "fixture",
                    "dynamic",
                    HISTORY.app_id,
                    crate::RecordId::for_tab(998),
                ),
            }],
        )
        .unwrap(),
    )
    .unwrap();
    layout(&mut app);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    key.set(true);
    // Public host synchronization used for model-derived structure changes.
    app.layout.node_map.clear();
    app.bindings.clear();
    app.refresh_focus_order();
    layout(&mut app);
    assert!(retained.borrow().is_some());
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    assert!(ledger.borrow().is_empty());
    assert!(app.pending_history_controls().is_empty());
    let named_action_target = named(&mut app, "Dynamic");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        &*ledger.borrow(),
        &[serde_json::json!({})],
        "new passive shared control uses ordinary canonical door"
    );
}

#[test]
fn control_custody_core_launch_initial_install_preserves_prehost_control() {
    thread_local! { static LEDGER: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>> = std::rc::Rc::new(std::cell::RefCell::new(Vec::new())); }
    fn root() -> Box<dyn crate::widget::Widget> {
        let ledger = LEDGER.with(Clone::clone);
        let mut root = custody_owned_button("Launch control", ledger);
        assert!(root.child.perform_action(
            &crate::a11y::Action::Activate,
            &mut automerge::AutoCommit::new(),
            &ROOT
        ));
        Box::new(root)
    }
    fn open(_: &'static str) -> (State, crate::persist::DocWriter) {
        (
            State::new(),
            crate::persist::DocWriter::disabled("inert custody launch fixture"),
        )
    }
    let spec = crate::launch::LaunchSpec::new(HISTORY.app_id, root)
        .open_app_global_state(open)
        .history(&HISTORY);
    let mut app = spec.build_controlled_app_for_test().unwrap();
    assert_eq!(app.control_coverage().registered, 1);
    assert!(app.control_coverage().legacy_compatibility_unqualified);
    layout(&mut app);
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let saved = app.pending_history_controls();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].invocation.address,
        tilo_actions::ActionAddress::new("fixture:control", "run")
    );
    assert!(saved[0].invocation.arguments.is_empty());
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    LEDGER.with(|ledger| assert!(ledger.borrow().is_empty()));
    let named_action_target = named(&mut app, "Continue");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    LEDGER.with(|ledger| assert_eq!(&*ledger.borrow(), &[serde_json::json!({})]));
}

#[test]
fn control_custody_core_driver_embedded_return_covers_final_draw_callback() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners.open(HISTORY.app_id, "draw-exit").unwrap();
    let checkout = crate::RecordCheckout::new(
        crate::RecordId::for_tab(891),
        crate::RecordKind::new("note"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "body", "live").unwrap();
            Ok(())
        })
        .unwrap();
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let output: std::rc::Rc<std::cell::RefCell<Option<crate::ControlOutbox>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let disposition = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut live = custody_owned_button("A", ledger.clone());
    live.draw_callback = Some({
        let output = output.clone();
        let armed = armed.clone();
        let disposition = disposition.clone();
        Box::new(move || {
            if armed.replace(false) {
                disposition
                    .borrow_mut()
                    .push(output.borrow().as_ref().unwrap().emit(
                        tilo_actions::ControlInvocation {
                            address: tilo_actions::ActionAddress::new("fixture:control", "run"),
                            arguments: serde_json::Map::new(),
                        },
                    ));
            }
        })
    });
    let mut tile = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(live)));
    tile.bind_record(store, checkout);
    tile.register_document_history(&HISTORY).unwrap();
    let owner = tile.history_owner().unwrap();
    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner.0 else {
        panic!("legacy owner")
    };
    *output.borrow_mut() = Some(
        lifetime
            .upgrade()
            .unwrap()
            .control_domain
            .producer(crate::ControlProducerDiscipline::ReplaceOne),
    );
    let mut driver = crate::headless::Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(tile),
        (900, 650),
        crate::Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::application_test_support::author(),
    );
    driver
        .perform_a11y_action(
            driver.a11y_node_named("History").unwrap(),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    armed.set(true);
    driver
        .perform_a11y_action(
            driver.a11y_node_named("History").unwrap(),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver.send(&winit::event::WindowEvent::Focused(true));
    assert!(
        ledger.borrow().is_empty(),
        "exit draw output cannot become first live work"
    );
    assert_eq!(
        &*disposition.borrow(),
        &[Ok(crate::control_custody::LocalEmission::Suppressed)]
    );
    driver
        .perform_a11y_action(
            driver.a11y_node_named("A").unwrap(),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    assert_eq!(
        ledger.borrow().len(),
        1,
        "first actual UI emission after return remains live"
    );
}

fn custody_round1_native_return(embedded: bool) {
    let directory = tempfile::tempdir().unwrap();
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let output = std::rc::Rc::new(std::cell::RefCell::new(None::<crate::ControlOutbox>));
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let dispositions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let unsettled = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut live = custody_owned_button("A", ledger.clone());
    live.unsettled_layout = Some(unsettled.clone());
    live.child = Box::new(crate::widget::Column::new().push(live.child).push(Box::new(
        crate::widget::Button::new("pulse", "Live pulse", None).on_click(|| {}),
    )));
    live.installed_output = Some(output.clone());
    for phase in ["draw", "overlay", "mark_drawn"] {
        let output = output.clone();
        let armed = armed.clone();
        let dispositions = dispositions.clone();
        let callback: Box<dyn FnMut()> = Box::new(move || {
            if armed.get() {
                dispositions.borrow_mut().push((
                    phase,
                    output
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .emit(tilo_actions::ControlInvocation {
                            address: tilo_actions::ActionAddress::new("fixture:control", "run"),
                            arguments: serde_json::Map::new(),
                        }),
                ));
            }
        });
        match phase {
            "overlay" => live.overlay_callback = Some(callback),
            "mark_drawn" => live.mark_drawn_callback = Some(callback),
            _ => live.draw_callback = Some(callback),
        }
    }
    let mut app = if embedded {
        let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
        let store = owners.open(HISTORY.app_id, "round1-native").unwrap();
        let checkout = crate::RecordCheckout::new(
            crate::RecordId::for_tab(892),
            crate::RecordKind::new("note"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
        let mut tile =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(live)));
        tile.bind_record(store, checkout);
        tile.register_document_history(&HISTORY).unwrap();
        App::from_factory_with_controlled_inputs(State::new(), || Box::new(tile))
    } else {
        let registration = HistoryRegistration::new(
            &HISTORY,
            vec![HistorySourceRegistration::HostDocument {
                scope: "document",
                resource: HistoryResource::new(
                    "fixture",
                    "round1-native",
                    HISTORY.app_id,
                    crate::RecordId::for_tab(893),
                ),
            }],
        )
        .unwrap();
        App::from_managed_root_with_controlled_inputs(
            State::new(),
            ManagedRoot::new(Box::new(live), registration),
        )
        .unwrap()
    };
    layout(&mut app);
    let a = node_for_test_id(&app, "A");
    assert!(
        app.dispatch_node(a, |widget, doc, path| widget.perform_action(
            &crate::a11y::Action::Activate,
            doc,
            path
        ))
        .unwrap()
    );
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    layout(&mut app);
    let reviews = |app: &App| {
        let mut saved = app.pending_history_controls();
        app.root_widget.visit_framework_history(&mut |history, _| {
            saved.extend(history.pending_control_reviews())
        });
        saved
    };
    let saved = reviews(&app);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].invocation.address,
        tilo_actions::ActionAddress::new("fixture:control", "run")
    );
    assert!(saved[0].invocation.arguments.is_empty());
    let named_action_target = named(&mut app, "History");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    // Deliberately stop at the production layout boundary: a deferred/aborted
    // CPU frame must retain custody until a later successful draw completes.
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(&mut app, root, 900, 650, 1.0, true, None).is_some());
    let deferred_request = tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("fixture:control", "run"),
        arguments: serde_json::Map::new(),
    };
    assert_eq!(
        output
            .borrow()
            .as_ref()
            .unwrap()
            .emit(deferred_request.clone()),
        Ok(crate::control_custody::LocalEmission::Suppressed),
        "layout without frame completion keeps custody"
    );
    unsettled.set(true);
    app.layout.layout_dirty = true;
    let root = app.layout.root_node.unwrap();
    assert!(
        run_layout_pass(&mut app, root, 900, 650, 1.0, true, None).is_none(),
        "actual unsettled layout aborts the frame"
    );
    assert_eq!(
        output.borrow().as_ref().unwrap().emit(deferred_request),
        Ok(crate::control_custody::LocalEmission::Suppressed),
        "aborted frame keeps custody"
    );
    unsettled.set(false);
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(&mut app, root, 900, 650, 1.0, true, None).is_some());
    armed.set(true);
    let mut scene = vello::Scene::new();
    draw_frame_content(&mut app, &mut scene, 900, 650, 1.0, None);
    complete_native_frame_content(&mut app, |app| {
        let _ = app.build_a11y_tree_update(1.0);
        dispositions.borrow_mut().push((
            "accessibility publication",
            output
                .borrow()
                .as_ref()
                .unwrap()
                .emit(tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("fixture:control", "run"),
                    arguments: serde_json::Map::new(),
                }),
        ));
    });
    armed.set(false);
    let named_action_target = named(&mut app, "Live pulse");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert!(
        ledger.borrow().is_empty(),
        "native exit draw output cannot become first live work; embedded={embedded}"
    );
    assert!(
        !dispositions.borrow().is_empty(),
        "actual live draw callback must run"
    );
    assert!(
        dispositions
            .borrow()
            .iter()
            .all(|(_, value)| *value == Ok(crate::control_custody::LocalEmission::Suppressed)),
        "all exit draw/overlay output must be suppressed: {:?}",
        dispositions.borrow()
    );
    if !embedded {
        assert_eq!(
            dispositions.borrow().len(),
            4,
            "outer draw, overlay, mark_drawn and accessibility publication exercised"
        );
    }
    let mut saved = saved;
    for item in &mut saved {
        item.status = crate::history_mode::pending_controls::PendingControlStatus::Ready;
    }
    assert_eq!(reviews(&app), saved);
    let named_action_target = named(&mut app, "A");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(
        ledger.borrow().len(),
        1,
        "fresh actual UI activation after completed return"
    );
    assert_eq!(reviews(&app), saved);
}
#[test]
fn control_custody_core_native_return_covers_final_draw_callbacks() {
    custody_round1_native_return(false);
}
#[test]
fn control_custody_core_native_embedded_return_covers_final_draw_callbacks() {
    custody_round1_native_return(true);
}

#[test]
fn control_custody_core_nested_unbound_tile_follows_authorized_record_renewal() {
    use crate::control_custody::{ControlDomain, CustodyAttachment, EmitError};
    use crate::widget::Widget;

    let directory = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(directory.path().join("records"));
    let store = owners.open(HISTORY.app_id, "nested-renewal").unwrap();
    let a = crate::RecordCheckout::new(
        crate::RecordId::for_tab(891),
        crate::RecordKind::new("note"),
    );
    let b = crate::RecordCheckout::new(
        crate::RecordId::for_tab(892),
        crate::RecordKind::new("note"),
    );
    for checkout in [&a, &b] {
        store
            .reserve_record(checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "body", "live").unwrap();
                Ok(())
            })
            .unwrap();
    }

    let record = std::rc::Rc::new(std::cell::RefCell::new(None));
    let ledger = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let inner_output = std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut inner_root = custody_owned_button("nested-renewal", ledger.clone());
    inner_root.installed_output = Some(inner_output.clone());
    let inner = crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(inner_root)));
    let outer_root = CustodyControlsRoot {
        draw_callback: None,
        overlay_callback: None,
        mark_drawn_callback: None,
        unsettled_layout: None,
        installed_output: None,
        record: Some(record.clone()),
        child: Box::new(inner),
        resources: vec![],
        ledger: ledger.clone(),
    };
    let mut outer =
        crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(outer_root)));
    outer.bind_record(store, a.clone());
    let owner_a = outer.history_owner().unwrap();
    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner_a.0 else {
        panic!("legacy A owner");
    };
    let domain_a = lifetime.upgrade().unwrap().control_domain.clone();
    let outer_raw = domain_a.producer(crate::ControlProducerDiscipline::ReplaceOne);

    let mut app = App::from_managed_root_with_controlled_inputs(
        State::new(),
        ManagedRoot::new(Box::new(outer), custody_top_registration(owner_a.clone())),
    )
    .unwrap();
    layout(&mut app);
    let inner_raw = inner_output
        .borrow()
        .as_ref()
        .expect("inner root installed under A")
        .clone();

    let node = node_for_test_id(&app, "nested-renewal");
    assert!(
        app.dispatch_node(node, |widget, doc, path| widget.perform_action(
            &crate::a11y::Action::Activate,
            doc,
            path,
        ))
        .unwrap()
    );
    custody_outer_history(&mut app);
    let saved = app.pending_history_controls();
    assert_eq!(saved.len(), 1);
    let saved_id = saved[0].id;
    let saved_owner = saved[0].owner.clone();
    let saved_invocation = saved[0].invocation.clone();
    assert_eq!(
        saved_invocation,
        tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new("fixture:control", "run"),
            arguments: serde_json::Map::new(),
        }
    );
    assert_eq!(
        saved_owner,
        format!("legacy owner {}", owner_a.lifetime_id().unwrap())
    );

    *record.borrow_mut() = Some(b.clone());
    custody_outer_history(&mut app);
    let after = app.pending_history_controls();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, saved_id);
    assert_eq!(after[0].owner, "legacy owner unavailable");
    assert_eq!(after[0].invocation, saved_invocation);
    assert!(matches!(
        &after[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Refused(_)
    ));

    let owner_b = app.root_widget.history_owner().unwrap();
    assert!(!owner_b.same_owner(&owner_a));
    assert!(owner_a.lifetime_id().is_none());
    assert_eq!(owner_a.store_checkout().1, &a);
    assert_eq!(owner_b.store_checkout().1, &b);
    assert_ne!(
        after[0].owner,
        format!("legacy owner {}", owner_b.lifetime_id().unwrap())
    );
    let crate::history_mode::owner::OwnerKind::Legacy { lifetime, .. } = &owner_b.0 else {
        panic!("legacy B owner");
    };
    let domain_b = lifetime.upgrade().unwrap().control_domain.clone();
    assert!(domain_a.closed());
    assert!(!domain_b.same(&domain_a));
    let closed_probe = nested_control("fixture:control", "old-handle", 91);
    assert_eq!(outer_raw.emit(closed_probe.clone()), Err(EmitError::Closed));
    assert_eq!(inner_raw.emit(closed_probe), Err(EmitError::Closed));
    assert!(
        domain_b.drain_live().is_empty(),
        "no A queue transfers to B"
    );
    assert!(
        ledger.borrow().is_empty(),
        "return does not execute saved A work"
    );

    let c = ControlDomain::outer();
    c.finish_initial_installation();
    app.root_widget.children_mut()[0].children_mut()[0]
        .install_control_custody(&c, CustodyAttachment::Dynamic);
    assert!(c.coverage().unsafe_late_attachments > 0);
    assert!(c.drain_live().is_empty());

    let old_history = app.history.take().unwrap();
    let old_review = old_history.pending_control_reviews();
    assert_eq!(old_review[0].id, saved_id);
    assert_eq!(old_review[0].owner, "legacy owner unavailable");
    assert_ne!(
        old_review[0].owner,
        format!("legacy owner {}", owner_b.lifetime_id().unwrap())
    );
    assert_eq!(old_review[0].invocation, saved_invocation);
    assert!(matches!(
        &old_review[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Refused(_)
    ));
    drop(old_history);
    app.install_history(custody_top_registration(owner_b))
        .unwrap();
    layout(&mut app);

    let named_action_target = named(&mut app, "nested-renewal");
    app.dispatch_a11y_action(named_action_target, &crate::a11y::Action::Activate);
    assert_eq!(&*ledger.borrow(), &[serde_json::json!({})]);
    assert!(
        c.drain_live().is_empty(),
        "foreign C never receives fresh B work"
    );
    let current = app
        .bindings
        .legacy_for(node_for_test_id(&app, "nested-renewal"))
        .unwrap();
    assert_eq!(current.store_checkout().1, &b);
}

#[test]
fn control_custody_core_unbound_tile_foreign_attachment_preserves_owner() {
    use crate::control_custody::{ControlDomain, CustodyAttachment, LocalEmission};
    use crate::widget::Widget;
    for suspended in [false, true] {
        let a = ControlDomain::outer();
        let b = ControlDomain::outer();
        let request = tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new("fixture:control", "run"),
            arguments: serde_json::Map::new(),
        };
        let origin_key = |item: &crate::control_custody::OwnedControl| {
            let crate::history_mode::pending_controls::PendingControlOrigin::Outer {
                root_lifetime,
            } = &item.origin
            else {
                panic!("original outer owner");
            };
            *root_lifetime
        };
        let probe = a.producer(crate::ControlProducerDiscipline::ReplaceOne);
        probe.emit(request.clone()).unwrap();
        let expected_owner = origin_key(&a.drain_live()[0]);
        let left = a.child();
        let right = a.child();
        let output = std::rc::Rc::new(std::cell::RefCell::new(None::<crate::ControlOutbox>));
        let mut live = custody_owned_button("A", Default::default());
        live.installed_output = Some(output.clone());
        let mut tile =
            crate::widget::TileHost::new(crate::widget::TileContent::App(Box::new(live)));
        assert!(tile.history_owner().is_none(), "actual unbound TileHost");
        tile.install_control_custody(&left, CustodyAttachment::Initial);
        a.finish_initial_installation();
        b.finish_initial_installation();
        let activate = |tile: &mut crate::widget::TileHost| {
            assert!(tile.children_mut()[0].children_mut()[0].perform_action(
                &crate::a11y::Action::Activate,
                &mut automerge::AutoCommit::new(),
                &ROOT
            ));
        };
        activate(&mut tile);
        let (lease, saved) = if suspended {
            let (lease, saved) = a.suspend();
            (Some(lease), saved)
        } else {
            (None, vec![])
        };
        tile.install_control_custody(&b, CustodyAttachment::Dynamic);
        assert!(
            b.drain_live().is_empty(),
            "foreign B must not adopt queued shared-control E"
        );
        assert!(
            b.coverage().unsafe_late_attachments > 0,
            "foreign attachment must be visibly refused"
        );

        let emitted = output
            .borrow()
            .as_ref()
            .unwrap()
            .emit(request.clone())
            .unwrap();
        if suspended {
            assert_eq!(
                emitted,
                LocalEmission::Suppressed,
                "A lease remains covering"
            );
            assert_eq!(saved.len(), 1);
            assert_eq!(origin_key(&saved[0]), expected_owner);
            assert_eq!(saved[0].invocation, request);
        } else {
            assert!(matches!(emitted, LocalEmission::Queued(_)));
            let original = a.drain_live();
            assert_eq!(original.len(), 2);
            assert!(
                original
                    .iter()
                    .all(|item| item.invocation == request && origin_key(item) == expected_owner)
            );
        }
        assert!(
            b.drain_live().is_empty(),
            "old retained raw handle cannot route to B"
        );
        drop(lease);
        // Legitimate presentation reparenting stays in A's exact incarnation.
        tile.install_control_custody(&right, CustodyAttachment::Dynamic);
        activate(&mut tile);
        assert_eq!(
            a.drain_live().len(),
            1,
            "same-owner shared control remains live exactly once"
        );
        assert!(b.drain_live().is_empty());
        assert_eq!(a.coverage().unsafe_late_attachments, 0);
    }
}

#[test]
fn committed_queued_publication_failure_restores_all_producer_frontiers() {
    let mut fixture = RoutedOwners::new();
    // Actually enqueue each owner snapshot, which clears its replay obligation.
    let deadline = crate::time::now() + crate::time::Duration::from_secs(2);
    loop {
        fixture.app.publish_owned_documents();
        if fixture
            .app
            .document_routes
            .owners
            .iter()
            .all(|owner| !owner.needs_snapshot)
        {
            break;
        }
        assert!(crate::time::now() < deadline);
        std::thread::sleep(crate::time::Duration::from_millis(10));
    }
    fixture
        .app
        .state
        .doc
        .put(ROOT, "primary_accepted", "keep")
        .unwrap();
    let (_, heads) = fixture.app.state.pending_outbound_delta();
    fixture.app.state.advance_outbound(heads);
    let mut index = automerge::AutoCommit::new();
    index.put(ROOT, "index_accepted", "keep").unwrap();
    fixture.app.persistence.room_index = Some(crate::state::DocumentReplica::new(index));
    fixture
        .app
        .collaboration
        .manager
        .fail_publication_for_test();
    fixture.app.refresh_document_routes();
    assert!(
        fixture
            .app
            .document_routes
            .owners
            .iter()
            .all(|owner| owner.needs_snapshot),
        "accepted owner snapshots must be replayable after terminal worker loss"
    );
    assert!(
        !fixture.app.state.pending_outbound_delta().0.is_empty(),
        "primary frontier must replay"
    );
    assert!(
        !fixture
            .app
            .persistence
            .room_index
            .as_mut()
            .unwrap()
            .pending_outbound_delta()
            .0
            .is_empty(),
        "index frontier must replay"
    );

    let mut bound = RoutedPrimary::new(PrimaryMetadataShape::None);
    let (delta, heads) = bound.app.pending_primary_document_delta().unwrap();
    assert!(!delta.is_empty());
    bound.app.advance_primary_document(heads);
    assert!(
        bound
            .app
            .pending_primary_document_delta()
            .unwrap()
            .0
            .is_empty()
    );
    bound.app.collaboration.manager.fail_publication_for_test();
    bound.app.refresh_document_routes();
    assert!(
        !bound
            .app
            .pending_primary_document_delta()
            .unwrap()
            .0
            .is_empty(),
        "canonical primary owner must replay its accepted history too"
    );
}
