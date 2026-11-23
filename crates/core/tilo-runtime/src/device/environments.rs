//! The Terminal environments of the workspaces this device keeps, which its runtime (`ush`) runs
//! beside their records: a branch made here takes its source's inside the branch operation, so a
//! branch is the person's only once its environments are there, and a runtime that refuses fails
//! the branch.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use tilo_workspace_service::workspaces::WorkspaceMachines;

/// How long a branch waits for the runtime to copy the environments: large homes take a while.
const PATIENCE: Duration = Duration::from_secs(600);

/// The runtime that keeps this device's Terminal environments, reached at its socket.
pub(crate) struct Environments(PathBuf);

impl Environments {
    /// This device's.
    pub(crate) fn here() -> Self {
        Self(launcher::services::socket_for("ush"))
    }
}

impl WorkspaceMachines for Environments {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        // No runtime runs here, so no workspace has environments to copy.
        if !self.0.exists() {
            return Ok(false);
        }
        let stream = tilo_files::native::socket::connect(&self.0)
            .map_err(|error| format!("the Terminal environments: {error}"))?;
        let asked = (|| -> Result<serde_json::Value, String> {
            stream
                .set_read_timeout(Some(PATIENCE))
                .map_err(|error| error.to_string())?;
            let mut line = serde_json::to_vec(&serde_json::json!({
                "op": "branch-workspace",
                "workspace": source,
                "into": branch,
            }))
            .map_err(|error| error.to_string())?;
            line.push(b'\n');
            (&stream)
                .write_all(&line)
                .map_err(|error| error.to_string())?;
            let mut reply = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut reply)
                .map_err(|error| error.to_string())?;
            serde_json::from_str(reply.trim()).map_err(|error| error.to_string())
        })()
        .map_err(|error| format!("the Terminal environments: {error}"))?;
        match asked.get("result").and_then(serde_json::Value::as_str) {
            Some("branched") => Ok(asked
                .get("environments")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|environments| environments > 0)),
            _ => Err(format!(
                "the Terminal environments: {}",
                asked
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the runtime did not branch them")
            )),
        }
    }

    /// Deleting a workspace asks nothing of the runtime, which keeps its environments by
    /// workspace.
    fn delete(&self, _workspace: &str) -> Result<(), String> {
        Ok(())
    }

    /// The runtime's environments are processes of this device's own session: nothing to
    /// suspend before the device restarts.
    fn drain(&self) -> Result<(), String> {
        Ok(())
    }
}
