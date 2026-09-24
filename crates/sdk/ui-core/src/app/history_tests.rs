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
    App::from_managed_root(
        state,
        ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
    )
    .unwrap()
}
fn layout(app: &mut App) {
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    assert!(run_layout_pass(app, root, 900, 650, 1.0, true, None).is_some());
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
                    .and_then(|(v, _)| v.to_str().map(str::to_owned))
            })
            .unwrap()
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
        fixture.app.document_routes.sent[0].0,
        fixture.bindings[0].broker_route().unwrap()
    );
    fixture.app.publish_owned_documents();
    assert_eq!(
        fixture.app.document_routes.sent.len(),
        1,
        "unchanged heads do not enqueue again"
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
    fixture.app.publish_owned_documents();
    let (_, request) = fixture
        .app
        .document_routes
        .sent
        .iter()
        .find(|(route, _)| Some(route) == fixture.bindings[0].broker_route().as_ref())
        .unwrap();
    let CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Snapshot(data),
    } = request
    else {
        panic!("reconnect needs fresh snapshot")
    };
    let doc = automerge::AutoCommit::load(data).unwrap();
    assert_eq!(
        doc.get(ROOT, "offline").unwrap().unwrap().0.to_str(),
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
fn native_tile_history_discards_only_own_queue_and_polls_admitted_hidden_effect() {
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
        "A's unadmitted local intent is discarded"
    );
    assert_eq!(
        probes[1].1.get(),
        1,
        "B's queued live intent survives A's History input frame"
    );
    probes[0].2.set(true);
    app.poll_directed_invocations();
    assert_eq!(
        probes[0].3.get(),
        1,
        "A's already admitted effect still reaches terminal polling while hidden"
    );
}

#[test]
fn native_admitted_legacy_effect_cannot_poll_replacement_or_closed_owner() {
    for close in [false, true] {
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
        tile.bind_record(first, checkout.clone());
        let pointer = std::ptr::from_mut(tile.as_mut());
        let mut app = App::new_with_controlled_inputs(State::new(), tile);
        layout(&mut app);
        let actor = app.collaboration.local_author.0.clone();
        let address = tilo_actions::ActionAddress::new("retained", "export");
        app.invoke_resource_action(ResourceInvocation::new(&address, &actor))
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
        probe.finish.set(true);
        assert!(
            app.poll_directed_invocations(),
            "owner retirement terminates the admitted invocation truthfully"
        );
        assert_eq!(
            probe.completions.get(),
            0,
            "old operation cannot poll a replacement or closed owner"
        );
        assert!(app.actions.local_invocations.is_empty());
    }
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
            .to_str(),
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
        .read_checked(&checkout.id, |doc| doc.base_heads())
        .unwrap();
    app.layout.node_map.clear();
    app.bindings.clear();
    unsafe {
        (*pointer).bind_record(replacement.clone(), checkout.clone());
    }
    app.drain_document_routes();
    assert_eq!(
        replacement
            .read_checked(&checkout.id, |doc| doc.base_heads())
            .unwrap(),
        before
    );
    assert!(app.document_routes.owners.is_empty());
    assert!(
        app.document_sync_status()
            .unwrap()
            .contains("owner closed or changed")
    );
    assert_eq!(
        first
            .read_checked(&checkout.id, |doc| {
                use automerge::ReadDoc;
                (
                    doc.pending_ops(),
                    doc.get(ROOT, "draft")
                        .unwrap()
                        .and_then(|(value, _)| value.to_str().map(str::to_owned)),
                    doc.get(ROOT, "remote").unwrap().is_some(),
                )
            })
            .unwrap(),
        (0, Some("pending".into()), false),
        "owner replacement settles its local draft but never applies the queued remote snapshot"
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
                .map(|(value, _)| value.to_str().unwrap().to_owned())
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
                .to_str(),
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
                .map(|(value, _)| value.to_str().unwrap().to_owned())
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
        } = &sent[0].1
        else {
            panic!("first owned publication is snapshot");
        };
        let published = automerge::AutoCommit::load(data).unwrap();
        assert_eq!(
            published.get(ROOT, "later-D").unwrap().unwrap().0.to_str(),
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
