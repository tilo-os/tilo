//! A shortcut saved into a room a host governs: asked of that host as the
//! member's own business (`tilo.member` `shortcut.save`), signed by this
//! device as its person's. The host, the owner's device or a node, commits it
//! into the room document every member admits; nothing is written here.
//!
//! A save is done once the room as this device holds it carries it: the
//! shortcut runs from here the moment its save ends, as a grant is in force
//! before the call it was given for goes on ([`super::grant_in_force`]).
//! The wait lasts as long as the room's sessions with its host do.
use std::sync::Arc;

use super::relay_rooms::receipt_value;
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::network::actor::room_governance::trace;
use crate::shell::state::RoomId;
use tilo_relay_session::rooms::SAVE_SHORTCUT;

impl ShellNetwork {
    /// This device's key and its certificate in `room`, the custody its
    /// changes there are signed with.
    ///
    /// # Errors
    /// This device holds no certified identity, or none for the room.
    pub(crate) fn room_signer(
        &self,
        room: RoomId,
    ) -> Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert), String> {
        let identity = self
            .host_identity
            .as_ref()
            .ok_or("this device has no certified identity")?;
        let owner = self.native_owner()?;
        let certificate = owner.with_current(room, |view| {
            identity
                .current_native_material(view.workspace())
                .map(|(certificate, _)| certificate)
        })?;
        if certificate.device_public_key != identity.device_signing_key.verifying_key().to_bytes() {
            return Err("this device's certificate is not its key's".into());
        }
        Ok((identity.device_signing_key.clone(), certificate))
    }

    /// Have the host governing `room` commit `change`, its saved shortcut
    /// already signed by this device. `None` when no host governs the room.
    /// The wait for the host's answer lasts as long as the room's sessions
    /// with its host do.
    pub(crate) async fn save_shortcut_at_host(
        self: Arc<Self>,
        room: RoomId,
        change: tilo_automation::ShortcutChange,
    ) -> Option<Result<(), String>> {
        let governor = self.host_governor(room);
        trace::note(|| format!("shortcut save in {room}: host governor {governor:?}"));
        let host = match governor {
            Ok(None) => return None,
            Ok(Some(host)) => host,
            Err(error) => return Some(Err(error)),
        };
        let asked = (|| {
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                "change".into(),
                serde_json::to_string(&change)
                    .map_err(|error| error.to_string())?
                    .into(),
            );
            self.ask_as_member(room, host, SAVE_SHORTCUT, arguments)
        })();
        let answer = match asked {
            Ok(answer) => answer,
            Err(error) => return Some(Err(error)),
        };
        let committed = tokio::task::spawn_blocking(move || answer.recv())
            .await
            .map_err(|_| "the request to the workspace's host stopped".to_owned())
            .and_then(|answer| {
                answer.map_err(|_| "the connection to the workspace's host was lost".to_owned())
            })
            .and_then(|answer| answer)
            .and_then(receipt_value);
        trace::note(|| format!("shortcut save in {room}: the host answered {committed:?}"));
        if let Err(error) = committed {
            return Some(Err(error));
        }
        let arrived = self.shortcut_change_arrives(room, &change).await;
        trace::note(|| format!("shortcut save in {room}: arrived {arrived:?}"));
        Some(arrived)
    }

    /// Wait until the room as this device holds it carries `change`, which
    /// its host committed. A write that changes a room's shortcuts announces
    /// a policy change (`WorkspaceTreeHandle::write`): armed before each
    /// look, so a change arriving between the look and the wait still wakes
    /// it.
    async fn shortcut_change_arrives(
        &self,
        room: RoomId,
        change: &tilo_automation::ShortcutChange,
    ) -> Result<(), String> {
        let (watch, mut closed) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .try_send(NetCmd::WatchHostSessions {
                ws: room_topic_id(room),
                ended: watch,
            })
            .map_err(|_| "the network is not running")?;
        let arrived = Arc::new(tokio::sync::Notify::new());
        let wake = Arc::clone(&arrived);
        let announcements =
            tilo_identity_host::policy_change::Watch::new(move || wake.notify_one());
        loop {
            announcements.arm();
            if self.holds_shortcut_change(room, change) {
                return Ok(());
            }
            tokio::select! {
                () = arrived.notified() => {}
                _ = &mut closed => {
                    return Err("the connection to the workspace's host was lost".into());
                }
            }
        }
    }

    /// Whether `room`'s workspace here carries `change`: the shortcut under
    /// the saver's proof it was saved with, or none by a deleted id.
    fn holds_shortcut_change(
        &self,
        room: RoomId,
        change: &tilo_automation::ShortcutChange,
    ) -> bool {
        self.tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .is_some_and(|workspace| match change {
                tilo_automation::ShortcutChange::Save(saved) => workspace
                    .shortcuts
                    .iter()
                    .any(|held| held.id == saved.id && held.saved_proof == saved.saved_proof),
                tilo_automation::ShortcutChange::Delete(id) => {
                    !workspace.shortcuts.iter().any(|held| &held.id == id)
                }
            })
    }
}
