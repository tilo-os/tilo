//! How a walk of a widget tree crosses the components it hosts: entering a
//! component's scope before calling into its widgets, and reading the
//! document as the component holds it.
//!
//! A component is an app's record owner, which only Tilo's model has: without
//! the `model` feature a tree hosts no components, every widget answers to
//! the window, and a walk reads the document as it is.

use super::Widget;
use automerge::AutoCommit;

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

/// Where a walk stands at one widget: whether the widget is a component's
/// boundary, the component it reads under, and that component's scope held.
pub(crate) struct Crossing {
    /// The widget hosts a component: what it publishes is the component's,
    /// read below it.
    pub(crate) boundary: bool,
    /// The component the widget belongs to, its own or the one it is inside.
    pub(crate) binding: Option<Binding>,
    _entry: Entry,
}

/// Step onto `widget`, inside `inherited`'s component. `None` when the walk
/// must leave the widget's subtree: it owns a document of its own no component
/// answers for, or its component has closed.
pub(crate) fn cross(widget: &dyn Widget, inherited: Option<Binding>) -> Option<Crossing> {
    let own = binding_of(widget);
    if own.is_none() && has_separate_document_owner(widget) {
        return None;
    }
    let boundary = own.is_some();
    let binding = own.or(inherited);
    let entry = enter(binding.as_ref())?;
    Some(Crossing {
        boundary,
        binding,
        _entry: entry,
    })
}

impl Crossing {
    /// `read` of `doc` as the widget holds it: its component's document, else
    /// the window's. `None` when the component's document cannot be read now.
    pub(crate) fn read<T>(
        &self,
        doc: &AutoCommit,
        read: impl FnOnce(&AutoCommit) -> T,
    ) -> Option<T> {
        match &self.binding {
            #[cfg(feature = "model")]
            Some(binding) => binding.read_document(doc, |doc, _| read(doc)).ok(),
            #[cfg(not(feature = "model"))]
            Some(binding) => match *binding {},
            None => Some(read(doc)),
        }
    }
}
