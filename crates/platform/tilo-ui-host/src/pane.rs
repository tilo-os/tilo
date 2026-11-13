//! A column's content that the window swaps or reconciles in place.

use ui::widget::{Decorator, Element, StructureChange, Widget};

/// A column's content. The window reaches it by type through the shell
/// (`NavShell::content_mut::<Pane>()`) and reconciles it, so a view that runs again keeps its
/// widgets, focus and scroll.
pub struct Pane {
    inner: Element,
    /// How what the pane shows changed since the host last read it.
    structure: StructureChange,
}

impl Pane {
    /// A new pane reports no structure change; whoever shows it reports that.
    #[must_use]
    pub fn new(inner: Element) -> Self {
        Self {
            inner,
            structure: StructureChange::None,
        }
    }

    pub fn set(&mut self, inner: Element) {
        self.inner = inner;
        self.structure = StructureChange::Rebuild;
    }

    /// The new content draws whole; the window repaints where its boxes were and are.
    pub fn replace(&mut self, inner: Element) {
        self.inner = inner;
        self.structure = self.structure.join(StructureChange::Described);
    }

    pub fn content_mut(&mut self) -> &mut Element {
        &mut self.inner
    }

    #[must_use]
    pub fn content(&self) -> &Element {
        &self.inner
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
    // A pane has no name of its own: it reads as what it shows.
}
