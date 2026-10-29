//! The profile's own workspace, placed on the room host this runtime runs.
//!
//! The runtime runs the same room host every device runs
//! ([`HostMode::Device`](tilo_room_host::host::HostMode::Device)), in
//! process: the host creates the profile's room and issues this device the
//! invitation its replica redeems ([`RoomHost::place_own_room`]), and from
//! then on the room is one of the rooms this profile joined, its session
//! carried in process ([`RoomHost::connect_local`]). The host takes part in
//! the room's channels with this device's own leaf, so this device takes a
//! channel's keys there ([`Joined::hosted_channel_keys`]).
//!
//! An app's write to the room goes to the host as a commit and is
//! acknowledged once the host keeps it. A write to a record the room does
//! not hold yet is taken only in a resource kind the app's installed
//! manifest declares; in dev mode, an app that is not installed is trusted
//! with the kind its own build names. The room lets the app author its
//! records first, as the person who placed it there.
use super::Joined;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tilo_desktop_model::RoomId;
use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use tilo_relay_session::{CoeditPayload, RelayCoeditSession, workspace_join::WorkspaceJoinClient};
use tilo_room_host::host::RoomHost;

/// How long a write waits for the room's session and for its host to keep it.
const KEPT_WITHIN: Duration = Duration::from_secs(10);

/// The resource kinds an installed app declares its records hold; `None`
/// for an app this profile has not installed.
pub(in crate::standalone) type DeclaredKinds =
    Box<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// In which kinds an app may write a record the room does not hold yet.
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

/// The room host this profile runs, and what it lets apps create there.
pub(super) struct OwnHost {
    host: Arc<RoomHost>,
    creation: RecordCreation,
}

impl Joined {
    /// Run `host`, this device's room host, from now on: the rooms it hosts
    /// are reached in process, and apps create records there as `creation`
    /// lets them.
    ///
    /// # Errors
    /// A host is already running.
    pub(in crate::standalone) fn install_host(
        &self,
        host: Arc<RoomHost>,
        creation: RecordCreation,
    ) -> Result<(), String> {
        self.own_host
            .set(OwnHost { host, creation })
            .map_err(|_| "this profile runs one room host".into())
    }

    fn room_host(&self) -> Option<&Arc<RoomHost>> {
        self.own_host.get().map(|own| &own.host)
    }

    fn own_device(&self) -> [u8; 32] {
        self.custody.device_signing_key.verifying_key().to_bytes()
    }

    /// Place `room`, the profile's workspace called `name`, on this
    /// device's host and enroll in it, unless this profile holds it
    /// already: the room is held when this returns, and synced once its
    /// session is held ([`Self::hold_session`]).
    ///
    /// # Errors
    /// No host runs, it refused the room, or the enrollment failed.
    pub(in crate::standalone) async fn place(
        &self,
        room: RoomId,
        name: &str,
    ) -> Result<(), String> {
        if self.owner.contains_room(room) {
            return Ok(());
        }
        let invitation = self
            .room_host()
            .ok_or("this profile runs no room host")?
            .place_own_room(&room.to_string(), name, &self.custody.device_cert)
            .await?;
        if self.enroll(&invitation).await? != room {
            return Err("this device's host placed another room".into());
        }
        Ok(())
    }

    /// Whether `room` is hosted by this device's own host.
    pub(in crate::standalone) fn hosted_here(&self, room: RoomId) -> bool {
        let own = self.own_device();
        self.owner
            .with_current(room, |view| {
                Ok(view
                    .transport_hint()
                    .is_ok_and(|hint| tilo_joined_room::session::hosted_by(hint, &own)))
            })
            .unwrap_or(false)
    }

    /// The workspace-join exchange `invitation` asks for: with this
    /// device's own host in process when the invitation names it, an
    /// endpoint cannot dial itself; over the network otherwise.
    pub(super) fn join_client(&self, invitation: &str) -> WorkspaceJoinClient {
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
                .any(|hint| tilo_joined_room::session::hosted_by(hint, &own))
        });
        match self.room_host().filter(|_| hosted_here) {
            Some(host) => WorkspaceJoinClient::local(host.join_service(), own),
            None => WorkspaceJoinClient::new(self.endpoint.clone()),
        }
    }

    /// The co-edit session of workspace `ws` with the host `hint` pins: in
    /// process with this device's own host, dialled otherwise.
    pub(super) async fn connect(
        &self,
        ws: u64,
        hint: &tilo_peer_state::join::TransportHint,
        device: tilo_user_identity::DeviceCert,
        revocations: tilo_user_identity::RevocationList,
    ) -> Result<RelayCoeditSession, String> {
        let own = self.own_device();
        let key = self.custody.device_signing_key.clone();
        if !tilo_joined_room::session::hosted_by(hint, &own) {
            return Ok(tilo_joined_room::session::dial(
                self.endpoint.clone(),
                ws,
                hint,
                device,
                key,
                revocations,
            )
            .await?);
        }
        let client = self
            .room_host()
            .ok_or("this device's room host is not running")?
            .connect_local(device, key, revocations)
            .await
            .map_err(|error| error.to_string())?;
        Ok(tilo_joined_room::session::join_local(
            client, ws, hint, own,
        )?)
    }

    /// `channel`'s content keys from this device's own room host when it
    /// hosts `room`: the host's leaf is this device's, so the keys are taken
    /// there. `None` for a room hosted elsewhere.
    pub(in crate::standalone) fn hosted_channel_keys(
        &self,
        room: RoomId,
        channel: collab_types::ChannelId,
        route: Option<&collab_types::ChannelRoute>,
        create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        if !self.hosted_here(room) {
            return None;
        }
        let host = Arc::clone(self.room_host()?);
        let (room, route) = (room.to_string(), route.cloned());
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        // Answered on the runtime's own tasks, never on the asking thread:
        // the broker asks from its connections' runtime, where waiting on
        // another runtime panics.
        tilo_rt::app().spawn(async move {
            let keys = host
                .rooms()
                .own_channel_keys(&room, channel, route.as_ref(), create)
                .await;
            let _ = answer.send(keys);
        });
        answered
            .recv()
            .unwrap_or_else(|_| Some(Err("the runtime stopped".into())))
    }

    /// Commit `request`, an app's write to `room`, a room this device's host
    /// hosts, at the host: answered once the host keeps it.
    pub(super) async fn commit_hosted(
        &self,
        room: RoomId,
        request: OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> Result<OwnerCommitReceipt, String> {
        let binding = request.binding()?;
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = request.operation
        else {
            return Err("an app writes documents to its room".into());
        };
        let own = self.own_device();
        if envelope.author.as_bytes() != &own {
            return Err("the document's signer differs from this device".into());
        }
        let (held, authors, generation) = self.owner.with_current(room, |view| {
            let now = SystemTime::now();
            let member = view.peer().permissions(own, now)?;
            Ok((
                view.peer().holds_document(&app, &record),
                member
                    .policy
                    .app_authors(&app, &tilo_action_policy::Context::at_time(now)),
                view.generation(),
            ))
        })?;
        let own_host = self
            .own_host
            .get()
            .ok_or("this profile runs no room host")?;
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
            // which signs the room's history under this device's key.
            own_host
                .host
                .rooms()
                .commit_own_membership(
                    &room.to_string(),
                    &self.custody.device_cert,
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                        app: app.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let message = neighbour_wire::NeighbourMsg::AppDocument {
            peer: u64::from_le_bytes([
                own[0], own[1], own[2], own[3], own[4], own[5], own[6], own[7],
            ]),
            app_id: app,
            cluster_id: room.to_string(),
            record_id: record,
            envelope,
        };
        let (channel, retention) = neighbour_wire::retention(&message, false)
            .ok_or("the write has no channel to be kept on")?;
        let body = postcard::to_allocvec(&message).map_err(|error| error.to_string())?;
        let digest = *blake3::hash(&body).as_bytes();
        let mut op = [0; 16];
        op.copy_from_slice(&digest[..16]);
        let payload = match retention {
            neighbour_wire::Retention::Checkpoint => CoeditPayload::Checkpoint { channel, body },
            neighbour_wire::Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
        };
        let deadline = Instant::now()
            .checked_add(KEPT_WITHIN)
            .ok_or("the clock cannot count a write's wait")?;
        let session = self.live_by(room, deadline).await?;
        let (kept, keeping) = tokio::sync::oneshot::channel();
        self.lock_commits().insert(op, kept);
        if let Err(error) = session.commit(op, &payload) {
            self.lock_commits().remove(&op);
            return Err(error.to_string());
        }
        let answer =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), keeping).await;
        self.lock_commits().remove(&op);
        match answer {
            Ok(Ok(kept)) => kept?,
            Ok(Err(_)) => {
                return Err("the room's session ended before its host kept the write".into());
            }
            Err(_) => return Err("the room's host did not keep the write in time".into()),
        }
        Ok(OwnerCommitReceipt {
            binding,
            generation: generation.max(1),
            aggregate_hash: digest,
        })
    }

    /// `room`'s connected session, waited for until `deadline`.
    async fn live_by(
        &self,
        room: RoomId,
        deadline: Instant,
    ) -> Result<Arc<RelayCoeditSession>, String> {
        loop {
            let changed = self.live_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(session) = self.live(room) {
                return Ok(session);
            }
            if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                .await
                .is_err()
            {
                return Err("the room's session with its host is not up".into());
            }
        }
    }

    /// The host answered commit `op`: kept, or refused for `reason`.
    pub(super) fn commit_answered(&self, op: [u8; 16], answer: Result<(), String>) {
        let waiting = self.lock_commits().remove(&op);
        if let Some(waiting) = waiting {
            let _ = waiting.send(answer);
        }
    }

    fn lock_commits(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<[u8; 16], super::CommitWaiter>> {
        self.commits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Joined {
    /// `app`'s `record` as this device's host keeps it in `room`: its latest
    /// whole write, opened with the channel's keys. Waits on the host: call
    /// it from a thread that may block.
    pub(in crate::standalone) fn test_kept_document(
        &self,
        room: RoomId,
        app: &str,
        record: &str,
    ) -> Option<Vec<u8>> {
        let host = Arc::clone(self.room_host()?);
        let ws = super::room_topic(room).ok()?;
        let retained = tilo_rt::app()
            .handle()
            .block_on(async move { host.room_records().retained(ws).await })
            .ok()?;
        let route = collab_types::ChannelRoute::new(app, record);
        let keys = self
            .hosted_channel_keys(room, route.channel(&room.to_string()), Some(&route), false)?
            .ok()?;
        retained.iter().rev().find_map(|kept| {
            let neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                record_id,
                envelope,
                ..
            } = postcard::from_bytes(&kept.body).ok()?
            else {
                return None;
            };
            if app_id != app || record_id != record {
                return None;
            }
            let metadata = envelope.encryption?;
            let ciphertext = collab_types::sealed_ciphertext(&envelope).ok()?;
            let plaintext = keys
                .open_content(envelope.artifact, &metadata, &ciphertext)
                .ok()?;
            match collab_types::DocumentFrame::decode(&plaintext).ok()? {
                collab_types::DocumentFrame::Whole(
                    collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes),
                ) => Some(bytes),
                collab_types::DocumentFrame::Segment(_) => None,
            }
        })
    }
}
