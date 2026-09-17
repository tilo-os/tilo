//! Capability-bounded personal-mesh compute jobs.
//!
//! These types are carried only inside an authenticated certified-device
//! route. A job names an installed signed package and catalogue entrypoint;
//! it never contains a shell command, host path, environment variable, or
//! bearer credential. Execution belongs to a worker that reuses the normal
//! package launcher sandbox.

#![allow(
    missing_docs,
    reason = "the bounded wire fields and state variants are described by their stable names and module contract"
)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mesh::DeviceKey;
use crate::{DeviceRole, PowerAvailability, ServerAdvertisement};

pub const COMPUTE_JOB_SCHEMA_VERSION: u16 = 1;
pub const MAX_COMPUTE_JOB_LIFETIME_MS: u64 = 5 * 60 * 1_000;
pub const MAX_COMPUTE_ARGUMENTS: usize = 64;
pub const MAX_COMPUTE_ARGUMENT_BYTES: usize = 16 * 1024;
pub const MAX_COMPUTE_INPUTS: usize = 64;
pub const MAX_COMPUTE_CPU_TIME_MS: u64 = 60 * 60 * 1_000;
pub const MAX_COMPUTE_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MAX_COMPUTE_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeResourceLimits {
    pub cpu_time_ms: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub gpu: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeJobSpec {
    pub version: u16,
    pub job_id: [u8; 16],
    pub requester: DeviceKey,
    pub worker: DeviceKey,
    pub workspace_id: u64,
    pub app_id: String,
    pub package_digest: [u8; 32],
    pub entrypoint: String,
    pub arguments: Vec<String>,
    pub inputs: Vec<[u8; 32]>,
    pub limits: ComputeResourceLimits,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl ComputeJobSpec {
    pub fn validate_for(
        &self,
        expected_worker: DeviceKey,
        now_ms: u64,
    ) -> Result<(), ComputeError> {
        if self.version != COMPUTE_JOB_SCHEMA_VERSION {
            return Err(ComputeError::UnsupportedSchema(self.version));
        }
        if self.job_id == [0; 16]
            || self.requester == self.worker
            || self.worker != expected_worker
            || self.package_digest == [0; 32]
            || self.issued_at_ms > now_ms
            || now_ms >= self.expires_at_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms) > MAX_COMPUTE_JOB_LIFETIME_MS
        {
            return Err(ComputeError::InvalidBinding);
        }
        if !valid_identifier(&self.app_id, 256) || !valid_identifier(&self.entrypoint, 128) {
            return Err(ComputeError::InvalidEntrypoint);
        }
        let argument_bytes = self
            .arguments
            .iter()
            .try_fold(0usize, |total, argument| {
                if argument.len() > 4 * 1024 || argument.contains('\0') {
                    return None;
                }
                total.checked_add(argument.len())
            })
            .ok_or(ComputeError::ArgumentsTooLarge)?;
        if self.arguments.len() > MAX_COMPUTE_ARGUMENTS
            || argument_bytes > MAX_COMPUTE_ARGUMENT_BYTES
            || self.inputs.len() > MAX_COMPUTE_INPUTS
            || self.inputs.iter().any(|digest| *digest == [0; 32])
        {
            return Err(ComputeError::ArgumentsTooLarge);
        }
        if self.limits.cpu_time_ms == 0
            || self.limits.cpu_time_ms > MAX_COMPUTE_CPU_TIME_MS
            || self.limits.memory_bytes == 0
            || self.limits.memory_bytes > MAX_COMPUTE_MEMORY_BYTES
            || self.limits.output_bytes > MAX_COMPUTE_OUTPUT_BYTES
        {
            return Err(ComputeError::InvalidLimits);
        }
        Ok(())
    }
}

pub fn admit_compute_job(
    spec: &ComputeJobSpec,
    worker: &ServerAdvertisement,
    now_ms: u64,
) -> Result<(), ComputeError> {
    spec.validate_for(worker.device, now_ms)?;
    if !worker.online
        || !worker.role.can_serve()
        || !worker.services.compute
        || !worker.workspaces.contains(&spec.workspace_id)
        || !worker.apps.contains(&spec.app_id)
        || worker.assignment_capacity == 0
        || worker.active_assignments >= worker.assignment_capacity
        || worker.energy_saver
        || matches!(worker.power, PowerAvailability::Battery(percent) if percent <= 15)
    {
        return Err(ComputeError::WorkerUnavailable);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComputeJobPhase {
    Pending,
    Running,
    Completed { outputs: Vec<[u8; 32]> },
    Failed { code: String },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeJobLifecycle {
    pub job_id: [u8; 16],
    pub sequence: u64,
    pub phase: ComputeJobPhase,
}

impl ComputeJobLifecycle {
    pub fn new(job_id: [u8; 16]) -> Result<Self, ComputeError> {
        if job_id == [0; 16] {
            return Err(ComputeError::InvalidBinding);
        }
        Ok(Self {
            job_id,
            sequence: 0,
            phase: ComputeJobPhase::Pending,
        })
    }

    pub fn advance(&mut self, sequence: u64, next: ComputeJobPhase) -> Result<(), ComputeError> {
        if sequence != self.sequence.saturating_add(1) {
            return Err(ComputeError::OutOfOrder);
        }
        let allowed = matches!(
            (&self.phase, &next),
            (
                ComputeJobPhase::Pending,
                ComputeJobPhase::Running | ComputeJobPhase::Cancelled
            ) | (
                ComputeJobPhase::Running,
                ComputeJobPhase::Completed { .. }
                    | ComputeJobPhase::Failed { .. }
                    | ComputeJobPhase::Cancelled
            )
        );
        if !allowed {
            return Err(ComputeError::InvalidTransition);
        }
        match &next {
            ComputeJobPhase::Completed { outputs }
                if outputs.len() > MAX_COMPUTE_INPUTS
                    || outputs.iter().any(|digest| *digest == [0; 32]) =>
            {
                return Err(ComputeError::InvalidOutput);
            }
            ComputeJobPhase::Failed { code } if !valid_identifier(code, 128) => {
                return Err(ComputeError::InvalidOutput);
            }
            _ => {}
        }
        self.sequence = sequence;
        self.phase = next;
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ComputeError {
    #[error("unsupported compute-job schema {0}")]
    UnsupportedSchema(u16),
    #[error("compute job identity, target, or lifetime is invalid")]
    InvalidBinding,
    #[error("compute app or catalogue entrypoint is invalid")]
    InvalidEntrypoint,
    #[error("compute job arguments or inputs exceed their bound")]
    ArgumentsTooLarge,
    #[error("compute resource limits are invalid")]
    InvalidLimits,
    #[error("target worker is unavailable or ineligible")]
    WorkerUnavailable,
    #[error("compute lifecycle update is out of order")]
    OutOfOrder,
    #[error("compute lifecycle transition is invalid")]
    InvalidTransition,
    #[error("compute result metadata is invalid")]
    InvalidOutput,
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::ServiceCapabilities;

    fn spec() -> ComputeJobSpec {
        ComputeJobSpec {
            version: COMPUTE_JOB_SCHEMA_VERSION,
            job_id: [1; 16],
            requester: [2; 32],
            worker: [3; 32],
            workspace_id: 7,
            app_id: "dev.tilo.video-editor".into(),
            package_digest: [4; 32],
            entrypoint: "render-export-v1".into(),
            arguments: vec!["preset:h264".into()],
            inputs: vec![[5; 32]],
            limits: ComputeResourceLimits {
                cpu_time_ms: 60_000,
                memory_bytes: 1024 * 1024 * 1024,
                output_bytes: 64 * 1024 * 1024,
                gpu: true,
            },
            issued_at_ms: 100,
            expires_at_ms: 1_000,
        }
    }

    fn worker() -> ServerAdvertisement {
        ServerAdvertisement {
            device: [3; 32],
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities::personal_server(),
            workspaces: BTreeSet::from([7]),
            apps: BTreeSet::from(["dev.tilo.video-editor".into()]),
            priority: 0,
            latency_ms: 4,
            power: PowerAvailability::External,
            energy_saver: false,
            active_assignments: 0,
            assignment_capacity: 2,
            online: true,
        }
    }

    #[test]
    fn exact_eligible_worker_accepts_bounded_catalogued_job() {
        assert_eq!(admit_compute_job(&spec(), &worker(), 200), Ok(()));
    }

    #[test]
    fn wrong_app_target_expiry_and_low_power_fail_closed() {
        let mut candidate = worker();
        candidate.apps.clear();
        assert_eq!(
            admit_compute_job(&spec(), &candidate, 200),
            Err(ComputeError::WorkerUnavailable)
        );
        assert_eq!(
            spec().validate_for([9; 32], 200),
            Err(ComputeError::InvalidBinding)
        );
        assert_eq!(
            spec().validate_for([3; 32], 1_000),
            Err(ComputeError::InvalidBinding)
        );
        candidate = worker();
        candidate.power = PowerAvailability::Battery(10);
        assert_eq!(
            admit_compute_job(&spec(), &candidate, 200),
            Err(ComputeError::WorkerUnavailable)
        );
    }

    #[test]
    fn lifecycle_is_monotonic_and_terminal() {
        let mut lifecycle = ComputeJobLifecycle::new([1; 16]).unwrap();
        lifecycle.advance(1, ComputeJobPhase::Running).unwrap();
        assert_eq!(
            lifecycle.advance(1, ComputeJobPhase::Cancelled),
            Err(ComputeError::OutOfOrder)
        );
        lifecycle
            .advance(
                2,
                ComputeJobPhase::Completed {
                    outputs: vec![[7; 32]],
                },
            )
            .unwrap();
        assert_eq!(
            lifecycle.advance(3, ComputeJobPhase::Cancelled),
            Err(ComputeError::InvalidTransition)
        );
    }
}
