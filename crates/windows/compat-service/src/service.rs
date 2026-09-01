use std::sync::Mutex;

use collab_types::caps::CapOverride;
use compat_catalogue::{
    ApplicationQuery, Catalogue, CatalogueError, LaunchPlan, Layer, MatchKind, VmFallback,
};
use launcher::capabilities::CapabilityPlan;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CapsuleCommandPlan, CapsuleError, CapsuleProgram, CapsuleStore, ExecutionError,
    ExecutionOutcome, HostSession, LaunchResourceBroker, NetworkShape, PlanError, ResourceError,
    ResourceRequest, RuntimeRegistry, SandboxExecutor, SandboxPlanner,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Wine,
    Proton,
    Vm,
}

impl From<Layer> for RouteKind {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Wine => Self::Wine,
            Layer::Proton => Self::Proton,
            Layer::Vm => Self::Vm,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    DefaultWine,
    ProductPublisher,
    Executable,
    ExecutableDigest,
    GameId,
    StoreProduct,
    AppId,
    PackageId,
}

impl From<MatchKind> for MatchReason {
    fn from(value: MatchKind) -> Self {
        match value {
            MatchKind::DefaultWine => Self::DefaultWine,
            MatchKind::ProductPublisher => Self::ProductPublisher,
            MatchKind::Executable => Self::Executable,
            MatchKind::ExecutableDigest => Self::ExecutableDigest,
            MatchKind::GameId => Self::GameId,
            MatchKind::StoreProduct => Self::StoreProduct,
            MatchKind::AppId => Self::AppId,
            MatchKind::PackageId => Self::PackageId,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccessSummary {
    pub files_read_only: usize,
    pub files_read_write: usize,
    pub network: String,
    pub microphone: bool,
    pub audio_output: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExplainStatus {
    pub route: RouteKind,
    pub catalogue_record: Option<String>,
    pub match_reason: MatchReason,
    pub capsule_id: Option<String>,
    pub runner: String,
    pub security_profile: String,
    pub vm_fallback_was_explicit: bool,
    pub access: AccessSummary,
    /// Human-facing reason suitable for the Windows manager details panel.
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct LaunchInput {
    pub app_id: String,
    pub workspace_id: String,
    pub program_id: String,
    pub query: ApplicationQuery,
    pub vm_fallback: VmFallback,
    /// Canonical qOS capability records, not a compatibility-local policy.
    pub capabilities: Vec<CapOverride>,
    pub now_unix: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchReceipt {
    pub explanation: ExplainStatus,
    pub success: bool,
    pub exit_code: Option<i32>,
}

pub struct VmRouteRequest<'a> {
    pub route: &'a LaunchPlan,
    pub app_id: &'a str,
    pub workspace_id: &'a str,
    pub published_application_id: &'a str,
    pub capabilities: &'a [CapOverride],
    pub resolved_capabilities: &'a CapabilityPlan,
}

/// Adapter boundary implemented by the single managed `windows-vm`
/// appliance. Raw canonical grants accompany the resolved launcher view so
/// the adapter need not reconstruct or invent permission semantics.
pub trait VmDelegate: Send + Sync {
    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError>;
}

pub struct CompatibilityService<R, B, E, V> {
    catalogue: Catalogue,
    store: CapsuleStore,
    runtimes: R,
    resources: B,
    executor: E,
    vm: V,
    session: HostSession,
    /// Prefix setup and recipe checks are serialized by the singleton service.
    capsule_gate: Mutex<()>,
}

impl<R, B, E, V> CompatibilityService<R, B, E, V>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
    E: SandboxExecutor,
    V: VmDelegate,
{
    pub fn new(
        catalogue: Catalogue,
        store: CapsuleStore,
        runtimes: R,
        resources: B,
        executor: E,
        vm: V,
        session: HostSession,
    ) -> Result<Self, ServiceError> {
        catalogue.validate()?;
        session.validate()?;
        Ok(Self {
            catalogue,
            store,
            runtimes,
            resources,
            executor,
            vm,
            session,
            capsule_gate: Mutex::new(()),
        })
    }

    /// Resolve only. It performs no prefix, broker, VM, or process mutation.
    pub fn explain(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<ExplainStatus, ServiceError> {
        validate_launch_id("app id", app_id)?;
        let route = self.resolve_route(query, vm_fallback, app_id)?;
        let resolved = CapabilityPlan::for_app(app_id, capabilities, now_unix);
        Ok(explanation(&route, vm_fallback, &resolved, false))
    }

    pub fn register_program(
        &self,
        capsule_id: &str,
        program: CapsuleProgram,
    ) -> Result<(), ServiceError> {
        let _guard = self
            .capsule_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.store.register_program(capsule_id, program)?;
        Ok(())
    }

    pub fn launch(&self, input: &LaunchInput) -> Result<LaunchReceipt, ServiceError> {
        validate_launch_id("app id", &input.app_id)?;
        validate_launch_id("workspace id", &input.workspace_id)?;
        validate_launch_id("program id", &input.program_id)?;
        let route = self.resolve_route(&input.query, input.vm_fallback, &input.app_id)?;
        let capabilities =
            CapabilityPlan::for_app(&input.app_id, &input.capabilities, input.now_unix);
        if route.runtime.layer == Layer::Vm {
            if input.vm_fallback != VmFallback::Explicit {
                return Err(ServiceError::VmWasNotExplicit);
            }
            let running = self.vm.launch(VmRouteRequest {
                route: &route,
                app_id: &input.app_id,
                workspace_id: &input.workspace_id,
                published_application_id: &input.program_id,
                capabilities: &input.capabilities,
                resolved_capabilities: &capabilities,
            })?;
            return Ok(LaunchReceipt {
                explanation: explanation(&route, input.vm_fallback, &capabilities, false),
                success: running.process.exit_code == Some(0),
                exit_code: running.process.exit_code,
            });
        }

        // Serialize only state/prefix maintenance. Do not hold this guard
        // while a Windows application is alive: unrelated launches continue.
        let (capsule, program) = {
            let _guard = self
                .capsule_gate
                .lock()
                .map_err(|_| ServiceError::StateLock)?;
            let capsule = self.store.ensure_for_plan(&route)?;
            let program = capsule
                .program(&input.program_id)
                .cloned()
                .ok_or_else(|| ServiceError::ProgramNotRegistered(input.program_id.clone()))?;
            (capsule, program)
        };
        let lease = self.resources.materialize(ResourceRequest {
            app_id: &input.app_id,
            workspace_id: &input.workspace_id,
            capabilities: &capabilities,
        })?;
        let result: Result<(CapsuleCommandPlan, ExecutionOutcome), ServiceError> = (|| {
            let planner = SandboxPlanner::new(&self.store, &self.runtimes, &self.session)?;
            let plan = planner.plan(
                &route,
                &capsule,
                &program,
                &capabilities,
                &input.workspace_id,
                &lease,
            )?;
            let outcome = self.executor.execute(&plan)?;
            Ok((plan, outcome))
        })();
        self.resources.revoke(&lease.id);
        let (command, outcome): (CapsuleCommandPlan, ExecutionOutcome) = result?;
        Ok(LaunchReceipt {
            explanation: explanation(
                &route,
                input.vm_fallback,
                &capabilities,
                command.audio_output,
            ),
            success: outcome.success,
            exit_code: outcome.exit_code,
        })
    }

    fn resolve_route(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
    ) -> Result<LaunchPlan, ServiceError> {
        let mut route = self.catalogue.resolver()?.resolve(query, vm_fallback);
        if route.record_id.is_none() && route.runtime.layer == Layer::Wine {
            let digest = Sha256::digest(app_id.as_bytes());
            route.runtime.id = format!("wine-{}", hex_lower(digest.iter().take(16).copied()));
        }
        Ok(route)
    }
}

fn hex_lower(bytes: impl IntoIterator<Item = u8>) -> String {
    let mut output = String::new();
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(value: u8) -> char {
    match value {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

fn explanation(
    route: &LaunchPlan,
    vm_fallback: VmFallback,
    capabilities: &CapabilityPlan,
    audio_output: bool,
) -> ExplainStatus {
    let network_shape = NetworkShape::from_capabilities(capabilities);
    let route_kind = RouteKind::from(route.runtime.layer);
    let runner = format!("{} {}", route.runner.family, route.runner.version);
    let access = AccessSummary {
        files_read_only: capabilities
            .ro_paths
            .iter()
            .filter(|path| !path.starts_with("/dev"))
            .count(),
        files_read_write: capabilities
            .rw_paths
            .iter()
            .filter(|path| !path.starts_with("/dev"))
            .count(),
        network: match network_shape {
            NetworkShape::Offline => "none".to_string(),
            NetworkShape::Filtered => format!("{} approved host(s)", capabilities.net_hosts.len()),
            NetworkShape::Unrestricted => "unrestricted".to_string(),
        },
        microphone: capabilities
            .devices
            .iter()
            .any(|device| device == "microphone"),
        audio_output,
    };
    ExplainStatus {
        route: route_kind,
        catalogue_record: route.record_id.clone(),
        match_reason: MatchReason::from(route.match_kind),
        capsule_id: (route.runtime.layer != Layer::Vm).then(|| route.runtime.id.clone()),
        runner: runner.clone(),
        security_profile: route.security_profile.as_str().to_string(),
        vm_fallback_was_explicit: vm_fallback == VmFallback::Explicit,
        summary: match route_kind {
            RouteKind::Wine => format!(
                "Runs in isolated capsule {} with {runner}",
                route.runtime.id
            ),
            RouteKind::Proton => {
                format!(
                    "Runs through UMU in isolated capsule {} with {runner}",
                    route.runtime.id
                )
            }
            RouteKind::Vm => {
                "Runs as one published app in the managed Windows fallback".to_string()
            }
        },
        access,
    }
}

fn validate_launch_id(label: &'static str, value: &str) -> Result<(), ServiceError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ServiceError::InvalidRequest(label));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Resources(#[from] ResourceError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Vm(#[from] windows_vm::LaunchError),
    #[error("invalid launch field {0}")]
    InvalidRequest(&'static str),
    #[error("program {0:?} is not registered in the selected capsule")]
    ProgramNotRegistered(String),
    #[error("VM route was selected without explicit fallback authorization")]
    VmWasNotExplicit,
    #[error("capsule service state lock is poisoned")]
    StateLock,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    use crate::{ExecutionOutcome, LaunchLease, ResourceError, SystemRuntimeRegistry};

    use super::*;

    struct Resources;

    impl LaunchResourceBroker for Resources {
        fn materialize(&self, _: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
            Ok(LaunchLease::deny_all("lease"))
        }

        fn revoke(&self, _: &str) {}
    }

    struct Executor;

    impl SandboxExecutor for Executor {
        fn execute(&self, _: &CapsuleCommandPlan) -> Result<ExecutionOutcome, ExecutionError> {
            Ok(ExecutionOutcome {
                success: true,
                exit_code: Some(0),
            })
        }
    }

    #[derive(Default)]
    struct Vm {
        calls: AtomicUsize,
    }

    impl VmDelegate for Vm {
        fn launch(
            &self,
            _: VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(windows_vm::LaunchError::Agent(
                windows_vm::AgentError::Unavailable,
            ))
        }
    }

    fn catalogue() -> Catalogue {
        Catalogue::from_json(
            r#"{
              "schema_version": 1,
              "records": [{
                "id": "vm-only",
                "version": 1,
                "provenance": "built_in",
                "signed": true,
                "matches": [{"executable": "special.exe"}],
                "recipe": {
                  "layer": "vm",
                  "runner": {"family": "windows-vm", "version": "managed"},
                  "settings": {},
                  "security_profile": "windows-app"
                }
              }]
            }"#,
        )
        .unwrap()
    }

    fn service(
        directory: &TempDir,
    ) -> CompatibilityService<SystemRuntimeRegistry, Resources, Executor, Vm> {
        CompatibilityService::new(
            catalogue(),
            CapsuleStore::new(directory.path()).unwrap(),
            SystemRuntimeRegistry::new(),
            Resources,
            Executor,
            Vm::default(),
            HostSession {
                uid: 1000,
                wayland_socket: PathBuf::from("/run/user/1000/nirios-ws-test"),
                render_node: PathBuf::from("/dev/dri/renderD128"),
            },
        )
        .unwrap()
    }

    #[test]
    fn matching_vm_recipe_does_not_auto_route_without_explicit_action() {
        let directory = TempDir::new().unwrap();
        let service = service(&directory);
        let query = ApplicationQuery::executable("special.exe");
        let status = service
            .explain(&query, VmFallback::Disabled, "app", &[], Some(1))
            .unwrap();
        assert_eq!(status.route, RouteKind::Wine);

        let result = service.launch(&LaunchInput {
            app_id: "app".to_string(),
            workspace_id: "workspace".to_string(),
            program_id: "special".to_string(),
            query,
            vm_fallback: VmFallback::Disabled,
            capabilities: Vec::new(),
            now_unix: Some(1),
        });
        assert!(matches!(result, Err(ServiceError::ProgramNotRegistered(_))));
        assert_eq!(service.vm.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn explicit_vm_action_is_visible_before_delegate_is_called() {
        let directory = TempDir::new().unwrap();
        let service = service(&directory);
        let status = service
            .explain(
                &ApplicationQuery::executable("special.exe"),
                VmFallback::Explicit,
                "app",
                &[],
                Some(1),
            )
            .unwrap();
        assert_eq!(status.route, RouteKind::Vm);
        assert!(status.vm_fallback_was_explicit);
    }

    #[test]
    fn protocol_launch_has_no_client_supplied_capability_field() {
        let request = serde_json::json!({
            "method": "launch",
            "version": 1,
            "app_id": "app",
            "workspace_id": "workspace",
            "program_id": "special",
            "query": {"executable": "special.exe"},
            "explicit_vm_fallback": false,
            "capabilities": [{"effect": "grant"}]
        });
        assert!(serde_json::from_value::<crate::ProtocolRequest>(request).is_err());
    }
}
