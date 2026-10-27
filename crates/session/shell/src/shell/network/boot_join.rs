//! Unattended join of the room a local device host governs, for a session
//! nobody sits at (a provider guest). The host issues the invitation; the
//! session redeems it through the same canonical native enrollment a person
//! uses, into the active workspace, once per room.
use super::{Path, RoomId, ShellNetwork};
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use std::time::{Duration, Instant};

/// A file holding one invitation text to redeem at startup.
pub const JOIN_INVITATION_FILE_ENV: &str = "TILO_SHELL_JOIN_INVITATION_FILE";
/// Set when the host's operator makes this session an administrator of the
/// room once it has joined: startup then waits for that authority before the
/// session serves agents, whose first acts may need it.
pub const JOIN_ADMINISTRATOR_ENV: &str = "TILO_SHELL_JOIN_ADMINISTRATOR";
const JOIN_DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);

/// What the startup join did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BootJoin {
    Joined(String),
    AlreadyMember(String),
}

impl ShellNetwork {
    /// Redeem the invitation [`JOIN_INVITATION_FILE_ENV`] names before the
    /// session builds anything on the active workspace's room (its broker and
    /// agent endpoints bind that room). Nothing happens when it is unset; a
    /// failure leaves the session in its own room.
    pub(crate) fn join_at_boot(&self) {
        let Some(path) = std::env::var_os(JOIN_INVITATION_FILE_ENV) else {
            return;
        };
        let room = match self.boot_join(Path::new(&path)) {
            Ok(BootJoin::Joined(room)) => {
                tracing::info!(%room, "boot join: room joined");
                room
            }
            Ok(BootJoin::AlreadyMember(room)) => {
                tracing::info!(%room, "boot join: already a member");
                room
            }
            Err(error) => {
                tracing::warn!(%error, "boot join failed");
                return;
            }
        };
        if std::env::var_os(JOIN_ADMINISTRATOR_ENV).is_some() {
            match self.await_administration(&room) {
                Ok(()) => tracing::info!(%room, "boot join: administering the room"),
                Err(error) => tracing::warn!(%room, %error, "boot join: not an administrator"),
            }
        }
    }

    /// Wait until the joined `room` names this session an administrator.
    fn await_administration(&self, room: &str) -> Result<(), String> {
        let target = RoomId::parse(room).ok_or("invitation names an invalid room")?;
        let tree = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .clone();
        let person = crate::shell::state::local_user_id_for_room(&target);
        let deadline = Instant::now()
            .checked_add(JOIN_DEADLINE)
            .ok_or("boot join deadline overflows")?;
        loop {
            // Startup has not reached the event loop yet: fold in what the
            // host sent here.
            self.tick();
            let now = tilo_clock::try_unix_now()
                .ok_or_else(|| "the system clock is before the Unix epoch".to_owned())?;
            if tree.read().desktop.workspaces.iter().any(|workspace| {
                workspace.room() == target
                    && workspace.may_govern_at(
                        &person,
                        tilo_action_policy::roles::governance::GRANT,
                        now,
                    )
            }) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("the workspace host did not make this session an administrator".into());
            }
            std::thread::sleep(POLL);
        }
    }

    /// Join the room the invitation in `path` names, as the workspace on
    /// screen, unless this device holds it already; a restart keeps its
    /// membership.
    pub(crate) fn boot_join(&self, path: &Path) -> Result<BootJoin, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .trim()
            .to_owned();
        let now = tilo_clock::try_unix_now_ns()
            .ok_or_else(|| "the system clock is before the Unix epoch".to_owned())?;
        let room = tilo_peer_state::join::decode_and_verify_invitation_text(
            &text,
            None,
            &tilo_user_identity::RevocationList::default(),
            now,
        )?
        .grant()
        .room
        .clone();
        let target = RoomId::parse(&room).ok_or("invitation names an invalid room")?;
        let tree = self
            .replica
            .lock()
            .map_err(|_| "native replica unavailable")?
            .tree
            .clone();
        let member = tree.read().holds_room(target);
        if member {
            // Restoring the native owner reconnects every retained room.
            self.native_owner()?;
            return Ok(BootJoin::AlreadyMember(room));
        }
        let deadline = Instant::now()
            .checked_add(JOIN_DEADLINE)
            .ok_or("boot join deadline overflows")?;
        let mut response = self.accept_peer_invitation(&text)?;
        // The joined workspace, the room, is the one on screen.
        {
            let mut tree = tree.write();
            if let Some(joined) = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == target)
                .map(|workspace| workspace.id)
            {
                tree.desktop.active = joined;
            }
        }
        loop {
            match response {
                PeerSyncResponse::EnrollmentJoined { room, .. }
                | PeerSyncResponse::Message(room) => return Ok(BootJoin::Joined(room)),
                PeerSyncResponse::EnrollmentPending { id } => {
                    if Instant::now() >= deadline {
                        let _ = self.cancel_enrollment(id);
                        return Err("boot join deadline elapsed".into());
                    }
                    std::thread::sleep(POLL);
                    response = self.poll_enrollment(id)?;
                }
                PeerSyncResponse::PendingInvitations(_)
                | PeerSyncResponse::PairingInvite { .. }
                | PeerSyncResponse::DevicesAsking(_)
                | PeerSyncResponse::Directory(_)
                | PeerSyncResponse::JoinedWorkspace { .. }
                | PeerSyncResponse::Publication(_) => {
                    return Err("unexpected enrollment response".into());
                }
            }
        }
    }
}
