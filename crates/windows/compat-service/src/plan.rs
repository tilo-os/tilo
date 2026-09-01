use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use compat_catalogue::{Architecture, LaunchPlan, Layer, Runner};
use launcher::capabilities::CapabilityPlan;
use launcher::gpu::{IcdSearch, select_for_node};
use launcher::sandbox::NetGateBind;
use launcher::seccomp::SeccompProfile;
use thiserror::Error;

use crate::store::{
    CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore, LaunchPaths, validate_id,
};

const PREFIX_MOUNT: &str = "/run/qos/capsule/prefix";
const HOME_MOUNT: &str = "/run/qos/capsule/home";
const FILES_MOUNT: &str = "/run/qos/files";
const SHIM_HOST: &str = "/usr/bin/qos-launcher";
const SHIM_MOUNT: &str = "/qos-launcher";
const LEASE_ROOT: &str = "/run/qos/windows-compat/leases";
const PINNED_PROTON: &str = "UMU-Proton-10.0-4";
const PROTON_HOST: &str = "/usr/lib/qos/windows-runners/proton/UMU-Proton-10.0-4";
const PROTON_MOUNT: &str = "/opt/qos/proton";

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeBind {
    host: PathBuf,
    sandbox: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeImage {
    binds: Vec<RuntimeBind>,
    sandbox_entrypoint: PathBuf,
    environment: Vec<(&'static str, String)>,
    needs_nested_runtime: bool,
}

impl RuntimeImage {
    pub fn host_paths(&self) -> impl Iterator<Item = &Path> {
        self.binds.iter().map(|binding| binding.host.as_path())
    }

    #[must_use]
    pub fn sandbox_entrypoint(&self) -> &Path {
        &self.sandbox_entrypoint
    }
}

pub trait RuntimeRegistry: Send + Sync {
    fn resolve(&self, layer: Layer, runner: &Runner) -> Result<RuntimeImage, PlanError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRuntimeRegistry;

impl SystemRuntimeRegistry {
    pub const fn new() -> Self {
        Self
    }
}

impl RuntimeRegistry for SystemRuntimeRegistry {
    fn resolve(&self, layer: Layer, runner: &Runner) -> Result<RuntimeImage, PlanError> {
        validate_runner_token(&runner.family)?;
        validate_runner_token(&runner.version)?;
        let expected_family = match layer {
            Layer::Wine => "wine",
            Layer::Proton => "umu-proton",
            Layer::Vm => return Err(PlanError::VmIsNotCapsule),
        };
        if runner.family != expected_family
            && !(layer == Layer::Proton && runner.family == "proton")
        {
            return Err(PlanError::RunnerLayerMismatch);
        }
        match layer {
            Layer::Wine if runner.version == "system" => Ok(RuntimeImage {
                binds: vec![
                    runtime_bind("/usr/bin/wine", "/usr/bin/wine"),
                    runtime_bind("/usr/bin/wineserver", "/usr/bin/wineserver"),
                    runtime_bind("/usr/bin/wineboot", "/usr/bin/wineboot"),
                ],
                sandbox_entrypoint: PathBuf::from("/usr/bin/wine"),
                environment: Vec::new(),
                needs_nested_runtime: false,
            }),
            Layer::Wine => Err(PlanError::RunnerNotInstalled(format!(
                "wine {}",
                runner.version
            ))),
            Layer::Proton if matches!(runner.version.as_str(), PINNED_PROTON | "10.0-4") => {
                Ok(RuntimeImage {
                    // Exact paths installed by os/Containerfile. UMU owns the
                    // Steam Runtime and protonfix invocation contract.
                    binds: vec![
                        runtime_bind("/usr/libexec/qos-umu-run", "/usr/libexec/qos-umu-run"),
                        runtime_bind("/usr/libexec/umu_run.py", "/usr/libexec/umu_run.py"),
                        // The pinned zipapp's shebang is `/usr/bin/env python3`.
                        runtime_bind("/usr/bin/env", "/usr/bin/env"),
                        runtime_bind("/usr/bin/python3", "/usr/bin/python3"),
                        runtime_bind("/bin/sh", "/bin/sh"),
                        runtime_bind("/usr/bin/bwrap", "/usr/bin/bwrap"),
                        runtime_bind(PROTON_HOST, PROTON_MOUNT),
                    ],
                    sandbox_entrypoint: PathBuf::from("/usr/libexec/qos-umu-run"),
                    environment: vec![("PROTONPATH", PROTON_MOUNT.to_string())],
                    needs_nested_runtime: true,
                })
            }
            Layer::Proton => Err(PlanError::RunnerNotInstalled(format!(
                "{} {}",
                runner.family, runner.version
            ))),
            Layer::Vm => Err(PlanError::VmIsNotCapsule),
        }
    }
}

fn runtime_bind(host: &str, sandbox: &str) -> RuntimeBind {
    RuntimeBind {
        host: PathBuf::from(host),
        sandbox: PathBuf::from(sandbox),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSession {
    pub uid: u32,
    pub wayland_socket: PathBuf,
    pub render_node: PathBuf,
}

impl HostSession {
    pub fn validate(&self) -> Result<(), PlanError> {
        let runtime = PathBuf::from(format!("/run/user/{}", self.uid));
        if !self.wayland_socket.starts_with(&runtime)
            || self.wayland_socket.parent() != Some(runtime.as_path())
        {
            return Err(PlanError::UnsafeWaylandSocket);
        }
        let render_name = self
            .render_node
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if self.render_node.parent() != Some(Path::new("/dev/dri"))
            || !render_name.strip_prefix("renderD").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            })
        {
            return Err(PlanError::UnsafeRenderNode);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AudioProtocol {
    PipeWire,
    Pulse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioBinding {
    pub protocol: AudioProtocol,
    /// Broker-owned filtered socket. It must live under this lease.
    pub host_socket: PathBuf,
    /// Whether capture is enabled on the proxy. Playback is always allowed.
    pub input_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkShape {
    Offline,
    Filtered,
    Unrestricted,
}

impl NetworkShape {
    #[must_use]
    pub fn from_capabilities(capabilities: &CapabilityPlan) -> Self {
        if !capabilities.allow_network || capabilities.net_hosts.is_empty() {
            Self::Offline
        } else if capabilities.net_hosts.as_slice() == ["*"] {
            Self::Unrestricted
        } else {
            Self::Filtered
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResourceRequest<'a> {
    pub app_id: &'a str,
    pub workspace_id: &'a str,
    pub capabilities: &'a CapabilityPlan,
}

#[derive(Clone, Debug)]
pub struct LaunchLease {
    pub id: String,
    pub audio: Vec<AudioBinding>,
    pub net_gate: Option<NetGateBind>,
}

impl LaunchLease {
    #[must_use]
    pub fn deny_all(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            audio: Vec::new(),
            net_gate: None,
        }
    }

    pub fn validate(
        &self,
        capabilities: &CapabilityPlan,
        network: NetworkShape,
    ) -> Result<(), PlanError> {
        validate_id("lease id", &self.id)?;
        let root = PathBuf::from(LEASE_ROOT).join(&self.id);
        let microphone = capabilities
            .devices
            .iter()
            .any(|device| device == "microphone");
        let mut protocols = HashSet::new();
        for audio in &self.audio {
            if !audio.host_socket.starts_with(&root) {
                return Err(PlanError::ResourceEscalation("audio socket"));
            }
            if audio.input_enabled && !microphone {
                return Err(PlanError::ResourceEscalation("microphone"));
            }
            if !protocols.insert(audio.protocol) {
                return Err(PlanError::DuplicateAudioProtocol);
            }
        }
        match (network, self.net_gate.as_ref()) {
            (NetworkShape::Filtered, Some(gate)) => {
                if !gate.host_socket.starts_with(&root)
                    || gate.sandbox_socket != Path::new("/run/qos/network/gate.sock")
                    || gate.port < 1024
                {
                    return Err(PlanError::ResourceEscalation("network gate"));
                }
            }
            (NetworkShape::Filtered, None) => return Err(PlanError::MissingNetworkGate),
            (NetworkShape::Offline | NetworkShape::Unrestricted, Some(_)) => {
                return Err(PlanError::UnexpectedNetworkGate);
            }
            (NetworkShape::Offline | NetworkShape::Unrestricted, None) => {}
        }
        Ok(())
    }
}

pub trait LaunchResourceBroker: Send + Sync {
    fn materialize(&self, request: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError>;
    fn revoke(&self, lease_id: &str);
}

#[derive(Debug)]
pub struct CapsuleCommandPlan {
    /// Bubblewrap flags and the fixed in-sandbox command. Host paths here are
    /// derived only from daemon configuration, capsule state, or resolved grants.
    pub bwrap_argv: Vec<String>,
    /// UMU needs a nested Bubblewrap for Steam Runtime. Proton retains the
    /// outer bwrap, Landlock and network namespace; Wine additionally gets
    /// the qOS baseline syscall filter.
    pub seccomp_profile: Option<SeccompProfile>,
    pub capsule_id: String,
    pub runner_label: String,
    pub network: NetworkShape,
    pub read_grants: usize,
    pub write_grants: usize,
    pub microphone: bool,
    pub audio_output: bool,
    pub lease_id: String,
    // Open directory descriptors backing the prefix/home bind sources. Kept
    // private so a plan cannot be copied after its authority-bearing handles.
    _launch_paths: LaunchPaths,
}

pub struct SandboxPlanner<'a, R> {
    store: &'a CapsuleStore,
    runtimes: &'a R,
    session: &'a HostSession,
    icd_search: IcdSearch,
}

impl<'a, R: RuntimeRegistry> SandboxPlanner<'a, R> {
    pub fn new(
        store: &'a CapsuleStore,
        runtimes: &'a R,
        session: &'a HostSession,
    ) -> Result<Self, PlanError> {
        session.validate()?;
        Ok(Self {
            store,
            runtimes,
            session,
            icd_search: IcdSearch::default(),
        })
    }

    #[must_use]
    pub fn with_icd_search(mut self, search: IcdSearch) -> Self {
        self.icd_search = search;
        self
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the ordered namespace plan is kept together so later arguments cannot weaken earlier policy"
    )]
    pub fn plan(
        &self,
        route: &LaunchPlan,
        capsule: &CapsuleState,
        program: &CapsuleProgram,
        capabilities: &CapabilityPlan,
        workspace_id: &str,
        lease: &LaunchLease,
    ) -> Result<CapsuleCommandPlan, PlanError> {
        validate_id("workspace id", workspace_id)?;
        capsule.validate()?;
        program.validate()?;
        if capsule.id != route.runtime.id || capsule.layer != route.runtime.layer {
            return Err(PlanError::CapsuleRouteMismatch);
        }
        let runtime = self.runtimes.resolve(capsule.layer, &capsule.runner)?;
        let network = NetworkShape::from_capabilities(capabilities);
        lease.validate(capabilities, network)?;

        let launch_paths = self.store.open_launch_paths(&capsule.id)?;
        let prefix_host = launch_paths.prefix_source();
        let home_host = launch_paths.home_source();

        let wayland =
            launcher::sandbox::wayland_bind_for(&self.session.wayland_socket, self.session.uid);
        let gpu = select_for_node(&self.session.render_node, &self.icd_search);
        let mut argv = Vec::new();
        launcher::sandbox::push_clean_environment(&mut argv);
        argv.extend(["--unshare-all".to_string()]);
        if network == NetworkShape::Unrestricted {
            argv.push("--share-net".to_string());
        }
        argv.extend([
            "--die-with-parent".to_string(),
            "--new-session".to_string(),
            "--tmpfs".to_string(),
            "/tmp".to_string(),
            "--proc".to_string(),
            "/proc".to_string(),
            "--dev".to_string(),
            "/dev".to_string(),
        ]);
        for directory in [
            "/run/qos",
            "/run/qos/capsule",
            FILES_MOUNT,
            "/run/qos/files/read",
            "/run/qos/files/write",
            "/run/qos/network",
            "/opt/qos",
            PROTON_MOUNT,
            "/usr/bin",
            "/usr/libexec",
        ] {
            argv.extend(["--dir".to_string(), directory.to_string()]);
        }
        launcher::profile::push_system_profile(&mut argv, network == NetworkShape::Unrestricted);
        for binding in &runtime.binds {
            bind(&mut argv, "--ro-bind", &binding.host, &binding.sandbox);
        }
        bind(&mut argv, "--bind", &prefix_host, PREFIX_MOUNT);
        bind(&mut argv, "--bind", &home_host, HOME_MOUNT);

        let mut landlock_read =
            launcher::profile::landlock_ro_paths(network == NetworkShape::Unrestricted);
        let mut landlock_write = launcher::profile::landlock_rw_device_paths();
        landlock_read.extend(runtime.binds.iter().map(|binding| binding.sandbox.clone()));
        landlock_write.extend([PathBuf::from(PREFIX_MOUNT), PathBuf::from(HOME_MOUNT)]);
        map_granted_files(
            &mut argv,
            capabilities,
            &mut landlock_read,
            &mut landlock_write,
        );

        launcher::sandbox::push_wayland_bind(&mut argv, &wayland);
        landlock_write.push(wayland.sandbox_socket.clone());
        launcher::sandbox::push_gpu_selection(&mut argv, &gpu, true);
        landlock_write.push(self.session.render_node.clone());
        landlock_read.extend(gpu.manifest_files());
        push_audio(&mut argv, lease, self.session.uid, &mut landlock_write);
        if let Some(gate) = &lease.net_gate {
            launcher::sandbox::push_net_gate(&mut argv, gate);
            landlock_write.push(gate.sandbox_socket.clone());
        }

        push_fixed_environment(&mut argv, capsule, route)?;
        for (key, value) in &runtime.environment {
            setenv(&mut argv, key, value);
        }
        launcher::sandbox::push_workspace_id(&mut argv, workspace_id);
        if !runtime.needs_nested_runtime {
            argv.extend(["--seccomp".to_string(), crate::SECCOMP_CHILD_FD.to_string()]);
        }
        argv.extend([
            "--ro-bind".to_string(),
            SHIM_HOST.to_string(),
            SHIM_MOUNT.to_string(),
            SHIM_MOUNT.to_string(),
            launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_string(),
        ]);
        for path in landlock_read {
            argv.extend(["--ro".to_string(), path.display().to_string()]);
        }
        for path in landlock_write {
            argv.extend(["--rw".to_string(), path.display().to_string()]);
        }
        match network {
            NetworkShape::Offline => argv.push("--net-restricted".to_string()),
            NetworkShape::Filtered => {
                let gate = lease
                    .net_gate
                    .as_ref()
                    .ok_or(PlanError::MissingNetworkGate)?;
                argv.extend([
                    "--net-gate".to_string(),
                    gate.sandbox_socket.display().to_string(),
                    "--net-gate-port".to_string(),
                    gate.port.to_string(),
                ]);
            }
            NetworkShape::Unrestricted => {}
        }
        argv.push("--".to_string());
        argv.push(runtime.sandbox_entrypoint().display().to_string());
        argv.push(
            Path::new(PREFIX_MOUNT)
                .join(&program.prefix_relative_executable)
                .display()
                .to_string(),
        );
        argv.extend(program.arguments.iter().cloned());

        Ok(CapsuleCommandPlan {
            bwrap_argv: argv,
            seccomp_profile: (!runtime.needs_nested_runtime).then_some(SeccompProfile::QosDefault),
            capsule_id: capsule.id.clone(),
            runner_label: format!("{} {}", capsule.runner.family, capsule.runner.version),
            network,
            read_grants: capabilities.ro_paths.len(),
            write_grants: capabilities.rw_paths.len(),
            microphone: capabilities
                .devices
                .iter()
                .any(|device| device == "microphone"),
            audio_output: !lease.audio.is_empty(),
            lease_id: lease.id.clone(),
            _launch_paths: launch_paths,
        })
    }
}

fn map_granted_files(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
    landlock_read: &mut Vec<PathBuf>,
    landlock_write: &mut Vec<PathBuf>,
) {
    for (index, source) in capabilities.ro_paths.iter().enumerate() {
        if is_host_microphone_endpoint(capabilities, source) {
            continue;
        }
        if source.starts_with("/dev") {
            bind(argv, "--ro-bind-try", source, source);
            landlock_read.push(source.clone());
            continue;
        }
        let destination = PathBuf::from(format!("{FILES_MOUNT}/read/{index:03}"));
        bind(argv, "--ro-bind-try", source, &destination);
        landlock_read.push(destination);
    }
    for (index, source) in capabilities.rw_paths.iter().enumerate() {
        if is_host_microphone_endpoint(capabilities, source) {
            // `CapabilityPlan`'s native-app projection includes the host
            // PipeWire socket and `/dev/snd` for a microphone grant. A Wine
            // capsule gets neither: both would bypass its short-lived audio
            // proxy and turn an output capability into raw device access.
            continue;
        }
        if source.starts_with("/dev") {
            bind(argv, "--dev-bind-try", source, source);
            landlock_write.push(source.clone());
            continue;
        }
        let destination = PathBuf::from(format!("{FILES_MOUNT}/write/{index:03}"));
        bind(argv, "--bind-try", source, &destination);
        landlock_write.push(destination);
    }
}

fn is_host_microphone_endpoint(capabilities: &CapabilityPlan, source: &Path) -> bool {
    capabilities
        .devices
        .iter()
        .any(|device| device == "microphone")
        && (source == Path::new("/dev/snd")
            || source.file_name().is_some_and(|name| name == "pipewire-0"))
}

fn push_audio(
    argv: &mut Vec<String>,
    lease: &LaunchLease,
    uid: u32,
    landlock_write: &mut Vec<PathBuf>,
) {
    let runtime = format!("/run/user/{uid}");
    for audio in &lease.audio {
        let destination = match audio.protocol {
            AudioProtocol::PipeWire => PathBuf::from(&runtime).join("pipewire-0"),
            AudioProtocol::Pulse => PathBuf::from(&runtime).join("pulse/native"),
        };
        if let Some(parent) = destination.parent() {
            argv.extend(["--dir".to_string(), parent.display().to_string()]);
        }
        bind(argv, "--bind", &audio.host_socket, &destination);
        landlock_write.push(destination.clone());
        match audio.protocol {
            AudioProtocol::PipeWire => setenv(argv, "PIPEWIRE_REMOTE", "pipewire-0"),
            AudioProtocol::Pulse => setenv(
                argv,
                "PULSE_SERVER",
                &format!("unix:{}", destination.display()),
            ),
        }
    }
}

fn push_fixed_environment(
    argv: &mut Vec<String>,
    capsule: &CapsuleState,
    route: &LaunchPlan,
) -> Result<(), PlanError> {
    for (key, value) in [
        ("HOME", HOME_MOUNT),
        ("XDG_DATA_HOME", "/run/qos/capsule/home/.local/share"),
        ("XDG_CONFIG_HOME", "/run/qos/capsule/home/.config"),
        ("XDG_CACHE_HOME", "/run/qos/capsule/home/.cache"),
        ("WINEPREFIX", PREFIX_MOUNT),
        ("STEAM_COMPAT_DATA_PATH", "/run/qos/capsule"),
        (
            "WINEARCH",
            match capsule.settings.architecture {
                Architecture::Win32 => "win32",
                Architecture::Win64 => "win64",
            },
        ),
        (
            "QOS_COMPAT_WINDOWS_VERSION",
            capsule.settings.windows_version.as_str(),
        ),
    ] {
        setenv(argv, key, value);
    }
    if capsule.layer == Layer::Proton {
        let recipe = route
            .proton_recipe
            .as_deref()
            .ok_or(PlanError::MissingProtonRecipe)?;
        validate_setting_value(recipe)?;
        setenv(argv, "GAMEID", recipe);
    }
    let mut projected = BTreeMap::new();
    for (key, value) in &capsule.settings.environment {
        projected.insert(key.as_str(), validate_environment(key, value)?);
    }
    for (key, value) in &capsule.settings.graphics {
        let env_key = match key.as_str() {
            "dxvk_hud" => "DXVK_HUD",
            "dxvk_log_level" => "DXVK_LOG_LEVEL",
            "vkd3d_config" => "VKD3D_CONFIG",
            "esync" => "WINEESYNC",
            "fsync" => "WINEFSYNC",
            _ => return Err(PlanError::ForbiddenGraphicsSetting(key.clone())),
        };
        projected.insert(env_key, validate_environment(env_key, value)?);
    }
    for (key, value) in projected {
        setenv(argv, key, &value);
    }
    Ok(())
}

fn validate_environment(key: &str, value: &str) -> Result<String, PlanError> {
    validate_setting_value(value)?;
    match key {
        "WINEDEBUG" if value.len() <= 256 => Ok(value.to_string()),
        "WINEESYNC" | "WINEFSYNC" | "PROTON_LOG" if matches!(value, "0" | "1") => {
            Ok(value.to_string())
        }
        "DXVK_LOG_LEVEL" if matches!(value, "none" | "error" | "warn" | "info" | "debug") => {
            Ok(value.to_string())
        }
        "DXVK_HUD" | "VKD3D_CONFIG"
            if value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b',' | b'_' | b'-')) =>
        {
            Ok(value.to_string())
        }
        _ => Err(PlanError::ForbiddenEnvironment(key.to_string())),
    }
}

fn validate_setting_value(value: &str) -> Result<(), PlanError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(PlanError::InvalidSettingValue);
    }
    Ok(())
}

fn validate_runner_token(value: &str) -> Result<(), PlanError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PlanError::InvalidRunner);
    }
    Ok(())
}

fn bind(
    argv: &mut Vec<String>,
    flag: &str,
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) {
    argv.extend([
        flag.to_string(),
        source.as_ref().display().to_string(),
        destination.as_ref().display().to_string(),
    ]);
}

fn setenv(argv: &mut Vec<String>, key: &str, value: &str) {
    argv.extend(["--setenv".to_string(), key.to_string(), value.to_string()]);
}

#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("resource broker is unavailable")]
    Unavailable,
    #[error("resource broker refused launch: {0}")]
    Refused(String),
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    #[error("VM route was sent to the capsule planner")]
    VmIsNotCapsule,
    #[error("runner token is invalid")]
    InvalidRunner,
    #[error("runner family does not match selected layer")]
    RunnerLayerMismatch,
    #[error("selected immutable runner is not installed: {0}")]
    RunnerNotInstalled(String),
    #[error("Wayland socket is outside the session runtime directory")]
    UnsafeWaylandSocket,
    #[error("GPU device is not a DRM render node")]
    UnsafeRenderNode,
    #[error("capsule does not match resolved route")]
    CapsuleRouteMismatch,
    #[error("derived capsule path escaped its configured root")]
    UnsafeDerivedPath,
    #[error("resource broker widened access through {0}")]
    ResourceEscalation(&'static str),
    #[error("resource broker returned the same audio protocol twice")]
    DuplicateAudioProtocol,
    #[error("filtered network launch has no qOS egress gate")]
    MissingNetworkGate,
    #[error("offline or unrestricted launch unexpectedly has a network gate")]
    UnexpectedNetworkGate,
    #[error("Proton route has no UMU recipe")]
    MissingProtonRecipe,
    #[error("catalogue environment variable {0:?} is not allow-listed")]
    ForbiddenEnvironment(String),
    #[error("catalogue graphics setting {0:?} is not allow-listed")]
    ForbiddenGraphicsSetting(String),
    #[error("catalogue setting value is empty, oversized, or contains controls")]
    InvalidSettingValue,
}

#[cfg(test)]
mod tests {
    use compat_catalogue::{
        ApplicationQuery, MatchKind, RunnerSettings, Runtime, SecurityProfileRef,
    };
    use tempfile::TempDir;

    use super::*;

    fn route(layer: Layer) -> LaunchPlan {
        let (runner, proton_recipe) = match layer {
            Layer::Wine => (Runner::new("wine", "system").unwrap(), None),
            Layer::Proton => (
                Runner::new("umu-proton", "UMU-Proton-10.0-4").unwrap(),
                Some("umu-123".to_string()),
            ),
            Layer::Vm => (Runner::new("windows-vm", "managed").unwrap(), None),
        };
        LaunchPlan {
            record_id: Some("example".to_string()),
            match_kind: MatchKind::Executable,
            runtime: Runtime {
                id: "example".to_string(),
                layer,
            },
            runner,
            settings: RunnerSettings::default(),
            security_profile: SecurityProfileRef::new("windows-app").unwrap(),
            proton_recipe,
            compatibility_grade: None,
        }
    }

    fn setup(route: &LaunchPlan) -> (TempDir, CapsuleStore, CapsuleState, CapsuleProgram) {
        let directory = TempDir::new().unwrap();
        let store = CapsuleStore::new(directory.path()).unwrap();
        let state = store.ensure_for_plan(route).unwrap();
        let program = CapsuleProgram {
            id: "program".to_string(),
            display_name: "Program".to_string(),
            prefix_relative_executable: PathBuf::from("drive_c/Program Files/App/app.exe"),
            query: ApplicationQuery::executable("app.exe"),
            kind: crate::ProgramKind::Application,
            arguments: Vec::new(),
        };
        let state = store.register_program(&state.id, program.clone()).unwrap();
        (directory, store, state, program)
    }

    fn session() -> HostSession {
        HostSession {
            uid: 1000,
            wayland_socket: PathBuf::from("/run/user/1000/nirios-ws-test"),
            render_node: PathBuf::from("/dev/dri/renderD999"),
        }
    }

    #[test]
    fn immutable_registry_matches_containerfile_artifacts() {
        let registry = SystemRuntimeRegistry::new();
        let wine = registry
            .resolve(Layer::Wine, &Runner::new("wine", "system").unwrap())
            .unwrap();
        assert!(
            wine.host_paths()
                .any(|path| path == Path::new("/usr/bin/wine"))
        );
        assert_eq!(wine.sandbox_entrypoint(), Path::new("/usr/bin/wine"));

        let proton = registry
            .resolve(
                Layer::Proton,
                &Runner::new("umu-proton", "UMU-Proton-10.0-4").unwrap(),
            )
            .unwrap();
        assert!(
            proton
                .host_paths()
                .any(|path| path == Path::new("/usr/libexec/qos-umu-run"))
        );
        assert!(
            proton
                .host_paths()
                .any(|path| path == Path::new("/usr/bin/env"))
        );
        assert!(
            proton
                .host_paths()
                .any(|path| path == Path::new(PROTON_HOST))
        );
        assert_eq!(
            proton.environment,
            vec![("PROTONPATH", PROTON_MOUNT.to_string())]
        );
        assert!(proton.needs_nested_runtime);
    }

    #[test]
    fn uninstalled_runner_version_is_not_turned_into_a_path() {
        let error = SystemRuntimeRegistry::new()
            .resolve(Layer::Wine, &Runner::new("wine", "../../evil").unwrap())
            .unwrap_err();
        assert!(matches!(error, PlanError::InvalidRunner));
        let error = SystemRuntimeRegistry::new()
            .resolve(Layer::Wine, &Runner::new("wine", "11.0").unwrap())
            .unwrap_err();
        assert!(matches!(error, PlanError::RunnerNotInstalled(_)));
    }

    #[test]
    fn catalogue_cannot_inject_environment_or_host_paths() {
        let mut route = route(Layer::Wine);
        route
            .settings
            .environment
            .insert("LD_PRELOAD".to_string(), "/home/user/evil.so".to_string());
        let (_directory, store, state, program) = setup(&route);
        let registry = SystemRuntimeRegistry::new();
        let host = session();
        let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();
        let error = planner
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap_err();
        assert!(matches!(error, PlanError::ForbiddenEnvironment(key) if key == "LD_PRELOAD"));
    }

    #[test]
    fn wine_plan_is_default_deny_and_uses_fixed_artifacts() {
        let route = route(Layer::Wine);
        let (_directory, store, state, program) = setup(&route);
        let registry = SystemRuntimeRegistry::new();
        let host = session();
        let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();
        let command = planner
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap();
        assert!(command.bwrap_argv.iter().any(|arg| arg == "--clearenv"));
        assert!(command.bwrap_argv.iter().any(|arg| arg == "--unshare-all"));
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg == "--net-restricted")
        );
        assert!(command.bwrap_argv.iter().any(|arg| arg == "--seccomp"));
        assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
        assert!(command.bwrap_argv.iter().any(|arg| arg == "/usr/bin/wine"));
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| { arg == "/run/qos/capsule/prefix/drive_c/Program Files/App/app.exe" })
        );
        assert!(
            !command
                .bwrap_argv
                .iter()
                .any(|arg| arg.contains("/home/user"))
        );
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg.starts_with(&format!("/proc/{}/fd/", std::process::id())))
        );
    }

    #[test]
    fn microphone_capsule_uses_brokered_audio_without_raw_sound_device() {
        let route = route(Layer::Wine);
        let (_directory, store, state, program) = setup(&route);
        let registry = SystemRuntimeRegistry::new();
        let host = session();
        let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();
        let capabilities = CapabilityPlan {
            rw_paths: vec![
                PathBuf::from("/run/user/1000/pipewire-0"),
                PathBuf::from("/dev/snd"),
            ],
            devices: vec!["microphone".to_string()],
            ..CapabilityPlan::empty()
        };
        let lease = LaunchLease {
            id: "audio-lease".to_string(),
            audio: vec![
                AudioBinding {
                    protocol: AudioProtocol::PipeWire,
                    host_socket: PathBuf::from(
                        "/run/qos/windows-compat/leases/audio-lease/audio/pipewire-0",
                    ),
                    input_enabled: true,
                },
                AudioBinding {
                    protocol: AudioProtocol::Pulse,
                    host_socket: PathBuf::from(
                        "/run/qos/windows-compat/leases/audio-lease/audio/pulse/native",
                    ),
                    input_enabled: true,
                },
            ],
            net_gate: None,
        };
        let command = planner
            .plan(&route, &state, &program, &capabilities, "workspace", &lease)
            .unwrap();
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg == "/run/qos/windows-compat/leases/audio-lease/audio/pipewire-0")
        );
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg == "/run/qos/windows-compat/leases/audio-lease/audio/pulse/native")
        );
        assert!(!command.bwrap_argv.iter().any(|arg| arg == "/dev/snd"));
        // The sandbox must name its own runtime destination so clients can
        // resolve the private PipeWire endpoint. The host socket occurs only
        // as the source half of the explicit broker bind above, never as a
        // capability-derived bind.
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg == "/run/user/1000/pipewire-0")
        );
    }

    #[test]
    fn planner_rejects_a_symlinked_capsule_data_component() {
        use std::os::unix::fs::symlink;

        let route = route(Layer::Wine);
        let (_directory, store, state, program) = setup(&route);
        let outside = TempDir::new().unwrap();
        let data = store.root().join("capsules").join(&state.id).join("data");
        std::fs::remove_dir_all(&data).unwrap();
        symlink(outside.path(), &data).unwrap();
        let registry = SystemRuntimeRegistry::new();
        let host = session();
        let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();

        assert!(
            planner
                .plan(
                    &route,
                    &state,
                    &program,
                    &CapabilityPlan::empty(),
                    "workspace",
                    &LaunchLease::deny_all("lease"),
                )
                .is_err()
        );
        assert!(
            !outside.path().join("prefix").exists(),
            "planner must not bind a path through an attacker symlink"
        );
    }

    #[test]
    fn proton_plan_uses_umu_and_protonpath_without_blocking_its_nested_runtime() {
        let route = route(Layer::Proton);
        let (_directory, store, state, program) = setup(&route);
        let registry = SystemRuntimeRegistry::new();
        let host = session();
        let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();
        let command = planner
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap();
        assert!(
            command
                .bwrap_argv
                .iter()
                .any(|arg| arg == "/usr/libexec/qos-umu-run")
        );
        assert!(
            command
                .bwrap_argv
                .windows(3)
                .any(|args| { args == ["--setenv", "PROTONPATH", PROTON_MOUNT] })
        );
        assert!(command.seccomp_profile.is_none());
    }
}
