//! A row open into its item where it stands: which row the list holds open, and the height it
//! grows through on the way to its document.

use super::{ListModel, VirtualList};

/// How far a row's height may be from where it is going before it is drawn on its way there
/// ([`VirtualList::grown_height`]), in logical pixels.
const GROWN_EPSILON: f64 = 0.5;

impl<M: ListModel> VirtualList<M> {
    /// The row standing open into its item, if one does: the item the person is reading
    /// where it stands, which the row builder draws its document into.
    #[must_use]
    pub fn open_row(&self) -> Option<&M::Key> {
        self.open.as_ref()
    }

    /// Open `key` into its item where it stands, or close it when it is the row standing open:
    /// the list's own answer to choosing a row that
    /// [`opens in place`](ListModel::opens_in_place), and how a host opens an item from
    /// elsewhere in the window. The two rows it changes build anew, so the one that opens
    /// grows into its document and the one that closes shuts it away.
    pub fn set_open(&mut self, key: Option<M::Key>) {
        if self.open == key {
            return;
        }
        let closed = self.open.take();
        let opened = key.filter(|key| {
            self.model
                .index_of(key)
                .is_some_and(|index| self.model.opens_in_place(index))
        });
        self.open = opened;
        let changed: Vec<M::Key> = closed.into_iter().chain(self.open.clone()).collect();
        self.growing.extend(changed.iter().cloned());
        self.invalidate_rows(changed);
    }

    /// The person opened `key` where it stands, or closed it when it was the row standing open:
    /// the row grows into its document, and a host keeping which item is open hears it as
    /// [`VirtualList::on_opened`].
    pub(super) fn open_where_it_stands(&mut self, key: M::Key) {
        let closing = self.open.as_ref() == Some(&key);
        self.set_open(if closing { None } else { Some(key) });
        self.report_open();
    }

    /// The person closed the row standing open (Escape, or a press beside it), if one stands
    /// open. Whether it did.
    pub(super) fn close_open(&mut self) -> bool {
        if self.open.is_none() {
            return false;
        }
        self.set_open(None);
        self.report_open();
        true
    }

    /// Tell the host what the person made of the row standing open: the item it opened, or
    /// that it closed.
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
