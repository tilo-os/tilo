use super::{Arc, Mutex, NetCmd, RoomId, ShellNetwork, SystemTime, room_topic_id};
use crate::{
    network::invite::enrollment_driver,
    shell::{
        native_workspace_owner::NativeWorkspaceOwner,
        peer_sync_overlay::PeerSyncResponse,
        state::{WorkspaceId, WorkspaceTreeHandle},
    },
};
use std::{
    sync::MutexGuard,
    time::{Duration, Instant},
};

/// A failed background open of the native owner is tried again no sooner
/// than this: a second writer of the state file can hold it for long.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct State {
    owner: Mutex<Option<NativeWorkspaceOwner>>,
    /// The open [`ShellNetwork::opened_native_owner`] runs off the frame loop.
    background: Arc<Mutex<Background>>,
    pub(super) attempts: Mutex<std::collections::HashMap<u64, enrollment_driver::Attempt>>,
}

#[derive(Default)]
struct Background {
    running: bool,
    failed_at: Option<Instant>,
    /// The owner the last background open produced, until it is retained.
    opened: Option<NativeWorkspaceOwner>,
}

type Retained<'a> = MutexGuard<'a, Option<NativeWorkspaceOwner>>;

/// Where the native owner stands, for a caller that must not wait for it.
pub(crate) enum NativeOwnerStatus {
    /// Open and retained.
    Open(NativeWorkspaceOwner),
    /// Being opened off the caller's thread; the loop is woken once it is.
    Opening,
    /// Neither open nor opening: the tree has no state file, or the last
    /// open failed and is not tried again yet.
    Unavailable,
}

impl NativeOwnerStatus {
    pub(crate) fn open(self) -> Option<NativeWorkspaceOwner> {
        match self {
            Self::Open(owner) => Some(owner),
            Self::Opening | Self::Unavailable => None,
        }
    }
}

impl ShellNetwork {
    /// The native owner, opened here if it is not yet: binding its writer
    /// and publishing its reopened generation, which can wait for the disk.
    /// The frame loop asks [`Self::native_owner_status`] instead.
    pub(crate) fn native_owner(&self) -> Result<NativeWorkspaceOwner, String> {
        let tree = self.tree.clone();
        let retained = self
            .native_enrollment
            .owner
            .lock()
            .map_err(|_| "native owner retention unavailable")?;
        if let Some(owner) = retained.as_ref() {
            return Ok(owner.clone());
        }
        // An owner the background open finished is this writer's: opening
        // again would only look it up.
        let finished = self
            .native_enrollment
            .background
            .lock()
            .ok()
            .and_then(|mut background| background.opened.take());
        let owner = match finished {
            Some(owner) => owner,
            None => crate::shell::native_workspace_owner::open(tree.clone())?,
        };
        Ok(self.retain_native_owner(retained, &tree, owner))
    }

    /// Who stewards each room's channels, as the native owner, once open,
    /// has the room: this device's enrolled device and the room's governor.
    /// The same device when this one governs it.
    pub(crate) fn room_stewards(
        network: &std::sync::Arc<Self>,
    ) -> crate::shell::channel_policy::Stewards {
        let network = std::sync::Arc::clone(network);
        Box::new(move |room| {
            network
                .opened_native_owner()?
                .with_current(room, |view| {
                    Ok((
                        view.device().device_public_key,
                        view.governor().device_public_key,
                    ))
                })
                .ok()
        })
    }

    /// The native owner if it is already open, for the frame loop, which
    /// must never wait for the state file ([`Self::native_owner_status`]).
    pub(crate) fn opened_native_owner(&self) -> Option<NativeWorkspaceOwner> {
        self.native_owner_status().open()
    }

    /// Where the native owner stands, never waiting for the state file:
    /// opening an owner takes its writer's lease, syncs its directory chain
    /// on first use and publishes it, seconds on a loaded disk. An owner
    /// already attached to the tree's writer is retained here. When none
    /// is, one is opened on a thread of its own (again at most every
    /// [`REOPEN_AFTER`] after a failure), the loop is woken once that open
    /// ends, and this answers [`NativeOwnerStatus::Opening`].
    pub(crate) fn native_owner_status(&self) -> NativeOwnerStatus {
        // The tree before the owner, in the order `native_owner` takes them.
        let tree = self.tree.clone();
        // Held only by a caller opening the owner itself.
        let retained = match self.native_enrollment.owner.try_lock() {
            Ok(retained) => retained,
            Err(std::sync::TryLockError::WouldBlock) => return NativeOwnerStatus::Opening,
            Err(std::sync::TryLockError::Poisoned(_)) => return NativeOwnerStatus::Unavailable,
        };
        if let Some(owner) = retained.as_ref() {
            return NativeOwnerStatus::Open(owner.clone());
        }
        let Ok(mut background) = self.native_enrollment.background.lock() else {
            return NativeOwnerStatus::Unavailable;
        };
        let opened = background
            .opened
            .take()
            .or_else(|| crate::shell::native_workspace_owner::attached(&tree));
        if let Some(owner) = opened {
            drop(background);
            return NativeOwnerStatus::Open(self.retain_native_owner(retained, &tree, owner));
        }
        drop(retained);
        if background.running {
            return NativeOwnerStatus::Opening;
        }
        // Without a state file no open can succeed: say so now, rather than
        // once a thread found out.
        if tree.read().persist_path.is_none()
            || background
                .failed_at
                .is_some_and(|failed| failed.elapsed() < REOPEN_AFTER)
        {
            return NativeOwnerStatus::Unavailable;
        }
        background.running = true;
        drop(background);
        let slot = Arc::clone(&self.native_enrollment.background);
        let finished = Arc::clone(&slot);
        let wake = Arc::clone(&self.event_wake);
        let spawned = std::thread::Builder::new()
            .name("native-owner-open".into())
            .spawn(move || {
                let opened = crate::shell::native_workspace_owner::open(tree);
                let Ok(mut background) = finished.lock() else {
                    return;
                };
                background.running = false;
                match opened {
                    Ok(owner) => {
                        background.opened = Some(owner);
                        background.failed_at = None;
                    }
                    Err(error) => {
                        background.failed_at = Some(Instant::now());
                        tracing::debug!(%error, "native owner unavailable");
                    }
                }
                drop(background);
                // Either way the loop has news: inbound events it held back
                // for the open go on now, with the owner or without one.
                if let Some(wake) = wake.get() {
                    wake();
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot start opening the native owner");
            if let Ok(mut background) = slot.lock() {
                background.running = false;
                background.failed_at = Some(Instant::now());
            }
            return NativeOwnerStatus::Unavailable;
        }
        NativeOwnerStatus::Opening
    }

    /// Keep `owner` for every later lookup, and connect the rooms it holds.
    fn retain_native_owner(
        &self,
        mut retained: Retained<'_>,
        tree: &WorkspaceTreeHandle,
        owner: NativeWorkspaceOwner,
    ) -> NativeWorkspaceOwner {
        *retained = Some(owner.clone());
        drop(retained);
        let rooms = tree.read().rooms();
        for room in rooms {
            if owner.with_current(room, |_| Ok(())).is_ok()
                && let Err(error) = self.connect_native_room(owner.clone(), room)
            {
                tracing::warn!(%error, "native owner restored while data transport is unavailable");
            }
        }
        owner
    }
    pub(crate) fn runtime_handle(&self) -> Result<tokio::runtime::Handle, String> {
        self.runtime
            .as_ref()
            .map(|runtime| runtime.handle().clone())
            .ok_or_else(|| "native runtime unavailable".into())
    }
    /// Join the workspace a peer invitation names, as a workspace of its own
    /// that is that room from the start and says Joining… until its replica
    /// installs ([`crate::shell::state::WorkspaceTree::join_native_room`]).
    /// The workspace in front stays what it is.
    ///
    /// # Errors
    ///
    /// Returns an error if the invitation does not verify, the replica,
    /// device custody or native owner is unavailable, or the enrollment
    /// cannot be queued.
    pub fn accept_peer_invitation(&self, token: &str) -> Result<PeerSyncResponse, String> {
        self.join_by_invitation(token, enrollment_driver::Via::Iroh)
    }

    /// [`Self::accept_peer_invitation`], reaching the issuing host `via` its
    /// iroh endpoint or its HTTPS routes.
    pub(crate) fn join_by_invitation(
        &self,
        token: &str,
        via: enrollment_driver::Via,
    ) -> Result<PeerSyncResponse, String> {
        let now = tilo_clock::try_unix_now_ns()
            .ok_or_else(|| "the system clock is before the Unix epoch".to_owned())?;
        let join = tilo_peer_state::join::decode_and_verify_invitation_text(
            token.trim(),
            None,
            &tilo_user_identity::RevocationList::new(),
            now,
        )?;
        let room = RoomId::parse(&join.grant().room).ok_or("invitation names an invalid room")?;
        let tree = self.tree.clone();
        if tree.read().holds_room(room) {
            return Ok(PeerSyncResponse::Message(room.to_string()));
        }
        let local = tree
            .write()
            .join_native_room(&crate::shell::i18n::t("workspace-joined-name"), room)
            .ok_or("that invitation is for a room that is not joined")?;
        self.accept_invitation_via(local, token.trim(), via)
    }

    /// Redeem `token` into `local`, the workspace made for the room it
    /// names, reaching the issuing host `via` its iroh endpoint or its HTTPS
    /// routes.
    pub(crate) fn accept_invitation_via(
        &self,
        local: WorkspaceId,
        token: &str,
        via: enrollment_driver::Via,
    ) -> Result<PeerSyncResponse, String> {
        let owner = self.native_owner()?;
        let room = self
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == local)
            .map(crate::shell::state::Workspace::room)
            .ok_or("target workspace disappeared")?;
        let topic = room_topic_id(room);
        let certificate = self
            .host_identity
            .as_ref()
            .ok_or("native device custody unavailable")?
            .current_native_material(topic)?
            .0;
        // A lost UI reply after the local barrier is reconciled from the actual
        // committed enrollment, never by another remote redemption.
        if owner
            .with_current(room, |view| {
                Ok(view.device() == &certificate
                    && view.enrollment_invitation().is_ok_and(|invitation| {
                        tilo_peer_state::join::encode_invitation_text(invitation) == token
                    }))
            })
            .unwrap_or(false)
        {
            self.connect_native_room(owner, room)?;
            return Ok(PeerSyncResponse::Message(room.to_string()));
        }
        let mut attempts = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?;
        if attempts.len() >= 8 || attempts.values().any(|attempt| attempt.local() == local) {
            return Err("native enrollment already pending or quota reached".into());
        }
        let (command, attempt) = enrollment_driver::command(
            owner,
            local,
            topic,
            token.to_owned(),
            via,
            self.enrollment_landed(),
        )?;
        let id = attempt.id();
        self.cmd_tx
            .try_send(NetCmd::NativeEnrollment(command))
            .map_err(|_| "native enrollment command queue unavailable")?;
        attempts.insert(id, attempt);
        drop(attempts);
        Ok(PeerSyncResponse::EnrollmentPending { id })
    }
    /// Finish the pending enrollment `id` once its remote exchange completed.
    pub fn poll_enrollment(&self, id: u64) -> Result<PeerSyncResponse, String> {
        // Which room and slot it is for: finishing it forgets its attempt.
        let target = self.enrollment_target(id);
        let attempt = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .get(&id)
            .cloned()
            .ok_or("native enrollment attempt no longer active")?;
        let result = attempt.finish(|enrollment| {
            let identity = self
                .host_identity
                .as_ref()
                .ok_or("native custody unavailable")?;
            let (certificate, _) = identity.current_native_material(attempt.identity_topic())?;
            if &certificate != enrollment.local_device() {
                return Err("selected native custody changed before installation".into());
            }
            let room =
                RoomId::parse(&enrollment.bootstrap().room).ok_or("invalid enrolled room")?;
            // Preserve the explicitly selected identity when the target slot acquires
            // its new room. This is existing custody storage, not semantic authority.
            if identity.identity_paths.is_some() {
                let registry = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|e| e.to_string())?;
                let profile = hex::encode(certificate.user_public_key);
                registry
                    .bind_workspace(&room.to_string(), &profile)
                    .map_err(|e| e.to_string())?;
                if identity.current_native_material(room_topic_id(room))?.0 != certificate {
                    return Err("native room custody did not retain selected certificate".into());
                }
            }
            attempt.owner().install(&enrollment, SystemTime::now())?;
            self.check_native_custody(attempt.owner(), room)?;
            if !self.join_topic(room_topic_id(room)) {
                return Err("native room committed; transport queue unavailable".into());
            }
            self.connect_native_room(attempt.owner().clone(), room)?;
            Ok(room)
        });
        let Some(result) = result else {
            return Ok(PeerSyncResponse::EnrollmentPending { id });
        };
        self.native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .remove(&id);
        match target {
            Some((room, local)) => {
                self.placement_enrollment_ended(room, result.is_ok());
                self.enrollment_ended(
                    room,
                    local,
                    result.as_ref().map(|_| ()).map_err(Clone::clone),
                );
            }
            None => {
                if let Err(error) = &result {
                    tracing::error!(%error, "a workspace was not enrolled");
                }
            }
        }
        result.map(|room| PeerSyncResponse::EnrollmentJoined {
            id,
            room: room.to_string(),
        })
    }
    /// Cancel the pending enrollment `id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the attempt is no longer active or cannot be cancelled.
    pub fn cancel_enrollment(&self, id: u64) -> Result<PeerSyncResponse, String> {
        let attempt = self
            .native_enrollment
            .attempts
            .lock()
            .map_err(|_| "native enrollment registry unavailable")?
            .remove(&id)
            .ok_or("native enrollment attempt no longer active")?;
        attempt.cancel()?;
        Ok(PeerSyncResponse::Message(
            "Enrollment cancellation requested".into(),
        ))
    }
}
