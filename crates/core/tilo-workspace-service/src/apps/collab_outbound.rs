//! The broker's durable record: where a locally-authored document write and an effect's
//! receipt are committed, in one place, so "did this edit reach the workspace" has one answer.

use std::sync::Arc;

/// The one place a local write is committed. Construct with [`PeerPublisher::with_commits`]
/// (a durable record installed) or [`PeerPublisher::disabled`]. The broker holds one instance,
/// so "did this edit leave the machine" has one answer per session.
pub struct PeerPublisher {
    commits: Option<Arc<dyn tilo_broker_core::workspace_commits::WorkspaceCommits>>,
}

impl PeerPublisher {
    /// A publisher that commits through the host-local durable record `commits`.
    pub fn with_commits(
        commits: Arc<dyn tilo_broker_core::workspace_commits::WorkspaceCommits>,
    ) -> Arc<Self> {
        Arc::new(Self {
            commits: Some(commits),
        })
    }
    /// Whether a durable record is installed here: every workspace's document writes
    /// cross it.
    pub fn has_commits(&self) -> bool {
        self.commits.is_some()
    }
    /// Whether `workspace`'s effect receipts are recorded here.
    pub fn records_effects_for(&self, workspace: &str) -> bool {
        self.commits.as_ref().is_some_and(|commits| {
            tilo_desktop_model::WorkspaceId::parse(workspace)
                .is_some_and(|workspace| commits.projects_workspace(workspace))
        })
    }
    /// Health of the installed durable record.
    pub fn commits_health(&self) -> Result<(), String> {
        self.commits
            .as_ref()
            .ok_or("durable workspace record unsupported")?
            .health()
    }
    /// Record an effect's terminal receipt in `workspace`.
    pub async fn record_effect(&self, workspace: String, redacted: Vec<u8>) -> Result<(), String> {
        self.commits
            .as_ref()
            .ok_or("durable workspace record unsupported")?
            .effect(workspace, redacted)
            .await
    }

    /// A publisher with no durable record: every commit is refused.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self { commits: None })
    }
}
