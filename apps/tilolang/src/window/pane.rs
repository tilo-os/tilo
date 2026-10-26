//! A column's content that the window swaps or reconciles in place.

use ui::widget::{Decorator, Element, StructureChange, Widget};

/// The content of one column of the shell. The window reaches it by type through the shell
/// (`NavShell::content_mut::<Pane>()`) and reconciles what it holds, so a view that runs again
/// keeps its widgets, their focus and their scroll.
pub struct Pane {
    inner: Element,
    /// How what the pane shows changed since the host last read it.
    structure: StructureChange,
}

impl Pane {
    /// A pane showing `inner`. A new pane reports nothing: whoever shows it (a shell taking a
    /// column, a container taking a child) reports that.
    #[must_use]
    pub fn new(inner: Element) -> Self {
        Self {
            inner,
            structure: StructureChange::None,
        }
    }

    /// Show `inner`, another content altogether, instead of what the pane shows: the window
    /// rebuilds and repaints whole.
    pub fn set(&mut self, inner: Element) {
        self.inner = inner;
        self.structure = StructureChange::Rebuild;
    }

    /// Show `inner` in place of what the pane shows, as a reconciler does when the widget it
    /// had could not take the change: the new content draws itself whole, and the window
    /// repaints where the boxes it moved were and are.
    pub fn replace(&mut self, inner: Element) {
        self.inner = inner;
        self.structure = self.structure.join(StructureChange::Described);
    }

    /// What the pane shows, to reconcile it in place.
    pub fn content_mut(&mut self) -> &mut Element {
        &mut self.inner
    }
}

impl Decorator for Pane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure).rebuilds();
        self.inner.take_structure_dirty() | own
    }

    fn take_structure_change(&mut self) -> StructureChange {
        let own = std::mem::take(&mut self.structure);
        own.join(self.inner.take_structure_change())
    }

    fn name(&self) -> &'static str {
        "TilolangPane"
    }
}
