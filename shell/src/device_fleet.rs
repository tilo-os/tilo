//! Persistent device enrollment and revocation for each local user identity.

use std::fs;
use std::io;
use std::path::PathBuf;

use qos_user_identity::{
    DeviceCapabilities, DeviceCert, EnrollmentToken, IdentityPaths, RevocationEntry,
    RevocationReason, decode_enrollment_token, encode_enrollment_token, mint_enrollment_token,
    revocation_signing_digest, verify_device_cert, verify_enrollment_token,
    verify_revocation_entry,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

const FLEET_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFleet {
    pub schema_version: u32,
    pub identity_id: String,
    pub certificates: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
}

impl DeviceFleet {
    fn new(identity_id: String) -> Self {
        Self {
            schema_version: FLEET_SCHEMA_VERSION,
            identity_id,
            certificates: Vec::new(),
            revocations: Vec::new(),
        }
    }

    pub fn is_revoked(&self, device_public_key: &[u8; 32]) -> bool {
        self.revocations
            .iter()
            .any(|entry| &entry.revoked_device_public_key == device_public_key)
    }
}

#[derive(Clone, Debug)]
pub struct DeviceFleetStore {
    identity_dir: PathBuf,
    identity_paths: IdentityPaths,
}

impl DeviceFleetStore {
    /// Open the device fleet store rooted at the environment's identity
    /// registry and collaboration identity paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity registry's environment (neither
    /// `XDG_CONFIG_HOME` nor `HOME` set) can't be resolved.
    pub fn from_env() -> io::Result<Self> {
        let registry = crate::identity_registry::IdentityRegistryStore::from_env()?;
        Ok(Self::at(
            registry.identity_dir(),
            crate::firstboot::collaboration_identity_paths(),
        ))
    }

    pub fn at(identity_dir: impl Into<PathBuf>, identity_paths: IdentityPaths) -> Self {
        Self {
            identity_dir: identity_dir.into(),
            identity_paths,
        }
    }

    /// Load the persisted device fleet for `identity_id`, or a fresh empty
    /// one if none has been saved yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but can't be read or parsed, if
    /// its schema version or identity id doesn't match, or if any stored
    /// certificate or revocation entry fails verification.
    pub fn load(&self, identity_id: &str) -> io::Result<DeviceFleet> {
        let path = self.path(identity_id);
        if !path.exists() {
            return Ok(DeviceFleet::new(identity_id.to_string()));
        }
        let fleet: DeviceFleet = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if fleet.schema_version != FLEET_SCHEMA_VERSION || fleet.identity_id != identity_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "device fleet identity/schema mismatch",
            ));
        }
        for cert in &fleet.certificates {
            verify_device_cert(cert)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        for entry in &fleet.revocations {
            verify_revocation_entry(entry)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        Ok(fleet)
    }

    /// Ensure the current machine appears in the fleet for `identity_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity can't be loaded, if this device's
    /// certificate can't be read, or if persisting the updated fleet fails.
    pub fn register_current_device(&self, identity_id: &str) -> io::Result<DeviceFleet> {
        let (_, profile) = self.identity(identity_id)?;
        let cert_path = self
            .identity_paths
            .device_cert_for_user(&profile.user_public_key);
        let cert = qos_user_identity::persist::read_device_cert(&cert_path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.insert_certificate(identity_id, cert)
    }

    /// Mint a short-lived enrollment token from this already trusted device.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity or this device's key/certificate
    /// can't be loaded, if the certificate doesn't verify or doesn't grant
    /// `can_admit_devices`, or if minting or encoding the token fails.
    pub fn mint_enrollment(
        &self,
        identity_id: &str,
        capabilities: DeviceCapabilities,
        expires_at_ns: u64,
    ) -> io::Result<String> {
        let (_, profile) = self.identity(identity_id)?;
        let device =
            qos_user_identity::persist::read_device_key(&self.identity_paths.device_privkey)
                .map_err(|error| io::Error::other(error.to_string()))?;
        let cert_path = self
            .identity_paths
            .device_cert_for_user(&profile.user_public_key);
        let cert = qos_user_identity::persist::read_device_cert(&cert_path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        verify_device_cert(&cert).map_err(|error| io::Error::other(error.to_string()))?;
        if cert.device_public_key != device.verifying_key().to_bytes()
            || !cert.capabilities.can_admit_devices
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this device may not admit another device",
            ));
        }
        let mut nonce = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let token = mint_enrollment_token(
            profile.user_public_key,
            capabilities,
            expires_at_ns,
            nonce,
            &device,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        encode_enrollment_token(&token).map_err(|error| io::Error::other(error.to_string()))
    }

    /// Complete enrollment after the new device returns its public key.
    ///
    /// # Errors
    ///
    /// Returns an error if the token can't be decoded, has expired, or names
    /// an issuer other than `identity_id`'s user key; if the identity can't
    /// be loaded; if signing or verifying the new certificate fails; or if
    /// persisting it fails.
    pub fn complete_enrollment(
        &self,
        identity_id: &str,
        encoded_token: &str,
        new_device_public_key: [u8; 32],
        device_label: impl Into<String>,
        now_ns: u64,
    ) -> io::Result<DeviceCert> {
        let token = decode_enrollment_token(encoded_token)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        verify_enrollment_token(&token, now_ns)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        let (root, profile) = self.identity(identity_id)?;
        self.validate_issuer(&token, &profile.user_public_key)?;
        let mut cert = DeviceCert {
            user_public_key: profile.user_public_key,
            device_public_key: new_device_public_key,
            device_label: device_label.into(),
            capabilities: token.capabilities,
            issued_at_ns: now_ns,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let digest = qos_user_identity::device_cert_signing_digest(&cert)
            .map_err(|error| io::Error::other(error.to_string()))?;
        cert.signature = root
            .sign(&digest)
            .map_err(|error| io::Error::other(error.to_string()))?;
        verify_device_cert(&cert).map_err(|error| io::Error::other(error.to_string()))?;
        self.insert_certificate(identity_id, cert.clone())?;
        Ok(cert)
    }

    /// Root-sign a permanent revocation and persist it with the fleet.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity or fleet can't be loaded, if
    /// `device_public_key` isn't enrolled under `identity_id`, if signing
    /// the revocation fails, or if persisting the updated fleet fails.
    pub fn revoke(
        &self,
        identity_id: &str,
        device_public_key: [u8; 32],
        reason: RevocationReason,
        revoked_at_ns: u64,
    ) -> io::Result<RevocationEntry> {
        let (root, profile) = self.identity(identity_id)?;
        let mut fleet = self.load(identity_id)?;
        if !fleet
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == device_public_key)
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "device is not enrolled under this identity",
            ));
        }
        let mut entry = RevocationEntry {
            user_public_key: profile.user_public_key,
            revoked_device_public_key: device_public_key,
            revoked_at_ns,
            reason,
            signature: [0; 64],
        };
        let digest = revocation_signing_digest(&entry)
            .map_err(|error| io::Error::other(error.to_string()))?;
        entry.signature = root
            .sign(&digest)
            .map_err(|error| io::Error::other(error.to_string()))?;
        verify_revocation_entry(&entry).map_err(|error| io::Error::other(error.to_string()))?;
        if !fleet.is_revoked(&device_public_key) {
            fleet.revocations.push(entry.clone());
            self.save(&fleet)?;
        }
        Ok(entry)
    }

    fn validate_issuer(&self, token: &EnrollmentToken, user_key: &[u8; 32]) -> io::Result<()> {
        if &token.user_public_key != user_key {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "enrollment token belongs to another identity",
            ));
        }
        let fleet = self.load(&hex_encode(user_key))?;
        let issuer = fleet
            .certificates
            .iter()
            .find(|cert| cert.device_public_key == token.issuer_device_public_key)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "issuer not enrolled")
            })?;
        if !issuer.capabilities.can_admit_devices || fleet.is_revoked(&issuer.device_public_key) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "issuer is revoked or cannot admit devices",
            ));
        }
        Ok(())
    }

    fn insert_certificate(&self, identity_id: &str, cert: DeviceCert) -> io::Result<DeviceFleet> {
        verify_device_cert(&cert).map_err(|error| io::Error::other(error.to_string()))?;
        let mut fleet = self.load(identity_id)?;
        if cert.user_public_key != decode_hex_32(identity_id)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device certificate belongs to another identity",
            ));
        }
        fleet
            .certificates
            .retain(|existing| existing.device_public_key != cert.device_public_key);
        fleet.certificates.push(cert);
        self.save(&fleet)?;
        Ok(fleet)
    }

    fn identity(
        &self,
        identity_id: &str,
    ) -> io::Result<(
        runtime_identity::SigningIdentity,
        crate::identity_registry::IdentityProfile,
    )> {
        let store = crate::identity_registry::IdentityRegistryStore::at(&self.identity_dir);
        let registry = store.migrate_legacy_primary()?;
        let profile = registry
            .identities
            .get(identity_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "identity is not registered"))?;
        let identity = runtime_identity::load_from_path(profile.record_path.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if identity.group_public_key() != profile.user_public_key {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity record does not match its registry profile",
            ));
        }
        Ok((identity, profile))
    }

    fn save(&self, fleet: &DeviceFleet) -> io::Result<()> {
        let path = self.path(&fleet.identity_id);
        let bytes = serde_json::to_vec_pretty(fleet)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        atomic_file::write_creating_parent(&path, &bytes)
    }

    fn path(&self, identity_id: &str) -> PathBuf {
        self.identity_dir
            .join("fleets")
            .join(format!("{identity_id}.json"))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex_32(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity id is not 32-byte hex",
        ));
    }
    let mut out = [0_u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        let end = start.saturating_add(2);
        let chunk = value
            .get(start..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid identity hex"))?;
        *slot = u8::from_str_radix(chunk, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid identity hex"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_list_and_revoke_a_secondary_device() {
        let root = tempfile::tempdir().expect("temp root");
        let identity_dir = root.path().join("config/identity");
        let paths = IdentityPaths::under(root.path().join("system"));
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths).expect("provision");
        let registry = crate::identity_registry::IdentityRegistryStore::at(&identity_dir)
            .load()
            .expect("registry");
        let identity_id = registry.default_identity.expect("default identity");
        let store = DeviceFleetStore::at(&identity_dir, paths);

        let before = store.load(&identity_id).expect("fleet");
        assert_eq!(before.certificates.len(), 1);
        let token = store
            .mint_enrollment(&identity_id, DeviceCapabilities::member(), 10_000)
            .expect("token");
        let phone = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let cert = store
            .complete_enrollment(
                &identity_id,
                &token,
                phone.verifying_key().to_bytes(),
                "Phone",
                5_000,
            )
            .expect("complete");
        assert_eq!(cert.device_label, "Phone");
        assert!(!cert.capabilities.can_admit_devices);
        assert_eq!(
            store.load(&identity_id).expect("fleet").certificates.len(),
            2
        );

        store
            .revoke(
                &identity_id,
                phone.verifying_key().to_bytes(),
                RevocationReason::Lost,
                6_000,
            )
            .expect("revoke");
        assert!(
            store
                .load(&identity_id)
                .expect("fleet")
                .is_revoked(&phone.verifying_key().to_bytes())
        );
    }
}
