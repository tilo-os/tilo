//! A shortcut saved into a room a host governs: asked of that host as the
//! member's own business (`tilo.member` `shortcut.save`), signed by this
//! device as its person's. The host, the owner's device or a node, commits it
//! into the room document every member admits; nothing is written here.
use std::sync::Arc;

use super::ShellNetwork;
use super::relay_rooms::receipt_value;
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
        let host = match self.host_governor(room) {
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
        Some(
            tokio::task::spawn_blocking(move || answer.recv())
                .await
                .map_err(|_| "the request to the workspace's host stopped".to_owned())
                .and_then(|answer| {
                    answer.map_err(|_| "the connection to the workspace's host was lost".to_owned())
                })
                .and_then(|answer| answer)
                .and_then(receipt_value)
                .map(drop),
        )
    }
}
