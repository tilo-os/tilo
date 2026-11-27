use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use compat_catalogue::{Architecture, LaunchPlan, Layer, Runner};
use launcher::capabilities::CapabilityPlan;
use launcher::gpu::{IcdSearch, select_for_node};
use launcher::sandbox::EgressProxyBind;
use launcher::seccomp::{SeccompProfile, SyscallGrants};
use thiserror::Error;

use crate::store::{
    CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore, LaunchPaths, validate_id,
};

const PREFIX_MOUNT: &str = "/run/tilo/capsule/prefix";
pub(crate) const HOME_MOUNT: &str = "/run/tilo/capsule/home";
const FILES_MOUNT: &str = "/run/tilo/files";
pub(crate) const TILO_LAUNCHER: &str = "/usr/libexec/tilo/tilo-launcher";
const SHIM_MOUNT: &str = "/tilo-launcher";
const LEASE_ROOT: &str = "/run/tilo/windows-compat/leases";

/// Where this person's launches' leases live: their own directory below the computer's root, or
/// the one a test pointed the service at ([`test_lease_root_at`]). The computer's root can be
/// written but not listed by the people who launch, so each lists only their own: a desktop
/// finds the leases of programs that ended while no desktop of theirs was there.
pub(crate) fn lease_root() -> PathBuf {
    #[cfg(feature = "test-support")]
    if let Some(root) = TEST_LEASE_ROOT.get() {
        return root.clone();
    }
    PathBuf::from(LEASE_ROOT).join(rustix::process::geteuid().as_raw().to_string())
}

/// The root a test of a crate that serves this service leases in: the first one it named, which
/// every later call answers too.
#[cfg(feature = "test-support")]
static TEST_LEASE_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Point leases at `root` for a test, and answer the root this process uses: the first test's.
#[cfg(feature = "test-support")]
pub fn test_lease_root_at(root: PathBuf) -> PathBuf {
    TEST_LEASE_ROOT.get_or_init(|| root).clone()
}
const PINNED_PROTON: &str = "UMU-Proton-10.0-4";
const PROTON_HOST: &str = "/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4";
const PROTON_MOUNT: &str = "/opt/tilo/proton";
const X86_ROOTFS_HOST: &str = "/usr/lib/tilo/windows-runners/x86";
const TRANSLATED_HELPER: &str = "/usr/libexec/tilo-windows-runtime";
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
    /// What the entrypoint is given before the program: the script it starts and its verb.
    entry_arguments: Vec<String>,
    /// Single files outside its binds the runtime reads, which Landlock lets it read and nothing
    /// beside them.
    reads: Vec<PathBuf>,
    environment: Vec<(&'static str, String)>,
    translated: bool,
}

impl RuntimeImage {
    /// Wine is a program loader: it lays out each Windows program's sections itself and then
    /// makes them executable, which a 32-bit program (every NSIS installer) always needs, and
    /// Windows programs generate code of their own (.NET, scripting engines). Under the
    /// write-xor-execute baseline Wine's `mprotect` to executable was refused and setup ended
    /// before showing anything, so a Wine capsule takes the profile for programs that make code.
    const fn seccomp_profile() -> SeccompProfile {
        SeccompProfile::TiloJit
    }

    /// Wine starts a 32-bit Windows program (every NSIS installer) inside its 64-bit process
    /// and gives each of its threads a 32-bit segment with the one 32-bit system call that
    /// sets one; without it such a program is killed before it starts.
    fn seccomp_grants() -> SyscallGrants {
        SyscallGrants {
            wow64_thread_area: true,
            ..SyscallGrants::default()
        }
    }

    /// Wine's own loaders and its locale tables, fonts and `wine.inf` come from the x86 guest
    /// tree, which FEX reads as its root filesystem.
    fn translate_for_arm(&mut self) {
        self.translated = true;
        self.sandbox_entrypoint = PathBuf::from(TRANSLATED_HELPER);
        self.binds.clear();
        for resource in ["usr/lib64/wine", "usr/lib/wine", "usr/share/wine"] {
            self.binds.push(RuntimeBind {
                host: Path::new(X86_ROOTFS_HOST).join(resource),
                sandbox: Path::new("/").join(resource),
            });
        }
        self.binds.extend([
            runtime_bind(TRANSLATED_HELPER, TRANSLATED_HELPER),
            runtime_bind("/usr/bin/FEX", "/usr/bin/FEX"),
            runtime_bind("/usr/bin/FEXServer", "/usr/bin/FEXServer"),
            runtime_bind("/usr/bin/FEXOfflineCompiler", "/usr/bin/FEXOfflineCompiler"),
            runtime_bind("/usr/share/fex-emu", "/usr/share/fex-emu"),
            runtime_bind(X86_ROOTFS_HOST, crate::translated::ROOTFS),
        ]);
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
    provider_manifest: Option<&'static str>,
    require_payload: bool,
}

impl SystemRuntimeRegistry {
    pub const fn new() -> Self {
        Self {
            provider_manifest: Some(PROVIDER_MANIFEST),
            require_payload: true,
        }
    }

    /// For tests on computers that are not Tilo images: the runtime payloads
    /// and the translated provider are taken as present.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub const fn assuming_provider() -> Self {
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
                    // Wine's locale tables, fonts and the `wine.inf` it sets a prefix up from:
                    // without them the wine server never starts, and no program opens.
                    runtime_bind("/usr/share/wine", "/usr/share/wine"),
                ],
                sandbox_entrypoint: PathBuf::from("/usr/bin/wine"),
                entry_arguments: Vec::new(),
                reads: Vec::new(),
                environment: Vec::new(),
                translated: false,
            }),
            Layer::Wine => Err(PlanError::RunnerNotInstalled(format!(
                "wine {}",
                runner.version
            ))),
            Layer::Proton if matches!(runner.version.as_str(), PINNED_PROTON | "10.0-4") => {
                // Proton is offered on x86 computers only. An ARM computer translates Windows
                // programs through FEX, and Proton's own program files are x86: the one wiring
                // that gives them the translator is the Steam Runtime's container, which starts
                // a sandbox of its own inside the capsule's, and a capsule's Landlock refuses
                // that. Wine stays available there.
                if host == "aarch64" {
                    return Err(PlanError::ProtonUnavailableOnArm);
                }
                Ok(proton_on_the_system())
            }
            Layer::Proton => Err(PlanError::RunnerNotInstalled(format!(
                "{} {}",
                runner.family, runner.version
            ))),
            Layer::Vm => Err(PlanError::VmIsNotCapsule),
        }?;
        match host {
            "x86_64" => {}
            "aarch64" => image.translate_for_arm(),
            _ => return Err(PlanError::UnsupportedHostArchitecture(host.to_string())),
        }
        Ok(image)
    }
}

/// Proton started by its own `proton` script on the system's libraries, as Wine is. UMU would put it
/// in the Steam runtime's container, which starts a sandbox of its own (new mounts, its own user
/// map) inside the capsule's, and the capsule's Landlock refuses every mount: there it never
/// started. Every library Proton loads comes from the image or the Windows extension
/// (`os/dev/proton_libraries.py` checks it when the extension is built), and the prefix is
/// laid out the way UMU lays it out before each start (`crate::store::prepare_for_proton`).
fn proton_on_the_system() -> RuntimeImage {
    RuntimeImage {
        binds: vec![
            runtime_bind("/usr/bin/python3", "/usr/bin/python3"),
            runtime_bind(PROTON_HOST, PROTON_MOUNT),
        ],
        sandbox_entrypoint: PathBuf::from("/usr/bin/python3"),
        // `waitforexitandrun` waits for the prefix's last program, so the capsule ends with it.
        entry_arguments: vec![
            format!("{PROTON_MOUNT}/proton"),
            "waitforexitandrun".to_string(),
        ],
        // The kernel limits Proton's script and its fixes check before a start, to warn when
        // one is low: without them the check ends the start.
        reads: vec![
            PathBuf::from("/proc/sys/fs/file-max"),
            PathBuf::from("/proc/sys/vm/max_map_count"),
        ],
        environment: vec![
            // Proton's esync and fsync keep their shared memory in /dev/shm, which the capsule
            // does not let a program write: Wine's own server-side synchronization works there.
            ("PROTON_NO_ESYNC", "1".to_string()),
            ("PROTON_NO_FSYNC", "1".to_string()),
            // A 32-bit part of a program goes through Wine's own WoW64, which needs no 32-bit
            // system libraries: the image carries none.
            ("PROTON_USE_WOW64", "1".to_string()),
            ("STEAM_COMPAT_CLIENT_INSTALL_PATH", String::new()),
            (
                "STEAM_COMPAT_SHADER_PATH",
                format!("{PREFIX_MOUNT}/shadercache"),
            ),
        ],
        translated: false,
    }
}

fn runtime_bind(host: &str, sandbox: &str) -> RuntimeBind {
    RuntimeBind {
        host: PathBuf::from(host),
        sandbox: PathBuf::from(sandbox),
    }
}

/// The display a Windows program shows its windows on, and the GPU it draws
/// with: what the caller of a launch gives it at that moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchDisplay {
    pub uid: u32,
    /// The directory the display's socket sits directly in: the login's
    /// runtime directory (`XDG_RUNTIME_DIR`), or the private directory of the
    /// display a program is kept behind.
    pub runtime_dir: PathBuf,
    pub wayland_socket: PathBuf,
    /// The GPU's render node the program draws with, or `None` where the display has no GPU: Wine
    /// then draws with the software renderer, as the desktop itself does there.
    pub render_node: Option<PathBuf>,
}

impl LaunchDisplay {
    pub fn validate(&self) -> Result<(), PlanError> {
        if !self.runtime_dir.is_absolute()
            || self.wayland_socket.parent() != Some(self.runtime_dir.as_path())
        {
            return Err(PlanError::UnsafeWaylandSocket);
        }
        if let Some(node) = &self.render_node
            && !is_render_node(node)
        {
            return Err(PlanError::UnsafeRenderNode(node.display().to_string()));
        }
        Ok(())
    }
}

/// Whether `node` is a GPU render node (`/dev/dri/renderD<N>`), the only device a launch binds into
/// its sandbox.
#[must_use]
pub fn is_render_node(node: &Path) -> bool {
    let name = node
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    node.parent() == Some(Path::new("/dev/dri"))
        && name
            .strip_prefix("renderD")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}

/// Where each launch's program shows its windows. The caller of the service
/// answers it when a program starts, not when the service starts: a display
/// that is bound later, or one made for a single launch, is the one the
/// program gets.
pub trait LaunchDisplays: Send + Sync + 'static {
    /// The display a program launched into `workspace_id` gets now, or why
    /// there is none.
    fn display_for(&self, workspace_id: &str) -> Result<LaunchDisplay, String>;

    /// A display of its own for `program`, which stays open while the desktop restarts in place,
    /// with the program behind it: `None` where programs are not kept, and the program then shows
    /// on [`Self::display_for`]'s and ends with the desktop.
    fn kept_display(
        &self,
        _program: &KeptProgram<'_>,
    ) -> Option<Result<Box<dyn KeptDisplay>, String>> {
        None
    }

    /// The programs started before the desktop last restarted in place, each taken once: they
    /// were kept, and are open again (or ended while no desktop was there, and say how).
    fn taken_back(&self) -> Vec<TakenBack> {
        Vec::new()
    }

    /// Whether this desktop started fresh, not after an in-place restart, once it knows: then
    /// nothing was kept, and every program the desktop before had open ended with it.
    fn started_fresh(&self) -> Option<bool> {
        None
    }
}

/// A program a launch keeps open through a desktop restart.
#[derive(Clone, Copy, Debug)]
pub struct KeptProgram<'a> {
    /// The app id its windows are shown under.
    pub app_id: &'a str,
    /// The name it is shown by.
    pub display_name: &'a str,
    /// Its entry in the manager's list: what a later desktop reopens.
    pub entry_id: &'a str,
    pub workspace_id: &'a str,
    /// It shows its windows through X11: the keeper serves it an X11 display of its own as well.
    pub x11: bool,
    /// It is a capsule's setup or uninstaller: it is done once it ends, and no later desktop
    /// opens it again.
    pub setup: bool,
}

/// The display a kept program shows on, and what starts the program behind it.
pub trait KeptDisplay: Send {
    /// The display to plan the program's sandbox for.
    fn display(&self) -> &LaunchDisplay;
    /// Start `command` behind the display. A display dropped without starting anything is given
    /// back.
    fn start(
        self: Box<Self>,
        command: crate::SandboxCommand,
    ) -> Result<Box<dyn crate::StartedProgram>, crate::ExecutionError>;
}

/// A kept program a service started before the desktop restarted, as the service after it finds
/// it.
pub struct TakenBack {
    pub entry_id: String,
    pub workspace_id: String,
    pub display_name: String,
    pub program: Box<dyn crate::StartedProgram>,
}

/// One display for every launch.
impl LaunchDisplays for LaunchDisplay {
    fn display_for(&self, _workspace_id: &str) -> Result<LaunchDisplay, String> {
        Ok(self.clone())
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
        // A Windows app declares no network, so it may ask for any host and its
        // grants decide: `*` is the host network, named hosts a proxy, and
        // grants that are not hosts in the one grammar (IP literals) nothing.
        match launcher::egress_proxy::EgressDecision::resolve(
            "host",
            &[],
            &capabilities.net_hosts,
            false,
        ) {
            launcher::egress_proxy::EgressDecision::Blocked => Self::Offline,
            launcher::egress_proxy::EgressDecision::Unrestricted => Self::Unrestricted,
            launcher::egress_proxy::EgressDecision::Filtered(_) => Self::Filtered,
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
    /// The private audio graph prepared for the launch (`launcher::audio_graph`), which the
    /// launch starts right before its sandbox: the directory the lease names.
    pub audio_graph: Option<PathBuf>,
    pub egress_proxy: Option<EgressProxyBind>,
}

impl LaunchLease {
    #[must_use]
    pub fn deny_all(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            audio: Vec::new(),
            audio_graph: None,
            egress_proxy: None,
        }
    }

    pub fn validate(
        &self,
        capabilities: &CapabilityPlan,
        network: NetworkShape,
    ) -> Result<(), PlanError> {
        validate_id("lease id", &self.id)?;
        let root = lease_root().join(&self.id);
        let microphone = capabilities
            .devices
            .contains(&tilo_permissions::caps::DeviceCap::Microphone);
        if self
            .audio_graph
            .as_ref()
            .is_some_and(|graph| *graph != root)
        {
            return Err(PlanError::ResourceEscalation("audio graph"));
        }
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
        match (network, self.egress_proxy.as_ref()) {
            (NetworkShape::Filtered, Some(proxy)) => {
                if !proxy.host_socket.starts_with(&root)
                    || proxy.sandbox_socket != Path::new("/run/tilo/network/proxy.sock")
                    || proxy.port < 1024
                {
                    return Err(PlanError::ResourceEscalation("egress proxy"));
                }
            }
            (NetworkShape::Filtered, None) => return Err(PlanError::MissingEgressProxy),
            (NetworkShape::Offline | NetworkShape::Unrestricted, Some(_)) => {
                return Err(PlanError::UnexpectedEgressProxy);
            }
            (NetworkShape::Offline | NetworkShape::Unrestricted, None) => {}
        }
        Ok(())
    }
}

pub trait LaunchResourceBroker: Send + Sync {
    fn materialize(&self, request: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError>;
    fn revoke(&self, lease_id: &str);
    /// Give back the leases of programs a desktop before this one started that have ended since:
    /// this desktop never held their lease ids, so it cannot revoke them one by one.
    fn release_ended(&self) {}
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
    pub seccomp_profile: Option<SeccompProfile>,
    /// What the filter of [`Self::seccomp_profile`] allows beyond it.
    pub seccomp_grants: SyscallGrants,
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
    /// The private audio graph to start right before the sandbox, whose sockets it binds.
    pub audio_graph: Option<PathBuf>,
    /// The program shows its windows through X11: the sandbox is given the X11 display its
    /// keeper serves (the launcher's `windows-x11-sandbox`).
    pub x11: bool,
    /// The network proxy to start right before the sandbox, whose socket it binds.
    pub network_proxy: Option<NetworkProxy>,
    // Open directory descriptors backing the prefix/home bind sources. Kept
    // private so a plan cannot be copied after its authority-bearing handles.
    launch_paths: LaunchPaths,
    /// Proton opens the prefix as its data directory: the prefix is laid out for it first.
    pub(crate) prefix_for_proton: bool,
    pub(crate) identity_uid: u32,
}

impl CapsuleCommandPlan {
    pub(crate) fn directory_fds(&self) -> [std::os::fd::BorrowedFd<'_>; 2] {
        self.launch_paths.directory_fds()
    }
}

/// A launch's filtered network as the launch starts it: the directory the compatibility service
/// prepared the policy in, and the control socket the session reaches the live proxy on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkProxy {
    pub dir: PathBuf,
    pub control: PathBuf,
}

pub struct SandboxPlanner<'a, R> {
    store: &'a CapsuleStore,
    runtimes: &'a R,
    display: &'a LaunchDisplay,
    /// Where the proxy's control socket goes for a kept launch: its keeper's directory, which the
    /// desktop that adopts the program after a restart finds again.
    network_control: Option<PathBuf>,
    icd_search: IcdSearch,
}

impl<'a, R: RuntimeRegistry> SandboxPlanner<'a, R> {
    pub fn new(
        store: &'a CapsuleStore,
        runtimes: &'a R,
        display: &'a LaunchDisplay,
    ) -> Result<Self, PlanError> {
        display.validate()?;
        Ok(Self {
            store,
            runtimes,
            display,
            network_control: None,
            icd_search: IcdSearch::default(),
        })
    }

    /// The control socket a kept launch's network proxy answers the session on.
    #[must_use]
    pub fn with_network_control(mut self, control: PathBuf) -> Self {
        self.network_control = Some(control);
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
        let network_proxy = lease.egress_proxy.as_ref().map(|proxy| {
            let dir = proxy
                .host_socket
                .parent()
                .map_or_else(|| proxy.host_socket.clone(), Path::to_path_buf);
            let control = self
                .network_control
                .clone()
                .unwrap_or_else(|| launcher::netproxy::control_beside(&dir));
            NetworkProxy { dir, control }
        });

        let launch_paths = self.store.open_launch_paths(&capsule.id)?;

        let wayland =
            launcher::sandbox::wayland_bind_for(&self.display.wayland_socket, self.display.uid);
        let gpu = self
            .display
            .render_node
            .as_deref()
            .map(|node| select_for_node(node, &self.icd_search));
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
        launcher::profile::push_gpu_devices(&mut argv);
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
        landlock_read.extend(runtime.reads.iter().cloned());
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
        if let Some(gpu) = &gpu {
            launcher::sandbox::push_gpu_selection(&mut argv, gpu, true);
            landlock_write.push(gpu.node.clone());
            landlock_read.extend(gpu.manifest_files());
        } else {
            // No GPU: no device node in the sandbox, and Wine's OpenGL and Vulkan load the
            // software renderer the desktop draws with, as a native app's launch does there.
            launcher::profile::hide_gpu_devices(&mut argv);
            if let Some(pin) = launcher::gpu::inherited_software_pin()
                .or_else(|| launcher::gpu::software_pin(&self.icd_search).ok())
            {
                launcher::sandbox::push_software_pin(&mut argv, &pin);
                landlock_read.extend(pin.manifest_files());
            }
        }
        if route.runtime.layer == Layer::Proton {
            let ntsync = PathBuf::from("/dev/ntsync");
            bind(&mut argv, "--dev-bind", &ntsync, &ntsync);
            landlock_write.push(ntsync);
        }
        push_audio(&mut argv, lease, self.display.uid, &mut landlock_write);
        if let Some(proxy) = &lease.egress_proxy {
            launcher::sandbox::push_egress_proxy(&mut argv, proxy);
            landlock_write.push(proxy.sandbox_socket.clone());
        }

        push_fixed_environment(&mut argv, capsule, route)?;
        for (key, value) in &runtime.environment {
            setenv(&mut argv, key, value);
        }
        // Proton reads the folder a program was installed in for the fixes it applies.
        if route.runtime.layer == Layer::Proton
            && !runtime.translated
            && let Some(folder) = Path::new(PREFIX_MOUNT)
                .join(&program.prefix_relative_executable)
                .parent()
        {
            setenv(
                &mut argv,
                "STEAM_COMPAT_INSTALL_PATH",
                &folder.display().to_string(),
            );
        }
        launcher::sandbox::push_workspace_id(&mut argv, workspace_id);
        argv.extend(["--seccomp".to_string(), crate::SECCOMP_CHILD_FD.to_string()]);
        argv.extend([
            "--ro-bind".to_string(),
            TILO_LAUNCHER.to_string(),
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
                let proxy = lease
                    .egress_proxy
                    .as_ref()
                    .ok_or(PlanError::MissingEgressProxy)?;
                argv.extend([
                    "--net-proxy".to_string(),
                    proxy.sandbox_socket.display().to_string(),
                    "--net-proxy-port".to_string(),
                    proxy.port.to_string(),
                ]);
            }
            NetworkShape::Unrestricted => {}
        }
        argv.push("--".to_string());
        argv.push(runtime.sandbox_entrypoint().display().to_string());
        argv.extend(runtime.entry_arguments.iter().cloned());
        if runtime.translated {
            // A translated runtime is Wine's: Proton is offered on x86 computers only.
            let mode = match capsule.settings.architecture {
                Architecture::Win32 => "wine32",
                Architecture::Win64 => "wine64",
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
            seccomp_profile: Some(RuntimeImage::seccomp_profile()),
            seccomp_grants: RuntimeImage::seccomp_grants(),
            capsule_id: capsule.id.clone(),
            runner_label: format!("{} {}", capsule.runner.family, capsule.runner.version),
            network,
            read_grants: capabilities.ro_paths.len(),
            write_grants: capabilities.rw_paths.len(),
            microphone: capabilities
                .devices
                .contains(&tilo_permissions::caps::DeviceCap::Microphone),
            audio_output: !lease.audio.is_empty(),
            latency_sensitive: route.runtime.layer == Layer::Proton,
            lease_id: lease.id.clone(),
            audio_graph: lease.audio_graph.clone(),
            x11: shows_through_x11(route.runtime.layer),
            network_proxy,
            launch_paths,
            prefix_for_proton: route.runtime.layer == Layer::Proton,
            identity_uid: self.display.uid,
        })
    }
}

/// Whether a program of `layer` shows its windows through X11. The pinned Proton's Wine has no
/// Wayland driver (it ships winex11 alone), so a Proton program given only a Wayland socket opens
/// no window at all.
pub(crate) fn shows_through_x11(layer: Layer) -> bool {
    layer == Layer::Proton
}

fn map_granted_files(
    argv: &mut Vec<String>,
    capabilities: &CapabilityPlan,
    landlock_read: &mut Vec<PathBuf>,
    landlock_write: &mut Vec<PathBuf>,
) {
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
        .contains(&tilo_permissions::caps::DeviceCap::Microphone)
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
        // Proton's data directory is the prefix itself: it finds its prefix at `pfx`, which
        // `crate::store::prepare_for_proton` links back to the prefix, as UMU does.
        ("STEAM_COMPAT_DATA_PATH", PREFIX_MOUNT),
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
        // What UMU tells Proton of a program's recipe, so the fixes Proton carries for a game
        // find it by the same id.
        setenv(argv, "UMU_ID", recipe);
        let app_id = steam_app_id(recipe);
        for key in ["SteamAppId", "SteamGameId", "STEAM_COMPAT_APP_ID"] {
            setenv(argv, key, app_id);
        }
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

/// The Steam app id UMU gives a recipe: the number of a `umu-<id>` recipe, else 0.
fn steam_app_id(recipe: &str) -> &str {
    recipe
        .strip_prefix("umu-")
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .unwrap_or("0")
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
        "Windows support is not installed on this device yet. It is a separate download of \
         about 2 GB, fetched only when you ask for it: turn on Windows apps in Settings, \
         then open the program again. Nothing was started."
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
    #[error("the program has no display to show its windows on: {0}")]
    NoDisplay(String),
    #[error(
        "the display's GPU device {0} is not a render node (/dev/dri/renderD*), so it is not \
         given to the program"
    )]
    UnsafeRenderNode(String),
    #[error("capsule does not match resolved route")]
    CapsuleRouteMismatch,
    #[error("derived capsule path escaped its configured root")]
    UnsafeDerivedPath,
    #[error("resource broker widened access through {0}")]
    ResourceEscalation(&'static str),
    #[error("resource broker returned the same audio protocol twice")]
    DuplicateAudioProtocol,
    #[error("filtered network launch has no Tilo egress proxy")]
    MissingEgressProxy,
    #[error("offline or unrestricted launch unexpectedly has a egress proxy")]
    UnexpectedEgressProxy,
    #[error("Proton route has no UMU recipe")]
    MissingProtonRecipe,
    #[error(
        "Proton cannot start on this ARM computer: it needs the Steam Runtime's own container \
         nested inside the Windows capsule, and a capsule's sandbox refuses that. Wine opens \
         Windows programs here: choose Wine for this program. Nothing was started."
    )]
    ProtonUnavailableOnArm,
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
            opens_with: None,
        };
        let state = store.register_program(&state.id, program.clone()).unwrap();
        (directory, store, state, program)
    }

    fn session() -> LaunchDisplay {
        LaunchDisplay {
            uid: 1000,
            runtime_dir: PathBuf::from("/xdg/1000"),
            wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
            render_node: Some(PathBuf::from("/dev/dri/renderD999")),
        }
    }

    /// Wine draws its windows with shared memory or the software renderer, so a display with no
    /// GPU (a virtual machine without 3D) still starts Windows programs: with no device node in
    /// the sandbox, never with a display's primary node in place of a render node.
    #[test]
    fn a_display_without_a_gpu_starts_the_program_with_no_device_node() {
        let route = route(Layer::Wine);
        let (_directory, store, state, program) = setup(&route);
        let registry = SystemRuntimeRegistry::assuming_provider();
        let host = LaunchDisplay {
            render_node: None,
            ..session()
        };
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
        let argv = &command.bwrap_argv;
        let hidden = argv
            .windows(2)
            .rposition(|args| args == ["--tmpfs", "/dev/dri"])
            .expect("the GPU's device directory is emptied");
        assert!(
            !argv[hidden..]
                .iter()
                .any(|arg| arg.starts_with("/dev/dri/")),
            "no device node is bound over it: {argv:?}"
        );

        let card = LaunchDisplay {
            render_node: Some(PathBuf::from("/dev/dri/card0")),
            ..session()
        };
        assert!(matches!(
            card.validate(),
            Err(PlanError::UnsafeRenderNode(node)) if node == "/dev/dri/card0"
        ));
        assert!(host.validate().is_ok());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn runtime_selection_follows_host_architecture_and_shipped_artifacts() {
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

            // On x86, Proton's own script starts it on the system's libraries, in the capsule's
            // sandbox alone: no UMU and no Steam runtime container inside it.
            let proton = SystemRuntimeRegistry::resolve_for_host(
                Layer::Proton,
                &Runner::new("umu-proton", "UMU-Proton-10.0-4").unwrap(),
                "x86_64",
            )
            .unwrap();
            assert_eq!(proton.sandbox_entrypoint(), Path::new("/usr/bin/python3"));
            assert_eq!(
                proton.entry_arguments,
                [
                    format!("{PROTON_MOUNT}/proton"),
                    "waitforexitandrun".to_string()
                ]
            );
            assert!(
                proton
                    .host_paths()
                    .any(|path| path == Path::new(PROTON_HOST))
            );
            assert!(!proton.host_paths().any(|path| {
                path.starts_with("/usr/lib/tilo/windows-runners/steam-runtime")
                    || path.ends_with("tilo-umu-run")
                    || path == Path::new("/usr/bin/bwrap")
            }));
            for variable in [
                ("PROTON_NO_ESYNC", "1"),
                ("PROTON_NO_FSYNC", "1"),
                ("PROTON_USE_WOW64", "1"),
            ] {
                assert!(
                    proton
                        .environment
                        .contains(&(variable.0, variable.1.to_string()))
                );
            }
            assert!(
                !proton
                    .environment
                    .iter()
                    .any(|(key, _)| key.starts_with("UMU_"))
            );
            assert_eq!(RuntimeImage::seccomp_profile(), SeccompProfile::TiloJit);
            assert_eq!(
                proton.reads,
                [
                    PathBuf::from("/proc/sys/fs/file-max"),
                    PathBuf::from("/proc/sys/vm/max_map_count")
                ]
            );

            // ARM translates Wine through FEX. Proton is offered there by neither route.
            let error = SystemRuntimeRegistry::resolve_for_host(
                Layer::Proton,
                &Runner::new("umu-proton", "UMU-Proton-10.0-4").unwrap(),
                "aarch64",
            )
            .unwrap_err();
            assert!(matches!(error, PlanError::ProtonUnavailableOnArm));
            let wine = SystemRuntimeRegistry::resolve_for_host(
                Layer::Wine,
                &Runner::new("wine", "system").unwrap(),
                "aarch64",
            )
            .unwrap();
            assert!(wine.translated);
            assert_eq!(wine.sandbox_entrypoint(), Path::new(TRANSLATED_HELPER));
            assert!(
                wine.host_paths()
                    .any(|path| path == Path::new(X86_ROOTFS_HOST))
            );
        }
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
                        if host == "aarch64" && layer == Layer::Proton {
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
                            assert!(matches!(error, PlanError::ProtonUnavailableOnArm));
                            continue;
                        }
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
                        assert_eq!(command.seccomp_profile, Some(SeccompProfile::TiloJit));
                        assert!(
                            command.seccomp_grants.wow64_thread_area,
                            "a Wine capsule may set its 32-bit thread area"
                        );
                        assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
                        assert!(
                            command
                                .bwrap_argv
                                .iter()
                                .any(|arg| arg == "--net-restricted")
                        );
                        if host == "aarch64" {
                            let mode = match architecture {
                                Architecture::Win32 => "wine32",
                                Architecture::Win64 => "wine64",
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
                            assert!(!command.bwrap_argv.iter().any(|arg| arg == "/usr/bin/wine"));
                            for resource in ["usr/lib/wine", "usr/lib64/wine"] {
                                let source = format!("{X86_ROOTFS_HOST}/{resource}");
                                let destination = format!("/{resource}");
                                assert!(
                                    command.bwrap_argv.windows(3).any(|args| {
                                        args == ["--ro-bind", &source, &destination]
                                    })
                                );
                            }
                            assert!(
                                !command
                                    .bwrap_argv
                                    .iter()
                                    .any(|arg| arg.contains("wine-wow64"))
                            );
                        } else {
                            assert!(
                                !command
                                    .bwrap_argv
                                    .iter()
                                    .any(|arg| arg == TRANSLATED_HELPER)
                            );
                            if layer == Layer::Wine {
                                assert!(command.bwrap_argv.windows(3).any(|args| {
                                    args == ["--ro-bind", "/usr/share/wine", "/usr/share/wine"]
                                }));
                            }
                        }
                    }
                }
            }
        }
        {
            for (host, layer) in [
                ("x86_64", Layer::Wine),
                ("x86_64", Layer::Proton),
                ("aarch64", Layer::Wine),
            ] {
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

    /// An ARM computer offers no Proton: Proton's own program files are x86, and Tilo can give
    /// them the translator only inside the Steam Runtime's container, which starts a sandbox of
    /// its own that a capsule's sandbox refuses. The person is told that plainly, and Wine stays.
    #[test]
    fn an_arm_computer_refuses_proton_with_the_reason_and_keeps_wine() {
        let refusal = SystemRuntimeRegistry::resolve_for_host(
            Layer::Proton,
            &Runner::new("umu-proton", PINNED_PROTON).unwrap(),
            "aarch64",
        )
        .unwrap_err();
        assert!(matches!(refusal, PlanError::ProtonUnavailableOnArm));
        let said = refusal.to_string();
        assert!(said.contains("Steam Runtime's own container"), "{said}");
        assert!(said.contains("capsule's sandbox refuses"), "{said}");
        assert!(
            said.contains("Wine"),
            "the message names what works here: {said}"
        );
        assert!(said.contains("Nothing was started"), "{said}");

        let route = route(Layer::Proton);
        let (_directory, store, state, program) = setup(&route);
        let error = SandboxPlanner::new(&store, &TestHostRegistry("aarch64"), &session())
            .unwrap()
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), said, "a launch is refused the same way");

        // The x86 Proton start and the ARM Wine start are unchanged by the refusal.
        let x86 = SystemRuntimeRegistry::resolve_for_host(
            Layer::Proton,
            &Runner::new("umu-proton", PINNED_PROTON).unwrap(),
            "x86_64",
        )
        .unwrap();
        assert_eq!(x86.sandbox_entrypoint(), Path::new("/usr/bin/python3"));
        let wine = SystemRuntimeRegistry::resolve_for_host(
            Layer::Wine,
            &Runner::new("wine", "system").unwrap(),
            "aarch64",
        )
        .unwrap();
        assert!(wine.translated);
        assert_eq!(wine.sandbox_entrypoint(), Path::new(TRANSLATED_HELPER));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn wine_plans_are_default_deny_with_fixed_artifacts_and_no_injection() {
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

    /// A Proton program shows through X11: the display its keeper picks once it starts is added
    /// to the planned sandbox, inside its private `/tmp`, after the environment is emptied, and
    /// reachable through Landlock, and nothing else of the plan moves.
    #[test]
    fn a_proton_sandbox_is_given_its_keepers_x11_display() {
        let route = route(Layer::Proton);
        let (_directory, store, state, program) = setup(&route);
        let host = session();
        let planned = SandboxPlanner::new(&store, &TestHostRegistry("x86_64"), &host)
            .unwrap()
            .plan(
                &route,
                &state,
                &program,
                &CapabilityPlan::empty(),
                "workspace",
                &LaunchLease::deny_all("lease"),
            )
            .unwrap()
            .bwrap_argv;
        let mut given: Vec<std::ffi::OsString> = planned.iter().map(Into::into).collect();
        let cookie = Path::new("/home/me/kept/3/Xauthority");
        launcher::sandbox::give_x11_display(&mut given, ":7", cookie).unwrap();
        let given: Vec<String> = given
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        let at = |wanted: &[&str]| {
            given
                .windows(wanted.len())
                .position(|args| args == wanted)
                .unwrap_or_else(|| panic!("{wanted:?} in {given:?}"))
        };
        let socket = "/tmp/.X11-unix/X7";
        let mount = launcher::sandbox::X11_AUTHORITY_MOUNT;
        let tmp = at(&["--tmpfs", "/tmp"]);
        let bound = at(&["--ro-bind", socket, socket]);
        let cookie_bound = at(&["--ro-bind", cookie.to_str().unwrap(), mount]);
        let display = at(&["--setenv", "DISPLAY", ":7"]);
        let authority = at(&["--setenv", "XAUTHORITY", mount]);
        let shim = at(&[launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND]);
        let read = at(&["--ro", mount]);
        let reached = at(&["--rw", socket]);
        let program = at(&["--"]);
        assert!(
            at(&["--clearenv"]) < display,
            "set after the environment is emptied"
        );
        assert!(
            tmp < bound && tmp < cookie_bound,
            "bound over the private /tmp"
        );
        assert!(authority < shim && shim < read && shim < reached && reached < program);
        assert_eq!(given.len(), planned.len() + 16, "nothing else is added");
        let mut rest = given.clone();
        rest.drain(shim + 1..shim + 5);
        rest.drain(tmp + 2..tmp + 14);
        assert_eq!(rest, planned, "and nothing of the plan moves");

        for (display, cookie) in [
            ("wayland-0", Path::new("/var/cookie")),
            (":7", Path::new("cookie")),
            ("remote:7", Path::new("/var/cookie")),
        ] {
            let mut args: Vec<std::ffi::OsString> = planned.iter().map(Into::into).collect();
            assert!(
                launcher::sandbox::give_x11_display(&mut args, display, cookie).is_err(),
                "{display} {cookie:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn capsules_provision_private_storage_brokered_audio_and_proton_runtimes() {
        {
            let route = route(Layer::Proton);
            let (_directory, store, state, program) = setup(&route);
            let host = session();
            let planner = SandboxPlanner::new(&store, &TestHostRegistry("x86_64"), &host).unwrap();
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
            // The prefix is Proton's data directory, and the program's recipe reaches the fixes
            // Proton carries under the id UMU would give it.
            let set = |key: &str| {
                command
                    .bwrap_argv
                    .windows(3)
                    .find(|args| args[0] == "--setenv" && args[1] == key)
                    .map(|args| args[2].clone())
            };
            assert_eq!(set("STEAM_COMPAT_DATA_PATH").as_deref(), Some(PREFIX_MOUNT));
            assert_eq!(set("WINEPREFIX").as_deref(), Some(PREFIX_MOUNT));
            let recipe = route.proton_recipe.unwrap();
            assert_eq!(set("SteamAppId").as_deref(), Some(steam_app_id(&recipe)));
            assert_eq!(set("UMU_ID"), Some(recipe));
            assert_eq!(set("STEAM_COMPAT_CLIENT_INSTALL_PATH").as_deref(), Some(""));
            assert_eq!(
                set("STEAM_COMPAT_INSTALL_PATH"),
                Path::new(PREFIX_MOUNT)
                    .join(&program.prefix_relative_executable)
                    .parent()
                    .map(|folder| folder.display().to_string())
            );
            let entry = command
                .bwrap_argv
                .iter()
                .position(|arg| arg == "--")
                .unwrap();
            assert_eq!(
                command.bwrap_argv[entry + 1..entry + 4],
                [
                    "/usr/bin/python3".to_string(),
                    format!("{PROTON_MOUNT}/proton"),
                    "waitforexitandrun".to_string()
                ]
            );
            assert!(command.prefix_for_proton);
            assert!(!command.bwrap_argv.iter().any(|arg| {
                arg.starts_with("/usr/lib/tilo/windows-runners/steam-runtime")
                    || (arg.starts_with("UMU_") && arg != "UMU_ID")
            }));
            assert_eq!(steam_app_id("umu-1091500"), "1091500");
            assert_eq!(steam_app_id("umu-"), "0");
            assert_eq!(steam_app_id("none"), "0");
            assert!(!command.bwrap_argv.iter().any(|arg| arg == "--share-net"));
        }
        {
            for layer in [Layer::Wine, Layer::Proton] {
                let route = route(layer);
                let (_directory, store, state, program) = setup(&route);
                // Proton is planned on an x86 computer; Wine is planned on ARM, translated.
                let registry = TestHostRegistry(if layer == Layer::Proton {
                    "x86_64"
                } else {
                    "aarch64"
                });
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
                devices: vec![tilo_permissions::caps::DeviceCap::Microphone],
                ..CapabilityPlan::empty()
            };
            let graph = lease_root().join("audio-lease");
            let pipewire = graph.join("audio/pipewire-0");
            let pulse = graph.join("audio/pulse/native");
            let lease = LaunchLease {
                id: "audio-lease".to_string(),
                audio: vec![
                    AudioBinding {
                        protocol: AudioProtocol::PipeWire,
                        host_socket: pipewire.clone(),
                        input_enabled: true,
                    },
                    AudioBinding {
                        protocol: AudioProtocol::Pulse,
                        host_socket: pulse.clone(),
                        input_enabled: true,
                    },
                ],
                audio_graph: Some(graph),
                egress_proxy: None,
            };
            let command = planner
                .plan(&route, &state, &program, &capabilities, "workspace", &lease)
                .unwrap();
            assert!(
                command
                    .bwrap_argv
                    .iter()
                    .any(|arg| *arg == pipewire.to_string_lossy())
            );
            assert!(
                command
                    .bwrap_argv
                    .iter()
                    .any(|arg| *arg == pulse.to_string_lossy())
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
        {
            let route = route(Layer::Proton);
            let (_directory, store, state, program) = setup(&route);
            let registry = TestHostRegistry("x86_64");
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
                    .windows(2)
                    .any(|args| args == ["--", "/usr/bin/python3"])
            );
            assert!(
                command
                    .bwrap_argv
                    .windows(3)
                    .any(|args| { args == ["--setenv", "PROTON_NO_FSYNC", "1"] })
            );
            // Proton's script reads two kernel limits; Landlock lets it read those files alone,
            // never the rest of the capsule's /proc.
            for read in ["/proc/sys/fs/file-max", "/proc/sys/vm/max_map_count"] {
                assert!(
                    command
                        .bwrap_argv
                        .windows(2)
                        .any(|args| args == ["--ro", read])
                );
            }
            assert!(
                !command
                    .bwrap_argv
                    .windows(2)
                    .any(|args| args == ["--ro", "/proc"])
            );
            // Proton's Wine is held to the same system calls as Wine's.
            assert_eq!(command.seccomp_profile, Some(SeccompProfile::TiloJit));
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

    fn proton() -> RuntimeImage {
        SystemRuntimeRegistry::resolve_for_host(
            Layer::Proton,
            &Runner::new("umu-proton", PINNED_PROTON).unwrap(),
            "x86_64",
        )
        .unwrap()
    }

    tilo_test_env::scenario! {
        fn the_windows_payload_is_required_before_a_plan_binds_it;

        phase a_device_without_windows_support_gets_an_install_instruction_not_a_failed_bind;
        {
            let error = require_payload(&proton(), |_| false).unwrap_err();
            assert!(matches!(error, PlanError::WindowsSupportNotInstalled));
            let message = error.to_string();
            assert!(message.contains("not installed"), "{message}");
            assert!(
                message.contains("Windows apps in Settings"),
                "the message names where to turn it on: {message}"
            );
            assert!(message.contains("Nothing was started"), "{message}");
        }

        phase a_merged_payload_plans_normally;
        {
            assert!(require_payload(&proton(), |_| true).is_ok());
        }

        phase only_payload_paths_are_probed_not_the_base_the_image_always_has;
        {
            let probed = std::cell::RefCell::new(Vec::new());
            let _ = require_payload(&proton(), |path| {
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

        phase production_registry_requires_the_payload_and_test_registry_does_not;
        {
            assert!(SystemRuntimeRegistry::new().require_payload);
            assert!(!SystemRuntimeRegistry::assuming_provider().require_payload);
        }

        phase the_payload_paths_are_exactly_what_the_sysext_stage_packs;
        {
            let containerfile = include_str!("../../../../os/Containerfile");
            for packed in [
                "/tree/usr/lib/tilo/windows-runners/proton/",
                "/payload/usr/lib/tilo/windows-runners",
            ] {
                assert!(containerfile.contains(packed), "{packed}");
            }
            // Proton starts on the system's libraries, never in the Steam
            // Runtime's nested sandbox, so the runtime is not payload.
            assert!(!containerfile.contains("steam-runtime"));
            for host in [PROTON_HOST, X86_ROOTFS_HOST] {
                assert!(
                    PAYLOAD_PATHS.iter().any(|prefix| host.starts_with(prefix)),
                    "{host}"
                );
            }
        }
    }
}
