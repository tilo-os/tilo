//! How a walk of a widget tree crosses the components it hosts: entering a
//! component's scope before calling into its widgets.
//!
//! A component is an app's record owner. The toolkit knows it only as the
//! [`ComponentScope`] a widget standing for it answers ([`ComponentBoundary`]);
//! the app layer implements both. A tree no such widget is in hosts no
//! components: every widget answers to the window.

use super::Widget;
use super::capability::capability;
use std::rc::Rc;

/// Why a walk could not reach a node, or enter the component it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkError {
    /// The node is not admitted, was replaced, or is inactive.
    MissingNode,
    /// The component has closed.
    ClosedLifetime,
    /// Entering the component would reach another record while one is open.
    CrossResourceReentry,
}

/// A component a widget tree hosts, as a walk crosses it.
pub trait ComponentScope: std::any::Any {
    /// Enter this component's scope for the calls about to run, held until the
    /// entry drops.
    ///
    /// # Errors
    /// When the component has closed, or another record is open.
    fn enter(&self) -> Result<ScopeEntry, WalkError>;

    /// Whether entering `other` sets exactly what entering `self` sets.
    fn enters_same_scope(&self, other: &dyn ComponentScope) -> bool;

    /// Whether `other` is the same component.
    fn same_component(&self, other: &dyn ComponentScope) -> bool;

    /// Whether the component can be entered now, without entering it.
    ///
    /// # Errors
    /// As [`Self::enter`].
    fn check_open(&self) -> Result<(), WalkError>;

    /// Input reached the component outside its passes: it counts the input as
    /// the person's.
    fn note_input(&self);
}

/// What entering a component holds until it drops: the slot its scope stands
/// in, which `leave` releases.
pub struct ScopeEntry(Option<(fn(usize), usize)>);

impl ScopeEntry {
    /// An entry into a scope this thread holds already: dropping it releases
    /// nothing.
    #[must_use]
    pub fn held() -> Self {
        Self(None)
    }

    /// An entry that `leave(slot)` releases when it drops.
    #[must_use]
    pub fn opened(leave: fn(usize), slot: usize) -> Self {
        Self(Some((leave, slot)))
    }
}

impl Drop for ScopeEntry {
    fn drop(&mut self) {
        if let Some((leave, slot)) = self.0.take() {
            leave(slot);
        }
    }
}

/// What a widget standing for a component answers, as a capability
/// ([`capability`]).
pub trait ComponentBoundary {
    /// The component this widget's descendants belong to, if it hosts one.
    fn component_scope(&self) -> Option<Rc<dyn ComponentScope>>;

    /// Whether this widget introduces a document owner distinct from the
    /// runner's outer State.
    fn has_separate_document_owner(&self) -> bool {
        self.component_scope().is_some()
    }
}

/// The component a widget belongs to, as a walk hands it down to the
/// widget's children.
pub type Binding = Rc<dyn ComponentScope>;

/// The component `widget` hosts, if it is a component's boundary.
pub fn binding_of<W: Widget + ?Sized>(widget: &W) -> Option<Binding> {
    capability::<dyn ComponentBoundary>(widget).and_then(ComponentBoundary::component_scope)
}

/// Whether `widget` owns a document apart from the window's: a component
/// boundary, or a widget that stands for one.
pub fn has_separate_document_owner<W: Widget + ?Sized>(widget: &W) -> bool {
    capability::<dyn ComponentBoundary>(widget)
        .is_some_and(ComponentBoundary::has_separate_document_owner)
}

/// The scope of a component a walk is inside, held while the answer is.
pub(crate) struct Entry(#[expect(dead_code, reason = "held for its drop")] Option<ScopeEntry>);

/// Enter `binding`'s component for the calls about to run. `None` when the
/// component has closed, so the walk leaves its widgets alone.
pub(crate) fn enter(binding: Option<&Binding>) -> Option<Entry> {
    binding
        .map(|binding| binding.enter())
        .transpose()
        .ok()
        .map(Entry)
}

/// The component the walk is in at node `node` of `bindings`' tree.
pub(crate) fn node_binding(
    bindings: &super::bound_nodes::BoundNodeMap,
    node: taffy::NodeId,
) -> Option<Binding> {
    bindings.binding_for(node)
}

/// Note that input reached node `node`: its component then counts it as the person's.
pub(crate) fn note_input(bindings: &super::bound_nodes::BoundNodeMap, node: taffy::NodeId) {
    if let Some(binding) = bindings.binding_ref(node) {
        binding.note_input();
    }
}

/// One component scope a walk holds across every node it asks, entered again
/// only when the walk reaches another component.
#[derive(Default)]
pub(crate) struct HeldScope {
    held: Option<(Binding, ScopeEntry)>,
}

impl HeldScope {
    /// Enter `binding`'s scope for the visit about to run, unless it is the one this walk holds.
    ///
    /// # Errors
    /// As [`ComponentScope::enter`].
    pub(crate) fn enter(&mut self, binding: Option<&Binding>) -> Result<(), WalkError> {
        let Some(binding) = binding else {
            self.held = None;
            return Ok(());
        };
        if let Some((held, _)) = &self.held
            && held.enters_same_scope(&**binding)
        {
            binding.check_open()?;
            return Ok(());
        }
        self.held = None;
        let entry = binding.enter()?;
        self.held = Some((Rc::clone(binding), entry));
        Ok(())
    }
}
