//! Bounded ephemeral context custody; entries confer no enrollment authority.
use crate::peer_delivery::incoming::IncomingBudget;
use automerge::resource_limits::NativeReservation;
use std::{cell::Cell, collections::BTreeMap, rc::Rc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub id: u32,
    pub generation: u64,
}
pub(crate) trait Context {
    fn deadline(&self) -> u64;
    fn cancel(&self);
    fn budget(&self) -> &IncomingBudget;
}
impl Context for crate::peer_onboarding::LocalContext {
    fn deadline(&self) -> u64 {
        self.expires_at_ns()
    }
    fn cancel(&self) {
        self.cancel().cancel();
    }
    fn budget(&self) -> &IncomingBudget {
        self.budget()
    }
}
pub(crate) struct Entry<T> {
    pub context: T,
    pub owner: Option<Owner>,
    pub alive: Rc<Cell<bool>>,
    busy: Cell<bool>,
    _credit: NativeReservation,
}
pub(crate) struct Table<T> {
    entries: BTreeMap<u32, Rc<Entry<T>>>,
    next: u32,
}
impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            next: 1,
        }
    }
}
impl<T: Context> Table<T> {
    pub fn insert(&mut self, context: T, owner: Option<Owner>, now: u64) -> Result<u32, String> {
        self.entries.retain(|_, entry| {
            if now >= entry.context.deadline() && !entry.busy.get() {
                if entry.alive.replace(false) {
                    entry.context.cancel();
                }
                false
            } else {
                true
            }
        });
        if now >= context.deadline() || self.entries.len() >= 8 {
            return Err("local onboarding context expired or capacity exhausted".into());
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or("local onboarding handle exhausted")?;
        // Fixed conservative metadata credit covers this bounded map and Rc nodes.
        let credit = context.budget().reserve(8192)?;
        let id = self.next;
        self.entries.insert(
            id,
            Rc::new(Entry {
                context,
                owner,
                alive: Rc::new(Cell::new(true)),
                busy: Cell::new(false),
                _credit: credit,
            }),
        );
        self.next = next;
        Ok(id)
    }
    pub fn acquire(
        &mut self,
        id: u32,
        owner: Option<Owner>,
        now: u64,
    ) -> Result<Rc<Entry<T>>, String> {
        let entry = self
            .entries
            .get(&id)
            .ok_or("unknown local onboarding context")?;
        if entry.owner != owner
            || !entry.alive.get()
            || entry.busy.get()
            || now >= entry.context.deadline()
        {
            return Err("local onboarding context differs, expired or consumed".into());
        }
        entry.busy.set(true);
        Ok(Rc::clone(entry))
    }
    pub fn cancel(&mut self, id: u32) {
        if let Some(entry) = self.entries.get(&id) {
            if entry.alive.replace(false) {
                entry.context.cancel();
            }
            if !entry.busy.get() {
                self.entries.remove(&id);
            }
        }
    }
    pub fn settle(&mut self, id: u32, entry: &Rc<Entry<T>>) {
        if self
            .entries
            .get(&id)
            .is_some_and(|current| Rc::ptr_eq(current, entry))
        {
            if entry.alive.replace(false) {
                entry.context.cancel();
            }
            self.entries.remove(&id);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Held {
        budget: IncomingBudget,
        cancelled: Rc<Cell<usize>>,
        deadline: u64,
    }
    impl Context for Held {
        fn deadline(&self) -> u64 {
            self.deadline
        }
        fn cancel(&self) {
            self.cancelled.set(self.cancelled.get() + 1);
        }
        fn budget(&self) -> &IncomingBudget {
            &self.budget
        }
    }
    fn held(budget: &IncomingBudget) -> Held {
        Held {
            budget: budget.clone(),
            cancelled: Rc::new(Cell::new(0)),
            deadline: 100,
        }
    }
    #[test]
    fn context_cancel_keeps_busy_slot_and_credit_until_raw_settlement() {
        let budget = IncomingBudget::new(128 * 1024).unwrap();
        let free = budget.available();
        let mut table = Table::default();
        let id = table.insert(held(&budget), None, 1).unwrap();
        let entry = table.acquire(id, None, 1).unwrap();
        assert!(budget.available() < free);
        table.cancel(id);
        assert!(!entry.alive.get());
        assert_eq!(entry.context.cancelled.get(), 1);
        assert_eq!(
            table.entries.len(),
            1,
            "cancelled raw operation still consumes its bounded slot"
        );
        for _ in 1..8 {
            table.insert(held(&budget), None, 1).unwrap();
        }
        assert!(table.insert(held(&budget), None, 1).is_err());
        table.settle(id, &entry);
        assert_eq!(table.entries.len(), 7);
        drop(entry);
        for id in table.entries.keys().copied().collect::<Vec<_>>() {
            table.cancel(id);
        }
        assert_eq!(budget.available(), free);
    }
    #[test]
    fn context_generation_expiry_and_monotonic_handles_fence_late_owners() {
        let budget = IncomingBudget::new(128 * 1024).unwrap();
        let mut table = Table::default();
        let owner = Owner {
            id: 1,
            generation: 2,
        };
        let id = table.insert(held(&budget), Some(owner), 1).unwrap();
        assert!(
            table
                .acquire(
                    id,
                    Some(Owner {
                        id: 1,
                        generation: 3
                    }),
                    1
                )
                .is_err()
        );
        assert!(table.acquire(id, None, 1).is_err());
        let entry = table.acquire(id, Some(owner), 1).unwrap();
        assert!(table.acquire(id, Some(owner), 1).is_err());
        table.cancel(id);
        let newer = table.insert(held(&budget), Some(owner), 1).unwrap();
        assert_ne!(id, newer);
        table.settle(id, &entry);
        assert!(table.entries.contains_key(&newer));
        assert!(table.acquire(newer, Some(owner), 100).is_err());
        table.next = u32::MAX;
        assert!(table.insert(held(&budget), None, 1).is_err());
    }
}
