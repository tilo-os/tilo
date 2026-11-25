//! The device key as its daemon signs with it.
//!
//! The daemon is the device key's one writer: no other program reads the key file. A program
//! that signs as the device asks the daemon to ([`DeviceActions::sign`]) through a
//! [`DaemonSigner`], and never has the key. It has no endpoint secret either: two endpoints
//! under one key push each other off the network, so only the daemon dials as the device.

use collab_types::signer::{DeviceSigner, SignError, SignFuture};
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};

/// Signs as this device by asking its daemon.
#[derive(Clone, Debug)]
pub struct DaemonSigner {
    actions: DeviceActions,
    key: [u8; 32],
}

impl DaemonSigner {
    /// The signer of this computer's daemon, for the login whose identity `paths` keeps.
    ///
    /// # Errors
    /// The daemon's sockets cannot be found, or the daemon does not answer which key it is.
    pub fn here(paths: IdentityPaths) -> Result<Self, DeviceRefusal> {
        Self::of(DeviceActions::here(paths).map_err(DeviceRefusal::said)?)
    }

    /// The signer of the daemon `actions` reaches.
    ///
    /// # Errors
    /// The daemon does not answer which key it is.
    pub fn of(actions: DeviceActions) -> Result<Self, DeviceRefusal> {
        let key = actions.device_key()?;
        Ok(Self { actions, key })
    }

    /// The device's public key.
    #[must_use]
    pub const fn public_key(&self) -> [u8; 32] {
        self.key
    }

    /// `message` signed as the device, at once.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    pub fn sign_now(&self, message: &[u8]) -> Result<[u8; 64], DeviceRefusal> {
        self.actions.sign(message)
    }
}

impl DeviceSigner for DaemonSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.key
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        // Asked when first polled, so a caller that signs within its turn is answered in it.
        Box::pin(async move {
            self.actions
                .sign(&message)
                .map_err(|refused| SignError(refused.message))
        })
    }
}
