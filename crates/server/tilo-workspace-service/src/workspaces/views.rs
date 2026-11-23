//! Exact-source discovery using the governor's already-owned channel state.
use super::readings::Governance;
use super::{GovernedWorkspace, SystemTime, Workspaces};
use tilo_action_policy::MemberAccess;
use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::{ViewDocument, ViewReadPolicy};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

const MAX_VIEW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

impl Governance<'_> {
    /// The access `caller` holds here now: only a person on a device this
    /// workspace certified, whose certificate is current and whose membership
    /// it still records. A revoked device, a removed member and a stranger hold
    /// none. What a member may do with what they find is decided per action, by
    /// this access.
    fn admitted(&self, caller: &Caller) -> Option<MemberAccess> {
        if !caller.acting().is_person() {
            return None;
        }
        let device = *caller.device_public_key()?;
        let user = caller.user_public_key()?;
        if !self
            .peer
            .certificates()
            .iter()
            .any(|cert| cert.device_public_key == device && &cert.user_public_key == user)
        {
            return None;
        }
        self.peer.permissions(device, tilo_clock::wall()).ok()
    }

    /// Whether `caller` is someone this workspace admits.
    fn admits(&self, caller: &Caller) -> bool {
        self.admitted(caller).is_some()
    }

    /// Whether this workspace keeps `caller`'s key as one its person revoked: nothing it asks
    /// is a member's until they approve another.
    fn key_revoked(&self, caller: &Caller) -> bool {
        let Some(device) = caller.device_public_key() else {
            return false;
        };
        matches!(
            self.peer.permissions(*device, tilo_clock::wall()),
            Err(tilo_peer_state::MembershipError::Revoked)
        )
    }

    /// `caller`'s access across the workspace now and its sandbox: what
    /// every record's policy holds before the record's own read is checked.
    fn workspace_policy(&self, caller: &Caller) -> Option<ViewReadPolicy> {
        let access = self.admitted(caller)?;
        let sandbox = self.peer.governance().governed_sandbox().ok()?;
        Some(ViewReadPolicy {
            workspace: self.workspace.to_owned(),
            access,
            sandbox,
            unix_time: tilo_clock::wall()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_secs(),
            agent_bound: Vec::new(),
        })
    }

    fn view_read_policy(
        &self,
        caller: &Caller,
        app: &str,
        resource: &str,
    ) -> Option<ViewReadPolicy> {
        let device = *caller.device_public_key()?;
        self.peer
            .check_document_read(device, app, resource, tilo_clock::wall())
            .ok()?;
        self.workspace_policy(caller)
    }
}

impl GovernedWorkspace {
    /// Called while holding this governed workspace's lock, including before keys.
    fn view_read_policy(
        &self,
        caller: &Caller,
        app: &str,
        resource: &str,
    ) -> Option<ViewReadPolicy> {
        Governance {
            peer: &self.peer(),
            workspace: self.workspace(),
        }
        .view_read_policy(caller, app, resource)
    }
}

impl Workspaces {
    /// Whether the workspace `workspace` governs here admits `caller`: false
    /// for a workspace it does not govern, and for anyone its governance does
    /// not certify a device for.
    pub async fn admits(&self, workspace: WorkspaceId, caller: &Caller) -> bool {
        self.governing(workspace, |runtime| runtime.admits(caller))
            .await
            .unwrap_or(false)
    }

    /// Whether the workspace `workspace` governs here keeps `caller`'s key as revoked: false
    /// for a workspace it does not govern.
    pub async fn key_revoked(&self, workspace: WorkspaceId, caller: &Caller) -> bool {
        self.governing(workspace, |runtime| runtime.key_revoked(caller))
            .await
            .unwrap_or(false)
    }

    pub async fn resource_view_policy(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        app: &str,
        record: &str,
    ) -> Option<ViewReadPolicy> {
        self.governing(workspace, |runtime| {
            runtime.view_read_policy(caller, app, record)
        })
        .await
        .flatten()
    }

    /// `caller`'s access across the workspace `workspace` governed here,
    /// with no record's read checked: what a workspace's content and its new
    /// records are decided by.
    pub async fn workspace_policy(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
    ) -> Option<ViewReadPolicy> {
        self.governing(workspace, |runtime| runtime.workspace_policy(caller))
            .await
            .flatten()
    }

    pub async fn resource_view_document(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        if source.workspace_id().ok() != Some(workspace) {
            return Err("source workspace mismatch".into());
        }
        let state = self
            .workspace(&source.cluster_id)
            .await
            .map_err(|error| error.to_string())?;
        let runtime = state.lock().await;
        if runtime.workspace_id() != workspace {
            return Err("source workspace mismatch".into());
        }
        // This guard is under the same lock as key access and replay. Revocation
        // cannot slip between a prior policy lookup and the first key access.
        runtime
            .view_read_policy(caller, &source.app_id, &source.document)
            .ok_or("resource read not permitted")?;
        let keys = match runtime.route_keys().await {
            Some(Ok(keys)) => Some(keys),
            _ => None,
        };
        let primary_governor = (source.document.is_empty()
            && runtime.peer().holds_document(&source.app_id, ""))
        .then_some(runtime.record.owner.device_public_key);
        if keys.is_none() && primary_governor.is_none() {
            return Err(if runtime.seals() {
                "source channel keys unavailable".into()
            } else {
                "this host takes part in no channel of the source's workspace".into()
            });
        }
        let retained = self.inner.records.view_channel(
            runtime.workspace_id(),
            neighbour_wire::app_document_group(&source.app_id, &source.document),
            MAX_VIEW_DOCUMENT_BYTES,
        )?;
        let bytes = replay_source(source, keys.as_ref(), primary_governor, retained)?;
        runtime
            .view_read_policy(caller, &source.app_id, &source.document)
            .ok_or("resource read not permitted")?;
        drop(runtime);
        drop(state);
        Ok(ViewDocument {
            revision: blake3::hash(&bytes).to_hex().to_string(),
            bytes,
        })
    }
}

fn replay_source(
    source: &DocumentRoute,
    keys: Option<&key_group::ContentKeys>,
    primary_governor: Option<[u8; 32]>,
    retained: Vec<crate::workspace_documents::RetainedBody>,
) -> Result<Vec<u8>, String> {
    let mut transfers = tilo_peer_state::document_frame::Transfers::default();
    let mut doc = automerge::AutoCommit::new();
    let mut bytes = 0usize;
    let mut found = false;
    for retained in retained {
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        }) = tilo_failure::ignored!(
            "a retained write that does not decode is reported where the governor takes it in",
            neighbour_wire::NeighbourMsg::decode(&retained.body)
        )
        else {
            continue;
        };
        if app_id != source.app_id
            || cluster_id != source.cluster_id
            || document_id != source.document
        {
            continue;
        }
        match envelope.encryption.as_ref() {
            Some(_) => {}
            None if primary_governor == Some(retained.author) => {}
            _ => return Err("source encrypted channel mismatch".into()),
        }
        bytes = bytes
            .checked_add(retained.body.len())
            .ok_or("source replay exceeds read budget")?;
        if bytes > MAX_VIEW_DOCUMENT_BYTES {
            return Err("source replay exceeds read budget".into());
        }
        let frame =
            tilo_peer_state::document_frame::open_frame(&retained.body, retained.author, keys)
                .map_err(|error| error.to_string())?
                .ok_or("source frame missing")?;
        let plaintext = frame
            .decrypted_payload
            .as_deref()
            .unwrap_or(&frame.envelope.payload);
        if let Some(mutation) = transfers.take(frame.author, plaintext)? {
            let (collab_types::DocumentMutation::Snapshot(change)
            | collab_types::DocumentMutation::Incremental(change)) = mutation;
            tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &change)
                .map_err(|error| error.to_string())?;
            found = true;
        }
    }
    if !found || !transfers.is_empty() || !doc.get_missing_deps(&[]).is_empty() {
        return Err("existing source is not completely retained".into());
    }
    Ok(doc.save())
}
