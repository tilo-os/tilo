//! Closed original-request authorization carried to the actual pre-write point.
use super::*;
use qos_action_broker::peer_actions::PolicyTime;

pub(crate) enum OwnerPolicyFence {
    Desktop(crate::desktop_commands::DesktopPolicyFence),
    LocalOwnerRequest(crate::owner_commit::LocalOwnerPolicyFence),
    HostSigning(crate::host_signing::HostSigningPolicyFence),
    Checkpoint(super::checkpoint::CheckpointPolicyFence),
}
/// A definite refusal before SessionStore was called, never an I/O classification.
#[derive(Debug)]
pub(crate) struct OwnerPolicyRefused(pub(crate) String);
impl std::fmt::Display for OwnerPolicyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for OwnerPolicyRefused {}
impl OwnerPolicyFence {
    fn policy_time(&self) -> &PolicyTime {
        match self {
            Self::Desktop(fence) => fence.policy_time(),
            Self::LocalOwnerRequest(fence) => fence.policy_time(),
            Self::HostSigning(fence) => fence.policy_time(),
            Self::Checkpoint(fence) => fence.policy_time(),
        }
    }
    pub(super) fn sample(&self) -> Result<SystemTime> {
        sample(Some(self.policy_time())).map_err(|error| OwnerPolicyRefused(error).into())
    }
    pub(super) fn revalidate(&self, owner: &SessionOwner) -> Result<()> {
        match self {
            Self::Desktop(fence) => fence.revalidate(owner),
            Self::LocalOwnerRequest(fence) => fence.revalidate(owner),
            Self::HostSigning(fence) => fence.revalidate(owner),
            Self::Checkpoint(fence) => fence.revalidate(owner),
        }
        .map_err(|error| OwnerPolicyRefused(error).into())
    }
}
pub(crate) fn sample(time: Option<&PolicyTime>) -> std::result::Result<SystemTime, String> {
    match time {
        Some(time) => SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_nanos(
                time.now_ns().map_err(|error| error.to_string())?,
            ))
            .ok_or_else(|| "owner policy time overflow".into()),
        // Only explicit standalone/no registered-context callers use this branch.
        None => Ok(SystemTime::now()),
    }
}
#[cfg(test)]
std::thread_local! {
    static BEFORE_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn before_check() {
    if let Some(hook) = BEFORE_CHECK.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
}
#[cfg(test)]
pub(crate) fn with_before_check<T>(
    hook: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Box<dyn FnOnce()>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            BEFORE_CHECK.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(BEFORE_CHECK.with(|slot| slot.replace(Some(Box::new(hook)))));
    let result = operation();
    drop(restore);
    result
}
