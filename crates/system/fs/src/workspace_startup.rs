//! Read-only binding preflight. It never repairs a namespace, mints identity,
//! contacts a peer, or prints the bearer ticket used by the existing daemon.
use std::path::Path;

use anyhow::{Context, Result};
use fs::Ticket;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceStatus {
    schema_version: u8,
    saved_namespace: bool,
    local_workspace_id: Option<uuid::Uuid>,
    discovered_workspace_id: Option<uuid::Uuid>,
    can_resume_existing: bool,
}

impl WorkspaceStatus {
    pub(super) fn require_existing(&self) -> Result<()> {
        anyhow::ensure!(
            self.can_resume_existing,
            "no saved Files namespace or discovered remote workspace; recover the existing workspace first, or explicitly initialize a new workspace (not recovered data)"
        );
        Ok(())
    }
}

fn regular_file(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file(),
                "workspace metadata must be a regular file"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect workspace metadata"),
    }
}

pub(super) fn inspect(cache: &Path) -> Result<WorkspaceStatus> {
    let saved_namespace = regular_file(&cache.join("namespace.bin"))?;
    let ticket_path = cache.join("workspace-ticket");
    let local_workspace_id = if regular_file(&ticket_path)? {
        let metadata = std::fs::metadata(&ticket_path)?;
        anyhow::ensure!(metadata.len() <= 64 * 1024, "workspace ticket is too large");
        let encoded = std::fs::read_to_string(ticket_path).context("read workspace binding")?;
        Some(
            Ticket::decode(&encoded)
                .context("invalid workspace binding")?
                .workspace_id(),
        )
    } else {
        None
    };
    let discovered = super::persisted_remote_workspace_ticket(Some(cache))?;
    let discovered_workspace_id = discovered.as_ref().map(Ticket::workspace_id);
    anyhow::ensure!(
        !(saved_namespace && discovered_workspace_id.is_some() && local_workspace_id.is_none()),
        "saved namespace lacks its workspace binding; recover the binding before remote bootstrap"
    );
    if saved_namespace
        && let (Some(local), Some(remote)) = (local_workspace_id, discovered_workspace_id)
    {
        anyhow::ensure!(
            local == remote,
            "saved namespace and discovered workspace disagree; use a separate cache"
        );
    }
    // A ticket alone cannot restore a local namespace. Discovery, unlike an
    // old host ticket, supplies the authenticated endpoint to bootstrap from.
    Ok(WorkspaceStatus {
        schema_version: 1,
        saved_namespace,
        local_workspace_id,
        discovered_workspace_id,
        can_resume_existing: saved_namespace || discovered.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::workspace_authority::PersistedContentPeer;

    fn ticket(topic: u8) -> Ticket {
        Ticket {
            node: iroh_base::EndpointAddr::new(iroh::SecretKey::from_bytes(&[topic; 32]).public()),
            workspace: [topic; 32],
        }
    }

    fn discover(cache: &Path, ticket: &Ticket) {
        let peers = [PersistedContentPeer {
            endpoint: ticket.node.clone(),
            ticket: Some(ticket.encode().unwrap()),
            relay_service_id: Some([9; 32]),
        }];
        std::fs::write(
            cache.join("content-peers.json"),
            serde_json::to_vec(&peers).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn missing_workspace_is_not_initialized_and_old_ticket_is_not_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent-cache");
        assert!(inspect(&missing).unwrap().require_existing().is_err());
        assert!(!missing.exists());
        std::fs::write(
            dir.path().join("workspace-ticket"),
            ticket(1).encode().unwrap(),
        )
        .unwrap();
        assert!(inspect(dir.path()).unwrap().require_existing().is_err());
        assert!(!dir.path().join("namespace.bin").exists());
    }

    #[test]
    fn discovered_binding_is_resumeable_and_output_never_contains_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let binding = ticket(2);
        discover(dir.path(), &binding);
        let status = inspect(dir.path()).unwrap();
        status.require_existing().unwrap();
        assert_eq!(status.discovered_workspace_id, Some(binding.workspace_id()));
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains(&binding.encode().unwrap()));
        assert!(!dir.path().join("namespace.bin").exists());
    }

    #[test]
    fn saved_namespace_cannot_be_rebound_to_another_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("namespace.bin"), b"existing durable state").unwrap();
        std::fs::write(
            dir.path().join("workspace-ticket"),
            ticket(3).encode().unwrap(),
        )
        .unwrap();
        inspect(dir.path()).unwrap().require_existing().unwrap();
        discover(dir.path(), &ticket(4));
        assert!(inspect(dir.path()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("namespace.bin")).unwrap(),
            b"existing durable state"
        );
    }

    #[test]
    fn malformed_and_ambiguous_bindings_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("workspace-ticket"), b"invalid").unwrap();
        assert!(inspect(dir.path()).is_err());
        std::fs::remove_file(dir.path().join("workspace-ticket")).unwrap();
        let peers: Vec<_> = [ticket(5), ticket(6)]
            .into_iter()
            .map(|t| PersistedContentPeer {
                endpoint: t.node,
                ticket: Some(
                    Ticket {
                        node: iroh_base::EndpointAddr::new(
                            iroh::SecretKey::from_bytes(&[8; 32]).public(),
                        ),
                        workspace: t.workspace,
                    }
                    .encode()
                    .unwrap(),
                ),
                relay_service_id: None,
            })
            .collect();
        std::fs::write(
            dir.path().join("content-peers.json"),
            serde_json::to_vec(&peers).unwrap(),
        )
        .unwrap();
        assert!(inspect(dir.path()).is_err());
    }
}
