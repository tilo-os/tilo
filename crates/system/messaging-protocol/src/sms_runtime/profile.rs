//! Selected hardware coordinates are encrypted under the approved account key.
use super::transport::{SERVICE, SimBinding};
use crate::{Error, journal::Owner};
use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use std::io::Read as _;
use zeroize::Zeroizing;
const FORMAT: VaultFormat = VaultFormat {
    name: "tilo.sms-selected-sim.v1",
    magic: b"TSIM",
    secret_service_label: "Tilo selected SIM",
    secret_service_attributes: &[],
    dpapi_entropy: b"tilo.sms-selected-sim.v1",
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    owner: Owner,
    modem: String,
    sim: String,
    #[serde(with = "private_string")]
    iccid: Zeroizing<String>,
    epoch: u64,
}
impl SimBinding {
    /// Persist only after the original native permission and SIM were checked.
    /// This never creates a key or permission and writes no public metadata.
    /// # Errors
    /// Refuses invalid ownership, relative storage or encrypted publication.
    pub fn save(&self, path: &std::path::Path, owner: &Owner, key: &VaultKey) -> Result<(), Error> {
        if !path.is_absolute() || owner.service != crate::Service::Sms {
            return Err(Error::WrongAccount);
        }
        let saved = Saved {
            owner: owner.clone(),
            modem: self.modem.to_string(),
            sim: self.sim.to_string(),
            iccid: self.iccid.clone(),
            epoch: self.epoch,
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&saved).map_err(|_| Error::InvalidFrame)?);
        let sealed = FORMAT
            .seal(key, &bytes)
            .ok_or(Error::ConnectionUnavailable)?;
        tilo_files::native::private::write_atomic(path, &sealed)
            .map_err(|_| Error::ConnectionUnavailable)
    }
    /// Resume the exact reviewed physical SIM using a fresh authenticated daemon
    /// owner. No discovery, alternate modem, grant or changed generation is used.
    /// The caller must validate this binding before opening message stores.
    /// # Errors
    /// Refuses foreign ciphertext, oversized files, malformed bindings or bus failure.
    pub async fn load(
        path: &std::path::Path,
        owner: &Owner,
        key: &VaultKey,
        connection: &zbus::Connection,
    ) -> Result<Self, Error> {
        if !path.is_absolute() || owner.service != crate::Service::Sms {
            return Err(Error::WrongAccount);
        }
        let (file, _) = tilo_files::native::private::open_owned_for_read(path, 8192)
            .map_err(|_| Error::ConnectionUnavailable)?;
        let mut blob = Vec::new();
        file.take(8193)
            .read_to_end(&mut blob)
            .map_err(|_| Error::ConnectionUnavailable)?;
        if blob.len() > 8192 {
            return Err(Error::InvalidFrame);
        }
        let bytes = FORMAT.open(key, &blob).ok_or(Error::WrongAccount)?;
        let saved: Saved = serde_json::from_slice(&bytes).map_err(|_| Error::InvalidFrame)?;
        if saved.owner != *owner {
            return Err(Error::WrongAccount);
        }
        let bus = zbus::fdo::DBusProxy::new(connection)
            .await
            .map_err(|_| Error::ConnectionUnavailable)?;
        let daemon = bus
            .get_name_owner(SERVICE.try_into().map_err(|_| Error::InvalidIdentity)?)
            .await
            .map_err(|_| Error::ConnectionUnavailable)?;
        Self::new(
            daemon.to_string(),
            saved.modem,
            saved.sim,
            saved.iccid.to_string(),
            saved.epoch,
        )
    }
}

mod private_string {
    use super::{Deserialize, Zeroizing};
    pub fn serialize<S: serde::Serializer>(
        value: &Zeroizing<String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Zeroizing<String>, D::Error> {
        String::deserialize(deserializer).map(Zeroizing::new)
    }
}
