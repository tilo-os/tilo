//! The elevated staging helper mode (`--tilo-staging-helper <request> <progress>`).

use serde::{Deserialize, Serialize};

use super::plan::Plan;
use super::record::ReleaseRef;

pub const STAGING_HELPER_ARG: &str = "--tilo-staging-helper";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Stage,
    Arm,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentPaths {
    #[serde(rename = "staging-esp")]
    pub staging_esp: String,
    #[serde(rename = "staging-root")]
    pub staging_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub operation_id: String,
    pub action: Action,
    pub release: ReleaseRef,
    pub manifest_path: String,
    pub component_paths: ComponentPaths,
    pub plan: Plan,
    pub plan_sha256: String,
    /// Secret-free installer profile fields copied into the handoff.
    #[serde(default)]
    pub profile: serde_json::Map<String, serde_json::Value>,
}

/// One line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub step: super::record::Step,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Final line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub ok: bool,
    /// Stable refusal ID (`plan-stale`, `boot-next-busy`, `digest-mismatch`, ...).
    pub code: Option<String>,
    pub message: Option<String>,
}

/// A running elevated helper launched by the UI.
pub struct Running {
    pub progress_path: std::path::PathBuf,
    child: Option<std::process::Child>,
}

/// Snapshot of a running helper, read from `progress.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    pub latest: Option<Progress>,
    /// `Some` once the helper has written its final line or exited.
    pub outcome: Option<Outcome>,
}

impl Running {
    pub fn poll(&mut self) -> Poll {
        let _ = &mut self.child;
        Poll::default()
    }
}

/// Write `request` to a private temp file and launch the elevated helper
/// (`pkexec` on Linux, `RunAs` on Windows).
pub fn launch(request: &Request) -> Result<Running, String> {
    let _ = request;
    Err("staging helper not implemented".into())
}

/// Run helper mode when `args` requests it; `Ok(None)` otherwise.
pub fn maybe_run_staging_helper(args: &[String]) -> Result<Option<i32>, String> {
    if args.get(1).map(String::as_str) != Some(STAGING_HELPER_ARG) {
        return Ok(None);
    }
    Err("staging helper not implemented".into())
}

/// Unfinished staging records visible to this user (for the resume card).
pub fn pending_records() -> Vec<super::record::Record> {
    Vec::new()
}
