//! A bounded bridge from the existing native-owner fence to existing run leases.
//! No new admission token, epoch or authority is introduced here.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tilo_actions::remote::Caller;
use tilo_desktop_model::RoomId;
use tilo_joined_room::{NativeFenceSubscription, NativeWorkspaceOwner};
use tilo_lock::MutexRecover as _;

const MAX_EXECUTIONS: usize = 512;

pub(super) struct ExecutionPins {
    entries: Mutex<HashMap<uuid::Uuid, ExecutionEntry>>,
    subscription: OnceLock<NativeFenceSubscription>,
}

struct ExecutionEntry {
    room: RoomId,
    lease: tilo_actions::remote::ExecutionLease,
    admission: Weak<super::joined::SessionAdmission>,
}

impl ExecutionPins {
    pub(super) fn new(owner: &NativeWorkspaceOwner) -> Arc<Self> {
        let pins = Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            subscription: OnceLock::new(),
        });
        let weak = Arc::downgrade(&pins);
        let subscription = owner.observe_fences(Arc::new(move |room, _continuity| {
            if let Some(pins) = weak.upgrade() {
                pins.fence(room);
            }
        }));
        // This is a fresh OnceLock, before the pins are exposed to any caller.
        let _ = pins.subscription.set(subscription);
        pins
    }

    fn fence(&self, room: RoomId) {
        // The native owner calls synchronously under its continuity lock. Only
        // atomic lease closure happens here: no owner reread, SDK or user code.
        let entries = self.entries.lock_recover();
        for entry in entries.values() {
            if entry.room == room {
                entry.lease.close();
            }
        }
    }

    pub(super) fn register_checked(
        self: &Arc<Self>,
        room: RoomId,
        caller: &Caller,
        recheck: impl FnOnce() -> Result<(), String>,
    ) -> Result<ExecutionRegistration, String> {
        let result = (|| {
            if caller.room() != Some(room.to_string().as_str()) || !caller.execution_is_live() {
                return Err("execution audience or lifetime unavailable".into());
            }
            let id = caller
                .execution_id()
                .ok_or("execution identity unavailable")?;
            let lease = caller
                .execution_lease()
                .ok_or("execution lifetime unavailable")?;
            {
                let mut entries = self
                    .entries
                    .lock()
                    .map_err(|_| "execution registry unavailable")?;
                if entries.len() >= MAX_EXECUTIONS || entries.contains_key(&id) {
                    return Err("execution registration unavailable".into());
                }
                entries.insert(
                    id,
                    ExecutionEntry {
                        room,
                        lease,
                        admission: Weak::new(),
                    },
                );
            }
            let registration = ExecutionRegistration {
                caller: caller.clone(),
                pins: Arc::downgrade(self),
            };
            // Subscription and entry already exist. A fence during recheck
            // closes this very lease; no lock is held while calling recheck.
            recheck()?;
            if !caller.execution_is_live() {
                return Err("execution fenced during admission".into());
            }
            Ok(registration)
        })();
        if result.is_err() {
            caller.retire_execution();
        }
        result
    }

    pub(super) fn attach(
        &self,
        admission: &Arc<super::joined::SessionAdmission>,
    ) -> Result<(), String> {
        let id = admission
            .caller()
            .execution_id()
            .ok_or("execution identity unavailable")?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "execution registry unavailable")?;
        let entry = entries
            .get_mut(&id)
            .ok_or("execution registration unavailable")?;
        if !entry.lease.is_live() {
            return Err("execution fenced before attachment".into());
        }
        entry.admission = Arc::downgrade(admission);
        drop(entries);
        Ok(())
    }

    pub(super) fn admissions(
        &self,
        room: RoomId,
    ) -> Result<Vec<Arc<super::joined::SessionAdmission>>, String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "execution registry unavailable")?;
        Ok(entries
            .values()
            .filter(|entry| entry.room == room && entry.lease.is_live())
            .filter_map(|entry| entry.admission.upgrade())
            .collect())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

impl Drop for ExecutionPins {
    fn drop(&mut self) {
        for entry in self
            .entries
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            entry.lease.close();
        }
        // Dropping subscription unregisters its strong observer; the owner
        // retains only a weak reference. There is one subscription per owner.
    }
}

pub(super) struct ExecutionRegistration {
    caller: Caller,
    pins: Weak<ExecutionPins>,
}
impl Drop for ExecutionRegistration {
    fn drop(&mut self) {
        self.caller.retire_execution();
        if let Some(pins) = self.pins.upgrade()
            && let Some(id) = self.caller.execution_id()
        {
            pins.entries.lock_recover().remove(&id);
        }
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
