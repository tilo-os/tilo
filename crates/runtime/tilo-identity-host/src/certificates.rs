//! Whether a certified device may act for the identity a workspace selected.

use std::sync::Arc;

use tilo_user_identity::{Authority, ChainPolicy, DeviceCert, verify_chain};

use crate::device_fleet::{DeviceFleet, DeviceFleetStore};

/// `cert` lets its device act for the identity selected for `room` now: the
/// identity's root signed it, it is inside its validity window, that root has
/// not revoked the device, and it grants [`Authority::Person`].
///
/// # Errors
///
/// Returns a message when no identity is selected, the stores cannot be read,
/// or any link of the chain fails.
pub fn authorize(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (user, fleet) = selected_fleet(room)?;
    check(cert, &user, &fleet, crate::device_fleet::now_ns(), false)
}

/// [`authorize`], and the device is enrolled in the identity's fleet.
///
/// # Errors
///
/// Returns any [`authorize`] error, or a message for an unenrolled device.
pub fn authorize_join(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (user, fleet) = selected_fleet(room)?;
    check(cert, &user, &fleet, crate::device_fleet::now_ns(), true)
}

fn selected_fleet(room: &str) -> Result<([u8; 32], Arc<DeviceFleet>), String> {
    let profile = crate::registry::selected_identity_profile(room)
        .map_err(|e| e.to_string())?
        .ok_or("workspace identity unavailable")?;
    let fleet = DeviceFleetStore::from_env()
        .and_then(|store| store.load_cached(&profile.id))
        .map_err(|e| e.to_string())?;
    Ok((profile.user_public_key, fleet))
}

fn check(
    cert: &DeviceCert,
    user_public_key: &[u8; 32],
    fleet: &DeviceFleet,
    now_ns: u64,
    require_enrolled: bool,
) -> Result<(), String> {
    let revocations = fleet.revocation_list();
    verify_chain(
        cert,
        &ChainPolicy {
            user_public_key,
            authority: Authority::Person,
            revocations: &revocations,
            now_ns,
        },
    )
    .map_err(|error| format!("device refused for this workspace's identity: {error}"))?;
    if require_enrolled
        && !fleet.certificates.iter().any(|known| {
            known.user_public_key == *user_public_key
                && known.device_public_key == cert.device_public_key
        })
    {
        return Err("certified collaboration device is not enrolled".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::device_fleet::loads_on_this_thread;
    use tilo_user_identity::{
        DeviceCapabilities, IdentityPaths, RevocationEntry, RevocationReason, UserIdentity,
        sign_device_cert, sign_revocation_entry,
    };

    pub(crate) const NOW: u64 = 1_000_000;

    pub(crate) fn certify(
        root: &ed25519_dalek::SigningKey,
        device: &ed25519_dalek::SigningKey,
        capabilities: DeviceCapabilities,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Board B".into(),
            capabilities,
            issued_at_ns: 100,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    pub(crate) fn revoke(root: &ed25519_dalek::SigningKey, device: [u8; 32]) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: NOW * 10,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn only_a_current_personal_certificate_of_the_selected_identity_is_authorized() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let (other_root, _) = UserIdentity::generate("Mallory", 1);
        let device = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let joined = certify(&root, &device, DeviceCapabilities::joined(), None);
        let mut fleet = DeviceFleet::new(hex::encode(user.public_key));
        let user = user.public_key;

        // Valid, but not yet enrolled: authorize passes, join does not.
        check(&joined, &user, &fleet, NOW, false).unwrap();
        assert!(check(&joined, &user, &fleet, NOW, true).is_err());
        fleet.certificates.push(certify(
            &other_root,
            &device,
            DeviceCapabilities::joined(),
            None,
        ));
        assert!(check(&joined, &user, &fleet, NOW, true).is_err());
        fleet.certificates.push(joined.clone());
        check(&joined, &user, &fleet, NOW, true).unwrap();

        let expired = certify(&root, &device, DeviceCapabilities::joined(), Some(NOW));
        assert!(check(&expired, &user, &fleet, NOW, false).is_err());
        let host = certify(&root, &device, DeviceCapabilities::host(), None);
        assert!(check(&host, &user, &fleet, NOW, false).is_err());
        let foreign = certify(&other_root, &device, DeviceCapabilities::joined(), None);
        assert!(check(&foreign, &user, &fleet, NOW, false).is_err());
        let mut forged = joined.clone();
        forged.capabilities.can_admit_devices = true;
        assert!(check(&forged, &user, &fleet, NOW, false).is_err());

        // Another identity's revocation does not count; the person's own
        // does, whatever its timestamp.
        fleet
            .revocations
            .push(revoke(&other_root, joined.device_public_key));
        check(&joined, &user, &fleet, NOW, true).unwrap();
        fleet
            .revocations
            .push(revoke(&root, joined.device_public_key));
        let refusal = check(&joined, &user, &fleet, NOW, true).unwrap_err();
        assert!(refusal.contains("revoked"), "{refusal}");
    }

    const ROOM: &str = "workspace";

    /// A provisioned identity the environment points at.
    struct Fixture {
        identity_dir: PathBuf,
        identity_id: String,
        cert: DeviceCert,
        // Dropped after the fields above: the environment is restored before
        // the directory it named is removed.
        _env: tilo_test_env::ScopedEnv,
        _root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temp root");
            let xdg = root.path().join("config");
            let system_root = root.path().join("system");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", &xdg)
                .set("TILO_IDENTITY_ROOT", &system_root)
                .set(crate::provisioning::FILE_SECRETS_ENV, "1");
            let identity_dir = xdg.join("tilo/identity");
            crate::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &IdentityPaths::under(&system_root),
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let profile = crate::registry::selected_identity_profile(ROOM)
                .expect("registry")
                .expect("selected identity");
            let cert = store()
                .load(&profile.id)
                .expect("fleet")
                .certificates
                .first()
                .cloned()
                .expect("this device is enrolled");
            Self {
                identity_dir,
                identity_id: profile.id,
                cert,
                _env: env,
                _root: root,
            }
        }

        fn fleet_file(&self) -> PathBuf {
            self.identity_dir
                .join("fleets")
                .join(format!("{}.json", self.identity_id))
        }

        /// Authorizes this device twice; returns how many fleet loads it took.
        fn authorize_join_twice(&self) -> usize {
            let before = loads_on_this_thread();
            authorize_join(ROOM, &self.cert).expect("an enrolled device joins");
            authorize_join(ROOM, &self.cert).expect("an enrolled device joins again");
            loads_on_this_thread().saturating_sub(before)
        }

        fn revoke(&self) {
            store()
                .revoke(
                    &self.identity_id,
                    self.cert.device_public_key,
                    RevocationReason::Lost,
                    self.cert.issued_at_ns.saturating_add(1),
                )
                .expect("revoke");
        }
    }

    fn store() -> DeviceFleetStore {
        DeviceFleetStore::from_env().expect("fleet store")
    }

    /// Date `path` back past the filesystem clock's granularity, as any file
    /// is a few seconds after its last write.
    fn settle(path: &Path) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open fleet file")
            .set_modified(
                SystemTime::now()
                    .checked_sub(Duration::from_secs(10))
                    .expect("a time ten seconds ago"),
            )
            .expect("date fleet file back");
    }

    #[test]
    fn authorize_join_loads_fleet_once() {
        let fixture = Fixture::new();
        settle(&fixture.fleet_file());
        assert_eq!(
            fixture.authorize_join_twice(),
            1,
            "one load serves both checks of the first call, and the unchanged fleet the second"
        );
    }

    #[test]
    fn cached_fleet_still_checks_certificate_validity_and_personal_authority() {
        let fixture = Fixture::new();
        settle(&fixture.fleet_file());
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");
        let record = crate::registry::selected_identity_path(ROOM)
            .expect("registry")
            .expect("selected identity record");
        let root = runtime_identity::load_from_path(record).expect("fixture root");
        let mut expired = fixture.cert.clone();
        expired.expires_at_ns = Some(expired.issued_at_ns);
        let mut future = fixture.cert.clone();
        future.issued_at_ns = u64::MAX;
        let mut host = fixture.cert;
        host.capabilities = DeviceCapabilities::host();
        let before = loads_on_this_thread();
        for mut cert in [expired, future, host] {
            let digest = tilo_user_identity::device_cert_signing_digest(&cert)
                .expect("negative control signing digest");
            cert.signature = root.sign(&digest).expect("root-signed negative control");
            assert!(authorize(ROOM, &cert).is_err());
            assert!(authorize_join(ROOM, &cert).is_err());
        }
        assert_eq!(loads_on_this_thread(), before, "no fleet was read again");
    }

    #[test]
    fn cached_fleet_scopes_permanent_revocation_to_the_selected_identity() {
        let fixture = Fixture::new();
        let (other_root, _) = UserIdentity::generate("Other person", 1);
        let mut fleet = store().load(&fixture.identity_id).expect("fleet");
        fleet
            .revocations
            .push(revoke(&other_root, fixture.cert.device_public_key));
        let path = fixture.fleet_file();
        std::fs::write(&path, serde_json::to_vec(&fleet).expect("fleet bytes"))
            .expect("foreign-root revocation");
        settle(&path);
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");
        assert!(authorize(ROOM, &fixture.cert).is_ok());
        assert!(
            !store()
                .load_cached(&fixture.identity_id)
                .expect("cached fleet")
                .is_revoked(&fixture.cert.device_public_key)
        );

        // Even a timestamp in the future revokes permanently. A foreign
        // entry for this same key cannot hide the selected root's entry.
        store()
            .revoke(
                &fixture.identity_id,
                fixture.cert.device_public_key,
                RevocationReason::Lost,
                u64::MAX,
            )
            .expect("future-dated root revocation");
        settle(&path);
        let revoked = store()
            .load_cached(&fixture.identity_id)
            .expect("revoked fleet");
        assert!(revoked.is_revoked(&fixture.cert.device_public_key));
        assert_eq!(
            authorize_join(ROOM, &fixture.cert),
            Err(
                "device refused for this workspace's identity: the identity revoked this device"
                    .into()
            )
        );
        assert_eq!(
            authorize(ROOM, &fixture.cert),
            Err(
                "device refused for this workspace's identity: the identity revoked this device"
                    .into()
            )
        );
    }

    #[test]
    fn revocation_visible_immediately_after_revoke() {
        let fixture = Fixture::new();
        settle(&fixture.fleet_file());
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");
        fixture.revoke();
        assert_eq!(
            authorize_join(ROOM, &fixture.cert),
            Err(
                "device refused for this workspace's identity: the identity revoked this device"
                    .into()
            )
        );
        assert_eq!(
            authorize(ROOM, &fixture.cert),
            Err(
                "device refused for this workspace's identity: the identity revoked this device"
                    .into()
            )
        );
    }

    #[test]
    fn external_fleet_write_invalidates() {
        let fixture = Fixture::new();
        let path = fixture.fleet_file();
        let enrolled = std::fs::read(&path).expect("enrolled fleet");
        fixture.revoke();
        let revoked = std::fs::read(&path).expect("revoked fleet");

        // Another process puts the enrolled fleet back; once old it is cached.
        std::fs::write(&path, &enrolled).expect("external write");
        settle(&path);
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");

        // Another process rewrites the file in place with the revocation.
        std::fs::write(&path, &revoked).expect("external revocation");
        assert_eq!(
            authorize_join(ROOM, &fixture.cert),
            Err(
                "device refused for this workspace's identity: the identity revoked this device"
                    .into()
            )
        );

        // A fleet that no longer parses denies instead of serving the cache.
        std::fs::write(&path, &enrolled).expect("external write");
        settle(&path);
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");
        std::fs::write(&path, b"{ not a fleet").expect("external corruption");
        assert!(authorize_join(ROOM, &fixture.cert).is_err());
        assert!(authorize(ROOM, &fixture.cert).is_err());
    }
}
