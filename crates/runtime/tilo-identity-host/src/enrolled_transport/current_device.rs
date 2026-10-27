//! Password-authenticated reference to this host's existing owner identity.
//! No device seed or personal signing share travels; activation only reviews
//! the same existing canonical identity and never publishes system custody.
use super::{
    DeviceCert, DeviceFleet, MAX_BYTES, TransportReceipt, fs, invalid, password_valid,
    validate_fleet,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};
use tilo_seal::{Context, Label, PasswordParams, envelope};

use zeroize::Zeroizing;

const DOMAIN: &[u8] = b"tilo-prepared-current-device/v1\0";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    schema: u32,
    certificate: DeviceCert,
    fleet: DeviceFleet,
    record_sha256: [u8; 32],
    record_name: String,
}
impl Reference {
    fn validate(&self, root: [u8; 32], device: [u8; 32], now: u64) -> io::Result<()> {
        if self.schema != 1
            || self.certificate.user_public_key != root
            || self.certificate.device_public_key != device
            || self.record_name.is_empty()
            || self.record_name.len() > 128
            || self.record_name.contains(['/', '\\'])
            || self.record_name == "."
            || self.record_name == ".."
        {
            return Err(invalid());
        }
        tilo_user_identity::verify_device_cert_at(&self.certificate, now).map_err(|_| invalid())?;
        validate_fleet(&self.fleet, &self.certificate)
    }
}
fn context(binding: &str) -> io::Result<Vec<u8>> {
    // Reuse the canonical operation-binding validation, with a separate domain.
    super::context(binding)?;
    let mut bytes = DOMAIN.to_vec();
    bytes.extend_from_slice(binding.as_bytes());
    Ok(bytes)
}
fn now() -> io::Result<u64> {
    tilo_clock::try_unix_now_ns().ok_or_else(invalid)
}

/// Public comparison facts; deserializing these never admits an identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentDeviceDeclaration {
    schema: u32,
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    record_name: String,
    record_sha256: [u8; 32],
    certificate_sha256: [u8; 32],
    fleet_sha256: [u8; 32],
}
impl CurrentDeviceDeclaration {
    pub fn public_key(&self) -> [u8; 32] {
        self.user_public_key
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.device_public_key
    }
    pub fn record_name(&self) -> &str {
        &self.record_name
    }
    /// Resolve the current owner only if it matches these comparison facts.
    ///
    /// # Errors
    /// Returns an error if current-owner review or revalidation fails, the
    /// declaration cannot be encoded, or any comparison fact differs.
    pub fn resolve_current(
        &self,
        system: &Path,
        identity: &Path,
    ) -> io::Result<ResolvedCurrentIdentity> {
        let current = CurrentDeviceReference::review(system, identity)?;
        if &current.declaration()? != self {
            return Err(invalid());
        }
        current.validate()?;
        Ok(ResolvedCurrentIdentity { current })
    }
}
impl Reference {
    fn declaration(&self) -> io::Result<CurrentDeviceDeclaration> {
        Ok(CurrentDeviceDeclaration {
            schema: 1,
            user_public_key: self.certificate.user_public_key,
            device_public_key: self.certificate.device_public_key,
            record_name: self.record_name.clone(),
            record_sha256: self.record_sha256,
            certificate_sha256: Sha256::digest(
                tilo_user_identity::canonical::canonical_bytes(&self.certificate)
                    .map_err(|_| invalid())?,
            )
            .into(),
            fleet_sha256: Sha256::digest(serde_json::to_vec(&self.fleet).map_err(|_| invalid())?)
                .into(),
        })
    }
}

/// Held source reference. This is deliberately neither serializable nor Clone.
pub struct CurrentDeviceReference {
    system: fs::Directory,
    identity: fs::Directory,
    identity_directory: PathBuf,
    reference: Reference,
}
impl CurrentDeviceReference {
    /// Review the active identity only when it is the canonical current owner.
    /// This reads existing custody; it never creates a key, cert or selector.
    ///
    /// # Errors
    /// Returns an error for inaccessible or insecure namespaces/files, an invalid
    /// selector or record, or an identity that does not own the host. Also refuses
    /// invalid, expired or revoked certification, inconsistent fleet/key custody,
    /// changes during review, and an unavailable clock.
    pub fn review(system_root: &Path, identity_directory: &Path) -> io::Result<Self> {
        let system = fs::Directory::open_public_root(system_root)?;
        let identity = fs::Directory::open(identity_directory)?;
        Self::from_directories(system, identity, identity_directory.to_owned())
    }
    fn from_directories(
        system: fs::Directory,
        identity: fs::Directory,
        identity_directory: PathBuf,
    ) -> io::Result<Self> {
        let selector = identity.optional("active", true, 128)?;
        let id = runtime_identity::selected_identity_id_from_bytes(
            selector.as_deref().map(Vec::as_slice),
        )?;
        let record_name = format!("{id}.json");
        let record_bytes = identity.file(&record_name, true, MAX_BYTES)?;
        let metadata =
            runtime_identity::load_metadata_from_bytes(&record_bytes).map_err(|_| invalid())?;
        let owner = system
            .child("user-identity", false)?
            .file("pubkey.bin", false, 32)?;
        let certificate: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(
            &system
                .child("device", false)?
                .file("cert.bin", false, 65_536)?,
        )
        .map_err(|_| invalid())?;
        if owner.as_slice() != metadata.group_public_key
            || certificate.user_public_key != metadata.group_public_key
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "This identity does not own the current device",
            ));
        }
        let fleet = serde_json::from_slice(&identity.child("fleets", false)?.file(
            &format!("{}.json", hex::encode(metadata.group_public_key)),
            true,
            MAX_BYTES,
        )?)
        .map_err(|_| invalid())?;
        let source = Self {
            system,
            identity,
            identity_directory,
            reference: Reference {
                schema: 1,
                certificate,
                fleet,
                record_sha256: Sha256::digest(&record_bytes).into(),
                record_name,
            },
        };
        source.validate()?;
        Ok(source)
    }
    /// Return comparison facts for the revalidated current-owner epoch.
    ///
    /// # Errors
    /// Returns an error if [`Self::validate`] fails or certificate/fleet encoding
    /// fails while computing the declaration hashes.
    pub fn declaration(&self) -> io::Result<CurrentDeviceDeclaration> {
        self.validate()?;
        self.reference.declaration()
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.reference.certificate.user_public_key
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.reference.certificate.device_public_key
    }
    pub fn device_label(&self) -> &str {
        &self.reference.certificate.device_label
    }
    pub fn identity_directory(&self) -> &Path {
        &self.identity_directory
    }
    pub fn identity_record_name(&self) -> &str {
        &self.reference.record_name
    }
    /// Read metadata from the exact reviewed identity record.
    ///
    /// # Errors
    /// Returns an error if revalidation fails before or after reading, the private
    /// record cannot be read, its hash changed, or its metadata is malformed.
    pub fn metadata(&self) -> io::Result<runtime_identity::IdentityMetadata> {
        self.validate()?;
        let bytes = self
            .identity
            .file(&self.reference.record_name, true, MAX_BYTES)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != self.reference.record_sha256 {
            return Err(invalid());
        }
        let metadata = runtime_identity::load_metadata_from_bytes(&bytes).map_err(|_| invalid())?;
        self.validate()?;
        Ok(metadata)
    }
    /// Reattest the held namespaces and exact current-owner custody.
    ///
    /// # Errors
    /// Returns an error for inaccessible, insecure or changed namespaces/files,
    /// an unavailable clock, invalid or expired certification, invalid/revoked
    /// fleet membership, or changes to the owner, device key, active selector,
    /// record, certificate or fleet since review. Decode/encode failures also fail.
    pub fn validate(&self) -> io::Result<()> {
        self.system.validate()?;
        self.identity.validate()?;
        self.reference
            .validate(self.public_key(), self.device_public_key(), now()?)?;
        let owner = self
            .system
            .child("user-identity", false)?
            .file("pubkey.bin", false, 32)?;
        let selector = self.identity.optional("active", true, 128)?;
        let id = runtime_identity::selected_identity_id_from_bytes(
            selector.as_deref().map(Vec::as_slice),
        )?;
        if owner.as_slice() != self.public_key()
            || self
                .system
                .child("device-private", false)?
                .device_public_key()?
                != self.device_public_key()
            || format!("{id}.json") != self.reference.record_name
        {
            return Err(invalid());
        }
        let record = self
            .identity
            .file(&self.reference.record_name, true, MAX_BYTES)?;
        if <[u8; 32]>::from(Sha256::digest(&record)) != self.reference.record_sha256 {
            return Err(invalid());
        }
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(
            &self
                .system
                .child("device", false)?
                .file("cert.bin", false, 65_536)?,
        )
        .map_err(|_| invalid())?;
        if tilo_user_identity::canonical::canonical_bytes(&cert).map_err(|_| invalid())?
            != tilo_user_identity::canonical::canonical_bytes(&self.reference.certificate)
                .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        let fleet: DeviceFleet =
            serde_json::from_slice(&self.identity.child("fleets", false)?.file(
                &format!("{}.json", hex::encode(self.public_key())),
                true,
                MAX_BYTES,
            )?)
            .map_err(|_| invalid())?;
        validate_fleet(&fleet, &cert)?;
        // Any changed fleet, including revocations, invalidates the reviewed epoch.
        if serde_json::to_vec(&fleet).map_err(|_| invalid())?
            != serde_json::to_vec(&self.reference.fleet).map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        self.system.validate()?;
        self.identity.validate()
    }
    /// Public material from this exact held current-owner epoch. No seeds or
    /// identity shares are returned, and changes invalidate the observation.
    ///
    /// # Errors
    /// Returns an error if [`Self::validate`] fails before or after collecting
    /// the certificate and revocations.
    pub fn public_native_material(
        &self,
    ) -> io::Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>)> {
        self.validate()?;
        let result = (
            self.reference.certificate.clone(),
            self.reference.fleet.revocations.clone(),
        );
        self.validate()?;
        Ok(result)
    }
    /// Seal only the current-device reference, preserving the source exactly.
    ///
    /// # Errors
    /// Returns an error for invalid passwords/bindings, failed source revalidation,
    /// encoding/sealing failures or oversized plaintext. Refuses an invalid or
    /// insecure destination, overlap with either source root, existing output,
    /// publication/readback failures, mismatched ciphertext or failed authenticated
    /// readback (including expired certification or an unavailable clock).
    pub fn export_password_sealed(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<TransportReceipt> {
        password_valid(password)?;
        self.validate()?;
        let plain = Zeroizing::new(serde_json::to_vec(&self.reference).map_err(|_| invalid())?);
        if plain.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let context = context(binding)?;
        let sealed = envelope::seal_with_password(
            password,
            PasswordParams::default(),
            Context::new(Label::MigrationPackage, &context),
            &plain,
        )
        .map_err(|_| invalid())?;
        let parent = fs::Directory::open(destination.parent().ok_or_else(invalid)?)?;
        if parent.overlaps(&self.system) || parent.overlaps(&self.identity) {
            return Err(invalid());
        }
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        self.validate()?;
        parent.publish(name, &sealed)?;
        let readback = parent.file(name, true, MAX_BYTES + 256)?;
        if readback.as_slice() != sealed {
            return Err(invalid());
        }
        AuthenticatedCurrentDeviceReference::from_bytes(
            &readback,
            password,
            binding,
            self.public_key(),
            self.device_public_key(),
            now()?,
        )?;
        self.validate()?;
        Ok(TransportReceipt {
            user_public_key: self.public_key(),
            device_public_key: self.device_public_key(),
            sealed_bytes: sealed.len() as u64,
        })
    }
}

/// Authenticated reference; alone it grants no host or identity-write authority.
pub struct AuthenticatedCurrentDeviceReference {
    reference: Reference,
}
impl AuthenticatedCurrentDeviceReference {
    /// Authenticate a bounded reference for the expected owner and device.
    ///
    /// # Errors
    /// Returns an error for an invalid artifact path, inaccessible or insecure
    /// ciphertext, invalid passwords/bindings, failed authentication, oversized
    /// ciphertext/plaintext or malformed references. Refuses mismatched owner/device,
    /// invalid or expired certification at `now`, and invalid/revoked fleet membership.
    pub fn open(
        artifact: &Path,
        password: &[u8],
        binding: &str,
        root: [u8; 32],
        device: [u8; 32],
        now: u64,
    ) -> io::Result<Self> {
        let parent = fs::Directory::open(artifact.parent().ok_or_else(invalid)?)?;
        let name = artifact
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        Self::from_bytes(
            &parent.file(name, true, MAX_BYTES + 256)?,
            password,
            binding,
            root,
            device,
            now,
        )
    }
    fn from_bytes(
        bytes: &[u8],
        password: &[u8],
        binding: &str,
        root: [u8; 32],
        device: [u8; 32],
        now: u64,
    ) -> io::Result<Self> {
        password_valid(password)?;
        if bytes.len() as u64 > MAX_BYTES + 256 {
            return Err(invalid());
        }
        let context = context(binding)?;
        let plain = envelope::open_with_password(
            password,
            Context::new(Label::MigrationPackage, &context),
            bytes,
        )
        .map_err(|_| invalid())?;
        if plain.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let reference: Reference = serde_json::from_slice(&plain).map_err(|_| invalid())?;
        reference.validate(root, device, now)?;
        Ok(Self { reference })
    }
    pub fn device_label(&self) -> &str {
        &self.reference.certificate.device_label
    }
    /// Resolve the same real current-owner record. No import, repair, remint,
    /// selector update, system-key write or certificate replacement occurs.
    pub fn resolve_current(
        &self,
        system_root: &Path,
        identity_directory: &Path,
    ) -> io::Result<ResolvedCurrentIdentity> {
        let current = CurrentDeviceReference::review(system_root, identity_directory)?;
        if serde_json::to_vec(&current.reference).map_err(|_| invalid())?
            != serde_json::to_vec(&self.reference).map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        current.validate()?;
        Ok(ResolvedCurrentIdentity { current })
    }
}

/// Retained, nonserializable admission of the exact active original record.
/// Callers load through this handle, never reopen a returned ambient path.
pub struct ResolvedCurrentIdentity {
    current: CurrentDeviceReference,
}
impl ResolvedCurrentIdentity {
    /// Return comparison facts for the retained current owner.
    ///
    /// # Errors
    /// Returns an error if current-owner revalidation or certificate/fleet encoding
    /// fails; see [`CurrentDeviceReference::declaration`].
    pub fn declaration(&self) -> io::Result<CurrentDeviceDeclaration> {
        self.current.declaration()
    }
    /// Reattest the retained current-owner epoch.
    ///
    /// # Errors
    /// Returns an error for unavailable, insecure, changed or invalid current-owner
    /// custody, or an unavailable clock; see [`CurrentDeviceReference::validate`].
    pub fn validate(&self) -> io::Result<()> {
        self.current.validate()
    }
    /// Read metadata from the retained exact identity record.
    ///
    /// # Errors
    /// Returns an error for failed revalidation, an unreadable or changed record,
    /// or malformed metadata; see [`CurrentDeviceReference::metadata`].
    pub fn metadata(&self) -> io::Result<runtime_identity::IdentityMetadata> {
        self.current.metadata()
    }
    /// Load signing identity from the retained record through its secret provider.
    ///
    /// # Errors
    /// Returns an error if revalidation fails before or after loading, the record
    /// cannot be read or its hash changed, or the canonical loader refuses malformed
    /// records, unavailable/mismatched secret providers or secret/public inconsistencies.
    pub fn load(&self) -> io::Result<runtime_identity::SigningIdentity> {
        self.current.validate()?;
        let bytes =
            self.current
                .identity
                .file(&self.current.reference.record_name, true, MAX_BYTES)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != self.current.reference.record_sha256 {
            return Err(invalid());
        }
        let identity =
            runtime_identity::load_from_admitted_bytes(&bytes, &self.current.identity_directory)
                .map_err(|_| invalid())?;
        self.current.validate()?;
        Ok(identity)
    }
}

/// Root-only observation of a held target-owner directory. The authenticated
/// owner channel must supply this descriptor and its actual executing peer UID.
/// This type cannot export a current-device reference or modify owner custody.
#[cfg(target_os = "linux")]
pub struct RootObservedCurrentDevice {
    current: CurrentDeviceReference,
    owner: u32,
}
#[cfg(target_os = "linux")]
impl RootObservedCurrentDevice {
    /// Review current-owner custody through a supplied owner directory descriptor.
    ///
    /// # Errors
    /// Returns an error unless both real and effective UID are root, or if the
    /// descriptor is not a readable, private directory owned by `expected_owner`.
    /// Descriptor setup failures and current-owner review/revalidation failures
    /// also fail; see [`CurrentDeviceReference::review`].
    pub fn review(identity: std::fs::File, expected_owner: u32) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let system = fs::Directory::open_public_root(Path::new("/var/lib/tilo"))?;
        let identity = fs::Directory::observe_owner(identity, expected_owner)?;
        // No pathname is used by this closed observer. Only its held directory
        // and exact current record/selector/key/certificate/fleet are validated.
        let current = CurrentDeviceReference::from_directories(system, identity, PathBuf::new())?;
        let value = Self {
            current,
            owner: expected_owner,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn owner_uid(&self) -> u32 {
        self.owner
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.current.public_key()
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.current.device_public_key()
    }
    pub fn identity_record_name(&self) -> &str {
        self.current.identity_record_name()
    }
    /// Reattest root authority and the observed current-owner epoch.
    ///
    /// # Errors
    /// Returns an error unless both real and effective UID remain root, or if
    /// [`CurrentDeviceReference::validate`] fails for the held owner custody.
    pub fn validate(&self) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.current.validate()
    }
    /// Return public certification and revocations from the observed owner epoch.
    ///
    /// # Errors
    /// Returns an error if root authority or held current-owner custody cannot be
    /// revalidated before, during or after collecting the public material.
    pub fn public_native_material(
        &self,
    ) -> io::Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>)> {
        self.validate()?;
        let result = self.current.public_native_material()?;
        self.validate()?;
        Ok(result)
    }
}
