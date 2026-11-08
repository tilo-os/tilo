//! Host-owned current certificate/revocation authority.
//!
//! Implementations read the owner's explicit identity store on every check;
//! this is never supplied by a wire request. Signature, expiry, device role,
//! room membership and action policy remain mandatory broker checks
//! independent of this provider.
use uuid::Uuid;

use crate::policy_clock::PolicyTime;

pub trait CertificateAuthority: Send + Sync {
    /// Installed local runtime clock/origin; never supplied by a wire peer.
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        Ok(None)
    }
    /// Checks `_certificate` for `_room` at the retained policy `_time`.
    fn authorize_with_policy_time(
        &self,
        _room: Uuid,
        _certificate: &tilo_user_identity::DeviceCert,
        _time: &PolicyTime,
    ) -> Result<(), String> {
        Err("retained policy time authority unavailable".into())
    }
    /// Exact current pinned host authority for a broker-allocated private app
    /// descriptor. Ordinary and remote joins must never use this exception.
    fn authorize_host_worker(
        &self,
        _route: &crate::route::CrdtRoute,
        _certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        Err("local host worker authority unavailable".into())
    }

    /// Checks `certificate` against the owner's current identity store for `room`.
    fn authorize(
        &self,
        room: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String>;
    /// Whether `_device` governs `_room`; `None` is this host's own device.
    /// A foreign device is held to the workspace's cap only in rooms it does
    /// not govern; the default governs none.
    fn governs(&self, _room: Uuid, _device: Option<&[u8; 32]>) -> bool {
        false
    }
    /// Local SDK admission may additionally require a host's current enrollment record.
    ///
    /// # Errors
    ///
    /// Returns a message when [`Self::authorize`] refuses or the certificate acts as a host.
    fn authorize_join(
        &self,
        room: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(room, certificate)?;
        if certificate.capabilities.can_act_as_host {
            return Err("device authority required".into());
        }
        Ok(())
    }
}
