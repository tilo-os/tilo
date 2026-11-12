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

/// What asking this device for the person's certificate answers while its identity settles.
#[derive(Debug)]
pub enum Readiness {
    /// The identity registry names the person, and this device holds their certificate here.
    Ready(tilo_user_identity::DeviceCert),
    /// This device holds an identity the registry has not named the person through yet: a login
    /// reads the identity directory as its files land, and the answer is a moment away.
    Pending,
    /// Nothing names this device's identity, so no wait changes the answer `why` says.
    Refused(String),
}

/// [`device_certificate`], asked the way the seat asks it before it acts: whether this device's
/// identity names the person yet.
///
/// A session started by a login reads its identity directory while that login is still writing
/// it, so the first action of a person can arrive before the registry holds their record. That
/// moment is [`Readiness::Pending`], never a refusal: the person's own authority is not in
/// question. A device that holds no identity at all is [`Readiness::Refused`] instead, because
/// nothing is on its way there.
#[must_use]
pub fn device_readiness(workspace: &str, person: &str) -> Readiness {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    if tilo_user_identity::persist::load_device_key(&paths).is_err()
        || tilo_user_identity::persist::read_device_cert(&paths.device_cert).is_err()
    {
        return Readiness::Refused("this device holds no identity to sign in with".to_owned());
    }
    // What the registry names is read before the certificate is decided against it: the store
    // hands a decision the registry its worker last reconciled, so a read that lands before that
    // worker has the person's record is a wait, not a refusal — and the answer after it is the one
    // that counts.
    let named = tilo_identity_host::registry::identity_for_workspace(workspace)
        .is_ok_and(|named| named.is_some());
    match device_certificate(workspace, person) {
        Ok(certificate) => Readiness::Ready(certificate),
        Err(why) if named => Readiness::Refused(why),
        Err(_) => Readiness::Pending,
    }
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

    /// Waits while the identity registry has not named the person yet, and answers as
    /// [`Self::certificate`] once it does.
    fn certificate_when_named(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        let mut waiting = false;
        loop {
            match device_readiness(workspace, person) {
                Readiness::Ready(certificate) => return Ok(certificate),
                Readiness::Refused(why) => return Err(why),
                Readiness::Pending => {
                    if !waiting {
                        waiting = true;
                        tracing::info!(
                            %workspace,
                            "the identity registry has not named this device's person yet; the \
                             action waits for it"
                        );
                    }
                    // Woken by the identity landing, never by a clock: a wait nothing
                    // announces is a wait nothing ends, so there is no timeout either.
                    tilo_identity_host::policy_change::wait_for_a_change(None);
                }
            }
        }
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
    use tilo_action_broker::services::LocalPersonServices as _;

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

    /// This test's login: its environment, the identity directory a login writes, and this
    /// device's certificate when an identity is provisioned in it.
    struct Login {
        identity_dir: std::path::PathBuf,
        certificate: Option<tilo_user_identity::DeviceCert>,
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    fn login(provision: bool) -> Login {
        let root = tempfile::tempdir().expect("a directory for the login");
        let config = root.path().join("config");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &config)
            .set("TILO_IDENTITY_ROOT", root.path().join("system"))
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = config.join("tilo/identity");
        let system = tilo_user_identity::IdentityPaths::under(root.path().join("system"));
        let certificate = provision.then(|| {
            tilo_identity_host::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &system,
                tilo_identity_host::provisioning::IdentityStorage::Files,
            )
            .expect("an identity for the login");
            tilo_user_identity::persist::read_device_cert(&system.device_cert)
                .expect("the device certificate")
        });
        Login {
            identity_dir,
            certificate,
            _env: env,
            _root: root,
        }
    }

    impl Login {
        /// The person this login's identity names.
        fn person(&self) -> String {
            tilo_action_broker::collab_app_broker::principal_id_for_root(
                self.certificate
                    .as_ref()
                    .expect("a provisioned login")
                    .user_public_key,
            )
        }

        /// The files the registry names the person through, as a login writes them: every identity
        /// record the directory holds and the index of them.
        fn naming(&self) -> Vec<(std::path::PathBuf, Vec<u8>)> {
            let mut files = Vec::new();
            for entry in std::fs::read_dir(&self.identity_dir).expect("the identity directory") {
                let path = entry.expect("an entry of the identity directory").path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    files.push((path.clone(), std::fs::read(&path).expect("a file it wrote")));
                }
            }
            assert!(!files.is_empty(), "the login holds a record and an index");
            files.sort();
            files
        }
    }

    /// An action asked in the first moment of a login waits for the identity the login is still
    /// writing, and is answered once the registry names its person: never refused, and never on a
    /// clock of its own.
    #[test]
    fn the_first_action_of_a_login_waits_until_the_registry_names_its_person() {
        let login = login(true);
        let person = login.person();
        assert!(
            matches!(device_readiness(WORKSPACE, &person), Readiness::Ready(_)),
            "the provisioned login names its person"
        );
        let naming = login.naming();
        for (path, _) in &naming {
            std::fs::remove_file(path).expect("the login has not written it yet");
        }
        assert!(
            matches!(device_readiness(WORKSPACE, &person), Readiness::Pending),
            "a device holding an identity the registry has not named is a moment away"
        );
        let waiting = std::thread::spawn(move || {
            NativeLocalPerson
                .certificate_when_named(WORKSPACE, &person)
                .expect("the identity names its person")
        });
        // Nothing has named the person yet: the wait is not over, and nothing about it is timed.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !waiting.is_finished(),
            "an action does not answer before the identity names its person"
        );
        for (path, bytes) in naming {
            // A record the login writes is private from its creation: the identity store refuses
            // one the current user does not own at mode 0600.
            let mut options = std::fs::OpenOptions::new();
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("the login writes the identity");
            std::io::Write::write_all(&mut file, &bytes).expect("the record is written");
        }
        tilo_identity_host::policy_change::announce();
        let certificate = waiting.join().expect("the waiting action answers");
        let held = login.certificate.as_ref().expect("a provisioned login");
        assert_eq!(
            certificate.device_public_key, held.device_public_key,
            "the action is taken under this device's certificate"
        );
    }

    /// A device that holds no identity at all answers at once: nothing is on its way, and no
    /// action waits on it.
    #[test]
    fn a_device_with_no_identity_answers_without_waiting() {
        let _login = login(false);
        let answered = NativeLocalPerson
            .certificate_when_named(WORKSPACE, "nobody")
            .expect_err("no identity names anybody");
        assert!(
            answered.contains("no identity to sign in with"),
            "{answered}"
        );
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
        let _login = login(false);
        let authority = NativeCertificates::default();
        let refused = authority
            .authorize_join(workspace(), &certificate(81, 82))
            .expect_err("an uncertified device joins nothing");
        assert_eq!(refused, "workspace identity unavailable");
    }
}
