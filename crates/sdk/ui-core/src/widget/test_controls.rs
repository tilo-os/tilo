//! Test support for action-bound controls: a mount outside host custody
//! and a one-parameter test action.

use super::{Decorator, Widget, capability, controls};

/// A widget mounted outside any host's custody, so a test drains its
/// control intents itself rather than the host dispatching them.
pub(crate) struct Uncustodied<W>(pub W);

impl<W: Widget + controls::ActionControls + 'static> Decorator for Uncustodied<W> {
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
        "Uncustodied"
    }
}

impl<W: Widget + controls::ActionControls + 'static> controls::ActionControls for Uncustodied<W> {
    fn install_control_custody(
        &mut self,
        _domain: &crate::ControlDomain,
        _attachment: crate::control_custody::CustodyAttachment,
    ) {
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        None
    }
}

/// A test action taking one `value` of `param`'s type, bound to a control
/// whose input `value` serializes.
pub(crate) fn test_value_action_binding<I: serde::Serialize + 'static>(
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
