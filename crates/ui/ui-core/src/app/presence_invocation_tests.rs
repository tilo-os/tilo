//! Delivered invocations resolve against the invoker's own presence view,
//! rebuilt at invoke time; the invoker comes from the host-set delivery, and a
//! remote presence may only change its own view.

use super::*;
use tilo_ui_base::collaboration::CollabEvent;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_actions::{ActionSpec, ExecutionLocus, OperationKind, PresenceId, ResourceSpec};

const MARK_TYPE: &str = "test.presence-mark";
const VIEW_TYPE: &str = "test.presence-view";

/// Writes which view built it into the document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Mark(String);

impl tilo_actions::Action for Mark {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        doc.put(&ROOT, "who", self.0.as_str())?;
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

#[derive(Default)]
struct Probe {
    joined: Vec<PresenceId>,
    left: Vec<PresenceId>,
    host: bool,
    labels: HashMap<PresenceId, String>,
    carets: HashMap<PresenceId, usize>,
    /// Require this record marker in the document a presence tree is built
    /// against (proves a bound component uses its own record document).
    require_marker: Option<&'static str>,
    /// (type, the presence current while the View op ran)
    view_ops: Vec<(String, Option<PresenceId>)>,
}

/// A two-view widget: its own view, plus one per joined presence.
struct TwoViews(Rc<RefCell<Probe>>);

fn tree(view: &str) -> Vec<ResourceSpec> {
    vec![
        ResourceSpec::new("test.doc", "test.document", "Document")
            .with_action(ActionSpec::immediate_encoded(
                "mark",
                "Mark",
                ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction::encode(MARK_TYPE, &Mark(view.into())).unwrap(),
            ))
            .with_action(
                ActionSpec::operation("move-caret", "Move", OperationKind::View, VIEW_TYPE, &())
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view())
                    .presence_view(),
            )
            .with_action(
                ActionSpec::operation("zoom", "Zoom", OperationKind::View, VIEW_TYPE, &())
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
            ),
    ]
}

impl Widget for TwoViews {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::presence_topology::Presences,
        dyn tilo_ui_base::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );

    crate::testing::inert_widget_methods!(leaf_layout, name = "two-views");
}

impl tilo_ui_base::widget::ResourceOperations for TwoViews {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[VIEW_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        let tilo_ui_base::host_operation::OperationRequest { type_id, .. } = op.request();
        let presence = crate::invocation_scope::current_presence();
        let mut probe = self.0.borrow_mut();
        if let Some(presence) = &presence {
            *probe.carets.entry(presence.clone()).or_default() += 1;
        }
        probe.view_ops.push((type_id.to_owned(), presence));
        Ok(Some(tilo_ui_base::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl tilo_ui_base::widget::resources::PublishesResources for TwoViews {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<ResourceSpec> {
        tree(if self.0.borrow().host {
            "schema"
        } else {
            "own"
        })
    }
    fn resources_for(
        &self,
        presence: Option<&PresenceId>,
        doc: &automerge::AutoCommit,
    ) -> Vec<ResourceSpec> {
        match presence {
            None => self.resources(doc),
            Some(presence) if self.0.borrow().joined.contains(presence) => {
                if let Some(marker) = self.0.borrow().require_marker
                    && !matches!(
                        doc.get(&ROOT, "marker"),
                        Ok(Some((value, _))) if value.as_str() == Some(marker)
                    )
                {
                    return Vec::new();
                }
                tree(presence.as_str())
            }
            Some(_) => Vec::new(),
        }
    }
}

impl tilo_ui_base::widget::presence_topology::Presences for TwoViews {
    fn join_presence(&mut self, presence: &PresenceId) -> bool {
        let mut probe = self.0.borrow_mut();
        if probe.joined.contains(presence) {
            return false;
        }
        probe.joined.push(presence.clone());
        true
    }
    fn leave_presence(&mut self, presence: &PresenceId) {
        let mut probe = self.0.borrow_mut();
        probe.joined.retain(|joined| joined != presence);
        probe.left.push(presence.clone());
    }
    fn set_presence_label(&mut self, presence: &PresenceId, label: &str) {
        self.0
            .borrow_mut()
            .labels
            .insert(presence.clone(), label.to_owned());
    }
    fn presences(&self, _: &automerge::AutoCommit) -> Vec<tilo_ui_base::widget::ViewPresence> {
        let probe = self.0.borrow();
        probe
            .joined
            .iter()
            .map(|presence| tilo_ui_base::widget::ViewPresence {
                presence: presence.clone(),
                label: probe.labels.get(presence).cloned(),
                key: tilo_ui_base::PresenceKey::Widget {
                    path: vec![tilo_ui_base::PathSegment::Field("wp_block".into())],
                },
                payload: tilo_ui_base::PresencePayload::Caret {
                    pos: probe.carets.get(presence).copied().unwrap_or(0),
                },
            })
            .collect()
    }
    fn set_remote_only(&mut self, host: bool) {
        self.0.borrow_mut().host = host;
    }
}

fn app() -> (App, Rc<RefCell<Probe>>) {
    let probe = Rc::new(RefCell::new(Probe::default()));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(TwoViews(probe.clone())));
    // Deliveries below are host-set fixture inputs on the legacy peer boundary.
    app.collaboration.manager = super::semantic_tree_app_tests::admitted_delivery_manager();
    app.action_registry_mut().register::<Mark>(MARK_TYPE);
    app.init_layout();
    (app, probe)
}

fn presence() -> PresenceId {
    PresenceId::new("agent-alpha-view").unwrap()
}

fn deliver(
    app: &mut App,
    id: u64,
    action: &str,
    actor: &tilo_actions::Actor,
    context: Option<tilo_actions::ActionContext>,
) {
    app.apply_collab_event_for_test(CollabEvent::Invocation {
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        id,
        resource: "test.doc".into(),
        action: action.into(),
        argument: None,
        actor: actor.clone(),
        context: context.map(Box::new),
    });
}

fn who(app: &App) -> Option<String> {
    match app.state.doc.get(&ROOT, "who") {
        Ok(Some((value, _))) => value.into_string().ok(),
        _ => None,
    }
}

fn host_context(
    actor: &tilo_actions::Acting,
    presence: Option<PresenceId>,
) -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::default()
        .with_group("11111111-1111-4111-8111-111111111111", "task", "task")
        .with_host_actor(actor, presence)
}

crate::testing::test_app_widget!(TwoViews, "Two views");

thread_local! {
    static BOUND_PROBE: RefCell<Option<Rc<RefCell<Probe>>>> = const { RefCell::new(None) };
}

#[allow(clippy::needless_pass_by_value, reason = "factory contract")]
fn bound_factory(
    mut context: crate::application::ProcessContext,
    target: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    if matches!(target, crate::application::LaunchTarget::New) {
        context
            .initialize_new_resource(|doc| {
                doc.put(ROOT, "marker", "bound-record")
                    .map_err(|e| e.to_string())
            })
            .map_err(crate::launch::LaunchError::Record)?;
    }
    let probe = BOUND_PROBE.with_borrow(|probe| probe.clone().expect("probe installed"));
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<Mark>(MARK_TYPE);
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(TwoViews(probe)),
        registry,
    ))
}

tilo_test_env::scenario! {
    fn a_presences_invocations_reach_only_its_own_view;

    phase delivered_invocation_resolves_against_the_invokers_own_view;
    {
        let (mut app, probe) = app();
        deliver(
            &mut app,
            1,
            "mark",
            &tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "guest")),
            Some(host_context(&tilo_actions::Acting::agent("scribe", "guest"), Some(presence()))),
        );
        assert_eq!(
            who(&app).as_deref(),
            Some(presence().as_str()),
            "built from the agent's presence tree, rebuilt at invoke time"
        );
        assert_eq!(
            probe.borrow().joined,
            vec![presence()],
            "first use creates its view"
        );

        // Durable attribution carries the same host-set invoker.
        let changes = app.state.doc.get_changes(&[]);
        let change = changes.last().expect("committed change");
        let context = change
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .expect("attributed change");
        assert_eq!(
            context.invoker,
            Some(tilo_actions::ActionInvoker::agent("scribe", "guest").with_presence(presence()))
        );

        // Without a presence the same address resolves in this window's view.
        deliver(&mut app, 2, "mark", &tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "guest")), None);
        assert_eq!(who(&app).as_deref(), Some("own"));
    }

    phase a_context_naming_another_actor_never_routes_to_a_presence_view;
    {
        let (mut app, probe) = app();
        probe.borrow_mut().joined.push(presence());
        // The delivery's actor is authoritative; a context whose invoker names a
        // different actor (forged or stale) is not a route to anybody's view.
        deliver(
            &mut app,
            3,
            "mark",
            &tilo_actions::Actor::Member(tilo_actions::MemberId::person("guest")),
            Some(host_context(&tilo_actions::Acting::agent("scribe", "guest"), Some(presence()))),
        );
        assert_eq!(who(&app).as_deref(), Some("own"));
        let changes = app.state.doc.get_changes(&[]);
        let change = changes.last().expect("committed change");
        let context = change
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .expect("attributed change");
        assert_eq!(
            context.invoker,
            Some(tilo_actions::ActionInvoker::person("guest")),
            "history names the delivered actor, never the envelope's"
        );
    }

    phase a_remote_presence_may_only_change_its_own_view;
    {
        let (mut app, probe) = app();
        deliver(
            &mut app,
            4,
            "move-caret",
            &tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "guest")),
            Some(host_context(&tilo_actions::Acting::agent("scribe", "guest"), Some(presence()))),
        );
        assert_eq!(
            probe.borrow().view_ops,
            vec![(VIEW_TYPE.to_owned(), Some(presence()))],
            "a presence-view op runs inside the invoker's scope"
        );
        // An untagged View op would move this window's private view.
        deliver(
            &mut app,
            5,
            "zoom",
            &tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "guest")),
            Some(host_context(&tilo_actions::Acting::agent("scribe", "guest"), Some(presence()))),
        );
        // A remote actor without its own presence cannot move any view.
        deliver(&mut app, 6, "move-caret", &tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe", "guest")), None);
        assert_eq!(
            probe.borrow().view_ops.len(),
            1,
            "both refused as private_view"
        );
    }

    phase presence_lifecycle_and_host_mode_reach_the_widget_tree;
    {
        let (mut app, probe) = app();
        assert!(app.join_presence(&presence()));
        assert!(!app.join_presence(&presence()), "idempotent");
        app.leave_presence(&presence());
        assert_eq!(probe.borrow().left, vec![presence()]);
        assert!(probe.borrow().joined.is_empty());
        app.set_remote_only(true);
        assert!(probe.borrow().host);
        assert!(
            tilo_actions::api::survey(&app.collect_resources())
                .iter()
                .all(|descriptor| descriptor.address.resource == "test.doc"),
        );
    }

    phase a_presence_is_reported_with_its_label;
    {
        let (mut app, _probe) = app();
        assert!(app.presences().is_empty());
        app.join_presence(&presence());
        app.set_presence_label(&presence(), "Scribe");
        let shown = app.presences();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].presence, presence());
        assert_eq!(shown[0].label.as_deref(), Some("Scribe"));
        app.leave_presence(&presence());
        assert!(app.presences().is_empty());
    }

    phase a_bound_component_builds_presence_trees_against_its_own_record;
    {
        let probe = Rc::new(RefCell::new(Probe {
            require_marker: Some("bound-record"),
            ..Probe::default()
        }));
        BOUND_PROBE.with_borrow_mut(|slot| *slot = Some(probe.clone()));
        let mut owners = crate::resource::ResourceOwners::in_memory();
        let records = owners
            .open("dev.tilo.two-views", "00000000-0000-0000-0000-000000000009")
            .unwrap();
        let views = Rc::new(
            crate::application_view::InstanceViewStore::in_memory(
                "dev.tilo.two-views",
                "D",
                "00000000-0000-0000-0000-000000000009",
            )
            .unwrap(),
        );
        let definition = crate::application::ApplicationDefinition {
            app_id: "dev.tilo.two-views".into(),
            title: "Two views".into(),
            icon: crate::widget::IconDescriptor::Glyph("T"),
            resource_type: crate::resource::ResourceType::new("two-views"),
            required: vec![],
            optional: vec![],
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: bound_factory,
        };
        let target = crate::application::LaunchTarget::New;
        let context = crate::application::ProcessContext::prepare(
            &definition,
            crate::application::ProcessContextInputs {
                records: records.clone(),
                views,
                device_id: "D".into(),
                instance_id: 1,
                permissions: tilo_ui_base::permissions::SharedPermissions::new(
                    tilo_ui_base::permissions::PermissionContext::permissive(),
                ),
                author: tilo_ui_base::permissions::MemberId::person("fixture-user"),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )
        .unwrap();
        let component = definition
            .instantiate(context, target, crate::application::RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        let column = crate::widget::Column::new()
            .push(Box::new(crate::widget::TileHost::from_component(component)));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(column));
        app.init_layout();
        assert!(app.join_presence(&presence()), "join reaches the bound root");

        // The boundary forwards a presence tree through its own record
        // document; built on the outer App document (no marker) it would be empty,
        // and the default would fall back to the boundary's own (schema) view.
        fn bound_presence_tree(
            widget: &dyn Widget,
            doc: &automerge::AutoCommit,
            presence: &PresenceId,
            found: &mut Option<Vec<ResourceSpec>>,
        ) {
            if found.is_none() && widget.name() == "BoundComponent" {
                *found = Some(tilo_ui_base::widget::resources::resources_for(
                    widget,
                    Some(presence),
                    doc,
                ));
            }
            widget.for_each_child(&mut |child| bound_presence_tree(child, doc, presence, found));
        }
        let mut resources = None;
        bound_presence_tree(
            app.root_widget.as_ref(),
            &app.state.doc,
            &presence(),
            &mut resources,
        );
        assert!(resources.is_some(), "a bound component boundary");
        // Resolution of a component target also rebuilds the invoker's tree.
        let snapshots = {
            let _scope = crate::invocation_scope::InvokerScope::enter(
                tilo_actions::ActionInvoker::agent("scribe", "guest").with_presence(presence()),
            );
            crate::bound_dispatch::discover_bound_resources(
                app.root_widget.as_ref(),
                &app.state.doc,
                Some(&tilo_actions::ActionAddress::new("test.doc", "mark")),
            )
            .unwrap()
        };
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.binding.same_component(&binding))
            .expect("component snapshot");
        let spec = tilo_actions::api::resolve(
            snapshot.resources(),
            &tilo_actions::ActionAddress::new("test.doc", "mark"),
        )
        .expect("presence resolves mark");
        assert_eq!(
            spec.fixed_invocation_encoding().unwrap().payload,
            tilo_actions::EncodedAction::encode(MARK_TYPE, &Mark(presence().as_str().into()))
                .unwrap()
                .payload,
            "not this window's own view"
        );
        BOUND_PROBE.with_borrow_mut(|slot| *slot = None);
    }
}
