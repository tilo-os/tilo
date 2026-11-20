//! Exact-source discovery using the governor's already-owned channel state.
use super::{GovernedWorkspace, StatePeer, SystemTime, Workspaces};
use std::collections::HashMap;
use std::sync::Arc;
use tilo_action_policy::MemberAccess;
use tilo_actions::remote::Caller;
use tilo_agent_session::workspace_actions::{ViewDocument, ViewReadPolicy};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

const MAX_VIEW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// A governed workspace's governance as its replica holds it now: what every
/// decision of who reads what is made against.
struct Governance<'a> {
    peer: &'a StatePeer,
    workspace: &'a str,
}

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

    /// What this workspace is read through without its lock.
    fn reading(&self) -> Reading {
        Reading {
            replica: self.peer.clone(),
            workspace: self.workspace().to_owned(),
            device: self.device.clone(),
        }
    }
}

/// What a governed workspace is read through without its lock: its replica,
/// which every change is made in and which is only ever held for a moment, the
/// workspace's name and the keys of the device it is held on. The workspace's
/// lock is held across what takes long: asking the key service, which settles a
/// newcomer's access first, and a commit's writes. A read waiting for those made
/// every call of an agent wait up to a minute while a person joined.
#[derive(Clone)]
pub(super) struct Reading {
    pub(super) replica: super::Replica,
    pub(super) workspace: String,
    pub(super) device: Option<super::DeviceKeys>,
}

type Governed = std::sync::Weak<tokio::sync::Mutex<GovernedWorkspace>>;

/// The [`Reading`] of each workspace governed here, once one was read.
#[derive(Default)]
pub(super) struct Readings(std::sync::Mutex<HashMap<WorkspaceId, (Governed, Reading)>>);

impl Readings {
    fn of(
        &self,
        workspace: WorkspaceId,
        governed: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
    ) -> Option<Reading> {
        let (of, reading) = self.0.lock_recover().get(&workspace)?.clone();
        // A workspace opened again is read through its new replica.
        (of.as_ptr() == Arc::as_ptr(governed)).then_some(reading)
    }

    fn keep(
        &self,
        workspace: WorkspaceId,
        governed: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
        reading: Reading,
    ) {
        let mut known = self.0.lock_recover();
        known.retain(|_, (of, _)| of.strong_count() > 0);
        known.insert(workspace, (Arc::downgrade(governed), reading));
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
    /// What the workspace `workspace` governed here is read through without
    /// its lock; `None` for a workspace not governed here. Only the first read
    /// of a workspace opened here waits for its lock.
    pub(super) async fn reading(&self, workspace: WorkspaceId) -> Option<Reading> {
        let governed = self.held(workspace).await?;
        if let Some(reading) = self.inner.readings.of(workspace, &governed) {
            return Some(reading);
        }
        let reading = governed.lock().await.reading();
        self.inner
            .readings
            .keep(workspace, &governed, reading.clone());
        Some(reading)
    }

    /// `answer` of the governance of the workspace `workspace` governed here,
    /// as its replica holds it now; `None` for a workspace not governed here.
    async fn governing<T>(
        &self,
        workspace: WorkspaceId,
        answer: impl FnOnce(&Governance<'_>) -> T,
    ) -> Option<T> {
        let reading = self.reading(workspace).await?;
        let peer = reading.replica.lock();
        Some(answer(&Governance {
            peer: &peer,
            workspace: &reading.workspace,
        }))
    }

    /// [`bot_shows_personas`] in the workspace of `workspace` governed here;
    /// false for a workspace not governed here.
    pub async fn bot_personas(&self, workspace: WorkspaceId, installation: &str) -> bool {
        self.governing(workspace, |runtime| {
            bot_shows_personas(runtime.peer, installation, tilo_clock::wall())
        })
        .await
        .unwrap_or(false)
    }

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
        let retained = self
            .inner
            .host
            .records
            .view_channel(
                runtime.workspace_id(),
                neighbour_wire::app_document_channel(&source.app_id, &source.document),
                MAX_VIEW_DOCUMENT_BYTES,
            )
            .await?;
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
            postcard::from_bytes(&retained.body)
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
