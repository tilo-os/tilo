//! Test-only, one-shot barriers scoped to an exact temporary store and operation.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
};
type Key = (PathBuf, &'static str);
struct Gate {
    released: Mutex<bool>,
    release: Condvar,
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
fn gates() -> &'static Mutex<HashMap<Key, Arc<Gate>>> {
    static GATES: OnceLock<Mutex<HashMap<Key, Arc<Gate>>>> = OnceLock::new();
    GATES.get_or_init(Mutex::default)
}
pub(crate) struct Release {
    key: Key,
    gate: Arc<Gate>,
}
impl Drop for Release {
    fn drop(&mut self) {
        *self.gate.released.lock().unwrap() = true;
        self.gate.release.notify_all();
        gates().lock().unwrap().remove(&self.key);
    }
}
pub(crate) fn install(
    root: &Path,
    operation: &'static str,
) -> (Release, tokio::sync::oneshot::Receiver<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let key = (root.to_path_buf(), operation);
    let gate = Arc::new(Gate {
        released: Mutex::new(false),
        release: Condvar::new(),
        reached: Mutex::new(Some(tx)),
    });
    assert!(
        gates()
            .lock()
            .unwrap()
            .insert(key.clone(), gate.clone())
            .is_none()
    );
    (Release { key, gate }, rx)
}
pub(crate) fn signal(root: &Path, operation: &'static str) {
    let gate = gates()
        .lock()
        .unwrap()
        .get(&(root.to_path_buf(), operation))
        .cloned();
    if let Some(gate) = gate
        && let Some(reached) = gate.reached.lock().unwrap().take()
    {
        let _ = reached.send(());
    }
}
pub(crate) fn pause(root: &Path, operation: &'static str) {
    let gate = gates()
        .lock()
        .unwrap()
        .remove(&(root.to_path_buf(), operation));
    if let Some(gate) = gate {
        let reached = gate.reached.lock().unwrap().take();
        if let Some(reached) = reached {
            let _ = reached.send(());
        }
        let mut released = gate.released.lock().unwrap();
        while !*released {
            released = gate.release.wait(released).unwrap();
        }
    }
}

// Deterministic I/O faults, scoped to a temporary path and removed on unwind.
fn faults() -> &'static Mutex<std::collections::HashSet<Key>> {
    static FAULTS: OnceLock<Mutex<std::collections::HashSet<Key>>> = OnceLock::new();
    FAULTS.get_or_init(Mutex::default)
}
pub(crate) struct Fault(Key);
impl Drop for Fault {
    fn drop(&mut self) {
        faults().lock().unwrap().remove(&self.0);
    }
}
pub(crate) fn fail(root: &Path, operation: &'static str) -> Fault {
    let key = (root.to_path_buf(), operation);
    assert!(faults().lock().unwrap().insert(key.clone()));
    Fault(key)
}
pub(crate) fn check(root: &Path, operation: &'static str) -> std::io::Result<()> {
    pause(root, operation);
    if faults()
        .lock()
        .unwrap()
        .contains(&(root.to_path_buf(), operation))
    {
        Err(std::io::Error::other(format!(
            "injected {operation} failure"
        )))
    } else {
        Ok(())
    }
}
