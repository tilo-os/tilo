//! Parts of a widget tree that change while their neighbours stay: [`Part`] and [`Switch`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{AppModel, Element, ModelPath, Widget};

/// What the app holds to replace a [`Part`]'s content.
#[derive(Clone)]
pub struct PartHandle {
    pending: Rc<RefCell<Option<Element>>>,
}

impl PartHandle {
    pub fn set(&self, content: Element) {
        *self.pending.borrow_mut() = Some(content);
    }
}

/// A place in the tree whose content the app replaces in place:
/// transparent to layout, input, paint and semantics.
#[crate::component::not_a_component(
    framework,
    "content its owner swaps in place; the language swaps content with `if`"
)]
pub struct Part {
    content: Element,
    pending: Rc<RefCell<Option<Element>>>,
    structure_dirty: bool,
    repaint: Cell<bool>,
}

impl Part {
    pub fn new(content: Element) -> (Self, PartHandle) {
        let pending = Rc::new(RefCell::new(None));
        let handle = PartHandle {
            pending: Rc::clone(&pending),
        };
        (
            Self {
                content,
                pending,
                structure_dirty: false,
                repaint: Cell::new(false),
            },
            handle,
        )
    }

    /// A part with nothing in it yet: it takes up nothing until it is set.
    pub fn empty() -> (Self, PartHandle) {
        Self::new(super::optional(None))
    }

    fn adopt(&mut self) {
        let next = self.pending.borrow_mut().take();
        if let Some(next) = next {
            self.content = next;
            self.structure_dirty = true;
            self.repaint.set(true);
        }
    }
}

impl super::Decorator for Part {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.adopt();
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let before = self.structure_dirty;
        self.adopt();
        self.content.update(state, path) || (self.structure_dirty && !before)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.content.take_structure_dirty() | own
    }

    fn settle(&mut self, edits: &mut super::Edits<'_>, path: &automerge::ObjId) {
        self.adopt();
        self.content.settle(edits, path);
    }

    /// Content handed over outside an event (a finished request, say) still
    /// needs a synchronization pass to be adopted.
    fn needs_idle_sync(&self) -> bool {
        self.pending.borrow().is_some() || self.content.needs_idle_sync()
    }

    fn needs_redraw(&self) -> bool {
        self.pending.borrow().is_some()
            || self.repaint.get()
            || (self.content.needs_redraw() || self.content.needs_animation_frame())
    }

    fn mark_drawn(&mut self) {
        self.repaint.set(false);
        self.content.mark_drawn();
    }

    fn name(&self) -> &str {
        "Part"
    }
}

/// One of several parts, chosen by a key, each built once and kept alive
/// while another shows. See the module docs.
#[crate::component::not_a_component(
    framework,
    "content its owner swaps in place; the language swaps content with `if`"
)]
pub struct Switch<K> {
    key: Box<dyn FnMut() -> K>,
    build: Box<dyn FnMut(&K) -> Element>,
    parts: Vec<(K, Element)>,
    current: usize,
    structure_dirty: bool,
}

impl<K: PartialEq + 'static> Switch<K> {
    /// Show the part for `key()`, built by `build` the first time that key
    /// is shown.
    pub fn new(
        mut key: impl FnMut() -> K + 'static,
        mut build: impl FnMut(&K) -> Element + 'static,
    ) -> Self {
        let first = key();
        let part = build(&first);
        Self {
            key: Box::new(key),
            build: Box::new(build),
            parts: vec![(first, part)],
            current: 0,
            structure_dirty: false,
        }
    }

    /// Show the part for the key `state` projects to.
    pub fn from_state<T: 'static>(
        state: &crate::LocalState<T>,
        mut key: impl FnMut(&T) -> K + 'static,
        build: impl FnMut(&K) -> Element + 'static,
    ) -> Self {
        let state = state.clone();
        Self::new(move || state.with(&mut key), build)
    }

    pub fn current(&mut self) -> &mut dyn Widget {
        self.parts[self.current].1.as_mut()
    }

    pub fn part(&mut self, key: &K) -> Option<&mut dyn Widget> {
        self.parts
            .iter_mut()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, part)| part.as_mut() as &mut dyn Widget)
    }

    fn observe(&mut self) {
        let key = (self.key)();
        if self.parts[self.current].0 == key {
            return;
        }
        let existing = self
            .parts
            .iter()
            .position(|(candidate, _)| *candidate == key);
        self.current = if let Some(index) = existing {
            index
        } else {
            let part = (self.build)(&key);
            self.parts.push((key, part));
            self.parts.len() - 1
        };
        self.structure_dirty = true;
    }
}

impl<K: PartialEq + 'static> super::Decorator for Switch<K> {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.parts[self.current].1.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.parts[self.current].1.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.observe();
        self.parts[self.current].1.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let before = self.current;
        self.observe();
        self.parts[self.current].1.update(state, path) || before != self.current
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.parts[self.current].1.take_structure_dirty() | own
    }

    fn name(&self) -> &str {
        "Switch"
    }
}
