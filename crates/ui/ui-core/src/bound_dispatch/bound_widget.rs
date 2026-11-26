//! What a widget above the toolkit answers about the records it stands for:
//! the component binding it dispatches through, the history it keeps beside
//! the document it shows, and how a traversal reaches the components it
//! retains.
//!
//! A widget answers through the capabilities [`BoundWidget`] and
//! [`BoundTraversal`]; a widget that provides neither is no bound component and
//! answers with its children ([`Widget::retained_children_mut`]).

use super::local_target::ResourceReadRequest;
use super::{BoundDispatchError, CallbackBinding, ComponentRequest, callback_binding_of};
use crate::history_mode::HistoryOwner;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{widget::capability::capability, widget::capability::capability_mut};
use tilo_ui_base::{widget::component_walk::ComponentBoundary, widget::component_walk::binding_of};

/// Which record owner a widget stands in ([`ComponentBoundary`]: the runtime
/// binding descendants in a matching layout topology inherit), and the history
/// it keeps beside the document it shows.
pub trait BoundWidget: ComponentBoundary {
    /// Opaque actual ownership for framework History source registration.
    fn history_owner(&self) -> Option<HistoryOwner> {
        own_binding(self).map(HistoryOwner::component)
    }

    /// Records this widget keeps open beside the document it shows, each
    /// synced on its own route: a framework app's records beside its own
    /// ([`crate::resource::StoredResource`]).
    fn resource_owners(&self) -> Vec<HistoryOwner> {
        Vec::new()
    }

    /// Exact app instance whose non-text shortcuts this subtree owns when
    /// activation has no control target.
    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        own_binding(self).map(|binding| binding.instance_id)
    }
}

/// The callback binding `widget` answers as its component's boundary.
fn own_binding(widget: &(impl ComponentBoundary + ?Sized)) -> Option<CallbackBinding> {
    widget
        .component_scope()
        .map(|scope| callback_binding_of(&*scope).clone())
}

/// How a traversal reaches the components a widget retains, when they differ
/// from its children: a component root enters its own binding, a tile reaches
/// its live content however it is shown.
pub(crate) trait BoundTraversal: Widget {
    fn visit_components(
        &mut self,
        request: &mut ComponentRequest<'_>,
    ) -> Result<(), BoundDispatchError>;

    fn visit_resource_owners(
        &self,
        request: &mut ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::ApiError> {
        request.visit(self)
    }
}

/// The callback binding of the component `widget` is the boundary of
/// ([`ComponentBoundary::component_scope`]).
pub fn callback_binding<W: Widget + ?Sized>(widget: &W) -> Option<CallbackBinding> {
    binding_of(widget).map(|scope| callback_binding_of(&*scope).clone())
}

/// [`BoundWidget::history_owner`] of `widget`.
pub fn history_owner<W: Widget + ?Sized>(widget: &W) -> Option<HistoryOwner> {
    capability::<dyn BoundWidget>(widget).and_then(BoundWidget::history_owner)
}

/// [`BoundWidget::resource_owners`] of `widget`.
pub fn resource_owners<W: Widget + ?Sized>(widget: &W) -> Vec<HistoryOwner> {
    capability::<dyn BoundWidget>(widget).map_or_else(Vec::new, BoundWidget::resource_owners)
}

/// [`BoundWidget::keyboard_input_scope`] of `widget`.
#[cfg(test)]
pub(crate) fn keyboard_input_scope<W: Widget + ?Sized>(
    widget: &W,
) -> Option<crate::application::InstanceId> {
    capability::<dyn BoundWidget>(widget).and_then(BoundWidget::keyboard_input_scope)
}

/// [`BoundTraversal::visit_components`] of `widget`, or of its retained
/// children when it is no bound component.
pub(crate) fn visit_components<W: Widget + ?Sized>(
    widget: &mut W,
    request: &mut ComponentRequest<'_>,
) -> Result<(), BoundDispatchError> {
    if let Some(component) = capability_mut::<dyn BoundTraversal>(widget) {
        return component.visit_components(request);
    }
    for child in widget.retained_children_mut() {
        if request.target.is_some() && request.found {
            break;
        }
        visit_components(child, request)?;
    }
    Ok(())
}

/// [`BoundTraversal::visit_resource_owners`] of `widget`.
pub(crate) fn visit_resource_owners<W: Widget + ?Sized>(
    widget: &W,
    request: &mut ResourceReadRequest<'_>,
) -> Result<(), tilo_actions::ApiError> {
    if let Some(component) = capability::<dyn BoundTraversal>(widget) {
        return component.visit_resource_owners(request);
    }
    request.visit(widget)
}
