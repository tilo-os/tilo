//! The original live slot and joined connector own the process-local retry.
use super::*;
use crate::agent_runtime_owner::FailedLaunchAppend;
use qos_peer_state::owner_commit::AgentCommitError;
use std::{os::unix::net::UnixStream, sync::Weak};

pub(crate) enum LiveLaunchSlot {
    Empty,
    Permit(AgentLaunchPermit),
    Retry(QuiescentLaunchRetry),
}
/// No Clone, data constructor, or public conversion from a failed write.
pub(crate) struct QuiescentLaunchRetry {
    attempt: Weak<LiveAttempt>,
    failed: FailedLaunchAppend,
}
impl QuiescentLaunchRetry {
    pub(crate) fn validate(
        &self,
        custody: &AgentAttemptCustody,
        store: &Store,
    ) -> Result<(), AgentCommitError> {
        let attempt = self
            .attempt
            .upgrade()
            .ok_or_else(|| AgentCommitError::Rejected("original launch attempt retired".into()))?;
        if !std::ptr::eq(&attempt.custody, custody) || !self.failed.belongs_to(&attempt) {
            return Err(AgentCommitError::Rejected(
                "retry belongs to another live attempt".into(),
            ));
        }
        self.failed.validate(custody, store)
    }
}

pub(super) fn take_permit(attempt: &Arc<LiveAttempt>) -> Result<AgentLaunchPermit, String> {
    let mut slot = attempt.launch.lock().map_err(|_| "launch slot poisoned")?;
    if !matches!(*slot, LiveLaunchSlot::Permit(_)) {
        return Err("fresh launch permit absent; status only".into());
    }
    let LiveLaunchSlot::Permit(permit) = std::mem::replace(&mut *slot, LiveLaunchSlot::Empty)
    else {
        unreachable!()
    };
    Ok(permit)
}
fn take_retry(attempt: &Arc<LiveAttempt>) -> Result<Option<QuiescentLaunchRetry>, String> {
    let mut slot = attempt.launch.lock().map_err(|_| "launch slot poisoned")?;
    match &*slot {
        LiveLaunchSlot::Empty => Ok(None),
        LiveLaunchSlot::Permit(_) => Err("launch permit already retained".into()),
        LiveLaunchSlot::Retry(_) => {
            let LiveLaunchSlot::Retry(retry) = std::mem::replace(&mut *slot, LiveLaunchSlot::Empty)
            else {
                unreachable!()
            };
            Ok(Some(retry))
        }
    }
}
fn restore_retry(attempt: &Arc<LiveAttempt>, retry: QuiescentLaunchRetry) -> Result<(), String> {
    if !retry
        .attempt
        .upgrade()
        .is_some_and(|original| Arc::ptr_eq(&original, attempt))
    {
        return Err("retry belongs to another live attempt".into());
    }
    let mut slot = attempt.launch.lock().map_err(|_| "launch slot poisoned")?;
    if !matches!(*slot, LiveLaunchSlot::Empty) {
        return Err("launch slot changed while retry was checked out".into());
    }
    *slot = LiveLaunchSlot::Retry(retry);
    Ok(())
}

/// Errors retain failed-call provenance until the joined descriptor owner handles it.
enum PrepareLaunchError {
    Unavailable(String),
    Failed(FailedLaunchAppend),
}
impl RuntimeOwner {
    fn original_attempt(&self, attempt: &Arc<LiveAttempt>) -> Result<(), String> {
        let live = self.live.lock().map_err(|_| "live registry poisoned")?;
        if !live
            .attempts
            .get(&attempt.custody.id().invocation())
            .is_some_and(|original| Arc::ptr_eq(original, attempt))
        {
            return Err("original launch attempt no longer retained".into());
        }
        Ok(())
    }
    fn retain_retry(
        &self,
        attempt: &Arc<LiveAttempt>,
        retry: QuiescentLaunchRetry,
    ) -> Result<(), String> {
        self.original_attempt(attempt)?;
        restore_retry(attempt, retry)
    }
    fn prepare_launch(
        &self,
        attempt: &Arc<LiveAttempt>,
    ) -> Result<ManagedBinding, PrepareLaunchError> {
        self.original_attempt(attempt)
            .map_err(PrepareLaunchError::Unavailable)?;
        let retry = take_retry(attempt).map_err(PrepareLaunchError::Unavailable)?;
        #[cfg(test)]
        if retry.is_some() {
            test_audit(self, attempt, "retry_taken");
        }
        // No slot/live lock is held while the exact original Source reopens Store.
        let preflight = (|| {
            self.validate_policy_time(&attempt.policy_time)?;
            let store = self.store_for(&attempt.policy_time)?;
            self.validate_spawn_current(&store, &attempt.custody.id(), &attempt.policy_time)?;
            let caller = store.agent_caller(attempt.caller.device_public_key)?;
            let runtime = self.runtime()?;
            Ok::<_, String>((store, caller, runtime))
        })();
        let (mut store, caller, runtime) = match preflight {
            Ok(value) => value,
            Err(error) => {
                if let Some(retry) = retry {
                    self.retain_retry(attempt, retry)
                        .map_err(PrepareLaunchError::Unavailable)?;
                }
                return Err(PrepareLaunchError::Unavailable(error));
            }
        };
        #[cfg(not(test))]
        let outcome = store.claim_agent_launch_for_runtime(&caller, attempt, runtime, retry);
        #[cfg(test)]
        let outcome = test_claim(self, || {
            store.claim_agent_launch_for_runtime(&caller, attempt, runtime, retry)
        });
        drop(store);
        match outcome {
            RuntimeLaunchClaim::Fresh { permit, receipt: _ } => {
                self.original_attempt(attempt)
                    .map_err(PrepareLaunchError::Unavailable)?;
                let binding = permit.binding().clone();
                let mut slot = attempt
                    .launch
                    .lock()
                    .map_err(|_| PrepareLaunchError::Unavailable("launch slot poisoned".into()))?;
                if !matches!(*slot, LiveLaunchSlot::Empty) {
                    return Err(PrepareLaunchError::Unavailable(
                        "launch slot changed after barrier".into(),
                    ));
                }
                *slot = LiveLaunchSlot::Permit(permit);
                drop(slot);
                #[cfg(test)]
                test_audit(self, attempt, "barrier");
                Ok(binding)
            }
            RuntimeLaunchClaim::Existing(_) => Err(PrepareLaunchError::Unavailable(
                "launch already intended or stage consumed; status only".into(),
            )),
            RuntimeLaunchClaim::Refused { error, retry } => {
                if let Some(retry) = retry {
                    self.retain_retry(attempt, retry)
                        .map_err(PrepareLaunchError::Unavailable)?;
                }
                Err(PrepareLaunchError::Unavailable(error.to_string()))
            }
            RuntimeLaunchClaim::Failed(failed) => {
                #[cfg(test)]
                test_audit(self, attempt, "commit_error");
                Err(PrepareLaunchError::Failed(failed))
            }
            RuntimeLaunchClaim::PostCommit(error) => {
                Err(PrepareLaunchError::Unavailable(error.to_string()))
            }
        }
    }
}

/// The handle owns its return descriptor; there is no independently pairable receiver.
pub(crate) struct GatewayConnector {
    attempt: Arc<LiveAttempt>,
    task: tokio::task::JoinHandle<Result<UnixStream, String>>,
}
impl GatewayConnector {
    pub(crate) fn start(
        attempt: Arc<LiveAttempt>,
        config: crate::agent_runtime_config::ManagedConfig,
    ) -> Self {
        let task = tokio::task::spawn_local(async move { config.model_connection().await });
        Self { attempt, task }
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
    pub(crate) async fn join(self) -> Result<JoinedGateway, String> {
        let model = self.task.await.map_err(|_| "gateway connector stopped")??;
        Ok(JoinedGateway {
            attempt: self.attempt,
            model,
        })
    }
    pub(crate) async fn abort_and_join(self) {
        self.task.abort();
        drop(self.task.await); // Includes a descriptor returned just before abort.
    }
}
/// Only joining the matching connector can create this descriptor/attempt pair.
pub(crate) struct JoinedGateway {
    attempt: Arc<LiveAttempt>,
    model: UnixStream,
}
impl JoinedGateway {
    pub(crate) fn prepare(
        self,
        owner: &RuntimeOwner,
    ) -> Result<(Arc<LiveAttempt>, UnixStream, ManagedBinding), String> {
        #[cfg(test)]
        test_audit(owner, &self.attempt, "joined");
        match owner.prepare_launch(&self.attempt) {
            Ok(binding) => Ok((self.attempt, self.model, binding)),
            Err(PrepareLaunchError::Failed(failed)) => {
                let error = failed.error().to_string();
                self.close_and_park(owner, failed)?;
                Err(error)
            }
            Err(PrepareLaunchError::Unavailable(error)) => Err(error),
        }
    }
    fn close_and_park(
        self,
        owner: &RuntimeOwner,
        failed: FailedLaunchAppend,
    ) -> Result<(), String> {
        let Self { attempt, model } = self;
        // The exact producing task is already joined. Close its exact output before
        // constructing any continuation or entering the slot; no launcher ran.
        #[cfg(test)]
        let audit_fd = owner
            .root
            .join("agent-test-launch-audit")
            .is_file()
            .then(|| {
                use std::os::fd::AsRawFd;
                model.as_raw_fd()
            });
        drop(model);
        #[cfg(test)]
        if let Some(fd) = audit_fd {
            // fcntl accepts an invalid fd and reports EBADF. No owner/resource
            // witness is constructed from this test-only numeric observation.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            test_audit(owner, &attempt, "closed");
        }
        if !failed.belongs_to(&attempt) {
            return Err("failed append belongs to another live attempt".into());
        }
        owner.retain_retry(
            &attempt,
            QuiescentLaunchRetry {
                attempt: Arc::downgrade(&attempt),
                failed,
            },
        )?;
        #[cfg(test)]
        test_audit(owner, &attempt, "parked");
        Ok(())
    }
}

// These hooks exist only in this crate's test binary. They observe actual runtime
// phases and inject a one-shot original Store fault; they grant no custody.
#[cfg(test)]
pub(crate) fn test_audit(owner: &RuntimeOwner, attempt: &Arc<LiveAttempt>, phase: &str) {
    use std::io::Write;
    let path = owner.root.join("agent-test-launch-audit");
    if !path.is_file() {
        return;
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open actual launch audit");
    // Pointer text is test observation only. Production compares the live Arc.
    let line = format!(
        "{}|{:p}|{phase}\n",
        attempt.custody.id().invocation(),
        Arc::as_ptr(attempt)
    );
    file.write_all(line.as_bytes())
        .expect("write actual launch audit");
}
#[cfg(test)]
fn test_claim(
    owner: &RuntimeOwner,
    operation: impl FnOnce() -> RuntimeLaunchClaim,
) -> RuntimeLaunchClaim {
    use atomic_file::test_support::Checkpoint;
    for (name, checkpoint) in [
        ("before-publish", Checkpoint::BeforePublish),
        ("publish-submitted", Checkpoint::PublishSubmitted),
        ("after-publish", Checkpoint::AfterPublish),
        ("before-commit", Checkpoint::BeforeCommit),
    ] {
        let path = owner.root.join(format!("agent-test-launch-fail-{name}"));
        if path.is_file() {
            std::fs::remove_file(path).expect("consume exact one-shot launch fault");
            return crate::storage::test_support::with_save_failure(
                &owner.root,
                checkpoint,
                operation,
            );
        }
    }
    operation()
}

#[cfg(test)]
mod tests;
