//! A component whose "Send" button is bound to its typed `send` action, an effect that reaches
//! outside the person's device: a press on it is the action's call, which the workspace's grants
//! decide (asking the person, or refusing), where a toolkit closure is never gated.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "test fixture: a failure is the assertion, the probe paints through the widget contract, and a test-support build hands another crate's tests only some of it"
)]

use super::Button;
use tilo_ui_base::widget::Widget;

/// What [`sends`] shows: its button, the resource it publishes and what a send runs.
struct Sends {
    child: tilo_ui_base::widget::Element,
    resource: tilo_actions::ResourceSpec,
    sent: Box<dyn FnMut()>,
}

const SEND_OPERATION: &str = "fixture.send";

/// The fixture, its button named `go`: `sent` runs only when the action is admitted.
pub fn sends(sent: impl FnMut() + 'static) -> tilo_ui_base::widget::Element {
    sends_through(sent, |binding| {
        Box::new(Button::new("go", "Send").on_action(binding))
    })
}

/// The fixture with the control `control` makes over the send action in its button's place: any
/// control a person presses to send, whatever input it takes (the action has no parameters).
pub fn sends_through<I: 'static>(
    sent: impl FnMut() + 'static,
    control: impl FnOnce(tilo_actions::ActionBinding<I>) -> tilo_ui_base::widget::Element,
) -> tilo_ui_base::widget::Element {
    use tilo_actions::{ActionBinding, ActionSpec, EncodedAction, OperationKind, ResourceSpec};
    let spec = ActionSpec::typed_operation(
        "send",
        "Send",
        "Send it",
        OperationKind::Effect,
        SEND_OPERATION,
        vec![],
        |_| EncodedAction::encode_operation(SEND_OPERATION, &()).unwrap(),
    );
    let resource = ResourceSpec::new("fixture:outbox", "fixture", "Outbox").with_action(spec);
    let binding = ActionBinding::new(&resource, &resource.actions[0], |_: I| {
        serde_json::Map::new()
    })
    .unwrap();
    Box::new(Sends {
        child: Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(control(binding)),
        ),
        resource,
        sent: Box::new(sent),
    })
}

impl Widget for Sends {
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn crate::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );

    crate::testing::delegate_widget_methods!(
        child: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );
    fn name(&self) -> &str {
        "Outbox fixture"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
}

impl tilo_ui_base::widget::ResourceOperations for Sends {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        std::iter::once(SEND_OPERATION.to_owned()).collect()
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        _: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        if op.request().type_id != SEND_OPERATION {
            return Ok(None);
        }
        (self.sent)();
        Ok(Some(
            tilo_ui_base::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl crate::widget::resources::PublishesResources for Sends {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        vec![self.resource.clone()]
    }
}

impl tilo_ui_base::widget::controls::ActionControls for Sends {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        tilo_ui_base::widget::controls::attach_controls(self.child.as_mut(), domain, attachment);
    }
}

crate::testing::test_app_widget!(Sends, "Outbox fixture");
