//! The host's check before its primary Files mount starts: the identity it mounts as is the
//! existing certified one, never a replacement. What the mount may read and write is the
//! workspace's people and grants, which the session tells the mount; nothing is signed here.
use std::path::Path;

use anyhow::{Context, Result};
use tilo_device_identity::{IdentityPaths, persist};

fn existing_person(paths: &IdentityPaths) -> Result<[u8; 32]> {
    let key = persist::load_device_key(paths).context("read existing certified device key")?;
    let person = persist::read_public_key(&paths.user_pubkey)?;
    let cert = persist::read_device_cert(&paths.device_cert)?;
    tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns())?;
    anyhow::ensure!(
        cert.user_public_key == person && cert.device_public_key == key.verifying_key().to_bytes(),
        "existing identity projection is inconsistent"
    );
    let revocations = tilo_fs::load_revocation_list(paths)?;
    anyhow::ensure!(
        !revocations.refuses_at(&cert, tilo_clock::unix_now_ns()),
        "existing device is revoked"
    );
    Ok(person)
}

pub(super) fn check(root: &Path) -> Result<()> {
    let person = existing_person(&IdentityPaths::under(root))?;
    println!(
        "This device is certified for person {}; no state changed.",
        hex::encode(person)
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed step is the assertion")]
mod tests {
    use super::*;

    #[test]
    fn only_an_existing_certified_identity_passes_the_check() {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

        let temp = tempfile::tempdir().unwrap();
        let identity = temp.path().join("identity");
        assert!(
            check(&identity).is_err(),
            "no identity is never one made here"
        );
        assert!(!identity.join("device.key").exists());
        let paths = IdentityPaths::under(&identity);
        paths.ensure_directories().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let person = root.verifying_key().to_bytes();
        persist::write_public_key(&paths.user_pubkey, &person).unwrap();
        persist::write_device_key(&paths.device_privkey, &device).unwrap();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "isolated source fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        check(&identity).unwrap();
        cert.signature[0] ^= 1;
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        assert!(check(&identity).is_err());
    }
}
