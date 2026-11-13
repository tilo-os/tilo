//! The user's devices, read for the package store each time it asks.
//!
//! The images an environment stacks may come from any of the user's devices, so which
//! those are is read afresh: this device's certificate from the identity, the fleet
//! record the identity host keeps (`fleets/<user>.json`: every device enrolled and
//! removed), and the home peers enrolled for the user (`peers/home-peers.json`), each
//! with the certificate it was enrolled under. Nothing is remembered between reads, so a
//! removed device is refused as soon as the removal has synced.

use std::path::{Path, PathBuf};

use package_store::{CertifiedFleet, FleetSource};
use tilo_user_identity::{DeviceCert, IdentityPaths, RevocationEntry};

const MAX_FLEET_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct UserFleet {
    identity: IdentityPaths,
    identity_dir: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
struct FleetRecord {
    identity_id: String,
    certificates: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
}

#[derive(serde::Deserialize)]
struct HomePeer {
    #[serde(default)]
    device_cert: Option<DeviceCert>,
}

impl UserFleet {
    pub fn at(identity: IdentityPaths, identity_dir: Option<PathBuf>) -> Self {
        Self {
            identity,
            identity_dir,
        }
    }

    pub fn from_env() -> Self {
        let identity = std::env::var_os("TILO_IDENTITY_ROOT")
            .filter(|root| !root.is_empty())
            .map_or_else(IdentityPaths::default_system, IdentityPaths::under);
        let identity_dir = tilo_paths::xdg::config_home()
            .filter(|dir| dir.is_absolute())
            .map(|config| config.join("tilo").join("identity"));
        Self::at(identity, identity_dir)
    }
}

impl FleetSource for UserFleet {
    fn fleet(&self) -> Result<CertifiedFleet, String> {
        let user = tilo_user_identity::persist::read_public_key(&self.identity.user_pubkey)
            .map_err(|error| format!("this device has no user identity: {error}"))?;
        let mut fleet = CertifiedFleet::new(user);
        // Each certificate is checked as it is added: one that does not
        // verify, or is another person's, makes nobody a device of this user.
        let own = self.identity.device_cert_for_user(&user);
        if let Ok(cert) = tilo_user_identity::persist::read_device_cert(&own) {
            let _ = fleet.certify(cert);
        }
        if let Some(dir) = &self.identity_dir {
            let path = dir
                .join("fleets")
                .join(format!("{}.json", hex::encode(user)));
            if let Some(record) = read_json::<FleetRecord>(&path)? {
                if record.identity_id != hex::encode(user) {
                    return Err(format!("{} is another identity's fleet", path.display()));
                }
                for cert in record.certificates {
                    let _ = fleet.certify(cert);
                }
                for entry in record.revocations {
                    let _ = fleet.revoke(entry);
                }
            }
        }
        let home_peers = self
            .identity
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .map(tilo_user_identity::home_peer_registry_path);
        if let Some(path) = home_peers
            && let Some(peers) = read_json::<Vec<HomePeer>>(&path)?
        {
            for cert in peers.into_iter().filter_map(|peer| peer.device_cert) {
                let _ = fleet.certify(cert);
            }
        }
        Ok(fleet)
    }
}

/// `path` parsed, `None` when it does not exist. A file that exists but
/// cannot be read is an error: a fleet read in part could miss a removal.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, MAX_FLEET_BYTES.saturating_add(1)),
        &mut bytes,
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_FLEET_BYTES {
        return Err(format!("{} is larger than a fleet may be", path.display()));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use package_store::DeviceStanding;
    use tilo_user_identity::{DeviceCapabilities, RevocationReason, UserIdentity};

    fn cert(root: &ed25519_dalek::SigningKey, device: [u8; 32]) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "device".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    #[test]
    fn the_fleet_is_read_from_the_identity_files_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let identity = IdentityPaths::under(dir.path().join("system"));
        let identity_dir = dir.path().join("config/identity");
        let (root, user) = UserIdentity::generate("Ada", 1);
        let (other_root, _) = UserIdentity::generate("Bob", 1);
        let source = UserFleet::at(identity.clone(), Some(identity_dir.clone()));

        assert!(source.fleet().is_err());

        identity.ensure_directories().unwrap();
        tilo_user_identity::persist::write_public_key(&identity.user_pubkey, &user.public_key)
            .unwrap();
        let own = identity.device_cert_for_user(&user.public_key);
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        tilo_user_identity::persist::write_device_cert(&own, &cert(&root, [1; 32])).unwrap();

        let fleets = identity_dir.join("fleets");
        std::fs::create_dir_all(&fleets).unwrap();
        let fleet_file = fleets.join(format!("{}.json", hex::encode(user.public_key)));
        let record = |revocations: Vec<RevocationEntry>| {
            serde_json::json!({
                "schema_version": 1,
                "identity_id": hex::encode(user.public_key),
                "certificates": [cert(&root, [2; 32]), cert(&other_root, [3; 32])],
                "revocations": revocations,
            })
        };
        std::fs::write(&fleet_file, record(Vec::new()).to_string()).unwrap();
        std::fs::create_dir_all(dir.path().join("system/peers")).unwrap();
        std::fs::write(
            tilo_user_identity::home_peer_registry_path(&dir.path().join("system")),
            serde_json::json!([
                { "label": "home", "device_cert": cert(&root, [4; 32]) },
                { "label": "someone's", "device_cert": cert(&other_root, [5; 32]) },
                { "label": "uncertified" },
            ])
            .to_string(),
        )
        .unwrap();

        let now = tilo_clock::unix_now_ns();
        let fleet = source.fleet().unwrap();
        for device in [[1; 32], [2; 32], [4; 32]] {
            assert_eq!(fleet.standing(&device, now), DeviceStanding::Certified);
        }
        for device in [[3; 32], [5; 32], [6; 32]] {
            assert_eq!(fleet.standing(&device, now), DeviceStanding::Uncertified);
        }

        let mut removal = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: [2; 32],
            revoked_at_ns: 5,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut removal, &root).unwrap();
        std::fs::write(&fleet_file, record(vec![removal]).to_string()).unwrap();
        assert_eq!(
            source.fleet().unwrap().standing(&[2; 32], now),
            DeviceStanding::Revoked(RevocationReason::Retired)
        );

        // A fleet that cannot be read is not read as "nobody removed".
        std::fs::write(&fleet_file, b"{").unwrap();
        assert!(source.fleet().is_err());
    }
}
