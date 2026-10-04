//! State only: deadlines and fairness never confer owner admission.
pub(crate) struct OwnerTurns<T> {
    due: Option<T>,
    after: Option<u64>,
}
impl<T> Default for OwnerTurns<T> {
    fn default() -> Self {
        Self {
            due: None,
            after: None,
        }
    }
}
impl<T: Copy + Ord> OwnerTurns<T> {
    pub(crate) fn request(&mut self, due: T) {
        self.due = Some(self.due.map_or(due, |old| old.min(due)));
    }
    pub(crate) fn reconcile(&mut self, now: T, earliest: Option<T>) {
        self.due = earliest.map(|due| {
            if due <= now {
                self.due.map_or(due, |old| old.min(due))
            } else {
                due
            }
        });
    }
    pub(crate) fn due(&self) -> Option<T> {
        self.due
    }
    pub(crate) fn ready(&self, now: T) -> bool {
        self.due.is_some_and(|due| due <= now)
    }
    pub(crate) fn order(&self, ids: &mut [u64]) {
        ids.sort_unstable();
        if let Some(after) = self.after {
            let split = ids.partition_point(|id| *id <= after);
            ids.rotate_left(split);
        }
    }
    pub(crate) fn returned(&mut self, id: u64) {
        self.after = Some(id);
        self.due = None;
    }
}
#[cfg(test)]
mod tests {
    use super::OwnerTurns;
    #[test]
    fn early_input_and_late_proxy_preserve_absolute_demand() {
        let mut turns = OwnerTurns::<u64>::default();
        turns.reconcile(10, Some(10));
        for now in 11..100 {
            turns.request(now);
            turns.reconcile(now, Some(now));
            assert_eq!(turns.due(), Some(10));
        }
        turns.returned(2);
        turns.reconcile(100, None);
        turns.request(101); // Proxy delivered after AboutToWait's scan.
        assert!(!turns.ready(100));
        assert!(turns.ready(101));
    }
    #[test]
    fn blocked_owner_deadline_does_not_spin_and_closed_queues_quiesce() {
        let mut turns = OwnerTurns::<u64>::default();
        turns.request(1);
        turns.returned(1);
        turns.reconcile(1, Some(51));
        assert!(!turns.ready(50));
        assert!(turns.ready(51));
        turns.reconcile(52, None);
        assert_eq!(turns.due(), None);
    }
    #[test]
    fn ready_mounts_rotate_despite_closed_cursor_or_blocked_mount() {
        let mut turns = OwnerTurns::<u64>::default();
        let mut ids = [7, 2, 5];
        turns.order(&mut ids);
        assert_eq!(ids, [2, 5, 7]);
        turns.returned(5);
        turns.order(&mut ids);
        assert_eq!(ids, [7, 2, 5]);
        let mut current = [9, 2];
        turns.order(&mut current);
        assert_eq!(current, [9, 2]);
        turns.returned(9);
        turns.order(&mut current);
        assert_eq!(current, [2, 9]);
    }
    #[test]
    fn earlier_semantic_task_does_not_consume_future_owner_deadline() {
        let mut turns = OwnerTurns::<u64>::default();
        turns.request(50);
        assert!(!turns.ready(20));
        assert_eq!(turns.due(), Some(50));
        // Busy: no returned call, so the exact demand remains.
        assert!(turns.ready(50));
        assert_eq!(turns.due(), Some(50));
    }
}
