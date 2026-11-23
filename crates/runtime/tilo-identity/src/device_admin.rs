//! Where the person's identity lives on this device, as the steps that administer it read it.

use std::path::PathBuf;

/// Where the identity an approved device joins lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPaths {
    /// The device root (`/var/lib/tilo` on an installed system).
    pub root: PathBuf,
    /// The person's identity directory.
    pub dir: PathBuf,
}

impl PersonPaths {
    /// The identity this account acts as: this device's identity root
    /// ([`crate::provisioning::collaboration_identity_root`]) and the
    /// person's identity directory.
    pub fn from_env() -> Result<Self, String> {
        let root = crate::provisioning::collaboration_identity_root();
        let dir = crate::store::identity_dir_from_env()
            .map_err(|error| format!("could not find your identity: {error}"))?;
        Ok(Self { root, dir })
    }
}
