//! A column's content that the window swaps or reconciles in place.

use ui::widget::{Decorator, Element, Spacer, Widget};

/// The content of one column of the shell. The window reaches it by type through the shell
/// (`NavShell::content_mut::<Pane>()`) and reconciles what it holds, so a view that runs again
/// keeps its widgets, their focus and their scroll.
pub struct Pane {
    inner: Element,
    dirty: bool,
}

impl Pane {
    #[must_use]
    pub fn new(inner: Element) -> Self {
        Self { inner, dirty: true }
    }

    /// Show `inner` instead of what the pane shows.
    pub fn set(&mut self, inner: Element) {
        self.inner = inner;
        self.dirty = true;
    }

    /// What the pane shows, taken out to be reconciled and put back with [`Self::set`].
    pub fn take(&mut self) -> Element {
        std::mem::replace(&mut self.inner, Box::new(Spacer::fixed(0.0)))
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
        let own = std::mem::take(&mut self.dirty);
        self.inner.take_structure_dirty() | own
    }

    fn name(&self) -> &'static str {
        "TilolangPane"
    }
}
