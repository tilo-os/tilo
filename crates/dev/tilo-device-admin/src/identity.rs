//! The keys and certificates each side of the route uses.
//!
//! Both sides keep the Tilo identity layout ([`IdentityPaths`]): a device
//! under `/var/lib/tilo`, a developer's machine under its own directory
//! (`tilo-vm` keeps it beside its registry). A developer's machine enrols as
//! one of the person's devices like any other: it has a device key, and the
//! person's root certifies that key with
//! [`DeviceCapabilities::administrator`](tilo_user_identity::DeviceCapabilities::administrator)
//! (`tilo-enroll certify --capability admin`).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{
    DeviceCert, IdentityPaths, RevocationEntry, RevocationList, persist, verify_device_cert,
};

/// The device's identity root.
pub const SYSTEM_ROOT: &str = "/var/lib/tilo";

/// The transport secret of the device's admin endpoint, derived from its
/// device key. The route is served by its own process beside the session's
/// endpoint; one endpoint id published by two processes would take each
/// other's relay slot, so the route's endpoint gets its own id, as
/// tilo-relay's does. The device key itself still proves who answers, in the
/// handshake.
pub fn transport_secret(device_key: &SigningKey) -> [u8; 32] {
    blake3::derive_key(
        "tilo-device-admin-iroh-service-key-v1",
        &device_key.to_bytes(),
    )
}

/// The admin endpoint id that belongs to `device_key`.
pub fn endpoint_id(device_key: &SigningKey) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&transport_secret(device_key)).public()
}

/// One side's identity: its device key and, once enrolled, its certificate.
pub struct Identity {
    pub paths: IdentityPaths,
    pub key: SigningKey,
    pub cert: Option<DeviceCert>,
}

impl Identity {
    /// Read the identity under `root`. A device key is required; the
    /// certificate is optional until the device is enrolled.
    ///
    /// The certificate's signature is not checked here: the other side
    /// checks it on every session, and refuses one that does not verify.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let paths = IdentityPaths::under(root);
        let key = persist::read_device_key(&paths.device_privkey)
            .with_context(|| format!("read the device key {}", paths.device_privkey.display()))?;
        let cert = match std::fs::symlink_metadata(&paths.device_cert) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            _ => Some(read_cert_for(&paths.device_cert, &key)?),
        };
        Ok(Self { paths, key, cert })
    }

    /// The device public key: this side's device id.
    pub fn device_id(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// The revocations this side has verified for its identity. A missing
    /// list is empty; a damaged one is an error rather than an empty list.
    ///
    /// # Errors
    /// An unreadable list, or an entry whose root signature does not verify.
    pub fn revocations(&self) -> anyhow::Result<RevocationList> {
        load_revocations(&revocations_path(&self.paths))
    }
}

fn read_cert_for(path: &Path, key: &SigningKey) -> anyhow::Result<DeviceCert> {
    let cert = persist::read_device_cert(path)
        .with_context(|| format!("read the device certificate {}", path.display()))?;
    if cert.device_public_key != key.verifying_key().to_bytes() {
        bail!(
            "{} certifies another device key than this machine's",
            path.display()
        );
    }
    Ok(cert)
}

/// Where the identity keeps its verified revocations (beside the person's
/// public key, as the filesystem and relay read them).
pub fn revocations_path(paths: &IdentityPaths) -> PathBuf {
    paths
        .user_pubkey
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("revocations.bin")
}

pub fn load_revocations(path: &Path) -> anyhow::Result<RevocationList> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RevocationList::new());
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let entries: Vec<RevocationEntry> =
        postcard::from_bytes(&bytes).with_context(|| format!("decode {}", path.display()))?;
    let mut list = RevocationList::new();
    for entry in entries {
        list.insert(entry)
            .with_context(|| format!("{} holds an entry that does not verify", path.display()))?;
    }
    Ok(list)
}

/// Make the dialing machine's device key under `root` if it has none, and
/// return its public half.
///
/// # Errors
/// An existing key that cannot be read, or one that cannot be written.
pub fn ensure_key(root: &Path) -> anyhow::Result<[u8; 32]> {
    let paths = IdentityPaths::under(root);
    let (key, _) = persist::load_or_create_device_key(&paths)
        .with_context(|| format!("make or read the device key under {}", root.display()))?;
    Ok(key.verifying_key().to_bytes())
}

/// Install a root-signed certificate for the dialing machine's key: it must
/// name this machine's key, verify, and grant administration.
///
/// # Errors
/// A certificate that is not this machine's, does not verify, or does not
/// grant administration; or a write failure.
pub fn install_cert(root: &Path, cert_path: &Path) -> anyhow::Result<DeviceCert> {
    let paths = IdentityPaths::under(root);
    let key = persist::read_device_key(&paths.device_privkey).with_context(|| {
        format!(
            "this machine has no device key under {}; make one first",
            root.display()
        )
    })?;
    let cert = read_cert_for(cert_path, &key)?;
    verify_device_cert(&cert)
        .with_context(|| format!("{} does not verify", cert_path.display()))?;
    if !cert.capabilities.can_administer_devices {
        bail!(
            "{} does not grant administration; certify this machine with `--capability admin`",
            cert_path.display()
        );
    }
    paths.ensure_directories()?;
    persist::write_device_cert(&paths.device_cert, &cert)?;
    persist::write_public_key(&paths.user_pubkey, &cert.user_public_key)?;
    Ok(cert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn certify(
        root: &SigningKey,
        device: [u8; 32],
        capabilities: DeviceCapabilities,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "workstation".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// A dialing machine makes its key once, installs only a certificate for
    /// that key that grants administration, and loads it back.
    #[test]
    fn a_dialing_machine_enrols_with_an_administration_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let device = ensure_key(dir.path()).unwrap();
        assert_eq!(ensure_key(dir.path()).unwrap(), device);
        let root = SigningKey::from_bytes(&[21; 32]);
        let write = |name: &str, cert: &DeviceCert| {
            let path = dir.path().join(name);
            persist::write_device_cert(&path, cert).unwrap();
            path
        };

        let member = write(
            "member.cert",
            &certify(&root, device, DeviceCapabilities::member()),
        );
        let error = install_cert(dir.path(), &member).unwrap_err().to_string();
        assert!(error.contains("--capability admin"), "{error}");
        let other = write(
            "other.cert",
            &certify(&root, [9; 32], DeviceCapabilities::administrator()),
        );
        assert!(install_cert(dir.path(), &other).is_err());
        assert!(Identity::load(dir.path()).unwrap().cert.is_none());

        let admin = certify(&root, device, DeviceCapabilities::administrator());
        let path = write("admin.cert", &admin);
        assert_eq!(install_cert(dir.path(), &path).unwrap(), admin);
        let identity = Identity::load(dir.path()).unwrap();
        assert_eq!(identity.cert, Some(admin));
        assert_eq!(identity.device_id(), device);
        assert!(identity.revocations().unwrap().is_empty());
    }

    #[test]
    fn the_admin_endpoint_is_not_the_device_endpoint() {
        let key = SigningKey::from_bytes(&[22; 32]);
        let admin = endpoint_id(&key);
        assert_ne!(admin.as_bytes(), &key.verifying_key().to_bytes());
        assert_eq!(admin, endpoint_id(&key));
    }
}
