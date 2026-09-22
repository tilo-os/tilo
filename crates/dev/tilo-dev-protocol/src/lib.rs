//! The contract between the host CLI `tilo-dev` and the guest daemon `tilo-devd`.
//!
//! Transport is newline-delimited JSON-RPC 2.0 over the stdio of
//! `ssh <vm> tilo-devd rpc`: one request object per line in, one response
//! object per line out, strictly in order. The first call on every connection
//! is [`Hello`], which carries [`PROTOCOL_VERSION`]; a daemon refuses every
//! other method from a client whose version differs, so an old CLI never
//! half-drives a new daemon.
//!
//! Layer bytes travel as base64 chunks ([`LayerUploadChunk`]) so the stream
//! stays line-oriented JSON and a truncated upload is detectable.

pub mod client;
pub mod manifest;

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use client::{Client, ClientError};
pub use manifest::{ComponentInfo, Manifest, Scope};

/// Bumped on any incompatible change to a request, response or semantics.
pub const PROTOCOL_VERSION: u32 = 1;

/// Decoded bytes per upload chunk. Small enough to keep a line bounded,
/// large enough that per-line overhead is noise.
pub const UPLOAD_CHUNK_BYTES: usize = 1 << 20;

/// The name of the pseudo-slot whose layers merge into the VM's own `/usr`.
pub const VM_SLOT: &str = "vm";

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
    #[serde(rename = "layer.upload.begin")]
    LayerUploadBegin(LayerUploadBegin),
    #[serde(rename = "layer.upload.chunk")]
    LayerUploadChunk(LayerUploadChunk),
    #[serde(rename = "layer.import")]
    LayerImport(LayerImport),
    #[serde(rename = "layer.list")]
    LayerList(LayerList),
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
}

impl Request {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Status(_) => "status",
            Self::SlotCreate(_) => "slot.create",
            Self::SlotList(_) => "slot.list",
            Self::SlotRestart(_) => "slot.restart",
            Self::SlotRm(_) => "slot.rm",
            Self::LayerUploadBegin(_) => "layer.upload.begin",
            Self::LayerUploadChunk(_) => "layer.upload.chunk",
            Self::LayerImport(_) => "layer.import",
            Self::LayerList(_) => "layer.list",
            Self::Swap(_) => "swap",
            Self::Rollback(_) => "rollback",
            Self::Logs(_) => "logs",
            Self::SeatClaim(_) => "seat.claim",
            Self::SeatRelease(_) => "seat.release",
            Self::Shot(_) => "shot",
            Self::Control(_) => "control",
            Self::Gc(_) => "gc",
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
    LayerUploadBegin => LayerUploadBeginResult,
    LayerUploadChunk => LayerUploadChunkResult,
    LayerImport => LayerImportResult,
    LayerList => Vec<LayerInfo>,
    Swap => ApplyResult,
    Rollback => ApplyResult,
    Logs => LogsResult,
    SeatClaim => SeatResult,
    SeatRelease => SeatResult,
    Shot => ShotResult,
    Control => ControlResult,
    Gc => GcResult,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerUploadBegin {}

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

/// Remove what no kept generation references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gc {
    /// Generations to keep per slot, besides the current one.
    pub keep: u32,
    pub dry_run: bool,
}

// ---- shared vocabulary ------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    Headful,
    Headless,
    /// Only the reserved [`VM_SLOT`] has this kind.
    Vm,
}

impl SlotKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Headful => "headful",
            Self::Headless => "headless",
            Self::Vm => "vm",
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
/// `/usr/share/tilo-dev/base.json`. Layers are checked against it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseInfo {
    #[serde(default)]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub builder_image_digest: Option<String>,
    #[serde(default)]
    pub toolchain: Option<String>,
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
    /// Recorded but not activated; takes effect at the next session restart.
    Deferred {
        reason: String,
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
    /// Current generation; 0 is the implicit base generation with no layers.
    pub generation: u64,
    /// Every recorded generation, oldest first (the base is implicit).
    pub generations: Vec<u64>,
    /// Component name to layer hash for the current generation.
    pub components: BTreeMap<String, String>,
    /// `systemctl is-active` of the session unit (`vm` reports `n/a`).
    pub session: String,
    pub seat: Option<String>,
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
    pub storage: Storage,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub slot: String,
    pub generation: u64,
    /// False when the request matched the current state and nothing was done.
    pub changed: bool,
    pub policy: Policy,
    pub actions: Vec<Action>,
    /// Non-fatal findings, e.g. a layer built against a different base image.
    #[serde(default)]
    pub warnings: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_line_is_flat_json_rpc() {
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

    #[test]
    fn method_names_match_serde_tags() {
        let requests: Vec<Request> = vec![
            Hello {
                protocol: 1,
                client_version: String::new(),
            }
            .into(),
            Status::default().into(),
            SlotList {}.into(),
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

    #[test]
    fn slot_names_fit_user_names() {
        assert!(valid_slot_name("vm-dev-loop"));
        assert!(!valid_slot_name("Upper"));
        assert!(!valid_slot_name("-lead"));
        assert!(!valid_slot_name("trail-"));
        assert!(!valid_slot_name(&"a".repeat(29)));
        assert!(!valid_slot_name("a/b"));
    }
}
