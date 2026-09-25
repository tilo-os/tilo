//! Concrete native owner disclosure fence. Never constructible from caller data.
use super::*;
use qos_action_broker::peer_actions::{
    OwnerPolicyClock, PolicyClockError, PolicyOrigin, PolicyTime,
};

/// One clock for the actual owner lifetime; each recovery gets a distinct,
/// terminally revocable origin on that same clock. Never lock owner state here.
pub(super) struct OwnerTime {
    clock: OwnerPolicyClock,
    origin: Mutex<Arc<PolicyOrigin>>,
}
impl OwnerTime {
    pub(super) fn new(clock: OwnerPolicyClock) -> Self {
        Self {
            origin: Mutex::new(Arc::new(clock.new_origin())),
            clock,
        }
    }
    pub(super) fn clock(&self) -> OwnerPolicyClock {
        self.clock.clone()
    }
    pub(super) fn origin(&self) -> Result<Arc<PolicyOrigin>, PolicyClockError> {
        self.origin
            .lock()
            .map(|origin| origin.clone())
            .map_err(|_| PolicyClockError)
    }
    pub(super) fn invalidate(&self) {
        // Poison may deny new reads, but must not leave already queued custody live.
        self.origin
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .invalidate();
    }
    pub(super) fn deny(&self, terminal: &AtomicBool) {
        let origin = self
            .origin
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        origin.invalidate();
        // Replacement and terminal retirement share this lock. No recovery
        // can install a live origin between retirement and terminal denial.
        terminal.store(true, Ordering::Release);
    }
    pub(super) fn recover(
        &self,
        terminal: &AtomicBool,
    ) -> Result<Arc<PolicyOrigin>, PolicyClockError> {
        let mut origin = self.origin.lock().map_err(|_| PolicyClockError)?;
        if terminal.load(Ordering::Acquire) {
            return Err(PolicyClockError);
        }
        origin.invalidate();
        self.clock.now_ns()?;
        *origin = Arc::new(self.clock.new_origin());
        Ok(origin.clone())
    }
}
#[derive(Clone)]
pub(crate) struct NativeDisclosureFence {
    time: PolicyTime,
    continuity: u64,
    epoch: u64,
    next_expiry_ns: Option<u64>,
}
impl CommittedView {
    /// Only the actual current/owner-update guard exposes this borrowed view.
    pub(crate) fn policy_time(&self) -> Result<PolicyTime, String> {
        if self.policy_origin.load(Ordering::Acquire) {
            return Err("owner terminally denied".into());
        }
        self.time_origin
            .as_ref()
            .ok_or("owner policy time unavailable")?
            .capture()
            .map_err(|error| error.to_string())
    }
    pub(crate) fn check_policy_time(&self, time: &PolicyTime) -> Result<u64, String> {
        if self.policy_origin.load(Ordering::Acquire)
            || !self
                .time_origin
                .as_ref()
                .is_some_and(|origin| origin.owns(time))
        {
            return Err("owner policy time origin changed".into());
        }
        time.now_ns().map_err(|error| error.to_string())
    }
    pub(crate) fn private_disclosure_fence(&self) -> Result<NativeDisclosureFence, String> {
        let time = self.policy_time()?;
        let now = self.check_policy_time(&time)?;
        let basis = self
            .private_policy
            .as_ref()
            .ok_or("private policy unavailable")?;
        let next_expiry_ns = basis
            .next_expiry_after(now / 1_000_000_000)
            .map(|at| {
                at.checked_mul(1_000_000_000)
                    .ok_or("private policy time overflow")
            })
            .transpose()?;
        Ok(NativeDisclosureFence {
            time,
            continuity: self.continuity,
            epoch: self.policy_epoch,
            next_expiry_ns,
        })
    }
}
impl NativeDisclosureFence {
    pub(crate) fn policy_time(&self) -> &PolicyTime {
        &self.time
    }
    pub(crate) fn check(&self, current: &CommittedView) -> Result<u64, String> {
        let now = current.check_policy_time(&self.time)?;
        if current.private_policy.is_none()
            || self.continuity != current.continuity
            || self.epoch != current.policy_epoch
            || self.next_expiry_ns.is_some_and(|at| now >= at)
        {
            return Err("private result policy changed or expired".into());
        }
        Ok(now)
    }
}
