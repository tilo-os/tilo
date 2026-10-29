use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHost, runner_permissions, runner_probe_definition,
};
use crate::widget::{AppModel, ModelPath};
use crate::{
    State,
    headless::FrameHarness,
    widget::{BoundAppWidget, Widget},
};
use automerge::{ROOT, transaction::Transactable};

static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
        content_schema: None,
    }],
};

#[test]
fn managed_components_bind_shared_history_to_their_real_owner() {
    // phase: managed_bound_history_resolves_real_owner_instead_of_outer_state
    {
        let host = ProbeHost::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 904, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        let selected = binding
            .records
            .with_history(&binding.checkout.id, |doc, history| {
                doc.put(ROOT, "body", "bound first").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                let hash = doc.base_heads()[0];
                doc.put(ROOT, "body", "bound latest").unwrap();
                history.commit_action_authored(doc, None, None, Some(binding.author.clone()));
                format!(
                    "timeline-entry:{}",
                    crate::widget::history_group::hex16(
                        &crate::history_source::entry_id_from_hash(&hash.to_string())
                    )
                )
            })
            .unwrap();
        let root = Box::new(BoundAppWidget::new(component));
        let source =
            HistorySourceRegistration::owned("document", APP, root.history_owner().unwrap())
                .unwrap();
        let registration = HistoryRegistration::new(&BOUND, vec![source]).unwrap();
        let mut state = State::new();
        state.doc.put(ROOT, "body", "unrelated outer").unwrap();
        state.commit_action(None, None);
        let mut driver = FrameHarness::from_managed_root(
            state,
            ManagedRoot::new(root, registration),
            (900, 650),
        )
        .expect("the registered actual bound owner must be admitted");
        driver.click_named("history-open").unwrap();
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&selected),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        assert!(driver.targets().contains_key("history-preview"));
        let names = format!("{:?}", driver.a11y_nodes());
        assert!(
            names.contains("bound"),
            "preview reads bound content: {names}"
        );
        assert!(!names.contains("unrelated outer"));
    }
    // phase: managed_component_bundle_automatically_installs_shared_history_in_frame_runner
    {
        let (_host, component) = component_with_history();
        let mut driver = FrameHarness::from_component(
            component,
            (900, 650),
            crate::application::ComponentRunnerInputs {
                theme: crate::Theme::dark(),
                text_renderer: crate::font_catalog::fixture_renderer(),
            },
        )
        .unwrap();
        assert!(
            driver.targets().contains_key("history-open"),
            "managed component metadata follows its actual runner"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
    // phase: managed_component_tile_automatically_composes_shared_history
    {
        let (_host, component) = component_with_history();
        let root = crate::widget::TileHost::from_component(component);
        let mut driver = FrameHarness::new(State::new(), Box::new(root), (900, 650));
        assert!(
            driver.targets().contains_key("history-open"),
            "a managed tile retains the component's History bundle"
        );
        driver.click_named("history-open").unwrap();
        assert!(driver.targets().contains_key("activity_timeline"));
    }
}

pub(crate) static RESTORABLE: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.restore-fixture",
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Notebook",
        presentation: HistoryPresentation::Domain {
            create: super::host_tests::preview,
        },
        content_schema: Some(&crate::state::owned_restore_tests::SCHEMA),
    }],
};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn managed_review_confirmation_rechecks_authority_and_freshness() {
    // phase: managed_review_confirm_restores_exactly_one_authored_change_and_can_cancel
    {
        let mut state = State::new();
        state.doc.put(ROOT, "body", "old").unwrap();
        state.commit_action(None, None);
        let hash = state.doc.base_heads()[0];
        state.doc.put(ROOT, "body", "live").unwrap();
        state.commit_action(None, None);
        let before = state.doc.base_heads();
        let source = HistorySourceRegistration::HostDocument {
            scope: "document",
            resource: HistoryResource::new(
                "fixture",
                "restore-lifetime",
                RESTORABLE.app_id,
                crate::RecordId::for_tab(17),
            ),
        };
        let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
        let mut driver = FrameHarness::from_managed_root(
            state,
            ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
            (900, 650),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        let target = format!(
            "timeline-entry:{}",
            crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
                &hash.to_string()
            ))
        );
        driver
            .perform_semantic_named(
                "activity_timeline",
                Some(&target),
                &crate::a11y::Action::Activate,
            )
            .unwrap();
        driver
            .click_named("history-restore")
            .expect("registered content schema exposes reviewed restore");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Restore version")
        );
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.key("Escape");
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        driver.click_named("history-restore").unwrap();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
        assert!(driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "live"
        );
    }
    // phase: actual_confirmation_rechecks_unchanged_grant_expiry_replacement_and_actor_mapping
    {
        use crate::permissions::*;
        for case in ["expiry", "replacement", "actor"] {
            let mut driver = review_driver();
            driver.set_permission_ctx(grant(LOCAL_USER, RESTORABLE.app_id, Some(150)));
            driver.click_named("history-restore").unwrap();
            let before = driver.state_mut().doc.base_heads();
            match case {
                "expiry" => NOW.set(200),
                "replacement" => driver.set_permission_ctx(
                    crate::permissions::PermissionContext::stranger(LOCAL_USER),
                ),
                _ => driver.set_permission_ctx(grant("another-person", RESTORABLE.app_id, None)),
            }
            let confirm = driver
                .a11y_node_named("Restore version")
                .expect("real modal footer exports AT action");
            driver
                .perform_a11y_action(confirm, &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(driver.state_mut().doc.base_heads(), before, "{case}");
            assert!(
                driver
                    .inspection_json()
                    .unwrap()
                    .contains("no longer have permission"),
                "{case}"
            );
            assert!(driver.a11y_node_named("Refresh review").is_some());
        }
    }
    // phase: stale_review_requires_refresh_and_cancelled_footer_target_cannot_confirm
    {
        let mut driver = review_driver();
        driver.click_named("history-restore").unwrap();
        let old_confirm = driver.a11y_node_named("Restore version").unwrap();
        driver.key("Escape");
        assert!(
            driver
                .perform_a11y_action(old_confirm, &crate::a11y::Action::Activate)
                .is_err()
        );
        driver.click_named("history-restore").unwrap();
        driver
            .state_mut()
            .doc
            .put(ROOT, "body", "remote change")
            .unwrap();
        driver.state_mut().commit_action(None, None);
        let before = driver.state_mut().doc.base_heads();
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(driver.state_mut().doc.base_heads(), before);
        assert!(driver.a11y_node_named("Refresh review").is_some());
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            driver.state_mut().doc.base_heads(),
            before,
            "refresh itself does not write"
        );
        driver
            .click_named("history-restore-review.primary")
            .unwrap();
        assert_eq!(
            crate::state::owned_restore_tests::value(&driver.state_mut().doc),
            "old"
        );
        assert_eq!(driver.state_mut().doc.get_changes(&before).len(), 1);
    }
}

thread_local! { static NOW: std::cell::Cell<u64> = const { std::cell::Cell::new(100) }; }
fn current_time() -> Option<u64> {
    Some(NOW.get())
}
fn review_driver() -> FrameHarness {
    NOW.set(100);
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    let hash = state.doc.base_heads()[0];
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let source = HistorySourceRegistration::HostDocument {
        scope: "document",
        resource: HistoryResource::new(
            "fixture",
            "restore-lifetime",
            RESTORABLE.app_id,
            crate::RecordId::for_tab(18),
        ),
    };
    let registration = HistoryRegistration::new(&RESTORABLE, vec![source]).unwrap();
    let mut driver = FrameHarness::from_managed_root(
        state,
        ManagedRoot::new(Box::new(crate::widget::Column::new()), registration),
        (900, 650),
    )
    .unwrap();
    driver.set_history_clock(current_time);
    driver.click_named("history-open").unwrap();
    let target = format!(
        "timeline-entry:{}",
        crate::widget::history_group::hex16(&crate::history_source::entry_id_from_hash(
            &hash.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &crate::a11y::Action::Activate,
        )
        .unwrap();
    driver
}

#[test]
fn managed_review_dialog_drains_and_returns_to_retained_preview() {
    // Dialogs appear from their origin (4480e9f9b6): this test checks that
    // the review dialog rests once drawn, so motion reads at its end.
    let _settled = crate::motion::testing::settled();
    let mut driver = review_driver();
    assert!(driver.targets().contains_key("history-preview"));
    assert!(
        !driver.frame_output().needs_redraw,
        "selected preview stayed dirty after draw"
    );

    driver.click_named("history-restore").unwrap();
    assert!(
        driver
            .inspection_json()
            .unwrap()
            .contains("Restore version")
    );
    assert!(
        !driver.frame_output().needs_redraw,
        "review dialog stayed dirty after draw: {:?}",
        driver.frame_requesters()
    );
    assert!(!driver.poll_frame().unwrap());

    driver.key("Escape");
    assert!(driver.targets().contains_key("history-preview"));
    assert!(driver.inspection_json().unwrap().contains("Notebook: old"));
    assert!(
        !driver.frame_output().needs_redraw,
        "retained preview was not current after closing review"
    );
    assert!(!driver.poll_frame().unwrap());
}
fn grant(actor: &str, app: &str, until: Option<u64>) -> crate::permissions::PermissionContext {
    use crate::permissions::*;
    PermissionContext::granted(
        actor,
        &TreePath::root().app(app),
        policy::KindSet::ALL,
        until,
    )
    .at_unix_time(100)
}

pub(crate) fn component_with_history() -> (ProbeHost, crate::application::AppComponent) {
    let host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 950, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
}

const RETAINED_LIFECYCLE_OPERATION: &str = "fixture.retained-lifecycle";

#[derive(Clone)]
struct RetainedLifecycleHandles {
    emitter: std::rc::Rc<std::cell::RefCell<crate::widget::ActionEmitter<()>>>,
    queued: std::rc::Rc<std::cell::RefCell<Option<QueuedLifecycleResult>>>,
    admitted: std::rc::Rc<std::cell::Cell<Option<tilo_actions::InvocationId>>>,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
    accepts: std::rc::Rc<std::cell::Cell<usize>>,
    completions: std::rc::Rc<std::cell::Cell<usize>>,
    polls: std::rc::Rc<std::cell::Cell<usize>>,
    syncs: std::rc::Rc<std::cell::Cell<usize>>,
    updates: std::rc::Rc<std::cell::Cell<usize>>,
    events: std::rc::Rc<std::cell::Cell<usize>>,
    event_value: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    wrong_owner: std::rc::Rc<std::cell::Cell<usize>>,
    intent: std::rc::Rc<std::cell::RefCell<Option<crate::record::RecordCheckout>>>,
    last_owner: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    accepted: Accepted,
}

/// The results an owner accepted, each with the owner of the document it was
/// handed when it did.
type Accepted = std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>;

struct QueuedLifecycleResult {
    invocation: tilo_actions::InvocationId,
    value: String,
}

struct RetainedLifecycleProbe {
    id: &'static str,
    expected_owner: &'static str,
    enforce_result_owner: bool,
    streaming: bool,
    text: crate::widget::Text,
    handles: RetainedLifecycleHandles,
    ready: bool,
    observed: String,
}

fn increment(counter: &std::cell::Cell<usize>) {
    counter.set(counter.get().checked_add(1).unwrap());
}

fn retained_lifecycle_resource(id: &str) -> tilo_actions::ResourceSpec {
    tilo_actions::ResourceSpec::new(id, "fixture", "Retained lifecycle").with_action(
        tilo_actions::ActionSpec::operation(
            "receive",
            "Receive queued result",
            tilo_actions::OperationKind::Effect,
            RETAINED_LIFECYCLE_OPERATION,
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job),
    )
}

fn retained_lifecycle_probe(
    id: &'static str,
    expected_owner: &'static str,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    retained_lifecycle_probe_with_owner_policy(id, expected_owner, true, false)
}

fn retained_lifecycle_probe_with_owner_policy(
    id: &'static str,
    expected_owner: &'static str,
    enforce_result_owner: bool,
    streaming: bool,
) -> (RetainedLifecycleProbe, RetainedLifecycleHandles) {
    let emitter = std::rc::Rc::new(std::cell::RefCell::new(crate::widget::ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&retained_lifecycle_resource(id), "receive").unwrap(),
    )));
    let handles = RetainedLifecycleHandles {
        emitter,
        queued: std::rc::Rc::new(std::cell::RefCell::new(None)),
        admitted: std::rc::Rc::new(std::cell::Cell::new(None)),
        starts: std::rc::Rc::new(std::cell::Cell::new(0)),
        accepts: std::rc::Rc::new(std::cell::Cell::new(0)),
        completions: std::rc::Rc::new(std::cell::Cell::new(0)),
        polls: std::rc::Rc::new(std::cell::Cell::new(0)),
        syncs: std::rc::Rc::new(std::cell::Cell::new(0)),
        updates: std::rc::Rc::new(std::cell::Cell::new(0)),
        events: std::rc::Rc::new(std::cell::Cell::new(0)),
        event_value: std::rc::Rc::new(std::cell::RefCell::new(None)),
        wrong_owner: std::rc::Rc::new(std::cell::Cell::new(0)),
        intent: std::rc::Rc::new(std::cell::RefCell::new(None)),
        last_owner: std::rc::Rc::new(std::cell::RefCell::new(None)),
        accepted: Accepted::default(),
    };
    (
        RetainedLifecycleProbe {
            id,
            expected_owner,
            enforce_result_owner,
            streaming,
            text: crate::widget::Text::new("Retained lifecycle"),
            handles: handles.clone(),
            ready: false,
            observed: String::new(),
        },
        handles,
    )
}

crate::testing::test_app_widget!(
    RetainedLifecycleProbe,
    "Retained lifecycle",
    icon = crate::widget::IconDescriptor::Glyph("R")
);

impl Widget for RetainedLifecycleProbe {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::record::capability::RecordScoped,
        dyn crate::widget::ResourceOperations
    );

    crate::testing::delegate_widget_methods!(text: apply_layout, draw, get_rect);
    fn name(&self) -> &'static str {
        "RetainedLifecycleProbe"
    }
    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        use automerge::ReadDoc;
        let (doc, path) = crate::doc::document_ref(doc, path);
        increment(&self.handles.syncs);
        self.observed = self
            .handles
            .accepted
            .borrow()
            .last()
            .map(|(_, value)| value.clone())
            .or_else(|| {
                doc.get(path, "body")
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned))
            })
            .unwrap_or_default();
        let Some(result) = self.handles.queued.borrow_mut().take() else {
            return;
        };
        let actual_owner = doc
            .get(path, "owner")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        self.handles.last_owner.replace(actual_owner.clone());
        if (self.enforce_result_owner && actual_owner.as_deref() != Some(self.expected_owner))
            || self.handles.admitted.get() != Some(result.invocation)
        {
            increment(&self.handles.wrong_owner);
            *self.handles.queued.borrow_mut() = Some(result);
            return;
        }
        self.handles
            .accepted
            .borrow_mut()
            .push((actual_owner.unwrap_or_default(), result.value.clone()));
        self.observed = result.value;
        increment(&self.handles.accepts);
        self.ready = true;
    }
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        increment(&self.handles.updates);
        false
    }
    fn has_separate_document_owner(&self) -> bool {
        false
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Document)
                .with_name(self.id)
                .with_value(self.observed.clone()),
        )
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        increment(&self.handles.events);
        if let Some(value) = self.handles.event_value.borrow().as_deref() {
            ctx.state
                .document_mut()
                .put(ctx.path, "body", value)
                .unwrap();
        }
    }
}

impl crate::widget::ResourceOperations for RetainedLifecycleProbe {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[RETAINED_LIFECYCLE_OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        use automerge::ReadDoc;
        let Some(crate::host_operation::OperationRequest {
            invocation,
            type_id,
            ..
        }) = op.unscoped()
        else {
            return Ok(None);
        };
        if type_id != RETAINED_LIFECYCLE_OPERATION {
            return Ok(None);
        }
        let owner = doc
            .get(path, "owner")
            .map_err(|error| error.to_string())?
            .and_then(|(value, _)| value.as_str().map(str::to_owned));
        if owner.as_deref() != Some(self.expected_owner) {
            return Err("operation admitted against the wrong document owner".into());
        }
        increment(&self.handles.starts);
        self.handles.admitted.set(Some(invocation));
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &automerge::AutoCommit,
        _: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !self.ready || self.handles.admitted.get() != Some(invocation) {
            return None;
        }
        increment(&self.handles.polls);
        if self.streaming {
            return Some(tilo_actions::InvocationUpdate::Item {
                invocation,
                sequence: u64::try_from(self.handles.polls.get()).unwrap(),
                value: serde_json::json!(self.handles.polls.get()),
            });
        }
        self.ready = false;
        self.handles.admitted.set(None);
        increment(&self.handles.completions);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::record::capability::RecordScoped for RetainedLifecycleProbe {
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.handles.intent.borrow().clone()
    }
}

impl crate::resources::PublishesResources for RetainedLifecycleProbe {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![retained_lifecycle_resource(self.id)]
    }
}

impl crate::widget::controls::ActionControls for RetainedLifecycleProbe {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.handles.emitter.borrow_mut().take()
    }
}

#[test]
fn hidden_canonical_component_updates_once_per_ordinary_host_pass() {
    let host = ProbeHost::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 717, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let (probe, handles) = retained_lifecycle_probe("canonical-component", "component");
    component.root = Some(Box::new(probe));
    component.register_document_history(&BOUND).unwrap();
    let root = crate::widget::TileHost::from_component(component);
    let mut driver = FrameHarness::new(State::new(), Box::new(root), (900, 650));

    let live = (handles.updates.get(), handles.syncs.get());
    driver.relayout();
    let live_delta = (handles.updates.get() - live.0, handles.syncs.get() - live.1);
    assert_eq!(live_delta.0, 1);
    assert!(live_delta.1 > 0);

    driver.click_named("history-open").unwrap();
    let hidden = (handles.updates.get(), handles.syncs.get());
    driver.relayout();
    assert_eq!(handles.updates.get() - hidden.0, 1);
    assert_eq!(handles.syncs.get() - hidden.1, live_delta.1);
}

#[allow(
    clippy::too_many_lines,
    reason = "The assertion helper compares queued and refused bindings across one custody transition."
)]
#[path = "readiness_tests.rs"]
mod readiness_tests;
