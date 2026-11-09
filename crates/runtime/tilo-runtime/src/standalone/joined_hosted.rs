//! The profile's own workspace, placed on the workspace host this runtime runs.
//!
//! The runtime runs the same workspace host every device runs
//! ([`HostMode::Device`](tilo_room_host::host::HostMode::Device)), in
//! process: the host creates the profile's workspace and issues this device the
//! invitation its replica redeems ([`WorkspaceHost::place_own_workspace`]), and from
//! then on the workspace is one of the workspaces this profile joined, its link
//! to the host paired in process. The host takes part in the workspace's
//! channels as this device's one peer, asking this device's own channel service
//! for their keys.
//!
//! An app's write to the workspace is admitted here and acknowledged once the
//! host keeps it. A write to a record the workspace does
//! not hold yet is taken only in a resource kind the app's installed
//! manifest declares; in dev mode, an app that is not installed is trusted
//! with the kind its own build names. The workspace lets the app author its
//! records first, as the person who placed it there.
use super::Joined;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::join_transport::WorkspaceJoinClient;
use tilo_peer_state::owner::session::Reach;
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_room_host::host::WorkspaceHost;

/// How long a write waits for the workspace's session and for its host to keep it.
const KEPT_WITHIN: Duration = Duration::from_secs(10);

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(in crate::standalone) type DeclaredKinds =
    Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a record the workspace does not hold yet.
pub(in crate::standalone) struct RecordCreation {
    pub(in crate::standalone) declared_kinds: DeclaredKinds,
    /// Started with `--dev`: an app that is not installed is a build under
    /// development, and may create records in the kinds it names.
    pub(in crate::standalone) dev: bool,
}

impl RecordCreation {
    fn allows(&self, app: &str, kind: &str) -> bool {
        match (self.declared_kinds)(app) {
            Some(declared) => declared.iter().any(|declared| declared == kind),
            None => self.dev,
        }
    }
}

/// The workspace host this profile runs, and what it lets apps create there.
pub(super) struct OwnHost {
    host: Arc<WorkspaceHost>,
    creation: RecordCreation,
}

impl Joined {
    /// Run `host`, this device's workspace host, from now on: the workspaces it hosts
    /// are reached in process, and apps create records there as `creation`
    /// lets them.
    ///
    /// # Errors
    /// A host is already running.
    pub(in crate::standalone) fn install_host(
        &self,
        host: Arc<WorkspaceHost>,
        creation: RecordCreation,
    ) -> Result<(), String> {
        self.own_host
            .set(OwnHost { host, creation })
            .map_err(|_| "this profile runs one workspace host".into())
    }

    fn room_host(&self) -> Option<&Arc<WorkspaceHost>> {
        self.own_host.get().map(|own| &own.host)
    }

    fn own_device(&self) -> [u8; 32] {
        self.custody.device_signing_key.verifying_key().to_bytes()
    }

    /// Place `workspace`, the profile's workspace called `name`, on this
    /// device's host and enroll in it, unless this profile holds it
    /// already: the workspace is held when this returns, and synced once its
    /// session is held ([`Self::hold_session`]).
    pub(in crate::standalone) async fn place(
        &self,
        workspace: WorkspaceId,
        name: &str,
    ) -> Result<(), String> {
        if self.owner.contains_workspace(workspace) {
            return Ok(());
        }
        let invitation = self
            .room_host()
            .ok_or("this profile runs no workspace host")?
            .place_own_room(&workspace.to_string(), name, &self.custody.device_cert)
            .await?;
        if self.enroll(&invitation).await? != workspace {
            return Err("this device's host placed another workspace".into());
        }
        Ok(())
    }

    /// Whether `workspace` is hosted by this device's own host.
    pub(in crate::standalone) fn hosted_here(&self, workspace: WorkspaceId) -> bool {
        let own = self.own_device();
        self.owner
            .with_current(workspace, |view| {
                Ok(view
                    .transport_hint()
                    .is_ok_and(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own)))
            })
            .unwrap_or(false)
    }

    /// How this device reaches a workspace's host: its own host in process,
    /// every other over the network.
    pub(super) fn reach(&self) -> Reach {
        Reach {
            endpoint: self.endpoint.clone(),
            own_host: self
                .room_host()
                .map(|host| Arc::clone(host) as Arc<dyn tilo_peer_state::owner::session::OwnHost>),
        }
    }

    /// The workspace-join exchange `invitation` asks for: with this
    /// device's own host in process when the invitation names it, an
    /// endpoint cannot dial itself; over the network otherwise.
    pub(super) fn join_client(&self, invitation: &str) -> Result<WorkspaceJoinClient, String> {
        let own = self.own_device();
        let hosted_here = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            None,
            &tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .is_ok_and(|verified| {
            verified
                .grant()
                .transport
                .iter()
                .any(|hint| tilo_peer_state::owner::session::hosted_by(hint, &own))
        });
        self.reach()
            .join_client(hosted_here, own)
            .ok_or_else(|| "this device's workspace host is not running".into())
    }

    /// Commit `request`, an app's write to `workspace`, a workspace this device's host
    /// hosts: sent to the host over this device's link to it, and answered once
    /// the host keeps it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the host is this profile's own, kept by a OnceLock: the reference holds no lock"
    )]
    pub(super) async fn commit_hosted(
        &self,
        workspace: WorkspaceId,
        request: OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = &request.operation
        else {
            return Err("an app writes documents to its workspace".into());
        };
        let (app, record, envelope) = (app.clone(), record.clone(), *envelope.clone());
        let own = self.own_device();
        if envelope.author.as_bytes() != &own {
            return Err("the document's signer differs from this device".into());
        }
        let own_host = self
            .own_host
            .get()
            .ok_or("this profile runs no workspace host")?;
        let (held, authors, generation) = self.owner.with_current(workspace, |view| {
            let now = SystemTime::now();
            Ok((
                view.peer().holds_document(&app, &record),
                view.peer().permissions(own, now).is_ok_and(|member| {
                    member
                        .policy
                        .app_authors(&app, &tilo_action_policy::Context::at_time(now))
                }),
                view.generation(),
            ))
        })?;
        if !held {
            let kind = record_kind
                .as_deref()
                .ok_or_else(|| format!("{app} named no kind for a new record"))?;
            if !own_host.creation.allows(&app, kind) {
                return Err(format!("{app} does not declare the resource kind `{kind}`"));
            }
        }
        if !authors {
            // This device lets the app author, committed by its own host,
            // which signs the workspace's history under this device's key. The
            // link carries the grant to this device behind the write, and the
            // host decides the write with the grant in hand.
            own_host
                .host
                .workspaces()
                .commit_own_membership(
                    &workspace.to_string(),
                    &self.custody.device_cert,
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                        app: app.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let message = neighbour_wire::NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&own),
            app_id: app.clone(),
            cluster_id: workspace.to_string(),
            record_id: record.clone(),
            envelope: Box::new(envelope.clone()),
        };
        let digest =
            *blake3::hash(&postcard::to_allocvec(&message).map_err(|error| error.to_string())?)
                .as_bytes();
        let deadline = Instant::now()
            .checked_add(KEPT_WITHIN)
            .ok_or("the clock cannot count a write's wait")?;
        let ws = super::workspace_topic(workspace)?;
        let host = own_host
            .host
            .workspaces()
            .direct()
            .link(ws)
            .ok_or("this device's host does not serve the workspace yet")?;
        self.live_by(workspace, deadline).await?.send(message)?;
        self.wait_until(
            &host,
            deadline,
            "the workspace's host did not keep the write in time",
            || {
                Ok(host.read(|peer| {
                    peer.lookup_exact_local_document(&app, &record, &envelope, SystemTime::now())
                        .is_ok_and(|found| found.is_some())
                }))
            },
        )
        .await?;
        Ok(OwnerCommitReceipt {
            binding,
            generation: generation.max(1),
            aggregate_hash: digest,
        })
    }

    /// This device's link to `workspace`'s host, up and ready to carry a frame,
    /// waited for until `deadline`.
    async fn live_by(
        &self,
        workspace: WorkspaceId,
        deadline: Instant,
    ) -> Result<Arc<tilo_peer_state::direct::DirectSync>, String> {
        let connected = self.connected(workspace)?;
        loop {
            let waiting = connected.notified();
            tokio::pin!(waiting);
            waiting.as_mut().enable();
            if let Some(link) = self.live(workspace) {
                return Ok(link);
            }
            if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), waiting)
                .await
                .is_err()
            {
                return Err("the workspace's link to its host is not up".into());
            }
        }
    }

    /// Wait until `check` holds as the host's replica changes, or `deadline`
    /// passes and `missing` says what did not happen.
    async fn wait_until(
        &self,
        host: &tilo_peer_state::direct::DirectSync,
        deadline: Instant,
        missing: &str,
        mut check: impl FnMut() -> Result<bool, String>,
    ) -> Result<(), String> {
        let mut changes = host.changes();
        let waited = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            loop {
                if check()? {
                    return Ok(());
                }
                changes
                    .changed()
                    .await
                    .map_err(|_| format!("{missing}: the link closed"))?;
            }
        });
        match waited.await {
            Ok(kept) => kept,
            Err(_) => Err(missing.to_owned()),
        }
    }
}

#[cfg(test)]
impl Joined {
    /// `app`'s `record` as this device's host keeps it in `workspace`: its latest
    /// whole write, out of the history the host keeps for the workspace's
    /// devices, opened with the channel's keys. `Err` says where the write
    /// stops on the way there, so one that never arrives is explained rather
    /// than just missing.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the host is this profile's own: the handle holds no lock"
    )]
    pub(in crate::standalone) fn test_kept_document(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Vec<u8>, String> {
        let host = self
            .room_host()
            .ok_or("this profile runs no workspace host")?;
        let ws = super::workspace_topic(workspace)?;
        let link = host
            .workspaces()
            .direct()
            .link(ws)
            .ok_or("this device's host does not serve the workspace")?;
        let store = link
            .store()
            .cloned()
            .ok_or("this device's host keeps no history of the workspace")?;
        let kept = store
            .replay(
                ws,
                &[],
                tilo_peer_state::store::Budget {
                    max_frames: usize::MAX,
                    max_bytes: u64::MAX,
                },
            )
            .map_err(|error| error.to_string())?;
        let route = collab_types::ChannelRoute::new(app, record);
        let keys = self
            .channel_keys()
            .get()
            .ok_or("this device holds no channel keys")?
            .record_keys(workspace, &route)
            .ok_or_else(|| {
                format!(
                    "the host keeps {} writes but this device takes part in no channel of {app}'s {record}",
                    kept.frames.len()
                )
            })?
            .map_err(|error| {
                format!(
                    "the host keeps {} writes but this device holds no keys for {app}'s {record}: {error}",
                    kept.frames.len()
                )
            })?;
        let mut kept_of_the_record = 0_usize;
        for message in kept.frames.iter().rev() {
            let neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                record_id,
                envelope,
                ..
            } = message
            else {
                continue;
            };
            if app_id != app || record_id != record {
                continue;
            }
            kept_of_the_record = kept_of_the_record.saturating_add(1);
            let metadata = envelope
                .encryption
                .ok_or_else(|| format!("{app}'s {record} reached the host unsealed"))?;
            let ciphertext = collab_types::sealed_ciphertext(envelope)
                .map_err(|error| format!("{app}'s {record} is not sealed as one frame: {error}"))?;
            let plaintext = keys
                .open_content(envelope.artifact, &metadata, &ciphertext)
                .map_err(|error| format!("{app}'s {record} does not open: {error}"))?;
            match collab_types::DocumentFrame::decode(&plaintext)
                .map_err(|error| format!("{app}'s {record} is not a document frame: {error}"))?
            {
                collab_types::DocumentFrame::Whole(
                    collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes),
                ) => return Ok(bytes),
                collab_types::DocumentFrame::Segment(_) => {}
            }
        }
        Err(format!(
            "the host keeps {} writes, {kept_of_the_record} of them {app}'s {record}, none whole",
            kept.frames.len()
        ))
    }
}
