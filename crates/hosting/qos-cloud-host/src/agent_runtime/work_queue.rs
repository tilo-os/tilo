//! Process-local scheduling of the existing durable private admissions.
use qos_peer_state::owner_commit::MAX_PRIVATE_AGENT_ADMISSIONS;
use std::collections::{BTreeSet, VecDeque};
use uuid::Uuid;
pub(super) const RECOVERY_BATCH: usize = 8;
#[derive(Default)]
pub(super) struct AgentWorkQueue {
    pending: VecDeque<Uuid>,
    running: BTreeSet<Uuid>,
    cursor: Option<Uuid>,
}
impl AgentWorkQueue {
    /// The serial owner calls this before a new durable admission; no await or
    /// other queue writer intervenes before enqueue. Retained retries coalesce.
    pub(super) fn reserve_new_slot(&self) -> Result<(), String> {
        if self.pending.len() + self.running.len() >= MAX_PRIVATE_AGENT_ADMISSIONS {
            return Err("private execution work capacity unavailable".into());
        }
        Ok(())
    }
    pub(super) fn enqueue(&mut self, id: Uuid) -> Result<(), String> {
        if self.pending.contains(&id) || self.running.contains(&id) {
            return Ok(());
        }
        self.reserve_new_slot()?;
        self.pending.push_back(id);
        Ok(())
    }
    pub(super) fn pop_front(&mut self) -> Option<Uuid> {
        self.pending.pop_front()
    }
    pub(super) fn rescan(&mut self, owner: &super::RuntimeOwner) -> Result<(), String> {
        let candidates = owner.pending_spawns(self.cursor, RECOVERY_BATCH)?;
        self.cursor = candidates.last().copied(); // Empty suffix wraps on the next bounded pass.
        for id in candidates {
            self.enqueue(id)?;
        }
        Ok(())
    }
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn synchronize(&mut self, running: impl Iterator<Item = Uuid>) {
        self.running = running.collect();
        self.pending.retain(|id| !self.running.contains(id));
    }
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn started(&mut self, id: Uuid) {
        self.running.insert(id);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_prelaunch_recovery_queue_coalesces_and_bounds_work() {
        let mut queue = AgentWorkQueue::default();
        for i in 1..=MAX_PRIVATE_AGENT_ADMISSIONS {
            queue.enqueue(Uuid::from_u128(i as u128)).unwrap();
            queue.enqueue(Uuid::from_u128(i as u128)).unwrap();
        }
        assert!(queue.reserve_new_slot().is_err());
        assert!(queue.enqueue(Uuid::from_u128(9999)).is_err());
        assert_eq!(queue.pending.len(), MAX_PRIVATE_AGENT_ADMISSIONS);
        let first = queue.pop_front().unwrap();
        queue.started(first);
        queue.enqueue(first).unwrap();
        assert_eq!(queue.pending.len(), MAX_PRIVATE_AGENT_ADMISSIONS - 1);
        assert!(queue.reserve_new_slot().is_err());
        assert!(queue.enqueue(Uuid::from_u128(9999)).is_err());
        queue.synchronize(std::iter::empty()); // Failed connection releases only process-local scheduling.
        queue.enqueue(first).unwrap();
        assert_eq!(queue.pending.back(), Some(&first));
    }
}
