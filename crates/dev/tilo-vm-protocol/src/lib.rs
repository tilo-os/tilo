//! The contract between the host CLI `tilo-vm` and the guest daemon `tilo-vmd`.
//!
//! Transport is newline-delimited JSON-RPC 2.0 over the stdio of
//! `ssh <vm> tilo-vmd rpc`: one request object per line in, one response
//! object per line out, strictly in order. The first call on every connection
//! is [`Hello`], which carries [`PROTOCOL_VERSION`]; a daemon refuses every
//! other method from a client whose version differs, so an old CLI never
//! half-drives a new daemon.
//!
//! Layer bytes prefer an optional bounded binary substream ([`LayerUploadBinary`])
//! with count and SHA-256 verification. Older protocol-8 peers retain base64
//! chunks ([`LayerUploadChunk`]); all other operations remain ordered JSON-RPC.
//!
//! Long jobs that stream output (in-VM builds, CI layer extraction) do not
//! use the RPC loop: `tilo-vm` runs `ssh <vm> tilo-vmd build` (or
//! `ci-layer`) with a [`BuildJob`] or [`CiJob`] on stdin. The job's own
//! output streams on stderr and its outcome is one [`JobLine`] on stdout.

pub mod binary;
pub mod client;
pub mod disk_unlock;
pub mod display;
pub mod input;
pub mod layer_hash;
pub mod manifest;

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use client::{Client, ClientError};
pub use disk_unlock::{DeviceActions, DeviceUnlockEnrol, DeviceUnlockEnrolResult};
pub use display::OutputSpec;
pub use input::{Button, InputAction, Stick};
pub use manifest::{ComponentInfo, Manifest, Scope};

/// Bumped on any incompatible change to a request, response or semantics.
pub const PROTOCOL_VERSION: u32 = 9;

/// Where in-VM builds keep one synced source tree per host worktree; its
/// `target/` is the worktree's own, evictable build dir.
pub const SRC_DIR: &str = "/var/lib/tilo-vm/src";

/// Decoded bytes per upload chunk. Small enough to keep a line bounded,
/// large enough that per-line overhead is noise.
pub const UPLOAD_CHUNK_BYTES: usize = 1 << 20;

/// The name of the pseudo-slot whose layers merge into the VM's own `/usr`.
pub const VM_SLOT: &str = "vm";

/// The name of the pseudo-slot that is the machine's own session: the
/// session an image runs for its account without a greeter (a board's
/// always-on session). Only images that run one have it.
pub const DEVICE_SLOT: &str = "device";

/// How long the keep-awake lease lasts after the last request: every
/// connection's [`Hello`] renews it, so a machine someone drives does not
/// suspend under them, and one they stopped driving can again.
pub const AWAKE_IDLE_SECONDS: u64 = 30 * 60;

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// One response line: exactly one of `result` or `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// An error the daemon reports. `hint` is always the next thing to try.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub see: Option<String>,
}

/// Error codes. The JSON-RPC reserved range is used only for transport faults.
pub mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const VERSION_MISMATCH: i64 = 1;
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

/// Every method, tagged by its wire name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "status")]
    Status(Status),
    #[serde(rename = "slot.create")]
    SlotCreate(SlotCreate),
    #[serde(rename = "slot.list")]
    SlotList(SlotList),
    #[serde(rename = "slot.restart")]
    SlotRestart(SlotRestart),
    #[serde(rename = "slot.rm")]
    SlotRm(SlotRm),
    #[serde(rename = "slot.show")]
    SlotShow(SlotShow),
    #[serde(rename = "layer.upload.begin")]
    LayerUploadBegin(LayerUploadBegin),
    #[serde(rename = "layer.upload.binary")]
    LayerUploadBinary(LayerUploadBinary),
    #[serde(rename = "layer.upload.chunk")]
    LayerUploadChunk(LayerUploadChunk),
    #[serde(rename = "layer.import")]
    LayerImport(LayerImport),
    #[serde(rename = "layer.list")]
    LayerList(LayerList),
    #[serde(rename = "layer.verify")]
    LayerVerify(LayerVerify),
    #[serde(rename = "swap")]
    Swap(Swap),
    #[serde(rename = "rollback")]
    Rollback(Rollback),
    #[serde(rename = "logs")]
    Logs(Logs),
    #[serde(rename = "seat.claim")]
    SeatClaim(SeatClaim),
    #[serde(rename = "seat.release")]
    SeatRelease(SeatRelease),
    #[serde(rename = "shot")]
    Shot(Shot),
    #[serde(rename = "control")]
    Control(Control),
    #[serde(rename = "gc")]
    Gc(Gc),
    #[serde(rename = "input")]
    Input(Input),
    #[serde(rename = "app.install")]
    AppInstall(AppInstall),
    #[serde(rename = "boot.confirm")]
    BootConfirm(BootConfirm),
    #[serde(rename = "awake")]
    Awake(Awake),
    #[serde(rename = "device.unlock.enrol")]
    DeviceUnlockEnrol(disk_unlock::DeviceUnlockEnrol),
    #[serde(rename = "device.actions")]
    DeviceActions(disk_unlock::DeviceActions),
}

impl Request {
    pub fn method(&self) -> &'static str {
        match self {
            Self::DeviceUnlockEnrol(_) => "device.unlock.enrol",
            Self::DeviceActions(_) => "device.actions",
            Self::Hello(_) => "hello",
            Self::Status(_) => "status",
            Self::SlotCreate(_) => "slot.create",
            Self::SlotList(_) => "slot.list",
            Self::SlotRestart(_) => "slot.restart",
            Self::SlotRm(_) => "slot.rm",
            Self::SlotShow(_) => "slot.show",
            Self::LayerUploadBegin(_) => "layer.upload.begin",
            Self::LayerUploadBinary(_) => "layer.upload.binary",
            Self::LayerUploadChunk(_) => "layer.upload.chunk",
            Self::LayerImport(_) => "layer.import",
            Self::LayerList(_) => "layer.list",
            Self::LayerVerify(_) => "layer.verify",
            Self::Swap(_) => "swap",
            Self::Rollback(_) => "rollback",
            Self::Logs(_) => "logs",
            Self::SeatClaim(_) => "seat.claim",
            Self::SeatRelease(_) => "seat.release",
            Self::Shot(_) => "shot",
            Self::Control(_) => "control",
            Self::Gc(_) => "gc",
            Self::Input(_) => "input",
            Self::AppInstall(_) => "app.install",
            Self::BootConfirm(_) => "boot.confirm",
            Self::Awake(_) => "awake",
        }
    }
}

/// A typed request: its params struct names its result type.
pub trait Call: Into<Request> {
    type Output: DeserializeOwned + Serialize;
}

macro_rules! call {
    ($($params:ident => $output:ty),* $(,)?) => {$(
        impl From<$params> for Request {
            fn from(params: $params) -> Self {
                Self::$params(params)
            }
        }
        impl Call for $params {
            type Output = $output;
        }
    )*};
}

call! {
    Hello => HelloResult,
    Status => StatusResult,
    SlotCreate => SlotCreateResult,
    SlotList => Vec<SlotInfo>,
    SlotRestart => ApplyResult,
    SlotRm => SlotRmResult,
    SlotShow => SlotShowResult,
    LayerUploadBegin => LayerUploadBeginResult,
    LayerUploadBinary => LayerUploadBeginResult,
    LayerUploadChunk => LayerUploadChunkResult,
    LayerImport => LayerImportResult,
    LayerList => Vec<LayerInfo>,
    LayerVerify => LayerInfo,
    Swap => ApplyResult,
    Rollback => ApplyResult,
    Logs => LogsResult,
    SeatClaim => SeatResult,
    SeatRelease => SeatResult,
    Shot => ShotResult,
    Control => ControlResult,
    Gc => GcResult,
    Input => InputResult,
    AppInstall => AppInstallResult,
    BootConfirm => BootConfirmResult,
    Awake => AwakeLease,
    DeviceUnlockEnrol => DeviceUnlockEnrolResult,
    DeviceActions => serde_json::Value,
}

// ---- requests ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub client_version: String,
}

/// Status of one slot, or of every slot when `slot` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub slot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotCreate {
    pub name: String,
    pub kind: SlotKind,
    #[serde(default)]
    pub owner: Owner,
    /// Headful only: one vkms connector per output, left to right, with
    /// this mode and scale applied through the display settings. Empty
    /// means one connector at the compositor's default mode and scale.
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    /// Give the slot's user a throwaway test identity through the session's
    /// dev-only control op, and start the session with fresh desktop state.
    #[serde(default)]
    pub identity: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotList {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRestart {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRm {
    pub name: String,
}

/// One slot with every file its current generation adds, and where each
/// comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotShow {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadBegin {}

/// Optional protocol-8 upgrade: ready response, bounded binary frames, then
/// a same-id byte-count response. Use `Client::upload` after explicit transport
/// opt-in; a bare RPC call cannot perform the substream. Older peers reject it
/// before consuming bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadBinary {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadChunk {
    pub upload: String,
    /// Standard base64 of at most [`UPLOAD_CHUNK_BYTES`] bytes.
    pub data: String,
}

/// Where the bytes of a layer come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerSource {
    /// A completed upload: an uncompressed tar of the layer's `usr/` tree.
    Upload(String),
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

/// Verify stored content before reusing a layer. Optional on protocol 8 peers;
/// clients must stream normally when this method is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerVerify {
    pub hash: String,
}

/// Replace (or add) one component's layer in a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Swap {
    pub slot: String,
    pub component: String,
    pub layer: String,
    /// Scope from the client's manifest, which matches the build. When absent
    /// the daemon falls back to its own manifest, then to session scope.
    #[serde(default)]
    pub info: Option<ComponentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    pub slot: String,
    /// Target generation; the one before the current generation when absent.
    #[serde(default)]
    pub to: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logs {
    pub slot: String,
    pub lines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatClaim {
    pub slot: String,
    #[serde(default)]
    pub owner: Owner,
    /// Take seat0 even when another slot holds the lease.
    #[serde(default)]
    pub steal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRelease {
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shot {
    pub slot: String,
}

/// Arguments passed verbatim to the in-VM `tilo-control` for this slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub slot: String,
    pub args: Vec<String>,
}

/// Remove what no kept generation references, and evict build dirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gc {
    /// Generations to keep per slot, besides the current one.
    pub keep: u32,
    pub dry_run: bool,
    /// In-VM build dirs (one per host worktree) to keep, most recently
    /// used first; older ones are evicted.
    #[serde(default)]
    pub keep_targets: Option<u32>,
    /// Evict further build dirs, least recently used first, until the VM
    /// has this much free space.
    #[serde(default)]
    pub free_goal_bytes: Option<u64>,
    /// Unreferenced layers younger than this stay (an import whose swap has
    /// not happened yet looks unreferenced); the daemon's default when absent.
    #[serde(default)]
    pub min_age_seconds: Option<u64>,
}

/// Drive a headful slot's seat through its own uinput keyboard and pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub slot: String,
    pub actions: Vec<InputAction>,
}

/// Install a `.tapp` into the slot user's app store (`tilo-install
/// install --allow-self-signed --force`, run as that user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstall {
    pub slot: String,
    /// An upload of the `.tapp` file, or a path to one inside the VM.
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

/// Change the keep-awake lease beyond what every request does by itself
/// (hold the machine awake until [`AWAKE_IDLE_SECONDS`] after it), or with
/// neither field set just report it.
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

// ---- shared vocabulary ------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    Headful,
    Headless,
    /// Only the reserved [`VM_SLOT`] has this kind.
    Vm,
    /// Only the reserved [`DEVICE_SLOT`] has this kind.
    Device,
}

impl SlotKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headful => "headful",
            Self::Headless => "headless",
            Self::Vm => "vm",
            Self::Device => "device",
        }
    }
}

/// Who created a slot, so humans can tell agents' slots apart.
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
}

fn local() -> String {
    "local".into()
}

impl Provenance {
    /// A component copied out of CI's image of a build, for `arch`:
    /// `requested` is what the client knew (component, profile, features,
    /// the commit it asked for); `digest` and `revision` are the pulled
    /// image's. The same record whether the VM or the host pulled it.
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

    /// A local build of `component` with nothing else known yet.
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
        }
    }
}

/// What the VM's base image was built with, from the dev image's
/// `/usr/share/tilo-vm/base.json`. Layers are checked against it, field for
/// field in the form [`Provenance`] records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseInfo {
    #[serde(default)]
    pub image_digest: Option<String>,
    /// `None` in a dev image: it is its own builder, identified by
    /// `toolchain`, `target` and `cargo_lock_hash` instead.
    #[serde(default)]
    pub builder_image_digest: Option<String>,
    /// `rustc --version` of the image's toolchain.
    #[serde(default)]
    pub toolchain: Option<String>,
    /// Rust target triple of the image's toolchain (`rustc -vV`'s host).
    #[serde(default)]
    pub target: Option<String>,
    /// SHA-256 (hex) of the `Cargo.lock` the image was built from.
    #[serde(default)]
    pub cargo_lock_hash: Option<String>,
}

/// Who holds seat0, since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub slot: String,
    pub owner: Owner,
    pub since: String,
}

/// Bytes on the VM disk that holds the state root.
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
    /// The slot's session logs out and a fresh login starts.
    SessionRestart,
    /// `systemd-sysext refresh`, then the affected system units restart.
    VmRefresh,
    /// A kernel changed: VM `/usr` refreshed, the new kernel installed as
    /// the next boot entry ([`ApplyResult::boot`]); the client reboots the VM
    /// and confirms the boot ([`BootConfirm`]).
    Reboot,
}

/// One observable step the daemon took, reported back so the CLI can say
/// what happened as a consequence.
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
    /// The slot user's desktop layout removed, so the next session starts
    /// with none.
    ResetDesktopState,
    /// The desktop of a login restarted in place (`tilo-session` on
    /// SIGUSR1): the login, its user manager and its services stayed.
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
    /// A kernel's vmlinuz and initramfs copied to `/boot` with a boot loader
    /// entry of its own.
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

// ---- results ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: u32,
    pub daemon_version: String,
    pub root: String,
    /// The VM's CPU architecture (`x86_64`, `aarch64`).
    pub arch: String,
    #[serde(default)]
    pub base: BaseInfo,
    /// State that needs attention: half-applied transitions, dead sessions.
    #[serde(default)]
    pub issues: Vec<Issue>,
    /// Which build and extraction tools the VM's image has.
    #[serde(default)]
    pub tools: VmTools,
    /// The keep-awake lease, as this hello renewed it.
    #[serde(default)]
    pub awake: Option<AwakeLease>,
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

/// Tools found on the VM's `PATH`, which decide where builds and CI layer
/// extraction can run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per tool"
)]
pub struct VmTools {
    pub cargo: bool,
    pub rsync: bool,
    pub python3: bool,
    pub sccache: bool,
    pub podman: bool,
}

impl VmTools {
    /// Everything the in-VM builder needs.
    pub fn can_build(&self) -> bool {
        self.cargo && self.rsync && self.python3
    }

    /// Names of the tools the in-VM builder lacks.
    pub fn missing_for_build(&self) -> Vec<&'static str> {
        [
            ("cargo", self.cargo),
            ("rsync", self.rsync),
            ("python3", self.python3),
        ]
        .into_iter()
        .filter(|(_, present)| !present)
        .map(|(name, _)| name)
        .collect()
    }
}

/// Something wrong with the VM's state, and how to repair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub slot: String,
    pub problem: String,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotInfo {
    pub name: String,
    pub kind: SlotKind,
    pub owner: Owner,
    pub user: Option<String>,
    /// Current generation; 0 is the base generation with no layers.
    pub generation: u64,
    /// Every generation a rollback can reach, oldest first, starting with
    /// the base generation 0.
    pub generations: Vec<u64>,
    /// Component name to layer hash for the current generation.
    pub components: BTreeMap<String, String>,
    /// `systemctl is-active` of the session unit (`vm` reports `n/a`).
    pub session: String,
    pub seat: Option<String>,
    /// The outputs the slot was created with (`--outputs`), left to right.
    #[serde(default)]
    pub outputs: Vec<OutputSpec>,
    /// How the running session differs from a greetd session (capabilities,
    /// `SELinux` domain); `None` when it matches or is not running.
    #[serde(default)]
    pub parity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcResult {
    pub dry_run: bool,
    pub removed_layers: Vec<String>,
    /// `<slot>/<generation>` of each pruned generation.
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

/// A slot and the files its current generation's layers add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotShowResult {
    pub slot: SlotInfo,
    /// Every file and symlink, grouped by component, sorted by path.
    pub files: Vec<SlotFile>,
}

/// One file a layer puts into the slot's view of the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFile {
    pub component: String,
    pub layer: String,
    /// Where the slot sees it, e.g. `/usr/bin/shell`.
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

/// `tilo-vmd exec` stops a command after this long by default.
pub const EXEC_TIMEOUT_SECONDS: u64 = 60;
/// `tilo-vmd exec` stops a command whose output passes this many bytes.
pub const EXEC_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub protocol: u32,
    pub daemon_version: String,
    pub slots: Vec<SlotInfo>,
    pub seat0: Option<Lease>,
    pub storage: Storage,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotCreateResult {
    pub slot: SlotInfo,
    pub created: bool,
    pub actions: Vec<Action>,
    /// Set exactly when [`SlotCreate::identity`] was asked for; a daemon
    /// that leaves it out does not know the flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<TestIdentity>,
}

/// The throwaway test identity a slot's user holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestIdentity {
    /// Minted by this create, rather than kept from an earlier one.
    pub created: bool,
    pub display_name: String,
    pub throwaway: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRmResult {
    pub removed: bool,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadBeginResult {
    pub upload: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadChunkResult {
    pub received: u64,
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

/// Canonical installer selection, separate from the protocol-v8 action vocabulary.
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
    pub slot: String,
    pub generation: u64,
    /// False when the request matched the current state and nothing was done.
    pub changed: bool,
    pub policy: Policy,
    pub actions: Vec<Action>,
    /// Signed package selections; older protocol-v8 clients ignore this field.
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
    /// How the initramfs was made (the image's dracut arguments, or the
    /// distribution's defaults), `None` when an existing one was reused.
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
    /// The kernel release the last swap or rollback installed; `None` when
    /// it put the image's kernel back.
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
    pub slot: String,
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
    pub slot: String,
    /// The logind seat the devices belong to (`seat-<slot>`, or `seat0`
    /// while the slot holds it).
    pub seat: String,
    /// Kernel input events written, `SYN` reports included.
    pub events: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstallResult {
    pub slot: String,
    /// `[package].id` of the installed app, when tilo-install reported it.
    pub id: Option<String>,
    pub version: Option<String>,
    /// What tilo-install printed.
    pub output: String,
}

// ---- streaming jobs ---------------------------------------------------------

/// What `tilo-vmd build` builds from the synced tree `SRC_DIR/<worktree>`.
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
    /// installed into `slot`'s app store.
    App { dir: String, slot: String },
}

/// Stdin of `tilo-vmd build`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildJob {
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

/// Stdin of `tilo-vmd ci-layer`: pull a published image and turn one
/// component's files into a layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiJob {
    pub image: String,
    pub component: String,
    /// The component's install paths from the manifest; `dir/` is a tree.
    pub paths: Vec<String>,
    pub provenance: Provenance,
}

/// What a job produced.
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

/// The one stdout line of a job: exactly one of `result` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JobOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Prefix of the job line, so it is found among anything else a remote
/// shell prints on stdout.
pub const JOB_LINE_PREFIX: &str = "tilo-vmd-job: ";

impl JobLine {
    /// The last job line in `stdout`.
    pub fn find(stdout: &str) -> Option<Self> {
        stdout
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(JOB_LINE_PREFIX))
            .and_then(|json| serde_json::from_str(json).ok())
    }

    pub fn render(&self) -> String {
        format!(
            "{JOB_LINE_PREFIX}{}",
            serde_json::to_string(self).unwrap_or_default()
        )
    }
}

/// Slot names become Linux user names (`tds-<name>`, at most 32 bytes)
/// and systemd instance names, so they are short, lowercase and dash-safe.
pub fn valid_slot_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=28).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.ends_with('-')
}

/// Distribution package names travel on a root command line, so only the
/// characters package names use get through: `gtk4-devel-tools`, `libfoo++`,
/// `python3.13`. No option can start one.
pub fn valid_package_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

/// How long `tilo-vmd install-packages` may take by default.
pub const INSTALL_TIMEOUT_SECONDS: u64 = 1800;

#[cfg(test)]
mod tests {
    use super::*;

    /// The v8 `ApplyResult` wire shape before optional cohort selection receipts.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct LegacyApplyResult {
        slot: String,
        generation: u64,
        changed: bool,
        policy: Policy,
        actions: Vec<Action>,
        #[serde(default)]
        warnings: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boot: Option<BootChange>,
    }

    #[test]
    fn cohort_selection_receipts_preserve_protocol_v8_apply_decoding() {
        let legacy = LegacyApplyResult {
            slot: "device".into(),
            generation: 40,
            changed: true,
            policy: Policy::SessionRestart,
            actions: vec![Action::RestartDesktop {
                user: "owner".into(),
            }],
            warnings: vec![],
            boot: None,
        };
        let old_wire = serde_json::to_value(&legacy).unwrap();
        let mut current: ApplyResult = serde_json::from_value(old_wire.clone()).unwrap();
        assert!(current.cohort_selection.is_empty());
        assert_eq!(serde_json::to_value(&current).unwrap(), old_wire);
        current.cohort_selection.push(CohortSelection {
            id: "so.tilo.settings".into(),
            root: "signed-root".into(),
            runtime: "matched-sdk".into(),
            preserved: false,
            pinned: true,
        });
        let new_wire = serde_json::to_vec(&current).unwrap();
        let decoded: LegacyApplyResult = serde_json::from_slice(&new_wire).unwrap();
        assert_eq!(decoded, legacy);
        assert_eq!(
            serde_json::from_slice::<ApplyResult>(&new_wire).unwrap(),
            current
        );
    }

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
    fn slot_create_carries_outputs_as_spec_strings() {
        let request = Request::SlotCreate(SlotCreate {
            name: "a".into(),
            kind: SlotKind::Headful,
            owner: Owner::default(),
            outputs: display::parse_outputs("2560x1600@2,1920x1080").unwrap(),
            identity: false,
        });
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value["params"]["outputs"],
            serde_json::json!(["2560x1600@2", "1920x1080@1"])
        );
        assert_eq!(serde_json::from_value::<Request>(value).unwrap(), request);
        // A client that sends no outputs gets the single default connector.
        let bare: SlotCreate = serde_json::from_str(r#"{"name":"a","kind":"headful"}"#).unwrap();
        assert!(bare.outputs.is_empty());
        // Nor a test identity.
        assert!(!bare.identity);
    }

    /// Request lines are flat JSON-RPC, method names match serde tags, and job lines are found among other
    /// output.
    #[test]
    fn the_vm_protocol_wire_is_flat_tagged_and_found_in_output() {
        // Phase: request_line_is_flat_json_rpc.
        {
            let envelope = RequestEnvelope {
                jsonrpc: "2.0".into(),
                id: 7,
                request: Request::SlotRestart(SlotRestart { name: "a".into() }),
            };
            let line = serde_json::to_string(&envelope).unwrap();
            assert_eq!(
                line,
                r#"{"jsonrpc":"2.0","id":7,"method":"slot.restart","params":{"name":"a"}}"#
            );
            let back: RequestEnvelope = serde_json::from_str(&line).unwrap();
            assert_eq!(back, envelope);
        }

        // Phase: method_names_match_serde_tags.
        {
            let requests: Vec<Request> = vec![
                Hello {
                    protocol: 1,
                    client_version: String::new(),
                }
                .into(),
                Status::default().into(),
                SlotList {}.into(),
                SlotShow { name: "a".into() }.into(),
                LayerList {}.into(),
                Swap {
                    slot: "a".into(),
                    component: "c".into(),
                    layer: "h".into(),
                    info: None,
                }
                .into(),
                Control {
                    slot: "a".into(),
                    args: vec![],
                }
                .into(),
            ];
            for request in requests {
                let value = serde_json::to_value(&request).unwrap();
                assert_eq!(value["method"], request.method());
            }
        }

        // Phase: job_lines_are_found_among_other_output.
        {
            let line = JobLine {
                result: None,
                error: Some(RpcError {
                    code: code::TOOL,
                    message: "no cargo".into(),
                    hint: "h".into(),
                    see: None,
                }),
            };
            let stdout = format!("motd noise\n{}\ntrailing\n", line.render());
            assert_eq!(JobLine::find(&stdout), Some(line));
            assert_eq!(JobLine::find("nothing here"), None);
            let job = BuildJob {
                worktree: "wt-1234".into(),
                target: BuildTarget::App {
                    dir: "notes".into(),
                    slot: "a".into(),
                },
                provenance: Provenance::local("app:notes"),
                owner: Owner::default(),
                color: false,
            };
            let text = serde_json::to_string(&job).unwrap();
            assert!(text.contains(r#""target":{"kind":"app","dir":"notes","slot":"a"}"#));
            assert_eq!(serde_json::from_str::<BuildJob>(&text).unwrap(), job);
        }
    }

    /// Slot names fit user names and base info reads what the dev image writes.
    #[test]
    fn slot_names_and_base_info_match_what_the_image_writes() {
        // Phase: slot_names_fit_user_names.
        {
            assert!(valid_slot_name("vm-dev-loop"));
            assert!(!valid_slot_name("Upper"));
            assert!(!valid_slot_name("-lead"));
            assert!(!valid_slot_name("trail-"));
            assert!(!valid_slot_name(&"a".repeat(29)));
            assert!(!valid_slot_name("a/b"));
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
