//! The device's current certificate authority, as the app broker asks it.
//!
//! The broker decides each join and each host action itself: the certificate's
//! signature, the route it is presented for, the workspace's policy. What it
//! cannot know is the device's own identity — which person this computer acts
//! for, which devices that person currently certifies, and which of them were
//! revoked since. This is that answer, read from the identity store on every
//! check, so a revocation or a replaced device is refused at the next
//! admission rather than at the next login.
//!
//! The policy clock is this service's too: an admission is decided at a time
//! that belongs to the service that decided it, and a decision retained past
//! that service is unusable. Nothing here falls back to wall time.
//!
//! The same store answers which certificate this device acts under
//! ([`device_certificate`]): what a native action of the person's own is
//! admitted with, and what its effect is re-checked against.

use tilo_action_broker::certificate_authority::CertificateAuthority;
use tilo_action_broker::policy_clock::{PolicyClockError, PolicyOrigin, PolicyTime};

/// The certificate this device presents in `workspace`, for the person it
/// belongs to: the identity the workspace is kept as, the device that
/// identity enrolled here, and the person's own authority over it now.
///
/// This is the certificate a native action is taken under and the one an app
/// of this session joins with, so it is read from the identity store every
/// time rather than kept: a device the person revoked, or an identity the
/// workspace switched to, is a different answer at the next call.
///
/// # Errors
///
/// The workspace has no identity selected here, this device is not the one
/// that identity enrolled (or was revoked), the certificate does not belong
/// to `person`, or the identity registry no longer claims this device.
pub fn device_certificate(
    workspace: &str,
    person: &str,
) -> Result<tilo_user_identity::DeviceCert, String> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    // The certificate the workspace's selected identity presents for this
    // device: the one its apps join the workspace with and the one the
    // workspace admitted. The device owner's system-facing certificate is
    // issued separately for the same key, so comparing a session's admission
    // against it refused every native action ("native seat differs from
    // original certified admission").
    let (_, certificate) = tilo_identity_host::broker::trusted_current_device(workspace)
        .map_err(|error| error.to_string())?;
    if tilo_action_broker::collab_app_broker::principal_id_for_root(certificate.user_public_key)
        != person
    {
        return Err("native caller does not match the device person".into());
    }
    // A selected alternate workspace identity cannot borrow this device's
    // primary person. The certificate's fleet revocation is checked afresh.
    tilo_identity_host::certificates::authorize(workspace, &certificate)?;
    let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env()
        .map_err(|error| error.to_string())?;
    if !tilo_identity_host::provisioning::active_identity_owns_device(
        registry.identity_dir(),
        &paths,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("native device identity is not currently claimed".into());
    }
    Ok(certificate)
}

/// The seat a session's own actions are taken under: an action the person at
/// this device starts, without an app or a peer asking for it.
///
/// The broker admits such an action only with the certificate it is taken
/// under and the policy time it is decided at, and re-checks both when the
/// action's effect is about to happen ([`device_certificate`] again). A host
/// that serves its apps with a certificate authority has no other way in:
/// offering it none is not a native action, it is an unproven one.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeLocalPerson;

impl tilo_action_broker::services::LocalPersonServices for NativeLocalPerson {
    fn certificate(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        device_certificate(workspace, person)
    }
}

/// The identity store's device-certificate checks, as a session's broker asks
/// them.
pub struct NativeCertificates {
    /// The clock every admission this service decides is at, or why this
    /// device has none. Retires with the service.
    pub origin: Result<PolicyOrigin, PolicyClockError>,
}

impl Default for NativeCertificates {
    fn default() -> Self {
        Self {
            origin: tilo_action_broker::policy_clock::OwnerPolicyClock::new()
                .map(|clock| clock.new_origin()),
        }
    }
}

impl CertificateAuthority for NativeCertificates {
    fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
        self.origin
            .as_ref()
            .map_err(ToString::to_string)?
            .capture()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn authorize_with_policy_time(
        &self,
        workspace: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
        time: &PolicyTime,
    ) -> Result<(), String> {
        let origin = self.origin.as_ref().map_err(ToString::to_string)?;
        if !origin.owns(time) {
            return Err("native certificate policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "native certificate expired at retained policy time")?;
        self.authorize(workspace, certificate)
    }

    fn authorize(
        &self,
        workspace: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize(&workspace.to_string(), certificate)
    }

    fn authorize_join(
        &self,
        workspace: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize_join(&workspace.to_string(), certificate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace the device certificates are asked about.
    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

    fn workspace() -> uuid::Uuid {
        uuid::Uuid::parse_str(WORKSPACE).expect("a workspace id")
    }

    /// A device certificate: the root `root_seed` certifies the device
    /// `device_seed`.
    fn certificate(root_seed: u8, device_seed: u8) -> tilo_user_identity::DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[root_seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[device_seed; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Clock test".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("a certified device");
        certificate
    }

    /// This test's login, with no identity provisioned in it: the store's
    /// refusals are its own, never the developer's identity.
    struct Unprovisioned {
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    fn unprovisioned() -> Unprovisioned {
        let root = tempfile::tempdir().expect("a directory for the login");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", root.path().join("config"))
            .set("TILO_IDENTITY_ROOT", root.path().join("system"))
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        Unprovisioned {
            _env: env,
            _root: root,
        }
    }

    #[test]
    fn native_certificate_policy_time_is_retained_and_retires_with_its_service() {
        let authority = NativeCertificates::default();
        let first = authority.policy_time().unwrap().unwrap();
        let second = authority.policy_time().unwrap().unwrap();
        assert!(first.same_origin(&second));
        let initial = first.now_ns().unwrap();
        assert!(second.now_ns().unwrap() >= initial);
        let replacement = NativeCertificates::default();
        assert!(!first.same_origin(&replacement.policy_time().unwrap().unwrap()));
        drop(authority);
        assert!(first.now_ns().is_err());
        assert!(second.now_ns().is_err());
        assert!(replacement.policy_time().unwrap().unwrap().now_ns().is_ok());
    }

    #[test]
    fn native_certificate_policy_clock_failure_never_falls_back_to_wall_time() {
        let authority = NativeCertificates {
            origin: Err(PolicyClockError),
        };
        assert!(authority.policy_time().is_err());
    }

    #[test]
    fn native_certificate_policy_refuses_another_services_retained_origin() {
        let authority = NativeCertificates::default();
        let other = NativeCertificates::default();
        let foreign_time = other.policy_time().unwrap().unwrap();
        let certificate = certificate(73, 74);
        assert_eq!(
            authority
                .authorize_with_policy_time(workspace(), &certificate, &foreign_time)
                .unwrap_err(),
            "native certificate policy origin changed"
        );
        drop(other);
        assert!(foreign_time.now_ns().is_err());
        assert_eq!(
            authority
                .authorize_with_policy_time(workspace(), &certificate, &foreign_time)
                .unwrap_err(),
            "native certificate policy origin changed"
        );
    }

    #[test]
    fn native_certificate_policy_time_never_replaces_an_unavailable_origin() {
        let certificates = NativeCertificates {
            origin: Err(PolicyClockError),
        };
        for _ in 0..2 {
            assert!(matches!(certificates.policy_time(), Err(error)
                if error == PolicyClockError.to_string()));
        }
        let certificates = NativeCertificates::default();
        let captured = certificates.policy_time().unwrap().unwrap();
        certificates.origin.as_ref().unwrap().invalidate();
        assert!(certificates.policy_time().is_err());
        assert!(captured.now_ns().is_err());
    }

    /// A device certificate the identity store's selected identity never
    /// certified is refused, with the store's own refusal: this login has no
    /// identity, so nothing is certified.
    #[test]
    fn a_device_the_identity_store_does_not_certify_acts_for_nobody() {
        let _login = unprovisioned();
        let authority = NativeCertificates::default();
        let refused = authority
            .authorize_join(workspace(), &certificate(81, 82))
            .expect_err("an uncertified device joins nothing");
        assert_eq!(refused, "workspace identity unavailable");
    }
}
