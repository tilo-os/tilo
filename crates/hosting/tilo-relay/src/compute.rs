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

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use launcher::capture::{
    CaptureCancellation, CapturedResourcePlan, CapturedRunOutput, CapturedTermination,
};
use launcher::manifest::{ManifestView, read_manifest};
#[cfg(target_os = "linux")]
use launcher::run::{IdentitySource, LaunchMode, RunOptions, run_with_captured_bounded};
use package_store::InstalledPackage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ServerAdvertisement;
use crate::mesh::DeviceKey;
use crate::placement::ComputeRequirements;

pub const COMPUTE_JOB_SCHEMA_VERSION: u16 = 1;
pub const MAX_COMPUTE_JOB_LIFETIME_MS: u64 = 5 * 60 * 1_000;
pub const MAX_COMPUTE_INPUTS: usize = 64;
pub const MAX_COMPUTE_CPU_TIME_MS: u64 = 5 * 60 * 1_000;
pub const MAX_COMPUTE_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_COMPUTE_INPUT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_COMPUTE_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_COMPUTE_OUTPUTS: usize = 64;

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
    /// Validate the exact target, lifetime, catalogue identifiers, arguments,
    /// content references, and resource ceilings.
    ///
    /// # Errors
    ///
    /// Returns the first violated wire or policy invariant.
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
        // Mesh jobs are content transforms, not a remote command runner.
        // Keeping the legacy wire field lets old peers fail explicitly, but
        // no requester-provided argv (and therefore no path/env/bearer-shaped
        // value) ever reaches the package process.
        if !self.arguments.is_empty() {
            return Err(ComputeError::RequesterArgumentsForbidden);
        }
        if self.inputs.len() > MAX_COMPUTE_INPUTS || self.inputs.contains(&[0; 32]) {
            return Err(ComputeError::ArgumentsTooLarge);
        }
        if self.limits.cpu_time_ms == 0
            || self.limits.cpu_time_ms > MAX_COMPUTE_CPU_TIME_MS
            || self.limits.memory_bytes < 16 * 1024 * 1024
            || self.limits.memory_bytes > MAX_COMPUTE_MEMORY_BYTES
            || self.limits.output_bytes == 0
            || self.limits.output_bytes > MAX_COMPUTE_OUTPUT_BYTES
        {
            return Err(ComputeError::InvalidLimits);
        }
        Ok(())
    }
}

/// Admit a validated job only to an eligible, available worker advertising
/// the exact workspace, app, accelerator, and compute service.
///
/// # Errors
///
/// Returns [`ComputeError::WorkerUnavailable`] for any ineligible worker, or
/// the underlying spec validation error.
pub fn admit_compute_job(
    spec: &ComputeJobSpec,
    worker: &ServerAdvertisement,
    now_ms: u64,
) -> Result<(), ComputeError> {
    spec.validate_for(worker.device, now_ms)?;
    if worker.compute_ineligibility(&spec.requirements()).is_some() {
        return Err(ComputeError::WorkerUnavailable);
    }
    Ok(())
}

impl ComputeJobSpec {
    /// What this job needs from its worker: the terms both placement and
    /// admission decide eligibility by.
    #[must_use]
    pub fn requirements(&self) -> ComputeRequirements<'_> {
        ComputeRequirements {
            requester: self.requester,
            workspace_id: self.workspace_id,
            app_id: &self.app_id,
            gpu: self.limits.gpu,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeJobPhase {
    Pending,
    Running,
    Completed { outputs: Vec<[u8; 32]> },
    Failed { code: String },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeJobLifecycle {
    pub job_id: [u8; 16],
    pub sequence: u64,
    pub phase: ComputeJobPhase,
}

impl ComputeJobLifecycle {
    /// Start a pending lifecycle for one non-zero job identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ComputeError::InvalidBinding`] for the zero identifier.
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

    /// Apply one exact-next lifecycle update and validate terminal metadata.
    pub fn advance(&mut self, sequence: u64, next: ComputeJobPhase) -> Result<(), ComputeError> {
        if sequence != self.sequence.saturating_add(1) {
            return Err(ComputeError::OutOfOrder);
        }
        let allowed = matches!(
            (&self.phase, &next),
            // A job can fail before it starts: its worker is lost or
            // revoked before it reports running.
            (
                ComputeJobPhase::Pending,
                ComputeJobPhase::Running
                    | ComputeJobPhase::Failed { .. }
                    | ComputeJobPhase::Cancelled
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
                if outputs.len() > MAX_COMPUTE_INPUTS || outputs.contains(&[0; 32]) =>
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
    #[error("compute jobs accept content references, not requester-provided argv")]
    RequesterArgumentsForbidden,
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

/// Trusted local catalogue lookup. Implementations return only package rows
/// produced by the signed-package installer; requesters never supply paths.
pub trait ComputePackageCatalogue: Send + Sync {
    /// Resolve an installed package by its signed manifest identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the trusted local catalogue cannot be read.
    fn resolve(&self, app_id: &str) -> Result<Option<InstalledPackage>, String>;
}

/// Production catalogue backed by the device's package store.
#[derive(Clone, Copy, Debug, Default)]
pub struct InstalledComputeCatalogue;

impl ComputePackageCatalogue for InstalledComputeCatalogue {
    fn resolve(&self, app_id: &str) -> Result<Option<InstalledPackage>, String> {
        package_store::find_installed(app_id).map_err(|error| error.to_string())
    }
}

/// Content-addressed storage used by a worker. The worker recomputes every
/// digest on both ingress and egress, so a faulty store cannot substitute
/// bytes under a caller-chosen name.
pub trait ComputeContentStore: Send + Sync {
    /// Read bytes addressed by their digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the local content store cannot be read.
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String>;
    /// Store bytes under their independently verified digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the local content store cannot persist the bytes.
    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String>;
    /// Where this store keeps the bytes of `digest`, as a 32-byte locator
    /// another device's [`HeldContent`] may understand (workspace content
    /// names the file version's manifest hash), or `None`. A hint offered
    /// with a possession proof, never authority.
    fn locator(&self, _digest: [u8; 32]) -> Option<[u8; 32]> {
        None
    }
}

/// Content a worker device already holds, which a job may run on in place
/// of an input fetched from its requester — but only once the requester
/// proved it holds the same bytes
/// ([`prove_job_input`](crate::compute_content::prove_job_input)). Holding
/// bytes is never authority to bind them to a job; the proof is.
pub trait HeldContent: Send + Sync {
    /// The bytes this device holds as `digest`, looked up by the digest or,
    /// when given, by the requester's `locator`
    /// ([`ComputeContentStore::locator`]); `Ok(None)` when it holds none or
    /// they would exceed `max_bytes`. Called off the async runtime. The
    /// caller checks the bytes against `digest` and the proof.
    ///
    /// # Errors
    ///
    /// Returns an error when local content cannot be read.
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String>;

    /// Whether [`Self::held`] can find content by a locator it would not
    /// find by digest alone. When `false`, a worker asks for a proof only
    /// for digests it already found, so a miss costs nothing.
    fn locates(&self) -> bool {
        false
    }
}

/// The outputs a worker retains for the requesters of the jobs it ran.
///
/// A completed job's outputs stay retrievable by digest, but only by that
/// job's requester and only the digests that job reported: knowing a digest
/// is never authority to read it.
pub trait ComputeOutputSource: Send + Sync {
    /// The bytes of output `digest` of job `job_id` in `workspace_id`, when
    /// `asker` submitted that job and it reported the digest; `Ok(None)`
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained output cannot be read or no
    /// longer matches its digest.
    fn retained_output(
        &self,
        asker: DeviceKey,
        workspace_id: u64,
        job_id: [u8; 16],
        digest: [u8; 32],
    ) -> Result<Option<Vec<u8>>, String>;
}

/// Information passed only to the trusted, local sandbox adapter.
pub struct ComputeSandboxInvocation<'a> {
    pub package: &'a InstalledPackage,
    pub manifest: &'a ManifestView,
    pub entrypoint: &'a str,
    pub workspace_slug: &'a str,
    pub host_job_dir: &'a Path,
    pub require_gpu: bool,
    pub plan: CapturedResourcePlan,
    pub cancellation: CaptureCancellation,
}

/// Narrow launch seam: production always uses the package launcher; tests
/// can model termination without creating a nested user namespace.
pub trait ComputeSandbox: Send + Sync {
    fn gpu_available(&self) -> bool;
    /// Launch one worker invocation through the trusted package sandbox.
    ///
    /// # Errors
    ///
    /// Returns an error when the sandbox cannot launch or supervise the worker.
    fn launch(&self, invocation: ComputeSandboxInvocation<'_>)
    -> Result<CapturedRunOutput, String>;
}

/// Production adapter. The render-node path is selected by trusted local
/// compositor/device policy; it is never present in a mesh request.
#[derive(Clone, Debug, Default)]
pub struct LauncherComputeSandbox {
    render_node: Option<PathBuf>,
}

impl LauncherComputeSandbox {
    #[must_use]
    pub fn new(render_node: Option<PathBuf>) -> Self {
        Self { render_node }
    }
}

impl ComputeSandbox for LauncherComputeSandbox {
    fn gpu_available(&self) -> bool {
        cfg!(target_os = "linux") && self.render_node.is_some()
    }

    fn launch(
        &self,
        invocation: ComputeSandboxInvocation<'_>,
    ) -> Result<CapturedRunOutput, String> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = invocation;
            Err("local sandboxed compute execution requires a Linux worker".into())
        }
        #[cfg(target_os = "linux")]
        {
            if invocation.require_gpu && self.render_node.is_none() {
                return Err("advertised GPU has no launcher-approved render node".into());
            }
            let mounted_dir = launcher::appdir::host_data_home()
                .map(|data_home| {
                    launcher::appdir::host_app_dir(
                        &data_home,
                        &launcher::appdir::room_slug(Some(invocation.workspace_slug)),
                        &invocation.manifest.package_id,
                    )
                })
                .ok_or_else(|| "launcher has no trusted data home".to_string())?;
            if mounted_dir != invocation.host_job_dir {
                return Err("staged compute directory is not the launcher's mounted appdir".into());
            }
            let opts = RunOptions {
                no_services: true,
                mode: LaunchMode::Cli {
                    command: invocation.entrypoint.to_string(),
                    passthrough_argv: vec![
                        "--tilo-compute-request".into(),
                        "/run/tilo/app/.tilo-compute/request.json".into(),
                        "--tilo-compute-output".into(),
                        "/run/tilo/app/.tilo-compute/outputs".into(),
                    ],
                },
                workspace_id: Some(invocation.workspace_slug.to_string()),
                app_id: Some(invocation.manifest.package_id.clone()),
                // A CPU-only request receives no DRM node even on a GPU worker.
                render_node: invocation
                    .require_gpu
                    .then(|| self.render_node.clone())
                    .flatten(),
                wayland_socket: None,
                capabilities_path: None,
                identity: IdentitySource::UserStore,
                trusted_identities_host_path: None,
                open_document: None,
                open_uri: None,
                ..RunOptions::default()
            };
            run_with_captured_bounded(
                invocation.manifest,
                &invocation.package.payload_root,
                &opts,
                invocation.plan,
                invocation.cancellation,
            )
            .map_err(|error| error.to_string())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeJobResult {
    pub lifecycle: ComputeJobLifecycle,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ComputeExecutionError {
    #[error("authenticated route does not match the job requester or workspace")]
    RouteMismatch,
    #[error(transparent)]
    Admission(#[from] ComputeError),
    #[error("signed package is not installed for the requested app")]
    PackageUnavailable,
    #[error("installed package digest does not match the requested signed root")]
    PackageDigestMismatch,
    #[error("requested entrypoint is not declared by the signed package catalogue")]
    EntrypointUnavailable,
    #[error("worker advertised a GPU but has no launcher-approved device")]
    GpuUnavailable,
    #[error("compute job id has already been claimed")]
    Replay,
    #[error("content input is unavailable")]
    MissingInput,
    #[error("content store returned bytes under the wrong digest")]
    InputDigestMismatch,
    #[error("content inputs exceed the worker staging bound")]
    InputsTooLarge,
    #[error("compute staging failed: {0}")]
    Staging(String),
    #[error("sandbox launch failed: {0}")]
    Launch(String),
    #[error("compute output is malformed or exceeds its bound")]
    InvalidOutput,
    #[error("content output persistence failed: {0}")]
    OutputStore(String),
}

impl ComputeExecutionError {
    /// The stable failure code a requester sees for this error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::RouteMismatch | Self::Admission(_) => "admission",
            Self::PackageUnavailable | Self::PackageDigestMismatch => "package",
            Self::EntrypointUnavailable => "entrypoint",
            Self::GpuUnavailable => "gpu_unavailable",
            Self::Replay => "replay",
            Self::MissingInput => "input_unavailable",
            Self::InputDigestMismatch => "input_integrity",
            Self::InputsTooLarge => "input_too_large",
            Self::Staging(_) => "staging",
            Self::Launch(_) => "launch",
            Self::InvalidOutput => "output",
            Self::OutputStore(_) => "output_store",
        }
    }
}

/// A local executor bound to one certified worker advertisement. Jobs are
/// synchronously supervised, while cancellation is safe to signal from any
/// thread through [`CaptureCancellation`].
pub struct ComputeWorker<C, P, S> {
    worker: ServerAdvertisement,
    data_home: PathBuf,
    state_root: PathBuf,
    content: C,
    catalogue: P,
    sandbox: S,
    jobs: Mutex<BTreeMap<[u8; 16], ComputeJobLifecycle>>,
}

impl<C, P, S> ComputeWorker<C, P, S>
where
    C: ComputeContentStore,
    P: ComputePackageCatalogue,
    S: ComputeSandbox,
{
    #[must_use]
    pub fn new(
        worker: ServerAdvertisement,
        data_home: PathBuf,
        state_root: PathBuf,
        content: C,
        catalogue: P,
        sandbox: S,
    ) -> Self {
        Self {
            worker,
            data_home,
            state_root,
            content,
            catalogue,
            sandbox,
            jobs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Resolve, stage, execute and persist one authenticated mesh job.
    ///
    /// The two authenticated values are derived from the certified transport
    /// route rather than from the serialized job. A mismatch is rejected
    /// before package lookup, staging, or durable job-id claim.
    pub fn execute(
        &self,
        authenticated_requester: DeviceKey,
        authenticated_workspace: u64,
        spec: &ComputeJobSpec,
        now_ms: u64,
        cancellation: CaptureCancellation,
    ) -> Result<ComputeJobResult, ComputeExecutionError> {
        self.execute_with_inputs(
            &self.content,
            (authenticated_requester, authenticated_workspace),
            spec,
            now_ms,
            cancellation,
        )
    }

    /// [`Self::execute`], staging the job's inputs from `inputs` (the bytes
    /// hydrated for this one job) rather than from the worker's own store.
    /// Outputs are still persisted to the worker's store, from which
    /// [`ComputeOutputSource`] serves them to the job's requester.
    ///
    /// `route` is the authenticated `(requester, workspace)` of the
    /// transport route, as for [`Self::execute`].
    ///
    /// # Errors
    ///
    /// As [`Self::execute`].
    #[allow(
        clippy::too_many_lines,
        reason = "keeps the security-critical execution sequence auditable"
    )]
    pub fn execute_with_inputs(
        &self,
        inputs: &dyn ComputeContentStore,
        route: (DeviceKey, u64),
        spec: &ComputeJobSpec,
        now_ms: u64,
        cancellation: CaptureCancellation,
    ) -> Result<ComputeJobResult, ComputeExecutionError> {
        let (authenticated_requester, authenticated_workspace) = route;
        if authenticated_requester != spec.requester || authenticated_workspace != spec.workspace_id
        {
            return Err(ComputeExecutionError::RouteMismatch);
        }
        admit_compute_job(spec, &self.worker, now_ms)?;
        if spec.limits.gpu && !self.sandbox.gpu_available() {
            return Err(ComputeExecutionError::GpuUnavailable);
        }

        let installed = self
            .catalogue
            .resolve(&spec.app_id)
            .map_err(ComputeExecutionError::Staging)?
            .ok_or(ComputeExecutionError::PackageUnavailable)?;
        verify_registry_binding(&installed, spec)?;
        verify_installed_payload(&installed, &installed.payload_root)?;
        let manifest = read_manifest(&installed.manifest_path)
            .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        if manifest.package_id != spec.app_id
            || !manifest
                .cli_commands
                .iter()
                .any(|name| name == &spec.entrypoint)
            || manifest.entrypoint.cli_bin().is_none()
        {
            return Err(ComputeExecutionError::EntrypointUnavailable);
        }

        let workspace_slug = format!("compute-job-{}", hex::encode(spec.job_id));
        self.claim_job(spec, &workspace_slug)?;
        self.advance(spec.job_id, 1, ComputeJobPhase::Running)?;

        // Launch an exact verified snapshot rather than the mutable installed
        // tree. Re-verifying the copy closes the check/re-open race: a source
        // mutation during copying makes the snapshot fail its signed hashes.
        let execution_root = self.state_root.join("jobs").join(hex::encode(spec.job_id));
        let snapshot_root = execution_root.join("payload");
        copy_payload_snapshot(&installed.payload_root, &snapshot_root)
            .map_err(|error| self.fail(spec.job_id, "integrity", error.to_string()))?;
        let record = verify_installed_payload(&installed, &snapshot_root)
            .map_err(|error| self.fail_existing(spec.job_id, "integrity", error))?;
        // The manifest the launcher reads comes from the verified record, not
        // from a second read of the installed file.
        let snapshot_manifest_path = execution_root.join("manifest.toml");
        fs::write(&snapshot_manifest_path, &record.manifest_bytes)
            .map_err(|error| self.fail(spec.job_id, "integrity", error.to_string()))?;
        let manifest = read_manifest(&snapshot_manifest_path)
            .map_err(|error| self.fail(spec.job_id, "integrity", error.to_string()))?;
        if manifest.package_id != spec.app_id
            || !manifest
                .cli_commands
                .iter()
                .any(|name| name == &spec.entrypoint)
            || manifest.entrypoint.cli_bin().is_none()
        {
            return Err(self.fail_existing(
                spec.job_id,
                "entrypoint",
                ComputeExecutionError::EntrypointUnavailable,
            ));
        }
        let package = InstalledPackage {
            payload_root: snapshot_root,
            manifest_path: snapshot_manifest_path,
            ..installed
        };

        let job_dir = launcher::appdir::prepare_host_app_dir(
            &self.data_home,
            Some(&workspace_slug),
            &spec.app_id,
        )
        .map_err(|error| self.fail(spec.job_id, "staging", error.to_string()))?;
        let _cleanup = JobDirectoryCleanup(vec![job_dir.clone(), execution_root]);
        let io_root = job_dir.join(".tilo-compute");
        let input_dir = io_root.join("inputs");
        let output_dir = io_root.join("outputs");
        prepare_private_dir(&input_dir)
            .and_then(|()| prepare_private_dir(&output_dir))
            .map_err(|error| self.fail(spec.job_id, "staging", error.to_string()))?;
        let output_dir_handle = File::open(&output_dir)
            .map_err(|error| self.fail(spec.job_id, "staging", error.to_string()))?;
        Self::stage_inputs(inputs, spec, &input_dir)
            .map_err(|error| self.fail_existing(spec.job_id, "input", error))?;
        write_request(spec, &io_root)
            .map_err(|error| self.fail(spec.job_id, "staging", error.to_string()))?;

        if cancellation.is_cancelled() {
            self.advance(spec.job_id, 2, ComputeJobPhase::Cancelled)?;
            return Ok(ComputeJobResult {
                lifecycle: self.lifecycle(spec.job_id)?,
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        let plan = resource_plan(spec, now_ms)?;
        let captured = self
            .sandbox
            .launch(ComputeSandboxInvocation {
                package: &package,
                manifest: &manifest,
                entrypoint: &spec.entrypoint,
                workspace_slug: &workspace_slug,
                host_job_dir: &job_dir,
                require_gpu: spec.limits.gpu,
                plan,
                cancellation,
            })
            .map_err(|error| {
                let _ = self.advance(
                    spec.job_id,
                    2,
                    ComputeJobPhase::Failed {
                        code: "launch".into(),
                    },
                );
                ComputeExecutionError::Launch(error)
            })?;
        self.finish(spec, &output_dir_handle, captured)
    }

    /// Read the current monotonic lifecycle for a claimed job.
    pub fn lifecycle(
        &self,
        job_id: [u8; 16],
    ) -> Result<ComputeJobLifecycle, ComputeExecutionError> {
        self.jobs
            .lock()
            .map_err(|_| ComputeExecutionError::Staging("job registry poisoned".into()))?
            .get(&job_id)
            .cloned()
            .ok_or(ComputeExecutionError::Replay)
    }

    fn claim_job(
        &self,
        spec: &ComputeJobSpec,
        workspace_slug: &str,
    ) -> Result<(), ComputeExecutionError> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| ComputeExecutionError::Staging("job registry poisoned".into()))?;
        if jobs.contains_key(&spec.job_id) {
            return Err(ComputeExecutionError::Replay);
        }
        prepare_private_dir(&self.state_root)
            .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        let claim_path = self.claim_path(spec.job_id);
        let mut claim = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(claim_path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ComputeExecutionError::Replay
                } else {
                    ComputeExecutionError::Staging(error.to_string())
                }
            })?;
        writeln!(
            claim,
            "{}:{}:{}:{}",
            hex::encode(spec.requester),
            hex::encode(spec.worker),
            spec.workspace_id,
            workspace_slug
        )
        .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        claim
            .sync_all()
            .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        jobs.insert(spec.job_id, ComputeJobLifecycle::new(spec.job_id)?);
        drop(jobs);
        Ok(())
    }

    fn advance(
        &self,
        job_id: [u8; 16],
        sequence: u64,
        phase: ComputeJobPhase,
    ) -> Result<(), ComputeExecutionError> {
        self.jobs
            .lock()
            .map_err(|_| ComputeExecutionError::Staging("job registry poisoned".into()))?
            .get_mut(&job_id)
            .ok_or(ComputeExecutionError::Replay)?
            .advance(sequence, phase)?;
        Ok(())
    }

    fn stage_inputs(
        inputs: &dyn ComputeContentStore,
        spec: &ComputeJobSpec,
        input_dir: &Path,
    ) -> Result<(), ComputeExecutionError> {
        let mut total = 0_u64;
        let mut staged = std::collections::BTreeSet::new();
        for digest in &spec.inputs {
            // A digest named twice is one input file.
            if !staged.insert(*digest) {
                continue;
            }
            let bytes = inputs
                .get(*digest)
                .map_err(ComputeExecutionError::Staging)?
                .ok_or(ComputeExecutionError::MissingInput)?;
            if blake3::hash(&bytes).as_bytes() != digest {
                return Err(ComputeExecutionError::InputDigestMismatch);
            }
            total = total
                .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .ok_or(ComputeExecutionError::InputsTooLarge)?;
            if total > MAX_COMPUTE_INPUT_BYTES || total > spec.limits.memory_bytes {
                return Err(ComputeExecutionError::InputsTooLarge);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o400)
                .open(input_dir.join(hex::encode(digest)))
                .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        }
        Ok(())
    }

    fn finish(
        &self,
        spec: &ComputeJobSpec,
        output_dir: &File,
        captured: CapturedRunOutput,
    ) -> Result<ComputeJobResult, ComputeExecutionError> {
        let captured_bytes = captured.stdout.len().saturating_add(captured.stderr.len());
        if captured_bytes > usize::try_from(spec.limits.output_bytes).unwrap_or(usize::MAX) {
            return Err(self.fail_existing(
                spec.job_id,
                "output",
                ComputeExecutionError::InvalidOutput,
            ));
        }
        let terminal = match captured.termination {
            CapturedTermination::Cancelled => ComputeJobPhase::Cancelled,
            CapturedTermination::TimedOut => ComputeJobPhase::Failed {
                code: "timeout".into(),
            },
            CapturedTermination::OutputLimit => ComputeJobPhase::Failed {
                code: "output_limit".into(),
            },
            CapturedTermination::Exited(status) if !status.success() => ComputeJobPhase::Failed {
                code: status
                    .code()
                    .map_or_else(|| "exit_signal".into(), |code| format!("exit_{code}")),
            },
            CapturedTermination::Exited(_) => {
                let remaining = usize::try_from(spec.limits.output_bytes)
                    .unwrap_or(usize::MAX)
                    .saturating_sub(captured_bytes);
                let outputs = match self.collect_outputs(output_dir, remaining) {
                    Ok(outputs) => outputs,
                    Err(error) => {
                        return Err(self.fail_existing(spec.job_id, "output", error));
                    }
                };
                // Record which digests this job's requester may retrieve
                // before reporting any of them: a digest the requester
                // cannot fetch is never reported.
                if let Err(error) = self.retain_outputs(spec.job_id, &outputs) {
                    return Err(self.fail_existing(spec.job_id, "output_store", error));
                }
                ComputeJobPhase::Completed { outputs }
            }
        };
        self.advance(spec.job_id, 2, terminal)?;
        Ok(ComputeJobResult {
            lifecycle: self.lifecycle(spec.job_id)?,
            stdout: captured.stdout,
            stderr: captured.stderr,
        })
    }

    fn collect_outputs(
        &self,
        output_dir: &File,
        mut remaining: usize,
    ) -> Result<Vec<[u8; 32]>, ComputeExecutionError> {
        let names = output_names(output_dir)
            .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
        if names.len() > MAX_COMPUTE_OUTPUTS {
            return Err(ComputeExecutionError::InvalidOutput);
        }
        let mut outputs = Vec::with_capacity(names.len());
        for name in names {
            let name =
                CString::new(name.as_bytes()).map_err(|_| ComputeExecutionError::InvalidOutput)?;
            // SAFETY: name is NUL-terminated and output_dir owns the pinned
            // output directory descriptor established before sandbox launch.
            let fd = unsafe {
                libc::openat(
                    output_dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return Err(ComputeExecutionError::InvalidOutput);
            }
            // SAFETY: openat returned a fresh owned descriptor.
            let mut file = unsafe { File::from_raw_fd(fd) };
            if !file
                .metadata()
                .map_err(|_| ComputeExecutionError::InvalidOutput)?
                .is_file()
            {
                return Err(ComputeExecutionError::InvalidOutput);
            }
            let mut bytes = Vec::new();
            std::io::Read::by_ref(&mut file)
                .take(
                    u64::try_from(remaining)
                        .unwrap_or(u64::MAX)
                        .saturating_add(1),
                )
                .read_to_end(&mut bytes)
                .map_err(|_| ComputeExecutionError::InvalidOutput)?;
            if bytes.len() > remaining {
                return Err(ComputeExecutionError::InvalidOutput);
            }
            remaining = remaining.saturating_sub(bytes.len());
            let digest = *blake3::hash(&bytes).as_bytes();
            self.content
                .put(digest, &bytes)
                .map_err(ComputeExecutionError::OutputStore)?;
            outputs.push(digest);
        }
        Ok(outputs)
    }

    fn claim_path(&self, job_id: [u8; 16]) -> PathBuf {
        self.state_root
            .join(format!("{}.claim", hex::encode(job_id)))
    }

    fn outputs_path(&self, job_id: [u8; 16]) -> PathBuf {
        self.state_root
            .join(format!("{}{OUTPUT_INDEX_SUFFIX}", hex::encode(job_id)))
    }

    fn retain_outputs(
        &self,
        job_id: [u8; 16],
        outputs: &[[u8; 32]],
    ) -> Result<(), ComputeExecutionError> {
        let mut index = String::with_capacity(outputs.len().saturating_mul(65));
        for digest in outputs {
            index.push_str(&hex::encode(digest));
            index.push('\n');
        }
        tilo_files::native::atomic::write(&self.outputs_path(job_id), index.as_bytes())
            .map_err(|error| ComputeExecutionError::OutputStore(error.to_string()))
    }

    fn fail(&self, job_id: [u8; 16], code: &str, detail: String) -> ComputeExecutionError {
        let _ = self.advance(
            job_id,
            2,
            ComputeJobPhase::Failed {
                code: code.to_string(),
            },
        );
        ComputeExecutionError::Staging(detail)
    }

    fn fail_existing(
        &self,
        job_id: [u8; 16],
        code: &str,
        error: ComputeExecutionError,
    ) -> ComputeExecutionError {
        let _ = self.advance(
            job_id,
            2,
            ComputeJobPhase::Failed {
                code: code.to_string(),
            },
        );
        error
    }
}

impl<C, P, S> ComputeOutputSource for ComputeWorker<C, P, S>
where
    C: ComputeContentStore,
    P: ComputePackageCatalogue,
    S: ComputeSandbox,
{
    fn retained_output(
        &self,
        asker: DeviceKey,
        workspace_id: u64,
        job_id: [u8; 16],
        digest: [u8; 32],
    ) -> Result<Option<Vec<u8>>, String> {
        // The claim names the job's authenticated requester and workspace;
        // the index names exactly the outputs that job reported.
        let Some(claim) = read_small(&self.claim_path(job_id))? else {
            return Ok(None);
        };
        let mut fields = claim.trim_end().split(':');
        let requester = fields.next().unwrap_or_default();
        let _worker = fields.next();
        let workspace = fields.next().unwrap_or_default();
        if requester != hex::encode(asker) || workspace != workspace_id.to_string() {
            return Ok(None);
        }
        let Some(index) = read_small(&self.outputs_path(job_id))? else {
            return Ok(None);
        };
        let wanted = hex::encode(digest);
        if !index.lines().any(|line| line == wanted) {
            return Ok(None);
        }
        let Some(bytes) = self.content.get(digest)? else {
            return Ok(None);
        };
        if blake3::hash(&bytes).as_bytes() != &digest {
            return Err("retained output does not match its digest".into());
        }
        Ok(Some(bytes))
    }
}

/// Suffix of the file beside a job's claim that lists the outputs it
/// reported, which its requester may retrieve.
pub const OUTPUT_INDEX_SUFFIX: &str = ".outputs";

/// A small private metadata file, or `None` when it does not exist.
fn read_small(path: &Path) -> Result<Option<String>, String> {
    const MAX_INDEX_BYTES: u64 = 64 * 1024;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut text = String::new();
    file.take(MAX_INDEX_BYTES)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    Ok(Some(text))
}

fn verify_registry_binding(
    package: &InstalledPackage,
    spec: &ComputeJobSpec,
) -> Result<(), ComputeExecutionError> {
    if package.id != spec.app_id {
        return Err(ComputeExecutionError::PackageUnavailable);
    }
    let digest = hex::decode(&package.package_id_hex)
        .map_err(|_| ComputeExecutionError::PackageDigestMismatch)?;
    if digest.as_slice() != spec.package_digest {
        return Err(ComputeExecutionError::PackageDigestMismatch);
    }
    Ok(())
}

fn verify_installed_payload(
    package: &InstalledPackage,
    payload_root: &Path,
) -> Result<package_store::integrity::Record, ComputeExecutionError> {
    use package_store::integrity;
    let report = integrity::verify(package, payload_root)
        .map_err(|error| ComputeExecutionError::Staging(error.to_string()))?;
    if !report.is_intact() || !report.unexpected.is_empty() {
        return Err(ComputeExecutionError::PackageDigestMismatch);
    }
    integrity::read_record(
        &integrity::record_dir(package),
        &package.id,
        &package.package_id_hex,
    )
    .map_err(|error| ComputeExecutionError::Staging(error.to_string()))
}

/// Owns a `DIR` stream from `fdopendir` and closes it exactly once.
struct DirectoryStream(*mut libc::DIR);
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this stream exclusively owns the descriptor from fdopendir.
        unsafe {
            libc::closedir(self.0);
        }
    }
}
fn output_names(output_dir: &File) -> io::Result<Vec<OsString>> {
    // fdopendir assumes ownership of the descriptor it receives, so duplicate
    // the held capability before transferring it. Enumeration and subsequent
    // opens remain relative to output_dir even if its path is replaced.
    let fd = unsafe { libc::fcntl(output_dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a newly duplicated live directory descriptor whose
    // ownership transfers to DIR on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir did not take ownership after reporting failure.
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    // The descriptor was opened before the sandbox wrote its outputs. POSIX
    // leaves entries added since opendir/rewinddir unspecified, and tmpfs
    // really does hide them, so refresh the stream before enumerating.
    // SAFETY: stream remains live for the duration of this call.
    unsafe {
        libc::rewinddir(stream.0);
    }
    let mut names = Vec::new();
    loop {
        clear_errno();
        // SAFETY: stream remains live for the duration of this call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error);
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated and remains valid until readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(target_os = "linux")]
fn clear_errno() {
    // SAFETY: errno is thread-local.
    unsafe {
        *libc::__errno_location() = 0;
    }
}

#[cfg(not(target_os = "linux"))]
fn clear_errno() {
    // SAFETY: errno is thread-local.
    unsafe {
        *libc::__error() = 0;
    }
}

fn copy_payload_snapshot(source: &Path, destination: &Path) -> std::io::Result<()> {
    const MAX_PACKAGE_FILES: usize = 16_384;
    const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

    fn copy_dir(
        source: &Path,
        destination: &Path,
        files: &mut usize,
        bytes: &mut u64,
    ) -> std::io::Result<()> {
        prepare_private_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir(&entry.path(), &target, files, bytes)?;
            } else if file_type.is_file() {
                *files = files.saturating_add(1);
                let metadata = entry.metadata()?;
                *bytes = bytes.saturating_add(metadata.len());
                if *files > MAX_PACKAGE_FILES || *bytes > MAX_PACKAGE_BYTES {
                    return Err(std::io::Error::other(
                        "installed package exceeds compute snapshot bounds",
                    ));
                }
                let mut input = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(entry.path())?;
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o500)
                    .open(target)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
            } else {
                return Err(std::io::Error::other(
                    "installed package contains a symlink or special file",
                ));
            }
        }
        Ok(())
    }

    let mut files = 0;
    let mut bytes = 0;
    copy_dir(source, destination, &mut files, &mut bytes)
}

fn resource_plan(
    spec: &ComputeJobSpec,
    now_ms: u64,
) -> Result<CapturedResourcePlan, ComputeExecutionError> {
    let cpu_seconds = spec.limits.cpu_time_ms.saturating_add(999) / 1_000;
    let output_bytes = usize::try_from(spec.limits.output_bytes)
        .map_err(|_| ComputeExecutionError::Admission(ComputeError::InvalidLimits))?;
    CapturedResourcePlan {
        cpu_seconds,
        memory_bytes: spec.limits.memory_bytes,
        output_bytes,
        wall_time: Duration::from_millis(spec.expires_at_ms.saturating_sub(now_ms)),
    }
    .validate()
    .map_err(|_| ComputeExecutionError::Admission(ComputeError::InvalidLimits))
}

fn prepare_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[derive(Serialize)]
struct StagedComputeRequest<'a> {
    version: u16,
    job_id: String,
    workspace_id: u64,
    inputs: Vec<String>,
    output_directory: &'a str,
}

fn write_request(spec: &ComputeJobSpec, io_root: &Path) -> std::io::Result<()> {
    let request = StagedComputeRequest {
        version: spec.version,
        job_id: hex::encode(spec.job_id),
        workspace_id: spec.workspace_id,
        inputs: spec.inputs.iter().map(hex::encode).collect(),
        output_directory: "/run/tilo/app/.tilo-compute/outputs",
    };
    let bytes = serde_json::to_vec(&request).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(io_root.join("request.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()
}

struct JobDirectoryCleanup(Vec<PathBuf>);

impl Drop for JobDirectoryCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            if let Err(error) = fs::remove_dir_all(path) {
                tracing::warn!(path = %path.display(), %error, "failed to clean compute job staging");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{ComputeAccelerator, DeviceRole, PowerAvailability, ServiceCapabilities};
    use ed25519_dalek::{Signer, SigningKey};
    use tilo_app_package::blobs::{BlobHash, Collection};
    use tilo_app_package::signed::SignedRoot;

    fn spec() -> ComputeJobSpec {
        ComputeJobSpec {
            version: COMPUTE_JOB_SCHEMA_VERSION,
            job_id: [1; 16],
            requester: [2; 32],
            worker: [3; 32],
            workspace_id: 7,
            app_id: "so.tilo.video-editor".into(),
            package_digest: [4; 32],
            entrypoint: "render-export-v1".into(),
            arguments: Vec::new(),
            inputs: vec![[5; 32]],
            limits: ComputeResourceLimits {
                cpu_time_ms: 60_000,
                memory_bytes: 1024 * 1024 * 1024,
                output_bytes: 8 * 1024 * 1024,
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
            apps: BTreeSet::from(["so.tilo.video-editor".into()]),
            accelerators: BTreeSet::from([ComputeAccelerator::Gpu]),
            priority: 0,
            latency_ms: 4,
            power: PowerAvailability::External,
            energy_saver: false,
            active_assignments: 0,
            assignment_capacity: 2,
            online: true,
        }
    }

    #[derive(Clone)]
    struct FakeCatalogue(InstalledPackage);

    impl ComputePackageCatalogue for FakeCatalogue {
        fn resolve(&self, app_id: &str) -> Result<Option<InstalledPackage>, String> {
            Ok((self.0.id == app_id).then(|| self.0.clone()))
        }
    }

    #[derive(Clone, Default)]
    struct MemoryContent(Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>);

    impl MemoryContent {
        fn insert(&self, digest: [u8; 32], bytes: Vec<u8>) {
            self.0.lock().unwrap().insert(digest, bytes);
        }
    }

    impl ComputeContentStore for MemoryContent {
        fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.lock().unwrap().get(&digest).cloned())
        }

        fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().insert(digest, bytes.to_vec());
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum FakeTermination {
        Success,
        Timeout,
    }

    struct FakeSandbox {
        gpu: bool,
        termination: FakeTermination,
        output: Option<Vec<u8>>,
        replace_output_with_symlink: bool,
        launches: Arc<AtomicUsize>,
    }

    impl FakeSandbox {
        fn success(gpu: bool) -> Self {
            Self {
                gpu,
                termination: FakeTermination::Success,
                output: Some(b"rendered".to_vec()),
                replace_output_with_symlink: false,
                launches: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ComputeSandbox for FakeSandbox {
        fn gpu_available(&self) -> bool {
            self.gpu
        }

        fn launch(
            &self,
            invocation: ComputeSandboxInvocation<'_>,
        ) -> Result<CapturedRunOutput, String> {
            self.launches.fetch_add(1, Ordering::AcqRel);
            let request = invocation.host_job_dir.join(".tilo-compute/request.json");
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(request).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
            if value.get("inputs").is_none()
                || value.get("arguments").is_some()
                || value.get("environment").is_some()
            {
                return Err("compute request did not preserve the canonical input schema".into());
            }
            if self.replace_output_with_symlink {
                let output_dir = invocation.host_job_dir.join(".tilo-compute/outputs");
                fs::remove_dir(&output_dir).map_err(|error| error.to_string())?;
                std::os::unix::fs::symlink("inputs", output_dir)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(output) = &self.output {
                fs::write(
                    invocation.host_job_dir.join(".tilo-compute/outputs/result"),
                    output,
                )
                .map_err(|error| error.to_string())?;
            }
            let termination = match self.termination {
                FakeTermination::Success => CapturedTermination::Exited(ExitStatus::from_raw(0)),
                FakeTermination::Timeout => CapturedTermination::TimedOut,
            };
            Ok(CapturedRunOutput {
                termination,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    fn installed_fixture(root: &Path, spec: &mut ComputeJobSpec) -> InstalledPackage {
        let data_root = root.join("apps");
        let manifest = format!(
            "[package]\nid = {:?}\nversion = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"ed25519:{}\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n\
             [entrypoint]\nbin = \"bin/worker\"\ntype = \"cli\"\n\
             [cli]\ncommands = [{:?}]\n",
            spec.app_id,
            "00".repeat(32),
            spec.entrypoint
        );
        let collection =
            Collection::from_entries([("bin/worker".to_string(), BlobHash::of(b"signed worker"))]);
        let collection_bytes = collection.encode();
        let signing_key = SigningKey::from_bytes(&[11; 32]);
        let mut signed = SignedRoot::unsigned(
            BlobHash::of(manifest.as_bytes()),
            BlobHash::of(&collection_bytes),
            signing_key.verifying_key().to_bytes(),
        );
        signed.signature = Some([0; 64]);
        signed.signature = Some(signing_key.sign(&signed.signing_bytes()).to_bytes());
        let signed_bytes = signed.encode();
        let package_digest = *BlobHash::of(&signed_bytes).as_bytes();
        spec.package_digest = package_digest;
        // The package store's layout: the tree under pkgs/<id>, its signed
        // record beside it.
        let layout = package_store::Layout::new(&data_root);
        let id = tilo_app_package::PackageId::from_bytes(package_digest);
        let payload = layout.payload(id);
        fs::create_dir_all(payload.join("bin")).unwrap();
        fs::write(payload.join("bin/worker"), b"signed worker").unwrap();
        let record = layout.meta_dir(id);
        fs::create_dir_all(&record).unwrap();
        fs::write(record.join("signed-root"), signed_bytes).unwrap();
        fs::write(record.join("manifest.toml"), manifest.as_bytes()).unwrap();
        fs::write(record.join("collection.qcl"), &collection_bytes).unwrap();
        InstalledPackage {
            id: spec.app_id.clone(),
            version: "1.0.0".into(),
            package_id_hex: hex::encode(package_digest),
            payload_root: payload,
            manifest_path: layout.manifest(id),
            publisher_key_hex: hex::encode(signing_key.verifying_key().to_bytes()),
            pinned: false,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: vec![spec.entrypoint.clone()],
            runtime: String::new(),
            source: String::new(),
            held: false,
            payload_bytes: 0,
            system: false,
        }
    }

    fn make_executor(
        root: &Path,
        spec: &mut ComputeJobSpec,
        content: MemoryContent,
        sandbox: FakeSandbox,
    ) -> ComputeWorker<MemoryContent, FakeCatalogue, FakeSandbox> {
        let package = installed_fixture(root, spec);
        ComputeWorker::new(
            worker(),
            root.join("data"),
            root.join("worker-state"),
            content,
            FakeCatalogue(package),
            sandbox,
        )
    }

    #[test]
    fn compute_jobs_are_admitted_exactly_and_move_monotonically() {
        {
            assert_eq!(admit_compute_job(&spec(), &worker(), 200), Ok(()));
        }
        {
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
            // The threshold is the mesh's one battery rule, not a local copy.
            candidate.power = PowerAvailability::Battery(15);
            assert_eq!(
                admit_compute_job(&spec(), &candidate, 200),
                Err(ComputeError::WorkerUnavailable)
            );
            candidate.power = PowerAvailability::Battery(16);
            assert_eq!(admit_compute_job(&spec(), &candidate, 200), Ok(()));
            candidate = worker();
            candidate.accelerators.clear();
            assert_eq!(
                admit_compute_job(&spec(), &candidate, 200),
                Err(ComputeError::WorkerUnavailable)
            );
        }
        {
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
        {
            let root = tempfile::tempdir().unwrap();
            let content = MemoryContent::default();
            let input = b"source frame".to_vec();
            let input_digest = *blake3::hash(&input).as_bytes();
            content.insert(input_digest, input);
            let mut job = spec();
            job.inputs = vec![input_digest];
            let sandbox = FakeSandbox::success(true);
            let launches = Arc::clone(&sandbox.launches);
            let executor = make_executor(root.path(), &mut job, content.clone(), sandbox);

            let result = executor
                .execute([2; 32], 7, &job, 200, CaptureCancellation::default())
                .unwrap();
            let expected = *blake3::hash(b"rendered").as_bytes();
            assert_eq!(
                result.lifecycle.phase,
                ComputeJobPhase::Completed {
                    outputs: vec![expected]
                }
            );
            assert_eq!(content.get(expected).unwrap(), Some(b"rendered".to_vec()));
            assert_eq!(launches.load(Ordering::Acquire), 1);
            assert_eq!(
                executor.execute([2; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::Replay)
            );
            assert_eq!(launches.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn compute_launches_fail_closed_on_bad_routes_inputs_and_payloads() {
        {
            let root = tempfile::tempdir().unwrap();
            let mut job = spec();
            job.inputs.clear();
            let sandbox = FakeSandbox::success(false);
            let launches = Arc::clone(&sandbox.launches);
            let executor = make_executor(root.path(), &mut job, MemoryContent::default(), sandbox);
            assert_eq!(
                executor.execute([9; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::RouteMismatch)
            );
            assert_eq!(
                executor.execute([2; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::GpuUnavailable)
            );
            assert_eq!(launches.load(Ordering::Acquire), 0);

            job.limits.gpu = false;
            job.arguments.push("/host/path".into());
            assert_eq!(
                executor.execute([2; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::Admission(
                    ComputeError::RequesterArgumentsForbidden
                ))
            );
            assert_eq!(launches.load(Ordering::Acquire), 0);
        }
        {
            let root = tempfile::tempdir().unwrap();
            let content = MemoryContent::default();
            let mut job = spec();
            job.limits.gpu = false;
            let claimed = [8; 32];
            job.inputs = vec![claimed];
            content.insert(claimed, b"different bytes".to_vec());
            let sandbox = FakeSandbox::success(false);
            let launches = Arc::clone(&sandbox.launches);
            let executor = make_executor(root.path(), &mut job, content, sandbox);
            assert_eq!(
                executor.execute([2; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::InputDigestMismatch)
            );
            assert!(matches!(
                executor.lifecycle(job.job_id).unwrap().phase,
                ComputeJobPhase::Failed { .. }
            ));
            assert_eq!(launches.load(Ordering::Acquire), 0);

            let second_root = tempfile::tempdir().unwrap();
            let content = MemoryContent::default();
            let oversized = vec![0_u8; 17 * 1024 * 1024];
            let digest = *blake3::hash(&oversized).as_bytes();
            content.insert(digest, oversized);
            let mut second = spec();
            second.job_id = [9; 16];
            second.limits.gpu = false;
            second.limits.memory_bytes = 16 * 1024 * 1024;
            second.inputs = vec![digest];
            let executor = make_executor(
                second_root.path(),
                &mut second,
                content,
                FakeSandbox::success(false),
            );
            assert_eq!(
                executor.execute([2; 32], 7, &second, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::InputsTooLarge)
            );
        }
        {
            let root = tempfile::tempdir().unwrap();
            let mut cancelled_job = spec();
            cancelled_job.inputs.clear();
            cancelled_job.limits.gpu = false;
            let sandbox = FakeSandbox::success(false);
            let launches = Arc::clone(&sandbox.launches);
            let executor = make_executor(
                root.path(),
                &mut cancelled_job,
                MemoryContent::default(),
                sandbox,
            );
            let cancellation = CaptureCancellation::default();
            cancellation.cancel();
            let result = executor
                .execute([2; 32], 7, &cancelled_job, 200, cancellation)
                .unwrap();
            assert_eq!(result.lifecycle.phase, ComputeJobPhase::Cancelled);
            assert_eq!(result.lifecycle.sequence, 2);
            assert_eq!(launches.load(Ordering::Acquire), 0);

            let timeout_root = tempfile::tempdir().unwrap();
            let mut timeout_job = spec();
            timeout_job.job_id = [6; 16];
            timeout_job.inputs.clear();
            timeout_job.limits.gpu = false;
            let executor = make_executor(
                timeout_root.path(),
                &mut timeout_job,
                MemoryContent::default(),
                FakeSandbox {
                    gpu: false,
                    termination: FakeTermination::Timeout,
                    output: None,
                    replace_output_with_symlink: false,
                    launches: Arc::new(AtomicUsize::new(0)),
                },
            );
            let result = executor
                .execute(
                    [2; 32],
                    7,
                    &timeout_job,
                    200,
                    CaptureCancellation::default(),
                )
                .unwrap();
            assert_eq!(
                result.lifecycle.phase,
                ComputeJobPhase::Failed {
                    code: "timeout".into()
                }
            );
            assert_eq!(result.lifecycle.sequence, 2);
        }
        {
            let root = tempfile::tempdir().unwrap();
            let mut job = spec();
            job.inputs.clear();
            job.limits.gpu = false;
            let package = installed_fixture(root.path(), &mut job);
            fs::write(package.payload_root.join("bin/worker"), b"tampered").unwrap();
            let sandbox = FakeSandbox::success(false);
            let launches = Arc::clone(&sandbox.launches);
            let executor = ComputeWorker::new(
                worker(),
                root.path().join("data"),
                root.path().join("state"),
                MemoryContent::default(),
                FakeCatalogue(package),
                sandbox,
            );
            assert_eq!(
                executor.execute([2; 32], 7, &job, 200, CaptureCancellation::default()),
                Err(ComputeExecutionError::PackageDigestMismatch)
            );
            assert_eq!(launches.load(Ordering::Acquire), 0);
        }
        {
            let root = tempfile::tempdir().unwrap();
            let content = MemoryContent::default();
            let bytes = b"private input".to_vec();
            let digest = *blake3::hash(&bytes).as_bytes();
            content.insert(digest, bytes);
            let mut job = spec();
            job.inputs = vec![digest];
            job.limits.gpu = false;
            let executor = make_executor(
                root.path(),
                &mut job,
                content,
                FakeSandbox {
                    gpu: false,
                    termination: FakeTermination::Success,
                    output: None,
                    replace_output_with_symlink: true,
                    launches: Arc::new(AtomicUsize::new(0)),
                },
            );
            let result = executor
                .execute([2; 32], 7, &job, 200, CaptureCancellation::default())
                .unwrap();
            assert_eq!(
                result.lifecycle.phase,
                ComputeJobPhase::Completed {
                    outputs: Vec::new()
                }
            );
        }
    }
}
