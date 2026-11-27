//! Binding a control to an action: the one place a control gets the [`BoundAction`] it runs.
//!
//! A widget takes what it runs as anything that [`Bind`]s: the `ActionBinding` a resource exports
//! (an app binding its control), or the [`BoundAction`] a component's event already carries. The
//! base the widgets are built on cannot bind on its own and has no other way to make one; the
//! repository contract `tools/policy/check_control_binding.py` holds every other crate to this
//! module.

use tilo_actions::ActionBinding;
use tilo_ui_base::widget::BoundAction;

/// Something a control can run: an action binding, checked against the action's parameters, or a
/// control's action bound already.
pub trait Bind<I> {
    /// The bound action a control runs.
    fn bound(self) -> BoundAction<I>;
}

impl<I: 'static> Bind<I> for ActionBinding<I> {
    fn bound(self) -> BoundAction<I> {
        let address = self.address().clone();
        let shared = self
            .shared_resource()
            .map(|(app, _, kind)| (app.to_owned(), kind.to_owned()));
        BoundAction::from_tilo_ui_binding(address, shared, move |input| {
            self.invoke(input).map_err(|error| error.to_string())
        })
    }
}

impl<I> Bind<I> for BoundAction<I> {
    fn bound(self) -> BoundAction<I> {
        self
    }
}
