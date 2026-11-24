//! Whether a certified device may act for the identity a workspace selected: the person's
//! devices as this host knows them ([`crate::sources::DeviceTrust`]), and the rules every host
//! admits a device by.

use tilo_user_identity::DeviceCert;

/// Whether `cert` is a device of `person`'s that their root did not take back: the check every
/// admitted action passes. A host that holds no answer for the person's devices knows of no
/// revocation by them.
#[must_use]
pub fn admitted(person: &[u8; 32], cert: &DeviceCert) -> bool {
    cert.user_public_key == *person
        && !crate::sources::device_trust()
            .and_then(|trust| trust.revoked(person, &cert.device_public_key))
            .unwrap_or(false)
}

/// [`admitted`], and the record of the person's devices holds the key `cert` names. A host that
/// keeps no answer for their devices has no record saying so, and takes nothing: a device is
/// enrolled only where a source installed by the host that holds the record says it is.
#[must_use]
pub fn enrolled(person: &[u8; 32], cert: &DeviceCert) -> bool {
    admitted(person, cert) && enrolment(crate::sources::device_trust(), cert)
}

/// [`enrolled`] with the answer `trust` gives, for a host that may have installed none.
#[must_use]
fn enrolment(trust: Option<&std::sync::Arc<dyn crate::sources::DeviceTrust>>, cert: &DeviceCert) -> bool {
    trust.and_then(|trust| trust.enrolled(cert)).unwrap_or(false)
}

#[cfg(unix)]
mod host {
    //! A Linux desktop answers from the identity it selects for a workspace.

    use tilo_user_identity::DeviceCert;

    /// `cert` belongs to the identity selected for `workspace` and its device is not revoked.
    ///
    /// # Errors
    ///
    /// Returns a message when no identity is selected or the certificate's identity or device
    /// does not qualify.
    pub fn authorize(workspace: &str, cert: &DeviceCert) -> Result<(), String> {
        let profile = crate::store::identity_for_workspace(workspace)
            .map_err(|error| error.to_string())?
            .ok_or("workspace identity unavailable")?;
        if super::admitted(&profile.user_public_key, cert) {
            return Ok(());
        }
        Err("device revoked or identity mismatched".into())
    }

    /// [`authorize`], and the person's devices record holds this device.
    pub fn authorize_join(workspace: &str, cert: &DeviceCert) -> Result<(), String> {
        let profile = crate::store::identity_for_workspace(workspace)
            .map_err(|error| error.to_string())?
            .ok_or("workspace identity unavailable")?;
        if !super::admitted(&profile.user_public_key, cert) {
            return Err("device revoked or identity mismatched".into());
        }
        if super::enrolled(&profile.user_public_key, cert) {
            return Ok(());
        }
        Err("certified collaboration device is not enrolled".into())
    }
}

#[cfg(unix)]
pub use host::{authorize, authorize_join};

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use tilo_device_identity::IdentityPaths;

    use super::*;

    const WORKSPACE: &str = "workspace";

    /// A provisioned identity the environment points at, with its device in the person's
    /// devices as the daemon's answer says.
    struct Fixture {
        identity_id: String,
        cert: DeviceCert,
        // Dropped after the fields above: the environment is restored before the directory it
        // named is removed.
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            crate::sources::test_trust::install();
            let root = tempfile::tempdir().expect("temp root");
            let xdg = root.path().join("config");
            let system_root = root.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = xdg.join("tilo/identity");
            let paths = IdentityPaths::under(&system_root);
            crate::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &paths,
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let profile = crate::store::identity_for_workspace(WORKSPACE)
                .expect("registry")
                .expect("selected identity");
            let cert = tilo_device_identity::persist::read_device_cert(
                &paths.device_cert_for_user(&profile.user_public_key),
            )
            .expect("this device's certificate");
            crate::sources::test_trust::enrol(profile.user_public_key, cert.device_public_key);
            Self {
                identity_id: profile.id,
                cert,
                _env: env,
                _root: root,
            }
        }

        /// The same certificate, for a person and device no record holds.
        fn stranger(&self) -> DeviceCert {
            let mut cert = self.cert.clone();
            cert.user_public_key = [7; 32];
            cert.device_public_key = [8; 32];
            cert
        }
    }

    tilo_test_env::scenario! {
        fn admitting_a_device_from_the_persons_devices;

        phase without_an_installed_source_no_device_is_enrolled;
        {
            // A host with no answer for the person's devices has no record of them: it says a
            // key is not enrolled rather than trusting it.
            let cert = DeviceCert {
                user_public_key: [7; 32],
                device_public_key: [8; 32],
                device_label: "Some device".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            assert!(!super::enrolment(None, &cert));
        }

        phase a_certified_device_is_admitted_and_enrolled;
        {
            let fixture = Fixture::new();
            authorize(WORKSPACE, &fixture.cert).expect("a live device acts");
            authorize_join(WORKSPACE, &fixture.cert).expect("an enrolled device joins");
        }

        phase a_revoked_device_is_refused_at_the_next_call;
        {
            let fixture = Fixture::new();
            authorize_join(WORKSPACE, &fixture.cert).expect("an enrolled device joins");
            // The person's root takes it back on another of their devices.
            crate::sources::test_trust::revoke(
                fixture.cert.user_public_key,
                fixture.cert.device_public_key,
            );
            assert_eq!(
                authorize_join(WORKSPACE, &fixture.cert),
                Err("device revoked or identity mismatched".into())
            );
            assert_eq!(
                authorize(WORKSPACE, &fixture.cert),
                Err("device revoked or identity mismatched".into())
            );
            assert_eq!(
                crate::store::IdentityRegistryStore::from_env()
                    .expect("registry")
                    .identity_of(&fixture.cert.user_public_key)
                    .map(|profile| profile.id),
                Some(fixture.identity_id.clone()),
                "the identity stays the one the workspace selected"
            );
        }

        phase another_persons_certificate_is_refused;
        {
            let fixture = Fixture::new();
            let stranger = fixture.stranger();
            assert_eq!(
                authorize_join(WORKSPACE, &stranger),
                Err("device revoked or identity mismatched".into())
            );
        }
    }
}
