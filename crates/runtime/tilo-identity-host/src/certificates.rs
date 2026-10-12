//! Whether a certified device may act for the identity a workspace selected.

use std::sync::Arc;

use tilo_user_identity::DeviceCert;

use crate::device_fleet::{DeviceFleet, DeviceFleetStore};
use crate::registry::IdentityProfile;

/// `cert` belongs to the identity selected for `room` and its device is not
/// revoked.
///
/// # Errors
///
/// Returns a message when no identity is selected, the stores cannot be read,
/// or the certificate's identity or device does not qualify.
pub fn authorize(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (profile, fleet) = selected_fleet(room)?;
    check_identity_and_revocation(&profile, &fleet, cert)
}

/// [`authorize`], and the device is enrolled in the identity's fleet.
///
/// # Errors
///
/// Returns any [`authorize`] error, or a message for an unenrolled device.
pub fn authorize_join(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (profile, fleet) = selected_fleet(room)?;
    check_identity_and_revocation(&profile, &fleet, cert)?;
    if !fleet
        .certificates
        .iter()
        .any(|known| known.device_public_key == cert.device_public_key)
    {
        return Err("certified collaboration device is not enrolled".into());
    }
    Ok(())
}

/// The identity selected for `room` and one verified snapshot of its fleet.
fn selected_fleet(room: &str) -> Result<(IdentityProfile, Arc<DeviceFleet>), String> {
    let profile = crate::registry::selected_identity_profile(room)
        .map_err(|e| e.to_string())?
        .ok_or("workspace identity unavailable")?;
    let fleet = DeviceFleetStore::from_env()
        .and_then(|store| store.load_cached(&profile.id))
        .map_err(|e| e.to_string())?;
    Ok((profile, fleet))
}

fn check_identity_and_revocation(
    profile: &IdentityProfile,
    fleet: &DeviceFleet,
    cert: &DeviceCert,
) -> Result<(), String> {
    if cert.user_public_key != profile.user_public_key || fleet.is_revoked(&cert.device_public_key)
    {
        return Err("device revoked or identity mismatched".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use tilo_user_identity::{IdentityPaths, RevocationReason};

    use super::*;
    use crate::device_fleet::loads_on_this_thread;

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
    fn revocation_visible_immediately_after_revoke() {
        let fixture = Fixture::new();
        settle(&fixture.fleet_file());
        assert_eq!(fixture.authorize_join_twice(), 1, "the fleet is cached");
        fixture.revoke();
        assert_eq!(
            authorize_join(ROOM, &fixture.cert),
            Err("device revoked or identity mismatched".into())
        );
        assert_eq!(
            authorize(ROOM, &fixture.cert),
            Err("device revoked or identity mismatched".into())
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
            Err("device revoked or identity mismatched".into())
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
