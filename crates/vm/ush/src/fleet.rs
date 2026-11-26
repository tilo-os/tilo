//! The user's devices, read for the package store each time it asks.
//!
//! The images an environment stacks may come from any of the user's devices, so which
//! those are is read afresh: this device's certificate from the identity, the fleet
//! record the identity host keeps (`fleets/<user>.json`: every device enrolled and
//! removed, the person's servers among them). Nothing is remembered between reads, so a
//! removed device is refused as soon as the removal has synced.

use std::path::{Path, PathBuf};

use package_store::{CertifiedFleet, FleetSource};
use tilo_device_identity::IdentityPaths;
use tilo_user_identity::{DeviceCert, RevocationEntry};

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
        let user = tilo_device_identity::persist::read_public_key(&self.identity.user_pubkey)
            .map_err(|error| format!("this device has no user identity: {error}"))?;
        let mut fleet = CertifiedFleet::new(user);
        // Each certificate is checked as it is added: one that does not
        // verify, or is another person's, makes nobody a device of this user.
        let own = self.identity.device_cert_for_user(&user);
        if let Ok(cert) = tilo_device_identity::persist::read_device_cert(&own) {
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
