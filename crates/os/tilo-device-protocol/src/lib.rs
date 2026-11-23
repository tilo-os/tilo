//! The typed actions a device answers for `tilo-vm`: each step is one action on the device's
//! `devices/<device>/vm` resource, invoked over the device's own action frames. A [`Call`] is a
//! step's argument struct; its [`Call::ACTION`] is the action id and [`Call::Output`] the value
//! the action completes with. A step that fails completes with an action failure whose
//! structured detail is an [`RpcError`]. A device whose catalogue lacks an action runs an image
//! that predates it.

pub mod disk_unlock;
pub mod display;
pub mod image;
pub mod input;
pub mod layer_hash;
pub mod manifest;
pub mod profile;

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use disk_unlock::{DeviceUnlockEnrol, DeviceUnlockEnrolResult};
pub use display::OutputSpec;
pub use input::{Button, InputAction, Stick};
pub use manifest::{ComponentInfo, Manifest, Scope};

/// One synced source tree per host worktree for in-VM builds; each one's `target/` is
/// evictable.
pub const SRC_DIR: &str = "/var/lib/tilo-vm/src";

/// The name of the pseudo-account whose layers merge into the VM's own `/usr`.
pub const VM_ACCOUNT: &str = "vm";

/// The pseudo-account that is the machine's own session: the one an image runs for its account
/// without a greeter (a board's always-on session). Only images that run one have it.
pub const DEVICE_ACCOUNT: &str = "device";

/// How long the keep-awake lease lasts after the last `awake` call that renewed it.
pub const AWAKE_IDLE_SECONDS: u64 = 30 * 60;

/// An error the daemon reports; `hint` is the next thing to try.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub see: Option<String>,
}

/// Error codes.
pub mod code {
    pub const INVALID_PARAMS: i64 = -32602;
    pub const NOT_FOUND: i64 = 2;
    pub const CONFLICT: i64 = 3;
    pub const INVALID: i64 = 4;
    pub const SCOPE: i64 = 5;
    pub const APPLY: i64 = 6;
    pub const IO: i64 = 7;
    pub const ABI: i64 = 8;
    /// The registry refused the VM's credentials.
    pub const AUTH: i64 = 9;
    /// A tool the job needs is not in the VM's image.
    pub const TOOL: i64 = 10;
    /// The build or extraction itself failed; its output says why.
    pub const JOB: i64 = 11;
}

/// A step's argument struct: the action it invokes and the value that action completes with.
pub trait Call: Serialize + DeserializeOwned {
    const ACTION: &'static str;
    type Output: Serialize + DeserializeOwned;
}

macro_rules! call {
    ($($params:path = $action:literal => $output:ty),* $(,)?) => {
        $(impl Call for $params {
            const ACTION: &'static str = $action;
            type Output = $output;
        })*
        /// Every action id a device publishes on `devices/<device>/vm`.
        pub const ACTIONS: &[&str] = &[$($action),*];
    };
}

call! {
    Status = "status" => StatusResult,
    AccountCreate = "account-create" => AccountCreateResult,
    AccountList = "account-list" => Vec<AccountInfo>,
    AccountRestart = "account-restart" => ApplyResult,
    AccountRm = "account-rm" => AccountRmResult,
    AccountShow = "account-show" => AccountShowResult,
    LayerImport = "layer-import" => LayerImportResult,
    LayerList = "layer-list" => Vec<LayerInfo>,
    LayerVerify = "layer-verify" => LayerInfo,
    Swap = "swap" => ApplyResult,
    UserspaceApply = "userspace-apply" => UserspaceResult,
    Rollback = "rollback" => ApplyResult,
    Logs = "logs" => LogsResult,
    SeatClaim = "seat-claim" => SeatResult,
    SeatRelease = "seat-release" => SeatResult,
    Shot = "shot" => ShotResult,
    Control = "control" => ControlResult,
    Input = "input" => InputResult,
    Gc = "gc" => GcResult,
    AppInstall = "app-install" => AppInstallResult,
    BootConfirm = "boot-confirm" => BootConfirmResult,
    Awake = "awake" => AwakeLease,
    NetworkWake = "network-wake" => NetworkWakeStatus,
    DeviceUnlockEnrol = "device-unlock-enrol" => DeviceUnlockEnrolResult,
    Build = "build" => JobOutput,
    CiLayer = "ci-layer" => JobOutput,
    Exec = "exec" => ExecResult,
    InstallPackages = "install-packages" => InstallPackagesResult,
    TreeSync = "tree-sync" => TreeSyncResult,
    image::Upgrade = "upgrade" => image::UpgradeResult,
    image::Reboot = "reboot" => image::RebootResult,
    image::Poweroff = "poweroff" => image::PoweroffResult,
}

/// Status of one account, or of every account when `account` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCreate {
    pub name: String,
    pub kind: AccountKind,
    #[serde(default)]
    pub owner: Owner,
    /// Headful only: one vkms connector per output, left to right, with this mode and scale
    /// applied through the display settings. Empty means one connector at the default mode.
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    /// Give the account's user a throwaway test identity through the session's dev-only control
    /// op, and start the session with fresh desktop state.
    #[serde(default)]
    pub identity: bool,
    /// Make the account's user this VM's person instead, as a device's first login does: the
    /// session's onboarding creates their identity and certifies the machine's device key, which
    /// starts `fs`. One account per VM; refused once the machine holds an identity.
    #[serde(default)]
    pub device_identity: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountList {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRestart {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRm {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountShow {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerSource {
    /// A blob in the device's content store, named by its blake3 hex: an uncompressed tar of
    /// the layer's `usr/` tree, or a `.tapp`.
    Content { hash: String },
    /// A tarball or directory that already exists inside the VM.
    Path(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerImport {
    pub source: LayerSource,
    pub provenance: Box<Provenance>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerList {}

/// Verify stored content before reusing a layer; when the method is unavailable, stream the
/// layer instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerVerify {
    pub hash: String,
}

/// Replace (or add) one component's layer in an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Swap {
    pub account: String,
    pub component: String,
    pub layer: String,
    /// Scope from the client's manifest, which matches the build. When absent
    /// the daemon falls back to its own manifest, then to session scope.
    #[serde(default)]
    pub info: Option<ComponentInfo>,
    /// The next generation of an account this swap moves together with, when the
    /// two are one matched set (a session cohort and the VM-wide runtime
    /// services it reads). The interface check judges this swap against that
    /// generation instead of the account's current one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner: Option<Partner>,
}

/// An account's next generation, for a swap that moves with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Partner {
    pub account: String,
    /// Component name to layer hash.
    pub components: std::collections::BTreeMap<String, String>,
}

/// The interface every runtime service speaks to apps.
pub const RUNTIME: &str = "runtime";

/// The runtime services apps reach that run VM-wide, outside any account's session set: swapped
/// into the `vm` account and shared by every account. Each speaks the runtime interface alone.
pub const VM_RUNTIME_SERVICES: &[&str] = &["fs"];

/// The matched session components, in layer precedence order. VM-wide managers and boot
/// components are separate.
pub const USERSPACE_COMPONENTS: &[&str] = &[
    "launcher",
    "install",
    "apps+runtime",
    "package-service",
    "native-runtime-host",
    "device-steps",
    "model-manager",
    "model-gateway",
    "control",
    "app-cli",
    "ush",
    "tilo-session",
];

/// A build receipt from the same frozen source inventory; reused layer bytes keep the provenance
/// of their own build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserspaceMember {
    pub component: String,
    pub layer: String,
    pub build: Provenance,
    pub source_inventory: String,
}

/// The account and login a generation was applied to, revalidated at activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserspaceSession {
    pub user: String,
    pub unit: String,
    pub login_session: Option<String>,
    pub login_wrapper: Option<u32>,
}

/// All members are imported before this applies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserspaceApply {
    pub account: String,
    pub expected_generation: u64,
    pub expected_session: UserspaceSession,
    pub source: Box<Provenance>,
    pub source_inventory: String,
    pub members: Vec<UserspaceMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserspacePhase {
    pub name: String,
    pub seconds: f64,
}

/// A failed selection keeps its SDK closure; it never reports success or leaves selected user
/// packages without the runtime beneath them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserspaceResult {
    pub apply: ApplyResult,
    pub source: Provenance,
    pub source_inventory: String,
    pub members: Vec<UserspaceMember>,
    pub phases: Vec<UserspacePhase>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    pub account: String,
    /// Target generation; the one before the current generation when absent.
    #[serde(default)]
    pub to: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logs {
    pub account: String,
    pub lines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatClaim {
    pub account: String,
    #[serde(default)]
    pub owner: Owner,
    /// Take seat0 even when another account holds the lease.
    #[serde(default)]
    pub steal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRelease {
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shot {
    pub account: String,
}

/// Arguments passed verbatim to the in-VM `tilo-control` for this account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub account: String,
    pub args: Vec<String>,
}

/// Remove what no kept generation references, and evict build dirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gc {
    /// Generations to keep per account, besides the current one.
    pub keep: u32,
    pub dry_run: bool,
    /// In-VM build dirs (one per host worktree) to keep, most recently used first.
    #[serde(default)]
    pub keep_targets: Option<u32>,
    /// Evict further build dirs, least recently used first, until the VM has this much free
    /// space.
    #[serde(default)]
    pub free_goal_bytes: Option<u64>,
    /// Unreferenced layers younger than this stay: an import whose swap has not happened yet
    /// looks unreferenced.
    #[serde(default)]
    pub min_age_seconds: Option<u64>,
}

/// Drive a headful account's seat through its own uinput keyboard and pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub account: String,
    pub actions: Vec<InputAction>,
}

/// Install a `.tapp` into the account user's app store (`tilo-install
/// install --allow-self-signed --force`, run as that user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstall {
    pub account: String,
    /// The `.tapp` file in the device's content store, or a path to one inside the VM.
    pub source: LayerSource,
}

/// After a reboot onto a newly installed kernel: report the running kernel
/// and, when it is the one the last swap or rollback installed, make its boot
/// entry the default. Until then that entry is booted once only, so a kernel
/// that never comes up falls back to the previous default on the next boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootConfirm {
    /// `uname -v` the new kernel reports (its build number and timestamp).
    /// Rebuilds keep the kernel release, so only this tells two builds of
    /// one release apart. When absent, the release alone is compared.
    #[serde(default)]
    pub expect_version: Option<String>,
}

/// Inspect or configure one saved `NetworkManager` profile's magic-packet wake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkWake {
    pub profile: String,
    pub enabled: Option<bool>,
}

/// A configured, supported profile does not prove the machine wakes from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkWakeStatus {
    pub profile: String,
    pub interface: Option<String>,
    pub configured: bool,
    pub supported: Option<bool>,
    pub applied: Option<bool>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Awake {
    /// Hold it awake this many seconds from now instead.
    #[serde(default)]
    pub seconds: Option<u64>,
    /// Release the lease now, so the machine may suspend; the next request
    /// takes it again.
    #[serde(default)]
    pub off: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Headful,
    Headless,
    /// Only the reserved [`VM_ACCOUNT`] has this kind.
    Vm,
    /// Only the reserved [`DEVICE_ACCOUNT`] has this kind.
    Device,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headful => "headful",
            Self::Headless => "headless",
            Self::Vm => "vm",
            Self::Device => "device",
        }
    }
}

/// Who created an account, to tell agents' accounts apart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// How a layer was built. Recorded verbatim in `provenance.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub component: String,
    /// `local` for anything built outside CI. Local layers are never promotable.
    #[serde(default = "local")]
    pub authority: String,
    #[serde(default)]
    pub source_commit: Option<String>,
    /// Hash of uncommitted changes; `None` for a clean tree.
    #[serde(default)]
    pub dirty_tree_hash: Option<String>,
    /// Digest of the base image the layer was built against.
    #[serde(default)]
    pub base_image_digest: Option<String>,
    #[serde(default)]
    pub builder_image_digest: Option<String>,
    /// Rust target triple, e.g. `x86_64-unknown-linux-gnu`.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
    #[serde(default)]
    pub cargo_lock_hash: Option<String>,
    #[serde(default)]
    pub cargo_profile: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    /// `host`, `vm` or `ci`.
    #[serde(default)]
    pub builder: Option<String>,
    /// The interface versions the layer was built for: the `[interfaces]` of `system-interfaces.toml`
    /// in its tree. After an image change a layer still merges when these match the booted
    /// image's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interfaces: BTreeMap<String, u32>,
}

/// Where a source tree declares the interfaces its image serves.
pub const SYSTEM_INTERFACES_IN_TREE: &str = "os/usr/share/tilo/system-interfaces.toml";
/// Where an image declares them.
pub const SYSTEM_INTERFACES: &str = "/usr/share/tilo/system-interfaces.toml";

/// The interfaces the source tree at `root` declares; empty when it declares none.
#[must_use]
pub fn tree_interfaces(root: &std::path::Path) -> BTreeMap<String, u32> {
    std::fs::read_to_string(root.join(SYSTEM_INTERFACES_IN_TREE))
        .map(|text| system_interfaces(&text))
        .unwrap_or_default()
}

/// The `[interfaces]` table of a `system-interfaces.toml`, each interface's one version. Empty
/// when the text is not such a file.
#[must_use]
pub fn system_interfaces(text: &str) -> BTreeMap<String, u32> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        interfaces: BTreeMap<String, u32>,
    }
    toml::from_str::<File>(text)
        .map(|file| file.interfaces)
        .unwrap_or_default()
}

/// What a `system-interfaces.toml` declares, as this code reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Declaration {
    /// There is no declaration: nothing is judged against one.
    Absent,
    /// There is one, but in a form this code does not read: it was written for another version
    /// of the system.
    Unreadable { detail: String },
    /// Each interface's one version.
    Versions { versions: BTreeMap<String, u32> },
}

impl Declaration {
    /// The declaration in `text`, a `system-interfaces.toml`.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        #[derive(Deserialize)]
        struct File {
            interfaces: BTreeMap<String, u32>,
        }
        match toml::from_str::<File>(text) {
            Ok(file) => Self::Versions {
                versions: file.interfaces,
            },
            Err(error) => Self::Unreadable {
                detail: error.message().to_owned(),
            },
        }
    }

    /// The declaration at `path`; [`Self::Absent`] when there is no file.
    #[must_use]
    pub fn read(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::Absent,
            Err(error) => Self::Unreadable {
                detail: error.to_string(),
            },
        }
    }

    /// The versions, when this is a declaration this code reads.
    #[must_use]
    pub fn versions(&self) -> Option<&BTreeMap<String, u32>> {
        match self {
            Self::Versions { versions } => Some(versions),
            Self::Absent | Self::Unreadable { .. } => None,
        }
    }
}

fn local() -> String {
    "local".into()
}

impl Provenance {
    /// A component copied out of CI's image of a build, for `arch`: `requested` is what the
    /// client knew (component, profile, features, the commit it asked for); `digest` and
    /// `revision` are the pulled image's.
    pub fn from_ci(
        requested: &Self,
        arch: &str,
        digest: Option<String>,
        revision: Option<String>,
    ) -> Self {
        let mut provenance = Self::local(&requested.component);
        provenance.authority = "ci".into();
        provenance.builder = Some("ci".into());
        provenance.source_commit = revision.or_else(|| requested.source_commit.clone());
        provenance.base_image_digest = digest;
        provenance.target = Some(format!("{arch}-unknown-linux-gnu"));
        provenance
            .cargo_profile
            .clone_from(&requested.cargo_profile);
        provenance.features.clone_from(&requested.features);
        provenance
    }

    pub fn local(component: &str) -> Self {
        Self {
            component: component.to_owned(),
            authority: local(),
            source_commit: None,
            dirty_tree_hash: None,
            base_image_digest: None,
            builder_image_digest: None,
            target: None,
            toolchain: None,
            cargo_lock_hash: None,
            cargo_profile: None,
            features: Vec::new(),
            builder: None,
            interfaces: BTreeMap::new(),
        }
    }
}

/// What the VM's base image was built with, from its `/usr/share/tilo-vm/base.json`. Layers are
/// checked against it, field for field in the form [`Provenance`] records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseInfo {
    #[serde(default)]
    pub image_digest: Option<String>,
    /// `None` in a dev image: it is its own builder, identified by
    /// `toolchain`, `target` and `cargo_lock_hash` instead.
    #[serde(default)]
    pub builder_image_digest: Option<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
    /// Rust target triple of the image's toolchain (`rustc -vV`'s host).
    #[serde(default)]
    pub target: Option<String>,
    /// SHA-256 (hex) of the `Cargo.lock` the image was built from.
    #[serde(default)]
    pub cargo_lock_hash: Option<String>,
}

/// Who holds seat0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub account: String,
    pub owner: Owner,
    pub since: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    /// Physically allocated bytes of all layers.
    pub layers_bytes: u64,
    /// In-VM build dirs, as measured after each build.
    #[serde(default)]
    pub targets_bytes: u64,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

/// What an apply does to running sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Nothing changed, or only apps changed: live at next launch.
    NoRestart,
    /// The account's session logs out and a fresh login starts.
    SessionRestart,
    /// `systemd-sysext refresh`, then the affected system units restart.
    VmRefresh,
    /// A kernel changed: VM `/usr` refreshed and the new kernel made the next boot entry
    /// ([`ApplyResult::boot`]); the client reboots the VM and confirms the boot.
    Reboot,
}

/// One observable step the daemon took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    EnsureUser {
        user: String,
        uid: u32,
    },
    RemoveUser {
        user: String,
    },
    WriteDropIn {
        unit: String,
    },
    RemoveDropIn {
        unit: String,
    },
    StartUnit {
        unit: String,
    },
    RestartUnit {
        unit: String,
    },
    StopUnit {
        unit: String,
    },
    SysextRefresh,
    AllocateSeat {
        seat: String,
    },
    ReleaseSeat {
        seat: String,
    },
    /// The outputs' modes and scales, confirmed through the display settings.
    SetOutputs {
        outputs: String,
    },
    /// The session gave its user a throwaway test identity.
    ProvisionTestIdentity {
        created: bool,
    },
    /// The session's onboarding made its user this VM's person; the machine's device certificate
    /// started `fs`.
    CompleteOnboarding {
        created: bool,
    },
    /// The account user's desktop layout removed, so the next session starts with none.
    ResetDesktopState,
    /// The desktop of a login restarted in place (`tilo-session` on SIGUSR1); the login, its
    /// user manager and its services stayed.
    RestartDesktop {
        user: String,
    },
    /// A user unit of the session's account restarted.
    RestartUserUnit {
        user: String,
        unit: String,
    },
    /// Recorded but not activated; takes effect at the next session restart.
    Deferred {
        reason: String,
    },
    /// A kernel's vmlinuz and initramfs copied to `/boot` with a boot loader entry of their own.
    InstallBootEntry {
        entry: String,
        kver: String,
    },
    RemoveBootEntry {
        entry: String,
    },
    /// The entry the next boot (only) starts.
    SetNextBoot {
        entry: String,
    },
    SetDefaultBoot {
        entry: String,
    },
}

/// The logind `sleep` block lock that keeps the machine from suspending
/// while someone works on it (`tilo-vm-awake.service`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwakeLease {
    /// Unix seconds when the lease ends; `None` once released.
    #[serde(default)]
    pub until: Option<u64>,
    /// Unix seconds an explicit hold (`tilo-vm vm awake --for`) lasts until.
    #[serde(default)]
    pub hold_until: Option<u64>,
    /// Whether the lock is in place right now.
    pub held: bool,
    /// Why it is not, when it should be.
    #[serde(default)]
    pub problem: Option<String>,
}

/// Tools found on the VM's `PATH`, which decide where builds and CI layer extraction can run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per tool"
)]
pub struct VmTools {
    pub cargo: bool,
    pub python3: bool,
    pub sccache: bool,
    pub podman: bool,
}

impl VmTools {
    /// Everything the in-VM builder needs.
    pub fn can_build(&self) -> bool {
        self.cargo && self.python3
    }

    /// Names of the tools the in-VM builder lacks.
    pub fn missing_for_build(&self) -> Vec<&'static str> {
        [("cargo", self.cargo), ("python3", self.python3)]
            .into_iter()
            .filter(|(_, present)| !present)
            .map(|(name, _)| name)
            .collect()
    }
}

/// Something wrong with the VM's state, and how to repair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub account: String,
    pub problem: String,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub name: String,
    pub kind: AccountKind,
    pub owner: Owner,
    pub user: Option<String>,
    /// Current generation; 0 is the base generation with no layers.
    pub generation: u64,
    /// Every generation a rollback can reach, oldest first, starting with the base generation 0.
    pub generations: Vec<u64>,
    /// Component name to layer hash for the current generation.
    pub components: BTreeMap<String, String>,
    /// `systemctl is-active` of the session unit (`vm` reports `n/a`).
    pub session: String,
    pub seat: Option<String>,
    /// The outputs the account was created with (`--outputs`), left to right.
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    /// How the running session differs from a greetd session (capabilities, `SELinux` domain);
    /// `None` when it matches or is not running.
    #[serde(default)]
    pub parity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<UserspaceSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcResult {
    pub dry_run: bool,
    pub removed_layers: Vec<String>,
    /// `<account>/<generation>` of each pruned generation.
    pub removed_generations: Vec<String>,
    pub freed_bytes: u64,
    /// Worktree ids whose in-VM build dir was evicted.
    #[serde(default)]
    pub removed_targets: Vec<String>,
    #[serde(default)]
    pub freed_target_bytes: u64,
    pub storage: Storage,
    /// Unreferenced layers kept because they are younger than the minimum age.
    #[serde(default)]
    pub kept_young: Vec<KeptLayer>,
    /// The minimum age this run used, in seconds.
    #[serde(default)]
    pub min_age_seconds: u64,
}

/// An unreferenced layer gc kept for its age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLayer {
    pub hash: String,
    pub component: String,
    pub age_seconds: u64,
    /// When a gc run with the same minimum age removes it.
    pub eligible_in_seconds: u64,
}

/// An account and the files its current generation's layers add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountShowResult {
    pub account: AccountInfo,
    /// Every file and symlink, grouped by component, sorted by path.
    pub files: Vec<AccountFile>,
    /// Import provenance of the selected layers, keyed by component. Missing metadata stays
    /// missing; callers must not infer a source from the request.
    #[serde(default)]
    pub provenance: std::collections::BTreeMap<String, Provenance>,
}

/// One file a layer puts into the account's view of the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountFile {
    pub component: String,
    pub layer: String,
    /// Where the account sees it, e.g. `/usr/bin/tilo-session`.
    pub path: String,
    /// SHA-256 (hex) of a regular file's contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// A symlink's target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default)]
    pub executable: bool,
}

/// `exec` stops a command after this long by default.
pub const EXEC_TIMEOUT_SECONDS: u64 = 60;
/// `exec` stops a command whose output passes this many bytes.
pub const EXEC_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResult {
    pub daemon_version: String,
    /// The VM's CPU architecture (`x86_64`, `aarch64`).
    pub arch: String,
    #[serde(default)]
    pub base: BaseInfo,
    /// Which build and extraction tools the VM's image has.
    #[serde(default)]
    pub tools: VmTools,
    /// The boot, the kernel and the image deployments the device runs.
    #[serde(default)]
    pub boot: image::BootFacts,
    pub accounts: Vec<AccountInfo>,
    pub seat0: Option<Lease>,
    pub storage: Storage,
    pub issues: Vec<Issue>,
    /// The last time the machine booted another OS image than its accounts were made on, and which
    /// accounts kept layers built on the previous one. Absent from a daemon that does not record
    /// images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_change: Option<ImageChange>,
    /// The OS image this boot runs, as [`booted_image`] names it. Absent from a daemon that does
    /// not record images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts_image: Option<String>,
}

/// The machine booted another OS image than the one recorded at the boot before. Layers are
/// built against one image (its libraries, its protocols, its other components), so on another
/// one they may run old programs against new ones. Every account stays on its generation and
/// nothing is deleted: a layer merges when it was built against the booted base or when its
/// interface versions match the booted image's, and any other is held and reported. An unknown
/// previous image is never a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageChange {
    /// The image recorded at the boot before. `None` only in a record an
    /// older tilo-vmd wrote, which counted state from before it recorded
    /// images as a change.
    pub from: Option<String>,
    /// The booted image, as `composefs:<digest>` or `ostree:<deployment>`.
    pub to: String,
    /// RFC 3339, when the boot found the change.
    pub at: String,
    /// Accounts whose current generation was made on `from`: each stays on
    /// it, its layers kept.
    #[serde(default)]
    pub kept: Vec<AccountLayers>,
    /// Layers of any account's generations held on `to`: kept, but not merged,
    /// because neither their base image nor their interface versions match
    /// it.
    #[serde(default)]
    pub held: Vec<HeldLayer>,
    /// Accounts an older tilo-vmd put back on the image at this change, deleting their generations.
    /// tilo-vmd no longer drops anything; the field stays readable so such a record is reported
    /// as what it was.
    #[serde(default)]
    pub dropped: Vec<AccountLayers>,
    /// What that older tilo-vmd deleted of the dropped accounts' layers.
    #[serde(default)]
    pub freed_bytes: u64,
}

/// A layer kept but not merged on the booted image, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldLayer {
    /// Its content hash.
    pub layer: String,
    /// The component it carries.
    pub component: String,
    /// Why it does not fit the booted image, for people.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLayers {
    pub account: String,
    pub generation: u64,
    /// The components that generation had swapped in, by name.
    pub components: Vec<String>,
}

impl AccountLayers {
    fn components_text(&self) -> String {
        if self.components.is_empty() {
            "no components".to_owned()
        } else {
            self.components.join(", ")
        }
    }
}

/// The booted OS image, from a kernel command line: the composefs digest of the deployment
/// (`composefs=`, bootc's composefs backend) or its ostree deployment path (`ostree=`). `None`
/// for a machine booted some other way, where nothing tells two images apart.
#[must_use]
pub fn booted_image(cmdline: &str) -> Option<String> {
    cmdline.split_whitespace().find_map(|word| {
        let (key, value) = word.split_once('=')?;
        match key {
            "composefs" if !value.is_empty() => Some(format!("composefs:{value}")),
            "ostree" if !value.is_empty() => Some(format!("ostree:{value}")),
            _ => None,
        }
    })
}

impl ImageChange {
    /// One line per account: which accounts run layers built on the previous image, and how to reach
    /// either image.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.kept.is_empty() {
            lines.push(format!(
                "the OS image changed ({}): these accounts keep the layers built on the previous image; tilo-vm rollback --account <account> --to 0 runs the image as installed, a new swap builds against it",
                self.at
            ));
            for account in &self.kept {
                lines.push(format!(
                    "  account {} stays on generation {} ({})",
                    account.account,
                    account.generation,
                    account.components_text()
                ));
            }
        }
        if !self.held.is_empty() {
            lines.push(
                "held, kept but not merged, until a new swap or a rollback replaces them:".into(),
            );
            for layer in &self.held {
                lines.push(format!(
                    "  {} {}: {}",
                    layer.component,
                    layer.layer.get(..12).unwrap_or(&layer.layer),
                    layer.reason
                ));
            }
        }
        if !self.dropped.is_empty() {
            lines.push(format!(
                "the OS image changed ({}): an older tilo-vmd dropped the layers built for the previous image{}",
                self.at,
                if self.freed_bytes > 0 {
                    format!(" and deleted {} bytes of them", self.freed_bytes)
                } else {
                    String::new()
                }
            ));
            for account in &self.dropped {
                lines.push(format!(
                    "  account {} left generation {} ({})",
                    account.account,
                    account.generation,
                    account.components_text()
                ));
            }
        }
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCreateResult {
    pub account: AccountInfo,
    pub created: bool,
    pub actions: Vec<Action>,
    /// Set exactly when [`AccountCreate::identity`] was asked for; a daemon
    /// that leaves it out does not know the flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<TestIdentity>,
}

/// The throwaway test identity an account's user holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestIdentity {
    /// Minted by this create, rather than kept from an earlier one.
    pub created: bool,
    pub display_name: String,
    pub throwaway: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRmResult {
    pub removed: bool,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerImportResult {
    pub hash: String,
    /// True when an identical layer was already present.
    pub existed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerInfo {
    pub hash: String,
    pub provenance: Provenance,
}

/// The installer's canonical package selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSelection {
    pub id: String,
    pub root: String,
    pub runtime: String,
    pub preserved: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub account: String,
    pub generation: u64,
    /// False when the request matched the current state and nothing was done.
    pub changed: bool,
    pub policy: Policy,
    pub actions: Vec<Action>,
    /// Signed package selections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cohort_selection: Vec<CohortSelection>,
    /// Non-fatal findings, e.g. a layer built against a different base image.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// With [`Policy::Reboot`]: what the next boot starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<BootChange>,
}

/// The boot entry a kernel swap or rollback prepared.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BootChange {
    /// Kernel release the next boot runs; `None` for the image's own kernel.
    pub kver: Option<String>,
    /// Boot loader entry the next boot starts (only once for a new kernel,
    /// until [`BootConfirm`]).
    pub entry: Option<String>,
    /// The default entry, which a failed boot falls back to.
    pub fallback: Option<String>,
    /// `systemd-boot` or `grub`.
    pub loader: String,
    /// How the initramfs was made (the image's dracut arguments, or the distribution's defaults);
    /// `None` when an existing one was reused.
    #[serde(default)]
    pub initramfs: Option<String>,
    #[serde(default)]
    pub initramfs_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootConfirmResult {
    /// `uname -r` of the running kernel.
    pub running: String,
    /// `uname -v` of the running kernel.
    pub version: String,
    /// The kernel release the last swap or rollback installed; `None` when it put the image's
    /// kernel back.
    pub expected: Option<String>,
    /// True when the running kernel is the expected one; its entry is now
    /// the default.
    pub confirmed: bool,
    /// The default boot entry after this call.
    pub default_entry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsResult {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatResult {
    pub account: String,
    pub seat: String,
    pub changed: bool,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotResult {
    /// Standard base64 of a PNG.
    pub png: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputResult {
    pub account: String,
    /// The logind seat the devices belong to (`seat-<account>`, or `seat0` while the account holds it).
    pub seat: String,
    /// Kernel input events written, `SYN` reports included.
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstallResult {
    pub account: String,
    /// `[package].id` of the installed app, when tilo-install reported it.
    pub id: Option<String>,
    pub version: Option<String>,
    /// What tilo-install printed.
    pub output: String,
}

/// What `build` builds from the synced tree `SRC_DIR/<worktree>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildTarget {
    /// A manifest component, imported as a layer. `profile` overrides the
    /// manifest's cargo profile (`swap --profile dev`).
    Component {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    /// An app under `apps/<dir>`, packaged as a self-signed `.tapp` and
    /// installed into `account`'s app store.
    App { dir: String, account: String },
}

/// Build a component (or package and install an app) from a synced worktree; the tools' output
/// streams as the step's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// Directory name under [`SRC_DIR`]; one per host worktree.
    pub worktree: String,
    pub target: BuildTarget,
    /// What the host knows (commit, dirty hash, `Cargo.lock` hash); the VM
    /// adds its toolchain, target and builder image.
    pub provenance: Provenance,
    #[serde(default)]
    pub owner: Owner,
    /// Colour and progress bars in the streamed output.
    #[serde(default)]
    pub color: bool,
}

/// Pull a published image and turn one component's files into a layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiLayer {
    pub image: String,
    pub component: String,
    /// The component's install paths from the manifest; `dir/` is a tree.
    pub paths: Vec<String>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobOutput {
    Layer {
        hash: String,
        existed: bool,
        provenance: Box<Provenance>,
        /// Seconds per phase (`build`, `assemble`, `import`, `pull`, ...).
        #[serde(default)]
        phases: BTreeMap<String, f64>,
        /// `sccache_hits` and `sccache_misses` of this build.
        #[serde(default)]
        cache: BTreeMap<String, u64>,
    },
    App {
        #[serde(flatten)]
        result: AppInstallResult,
        #[serde(default)]
        phases: BTreeMap<String, f64>,
    },
}

/// Run one command as an account sees the system: in its session's mount namespace, as its user.
/// Stderr streams as the step's progress; the run is bounded in time and output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exec {
    pub account: String,
    pub command: Vec<String>,
    /// Seconds before the command is killed (exit 124).
    #[serde(default = "exec_timeout")]
    pub timeout: u64,
    /// Output bytes after which the command is killed (exit 125).
    #[serde(default = "exec_max_bytes")]
    pub max_bytes: u64,
}

fn exec_timeout() -> u64 {
    EXEC_TIMEOUT_SECONDS
}

fn exec_max_bytes() -> u64 {
    EXEC_MAX_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    /// The command's exit status, 124 after the time limit, 125 past the output limit.
    pub code: u8,
    /// Standard base64 of what the command wrote to stdout.
    pub stdout: String,
}

/// Install distribution packages into the VM's `/usr` with its package manager (a transient
/// overlay on bootc images); the package manager's output streams as the step's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPackages {
    /// Package names, e.g. `xterm`, `zenity`.
    pub packages: Vec<String>,
    /// Seconds before the install is stopped (exit 124).
    #[serde(default = "install_timeout")]
    pub timeout: u64,
}

fn install_timeout() -> u64 {
    INSTALL_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPackagesResult {
    /// The package manager's exit status, 124 after the time limit.
    pub code: u8,
}

/// Lay a worktree out under [`SRC_DIR`]`/<worktree>` from a [`TreeManifest`] in the device's
/// content store, whose files are blobs there too. Files the manifest does not name are deleted;
/// `target/` stays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSync {
    pub worktree: String,
    /// Blake3 hex of the manifest's JSON in the content store.
    pub manifest: String,
}

/// A worktree's files and symlinks, paths relative to its root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeManifest {
    pub entries: Vec<TreeFile>,
    #[serde(default)]
    pub symlinks: Vec<TreeSymlink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeFile {
    pub path: String,
    /// Unix permission bits.
    pub mode: u32,
    /// Blake3 hex of the contents in the content store.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSymlink {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSyncResult {
    /// Files written because they were missing or differed.
    pub written: u64,
    /// Files and symlinks deleted because the manifest no longer names them.
    pub removed: u64,
}

/// Account names become Linux user names (`tds-<name>`, at most 32 bytes) and systemd instance
/// names, so they are short, lowercase and dash-safe.
pub fn valid_account_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=28).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.ends_with('-')
}

/// Distribution package names travel on a root command line, so only the characters package
/// names use get through: `gtk4-devel-tools`, `libstdc++`, `python3.13`. No option can start one.
pub fn valid_package_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

/// How long `install-packages` may take by default.
pub const INSTALL_TIMEOUT_SECONDS: u64 = 1800;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_cannot_smuggle_options_or_shell() {
        for good in [
            "xterm",
            "gtk4-devel-tools",
            "libstdc++",
            "python3.13",
            "a_b",
        ] {
            assert!(valid_package_name(good), "{good}");
        }
        for bad in [
            "",
            "-y",
            "--installroot=/",
            "a b",
            "a;b",
            "$(x)",
            "a/b",
            "*",
        ] {
            assert!(!valid_package_name(bad), "{bad}");
        }
    }

    #[test]
    fn account_create_carries_outputs_as_spec_strings() {
        let request = AccountCreate {
            name: "a".into(),
            kind: AccountKind::Headful,
            owner: Owner::default(),
            outputs: display::parse_outputs("2560x1600@2,1920x1080").unwrap(),
            identity: false,
            device_identity: false,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value["outputs"],
            serde_json::json!(["2560x1600@2", "1920x1080@1"])
        );
        assert_eq!(
            serde_json::from_value::<AccountCreate>(value).unwrap(),
            request
        );
        let bare: AccountCreate = serde_json::from_str(r#"{"name":"a","kind":"headful"}"#).unwrap();
        assert!(bare.outputs.is_empty());
        assert!(!bare.identity);
    }

    #[test]
    fn every_step_is_one_distinct_action_with_its_wire_form() {
        let mut ids = ACTIONS.to_vec();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len());
        assert_eq!(<TreeSync as Call>::ACTION, "tree-sync");
        assert!(
            ACTIONS
                .iter()
                .all(|id| id.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
        );

        let job = Build {
            worktree: "wt-1234".into(),
            target: BuildTarget::App {
                dir: "notes".into(),
                account: "a".into(),
            },
            provenance: Provenance::local("app:notes"),
            owner: Owner::default(),
            color: false,
        };
        let text = serde_json::to_string(&job).unwrap();
        assert!(text.contains(r#""target":{"kind":"app","dir":"notes","account":"a"}"#));
        assert_eq!(serde_json::from_str::<Build>(&text).unwrap(), job);

        let source = serde_json::to_value(LayerSource::Content { hash: "ab".into() }).unwrap();
        assert_eq!(source, serde_json::json!({"content":{"hash":"ab"}}));
        let exec: Exec = serde_json::from_str(r#"{"account":"a","command":["true"]}"#).unwrap();
        assert_eq!(
            (exec.timeout, exec.max_bytes),
            (EXEC_TIMEOUT_SECONDS, EXEC_MAX_BYTES)
        );
    }

    #[test]
    fn network_wake_refuses_fields_it_does_not_declare() {
        let request = NetworkWake {
            profile: "11111111-1111-4111-8111-111111111111".into(),
            enabled: Some(true),
        };
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<NetworkWake>(wire.clone()).unwrap(),
            request
        );
        let mut injected = wire;
        injected["command"] = serde_json::json!("unrelated-tool");
        assert!(serde_json::from_value::<NetworkWake>(injected).is_err());
    }

    #[test]
    fn account_names_and_base_info_match_what_the_image_writes() {
        // Phase: account_names_fit_user_names.
        {
            assert!(valid_account_name("vm-dev-loop"));
            assert!(!valid_account_name("Upper"));
            assert!(!valid_account_name("-lead"));
            assert!(!valid_account_name("trail-"));
            assert!(!valid_account_name(&"a".repeat(29)));
            assert!(!valid_account_name("a/b"));
        }

        // Phase: base_info_reads_what_the_dev_image_writes.
        {
            // The shape os/scripts/finalize-dev-image.py writes: a dev image is
            // its own builder, so `builder_image_digest` is null.
            let base: BaseInfo = serde_json::from_str(
                r#"{
                  "image_digest": "sha256:aaaa",
                  "builder_image_digest": null,
                  "toolchain": "rustc 1.97.1 (abc 2026-08-01)",
                  "target": "x86_64-unknown-linux-gnu",
                  "cargo_lock_hash": "0123abcd",
                  "base_image": "registry.tilo.so/tilo/tilo:main-1a2b3c4"
                }"#,
            )
            .unwrap();
            assert_eq!(base.builder_image_digest, None);
            assert_eq!(base.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
            assert_eq!(base.cargo_lock_hash.as_deref(), Some("0123abcd"));
            assert_eq!(
                base.toolchain.as_deref(),
                Some("rustc 1.97.1 (abc 2026-08-01)")
            );
        }
    }
}
