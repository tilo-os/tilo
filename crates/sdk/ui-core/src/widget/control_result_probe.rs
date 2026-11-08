//! A probe that reports the control results a test drives, standing in for an app.
//!
//! `widget::control_result_probe`, kept here so `widget/mod.rs` stays readable.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test support")]

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Clone, Default)]
pub(crate) struct Observed {
    pub(crate) results: Rc<RefCell<Vec<Result<ResourceOperationStart, tilo_actions::DoorError>>>>,
    pub(crate) calls: Rc<Cell<usize>>,
    pub(crate) polls: Rc<Cell<usize>>,
    pub(crate) pending: Rc<RefCell<VecDeque<tilo_actions::ControlInvocation>>>,
    pub(crate) payloads: Rc<RefCell<Vec<String>>>,
}
impl Observed {
    pub(crate) fn enqueue(&self, action: &str) {
        self.pending
            .borrow_mut()
            .push_back(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("test.control-result", action),
                arguments: serde_json::Map::new(),
            });
    }
    pub(crate) fn enqueue_text(&self, action: &str, text: &str) {
        self.pending
            .borrow_mut()
            .push_back(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new("test.control-result", action),
                arguments: serde_json::Map::from_iter([(
                    "text".to_string(),
                    serde_json::Value::String(text.to_string()),
                )]),
            });
    }
}
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent test switches exercise acceptance, confirmation and typing"
)]
pub(crate) struct Probe {
    observed: Observed,
    accepted: bool,
    pub(crate) confirmation: bool,
    pub(crate) terminal_after: Option<(usize, bool)>,
    pub(crate) typed_text: bool,
}
impl Probe {
    pub(crate) fn new(accepted: bool) -> (Self, Observed) {
        let observed = Observed::default();
        (
            Self {
                observed: observed.clone(),
                accepted,
                confirmation: false,
                terminal_after: None,
                typed_text: false,
            },
            observed,
        )
    }
}
impl Widget for Probe {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn name(&self) -> &'static str {
        "ControlResultProbe"
    }
}

impl crate::widget::ResourceOperations for Probe {
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let (after, succeeds) = self.terminal_after?;
        let polls = self.observed.polls.get().saturating_add(1);
        self.observed.polls.set(polls);
        if polls < after {
            return None;
        }
        Some(if succeeds {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        } else {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "fixture_failed".into(),
                    message: "Delayed finalizer failed".into(),
                    details: None,
                    retryable: false,
                },
            }
        })
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["test.control-result.run"]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        _: &mut crate::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if let Ok(text) = serde_json::from_slice::<String>(request.payload) {
            self.observed.payloads.borrow_mut().push(text);
        }
        self.observed
            .calls
            .set(self.observed.calls.get().saturating_add(1));
        Ok(Some(if self.accepted {
            ResourceOperationStart::Accepted
        } else {
            ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
        }))
    }
}

impl crate::resources::PublishesResources for Probe {
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let mut contract = tilo_actions::ActionContract::default();
        if self.accepted {
            contract.execution = tilo_actions::ExecutionMode::Background { cancellable: false };
            contract.result = tilo_actions::ResultSpec::Job;
        }
        if self.confirmation {
            contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        }
        let action = if self.typed_text {
            tilo_actions::ActionSpec::typed_operation(
                "run",
                "Run",
                "text",
                tilo_actions::OperationKind::Effect,
                "test.control-result.run",
                vec![tilo_actions::ActionParam::required(
                    "text",
                    tilo_actions::ParamType::Text,
                    "Captured text",
                )],
                |args| {
                    tilo_actions::EncodedAction::encode_operation(
                        "test.control-result.run",
                        &args.text("text").unwrap_or_default(),
                    )
                    .expect("a borrowed string always serializes")
                },
            )
        } else {
            tilo_actions::ActionSpec::operation(
                "run",
                "Run",
                tilo_actions::OperationKind::Effect,
                "test.control-result.run",
                &(),
            )
            .unwrap()
        };
        vec![
            tilo_actions::ResourceSpec::new("test.control-result", "test", "Probe")
                .with_action(action.with_contract(contract)),
        ]
    }
}

impl crate::widget::controls::ActionControls for Probe {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.observed.pending.borrow_mut().pop_front()
    }
    fn on_control_invocation_result(
        &mut self,
        _: &tilo_actions::ActionAddress,
        result: &Result<ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        self.observed.results.borrow_mut().push(result.clone());
    }
}
thread_local! { static NEXT: RefCell<Option<Probe>> = const { RefCell::new(None) }; }
fn factory(
    mut context: crate::application::HostContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(NEXT.with_borrow_mut(|next| next.take().unwrap())),
        tilo_actions::ActionRegistry::new(),
    ))
}
pub(crate) fn component(
    host: &crate::application_test_support::ProbeHost,
    instance: u64,
    accepted: bool,
) -> (crate::application::AppComponent, Observed) {
    let (probe, observed) = Probe::new(accepted);
    NEXT.with_borrow_mut(|next| *next = Some(probe));
    let definition = crate::application_test_support::definition(factory);
    let target = crate::application::LaunchTarget::New;
    let context = host
        .prepare_with_permissions(
            &definition,
            instance,
            &target,
            crate::application_test_support::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, target, crate::application::RestoreInput::None)
        .unwrap();
    (component, observed)
}

impl AppWidget for Probe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Control result probe"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
}
