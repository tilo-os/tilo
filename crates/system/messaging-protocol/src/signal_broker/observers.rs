//! Wake-ups contain no account, recipient or plaintext.
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
type Listener = Arc<dyn Fn() + Send + Sync>;
#[derive(Default)]
pub(crate) struct Observers {
    next: AtomicU64,
    listeners: Mutex<BTreeMap<u64, Listener>>,
}
impl Observers {
    pub(crate) fn observe(
        self: &Arc<Self>,
        listener: Listener,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        let id = self
            .next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_add(1))
            .ok()?;
        let mut listeners = self.listeners.lock().ok()?;
        if listeners.len() >= 128 {
            return None;
        }
        listeners.insert(id, listener);
        drop(listeners);
        Some(Box::new(Guard {
            source: Arc::downgrade(self),
            id,
        }))
    }
    pub(crate) fn publish(&self) {
        let callbacks = self
            .listeners
            .lock()
            .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for callback in callbacks {
            callback();
        }
    }
}
struct Guard {
    source: Weak<Observers>,
    id: u64,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(source) = self.source.upgrade()
            && let Ok(mut listeners) = source.listeners.lock()
        {
            listeners.remove(&self.id);
        }
    }
}
