//! Unattended join of the room a local device host governs, for a session
//! nobody sits at (a provider guest). The host issues the invitation; the
//! session redeems it through the same canonical native enrollment a person
//! uses, into the active workspace, once per room.
use super::*;
use crate::shell::peer_sync_overlay::PeerSyncResponse;
use std::time::{Duration, Instant};

/// A file holding one invitation text to redeem at startup.
pub const JOIN_INVITATION_FILE_ENV: &str = "TILO_SHELL_JOIN_INVITATION_FILE";
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
        match self.boot_join(Path::new(&path)) {
            Ok(BootJoin::Joined(room)) => tracing::info!(%room, "boot join: room joined"),
            Ok(BootJoin::AlreadyMember(room)) => {
                tracing::info!(%room, "boot join: already a member");
            }
            Err(error) => tracing::warn!(%error, "boot join failed"),
        }
    }

    /// Redeem the invitation in `path` into the active workspace unless a
    /// workspace already holds its room; a restart keeps its membership.
    pub(crate) fn boot_join(&self, path: &Path) -> Result<BootJoin, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .trim()
            .to_owned();
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos(),
        )
        .map_err(|error| error.to_string())?;
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
        let (local, member) = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "native replica unavailable")?;
            let tree = replica.tree.read();
            let member = tree
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == target);
            (tree.desktop.active, member)
        };
        if member {
            // Restoring the native owner reconnects every retained room.
            self.native_owner()?;
            return Ok(BootJoin::AlreadyMember(room));
        }
        let deadline = Instant::now() + JOIN_DEADLINE;
        let mut response = self.accept_peer_invitation(local, &text)?;
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
                PeerSyncResponse::PendingInvitations(_) => {
                    return Err("unexpected enrollment response".into());
                }
            }
        }
    }
}
