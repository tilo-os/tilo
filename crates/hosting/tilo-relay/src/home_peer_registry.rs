//! The machine's registry of enrolled home peers (`home-peers.json`).
//!
//! An approving device signs a home peer's certificate with the user root in
//! the owner's unprivileged session. Only the finished, root-signed
//! [`HomePeerAdmission`] reaches this registry: an identity root the approver
//! can write takes it directly, and the machine-wide `/var/lib/tilo` takes it
//! through `tilo-home-peer-control register-home-peer`, which re-verifies it
//! with [`verify_admission`] and never holds a root signer itself.

use std::path::Path;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tilo_relay_protocol::HomePeerRegistration;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, verify_device_cert, verify_device_cert_at,
};

/// Largest registry this machine keeps.
const MAX_REGISTRATIONS: usize = 128;
/// Largest number of relay workspaces one home peer serves.
const MAX_RELAY_WORKSPACES: usize = 4096;
/// Largest encoded registry or admission accepted.
pub const MAX_REGISTRY_BYTES: usize = 1024 * 1024;

/// A home peer the user root has certified, as the approving device hands it
/// to the privileged registry writer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomePeerAdmission {
    /// The home peer's root-signed host certificate.
    pub cert: DeviceCert,
    /// The relay service node id the home peer announced while enrolling.
    pub relay_service_node_id: [u8; 32],
    /// Relay workspaces the home peer serves for this identity.
    pub relay_workspace_ids: Vec<u64>,
}

/// Whether a registration's optional relay endpoint names its own service.
pub fn valid_relay_endpoint_hint(entry: &HomePeerRegistration) -> bool {
    entry.relay_endpoint.as_ref().is_none_or(|endpoint| {
        !endpoint.is_empty()
            && hex::decode(&entry.relay_service_node_id)
                .ok()
                .is_some_and(|service| service.as_slice() == endpoint.id.as_bytes())
    })
}

/// Check that `admission` is a host this machine's identity certified.
///
/// The certificate must be a currently valid host certificate, and this
/// machine's device must hold a valid certificate from the same user root
/// that lets it admit devices; that is what makes the approving session the
/// identity's own. The continuity workspace must be among the served ones.
///
/// # Errors
///
/// Returns an error describing the first check that fails.
pub fn verify_admission(
    identity_root: &Path,
    admission: &HomePeerAdmission,
    now_ns: u64,
) -> anyhow::Result<()> {
    let cert = &admission.cert;
    verify_device_cert_at(cert, now_ns).context("home-peer certificate is not valid")?;
    if cert.capabilities != DeviceCapabilities::host() {
        bail!("home-peer certificate does not grant exactly the host capabilities");
    }
    let user = cert.user_public_key;
    if admission.relay_workspace_ids.len() > MAX_RELAY_WORKSPACES
        || !admission.relay_workspace_ids.contains(
            &tilo_relay_protocol::personal_continuity_workspace_id(&user),
        )
    {
        bail!("home-peer admission does not serve its identity's continuity workspace");
    }
    let paths = IdentityPaths::under(identity_root);
    let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
        .context("read this machine's device key")?;
    let issuer = tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))
        .context("this machine holds no certificate from the admitting identity")?;
    verify_device_cert_at(&issuer, now_ns).context("this machine's certificate is not valid")?;
    if issuer.user_public_key != user
        || issuer.device_public_key != device.verifying_key().to_bytes()
        || !issuer.capabilities.can_admit_devices
    {
        bail!("the admitting identity has not authorised this machine to admit devices");
    }
    Ok(())
}

/// Add or replace one home peer in `identity_root/home-peers.json`.
///
/// # Errors
///
/// Returns an error if the registration or the existing registry violates
/// its bounded, verified schema, or the registry cannot be written.
pub fn register_home_peer(
    identity_root: &Path,
    cert: &DeviceCert,
    relay_service_node_id: [u8; 32],
    relay_workspace_ids: &[u64],
) -> anyhow::Result<()> {
    let path = identity_root.join("home-peers.json");
    if !tilo_relay_protocol::valid_home_peer_label(&cert.device_label)
        || relay_workspace_ids.len() > MAX_RELAY_WORKSPACES
    {
        bail!("home-peer registration exceeds its bounded schema");
    }
    let mut registrations = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= MAX_REGISTRY_BYTES => {
            serde_json::from_slice::<Vec<HomePeerRegistration>>(&bytes)
                .context("existing home-peer registry is invalid")?
        }
        Ok(_) => bail!("existing home-peer registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if registrations.len() > MAX_REGISTRATIONS
        || registrations.iter().any(|entry| {
            entry.device_node_id.len() != 64
                || hex::decode(&entry.device_node_id).is_err()
                || entry.relay_service_node_id.len() != 64
                || hex::decode(&entry.relay_service_node_id).is_err()
                || entry.user_public_key.len() != 64
                || hex::decode(&entry.user_public_key).is_err()
                || !valid_relay_endpoint_hint(entry)
                || entry.device_cert.as_ref().is_some_and(|cert| {
                    verify_device_cert(cert).is_err()
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                        || hex::encode(cert.user_public_key) != entry.user_public_key
                })
                || !tilo_relay_protocol::valid_home_peer_label(&entry.label)
                || entry.relay_workspace_ids.len() > MAX_RELAY_WORKSPACES
        })
    {
        bail!("existing home-peer registry violates its bounded schema");
    }
    let device_node_id = hex::encode(cert.device_public_key);
    registrations.retain(|entry| entry.device_node_id != device_node_id);
    registrations.push(HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id,
        relay_service_node_id: hex::encode(relay_service_node_id),
        relay_endpoint: None,
        label: cert.device_label.clone(),
        user_public_key: hex::encode(cert.user_public_key),
        device_cert: Some(cert.clone()),
        relay_workspace_ids: relay_workspace_ids.to_vec(),
    });
    if registrations.len() > MAX_REGISTRATIONS {
        bail!("home-peer registry is full");
    }
    registrations.sort_by(|left, right| left.device_node_id.cmp(&right.device_node_id));
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&registrations)?, 0o644)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::sign_device_cert;

    fn cert(
        root: &ed25519_dalek::SigningKey,
        device: [u8; 32],
        capabilities: DeviceCapabilities,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "Home server".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// A machine whose device holds `capabilities` from `root`.
    fn machine(
        root: &ed25519_dalek::SigningKey,
        capabilities: DeviceCapabilities,
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path());
        paths.ensure_directories().unwrap();
        let device = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root.verifying_key().to_bytes()),
            &cert(root, device.verifying_key().to_bytes(), capabilities),
        )
        .unwrap();
        dir
    }

    fn admission(root: &ed25519_dalek::SigningKey) -> HomePeerAdmission {
        let user = root.verifying_key().to_bytes();
        HomePeerAdmission {
            cert: cert(root, [4; 32], DeviceCapabilities::host()),
            relay_service_node_id: [5; 32],
            relay_workspace_ids: vec![
                7,
                tilo_relay_protocol::personal_continuity_workspace_id(&user),
            ],
        }
    }

    #[test]
    fn admission_from_an_identity_that_admits_through_this_machine_verifies() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let dir = machine(&root, DeviceCapabilities::primary());
        let admission = admission(&root);
        verify_admission(dir.path(), &admission, 2).unwrap();
        register_home_peer(
            dir.path(),
            &admission.cert,
            admission.relay_service_node_id,
            &admission.relay_workspace_ids,
        )
        .unwrap();
        let stored: Vec<HomePeerRegistration> =
            serde_json::from_slice(&std::fs::read(dir.path().join("home-peers.json")).unwrap())
                .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].device_cert.as_ref(), Some(&admission.cert));
    }

    #[test]
    fn admission_is_refused_unless_every_binding_holds() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let dir = machine(&root, DeviceCapabilities::primary());

        let mut member = admission(&root);
        member.cert = cert(&root, [4; 32], DeviceCapabilities::member());
        assert!(verify_admission(dir.path(), &member, 2).is_err());

        let mut tampered = admission(&root);
        tampered.cert.device_label = "Changed".into();
        assert!(verify_admission(dir.path(), &tampered, 2).is_err());

        let mut no_continuity = admission(&root);
        no_continuity.relay_workspace_ids = vec![7];
        assert!(verify_admission(dir.path(), &no_continuity, 2).is_err());

        let stranger = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        assert!(verify_admission(dir.path(), &admission(&stranger), 2).is_err());

        let member_machine = machine(&root, DeviceCapabilities::member());
        assert!(verify_admission(member_machine.path(), &admission(&root), 2).is_err());
    }
}
