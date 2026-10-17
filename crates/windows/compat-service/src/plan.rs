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

const PREFIX_MOUNT: &str = "/run/tilo/capsule/prefix";
pub(crate) const HOME_MOUNT: &str = "/run/tilo/capsule/home";
const FILES_MOUNT: &str = "/run/tilo/files";
const SHIM_HOST: &str = "/usr/bin/tilo-launcher";
const SHIM_MOUNT: &str = "/tilo-launcher";
const LEASE_ROOT: &str = "/run/tilo/windows-compat/leases";
const PINNED_PROTON: &str = "UMU-Proton-10.0-4";
const PROTON_HOST: &str = "/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4";
const PROTON_MOUNT: &str = "/opt/tilo/proton";
const STEAM_RUNTIME_HOST: &str =
    "/usr/lib/tilo/windows-runners/steam-runtime/SteamLinuxRuntime_sniper";
const UMU_FOLDERS_MOUNT: &str = "/tmp/tilo-runtime";
const STEAM_RUNTIME_MOUNT: &str = "/tmp/tilo-runtime/umu/steamrt3";
const STEAM_PLATFORM: &str = "sniper_platform_3.0.20260805.254768";
const X86_ROOTFS_HOST: &str = "/usr/lib/tilo/windows-runners/x86";
const TRANSLATED_HELPER: &str = "/usr/libexec/tilo-windows-runtime";
const EMULATOR_MANIFEST: &str = "/usr/share/tilo/windows/fex-emulator.json";
const GRAPHICS_MANIFEST: &str = "/usr/share/tilo/windows/fex-graphics-provider.json";
/// Build record of the Windows provider the image assembled (os/Containerfile).
const PROVIDER_MANIFEST: &str = "/usr/share/tilo/windows/provider.json";

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
    runtime_lock_mount: Option<PathBuf>,
    translated: bool,
}

impl RuntimeImage {
    fn seccomp_profile(&self) -> Option<SeccompProfile> {
        if self.needs_nested_runtime {
            None
        } else if self.translated {
            Some(SeccompProfile::TiloJit)
        } else {
            Some(SeccompProfile::TiloDefault)
        }
    }

    fn translate_for_arm(&mut self, layer: Layer) {
        self.translated = true;
        self.sandbox_entrypoint = PathBuf::from(TRANSLATED_HELPER);
        if layer == Layer::Wine {
            self.binds.clear();
            for resource in ["usr/lib64/wine", "usr/lib/wine", "usr/share/wine"] {
                self.binds.push(RuntimeBind {
                    host: Path::new(X86_ROOTFS_HOST).join(resource),
                    sandbox: Path::new("/").join(resource),
                });
            }
        }
        self.binds.extend([
            runtime_bind(TRANSLATED_HELPER, TRANSLATED_HELPER),
            runtime_bind("/usr/bin/FEX", "/usr/bin/FEX"),
            runtime_bind("/usr/bin/FEXServer", "/usr/bin/FEXServer"),
            runtime_bind("/usr/bin/FEXOfflineCompiler", "/usr/bin/FEXOfflineCompiler"),
            runtime_bind("/usr/share/fex-emu", "/usr/share/fex-emu"),
            runtime_bind(X86_ROOTFS_HOST, crate::translated::ROOTFS),
        ]);
        if layer == Layer::Proton {
            self.binds.extend([
                runtime_bind(EMULATOR_MANIFEST, EMULATOR_MANIFEST),
                runtime_bind(GRAPHICS_MANIFEST, GRAPHICS_MANIFEST),
            ]);
            self.environment.extend([
                ("STEAM_COMPAT_EMULATOR", EMULATOR_MANIFEST.to_string()),
                (
                    "STEAM_COMPAT_GRAPHICS_PROVIDER",
                    GRAPHICS_MANIFEST.to_string(),
                ),
                (
                    "STEAM_COMPAT_MACHINE_ARCHITECTURE",
                    "aarch64-linux-gnu".to_string(),
                ),
                (
                    "PRESSURE_VESSEL_GRAPHICS_PROVIDER_aarch64_linux_gnu",
                    "/".to_string(),
                ),
                (
                    "PRESSURE_VESSEL_GRAPHICS_PROVIDER_x86_64_linux_gnu",
                    crate::translated::ROOTFS.to_string(),
                ),
                (
                    "PRESSURE_VESSEL_GRAPHICS_PROVIDER_i386_linux_gnu",
                    crate::translated::ROOTFS.to_string(),
                ),
            ]);
        }
    }

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

/// Paths the Windows compatibility system extension (tilo-windows-compat, see
/// os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json) overlays
/// on `/usr`. The base image carries none of them, so a plan that binds one of
/// them on a device that has not installed Windows support must say so instead
/// of failing inside bubblewrap on a missing source.
const PAYLOAD_PATHS: [&str; 3] = [
    "/usr/lib/tilo/windows-runners",
    "/usr/bin/FEX",
    "/usr/share/fex-emu",
];

#[derive(Clone, Copy, Debug)]
pub struct SystemRuntimeRegistry {
    /// Where translated (ARM) runtimes learn whether the image includes them.
    provider_manifest: Option<&'static str>,
    /// Whether to require the on-demand Windows payload to be merged.
    require_payload: bool,
}

impl SystemRuntimeRegistry {
    pub const fn new() -> Self {
        Self {
            provider_manifest: Some(PROVIDER_MANIFEST),
            require_payload: true,
        }
    }

    /// Planning tests run on hosts that are not Tilo images.
    #[cfg(test)]
    pub(crate) const fn assuming_provider() -> Self {
        Self {
            provider_manifest: None,
            require_payload: false,
        }
    }
}

impl Default for SystemRuntimeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeRegistry for SystemRuntimeRegistry {
    fn resolve(&self, layer: Layer, runner: &Runner) -> Result<RuntimeImage, PlanError> {
        let image = Self::resolve_for_host(layer, runner, std::env::consts::ARCH)?;
        if let (true, Some(manifest)) = (image.translated, self.provider_manifest) {
            require_translated_provider(std::fs::read_to_string(manifest).ok().as_deref())?;
        }
        if self.require_payload {
            require_payload(&image, std::path::Path::exists)?;
        }
        Ok(image)
    }
}

/// The runtime binds the payload only when it is merged. Absent, the person is
/// told what to install; the launch never starts.
fn require_payload(image: &RuntimeImage, exists: impl Fn(&Path) -> bool) -> Result<(), PlanError> {
    let missing = image
        .host_paths()
        .any(|path| PAYLOAD_PATHS.iter().any(|prefix| path.starts_with(prefix)) && !exists(path));
    if missing {
        Err(PlanError::WindowsSupportNotInstalled)
    } else {
        Ok(())
    }
}

/// An ARM image carries the translated provider only when it was built with
/// both pinned payloads. Its build record says which; a missing or unreadable
/// record is treated as absent rather than launching into missing files.
fn require_translated_provider(manifest: Option<&str>) -> Result<(), PlanError> {
    let provider = manifest
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.get("provider")?.as_str().map(str::to_owned));
    match provider.as_deref() {
        Some("fex-traditional-wine") => Ok(()),
        _ => Err(PlanError::WindowsProviderNotIncluded),
    }
}

impl SystemRuntimeRegistry {
    fn resolve_for_host(
        layer: Layer,
        runner: &Runner,
        host: &str,
    ) -> Result<RuntimeImage, PlanError> {
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
        let mut image = match layer {
            Layer::Wine if runner.version == "system" => Ok(RuntimeImage {
                binds: vec![
                    runtime_bind("/usr/bin/wine", "/usr/bin/wine"),
                    runtime_bind("/usr/bin/wineserver", "/usr/bin/wineserver"),
                    runtime_bind("/usr/bin/wineboot", "/usr/bin/wineboot"),
                ],
                sandbox_entrypoint: PathBuf::from("/usr/bin/wine"),
                environment: Vec::new(),
                needs_nested_runtime: false,
                runtime_lock_mount: None,
                translated: false,
            }),
            Layer::Wine => Err(PlanError::RunnerNotInstalled(format!(
                "wine {}",
                runner.version
            ))),
            Layer::Proton if matches!(runner.version.as_str(), PINNED_PROTON | "10.0-4") => {
                let mut image = RuntimeImage {
                    // Exact paths installed by os/Containerfile. UMU owns the
                    // Steam Runtime and protonfix invocation contract.
                    binds: vec![
                        runtime_bind("/usr/libexec/tilo-umu-run", "/usr/libexec/tilo-umu-run"),
                        runtime_bind("/usr/libexec/umu_run.py", "/usr/libexec/umu_run.py"),
                        // The pinned zipapp's shebang is `/usr/bin/env python3`.
                        runtime_bind("/usr/bin/env", "/usr/bin/env"),
                        runtime_bind("/usr/bin/python3", "/usr/bin/python3"),
                        runtime_bind("/bin/sh", "/bin/sh"),
                        // The pinned Steam entry points invoke these native
                        // helpers, including after the ARM emulation handoff.
                        runtime_bind("/usr/bin/readlink", "/usr/bin/readlink"),
                        runtime_bind("/usr/bin/getopt", "/usr/bin/getopt"),
                        // pressure-vessel probes its nested bwrap with `true`.
                        runtime_bind("/usr/bin/true", "/usr/bin/true"),
                        runtime_bind("/usr/bin/bwrap", "/usr/bin/bwrap"),
                        runtime_bind(PROTON_HOST, PROTON_MOUNT),
                    ],
                    sandbox_entrypoint: PathBuf::from("/usr/libexec/tilo-umu-run"),
                    environment: vec![
                        ("PROTONPATH", PROTON_MOUNT.to_string()),
                        ("UMU_FOLDERS_PATH", UMU_FOLDERS_MOUNT.to_string()),
                        ("UMU_RUNTIME_UPDATE", "0".to_string()),
                        ("PRESSURE_VESSEL_COPY_RUNTIME", "1".to_string()),
                        (
                            "PRESSURE_VESSEL_VARIABLE_DIR",
                            "/tmp/tilo-runtime/var".to_string(),
                        ),
                        ("PROTON_USE_NTSYNC", "1".to_string()),
                    ],
                    needs_nested_runtime: true,
                    translated: false,
                    runtime_lock_mount: Some(
                        Path::new(STEAM_RUNTIME_MOUNT)
                            .join(STEAM_PLATFORM)
                            .join("files/.ref"),
                    ),
                };
                // Keep the parent writable for UMU's atomic install marker,
                // but mount every executable and shared runtime tree read-only.
                for entry in [
                    "_v2-entry-point",
                    "run",
                    "run-in-sniper",
                    "VERSIONS.txt",
                    "toolmanifest.vdf",
                    "mtree.txt.gz",
                    "pressure-vessel",
                    "pressure-vessel-arm64",
                    STEAM_PLATFORM,
                ] {
                    image.binds.push(RuntimeBind {
                        host: Path::new(STEAM_RUNTIME_HOST).join(entry),
                        sandbox: Path::new(STEAM_RUNTIME_MOUNT).join(entry),
                    });
                }
                Ok(image)
            }
            Layer::Proton => Err(PlanError::RunnerNotInstalled(format!(
                "{} {}",
                runner.family, runner.version
            ))),
            Layer::Vm => Err(PlanError::VmIsNotCapsule),
        }?;
        match host {
            "x86_64" => {}
            "aarch64" => image.translate_for_arm(layer),
            _ => return Err(PlanError::UnsupportedHostArchitecture(host.to_string())),
        }
        Ok(image)
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
        if !capabilities.allow_network {
            return Self::Offline;
        }
        // The decision an app's grants get, with no manifest to add: `*` is
        // the host network, named hosts a gate, and grants that are not hosts
        // in the one grammar (IP literals) nothing.
        match launcher::netgate::EgressDecision::resolve("off", &[], &capabilities.net_hosts) {
            launcher::netgate::EgressDecision::Blocked => Self::Offline,
            launcher::netgate::EgressDecision::Unrestricted => Self::Unrestricted,
            launcher::netgate::EgressDecision::Filtered(_) => Self::Filtered,
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
                    || gate.sandbox_socket != Path::new("/run/tilo/network/gate.sock")
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent device grants and launch requirements, not states of one machine"
)]
pub struct CapsuleCommandPlan {
    /// Bubblewrap flags and the fixed in-sandbox command. Host paths here are
    /// derived only from daemon configuration, capsule state, or resolved grants.
    pub bwrap_argv: Vec<String>,
    /// UMU needs a nested Bubblewrap for Steam Runtime. Proton retains the
    /// outer bwrap, Landlock and network namespace; Wine additionally gets
    /// the Tilo baseline syscall filter.
    pub seccomp_profile: Option<SeccompProfile>,
    pub capsule_id: String,
    pub runner_label: String,
    pub network: NetworkShape,
    pub read_grants: usize,
    pub write_grants: usize,
    pub microphone: bool,
    pub audio_output: bool,
    /// Tilo-owned game classification, never a package-manifest request.
    pub latency_sensitive: bool,
    pub lease_id: String,
    // Open directory descriptors backing the prefix/home bind sources. Kept
    // private so a plan cannot be copied after its authority-bearing handles.
    launch_paths: LaunchPaths,
    pub(crate) runtime_lock_required: bool,
    pub(crate) identity_uid: u32,
}

impl CapsuleCommandPlan {
    pub(crate) fn directory_fds(&self) -> [std::os::fd::BorrowedFd<'_>; 2] {
        self.launch_paths.directory_fds()
    }
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
            "/run/tilo",
            "/run/tilo/capsule",
            FILES_MOUNT,
            "/run/tilo/files/read",
            "/run/tilo/files/write",
            "/run/tilo/network",
            "/opt/tilo",
            PROTON_MOUNT,
            "/usr/bin",
            "/usr/libexec",
        ] {
            argv.extend(["--dir".to_string(), directory.to_string()]);
        }
        launcher::profile::push_system_profile(&mut argv, network == NetworkShape::Unrestricted);
        for (fd, destination) in [
            (crate::execute::PASSWD_CHILD_FD, "/etc/passwd"),
            (crate::execute::GROUP_CHILD_FD, "/etc/group"),
        ] {
            argv.extend([
                "--ro-bind-data".to_string(),
                fd.to_string(),
                destination.to_string(),
            ]);
        }
        for binding in &runtime.binds {
            bind(&mut argv, "--ro-bind", &binding.host, &binding.sandbox);
        }
        if let Some(lock) = &runtime.runtime_lock_mount {
            argv.extend([
                "--symlink".to_string(),
                "_v2-entry-point".to_string(),
                format!("{STEAM_RUNTIME_MOUNT}/umu"),
                "--bind-data".to_string(),
                crate::execute::RUNTIME_LOCK_CHILD_FD.to_string(),
                lock.display().to_string(),
            ]);
        }
        for (fd, destination) in [
            (crate::execute::PREFIX_CHILD_FD, PREFIX_MOUNT),
            (crate::execute::HOME_CHILD_FD, HOME_MOUNT),
        ] {
            argv.extend([
                "--bind-fd".to_string(),
                fd.to_string(),
                destination.to_string(),
            ]);
        }
        // The private prefix always has C:, but deliberately has no Z: mapping.
        // An inherited host working directory cannot be represented by Wine.
        // Use the existing drive root even before Wine creates system32.
        argv.extend(["--chdir".to_string(), format!("{PREFIX_MOUNT}/drive_c")]);

        let mut landlock_read =
            launcher::profile::landlock_ro_paths(network == NetworkShape::Unrestricted);
        let mut landlock_write = launcher::profile::landlock_rw_device_paths();
        landlock_read.extend(runtime.binds.iter().map(|binding| binding.sandbox.clone()));
        if runtime.translated {
            // FEXServer and its children enumerate their own descriptors.
            // /proc/self resolved by this launcher's shim would name only the
            // shim PID, not its children. This is the private procfs mounted
            // above in --unshare-all's PID namespace, never a host proc bind.
            landlock_read.push(PathBuf::from("/proc"));
        }
        landlock_read.extend([PathBuf::from("/etc/passwd"), PathBuf::from("/etc/group")]);
        // This names the private tmpfs mounted above, never the host's /tmp.
        // Wine and translated runtimes need temporary files and local sockets.
        landlock_write.extend([
            PathBuf::from(PREFIX_MOUNT),
            PathBuf::from(HOME_MOUNT),
            PathBuf::from("/tmp"),
        ]);
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
        if route.runtime.layer == Layer::Proton {
            let ntsync = PathBuf::from("/dev/ntsync");
            bind(&mut argv, "--dev-bind", &ntsync, &ntsync);
            landlock_write.push(ntsync);
        }
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
        if runtime.seccomp_profile().is_some() {
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
        if runtime.translated {
            let mode = match (capsule.layer, capsule.settings.architecture) {
                (Layer::Wine, Architecture::Win32) => "wine32",
                (Layer::Wine, Architecture::Win64) => "wine64",
                (Layer::Proton, _) => "proton",
                (Layer::Vm, _) => return Err(PlanError::VmIsNotCapsule),
            };
            argv.push(mode.to_string());
        }
        argv.push(
            Path::new(PREFIX_MOUNT)
                .join(&program.prefix_relative_executable)
                .display()
                .to_string(),
        );
        argv.extend(program.arguments.iter().cloned());

        Ok(CapsuleCommandPlan {
            bwrap_argv: argv,
            seccomp_profile: runtime.seccomp_profile(),
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
            latency_sensitive: route.runtime.layer == Layer::Proton,
            lease_id: lease.id.clone(),
            launch_paths,
            runtime_lock_required: runtime.runtime_lock_mount.is_some(),
            identity_uid: self.session.uid,
        })
    }
}

fn map_granted_files(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
    landlock_read: &mut Vec<PathBuf>,
    landlock_write: &mut Vec<PathBuf>,
) {
    // (host path, mount point, writable) of every file grant, for the
    // private-store masks below.
    let mut mounted: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
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
        mounted.push((source.clone(), destination.clone(), false));
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
        mounted.push((source.clone(), destination.clone(), true));
        landlock_write.push(destination);
    }
    push_private_store_masks(argv, capabilities, &mounted);
}

/// Hide the Tilo private stores a file grant contains
/// ([`CapabilityPlan::masked_dirs`]) at the grant's mount point here, as the
/// native launcher does at the grant's own path. Every grant is its own
/// mount in a capsule, so inside a writable one each directory on the way to
/// a store is pinned (bound onto itself, which makes it a mount point the app
/// cannot rename) — all pins first, so none re-exposes a store already
/// masked — then each store gets an empty read-only tmpfs.
fn push_private_store_masks(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
    mounted: &[(PathBuf, PathBuf, bool)],
) {
    let mut pins: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut masks: Vec<PathBuf> = Vec::new();
    for store in &capabilities.masked_dirs {
        for (source, destination, writable) in mounted {
            let Ok(rel) = store.strip_prefix(source) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            if *writable {
                let (mut host, mut inside) = (source.clone(), destination.clone());
                let mut parts = rel.components().peekable();
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        break;
                    }
                    host.push(part);
                    inside.push(part);
                    if !pins.iter().any(|(_, at)| at == &inside) {
                        pins.push((host.clone(), inside.clone()));
                    }
                }
            }
            let at = destination.join(rel);
            if !masks.contains(&at) {
                masks.push(at);
            }
        }
    }
    pins.sort_by_key(|(_, at)| at.components().count());
    for (host, at) in &pins {
        bind(argv, "--bind", host, at);
    }
    for at in &masks {
        argv.extend([
            "--tmpfs".to_string(),
            at.display().to_string(),
            "--remount-ro".to_string(),
            at.display().to_string(),
        ]);
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
        ("USER", "tilo"),
        ("LOGNAME", "tilo"),
        ("XDG_DATA_HOME", "/run/tilo/capsule/home/.local/share"),
        ("XDG_CONFIG_HOME", "/run/tilo/capsule/home/.config"),
        ("XDG_CACHE_HOME", "/run/tilo/capsule/home/.cache"),
        ("WINEPREFIX", PREFIX_MOUNT),
        ("STEAM_COMPAT_DATA_PATH", "/run/tilo/capsule"),
        (
            "WINEARCH",
            match capsule.settings.architecture {
                Architecture::Win32 => "win32",
                Architecture::Win64 => "win64",
            },
        ),
        (
            "TILO_COMPAT_WINDOWS_VERSION",
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
    #[error("Windows compatibility is not packaged for host architecture {0}")]
    UnsupportedHostArchitecture(String),
    #[error(
        "Windows apps are not available on this Tilo image: it was built without the x86 \
         Windows provider for ARM. The Windows VM remains available where installed."
    )]
    WindowsProviderNotIncluded,
    #[error(
        "Windows support is not installed on this device. It is a separate download of about \
         3 GB that is only fetched when you ask for it. Install it with \
         `systemctl start tilo-extension@install:windows-compat.service` (as an administrator), \
         then launch again. Nothing was started."
    )]
    WindowsSupportNotInstalled,
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
    #[error("filtered network launch has no Tilo egress gate")]
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
    const TEST_REGISTRY: SystemRuntimeRegistry = SystemRuntimeRegistry::assuming_provider();
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
            wayland_socket: PathBuf::from("/run/user/1000/tilo-ws-test"),
            render_node: PathBuf::from("/dev/dri/renderD999"),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn runtime_selection_follows_host_architecture_and_shipped_artifacts() {
        // Phase `immutable_registry_matches_containerfile_artifacts`.
        {
            let wine = SystemRuntimeRegistry::resolve_for_host(
                Layer::Wine,
                &Runner::new("wine", "system").unwrap(),
                "x86_64",
            )
            .unwrap();
            assert!(
                wine.host_paths()
                    .any(|path| path == Path::new("/usr/bin/wine"))
            );
            assert_eq!(wine.sandbox_entrypoint(), Path::new("/usr/bin/wine"));

            let proton = SystemRuntimeRegistry::resolve_for_host(
                Layer::Proton,
                &Runner::new("umu-proton", "UMU-Proton-10.0-4").unwrap(),
                "x86_64",
            )
            .unwrap();
            assert!(
                proton
                    .host_paths()
                    .any(|path| path == Path::new("/usr/libexec/tilo-umu-run"))
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
            assert!(
                proton
                    .environment
                    .contains(&("PROTONPATH", PROTON_MOUNT.to_string()))
            );
            assert!(
                proton
                    .environment
                    .contains(&("UMU_RUNTIME_UPDATE", "0".to_string()))
            );
            assert!(
                proton
                    .environment
                    .contains(&("PROTON_USE_NTSYNC", "1".to_string()))
            );
            assert!(proton.needs_nested_runtime);
        }
        // Phase `host_architecture_selects_runtime_and_syscall_policy`.
        {
            for host in ["x86_64", "aarch64"] {
                for layer in [Layer::Wine, Layer::Proton] {
                    for architecture in [Architecture::Win32, Architecture::Win64] {
                        let mut route = route(layer);
                        route.settings.architecture = architecture;
                        let (_directory, store, state, program) = setup(&route);
                        let session = session();
                        let registry = TestHostRegistry(host);
                        let planner = SandboxPlanner::new(&store, &registry, &session).unwrap();
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
                        let expected_filter = match (host, layer) {
                            (_, Layer::Proton) => None,
                            ("aarch64", _) => Some(SeccompProfile::TiloJit),
                            _ => Some(SeccompProfile::TiloDefault),
                        };
                        assert_eq!(command.seccomp_profile, expected_filter);
                        assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
                        assert!(
                            command
                                .bwrap_argv
                                .iter()
                                .any(|arg| arg == "--net-restricted")
                        );
                        if host == "aarch64" {
                            let mode = match (layer, architecture) {
                                (Layer::Proton, _) => "proton",
                                (_, Architecture::Win32) => "wine32",
                                (_, Architecture::Win64) => "wine64",
                            };
                            assert!(
                                command
                                    .bwrap_argv
                                    .windows(2)
                                    .any(|args| args == [TRANSLATED_HELPER, mode])
                            );
                            assert!(command.bwrap_argv.windows(3).any(|args| {
                                args == ["--ro-bind", X86_ROOTFS_HOST, crate::translated::ROOTFS]
                            }));
                            if layer == Layer::Wine {
                                assert!(
                                    !command.bwrap_argv.iter().any(|arg| arg == "/usr/bin/wine")
                                );
                                for resource in ["usr/lib/wine", "usr/lib64/wine"] {
                                    let source = format!("{X86_ROOTFS_HOST}/{resource}");
                                    let destination = format!("/{resource}");
                                    assert!(command.bwrap_argv.windows(3).any(|args| {
                                        args == ["--ro-bind", &source, &destination]
                                    }));
                                }
                                assert!(
                                    !command
                                        .bwrap_argv
                                        .iter()
                                        .any(|arg| arg.contains("wine-wow64"))
                                );
                            }
                        } else {
                            assert!(
                                !command
                                    .bwrap_argv
                                    .iter()
                                    .any(|arg| arg == TRANSLATED_HELPER)
                            );
                        }
                    }
                }
            }
        }
        // Phase `only_translated_runtimes_read_their_private_procfs`.
        {
            for host in ["x86_64", "aarch64"] {
                for layer in [Layer::Wine, Layer::Proton] {
                    let route = route(layer);
                    let (_directory, store, state, program) = setup(&route);
                    let session = session();
                    let registry = TestHostRegistry(host);
                    let command = SandboxPlanner::new(&store, &registry, &session)
                        .unwrap()
                        .plan(
                            &route,
                            &state,
                            &program,
                            &CapabilityPlan::empty(),
                            "workspace",
                            &LaunchLease::deny_all("lease"),
                        )
                        .unwrap();
                    let shim_position = command
                        .bwrap_argv
                        .iter()
                        .position(|argument| {
                            argument == launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND
                        })
                        .unwrap();
                    let shim = launcher::landlock_shim::ShimArgs::parse(
                        &command.bwrap_argv[shim_position + 1..],
                    )
                    .unwrap();
                    assert_eq!(shim.ro.contains(&PathBuf::from("/proc")), host == "aarch64");
                    assert!(!shim.rw.iter().any(|path| path.starts_with("/proc")));
                    assert!(!shim.no_landlock);
                    let namespace = &command.bwrap_argv[..shim_position];
                    assert!(namespace.iter().any(|argument| argument == "--unshare-all"));
                    assert!(
                        namespace
                            .windows(2)
                            .any(|arguments| arguments == ["--proc", "/proc"])
                    );
                    assert!(!namespace.iter().any(|argument| argument == "--share-pid"));
                    assert!(
                        !namespace
                            .windows(3)
                            .any(|arguments| arguments[0].contains("bind")
                                && (Path::new(&arguments[1]).starts_with("/proc")
                                    || Path::new(&arguments[2]).starts_with("/proc")))
                    );
                }
            }
        }
        // Phase `arm_capsules_require_the_assembled_translated_provider`.
        {
            assert!(
                require_translated_provider(Some(r#"{"provider":"fex-traditional-wine"}"#)).is_ok()
            );
            for manifest in [
                Some(r#"{"schema_version":1,"host_architecture":"aarch64","provider":"none"}"#),
                Some(r#"{"provider":"native-wine"}"#),
                Some("not json"),
                None,
            ] {
                assert!(matches!(
                    require_translated_provider(manifest),
                    Err(PlanError::WindowsProviderNotIncluded)
                ));
            }
        }
        // Phase `unknown_host_architecture_fails_closed`.
        {
            let error = SystemRuntimeRegistry::resolve_for_host(
                Layer::Wine,
                &Runner::new("wine", "system").unwrap(),
                "riscv64",
            )
            .unwrap_err();
            assert!(
                matches!(error, PlanError::UnsupportedHostArchitecture(host) if host == "riscv64")
            );
        }
        // Phase `shipped_fex_manifests_match_the_fixed_mount_contract`.
        {
            let emulator: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../os/usr/share/tilo/windows/fex-emulator.json"
            ))
            .unwrap();
            let graphics: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../os/usr/share/tilo/windows/fex-graphics-provider.json"
            ))
            .unwrap();
            assert_eq!(
                emulator["emulator_v0"]["environment"]["FEX_ROOTFS"],
                crate::translated::ROOTFS
            );
            assert_eq!(
                emulator["emulator_v0"]["container_environment"]["FEX_ROOTFS"],
                ""
            );
            assert_eq!(
                emulator["emulator_v0"]["argv"],
                serde_json::json!(["/usr/bin/FEX"])
            );
            assert_eq!(
                graphics["graphics_provider_v0"]["root"],
                crate::translated::ROOTFS
            );
        }
    }

    struct TestHostRegistry(&'static str);

    impl RuntimeRegistry for TestHostRegistry {
        fn resolve(&self, layer: Layer, runner: &Runner) -> Result<RuntimeImage, PlanError> {
            SystemRuntimeRegistry::resolve_for_host(layer, runner, self.0)
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn wine_plans_are_default_deny_with_fixed_artifacts_and_no_injection() {
        // Phase `uninstalled_runner_version_is_not_turned_into_a_path`.
        {
            let error = SystemRuntimeRegistry::new()
                .resolve(Layer::Wine, &Runner::new("wine", "../../evil").unwrap())
                .unwrap_err();
            assert!(matches!(error, PlanError::InvalidRunner));
            let error = SystemRuntimeRegistry::new()
                .resolve(Layer::Wine, &Runner::new("wine", "11.0").unwrap())
                .unwrap_err();
            assert!(matches!(error, PlanError::RunnerNotInstalled(_)));
        }
        // Phase `catalogue_cannot_inject_environment_or_host_paths`.
        {
            let mut route = route(Layer::Wine);
            route
                .settings
                .environment
                .insert("LD_PRELOAD".to_string(), "/home/user/evil.so".to_string());
            let (_directory, store, state, program) = setup(&route);
            let registry = SystemRuntimeRegistry::assuming_provider();
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
        // Phase `wine_plan_is_default_deny_and_uses_fixed_artifacts`.
        {
            let route = route(Layer::Wine);
            let (_directory, store, state, program) = setup(&route);
            let registry = SystemRuntimeRegistry::assuming_provider();
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
            assert!(
                command
                    .bwrap_argv
                    .windows(2)
                    .any(|args| { args == ["--chdir", &format!("{PREFIX_MOUNT}/drive_c")] })
            );
            assert_eq!(command.identity_uid, host.uid);
            for (fd, path) in [
                (crate::execute::PASSWD_CHILD_FD, "/etc/passwd"),
                (crate::execute::GROUP_CHILD_FD, "/etc/group"),
            ] {
                assert!(
                    command
                        .bwrap_argv
                        .windows(3)
                        .any(|args| args == ["--ro-bind-data", fd.to_string().as_str(), path])
                );
                assert!(
                    command
                        .bwrap_argv
                        .windows(2)
                        .any(|args| args == ["--ro", path])
                );
                assert!(
                    !command
                        .bwrap_argv
                        .windows(3)
                        .any(|args| matches!(args[0].as_str(), "--bind" | "--ro-bind")
                            && args[1] == path)
                );
            }
            for key in ["USER", "LOGNAME"] {
                assert!(
                    command
                        .bwrap_argv
                        .windows(3)
                        .any(|args| args == ["--setenv", key, "tilo"])
                );
            }
            assert!(command.bwrap_argv.iter().any(|arg| arg == "--unshare-all"));
            assert!(
                command
                    .bwrap_argv
                    .iter()
                    .any(|arg| arg == "--net-restricted")
            );
            assert!(!command.latency_sensitive);
            assert!(command.bwrap_argv.iter().any(|arg| arg == "--seccomp"));
            assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
            let expected_entry = if std::env::consts::ARCH == "aarch64" {
                TRANSLATED_HELPER
            } else {
                "/usr/bin/wine"
            };
            assert!(command.bwrap_argv.iter().any(|arg| arg == expected_entry));
            assert!(command.bwrap_argv.iter().any(|arg| {
                arg == "/run/tilo/capsule/prefix/drive_c/Program Files/App/app.exe"
            }));
            assert!(
                !command
                    .bwrap_argv
                    .iter()
                    .any(|arg| arg.contains("/home/user"))
            );
            for (fd, destination) in [
                (crate::execute::PREFIX_CHILD_FD, PREFIX_MOUNT),
                (crate::execute::HOME_CHILD_FD, HOME_MOUNT),
            ] {
                assert!(
                    command.bwrap_argv.windows(3).any(|args| {
                        args == ["--bind-fd", fd.to_string().as_str(), destination]
                    })
                );
            }
        }
    }

    #[test]
    #[ignore = "requires bubblewrap and unprivileged user namespaces"]
    fn executor_retains_directories_after_path_replacement() {
        use crate::SandboxExecutor;

        let route = route(Layer::Wine);
        let (directory, store, state, program) = setup(&route);
        let session = session();
        let planner = SandboxPlanner::new(&store, &TEST_REGISTRY, &session).unwrap();
        let mut command = planner
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap();
        for (path, expected) in [
            (store.prefix_path(&state.id), "original-prefix"),
            (store.home_path(&state.id), "original-home"),
        ] {
            std::fs::write(path.join("value"), expected).unwrap();
            std::fs::rename(&path, path.with_extension("retained")).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("value"), "replacement").unwrap();
        }
        // Exercise the real executor and descriptor transfer without requiring
        // Wine, a GPU, or the production Landlock kernel for this mount test.
        command.seccomp_profile = None;
        command.bwrap_argv = [
            "--unshare-all", "--die-with-parent", "--ro-bind", "/usr", "/usr",
            "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib",
            "--symlink", "usr/lib64", "/lib64", "--proc", "/proc", "--dev", "/dev",
            "--bind-fd", "33", "/prefix", "--bind-fd", "34", "/home",
            "/bin/sh", "-ec",
            "test \"$(cat /prefix/value)\" = original-prefix; test \"$(cat /home/value)\" = original-home",
        ].map(str::to_owned).to_vec();
        let immutable_lock = directory.path().join("runtime.ref");
        std::fs::write(&immutable_lock, "immutable").unwrap();
        command.runtime_lock_required = true;
        // This fixture runs as the invoking user, independently of the fixed
        // session UID used by planner-only unit tests.
        command.identity_uid = rustix::process::geteuid().as_raw();
        command.bwrap_argv.splice(
            0..0,
            [
                "--dir".to_string(),
                "/runtime".to_string(),
                "--ro-bind".to_string(),
                immutable_lock.display().to_string(),
                "/runtime/.ref".to_string(),
                "--ro-bind-data".to_string(),
                crate::execute::PASSWD_CHILD_FD.to_string(),
                "/etc/passwd".to_string(),
                "--ro-bind-data".to_string(),
                crate::execute::GROUP_CHILD_FD.to_string(),
                "/etc/group".to_string(),
                "--bind-data".to_string(),
                crate::execute::RUNTIME_LOCK_CHILD_FD.to_string(),
                "/runtime/.ref".to_string(),
            ],
        );
        command
            .bwrap_argv
            .last_mut()
            .unwrap()
            .push_str("; test ! -s /runtime/.ref; printf private > /runtime/.ref");
        command.bwrap_argv.last_mut().unwrap().push_str(
            "; test \"$(getent passwd $(id -u))\" = \"tilo:x:$(id -u):$(id -g):Tilo capsule:/run/tilo/capsule/home:/usr/bin/nologin\"; test \"$(getent group $(id -g))\" = \"tilo:x:$(id -g):\"; test \"$(wc -l < /etc/passwd)\" = 1; test \"$(wc -l < /etc/group)\" = 1; if (printf bad >> /etc/passwd) 2>/dev/null; then exit 1; fi; if (printf bad >> /etc/group) 2>/dev/null; then exit 1; fi; test ! -e /etc/shadow",
        );
        for profile in [None, Some(SeccompProfile::TiloDefault)] {
            command.seccomp_profile = profile;
            if profile.is_some() {
                command.bwrap_argv.splice(
                    0..0,
                    ["--seccomp".to_string(), crate::SECCOMP_CHILD_FD.to_string()],
                );
            }
            let outcome = crate::BwrapExecutor.execute(&command).unwrap();
            assert!(outcome.success, "{profile:?}: {outcome:?}");
            assert_eq!(
                std::fs::read_to_string(&immutable_lock).unwrap(),
                "immutable"
            );
        }
    }

    // Run these explicitly in a disposable native ARM container, with the
    // production artifacts mounted at their fixed host paths. Provide a real
    // architecture-matching Wine/Proton cmd.exe via TILO_ARM_CAPSULE_CMD_EXE,
    // writable /run/user/<uid>, and a dummy /dev/dri/renderD128. A Unix listener
    // below supplies the mandatory Wayland bind; it is closed before execution
    // so clients fail promptly instead of waiting for a fake display server.
    // Neither stub tests graphics.
    // Use an external 240-second timeout and tear down the container on failure:
    // the production executor waits synchronously for its owned process.
    // These tests deliberately omit the Landlock executable shim because WSL's
    // ABI 3 cannot satisfy Tilo's required ABI 5. They prove namespace execution,
    // not Landlock or full production sandbox acceptance. No production policy
    // is changed, and every generated mount, fd, environment and seccomp option
    // remains intact. The generated unshare-all retains an offline namespace.
    #[test]
    #[ignore = "requires disposable native ARM runtime container; namespace-only acceptance"]
    fn generated_arm_wine32_namespace_executes_windows_console() {
        generated_arm_namespace_console(Layer::Wine, Architecture::Win32, false);
    }

    #[test]
    #[ignore = "requires disposable native ARM runtime container; namespace-only acceptance"]
    fn generated_arm_wine64_namespace_executes_windows_console() {
        generated_arm_namespace_console(Layer::Wine, Architecture::Win64, false);
    }

    #[test]
    #[ignore = "requires disposable native ARM runtime container; namespace-only acceptance"]
    fn generated_arm_proton_namespace_executes_windows_console() {
        generated_arm_namespace_console(Layer::Proton, Architecture::Win64, false);
    }

    #[test]
    #[ignore = "requires disposable native ARM VM with Landlock ABI >= 5 and runtime artifacts"]
    fn generated_arm_wine32_landlock_executes_windows_console() {
        generated_arm_namespace_console(Layer::Wine, Architecture::Win32, true);
    }

    #[test]
    #[ignore = "requires disposable native ARM VM with Landlock ABI >= 5 and runtime artifacts"]
    fn generated_arm_wine64_landlock_executes_windows_console() {
        generated_arm_namespace_console(Layer::Wine, Architecture::Win64, true);
    }

    #[test]
    #[ignore = "requires disposable native ARM VM with Landlock ABI >= 5 and runtime artifacts"]
    fn generated_arm_proton_landlock_executes_windows_console() {
        generated_arm_namespace_console(Layer::Proton, Architecture::Win64, true);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep the namespace fixture and its exact production-plan adjustment auditable together"
    )]
    fn generated_arm_namespace_console(layer: Layer, architecture: Architecture, landlock: bool) {
        use crate::SandboxExecutor;
        use std::os::unix::net::UnixListener;

        assert_eq!(std::env::consts::ARCH, "aarch64");
        if landlock {
            // SAFETY: the version query takes a null attributes pointer and size zero.
            let abi = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    std::ptr::null::<u8>(),
                    0,
                    1,
                )
            };
            assert!(
                abi >= 5,
                "full capsule acceptance requires Landlock ABI >= 5: {abi}"
            );
            println!("TILO_ARM_CAPSULE_LANDLOCK_ABI={abi}");
        }
        let mut route = route(layer);
        route.settings.architecture = architecture;
        route.settings.environment.insert(
            "WINEDEBUG".into(),
            std::env::var("TILO_ARM_CAPSULE_WINEDEBUG").unwrap_or_else(|_| "-all".into()),
        );
        if layer == Layer::Proton {
            route.proton_recipe = Some("umu-0".into());
        }
        let (_directory, store, state, mut program) = setup(&route);
        let marker = "TILO_GENERATED_ARM_CAPSULE_PASS";
        program.prefix_relative_executable = PathBuf::from("drive_c/tilo-console.exe");
        program.arguments = vec![
            "/d".into(),
            "/c".into(),
            format!("echo {marker}>C:\\tilo-namespace-marker.txt"),
        ];
        let prefix = store.prefix_path(&state.id);
        let executable = prefix.join(&program.prefix_relative_executable);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        copy_arm_console_fixture(&executable, architecture);
        let uid = rustix::process::geteuid().as_raw();
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        std::fs::create_dir_all(&runtime).unwrap();
        let socket = runtime.join(format!("tilo-console-{}", std::process::id()));
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);
        let host = HostSession {
            uid,
            wayland_socket: socket.clone(),
            render_node: PathBuf::from("/dev/dri/renderD128"),
        };
        assert!(
            host.render_node.exists(),
            "provide a dummy render-node bind source"
        );
        let planner = SandboxPlanner::new(&store, &TEST_REGISTRY, &host).unwrap();
        let mut command = planner
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "namespace-test",
                &LaunchLease::deny_all("namespace-test"),
            )
            .unwrap();
        assert_eq!(command.network, NetworkShape::Offline);
        assert!(command.bwrap_argv.iter().any(|arg| arg == "--unshare-all"));
        assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
        assert_eq!(
            command.seccomp_profile,
            if layer == Layer::Wine {
                Some(SeccompProfile::TiloJit)
            } else {
                None
            }
        );
        // Namespace-only cases remove exactly the executable shim and its
        // arguments. Full cases preserve the entire production command.
        let shim = command
            .bwrap_argv
            .windows(2)
            .position(|args| args == [SHIM_MOUNT, launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND])
            .unwrap();
        let separator = command.bwrap_argv[shim..]
            .iter()
            .position(|arg| arg == "--")
            .unwrap()
            .checked_add(shim)
            .unwrap();
        if landlock {
            assert!(!command.bwrap_argv.iter().any(|arg| arg == "--no-landlock"));
            assert!(
                command.bwrap_argv[shim..separator]
                    .iter()
                    .any(|arg| arg == "--net-restricted")
            );
        } else {
            command.bwrap_argv.drain(shim..=separator);
        }
        let outcome = crate::BwrapExecutor.execute(&command);
        std::fs::remove_file(socket).unwrap();
        assert!(
            outcome.as_ref().is_ok_and(|result| result.success),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(prefix.join("drive_c/tilo-namespace-marker.txt"))
                .expect("Windows must create the marker in the retained capsule prefix")
                .trim(),
            marker,
        );
    }

    fn copy_arm_console_fixture(destination: &Path, architecture: Architecture) {
        let source = std::env::var_os("TILO_ARM_CAPSULE_CMD_EXE")
            .expect("set TILO_ARM_CAPSULE_CMD_EXE to the matching runtime's cmd.exe");
        let bytes = std::fs::read(source).unwrap();
        assert_eq!(
            &bytes[..2],
            b"MZ",
            "fixture must be a Windows PE executable"
        );
        let pe = usize::try_from(u32::from_le_bytes(bytes[60..64].try_into().unwrap())).unwrap();
        let signature_end = pe.checked_add(4).unwrap();
        let machine_end = pe.checked_add(6).unwrap();
        assert_eq!(&bytes[pe..signature_end], b"PE\0\0");
        let machine = u16::from_le_bytes(bytes[signature_end..machine_end].try_into().unwrap());
        assert_eq!(
            machine,
            match architecture {
                Architecture::Win32 => 0x014c,
                Architecture::Win64 => 0x8664,
            },
            "fixture architecture must match the selected capsule"
        );
        std::fs::write(destination, bytes).unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn capsules_provision_private_storage_brokered_audio_and_proton_runtimes() {
        // Phase `proton_provisions_immutable_runtime_with_private_lock_and_metadata`.
        {
            let route = route(Layer::Proton);
            let (_directory, store, state, program) = setup(&route);
            let host = session();
            let planner = SandboxPlanner::new(&store, &TEST_REGISTRY, &host).unwrap();
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
            assert!(command.runtime_lock_required);
            let platform = format!("{STEAM_RUNTIME_MOUNT}/{STEAM_PLATFORM}");
            let lock = format!("{platform}/files/.ref");
            let platform_bind = command
                .bwrap_argv
                .windows(3)
                .position(|args| args[0] == "--ro-bind" && args[2] == platform)
                .unwrap();
            let lock_bind = command
                .bwrap_argv
                .windows(3)
                .position(|args| {
                    args == [
                        "--bind-data",
                        crate::execute::RUNTIME_LOCK_CHILD_FD.to_string().as_str(),
                        &lock,
                    ]
                })
                .unwrap();
            assert!(lock_bind > platform_bind);
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--setenv", "UMU_FOLDERS_PATH", UMU_FOLDERS_MOUNT] })
            );
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--setenv", "UMU_RUNTIME_UPDATE", "0"] })
            );
            assert!(
                !command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args[0] == "--bind" && args[1].starts_with(STEAM_RUNTIME_HOST) })
            );
            assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
        }
        // Phase `capsule_temporary_storage_is_private_and_writable`.
        {
            for layer in [Layer::Wine, Layer::Proton] {
                let route = route(layer);
                let (_directory, store, state, program) = setup(&route);
                let registry = SystemRuntimeRegistry::assuming_provider();
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
                let shim_position = command
                    .bwrap_argv
                    .iter()
                    .position(|arg| arg == launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
                    .unwrap();
                let shim = launcher::landlock_shim::ShimArgs::parse(
                    &command.bwrap_argv[shim_position + 1..],
                )
                .unwrap();
                assert!(shim.rw.contains(&PathBuf::from("/tmp")));
                assert!(!shim.no_landlock);
                assert!(shim.net_restricted);
                let mounts = &command.bwrap_argv[..shim_position];
                assert!(mounts.windows(2).any(|args| args == ["--tmpfs", "/tmp"]));
                assert!(!mounts.windows(3).any(|args| {
                    args[0].contains("bind") && (args[1] == "/tmp" || args[2] == "/tmp")
                }));
            }
        }
        // Phase `microphone_capsule_uses_brokered_audio_without_raw_sound_device`.
        {
            let route = route(Layer::Wine);
            let (_directory, store, state, program) = setup(&route);
            let registry = SystemRuntimeRegistry::assuming_provider();
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
                            "/run/tilo/windows-compat/leases/audio-lease/audio/pipewire-0",
                        ),
                        input_enabled: true,
                    },
                    AudioBinding {
                        protocol: AudioProtocol::Pulse,
                        host_socket: PathBuf::from(
                            "/run/tilo/windows-compat/leases/audio-lease/audio/pulse/native",
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
                command.bwrap_argv.iter().any(
                    |arg| arg == "/run/tilo/windows-compat/leases/audio-lease/audio/pipewire-0"
                )
            );
            assert!(
                command
                    .bwrap_argv
                    .iter()
                    .any(|arg| arg
                        == "/run/tilo/windows-compat/leases/audio-lease/audio/pulse/native")
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
        // Phase `private_stores_inside_a_file_grant_are_masked_at_its_mount`.
        {
            let route = route(Layer::Wine);
            let (_directory, store, state, program) = setup(&route);
            let registry = SystemRuntimeRegistry::assuming_provider();
            let host = session();
            let planner = SandboxPlanner::new(&store, &registry, &host).unwrap();
            let capabilities = CapabilityPlan {
                rw_paths: vec![PathBuf::from("/home/alice")],
                masked_dirs: vec![PathBuf::from("/home/alice/.config/tilo")],
                pinned_dirs: vec![PathBuf::from("/home/alice/.config")],
                ..CapabilityPlan::empty()
            };
            let command = planner
                .plan(
                    &route,
                    &state,
                    &program,
                    &capabilities,
                    "workspace",
                    &LaunchLease::deny_all("lease"),
                )
                .unwrap();
            let argv = &command.bwrap_argv;
            let position = |pattern: &[&str]| {
                argv.windows(pattern.len())
                    .position(|w| w.iter().zip(pattern).all(|(a, b)| a == b))
            };
            let grant = position(&["--bind-try", "/home/alice", "/run/tilo/files/write/000"]);
            let pin = position(&[
                "--bind",
                "/home/alice/.config",
                "/run/tilo/files/write/000/.config",
            ]);
            let mask = position(&[
                "--tmpfs",
                "/run/tilo/files/write/000/.config/tilo",
                "--remount-ro",
                "/run/tilo/files/write/000/.config/tilo",
            ]);
            assert!(
                matches!((grant, pin, mask), (Some(g), Some(p), Some(m)) if g < p && p < m),
                "the store must be pinned and masked after the grant's mount: {argv:?}"
            );
        }
        // Phase `planner_rejects_a_symlinked_capsule_data_component`.
        {
            use std::os::unix::fs::symlink;

            let route = route(Layer::Wine);
            let (_directory, store, state, program) = setup(&route);
            let outside = TempDir::new().unwrap();
            let data = store.root().join("capsules").join(&state.id).join("data");
            std::fs::remove_dir_all(&data).unwrap();
            symlink(outside.path(), &data).unwrap();
            let registry = SystemRuntimeRegistry::assuming_provider();
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
        // Phase `proton_plan_uses_umu_and_protonpath_without_blocking_its_nested_runtime`.
        {
            let route = route(Layer::Proton);
            let (_directory, store, state, program) = setup(&route);
            let registry = SystemRuntimeRegistry::assuming_provider();
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
                    .any(|arg| arg == "/usr/libexec/tilo-umu-run")
            );
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--setenv", "PROTONPATH", PROTON_MOUNT] })
            );
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--setenv", "PROTON_USE_NTSYNC", "1"] })
            );
            assert!(command.seccomp_profile.is_none());
            assert!(command.latency_sensitive);
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--dev-bind", "/dev/ntsync", "/dev/ntsync"] })
            );
        }
    }
}

#[cfg(test)]
mod payload_tests {
    use super::*;

    fn proton(host: &str) -> RuntimeImage {
        SystemRuntimeRegistry::resolve_for_host(
            Layer::Proton,
            &Runner::new("umu-proton", PINNED_PROTON).unwrap(),
            host,
        )
        .unwrap()
    }

    #[test]
    fn a_device_without_windows_support_gets_an_install_instruction_not_a_failed_bind() {
        for host in ["x86_64", "aarch64"] {
            let error = require_payload(&proton(host), |_| false).unwrap_err();
            assert!(
                matches!(error, PlanError::WindowsSupportNotInstalled),
                "{host}"
            );
            let message = error.to_string();
            assert!(message.contains("not installed"), "{message}");
            assert!(
                message.contains("tilo-extension@install:windows-compat.service"),
                "the message names the action: {message}"
            );
            assert!(message.contains("Nothing was started"), "{message}");
        }
    }

    #[test]
    fn a_merged_payload_plans_normally() {
        assert!(require_payload(&proton("x86_64"), |_| true).is_ok());
        assert!(require_payload(&proton("aarch64"), |_| true).is_ok());
    }

    #[test]
    fn only_payload_paths_are_probed_not_the_base_the_image_always_has() {
        let probed = std::cell::RefCell::new(Vec::new());
        let _ = require_payload(&proton("aarch64"), |path| {
            probed.borrow_mut().push(path.to_path_buf());
            true
        });
        let probed = probed.into_inner();
        assert!(!probed.is_empty());
        assert!(
            probed
                .iter()
                .all(|path| { PAYLOAD_PATHS.iter().any(|prefix| path.starts_with(prefix)) })
        );
        assert!(probed.contains(&PathBuf::from(PROTON_HOST)));
        assert!(!probed.contains(&PathBuf::from("/usr/bin/bwrap")));
    }

    #[test]
    fn production_registry_requires_the_payload_and_test_registry_does_not() {
        assert!(SystemRuntimeRegistry::new().require_payload);
        assert!(!SystemRuntimeRegistry::assuming_provider().require_payload);
    }

    #[test]
    fn the_payload_paths_are_exactly_what_the_sysext_stage_packs() {
        let containerfile = include_str!("../../../../os/Containerfile");
        for packed in [
            "/tree/usr/lib/tilo/windows-runners/proton/",
            "/tree/usr/lib/tilo/windows-runners/steam-runtime/",
            "/payload/usr/lib/tilo/windows-runners",
        ] {
            assert!(containerfile.contains(packed), "{packed}");
        }
        for host in [PROTON_HOST, STEAM_RUNTIME_HOST, X86_ROOTFS_HOST] {
            assert!(
                PAYLOAD_PATHS.iter().any(|prefix| host.starts_with(prefix)),
                "{host}"
            );
        }
    }
}
