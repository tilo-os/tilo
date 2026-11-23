//! The runs `tilo migrate` makes on this computer: where it keeps each workspace's migrations,
//! whether a run of a source is going, and asking a going run to stop.
//!
//! A run is this process while it brings a source over. It notes its process id beside the
//! source's migration, so another `tilo migrate` sees it going and `tilo migrate stop` asks it to
//! stop after the item it is on.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

/// Where the migrations into `workspace` are kept on this computer.
///
/// # Errors
/// This account has no state directory.
pub fn root(workspace: &str) -> Result<PathBuf> {
    let root = match std::env::var_os("TILO_MIGRATE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => tilo_paths::state_dir()
            .context("this account has no state directory (no HOME)")?
            .join("tilo/migrate"),
    };
    Ok(root.join(component(workspace)))
}

/// `name` as one path component.
fn component(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_owned()
}

fn pid_file(root: &Path, source: &str) -> PathBuf {
    root.join(component(source)).join("run.pid")
}

fn stop_file(root: &Path, source: &str) -> PathBuf {
    root.join(component(source)).join("stop")
}

/// Whether a run of `source` is going under `root`.
pub fn running(root: &Path, source: &str) -> bool {
    std::fs::read_to_string(pid_file(root, source))
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

/// This process runs `source` under `root` until the answer is dropped.
///
/// # Errors
/// The run could not be noted.
pub fn begin(root: &Path, source: &str) -> Result<Going> {
    let pid = pid_file(root, source);
    if let Some(dir) = pid.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(stop_file(root, source));
    std::fs::write(&pid, std::process::id().to_string())
        .with_context(|| format!("noting the run in {}", pid.display()))?;
    Ok(Going {
        pid,
        stop: stop_file(root, source),
    })
}

/// A run this process is making.
pub struct Going {
    pid: PathBuf,
    stop: PathBuf,
}

impl Going {
    /// Whether the run was asked to stop.
    pub fn stopping(&self) -> bool {
        self.stop.exists()
    }
}

impl Drop for Going {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid);
        let _ = std::fs::remove_file(&self.stop);
    }
}

/// `stop`: ask the run of `source` under `root` to stop after the item it is on.
///
/// # Errors
/// The ask could not be noted.
pub fn stop(root: &Path, source: &str) -> Result<Value> {
    if !running(root, source) {
        return Ok(json!({"source": source, "said": format!("no {source} migration is running")}));
    }
    std::fs::write(stop_file(root, source), b"")?;
    Ok(json!({"source": source, "said": format!(
        "the {source} migration stops after the item it is on; `tilo migrate start {source}` resumes it"
    )}))
}
