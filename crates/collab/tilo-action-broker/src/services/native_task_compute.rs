//! Host-owned authenticated native task executor registrations.
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Private owner custody of one conscious Start. Never serialized or published.
/// Dropping this capability abandons enrollment without approving a run.
pub trait PendingNativeTaskStart: Send + Sync {
    /// Bind and approve only after revalidating the original request, frozen
    /// ceiling/frontier, exact executor certificate, model setup and cancellation.
    /// The receipt must validate against the original Start request.
    fn approve(&self, executor: NativeTaskExecutor) -> tilo_owner_commit::CommitFuture;
}

type ExecutorCheck = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;
type NativeTaskExecutorEntries =
    BTreeMap<(String, String), (Option<u64>, NativeTaskExecutor, Option<ExecutorCheck>)>;

/// Authenticated managed connections, including retained launch candidates.
/// Only the broker's protected subscription path may mutate this registry.
#[derive(Default)]
pub struct NativeTaskExecutorRegistry {
    entries: Mutex<NativeTaskExecutorEntries>,
    changes: tokio::sync::Notify,
}

/// Host observation, not a permission grant or model-readiness assertion.
#[derive(Clone)]
pub struct NativeTaskExecutor {
    /// Canonical managed workspace.
    pub room: String,
    /// Agent selected by the protected launch lifecycle.
    pub agent: String,
    /// Exact certificate observed at the authenticated subscription.
    pub certificate: tilo_user_identity::DeviceCert,
}

impl NativeTaskExecutorRegistry {
    pub(crate) fn register(
        &self,
        peer: u64,
        executor: NativeTaskExecutor,
        check: ExecutorCheck,
    ) -> Result<(), String> {
        self.entries
            .lock()
            .map_err(|_| "native executor registry unavailable")?
            .insert(
                (executor.room.clone(), executor.agent.clone()),
                (Some(peer), executor, Some(check)),
            );
        self.changes.notify_waiters();
        Ok(())
    }

    pub(crate) fn executor_for_peer(
        &self,
        peer: u64,
        room: &str,
        agent: &str,
    ) -> Option<NativeTaskExecutor> {
        let (current, executor, check) = self
            .entries
            .lock()
            .ok()?
            .get(&(room.into(), agent.into()))?
            .clone();
        (current == Some(peer) && (check?)()).then_some(executor)
    }

    pub(crate) fn registered_peer(&self, peer: u64, room: &str, agent: &str) -> bool {
        self.executor_for_peer(peer, room, agent).is_some()
    }

    pub(crate) fn disconnect(&self, peer: u64) {
        let mut retired = Vec::new();
        if let Ok(mut entries) = self.entries.lock() {
            for (connection, _, check) in entries.values_mut() {
                if *connection == Some(peer) {
                    // Retain the genuine managed registration for the existing
                    // protected relaunch lifecycle, never UI/presence liveness.
                    *connection = None;
                    retired.extend(check.take());
                }
            }
        }
        // Presentation retains certificates, not process descriptors or custody.
        drop(retired);
        self.changes.notify_waiters();
    }

    /// Waiters subscribe before sampling, so a subscription cannot be missed.
    pub(crate) fn changed(&self) -> impl std::future::Future<Output = ()> + '_ {
        let notified = self.changes.notified();
        Box::pin(async move {
            notified.await;
        })
    }

    pub(crate) fn live_executor(&self, room: &str, agent: &str) -> Option<NativeTaskExecutor> {
        let (peer, _, _) = self
            .entries
            .lock()
            .ok()?
            .get(&(room.into(), agent.into()))?
            .clone();
        self.executor_for_peer(peer?, room, agent)
    }

    /// Revalidate the exact protected live subscription immediately before an
    /// owner approval turn. Retained metadata is not a live lease.
    #[must_use]
    pub fn is_current(&self, executor: &NativeTaskExecutor) -> bool {
        self.live_executor(&executor.room, &executor.agent)
            .is_some_and(|current| current.certificate == executor.certificate)
    }

    /// Sample current owner policy over actual managed registrations. The model
    /// label must come from the host's Settings/gateway metadata check, never
    /// action arguments. This operation grants no authority.
    /// # Errors
    /// The caller certificate changed or current admission is unavailable.
    pub fn sample(
        &self,
        peer: &tilo_peer_state::StatePeer,
        governor: &tilo_user_identity::DeviceCert,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        configured_model: Option<&str>,
    ) -> Result<Vec<super::NativeTaskComputeRegistration>, String> {
        use tilo_action_policy::{ActionKind, TreePath};
        let now = std::time::SystemTime::now();
        let person = peer.permissions(caller.device_public_key, now)?;
        let certificates = peer.certificates();
        let actors = peer.governance().governed_agents(now)?;
        if !certificates.iter().any(|certificate| certificate == caller) {
            return Err("compute caller certificate changed".into());
        }
        let permitted = person.allows(&TreePath::room(room), ActionKind::Undoable, now);
        let mut workers = Vec::new();
        for executor in self.candidates(room)? {
            if executor.certificate.user_public_key != governor.user_public_key
                || !actors.iter().any(|actor| {
                    actor.agent_id == executor.agent
                        && actor.device_public_key == executor.certificate.device_public_key
                        && actor.acting
                })
                || !certificates
                    .iter()
                    .any(|certificate| certificate == &executor.certificate)
                || peer
                    .permissions(executor.certificate.device_public_key, now)
                    .is_err()
                || peer
                    .workspace()
                    .check_task_execution_actor(&executor.agent, None, now)
                    .is_err()
            {
                continue;
            }
            let current_certified_executor = self.is_current(&executor);
            workers.push(super::NativeTaskComputeRegistration {
                agent_id: executor.agent,
                workspace: room.into(),
                profile: configured_model.unwrap_or_default().into(),
                current_certified_executor,
                model_ready: configured_model.is_some(),
                permitted,
            });
        }
        Ok(workers)
    }

    /// Retained registrations. Check exact certificates and current permissions
    /// against the canonical owner on every use; retention is not authority.
    /// # Errors
    /// Registry locking failed.
    pub fn candidates(&self, room: &str) -> Result<Vec<NativeTaskExecutor>, String> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| "native executor registry unavailable")?
            .values()
            .filter(|(_, entry, _)| entry.room == room)
            .map(|(_, entry, _)| entry.clone())
            .collect())
    }
}
