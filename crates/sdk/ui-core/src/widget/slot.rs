//! Parts of a widget tree that change while their neighbours stay:
//! [`Slot`] and [`Switch`].
//!
//! A request panel, a response panel or a dialog changes with the model
//! while the controls around it (a URL bar, the tabs, a split) keep their
//! focus and state. A [`Slot`] stands where the changing part goes; the app
//! keeps its [`SlotHandle`] and hands over a freshly built element, which the
//! slot adopts at its next synchronization, so a replacement never happens
//! while an event is still being routed.
//!
//! A [`Switch`] shows one of several parts chosen by a key (the open tab,
//! the screen of a wizard) and keeps every part it has built alive, so
//! switching back finds a part as it was left: its scroll position, its
//! drafts, its focus.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{AppModel, Element, ModelPath, Widget};

/// What the app holds to replace a [`Slot`]'s content.
#[derive(Clone)]
pub struct SlotHandle {
    pending: Rc<RefCell<Option<Element>>>,
}

impl SlotHandle {
    /// Replace the slot's content at its next synchronization.
    pub fn set(&self, content: Element) {
        *self.pending.borrow_mut() = Some(content);
    }
}

/// A place in the tree whose content the app replaces in place:
/// transparent to layout, input, paint and semantics.
pub struct Slot {
    content: Element,
    pending: Rc<RefCell<Option<Element>>>,
    structure_dirty: bool,
    repaint: Cell<bool>,
}

impl Slot {
    /// A slot showing `content`, and the handle that replaces it.
    pub fn new(content: Element) -> (Self, SlotHandle) {
        let pending = Rc::new(RefCell::new(None));
        let handle = SlotHandle {
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

    /// A slot with nothing in it yet: it takes no room until it is set.
    pub fn empty() -> (Self, SlotHandle) {
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

impl super::Decorator for Slot {
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
        self.pending.borrow().is_some() || self.repaint.get() || self.content.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.repaint.set(false);
        self.content.mark_drawn();
    }

    fn name(&self) -> &str {
        "Slot"
    }
}

/// One of several parts, chosen by a key, each built once and kept alive
/// while another shows. See the [module docs](self).
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

    /// The part showing now.
    pub fn current(&mut self) -> &mut dyn Widget {
        self.parts[self.current].1.as_mut()
    }

    /// The part kept for `key`, if it was built.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Text, TextInput};

    #[test]
    fn a_slot_adopts_new_content_at_its_next_sync() {
        let (mut slot, handle) = Slot::new(Box::new(Text::new("Loading")));
        handle.set(Box::new(Text::new("Ready")));
        assert_eq!(
            Widget::accessibility(&slot).and_then(|n| n.name).as_deref(),
            Some("Loading"),
            "not before the sync"
        );
        let doc = automerge::AutoCommit::new();
        Widget::sync(&mut slot, &doc, &automerge::ROOT);
        assert_eq!(
            Widget::accessibility(&slot).and_then(|n| n.name).as_deref(),
            Some("Ready")
        );
        assert!(Widget::take_structure_dirty(&mut slot));
        let (empty, _) = Slot::empty();
        assert!(Widget::children(&empty).is_empty());
    }

    #[test]
    fn a_switch_keeps_each_part_it_built_alive() {
        let tab = crate::LocalState::new(0_u8);
        let builds = Rc::new(Cell::new(0));
        let counted = Rc::clone(&builds);
        let mut switch = Switch::from_state(
            &tab,
            |tab| *tab,
            move |tab| {
                counted.set(counted.get() + 1);
                Box::new(TextInput::local().with_id(&format!("tab-{tab}"))) as Element
            },
        );
        let doc = automerge::AutoCommit::new();
        assert_eq!(builds.get(), 1);
        tab.set(1);
        Widget::sync(&mut switch, &doc, &automerge::ROOT);
        assert_eq!(builds.get(), 2);
        assert!(Widget::take_structure_dirty(&mut switch));
        assert_eq!(switch.current().test_id(), Some("tab-1"));
        tab.set(0);
        Widget::sync(&mut switch, &doc, &automerge::ROOT);
        assert_eq!(builds.get(), 2, "the first part was kept, not rebuilt");
        assert_eq!(switch.current().test_id(), Some("tab-0"));
        assert!(switch.part(&1).is_some());
    }
}
