//! Test support for action-bound controls: a mount outside host ownership
//! and a one-parameter test action.
#![allow(
    clippy::unwrap_used,
    reason = "a test-support binding: a failure here is the assertion"
)]

use tilo_ui_base::{widget::Decorator, widget::Widget, widget::capability, widget::controls};

#[tilo_ui_base::component::not_a_component(
    framework,
    "a test's mount of a control outside a host's ownership"
)]
pub struct Unattached<W>(pub W);

impl<W: Widget + controls::ActionControls + 'static> Decorator for Unattached<W> {
    type Inner = W;

    fn inner(&self) -> &W {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.0
    }

    fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>) {
        if request.wants::<dyn controls::ActionControls>() {
            request.provide::<dyn controls::ActionControls>(self);
            return;
        }
        self.0.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>) {
        if request.wants::<dyn controls::ActionControls>() {
            request.provide::<dyn controls::ActionControls>(self);
            return;
        }
        self.0.provide_mut(request);
    }

    fn name(&self) -> &'static str {
        "Unattached"
    }
}

impl<W: Widget + controls::ActionControls + 'static> controls::ActionControls for Unattached<W> {
    fn attach_controls(
        &mut self,
        _domain: &tilo_ui_base::ControlDomain,
        _attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        None
    }
}

pub fn test_value_action_binding<I: serde::Serialize + 'static>(
    param: tilo_actions::ParamType,
) -> tilo_actions::ActionBinding<I> {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ResourceSpec};
    let spec = ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_value",
        vec![ActionParam::required("value", param, "Value")],
        |_| EncodedAction::encode_operation("test.set_value", &()).unwrap(),
    );
    let resource = ResourceSpec::new("test:control", "test", "Test").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |value: I| {
        serde_json::Map::from_iter([("value".to_owned(), serde_json::json!(value))])
    })
    .unwrap()
}
