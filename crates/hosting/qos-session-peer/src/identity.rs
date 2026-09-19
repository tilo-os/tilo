//! Existing native identity paths are the only private-key owner.
use crate::store::{Custody, check_dir, custody, private_dir, sync_dir};
use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity,
    persist::{self, IdentityPaths},
    sign_device_cert, verify_device_cert_at,
};
use std::path::Path;
pub(crate) struct Identity {
    pub root: SigningKey,
    pub native: SigningKey,
    pub native_cert: DeviceCert,
    pub host: SigningKey,
    pub host_cert: DeviceCert,
    _custody: Custody,
}
fn directories(root: &Path) -> Result<IdentityPaths> {
    private_dir(root)?;
    private_dir(&root.join("user-identity"))?;
    private_dir(&root.join("device"))?;
    Ok(IdentityPaths::under(root))
}
fn enroll(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> Result<(SigningKey, DeviceCert)> {
    let now = qos_relay::clock::now_ns();
    let (key, device) = DeviceIdentity::generate(label, now);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.public_key,
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root)?;
    Ok((key, cert))
}
pub(crate) fn initialize(root: &Path, host: &Path) -> Result<Identity> {
    let paths = directories(root)?;
    let host_paths = directories(host)?;
    let (key, user) =
        UserIdentity::generate("Local qOS session authority", qos_relay::clock::now_ns());
    let (native_key, native_cert) =
        enroll(&key, "Native workspace peer", DeviceCapabilities::primary())?;
    let (host_key, host_cert) = enroll(&key, "Local native relay", DeviceCapabilities::host())?;
    persist::write_public_key(&paths.user_pubkey, &user.public_key)?;
    persist::write_signing_key(&paths.user_privkey, &key)?;
    persist::adopt_device_key(&paths, &native_key.to_bytes())?;
    persist::write_device_cert(&paths.device_cert, &native_cert)?;
    persist::write_public_key(&host_paths.user_pubkey, &user.public_key)?;
    persist::adopt_device_key(&host_paths, &host_key.to_bytes())?;
    persist::write_device_cert(&host_paths.device_cert, &host_cert)?;
    for dir in [root, host] {
        sync_dir(&dir.join("user-identity"))?;
        sync_dir(&dir.join("device"))?;
        sync_dir(dir)?;
    }
    load(root, host)
}
pub(crate) fn load(root: &Path, host: &Path) -> Result<Identity> {
    for dir in [root, host] {
        check_dir(dir)?;
        check_dir(&dir.join("user-identity"))?;
        check_dir(&dir.join("device"))?;
    }
    let lock = custody(&root.join("session-custody.lock"))?;
    let paths = IdentityPaths::under(root);
    let hp = IdentityPaths::under(host);
    for path in [
        &paths.user_privkey,
        &paths.user_pubkey,
        &paths.device_privkey,
        &paths.device_cert,
        &hp.user_pubkey,
        &hp.device_privkey,
        &hp.device_cert,
    ] {
        if !std::fs::symlink_metadata(path)?.is_file() {
            bail!("identity storage must use regular files, not symbolic links");
        }
    }
    let root_key = persist::read_signing_key(&paths.user_privkey)?;
    let root_public = root_key.verifying_key().to_bytes();
    let native = persist::read_device_key(&paths.device_privkey)?;
    let host_key = persist::read_device_key(&hp.device_privkey)?;
    let native_cert = persist::read_device_cert(&paths.device_cert)?;
    let host_cert = persist::read_device_cert(&hp.device_cert)?;
    for (cert, public) in [
        (&native_cert, native.verifying_key().to_bytes()),
        (&host_cert, host_key.verifying_key().to_bytes()),
    ] {
        verify_device_cert_at(cert, qos_relay::clock::now_ns())?;
        if cert.device_public_key != public || cert.user_public_key != root_public {
            bail!("native identity and certificate disagree");
        }
    }
    if persist::read_public_key(&paths.user_pubkey)? != root_public
        || persist::read_public_key(&hp.user_pubkey)? != root_public
        || !native_cert.capabilities.can_own_workspaces
        || !host_cert.capabilities.can_act_as_host
    {
        bail!("native authority or relay capability mismatch");
    }
    Ok(Identity {
        root: root_key,
        native,
        native_cert,
        host: host_key,
        host_cert,
        _custody: lock,
    })
}
