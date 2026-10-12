//! Delivered invocations resolve against the invoker's own participant view,
//! rebuilt at invoke time; the invoker comes from the host-set delivery, and a
//! remote participant may only change its own view.

use super::*;
use crate::collaboration::CollabEvent;
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_actions::{ActionSpec, ExecutionLocus, OperationKind, ParticipantId, Resource};

const MARK_TYPE: &str = "test.participant-mark";
const VIEW_TYPE: &str = "test.participant-view";

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
    joined: Vec<ParticipantId>,
    left: Vec<ParticipantId>,
    host: bool,
    labels: HashMap<ParticipantId, String>,
    carets: HashMap<ParticipantId, usize>,
    /// Require this record marker in the document a participant tree is built
    /// against (proves a bound component uses its own record document).
    require_marker: Option<&'static str>,
    /// (type, the participant current while the View op ran)
    view_ops: Vec<(String, Option<ParticipantId>)>,
}

/// A two-view widget: its own view, plus one per joined participant.
struct TwoViews(Rc<RefCell<Probe>>);

fn tree(view: &str) -> Vec<Resource> {
    vec![
        Resource::new("test.doc", "test.document", "Document")
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
                    .participant_view(),
            )
            .with_action(
                ActionSpec::operation("zoom", "Zoom", OperationKind::View, VIEW_TYPE, &())
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view()),
            ),
    ]
}

impl Widget for TwoViews {
    crate::provides!(
        dyn crate::collaboration::participants::Participants,
        dyn crate::resources::PublishesResources
    );

    crate::testing::inert_widget_methods!(leaf_layout, name = "two-views");
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[VIEW_TYPE]
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::host_operation::OperationRequest { type_id, .. }) = op.unscoped() else {
            return Ok(None);
        };
        let participant = crate::invocation_scope::current_participant();
        let mut probe = self.0.borrow_mut();
        if let Some(participant) = &participant {
            *probe.carets.entry(participant.clone()).or_default() += 1;
        }
        probe.view_ops.push((type_id.to_owned(), participant));
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::resources::PublishesResources for TwoViews {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<Resource> {
        tree(if self.0.borrow().host {
            "schema"
        } else {
            "own"
        })
    }
    fn resources_for(
        &self,
        participant: Option<&ParticipantId>,
        doc: &automerge::AutoCommit,
    ) -> Vec<Resource> {
        match participant {
            None => self.resources(doc),
            Some(participant) if self.0.borrow().joined.contains(participant) => {
                if let Some(marker) = self.0.borrow().require_marker
                    && !matches!(
                        doc.get(&ROOT, "marker"),
                        Ok(Some((value, _))) if value.as_str() == Some(marker)
                    )
                {
                    return Vec::new();
                }
                tree(participant.as_str())
            }
            Some(_) => Vec::new(),
        }
    }
}

impl crate::collaboration::participants::Participants for TwoViews {
    fn join_participant(&mut self, participant: &ParticipantId) -> bool {
        let mut probe = self.0.borrow_mut();
        if probe.joined.contains(participant) {
            return false;
        }
        probe.joined.push(participant.clone());
        true
    }
    fn leave_participant(&mut self, participant: &ParticipantId) {
        let mut probe = self.0.borrow_mut();
        probe.joined.retain(|joined| joined != participant);
        probe.left.push(participant.clone());
    }
    fn set_participant_label(&mut self, participant: &ParticipantId, label: &str) {
        self.0
            .borrow_mut()
            .labels
            .insert(participant.clone(), label.to_owned());
    }
    fn participant_presence(
        &self,
        _: &automerge::AutoCommit,
    ) -> Vec<crate::widget::ParticipantPresence> {
        let probe = self.0.borrow();
        probe
            .joined
            .iter()
            .map(|participant| crate::widget::ParticipantPresence {
                participant: participant.clone(),
                label: probe.labels.get(participant).cloned(),
                key: crate::PresenceKey::Widget {
                    path: vec![crate::PathSegment::Field("wp_block".into())],
                },
                payload: crate::PresencePayload::Caret {
                    pos: probe.carets.get(participant).copied().unwrap_or(0),
                },
            })
            .collect()
    }
    fn set_participant_host(&mut self, host: bool) {
        self.0.borrow_mut().host = host;
    }
}

fn app() -> (App, Rc<RefCell<Probe>>) {
    let probe = Rc::new(RefCell::new(Probe::default()));
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(TwoViews(probe.clone())));
    // Deliveries below are host-set fixture inputs on the legacy peer seam.
    app.collaboration.manager = super::semantic_tree_app_tests::admitted_delivery_manager();
    app.action_registry_mut().register::<Mark>(MARK_TYPE);
    app.init_layout();
    (app, probe)
}

fn task() -> ParticipantId {
    ParticipantId::for_agent_task("11111111-1111-4111-8111-111111111111").unwrap()
}

fn deliver(
    app: &mut App,
    id: u64,
    action: &str,
    actor: &str,
    context: Option<tilo_actions::ActionContext>,
) {
    app.apply_collab_event_for_test(CollabEvent::Invocation {
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        id,
        resource: "test.doc".into(),
        action: action.into(),
        argument: None,
        actor: actor.into(),
        context: context.map(Box::new),
    });
}

fn who(app: &App) -> Option<String> {
    match app.state.doc.get(&ROOT, "who") {
        Ok(Some((value, _))) => value.into_string().ok(),
        _ => None,
    }
}

fn host_context(actor: &str, participant: Option<ParticipantId>) -> tilo_actions::ActionContext {
    tilo_actions::ActionContext::default()
        .with_group("11111111-1111-4111-8111-111111111111", "task", "task")
        .with_host_invoker(actor, participant)
}

#[test]
fn delivered_invocation_resolves_against_the_invokers_own_view() {
    let (mut app, probe) = app();
    deliver(
        &mut app,
        1,
        "mark",
        "agent:scribe",
        Some(host_context("agent:scribe", Some(task()))),
    );
    assert_eq!(
        who(&app).as_deref(),
        Some(task().as_str()),
        "built from the agent's participant tree, rebuilt at invoke time"
    );
    assert_eq!(
        probe.borrow().joined,
        vec![task()],
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
        Some(tilo_actions::ActionInvoker::actor("agent:scribe").with_participant(task()))
    );

    // Without a participant the same address resolves in this window's view.
    deliver(&mut app, 2, "mark", "agent:scribe", None);
    assert_eq!(who(&app).as_deref(), Some("own"));
}

#[test]
fn a_context_naming_another_actor_never_routes_to_a_participant_view() {
    let (mut app, probe) = app();
    probe.borrow_mut().joined.push(task());
    // The delivery's actor is authoritative; a context whose invoker names a
    // different actor (forged or stale) is not a route to anybody's view.
    deliver(
        &mut app,
        3,
        "mark",
        "person:guest",
        Some(host_context("agent:scribe", Some(task()))),
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
        Some(tilo_actions::ActionInvoker::actor("person:guest")),
        "history names the delivered actor, never the envelope's"
    );
}

#[test]
fn a_remote_participant_may_only_change_its_own_view() {
    let (mut app, probe) = app();
    deliver(
        &mut app,
        4,
        "move-caret",
        "agent:scribe",
        Some(host_context("agent:scribe", Some(task()))),
    );
    assert_eq!(
        probe.borrow().view_ops,
        vec![(VIEW_TYPE.to_owned(), Some(task()))],
        "a participant-view op runs inside the invoker's scope"
    );
    // An untagged View op would move this window's private view.
    deliver(
        &mut app,
        5,
        "zoom",
        "agent:scribe",
        Some(host_context("agent:scribe", Some(task()))),
    );
    // A remote actor without its own participant cannot move any view.
    deliver(&mut app, 6, "move-caret", "agent:scribe", None);
    assert_eq!(
        probe.borrow().view_ops.len(),
        1,
        "both refused as private_view"
    );
}

#[test]
fn participant_lifecycle_and_host_mode_reach_the_widget_tree() {
    let (mut app, probe) = app();
    assert!(app.join_participant(&task()));
    assert!(!app.join_participant(&task()), "idempotent");
    app.leave_participant(&task());
    assert_eq!(probe.borrow().left, vec![task()]);
    assert!(probe.borrow().joined.is_empty());
    app.set_participant_host(true);
    assert!(probe.borrow().host);
    assert!(
        tilo_actions::door::survey(&app.collect_resources())
            .iter()
            .all(|descriptor| descriptor.address.resource == "test.doc"),
    );
}

#[test]
fn participant_presence_is_reported_with_its_label() {
    let (mut app, _probe) = app();
    assert!(app.participant_presence().is_empty());
    app.join_participant(&task());
    app.set_participant_label(&task(), "Scribe");
    let presence = app.participant_presence();
    assert_eq!(presence.len(), 1);
    assert_eq!(presence[0].participant, task());
    assert_eq!(presence[0].label.as_deref(), Some("Scribe"));
    app.leave_participant(&task());
    assert!(app.participant_presence().is_empty());
}

#[test]
fn a_participant_view_op_is_re_emitted_as_a_presence_change() {
    let (mut app, _probe) = app();
    app.join_participant(&task());
    let mut published = std::collections::BTreeMap::new();
    let joined = crate::host_worker::participant_presence_changes(
        &mut published,
        app.participant_presence(),
    );
    assert!(matches!(
        joined.as_slice(),
        [crate::host_worker::ParticipantPresenceUpdate::Changed(p)]
            if p.payload == crate::PresencePayload::Caret { pos: 0 }
    ));
    assert!(
        crate::host_worker::participant_presence_changes(
            &mut published,
            app.participant_presence()
        )
        .is_empty()
    );
    deliver(
        &mut app,
        7,
        "move-caret",
        "agent:scribe",
        Some(host_context("agent:scribe", Some(task()))),
    );
    let moved = crate::host_worker::participant_presence_changes(
        &mut published,
        app.participant_presence(),
    );
    assert!(
        matches!(
            moved.as_slice(),
            [crate::host_worker::ParticipantPresenceUpdate::Changed(p)]
                if p.participant == task() && p.payload == crate::PresencePayload::Caret { pos: 1 }
        ),
        "the moved caret is re-emitted after the invocation: {moved:?}"
    );
}

crate::testing::test_app_widget!(TwoViews, "Two views");

thread_local! {
    static BOUND_PROBE: RefCell<Option<Rc<RefCell<Probe>>>> = const { RefCell::new(None) };
}

#[allow(clippy::needless_pass_by_value, reason = "factory contract")]
fn bound_factory(
    mut context: crate::application::HostContext,
    target: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    if matches!(target, crate::application::LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
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

#[test]
fn a_bound_component_builds_participant_trees_against_its_own_record() {
    let probe = Rc::new(RefCell::new(Probe {
        require_marker: Some("bound-record"),
        ..Probe::default()
    }));
    BOUND_PROBE.with_borrow_mut(|slot| *slot = Some(probe.clone()));
    let temp = tempfile::tempdir().unwrap();
    let mut owners = crate::record::RecordOwners::new(temp.path().into());
    let records = owners
        .open("dev.tilo.two-views", "00000000-0000-0000-0000-000000000009")
        .unwrap();
    let views = Rc::new(
        crate::application_view::InstanceViewStore::new(
            temp.path().into(),
            "D",
            "00000000-0000-0000-0000-000000000009",
        )
        .unwrap(),
    );
    let definition = crate::application::ApplicationDefinition {
        app_id: "dev.tilo.two-views",
        title: "Two views",
        icon: crate::widget::IconDescriptor::Glyph("T"),
        record_kind: crate::record::RecordKind::new("two-views"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
        create: bound_factory,
    };
    let target = crate::application::LaunchTarget::New;
    let context = crate::application::HostContext::prepare(
        &definition,
        crate::application::HostContextInputs {
            records: records.clone(),
            views,
            device_id: "D".into(),
            instance_id: 1,
            permissions: Rc::new(RefCell::new(
                crate::permissions::PermissionContext::permissive(),
            )),
            author: crate::permissions::UserId("fixture-user".into()),
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
    assert!(app.join_participant(&task()), "join reaches the bound root");

    // The boundary forwards a participant tree through its own record
    // document; built on the outer App document (no marker) it would be empty,
    // and the default would fall back to the boundary's own (schema) view.
    fn bound_participant_tree(
        widget: &dyn Widget,
        doc: &automerge::AutoCommit,
        participant: &ParticipantId,
        found: &mut Option<Vec<Resource>>,
    ) {
        if found.is_none() && widget.name() == "BoundComponent" {
            *found = Some(crate::resources::resources_for(
                widget,
                Some(participant),
                doc,
            ));
        }
        widget.for_each_child(&mut |child| bound_participant_tree(child, doc, participant, found));
    }
    let mut resources = None;
    bound_participant_tree(
        app.root_widget.as_ref(),
        &app.state.doc,
        &task(),
        &mut resources,
    );
    assert!(resources.is_some(), "a bound component boundary");
    // Resolution of a component target also rebuilds the invoker's tree.
    let snapshots = {
        let _scope = crate::invocation_scope::InvokerScope::enter(
            tilo_actions::ActionInvoker::actor("agent:scribe").with_participant(task()),
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
    let spec = tilo_actions::door::resolve(
        snapshot.resources(),
        &tilo_actions::ActionAddress::new("test.doc", "mark"),
    )
    .expect("participant resolves mark");
    assert_eq!(
        spec.fixed_invocation_encoding().unwrap().payload,
        tilo_actions::EncodedAction::encode(MARK_TYPE, &Mark(task().as_str().into()))
            .unwrap()
            .payload,
        "not this window's own view"
    );
    BOUND_PROBE.with_borrow_mut(|slot| *slot = None);
}
