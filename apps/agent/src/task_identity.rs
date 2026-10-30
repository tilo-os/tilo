//! A task agent's identity: the credentials its desktop left for it when it
//! made the agent for one task, turned into what the agent joins with.
//!
//! The desktop mints a key for the task, signs a task delegation with its
//! device key, and leaves both in this agent's app directory under
//! [`runtime_collab::TASK_AGENT_CREDENTIALS_DIR`], one file per task agent
//! named by its id. The agent reads its file once and removes it, so the
//! task key lives only in the process made for the task.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ed25519_dalek::Signer as _;
use runtime_collab::TaskAgentCredentials;
use ui::collaboration::CertifiedEndpointIdentity;

/// Where the desktop leaves task agents' credentials: in the app directory
/// the launcher gave this process (`TILO_APP_DIR`), else its home.
///
/// # Errors
/// Neither is set.
pub fn credentials_dir() -> Result<PathBuf> {
    let base = std::env::var_os("TILO_APP_DIR")
        .or_else(|| std::env::var_os("HOME"))
        .context("neither TILO_APP_DIR nor HOME names this agent's directory")?;
    Ok(PathBuf::from(base).join(runtime_collab::TASK_AGENT_CREDENTIALS_DIR))
}

/// Read and remove the credentials of task agent `agent_id` from `dir`, and
/// build the identity it joins `workspace` with: the presentation carrying
/// the device's certificate and task delegation, proved and signed by the
/// task key.
///
/// # Errors
/// The file cannot be read, decoded or removed, or it names another
/// workspace.
pub fn load(
    dir: &Path,
    agent_id: &str,
    workspace: uuid::Uuid,
) -> Result<CertifiedEndpointIdentity> {
    let path = dir.join(TaskAgentCredentials::file_name(agent_id));
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading the task agent's credentials at {}", path.display()))?;
    std::fs::remove_file(&path).with_context(|| {
        format!(
            "removing the task agent's credentials at {}",
            path.display()
        )
    })?;
    let credentials = TaskAgentCredentials::decode(&bytes).with_context(|| {
        format!(
            "decoding the task agent's credentials at {}",
            path.display()
        )
    })?;
    let room = workspace.to_string();
    anyhow::ensure!(
        credentials.workspace_id == room,
        "the task agent's credentials are for workspace {}, not {room}",
        credentials.workspace_id
    );
    let key = ed25519_dalek::SigningKey::from_bytes(&credentials.agent_key);
    let proof = key
        .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
        .to_bytes();
    let presentation = credentials
        .presentation(proof)
        .context("encoding the task agent's join presentation")?;
    Ok(CertifiedEndpointIdentity::new(presentation, key))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;

    const ROOM: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

    fn leave(dir: &Path, agent_id: &str, workspace: &str) -> PathBuf {
        let credentials = TaskAgentCredentials {
            workspace_id: workspace.into(),
            device_certificate: b"{}".to_vec(),
            delegation: vec![1, 2, 3],
            agent_key: [7; 32],
        };
        let path = dir.join(TaskAgentCredentials::file_name(agent_id));
        std::fs::write(&path, credentials.encode().unwrap()).unwrap();
        path
    }

    #[test]
    fn credentials_are_read_once_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = leave(dir.path(), "task-0a0b0c0d0e0f", ROOM);
        let identity = load(dir.path(), "task-0a0b0c0d0e0f", ROOM.parse().unwrap()).unwrap();
        assert!(format!("{identity:?}").contains("redacted"));
        assert!(
            !path.exists(),
            "the task key is removed from disk once read"
        );
        assert!(
            load(dir.path(), "task-0a0b0c0d0e0f", ROOM.parse().unwrap()).is_err(),
            "read once: a second reading finds nothing"
        );
    }

    #[test]
    fn credentials_for_another_workspace_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        leave(
            dir.path(),
            "task-0a0b0c0d0e0f",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        );
        let error = load(dir.path(), "task-0a0b0c0d0e0f", ROOM.parse().unwrap()).unwrap_err();
        assert!(error.to_string().contains("not 6f1c2a3b"), "{error}");
    }
}
