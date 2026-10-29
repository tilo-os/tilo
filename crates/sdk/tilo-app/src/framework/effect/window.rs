//! Effect handlers that answer on the window a view laid out.

use std::any::Any;

use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_resource::{Effect, Resource};
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};

use super::{EffectAnswer, EffectCx, EffectRunner, ViewHandler};

type WindowHandle<R, E, W> = Box<dyn FnMut(&mut W, E, &mut EffectCx<R>) -> EffectAnswer>;

/// A view's effect handler that answers on the window the view laid out
/// ([`ViewCx::window_effects`](crate::framework::ViewCx::window_effects)): it finds
/// the window `W` in the view's tree as the effect starts and lends it to
/// the handler for that call. What finishes later (a job's completion)
/// reaches state the window shares, as for any view handler.
pub(super) struct WindowHandler<R: Resource, E, W> {
    pub(super) runner: EffectRunner<R, (), E>,
    pub(super) handle: WindowHandle<R, E, W>,
}

impl<R: Resource, E: Effect, W: 'static> ViewHandler for WindowHandler<R, E, W> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &tilo_resource::Doc,
        view: &mut dyn ui_core::widget::Widget,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if op.with_custody().0.type_id != E::OPERATION {
            return Ok(None);
        }
        let window = window_in::<W>(view).ok_or("the window is not open")?;
        let handle = &mut self.handle;
        self.runner.start(op, &mut (), Some(doc), |(), effect, cx| {
            handle(window, effect, cx)
        })
    }

    fn poll(&mut self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        self.runner.poll(invocation, cancel, &mut ())
    }
}

/// The first widget of type `W` in `widget`, whatever wraps it.
fn window_in<W: 'static>(widget: &mut dyn ui_core::widget::Widget) -> Option<&mut W> {
    if widget.as_any().is_some_and(<dyn Any>::is::<W>) {
        return widget.as_any_mut()?.downcast_mut::<W>();
    }
    widget.children_mut().into_iter().find_map(window_in::<W>)
}
