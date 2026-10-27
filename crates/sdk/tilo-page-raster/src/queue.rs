//! The render queue: each viewer's current batch of one kind of page work.

use std::collections::{HashMap, VecDeque};

/// Each session's (viewer window's) current batch of one kind of page work,
/// most important first, and the round-robin order sessions are served in,
/// so one window's batch cannot starve another's. A new batch replaces the
/// session's queued one: after a scroll or zoom, work that left the view is
/// never done ahead of the work that entered it.
pub struct BatchQueue<T> {
    queues: HashMap<u64, VecDeque<T>>,
    order: VecDeque<u64>,
}

impl<T> Default for BatchQueue<T> {
    fn default() -> Self {
        Self {
            queues: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<T: PartialEq> BatchQueue<T> {
    /// Replace `session`'s batch; an empty one cancels it.
    pub fn replace(&mut self, session: u64, items: VecDeque<T>) {
        if items.is_empty() {
            self.drop_session(session);
            return;
        }
        if !self.queues.contains_key(&session) {
            self.order.push_back(session);
        }
        self.queues.insert(session, items);
    }

    /// Queue one item behind `session`'s batch, unless it is already there
    /// or the batch holds `cap`.
    pub fn push(&mut self, session: u64, item: T, cap: usize) {
        if !self.queues.contains_key(&session) {
            self.order.push_back(session);
        }
        let queue = self.queues.entry(session).or_default();
        if !queue.contains(&item) && queue.len() < cap {
            queue.push_back(item);
        }
    }

    pub fn drop_session(&mut self, session: u64) {
        self.queues.remove(&session);
        self.order.retain(|queued| *queued != session);
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The next item, from the next session in turn.
    pub fn pop(&mut self) -> Option<(u64, T)> {
        while let Some(session) = self.order.pop_front() {
            let Some(queue) = self.queues.get_mut(&session) else {
                continue;
            };
            let Some(item) = queue.pop_front() else {
                self.queues.remove(&session);
                continue;
            };
            if queue.is_empty() {
                self.queues.remove(&session);
            } else {
                self.order.push_back(session);
            }
            return Some((session, item));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_replace_and_sessions_take_turns() {
        let mut queue = BatchQueue::default();
        queue.replace(1, [10, 11, 12].into());
        queue.replace(2, [20].into());
        queue.replace(1, [13, 14].into());
        assert_eq!(queue.pop(), Some((1, 13)));
        assert_eq!(queue.pop(), Some((2, 20)));
        queue.push(2, 21, 4);
        queue.push(2, 21, 4);
        assert_eq!(queue.pop(), Some((1, 14)));
        assert_eq!(queue.pop(), Some((2, 21)));
        assert_eq!(queue.pop(), None);
        queue.replace(3, [30].into());
        queue.replace(3, VecDeque::new());
        assert!(queue.is_empty());
    }
}
