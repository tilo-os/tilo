//! Host-owned current certificate/revocation authority.
//!
//! Implementations read the owner's explicit identity store on every check;
//! this is never supplied by a wire request. Signature, expiry, device role,
//! workspace membership and action policy remain mandatory broker checks
//! independent of this provider.
use uuid::Uuid;

use crate::policy_clock::PolicyTime;

pub trait CertificateAuthority: Send + Sync {
    /// Installed local runtime clock/origin; never supplied by a wire peer.
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        Ok(None)
    }
    /// Checks `_certificate` for `_workspace` at the retained policy `_time`.
    fn authorize_with_policy_time(
        &self,
        _workspace: Uuid,
        _certificate: &tilo_user_identity::DeviceCert,
        _time: &PolicyTime,
    ) -> Result<(), String> {
        Err("retained policy time authority unavailable".into())
    }
    /// Checks `certificate` against the owner's current identity store for `workspace`.
    fn authorize(
        &self,
        workspace: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String>;
    /// The key of the device this host runs on: the computer whose displays its seat holds.
    /// `None` on a host that runs on no computer of its own, which changes no displays.
    fn device(&self) -> Option<[u8; 32]> {
        None
    }
    /// The certificate this host's own device holds, which it acts under itself before anyone
    /// signs in here: in a workspace that names its person owner or administrator. `None` on a
    /// host whose device does not act as itself.
    fn own_device(&self) -> Option<tilo_user_identity::DeviceCert> {
        None
    }
    /// The certificate this device's own person holds in `workspace`: what an app this device
    /// launched acts under there, as the identity this device keeps selects it.
    ///
    /// # Errors
    ///
    /// The device holds no identity in `workspace`.
    fn app_certificate(&self, workspace: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        tilo_identity::trusted_device::current(workspace)
            .map(|(_, certificate)| certificate)
            .map_err(|error| error.to_string())
    }
    /// Local SDK admission may additionally require a current enrollment record.
    ///
    /// # Errors
    ///
    /// Returns a message when [`Self::authorize`] refuses.
    fn authorize_join(
        &self,
        workspace: Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(workspace, certificate)
    }
}
