//! The custody this device signs its own room changes with, such as a
//! shortcut it saves: its key and its certificate in the room.
use super::ShellNetwork;
use crate::shell::state::RoomId;

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
}
