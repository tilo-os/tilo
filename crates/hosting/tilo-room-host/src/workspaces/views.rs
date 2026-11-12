//! Exact-source discovery using the governor's already-owned channel state.
use super::{GovernedWorkspace, StatePeer, SystemTime, Workspaces};
use tilo_actions::remote::{Caller, CallerPrincipal};
use tilo_agent_participant::workspace_actions::{ViewDocument, ViewReadPolicy};
use tilo_peer_state::MemberAuthority;
use tilo_peer_state::document_frame::DocumentRoute;

const MAX_VIEW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

impl GovernedWorkspace {
    /// The authority `caller` holds here now: only a person on a device this
    /// workspace certified, whose certificate is current and whose membership
    /// it still records. A revoked device, a removed member and a stranger hold
    /// none. What a member may do with what they find is decided per action, by
    /// this authority.
    fn admitted(&self, caller: &Caller) -> Option<MemberAuthority> {
        if !matches!(caller.principal(), CallerPrincipal::Person(_)) {
            return None;
        }
        let device = *caller.device_public_key()?;
        let user = caller.user_public_key()?;
        let peer = self.peer();
        if !peer
            .certificates()
            .iter()
            .any(|cert| cert.device_public_key == device && &cert.user_public_key == user)
        {
            return None;
        }
        peer.permissions(device, SystemTime::now()).ok()
    }

    /// Whether `caller` is someone this workspace admits.
    fn admits(&self, caller: &Caller) -> bool {
        self.admitted(caller).is_some()
    }

    /// `caller`'s standing across the workspace now and its sandbox: what
    /// every record's policy holds before the record's own read is checked.
    fn workspace_policy(&self, caller: &Caller) -> Option<ViewReadPolicy> {
        let standing = self.admitted(caller)?.standing();
        let sandbox = self.peer().governance().governed_sandbox().ok()?;
        Some(ViewReadPolicy {
            workspace: self.workspace().to_owned(),
            standing,
            sandbox,
            unix_time: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_secs(),
        })
    }

    /// Called while holding this governed workspace's lock, including before keys.
    fn view_read_policy(&self, caller: &Caller, app: &str, record: &str) -> Option<ViewReadPolicy> {
        let device = *caller.device_public_key()?;
        self.peer()
            .check_document_read(device, app, record, SystemTime::now())
            .ok()?;
        self.workspace_policy(caller)
    }
}

/// Whether the bot installed as `installation` may show other names on what
/// it writes in `peer`'s workspace at `now`: its governed installation row is
/// live, acting, and allows `Personas`. Any other device, an uninstalled or
/// moved-away row, or a bot installed to post as itself is refused.
pub fn bot_shows_personas(peer: &StatePeer, installation: &str, now: SystemTime) -> bool {
    peer.governance()
        .installation(installation, now)
        .is_some_and(|bot| {
            bot.acting
                && bot.withdrawn_at_ns.is_none()
                && bot.installation.appearance == tilo_peer_state::AppearancePolicy::Personas
        })
}

impl Workspaces {
    /// `answer` of the workspace `workspace` governed here, under its lock;
    /// `None` for a workspace not governed here.
    async fn governing<T>(
        &self,
        workspace: u64,
        answer: impl FnOnce(&GovernedWorkspace) -> T,
    ) -> Option<T> {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for governed in workspaces {
            let runtime = governed.lock().await;
            if runtime.workspace_id() == workspace {
                return Some(answer(&runtime));
            }
        }
        None
    }

    /// [`bot_shows_personas`] in the workspace of `workspace` governed here;
    /// false for a workspace not governed here.
    pub async fn bot_personas(&self, workspace: u64, installation: &str) -> bool {
        self.governing(workspace, |runtime| {
            bot_shows_personas(&runtime.peer(), installation, SystemTime::now())
        })
        .await
        .unwrap_or(false)
    }

    /// Whether the workspace `workspace` governs here admits `caller`: false
    /// for a workspace it does not govern, and for anyone its governance does
    /// not certify a device for.
    pub async fn admits(&self, workspace: u64, caller: &Caller) -> bool {
        self.governing(workspace, |runtime| runtime.admits(caller))
            .await
            .unwrap_or(false)
    }

    pub async fn resource_view_policy(
        &self,
        workspace: u64,
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

    /// `caller`'s standing across the workspace `workspace` governed here,
    /// with no record's read checked: what a workspace's content and its new
    /// records are decided by.
    pub async fn workspace_policy(
        &self,
        workspace: u64,
        caller: &Caller,
    ) -> Option<ViewReadPolicy> {
        self.governing(workspace, |runtime| runtime.workspace_policy(caller))
            .await
            .flatten()
    }

    pub async fn resource_view_document(
        &self,
        workspace: u64,
        caller: &Caller,
        source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        if source.workspace_id().ok() != Some(workspace) {
            return Err("source room mismatch".into());
        }
        let state = self
            .workspace(&source.cluster_id)
            .await
            .map_err(|error| error.to_string())?;
        let runtime = state.lock().await;
        if runtime.workspace_id() != workspace {
            return Err("source room mismatch".into());
        }
        // This guard is under the same lock as key access and replay. Revocation
        // cannot slip between a prior policy lookup and the first key access.
        runtime
            .view_read_policy(caller, &source.app_id, &source.record)
            .ok_or("resource read not permitted")?;
        let route = collab_types::ChannelRoute::new(&source.app_id, &source.record);
        let keys = match runtime.route_keys(&route).await {
            Some(Ok(keys)) => Some(keys),
            _ => None,
        };
        let primary_governor = (source.record.is_empty()
            && runtime.peer().holds_document(&source.app_id, ""))
        .then_some(runtime.record.authority.device_public_key);
        if keys.is_none() && primary_governor.is_none() {
            return Err(if runtime.seals() {
                "source channel keys unavailable".into()
            } else {
                "this host takes part in no channel of the source's room".into()
            });
        }
        let retained = self
            .inner
            .host
            .records
            .view_channel(
                runtime.workspace_id(),
                neighbour_wire::app_document_channel(&source.app_id, &source.record),
                MAX_VIEW_DOCUMENT_BYTES,
            )
            .await?;
        let bytes = replay_source(source, keys.as_ref(), primary_governor, retained)?;
        runtime
            .view_read_policy(caller, &source.app_id, &source.record)
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
    keys: Option<&channel_crypto::ChannelContentKeys>,
    primary_governor: Option<[u8; 32]>,
    retained: Vec<crate::workspace_records::RetainedBody>,
) -> Result<Vec<u8>, String> {
    let channel =
        collab_types::document_channel_id(&source.app_id, &source.cluster_id, &source.record);
    let mut transfers = tilo_peer_state::document_frame::Transfers::default();
    let mut doc = automerge::AutoCommit::new();
    let mut bytes = 0usize;
    let mut found = false;
    for retained in retained {
        // ignored-failure: a retained write that does not decode is reported where the governor takes it in (Workspaces::absorb) and when it is replayed (recall_retained)
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes(&retained.body)
        else {
            continue;
        };
        if app_id != source.app_id || cluster_id != source.cluster_id || record_id != source.record
        {
            continue;
        }
        match envelope.encryption.as_ref() {
            Some(metadata) if metadata.channel == channel => {}
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
