//! Platform-independent captured-job bounds, cancellation and outcomes.
use std::process::ExitStatus;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
};
use std::time::Duration;

/// A captured job requested absent or excessive resource bounds.
#[derive(Debug, thiserror::Error)]
#[error("captured worker resource plan is missing or exceeds launcher bounds")]
pub struct InvalidCapturePlan;

/// Strict resource envelope for a short-lived captured sandbox worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedResourcePlan {
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub output_bytes: usize,
    pub wall_time: Duration,
}

impl CapturedResourcePlan {
    pub const MAX_CPU_SECONDS: u64 = 300;
    pub const MAX_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
    pub const MAX_WALL_TIME: Duration = Duration::from_secs(10 * 60);

    /// Reject absent or excessively broad limits before a process exists.
    pub fn validate(self) -> Result<Self, InvalidCapturePlan> {
        if self.cpu_seconds == 0
            || self.cpu_seconds > Self::MAX_CPU_SECONDS
            || self.memory_bytes < 16 * 1024 * 1024
            || self.memory_bytes > Self::MAX_MEMORY_BYTES
            || self.output_bytes == 0
            || self.output_bytes > Self::MAX_OUTPUT_BYTES
            || self.wall_time.is_zero()
            || self.wall_time > Self::MAX_WALL_TIME
        {
            return Err(InvalidCapturePlan);
        }
        Ok(self)
    }
}

/// Cooperative cancellation shared with the trusted worker scheduler. The
/// sandbox receives no token or descriptor and cannot cancel another job.
#[derive(Clone, Debug, Default)]
pub struct CaptureCancellation(Arc<AtomicBool>);

impl CaptureCancellation {
    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Acquire)
    }
}

#[derive(Debug)]
pub enum CapturedTermination {
    Exited(ExitStatus),
    Cancelled,
    TimedOut,
    OutputLimit,
}

#[derive(Debug)]
pub struct CapturedRunOutput {
    pub termination: CapturedTermination,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
