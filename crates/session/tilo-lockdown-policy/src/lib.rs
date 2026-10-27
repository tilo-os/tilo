//! Durable lockdown policy persistence and ordered local observation.
//!
//! Every store opened on the same path shares one commit lane, so observers
//! see each committed policy in order, restrictions before later relaxations.
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tilo_lock::MutexRecover as _;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent persisted lockdown switches, one per portal capability"
)]
pub struct LockdownPolicy {
    pub disable_printing: bool,
    pub disable_save_to_disk: bool,
    pub disable_application_handlers: bool,
    pub disable_location: bool,
    pub disable_camera: bool,
    pub disable_microphone: bool,
    pub disable_sound_output: bool,
}

pub type PolicyListener = dyn Fn(&LockdownPolicy) + Send + Sync;
struct SharedPolicy {
    commit: Mutex<()>,
    delivery: Mutex<()>,
    policy: Mutex<LockdownPolicy>,
    pending: Mutex<VecDeque<LockdownPolicy>>,
    listeners: Mutex<Vec<Weak<PolicyListener>>>,
}
static POLICIES: OnceLock<Mutex<std::collections::BTreeMap<PathBuf, Arc<SharedPolicy>>>> =
    OnceLock::new();
thread_local! { static DELIVERING: Cell<bool> = const { Cell::new(false) }; }
struct DeliveryGuard;
impl Drop for DeliveryGuard {
    fn drop(&mut self) {
        DELIVERING.set(false);
    }
}

/// Callbacks may inspect current policy or drop observers. Synchronous writes
/// and registration from inside a callback are rejected before taking locks.
fn require_non_reentrant() -> std::io::Result<()> {
    if DELIVERING.get() {
        Err(failed(
            "policy mutation or registration during observer delivery is unavailable",
        ))
    } else {
        Ok(())
    }
}
fn path_identity(path: PathBuf) -> PathBuf {
    let path = std::path::absolute(&path).unwrap_or(path);
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        // The first observer may register before persist creates the portal
        // directories. Resolve the nearest existing ancestor so creation does
        // not change the key when an earlier ancestor is a symlink.
        for ancestor in parent.ancestors() {
            if let (Ok(mut resolved), Ok(tail)) =
                (ancestor.canonicalize(), parent.strip_prefix(ancestor))
            {
                for component in tail.components() {
                    match component {
                        std::path::Component::Normal(name) => resolved.push(name),
                        std::path::Component::ParentDir => {
                            resolved.pop();
                        }
                        std::path::Component::CurDir => {}
                        _ => return path,
                    }
                }
                return resolved.join(name);
            }
        }
    }
    path
}
fn shared_policy(path: &PathBuf) -> Arc<SharedPolicy> {
    let mut policies = POLICIES.get_or_init(Mutex::default).lock_recover();
    policies.retain(|_, state| {
        Arc::strong_count(state) > 1
            || state
                .listeners
                .lock_recover()
                .iter()
                .any(|listener| listener.strong_count() != 0)
    });
    policies
        .entry(path.clone())
        .or_insert_with(|| {
            Arc::new(SharedPolicy {
                commit: Mutex::new(()),
                delivery: Mutex::new(()),
                policy: Mutex::new(
                    fs::read(path)
                        .ok()
                        .and_then(|value| serde_json::from_slice(&value).ok())
                        .unwrap_or_default(),
                ),
                pending: Mutex::new(VecDeque::new()),
                listeners: Mutex::new(Vec::new()),
            })
        })
        .clone()
}

/// Register `listener` for every policy committed at `path`, delivering the
/// current policy to it first.
pub fn watch_policy_at(path: PathBuf, listener: &Arc<PolicyListener>) -> std::io::Result<()> {
    watch_policy_with_hook(path, listener, || ())
}
fn watch_policy_with_hook(
    path: PathBuf,
    listener: &Arc<PolicyListener>,
    after_registration: impl FnOnce(),
) -> std::io::Result<()> {
    require_non_reentrant()?;
    let store = PolicyStore::at_path(path);
    {
        let mut listeners = store
            .shared
            .listeners
            .lock()
            .map_err(|_| failed("policy observer lock poisoned"))?;
        listeners.retain(|entry| entry.strong_count() != 0);
        listeners.push(Arc::downgrade(listener));
    }
    after_registration();
    store.deliver(Some(listener))
}

#[derive(Clone)]
pub struct PolicyStore {
    shared: Arc<SharedPolicy>,
    path: PathBuf,
}
impl PolicyStore {
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path_identity(path.into());
        Self {
            shared: shared_policy(&path),
            path,
        }
    }
    pub fn policy(&self) -> LockdownPolicy {
        self.shared.policy.lock_recover().clone()
    }
    /// Durably commit `policy`, then deliver it to every observer in commit order.
    ///
    /// # Errors
    ///
    /// Fails when called from inside an observer callback, when the delivery
    /// queue is full, or when the policy cannot be persisted.
    pub fn set_policy(&self, policy: LockdownPolicy) -> std::io::Result<()> {
        self.set_policy_with_hook(policy, || ())
    }
    fn set_policy_with_hook(
        &self,
        policy: LockdownPolicy,
        after_commit: impl FnOnce(),
    ) -> std::io::Result<()> {
        require_non_reentrant()?;
        {
            let _commit = self
                .shared
                .commit
                .lock()
                .map_err(|_| failed("policy commit lock poisoned"))?;
            let mut pending = self
                .shared
                .pending
                .lock()
                .map_err(|_| failed("policy delivery queue poisoned"))?;
            // A restriction is an irreversible stop obligation, so do not
            // coalesce commits. Refuse overload before touching durable state.
            if pending.len() >= 32 {
                return Err(failed("policy delivery queue full"));
            }
            persist(&self.path, &policy)?;
            *self
                .shared
                .policy
                .lock()
                .map_err(|_| failed("policy state lock poisoned"))? = policy.clone();
            pending.push_back(policy);
        }
        after_commit();
        self.deliver(None)
    }
    fn deliver(&self, only: Option<&Arc<PolicyListener>>) -> std::io::Result<()> {
        require_non_reentrant()?;
        let _delivery = self
            .shared
            .delivery
            .lock()
            .map_err(|_| failed("policy delivery lock poisoned"))?;
        DELIVERING.set(true);
        let _active = DeliveryGuard;
        let mut initial = only;
        loop {
            let (policy, initial_only) = {
                let mut pending = self
                    .shared
                    .pending
                    .lock()
                    .map_err(|_| failed("policy delivery queue poisoned"))?;
                if let Some(policy) = pending.pop_front() {
                    (policy, None)
                } else if let Some(listener) = initial.take() {
                    // Queue and snapshot share the commit boundary. Newer
                    // commits can enqueue, but cannot deliver ahead of this.
                    (self.policy(), Some(listener.clone()))
                } else {
                    break;
                }
            };
            let listeners = if let Some(listener) = initial_only {
                vec![listener]
            } else {
                self.shared
                    .listeners
                    .lock()
                    .map_err(|_| failed("policy observer lock poisoned"))?
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect()
            };
            // Neither the commit, queue, policy nor observer lock is held.
            // All committed edges are delivered in order, including disables
            // followed immediately by relaxations. Delayed writers only drain
            // outstanding obligations; they never replay their old payload.
            for listener in listeners {
                listener(&policy);
            }
        }
        Ok(())
    }
}
fn persist(path: &Path, policy: &LockdownPolicy) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(policy)
        .map_err(|e| failed(&format!("cannot encode lockdown policy: {e}")))?;
    tilo_files::native::atomic::write_private(path, &encoded)
        .map_err(|e| failed(&format!("cannot commit lockdown policy: {e}")))
}
fn failed(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}
#[cfg(test)]
mod tests;
