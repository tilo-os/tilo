//! Real retained controls and owner operation polling exercise the host boundary.
use super::host_tests::registration;
use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::{
    State,
    headless::FrameHarness,
    widget::{ActionEmitter, Text, Widget},
};
use crate::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use automerge::{AutoCommit, ObjId, ROOT, transaction::Transactable};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use taffy::{NodeId, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;

fn resource() -> tilo_actions::Resource {
    resource_for("live", "fixture.export")
}
fn resource_for(id: &str, operation: &str) -> tilo_actions::Resource {
    let target = id.to_owned();
    let handler = operation.to_owned();
    tilo_actions::Resource::new(id, "fixture", "Live owner").with_action(
        tilo_actions::ActionSpec::operation(
            "export",
            "Export",
            tilo_actions::OperationKind::Effect,
            operation,
            &(),
        )
        .unwrap()
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job)
        .with_continuation_identity(move |query| match query {
            tilo_actions::ContinuationQuery::Exact(_) => {
                Some(tilo_actions::ContinuationEvidence::Exact {
                    target: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.target",
                        [target.as_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.handler",
                        [handler.as_bytes()],
                    ),
                })
            }
            tilo_actions::ContinuationQuery::Draft => {
                Some(tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.domain",
                        [target.as_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.handler",
                        [handler.as_bytes()],
                    ),
                })
            }
        }),
    )
}
fn prompted_resource_for(
    id: &str,
    operation: &str,
    confirmation: tilo_actions::ConfirmationPolicy,
) -> tilo_actions::Resource {
    let mut spec = tilo_actions::ActionSpec::prompted_operation(
        "export",
        "Export",
        "Payload",
        "payload",
        tilo_actions::OperationKind::Effect,
        operation,
        {
            let operation = operation.to_owned();
            move |value| tilo_actions::EncodedAction::encode_operation(&operation, &value).unwrap()
        },
    );
    spec.type_mut().contract.confirmation = confirmation;
    let target = id.to_owned();
    let handler = operation.to_owned();
    let spec = spec
        .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
        .returns(tilo_actions::ResultSpec::Job)
        .with_continuation_identity(move |query| match query {
            tilo_actions::ContinuationQuery::Exact(_) => {
                Some(tilo_actions::ContinuationEvidence::Exact {
                    target: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.target",
                        [target.as_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.handler",
                        [handler.as_bytes()],
                    ),
                })
            }
            tilo_actions::ContinuationQuery::Draft => {
                Some(tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.domain",
                        [target.as_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.fixture.handler",
                        [handler.as_bytes()],
                    ),
                })
            }
        });
    tilo_actions::Resource::new(id, "fixture", "Live owner").with_action(spec)
}

struct MutableContinuationIdentity {
    target: Cell<u64>,
    handler: Cell<u64>,
    supported: Cell<bool>,
    duplicate_resource: Cell<bool>,
    /// The action is declared housekeeping.
    housekeeping: Cell<bool>,
}

fn mutable_identity_resources(
    id: &str,
    operation: &str,
    identity: &MutableContinuationIdentity,
) -> Vec<tilo_actions::Resource> {
    let target = identity.target.get();
    let handler = identity.handler.get();
    let supported = identity.supported.get();
    let mut spec = tilo_actions::ActionSpec::operation(
        "export",
        "Export",
        tilo_actions::OperationKind::Effect,
        operation,
        &(),
    )
    .unwrap()
    .execution(tilo_actions::ExecutionMode::Background { cancellable: false })
    .returns(tilo_actions::ResultSpec::Job)
    .with_continuation_identity(move |query| {
        supported.then(|| match query {
            tilo_actions::ContinuationQuery::Exact(_) => {
                tilo_actions::ContinuationEvidence::Exact {
                    target: tilo_actions::ContinuationKey::new(
                        "ui.history.mutable.target",
                        [target.to_be_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "ui.history.mutable.handler",
                        [handler.to_be_bytes()],
                    ),
                }
            }
            tilo_actions::ContinuationQuery::Draft => tilo_actions::ContinuationEvidence::Draft {
                resolution_domain: tilo_actions::ContinuationKey::new(
                    "ui.history.mutable.domain",
                    [target.to_be_bytes()],
                ),
                handler: tilo_actions::ContinuationKey::new(
                    "ui.history.mutable.handler",
                    [handler.to_be_bytes()],
                ),
            },
        })
    });
    if identity.housekeeping.get() {
        spec.type_mut()
            .contract
            .tags
            .push(tilo_actions::HOUSEKEEPING_TAG.to_owned());
    }
    let resource = tilo_actions::Resource::new(id, "fixture", "Live owner").with_action(spec);
    if identity.duplicate_resource.get() {
        vec![
            resource,
            tilo_actions::Resource::new(id, "fixture", "Duplicate declaration"),
        ]
    } else {
        vec![resource]
    }
}

struct LiveOwner {
    resource_id: &'static str,
    type_id: &'static str,
    text: Text,
    emitter: Rc<RefCell<ActionEmitter<()>>>,
    starts: Rc<Cell<usize>>,
    finish: Rc<Cell<bool>>,
    completions: Rc<Cell<usize>>,
    local_events: Rc<Cell<usize>>,
    collab_events: Rc<Cell<usize>>,
    collab_wake_handles: Rc<RefCell<Vec<crate::WakeHandle>>>,
    bracket: Rc<RefCell<Option<Vec<automerge::ChangeHash>>>>,
    undo_request: Rc<Cell<Option<crate::undo::UndoRequest>>>,
    active: Option<tilo_actions::InvocationId>,
    manual: Rc<RefCell<Option<tilo_actions::ControlInvocation>>>,
    confirmation: tilo_actions::ConfirmationPolicy,
    prompted: bool,
    mutable_identity: Option<Rc<MutableContinuationIdentity>>,
}
impl crate::widget::Collaborative for LiveOwner {
    fn handle_collab_event(&mut self, _: &crate::collaboration::CollabEvent) {
        self.collab_events.set(self.collab_events.get() + 1);
        self.collab_wake_handles
            .borrow_mut()
            .push(crate::WakeHandle::new());
    }
}

impl Widget for LiveOwner {
    crate::provides!(
        dyn crate::widget::Collaborative,
        dyn crate::undo::CommittedActions,
        dyn crate::undo::UndoControl,
        dyn crate::undo::LocalUndo,
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources
    );

    fn name(&self) -> &str {
        "Live owner"
    }
    fn test_id(&self) -> Option<&str> {
        Some(if self.resource_id == "live" {
            "live-owner"
        } else {
            self.resource_id
        })
    }
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.text.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.text.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> Rect {
        self.text.get_rect()
    }
    fn handle_event(&mut self, _: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.local_events.set(self.local_events.get() + 1);
    }
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        [self.type_id.to_owned()].into_iter().collect()
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::host_operation::OperationRequest {
            invocation,
            type_id,
            ..
        }) = op.unscoped()
        else {
            return Ok(None);
        };
        if type_id != self.type_id {
            return Ok(None);
        }
        self.starts.set(self.starts.get() + 1);
        self.active = Some(invocation);
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.active != Some(invocation) || !self.finish.get() {
            return None;
        }
        self.active = None;
        self.completions.set(self.completions.get() + 1);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::resources::PublishesResources for LiveOwner {
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if let Some(identity) = &self.mutable_identity {
            return mutable_identity_resources(self.resource_id, self.type_id, identity);
        }
        vec![if self.prompted {
            prompted_resource_for(self.resource_id, self.type_id, self.confirmation.clone())
        } else {
            let mut resource = resource_for(self.resource_id, self.type_id);
            resource.actions[0].type_mut().contract.confirmation = self.confirmation.clone();
            resource
        }]
    }
}

impl crate::widget::controls::ActionControls for LiveOwner {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.manual
            .borrow_mut()
            .take()
            .or_else(|| self.emitter.borrow_mut().take())
    }
}

impl crate::undo::LocalUndo for LiveOwner {
    fn handle_local_undo_request(&mut self, _: crate::undo::UndoRequest) -> bool {
        self.local_events.set(self.local_events.get() + 1);
        true
    }
}

impl crate::undo::UndoControl for LiveOwner {
    fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
        self.undo_request.take()
    }
}

impl crate::undo::CommittedActions for LiveOwner {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.bracket.borrow_mut().take()
    }
}
#[test]
fn preview_discards_unadmitted_local_intents_but_polls_an_already_admitted_effect() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let local_events = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        resource_id: "live",
        type_id: "fixture.export",
        text: Text::new("Live owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        local_events: local_events.clone(),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual: Rc::new(RefCell::new(None)),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: None,
    };
    let mut state = State::new();
    state.doc.put(ROOT, "body", "first").unwrap();
    state.commit_action(None, None);
    let mut driver = FrameHarness::from_managed_root(
        state,
        ManagedRoot::new(Box::new(owner), registration()),
        (900, 650),
    )
    .unwrap();
    emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(
        starts.get(),
        1,
        "real local host door admitted the first effect"
    );
    emitter.borrow_mut().emit(()).unwrap();
    driver.click_named("history-open").unwrap();
    assert!(
        emitter.borrow_mut().take().is_none(),
        "opening History moves the exact pre-entry queued intent into host custody"
    );
    let retained = driver.pending_history_controls();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].invocation.address.resource, "live");
    assert_eq!(retained[0].invocation.address.action, "export");
    assert_eq!(
        retained[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Suspended
    );
    driver
        .perform_named("activity_timeline", &crate::a11y::Action::Focus)
        .unwrap();
    driver.key("End");
    driver.key("Enter");
    assert!(driver.targets().contains_key("history-preview"));
    let events = local_events.get();
    emitter.borrow_mut().emit(()).unwrap();
    finish.set(true);
    driver.key("x");
    driver.set_modifiers(&["Ctrl".into()]);
    driver.key("z");
    driver.set_modifiers(&[]);
    assert_eq!(
        local_events.get(),
        events,
        "hidden live keyboard and undo routes are absent"
    );
    assert_eq!(starts.get(), 1);
    assert_eq!(
        completions.get(),
        1,
        "admitted effect remains attached to the live owner"
    );
    assert!(driver.click_named("live-owner").is_err());
    driver.click_named("history-back-live").unwrap();
    let retained = driver.pending_history_controls();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Ready
    );
    assert_eq!(
        starts.get(),
        1,
        "return to live does not admit retained work"
    );
    assert_eq!(completions.get(), 1, "return does not replay entered work");
    let stale = driver
        .pending_history_control_token(retained[0].id)
        .expect("current exact continuation token");
    driver.click_named("history-open").unwrap();
    assert!(driver.continue_pending_history_control(&stale).is_none());
    driver.click_named("history-open").unwrap();
    assert!(driver.continue_pending_history_control(&stale).is_none());
    let retained = driver.pending_history_controls();
    let token = driver
        .pending_history_control_token(retained[0].id)
        .expect("fresh continuation after re-entry");
    assert_eq!(
        driver.continue_pending_history_control(&token),
        Some(crate::history_mode::pending_controls::PendingControlStatus::Consumed)
    );
    assert!(
        driver.pending_history_controls().is_empty(),
        "confirmed item is consumed: {:?}",
        driver.pending_history_controls()
    );
    assert_eq!(starts.get(), 2, "explicit continuation enters once");
    assert!(driver.continue_pending_history_control(&token).is_none());
    driver.key("y");
    assert_eq!(
        starts.get(),
        2,
        "preview cannot leave a queued local effect to run on return"
    );
    assert!(local_events.get() > events, "normal live input resumes");
}

#[test]
fn retained_phrase_confirmation_requires_fresh_visible_input_and_preserves_arguments() {
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource(), "export").unwrap(),
    )));
    let manual = Rc::new(RefCell::new(Some(tilo_actions::ControlInvocation {
        address: tilo_actions::ActionAddress::new("phrase-owner", "export"),
        arguments: [(
            tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("alpha payload".into()),
        )]
        .into_iter()
        .collect(),
    })));
    let starts = Rc::new(Cell::new(0));
    let owner = LiveOwner {
        resource_id: "phrase-owner",
        type_id: "fixture.phrase-export",
        text: Text::new("Phrase owner"),
        emitter,
        starts: starts.clone(),
        finish: Rc::new(Cell::new(false)),
        completions: Rc::new(Cell::new(0)),
        local_events: Rc::new(Cell::new(0)),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual,
        confirmation: tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "ERASE phrase-owner".into(),
        },
        prompted: true,
        mutable_identity: None,
    };
    let mut driver = FrameHarness::from_managed_root(
        State::new(),
        ManagedRoot::new(Box::new(owner), registration()),
        (900, 650),
    )
    .unwrap();

    driver.click_named("history-open").unwrap();
    assert_eq!(driver.pending_history_controls().len(), 1);
    driver.click_named("history-open").unwrap();
    assert_eq!(starts.get(), 0, "Back to live remains inert");
    assert_eq!(
        driver.pending_history_controls()[0].status,
        crate::history_mode::pending_controls::PendingControlStatus::Ready
    );
    assert!(
        driver.targets().contains_key("history-pending-1"),
        "Continue is public after return: {:?}",
        driver.targets().keys().collect::<Vec<_>>()
    );
    driver.click_named("history-pending-1").unwrap();
    assert_eq!(
        starts.get(),
        0,
        "Continue enters confirmation, not the effect"
    );

    let phrase_bounds = driver
        .targets()
        .get("history-pending-1-phrase")
        .copied()
        .expect("fresh phrase field is publicly addressable");
    assert!(phrase_bounds.width() > 0.0 && phrase_bounds.height() > 0.0);
    let labels = driver
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        labels.iter().any(|label| label.contains("“Export”")),
        "the held control is named by its action's label: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label.contains("alpha payload") || label.contains("component ")),
        "internal owners and raw arguments are never shown: {labels:?}"
    );

    driver
        .perform_named(
            "history-pending-1-phrase",
            &crate::a11y::Action::SetValue("wrong".into()),
        )
        .unwrap();
    driver.click_named("history-pending-1").unwrap();
    assert_eq!(starts.get(), 0, "wrong phrase cannot attest confirmation");
    driver
        .perform_named(
            "history-pending-1-phrase",
            &crate::a11y::Action::SetValue("ERASE phrase-owner".into()),
        )
        .unwrap();
    driver.click_named("history-pending-1").unwrap();
    assert_eq!(starts.get(), 1, "fresh exact phrase admits once");
    assert!(
        driver.pending_history_controls().is_empty(),
        "confirmed item is consumed: {:?}",
        driver.pending_history_controls()
    );
}

fn retained_identity_fixture(
    identity: Rc<MutableContinuationIdentity>,
) -> (
    FrameHarness,
    Rc<Cell<usize>>,
    Rc<RefCell<ActionEmitter<()>>>,
) {
    let starts = Rc::new(Cell::new(0));
    let resource =
        mutable_identity_resources("mutable-owner", "fixture.mutable-export", &identity).remove(0);
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource, "export").unwrap(),
    )));
    let owner = LiveOwner {
        resource_id: "mutable-owner",
        type_id: "fixture.mutable-export",
        text: Text::new("Mutable owner"),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: Rc::new(Cell::new(false)),
        completions: Rc::new(Cell::new(0)),
        local_events: Rc::new(Cell::new(0)),
        collab_events: Rc::new(Cell::new(0)),
        collab_wake_handles: Rc::new(RefCell::new(Vec::new())),
        bracket: Rc::new(RefCell::new(None)),
        undo_request: Rc::new(Cell::new(None)),
        active: None,
        manual: Rc::new(RefCell::new(Some(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new("mutable-owner", "export"),
            arguments: serde_json::Map::new(),
        }))),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: Some(identity),
    };
    (
        FrameHarness::from_managed_root(
            State::new(),
            ManagedRoot::new(Box::new(owner), registration()),
            (900, 650),
        )
        .unwrap(),
        starts,
        emitter,
    )
}

#[test]
fn retained_control_refuses_changed_target_handler_missing_or_ambiguous_identity() {
    let mutations: [fn(&MutableContinuationIdentity); 4] = [
        |identity| identity.target.set(2),
        |identity| identity.handler.set(2),
        |identity| identity.supported.set(false),
        |identity| identity.duplicate_resource.set(true),
    ];
    for mutate in mutations {
        let identity = Rc::new(MutableContinuationIdentity {
            target: Cell::new(1),
            handler: Cell::new(1),
            supported: Cell::new(true),
            duplicate_resource: Cell::new(false),
            housekeeping: Cell::new(false),
        });
        let (mut driver, starts, _emitter) = retained_identity_fixture(identity.clone());
        driver.click_named("history-open").unwrap();
        driver.click_named("history-open").unwrap();
        let item = driver.pending_history_controls().remove(0);
        let token = driver.pending_history_control_token(item.id).unwrap();
        mutate(&identity);
        let status = driver.continue_pending_history_control(&token).unwrap();
        assert!(
            matches!(
                status,
                crate::history_mode::pending_controls::PendingControlStatus::Refused(_)
            ),
            "changed or unsupported exact identity must remain visibly refused: {status:?}"
        );
        assert_eq!(starts.get(), 0);
        assert_eq!(driver.pending_history_controls().len(), 1);
    }
}

/// A person confirms each held action they asked for; housekeeping an app
/// emits while History shows a past version is never held, announced or
/// confirmed, and does not run then: the app does it again on its own.
#[test]
fn housekeeping_is_never_held_for_review() {
    let identity = Rc::new(MutableContinuationIdentity {
        target: Cell::new(1),
        handler: Cell::new(1),
        supported: Cell::new(true),
        duplicate_resource: Cell::new(false),
        housekeeping: Cell::new(true),
    });
    let (mut driver, starts, _emitter) = retained_identity_fixture(identity);
    driver.click_named("history-open").unwrap();
    assert!(
        driver.pending_history_controls().is_empty(),
        "housekeeping is not held while History shows"
    );
    driver.click_named("history-open").unwrap();
    assert!(driver.pending_history_controls().is_empty());
    assert!(!driver.targets().contains_key("history-pending-cancel-1"));
    assert_eq!(starts.get(), 0, "dropped, not run behind the person's back");
}

#[test]
fn actual_cancel_callback_removes_retained_item_and_future_live_control_runs() {
    let identity = Rc::new(MutableContinuationIdentity {
        target: Cell::new(1),
        handler: Cell::new(1),
        supported: Cell::new(true),
        duplicate_resource: Cell::new(false),
        housekeeping: Cell::new(false),
    });
    let (mut driver, starts, emitter) = retained_identity_fixture(identity);
    driver.click_named("history-open").unwrap();
    driver.click_named("history-open").unwrap();
    driver.click_named("history-pending-cancel-1").unwrap();
    assert!(driver.pending_history_controls().is_empty());
    assert_eq!(starts.get(), 0);

    emitter.borrow_mut().emit(()).unwrap();
    driver.key("x");
    assert_eq!(
        starts.get(),
        1,
        "ordinary future live control keeps its semantics"
    );
}

pub(crate) struct EffectProbe {
    pub(crate) root: Box<dyn crate::widget::AppWidget>,
    pub(crate) emitter: Rc<RefCell<ActionEmitter<()>>>,
    pub(crate) starts: Rc<Cell<usize>>,
    pub(crate) finish: Rc<Cell<bool>>,
    pub(crate) completions: Rc<Cell<usize>>,
    pub(crate) collab_events: Rc<Cell<usize>>,
    pub(crate) collab_wake_handles: Rc<RefCell<Vec<crate::WakeHandle>>>,
    pub(crate) bracket: Rc<RefCell<Option<Vec<automerge::ChangeHash>>>>,
    pub(crate) undo_request: Rc<Cell<Option<crate::undo::UndoRequest>>>,
}
pub(crate) fn native_effect_probe_parts(id: &'static str, operation: &'static str) -> EffectProbe {
    let starts = Rc::new(Cell::new(0));
    let finish = Rc::new(Cell::new(false));
    let completions = Rc::new(Cell::new(0));
    let emitter = Rc::new(RefCell::new(ActionEmitter::new(
        tilo_actions::ActionBinding::unit_for(&resource_for(id, operation), "export").unwrap(),
    )));
    let collab_events = Rc::new(Cell::new(0));
    let collab_wake_handles = Rc::new(RefCell::new(Vec::new()));
    let bracket = Rc::new(RefCell::new(None));
    let undo_request = Rc::new(Cell::new(None));
    let root = Box::new(LiveOwner {
        resource_id: id,
        type_id: operation,
        bracket: bracket.clone(),
        undo_request: undo_request.clone(),
        text: Text::new(id),
        emitter: emitter.clone(),
        starts: starts.clone(),
        finish: finish.clone(),
        completions: completions.clone(),
        collab_events: collab_events.clone(),
        collab_wake_handles: collab_wake_handles.clone(),
        local_events: Rc::new(Cell::new(0)),
        active: None,
        manual: Rc::new(RefCell::new(None)),
        confirmation: tilo_actions::ConfirmationPolicy::None,
        prompted: false,
        mutable_identity: None,
    });
    EffectProbe {
        root,
        emitter,
        starts,
        finish,
        completions,
        collab_events,
        collab_wake_handles,
        bracket,
        undo_request,
    }
}
impl crate::widget::AppWidget for LiveOwner {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Live effect probe"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("P")
    }
}
