//! A row open into its item where it stands: which row the list holds open, the height it
//! grows through on the way to its document, and the name it keeps over it.

use super::{Element, ListModel, VirtualList, Widget};
use crate::arriving::Arrival;
use crate::widget::Decorator;

/// How far a row's height may be from where it is going before it is drawn on its way there
/// ([`VirtualList::grown_height`]), in logical pixels.
const GROWN_EPSILON: f64 = 0.5;

impl<M: ListModel> VirtualList<M> {
    pub(super) fn opens_here(&self, index: usize) -> bool {
        self.in_place && self.model.opens_in_place(index)
    }

    pub(super) fn opens_item(&self, index: usize) -> bool {
        self.model.opens_in_place(index)
    }

    #[must_use]
    pub fn open_row(&self) -> Option<&M::Key> {
        self.open.as_ref()
    }

    pub fn set_open(&mut self, key: Option<M::Key>) {
        if self.open == key {
            return;
        }
        let closed = self.open.take();
        let opened = key.filter(|key| {
            self.model
                .index_of(key)
                .is_some_and(|index| self.opens_here(index))
        });
        self.open = opened;
        let changed: Vec<M::Key> = closed.into_iter().chain(self.open.clone()).collect();
        self.growing.extend(changed.iter().cloned());
        self.invalidate_rows(changed);
    }

    /// Hold the item `item` open where its row stands, as a host keeping which item is open asks
    /// it to, or none. An item no row of the list shows closes the row standing open. An item
    /// the rows cannot answer for yet — the app is reading them, or its row has not arrived, so
    /// whether it opens where it stands is unknown — leaves the row standing open as it is, and
    /// opens as its row arrives: a window that moves to another place to show an item names
    /// the item before the place's rows are there.
    pub fn hold_open(&mut self, item: Option<&str>) {
        self.held_open = None;
        let Some(item) = item else {
            self.set_open(None);
            return;
        };
        let index = self
            .model
            .key_presenting(item)
            .and_then(|key| Some((self.model.index_of(&key)?, key)));
        match index {
            Some((index, key)) if matches!(self.model.row_state(index), Arrival::Ready(())) => {
                self.set_open(Some(key));
            }
            Some(_) => self.held_open = Some(item.to_owned()),
            None if self.model.len() > 0 && self.model.key_at(0).is_none() => {
                self.held_open = Some(item.to_owned());
            }
            None => self.set_open(None),
        }
    }

    pub(super) fn open_held(&mut self) {
        if let Some(item) = self.held_open.take() {
            self.hold_open(Some(&item));
        }
    }

    pub(super) fn open_where_it_stands(&mut self, key: M::Key) {
        self.held_open = None;
        let closing = self.open.as_ref() == Some(&key);
        self.set_open(if closing { None } else { Some(key) });
        self.report_open();
    }

    pub(super) fn close_open(&mut self) -> bool {
        self.held_open = None;
        if self.open.is_none() {
            return false;
        }
        self.set_open(None);
        self.report_open();
        true
    }

    pub(super) fn report_open(&mut self) {
        match self.open.clone() {
            Some(key) => {
                if let Some(callback) = &mut self.on_opened {
                    callback(key);
                }
            }
            None => {
                if let Some(callback) = &mut self.on_closed {
                    callback();
                }
            }
        }
    }

    /// The height a row stands at this frame on its way to `target`: a row
    /// whose height changed (it opened into its item, or closed again) grows
    /// or shrinks on the expand/collapse motion ([`crate::motion::Disclose`]),
    /// and the rows after it move with its edge, so opening a row pushes the
    /// list down rather than jumping it there. A row measured for the first
    /// time was never on screen at another height and stands at `target` at
    /// once.
    pub(super) fn grown_height(
        &mut self,
        index: usize,
        key: &M::Key,
        target: f64,
        first: bool,
    ) -> f64 {
        let at = self.growth.iter().position(|(grown, _)| grown == key);
        // Only a row the list opens or closes into its item moves its height: content that
        // changes under a row stands at its new height at once, and a row measured the first
        // time was never on screen at another.
        if first || self.reduce_motion || !self.growing.contains(key) {
            if let Some(at) = at {
                self.growth.remove(at);
            }
            self.growing.remove(key);
            return target;
        }
        let now = crate::motion::frame_now();
        let Some(at) = at else {
            let from = self.viewport.item_height(index).unwrap_or(target);
            if (target - from).abs() < GROWN_EPSILON {
                self.growing.remove(key);
                return target;
            }
            let timing = crate::motion::Disclose::timing(
                from,
                target,
                self.motion,
                crate::motion::input_source(),
            );
            if timing.is_instant() {
                self.growing.remove(key);
                return target;
            }
            let mut motion =
                crate::motion::MotionValue::new(from).with_tolerance(GROWN_EPSILON / 2.0);
            motion.set(target, timing, now);
            self.growth.push((key.clone(), motion));
            return motion.get(now);
        };
        let motion = &mut self.growth[at].1;
        let timing = crate::motion::Disclose::timing(
            motion.get(now),
            target,
            self.motion,
            crate::motion::input_source(),
        );
        motion.set(target, timing, now);
        if motion.is_moving(now) {
            motion.get(now)
        } else {
            self.growth.remove(at);
            self.growing.remove(key);
            target
        }
    }
}

pub(crate) struct RowOpen {
    inner: Element,
    tree: bool,
}

impl RowOpen {
    pub(crate) fn new(inner: Element, tree: bool) -> Self {
        Self { inner, tree }
    }
}

impl Decorator for RowOpen {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }

    fn name(&self) -> &'static str {
        "RowOpen"
    }

    fn is_item_opened_in_place(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let document = self.inner.as_ref();
        let name = first_page_title(document)
            .or_else(|| document.accessibility().and_then(|node| node.name))?;
        Some(
            crate::a11y::AccessNode::new(if self.tree {
                crate::a11y::Role::TreeItem
            } else {
                crate::a11y::Role::ListItem
            })
            .with_name(name)
            .with_state(crate::a11y::AccessState::new().expanded(true)),
        )
    }
}

pub(super) fn first_page_title(widget: &dyn Widget) -> Option<String> {
    Widget::own_page_title(widget).or_else(|| {
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = first_page_title(child);
            }
        });
        found
    })
}
