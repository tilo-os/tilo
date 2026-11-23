//! Bring the workspace's earlier Chat reports and decisions into its moderation record.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use automerge::AutoCommit;
use collab_types::WorkspaceId;
use collab_types::document::{DocumentId, ResourceType, identity};
use moderation_model::{DOCUMENT, Moderation, earlier_chat_history};
use tilo_app_id::apps::CHAT;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_resource::{Fields as _, Identified as _, Resource as _};

use super::{GovernedWorkspace, Workspaces};

impl GovernedWorkspace {
    pub(super) fn moderation_history_pending(&self, app: &str, resource: &str) -> bool {
        if (app, resource) != (CHAT, DOCUMENT)
            || self.member.is_some()
            || self
                .moderation_history_checked
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        let peer = self.peer();
        peer.holds_document(CHAT, "") || peer.checkpoint_pending(CHAT, "")
    }
}

impl Workspaces {
    /// Try restored workspaces before serving reads. Keep retrying only the ones
    /// waiting for their retained history or keys, and take notice of its arrival.
    pub(super) async fn follow_moderation_history(&self) {
        let mut writes = self.document_writes();
        let mut pending = BTreeSet::new();
        for workspace in self.held_workspaces() {
            if !self.moderation_history_ready(&workspace).await {
                pending.insert(workspace);
            }
        }
        let workspaces = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                let workspace = tokio::select! {
                    _ = tick.tick(), if !pending.is_empty() => None,
                    write = writes.recv() => match write {
                        Ok(write) if write.app == CHAT && write.record.is_empty() => {
                            Some(write.workspace)
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                };
                let Some(inner) = workspaces.upgrade() else {
                    return;
                };
                let service = Self { inner };
                if let Some(workspace) = workspace {
                    if !service.moderation_history_ready(&workspace).await {
                        pending.insert(workspace);
                    }
                } else {
                    // A checkpoint can finish before its write notice. Re-read
                    // the live readiness of the held workspaces after a gap.
                    pending.clear();
                    for workspace in service.held_workspaces() {
                        if !service.moderation_history_ready(&workspace).await {
                            pending.insert(workspace);
                        }
                    }
                }
            }
        });
    }

    async fn moderation_history_ready(&self, workspace: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return true;
        };
        let pending = state
            .lock()
            .await
            .moderation_history_pending(CHAT, DOCUMENT);
        if !pending {
            return true;
        }
        match Box::pin(self.migrate_moderation_history(workspace)).await {
            Ok(true) => {
                state
                    .lock()
                    .await
                    .moderation_history_checked
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::debug!(%workspace, %error, "waiting to move moderation history");
                false
            }
        }
    }

    async fn migrate_moderation_history(&self, workspace: &str) -> Result<bool, String> {
        // A strict read refuses incomplete sealed history. In particular,
        // absent and pending are never a source of empty moderation genesis.
        let DocumentRead::Whole { bytes, .. } = self
            .own_read(workspace, (CHAT, ""), &[], usize::MAX)
            .await?
        else {
            return Ok(false);
        };
        let mut source = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        let earlier = earlier_chat_history(&mut source).map_err(|error| error.to_string())?;
        if earlier.reports.is_empty() && earlier.decisions.is_empty() {
            return Ok(true);
        }

        let id = DocumentId::parse(DOCUMENT).ok_or("moderation has no document id")?;
        let kind = ResourceType::new(Moderation::KIND);
        let mut genesis =
            AutoCommit::new().with_actor(identity::genesis_actor(&id, &kind, workspace));
        if !identity::stamp_founding(&mut genesis, &id, &kind, workspace, workspace) {
            return Err("the moderation record has no workspace identity".into());
        }
        Moderation::initialize(&mut genesis).map_err(|error| error.to_string())?;
        identity::commit_genesis(&mut genesis);

        let workspace_id = WorkspaceId::parse(workspace).ok_or("invalid workspace id")?;
        let reading = self
            .reading(workspace_id)
            .await
            .ok_or("workspace is not held")?;
        let current = super::sealed_documents::readable_now(&reading, (CHAT, DOCUMENT)).await?;
        let current = current.map_or(Ok(Moderation::default()), |bytes| {
            let mut doc = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
            read_moderation(&mut doc)
        })?;
        let mut genesis = Some(Box::new(genesis));
        // Keep each publication within the normal size limit and make an interrupted
        // copy resumable. The first publication contains an actual report or decision.
        for report in earlier
            .reports
            .iter()
            .filter(|row| current.reports.find(&row.id()).is_none())
        {
            self.import_moderation(workspace, genesis.take(), |state| {
                if state.reports.find(&report.id()).is_none() {
                    state.reports.push(report.clone());
                }
            })
            .await?;
        }
        for decision in earlier
            .decisions
            .iter()
            .filter(|row| current.decisions.find(&row.id()).is_none())
        {
            self.import_moderation(workspace, genesis.take(), |state| {
                if state.decisions.find(&decision.id()).is_none() {
                    state.decisions.push(decision.clone());
                }
            })
            .await?;
        }
        Ok(true)
    }

    async fn import_moderation(
        &self,
        workspace: &str,
        genesis: Option<Box<AutoCommit>>,
        import: impl FnOnce(&mut Moderation),
    ) -> Result<(), String> {
        self.author_workspace_document_from(
            (workspace, self.device_cert()),
            (CHAT, DOCUMENT),
            genesis,
            |doc| {
                let before = read_moderation(doc)?;
                let mut after = before.clone();
                import(&mut after);
                after
                    .write_changed_fields(
                        &before,
                        doc,
                        &automerge::ROOT,
                        &tilo_resource::Delta::default(),
                    )
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .map_err(|error| error.to_string())
    }
}

fn read_moderation(doc: &mut AutoCommit) -> Result<Moderation, String> {
    tilo_resource::evolution::migrate::<Moderation>(doc).map_err(|error| error.to_string())?;
    Moderation::read(doc).map_err(|error| error.to_string())
}
