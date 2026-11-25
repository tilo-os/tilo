//! How a walk of a widget tree crosses the components it hosts: entering a
//! component's scope before calling into its widgets.
//!
//! A component is an app's record owner, which only Tilo's model has: without
//! the `model` feature a tree hosts no components, every widget answers to
//! the window, and a walk reads the document as it is.

use super::Widget;

/// The component a widget belongs to, as a walk hands it down to the
/// widget's children.
#[cfg(feature = "model")]
pub(crate) type Binding = crate::bound_dispatch::CallbackBinding;

/// Without the model no widget belongs to a component, so no binding exists.
#[cfg(not(feature = "model"))]
#[derive(Clone)]
pub(crate) enum Binding {}

/// The component `widget` hosts, if it is a component's boundary.
#[cfg(feature = "model")]
pub(crate) fn binding_of<W: Widget + ?Sized>(widget: &W) -> Option<Binding> {
    crate::bound_dispatch::callback_binding(widget)
}

#[cfg(not(feature = "model"))]
pub(crate) fn binding_of<W: Widget + ?Sized>(_widget: &W) -> Option<Binding> {
    None
}

/// Whether `widget` owns a document apart from the window's: a component
/// boundary, or a widget that stands for one.
#[cfg(feature = "model")]
pub(crate) fn has_separate_document_owner<W: Widget + ?Sized>(widget: &W) -> bool {
    crate::bound_dispatch::has_separate_document_owner(widget)
}

#[cfg(not(feature = "model"))]
pub(crate) fn has_separate_document_owner<W: Widget + ?Sized>(_widget: &W) -> bool {
    false
}

/// The scope of a component a walk is inside, held while the answer is.
#[cfg(feature = "model")]
pub(crate) struct Entry(
    #[expect(dead_code, reason = "held for its drop")]
    Option<crate::bound_dispatch::CallbackScopeGuard>,
);

#[cfg(not(feature = "model"))]
pub(crate) struct Entry;

/// Enter `binding`'s component for the calls about to run. `None` when the
/// component has closed, so the walk leaves its widgets alone.
#[cfg(feature = "model")]
pub(crate) fn enter(binding: Option<&Binding>) -> Option<Entry> {
    binding.map(Binding::enter).transpose().ok().map(Entry)
}

#[cfg(not(feature = "model"))]
pub(crate) fn enter(binding: Option<&Binding>) -> Option<Entry> {
    match binding {
        Some(binding) => match *binding {},
        None => Some(Entry),
    }
}

/// The component the walk is in at node `node` of `bindings`' tree.
#[cfg(feature = "model")]
pub(crate) fn node_binding(
    bindings: &crate::bound_dispatch::BoundNodeMap,
    node: taffy::NodeId,
) -> Option<Binding> {
    bindings.binding_for(node)
}

#[cfg(not(feature = "model"))]
pub(crate) fn node_binding(
    _bindings: &crate::bound_dispatch::BoundNodeMap,
    _node: taffy::NodeId,
) -> Option<Binding> {
    None
}

#[cfg(not(feature = "model"))]
impl Binding {
    /// Whether `self` and `other` are the same component.
    pub(crate) fn same_component(&self, _other: &Self) -> bool {
        match *self {}
    }
}

/// Note that input reached node `node`: its component then counts it as the person's.
#[cfg(feature = "model")]
pub(crate) fn note_input(bindings: &crate::bound_dispatch::BoundNodeMap, node: taffy::NodeId) {
    if let Some(binding) = bindings.binding_ref(node) {
        binding.note_input();
    }
}

#[cfg(not(feature = "model"))]
pub(crate) fn note_input(_bindings: &crate::bound_dispatch::BoundNodeMap, _node: taffy::NodeId) {}

/// One component scope a walk holds across every node it asks, entered again
/// only when the walk reaches another component.
#[cfg(feature = "model")]
pub(crate) type HeldScope = crate::bound_dispatch::HeldScope;

#[cfg(not(feature = "model"))]
#[derive(Default)]
pub(crate) struct HeldScope;

#[cfg(not(feature = "model"))]
impl HeldScope {
    pub(crate) fn enter(
        &mut self,
        binding: Option<&Binding>,
    ) -> Result<(), std::convert::Infallible> {
        match binding {
            Some(binding) => match *binding {},
            None => Ok(()),
        }
    }
}
