//! Authenticated transport of one already enrolled destination device.
//!
//! This is not a root-key backup or a new enrollment protocol. Only the exact
//! member device seed, certificate and public projections from isolated future
//! device custody travel. A privileged installer imports system custody before
//! target-user activation; callers retain the staged device until handoff.
use crate::device_fleet::DeviceFleet;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};
use tilo_seal::{Context, Label, PasswordParams, envelope};
use tilo_user_identity::{
    DeviceCert, verify_device_cert, verify_device_cert_at, verify_revocation_entry,
};
use zeroize::{Zeroize, Zeroizing};
mod current_device;
mod fs;
#[cfg(target_os = "linux")]
pub use current_device::RootObservedCurrentDevice;
pub use current_device::{
    AuthenticatedCurrentDeviceReference, CurrentDeviceDeclaration, CurrentDeviceReference,
    ResolvedCurrentIdentity,
};
#[cfg(test)]
mod tests;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const DOMAIN: &[u8] = b"tilo-prepared-enrolled-device/v1\0";

mod seed_serde {
    use serde::{Deserializer, Serializer};
    use zeroize::Zeroizing;
    pub(super) fn serialize<S: Serializer>(
        seed: &Zeroizing<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(seed.as_slice(), serializer)
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Zeroizing<Vec<u8>>, D::Error> {
        struct Seed;
        impl<'de> serde::de::Visitor<'de> for Seed {
            type Value = Zeroizing<Vec<u8>>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("one 32-byte device seed")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut seed = Zeroizing::new(Vec::with_capacity(32));
                while let Some(byte) = sequence.next_element::<u8>()? {
                    if seed.len() == 32 {
                        return Err(serde::de::Error::custom("device seed exceeds limit"));
                    }
                    seed.push(byte);
                }
                if seed.len() != 32 {
                    return Err(serde::de::Error::custom("device seed length invalid"));
                }
                Ok(seed)
            }
        }
        deserializer.deserialize_seq(Seed)
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "enrolled device custody is invalid",
    )
}
fn context(binding: &str) -> io::Result<Vec<u8>> {
    if binding.is_empty() || binding.len() > 256 || binding.chars().any(char::is_control) {
        return Err(invalid());
    }
    let mut context = DOMAIN.to_vec();
    context.extend_from_slice(binding.as_bytes());
    Ok(context)
}
fn password_valid(password: &[u8]) -> io::Result<()> {
    if password.len() > 4096
        || std::str::from_utf8(password).map_or(true, |value| value.chars().count() < 15)
    {
        return Err(invalid());
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema: u32,
    #[serde(with = "seed_serde")]
    device_seed: Zeroizing<Vec<u8>>,
    certificate: DeviceCert,
    public_record: Vec<u8>,
    fleet: DeviceFleet,
    home_peers: Vec<tilo_relay_protocol::HomePeerRegistration>,
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.device_seed.zeroize();
    }
}
impl Payload {
    fn validate(&self, root: [u8; 32], device: [u8; 32], now: u64) -> io::Result<()> {
        let seed = Zeroizing::new(
            <[u8; 32]>::try_from(self.device_seed.as_slice()).map_err(|_| invalid())?,
        );
        let signer = ed25519_dalek::SigningKey::from_bytes(&seed);
        let caps = &self.certificate.capabilities;
        if self.schema != 1
            || signer.verifying_key().to_bytes() != device
            || self.certificate.device_public_key != device
            || self.certificate.user_public_key != root
            || caps.can_admit_devices
            || caps.can_act_as_host
            || caps.can_read_all_workspaces
            || caps.foreign
            || caps.can_administer_devices
            || self.public_record.len() as u64 > MAX_BYTES
        {
            return Err(invalid());
        }
        verify_device_cert_at(&self.certificate, now).map_err(|_| invalid())?;
        validate_record(&self.public_record, root)?;
        validate_fleet(&self.fleet, &self.certificate)?;
        validate_peers(&self.home_peers, root)?;
        Ok(())
    }
}
fn validate_record(bytes: &[u8], root: [u8; 32]) -> io::Result<()> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let threshold = value
        .get("threshold_identity")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(invalid)?;
    if threshold.get("single_device") != Some(&serde_json::json!(false))
        || threshold
            .get("secret_bytes")
            .is_some_and(|secret| secret != &serde_json::json!([]))
        || value
            .get("secret_ref")
            .is_some_and(|value| !value.is_null())
    {
        return Err(invalid());
    }
    let identity = runtime_identity::load_from_bytes(bytes).map_err(|_| invalid())?;
    if identity.group_public_key() != root || identity.is_single_device() {
        return Err(invalid());
    }
    Ok(())
}
fn validate_fleet(fleet: &DeviceFleet, cert: &DeviceCert) -> io::Result<()> {
    if fleet.schema_version != 1
        || fleet.identity_id != hex::encode(cert.user_public_key)
        || fleet.certificates.len() > 256
        || fleet.revocations.len() > 256
        || fleet.is_revoked(&cert.device_public_key)
    {
        return Err(invalid());
    }
    let expected = tilo_user_identity::canonical::canonical_bytes(cert).map_err(|_| invalid())?;
    let mut found = false;
    for known in &fleet.certificates {
        verify_device_cert(known).map_err(|_| invalid())?;
        if known.user_public_key != cert.user_public_key {
            return Err(invalid());
        }
        if known.device_public_key == cert.device_public_key {
            if tilo_user_identity::canonical::canonical_bytes(known).map_err(|_| invalid())?
                != expected
            {
                return Err(invalid());
            }
            found = true;
        }
    }
    for revocation in &fleet.revocations {
        verify_revocation_entry(revocation).map_err(|_| invalid())?;
        if revocation.user_public_key != cert.user_public_key {
            return Err(invalid());
        }
    }
    if !found {
        return Err(invalid());
    }
    Ok(())
}
fn validate_peers(
    peers: &[tilo_relay_protocol::HomePeerRegistration],
    root: [u8; 32],
) -> io::Result<()> {
    if peers.len() > 128 {
        return Err(invalid());
    }
    for peer in peers {
        let cert = peer.device_cert.as_ref().ok_or_else(invalid)?;
        verify_device_cert(cert).map_err(|_| invalid())?;
        if peer.schema_version != tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA
            || cert.user_public_key != root
            || peer.user_public_key != hex::encode(root)
            || peer.device_node_id != hex::encode(cert.device_public_key)
            || !cert.capabilities.can_act_as_host
            || peer.relay_workspace_ids.len() > 4096
            || hex::decode(&peer.relay_service_node_id).map_or(true, |bytes| bytes.len() != 32)
            || peer.label.len() > 256
            || peer.label.chars().any(char::is_control)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

struct BoundedPlain<'a>(&'a mut Vec<u8>);
impl io::Write for BoundedPlain<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length as u64 > MAX_BYTES)
        {
            return Err(invalid());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Non-secret proof identifying the one device carried by an authenticated artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportReceipt {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub sealed_bytes: u64,
}
/// Retained, immutable source material from a canonical personal-device approval.
/// This object contains private seed custody and intentionally has no serializer.
pub struct EnrolledDevice {
    system: fs::Directory,
    system_directory: PathBuf,
    identity: fs::Directory,
    record_name: String,
    identity_directory: PathBuf,
    payload: Payload,
}
/// Canonical device custody permits the certified service group's 0640 key;
/// the source root is separately held private. Its reader rejects every symlink,
/// foreign key owner, extra hard link and insecure mode without changing files.
fn read_source_device_seed(
    directory: &fs::Directory,
    path: &Path,
) -> io::Result<Zeroizing<Vec<u8>>> {
    directory.validate()?;
    let private = directory.child("device-private", false)?;
    private.validate()?;
    let key =
        tilo_user_identity::persist::read_device_key(&path.join("device-private/keypair.bin"))
            .map_err(|_| invalid())?;
    let seed = Zeroizing::new(key.to_bytes());
    private.validate()?;
    directory.validate()?;
    Ok(Zeroizing::new(seed.to_vec()))
}

impl EnrolledDevice {
    /// Review the exact already enrolled device in isolated owner-private source custody.
    ///
    /// # Errors
    /// Refuses redirected/unavailable custody, a human root secret, mismatched keys,
    /// invalid certification, revoked membership or invalid public projections.
    pub fn review(
        system_root: &Path,
        identity_record: &Path,
        expected_root: [u8; 32],
        expected_device: [u8; 32],
        now_ns: u64,
    ) -> io::Result<Self> {
        tilo_user_identity::persist::protect_private_custody()?;
        let system = fs::Directory::open(system_root)?;
        let identity = fs::Directory::open(identity_record.parent().ok_or_else(invalid)?)?;
        let record_name = identity_record
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?
            .to_owned();
        let user = system.child("user-identity", false)?;
        if user.optional("privkey.bin", true, 32)?.is_some() {
            return Err(invalid());
        }
        let root = user.file("pubkey.bin", false, 32)?;
        if root.as_slice() != expected_root {
            return Err(invalid());
        }
        let seed = read_source_device_seed(&system, system_root)?;
        let payload = Payload {
            schema: 1,
            device_seed: seed,
            certificate: tilo_user_identity::canonical::from_canonical_bytes(
                &system
                    .child("device", false)?
                    .file("cert.bin", false, 65_536)?,
            )
            .map_err(|_| invalid())?,
            public_record: identity.file(&record_name, true, MAX_BYTES)?.to_vec(),
            fleet: serde_json::from_slice(&identity.child("fleets", false)?.file(
                &format!("{}.json", hex::encode(expected_root)),
                true,
                MAX_BYTES,
            )?)
            .map_err(|_| invalid())?,
            home_peers: match system.child("peers", false) {
                Ok(peers) => match peers.optional("home-peers.json", false, MAX_BYTES)? {
                    Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| invalid())?,
                    None => Vec::new(),
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error),
            },
        };
        payload.validate(expected_root, expected_device, now_ns)?;
        system.validate()?;
        identity.validate()?;
        Ok(Self {
            system,
            system_directory: system_root.to_owned(),
            identity,
            record_name,
            identity_directory: identity_record.parent().ok_or_else(invalid)?.to_owned(),
            payload,
        })
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.payload.certificate.user_public_key
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.payload.certificate.device_public_key
    }
    /// Public native authority from this already password-authenticated transport.
    /// This does not expose device seeds or permit caller-authored fleet claims.
    pub fn public_native_material(
        &self,
    ) -> io::Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>)> {
        self.payload.validate(
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )?;
        Ok((
            self.payload.certificate.clone(),
            self.payload.fleet.revocations.clone(),
        ))
    }
    pub fn device_label(&self) -> &str {
        &self.payload.certificate.device_label
    }
    /// Read the reviewed canonical public presentation.
    ///
    /// # Errors
    /// Refuses changed source custody or invalid canonical public metadata.
    pub fn metadata(&self) -> io::Result<runtime_identity::IdentityMetadata> {
        self.validate_source()?;
        runtime_identity::load_metadata_from_bytes(&self.payload.public_record)
            .map_err(|_| invalid())
    }
    pub fn identity_record_name(&self) -> &str {
        &self.record_name
    }
    pub fn identity_directory(&self) -> &Path {
        &self.identity_directory
    }
    /// Reattest the selected source and current device certification.
    ///
    /// # Errors
    /// Refuses changed source authority, expired certification or revoked membership.
    pub fn validate(&self) -> io::Result<()> {
        self.validate_source()?;
        self.payload.validate(
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )
    }
    fn validate_source(&self) -> io::Result<()> {
        self.system.validate()?;
        self.identity.validate()?;
        let user = self.system.child("user-identity", false)?;
        if user.optional("privkey.bin", true, 32)?.is_some()
            || user.file("pubkey.bin", false, 32)?.as_slice() != self.public_key()
            || read_source_device_seed(&self.system, &self.system_directory)?.as_slice()
                != self.payload.device_seed.as_slice()
            || self
                .identity
                .file(&self.record_name, true, MAX_BYTES)?
                .as_slice()
                != self.payload.public_record
        {
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
            != tilo_user_identity::canonical::canonical_bytes(&self.payload.certificate)
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
        Ok(())
    }
    /// Seal the exact selected device under its setup binding and publish exclusively.
    ///
    /// # Errors
    /// Refuses invalid passwords/bindings, changed source custody, excessive payloads,
    /// overlapping destinations, existing output or failed authenticated readback.
    pub fn export_password_sealed(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<TransportReceipt> {
        password_valid(password)?;
        self.validate()?;
        let context = context(binding)?;
        let mut plain = Zeroizing::new(Vec::new());
        serde_json::to_writer(BoundedPlain(&mut plain), &self.payload).map_err(|_| invalid())?;
        if plain.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
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
        parent.publish(name, &sealed)?;
        let readback = parent.file(name, true, MAX_BYTES + 256)?;
        if readback.as_slice() != sealed {
            return Err(invalid());
        }
        AuthenticatedEnrolledDevice::from_bytes(
            &readback,
            password,
            binding,
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )?;
        Ok(TransportReceipt {
            user_public_key: self.public_key(),
            device_public_key: self.device_public_key(),
            sealed_bytes: sealed.len() as u64,
        })
    }
}

/// Password-authenticated, exact root/device-bound custody ready for privileged import.
pub struct AuthenticatedEnrolledDevice {
    payload: Payload,
}
impl AuthenticatedEnrolledDevice {
    /// Public native authority from this already password-authenticated transport.
    /// This does not expose device seeds or permit caller-authored fleet claims.
    pub fn public_native_material(
        &self,
    ) -> io::Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>)> {
        self.payload.validate(
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )?;
        Ok((
            self.payload.certificate.clone(),
            self.payload.fleet.revocations.clone(),
        ))
    }
    pub fn device_label(&self) -> &str {
        &self.payload.certificate.device_label
    }
    /// Authenticate bounded transport for one exact expected person and device.
    ///
    /// # Errors
    /// Refuses unavailable private ciphertext, wrong password/binding/authority,
    /// malformed payloads, expired certification or revoked membership.
    pub fn open(
        artifact: &Path,
        password: &[u8],
        binding: &str,
        expected_root: [u8; 32],
        expected_device: [u8; 32],
        now_ns: u64,
    ) -> io::Result<Self> {
        let parent = fs::Directory::open(artifact.parent().ok_or_else(invalid)?)?;
        let name = artifact
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        let bytes = parent.file(name, true, MAX_BYTES + 256)?;
        Self::from_bytes(
            &bytes,
            password,
            binding,
            expected_root,
            expected_device,
            now_ns,
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
        tilo_user_identity::persist::protect_private_custody()?;
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
        let payload: Payload = serde_json::from_slice(&plain).map_err(|_| invalid())?;
        payload.validate(root, device, now)?;
        Ok(Self { payload })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedEnrolledDevice {
    pub identity_record: PathBuf,
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}
struct TargetFile {
    system: bool,
    directories: Vec<String>,
    name: String,
    bytes: Zeroizing<Vec<u8>>,
}
impl TargetFile {
    fn directory(
        &self,
        system: &fs::Directory,
        identity: &fs::Directory,
        create: bool,
    ) -> io::Result<Option<fs::Directory>> {
        let root = if self.system { system } else { identity };
        let mut path: Option<fs::Directory> = None;
        for name in &self.directories {
            let parent = path.as_ref().unwrap_or(root);
            match parent.child(name, create) {
                Ok(child) => path = Some(child),
                Err(error) if !create && error.kind() == io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
        match path {
            Some(path) => Ok(Some(path)),
            None => Err(invalid()),
        }
    }
    fn check(&self, system: &fs::Directory, identity: &fs::Directory) -> io::Result<()> {
        let existing = if self.directories.is_empty() {
            (if self.system { system } else { identity }).optional(&self.name, false, MAX_BYTES)?
        } else {
            match self.directory(system, identity, false)? {
                Some(directory) => directory.optional(&self.name, false, MAX_BYTES)?,
                None => None,
            }
        };
        if existing
            .as_ref()
            .is_some_and(|bytes| bytes.as_slice() != self.bytes.as_slice())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination identity custody conflicts",
            ));
        }
        Ok(())
    }
    fn publish(&self, system: &fs::Directory, identity: &fs::Directory) -> io::Result<()> {
        let directory = if self.directories.is_empty() {
            None
        } else {
            self.directory(system, identity, true)?
        };
        let parent = directory
            .as_ref()
            .unwrap_or(if self.system { system } else { identity });
        if parent.optional(&self.name, false, MAX_BYTES)?.is_some() {
            return self.check(system, identity);
        }
        parent.publish(&self.name, &self.bytes)?;
        if parent.file(&self.name, false, MAX_BYTES)?.as_slice() != self.bytes.as_slice() {
            return Err(invalid());
        }
        Ok(())
    }
}
impl AuthenticatedEnrolledDevice {
    pub fn public_key(&self) -> [u8; 32] {
        self.payload.certificate.user_public_key
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.payload.certificate.device_public_key
    }
    fn target_files(&self) -> io::Result<Vec<TargetFile>> {
        let root = hex::encode(self.public_key());
        let cert = tilo_user_identity::canonical::canonical_bytes(&self.payload.certificate)
            .map_err(|_| invalid())?;
        let make = |system, directories: Vec<String>, name: &str, bytes: Vec<u8>| TargetFile {
            system,
            directories,
            name: name.into(),
            bytes: Zeroizing::new(bytes),
        };
        Ok(vec![
            make(
                true,
                vec!["user-identity".into()],
                "pubkey.bin",
                self.public_key().to_vec(),
            ),
            make(
                true,
                vec!["device-private".into()],
                "keypair.bin",
                self.payload.device_seed.to_vec(),
            ),
            make(
                true,
                vec!["device".into(), "certs".into()],
                &format!("{root}.bin"),
                cert.clone(),
            ),
            make(
                false,
                vec![],
                "primary.json",
                self.payload.public_record.clone(),
            ),
            make(
                false,
                vec!["fleets".into()],
                &format!("{root}.json"),
                serde_json::to_vec(&self.payload.fleet).map_err(|_| invalid())?,
            ),
            make(
                true,
                vec!["peers".into()],
                "home-peers.json",
                serde_json::to_vec(&self.payload.home_peers).map_err(|_| invalid())?,
            ),
            // Publish default certification last, after all projection/key custody.
            make(true, vec!["device".into()], "cert.bin", cert),
        ])
    }
    /// Publish exact device custody under explicitly authorized private roots.
    /// System-root publication is a privileged installer operation, never a
    /// target-user privilege escalation. All existing files must match before
    /// any write; exact partial retries resume without replacing other custody.
    ///
    /// # Errors
    /// Refuses unauthorized or overlapping roots, conflicting existing files,
    /// concurrent custody operations or incomplete durable publication.
    pub fn import_into(
        &self,
        system_root: &Path,
        identity_dir: &Path,
    ) -> io::Result<ImportedEnrolledDevice> {
        self.payload.validate(
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )?;
        let system = fs::Directory::open_namespace(system_root)?;
        let identity = fs::Directory::open(identity_dir)?;
        if system.overlaps(&identity) {
            return Err(invalid());
        }
        refuse_human_root(&system)?;
        let files = self.target_files()?;
        for file in &files {
            file.check(&system, &identity)?;
        }
        let _system_lock = system.lock(".enrolled-device.lock")?;
        let _identity_lock = identity.lock(".enrolled-device.lock")?;
        for file in &files {
            file.check(&system, &identity)?;
        }
        for file in &files {
            file.publish(&system, &identity)?;
        }
        system.validate()?;
        identity.validate()?;
        let paths = tilo_user_identity::IdentityPaths::under(system_root);
        if !tilo_user_identity::persist::joined_device_is_ready(
            &paths,
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        Ok(ImportedEnrolledDevice {
            identity_record: identity_dir.join("primary.json"),
            user_public_key: self.public_key(),
            device_public_key: self.device_public_key(),
        })
    }
    /// Verify public member identity against already installed machine custody.
    /// This read-only target-user operation neither imports a machine key nor
    /// generates one. Privileged import must have completed beforehand.
    ///
    /// # Errors
    /// Refuses unavailable target custody or any missing/mismatched installed file.
    pub fn verify_installed(
        &self,
        system_root: &Path,
        identity_dir: &Path,
    ) -> io::Result<ImportedEnrolledDevice> {
        let system = fs::Directory::open(system_root)?;
        let identity = fs::Directory::open(identity_dir)?;
        for file in self.target_files()? {
            let directory = if file.directories.is_empty() {
                None
            } else {
                file.directory(&system, &identity, false)?
                    .ok_or_else(invalid)
                    .map(Some)?
            };
            let parent =
                directory
                    .as_ref()
                    .unwrap_or(if file.system { &system } else { &identity });
            if parent.file(&file.name, true, MAX_BYTES)?.as_slice() != file.bytes.as_slice() {
                return Err(invalid());
            }
        }
        Ok(ImportedEnrolledDevice {
            identity_record: identity_dir.join("primary.json"),
            user_public_key: self.public_key(),
            device_public_key: self.device_public_key(),
        })
    }
}
fn refuse_human_root(system: &fs::Directory) -> io::Result<()> {
    match system.child("user-identity", false) {
        Ok(user) => {
            if user.optional("privkey.bin", true, 32)?.is_some() {
                return Err(invalid());
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn normalize_public_projection(system: &fs::Directory, root: [u8; 32]) -> io::Result<()> {
    let user = system.child("user-identity", false)?;
    user.normalize_public_file("pubkey.bin")?;
    user.normalize_public_directory()?;
    let device = system.child("device", false)?;
    let certs = device.child("certs", false)?;
    certs.normalize_public_file(&format!("{}.bin", hex::encode(root)))?;
    certs.normalize_public_directory()?;
    device.normalize_public_file("cert.bin")?;
    device.normalize_public_directory()?;
    let peers = system.child("peers", false)?;
    peers.normalize_public_file("home-peers.json")?;
    peers.normalize_public_directory()?;
    system.normalize_public_directory()
}

impl AuthenticatedEnrolledDevice {
    /// Privileged machine publication. No personal identity or human root key
    /// is imported. The existing OS firstboot permission service publishes the
    /// public projection and grants its broker group device-key read access.
    ///
    /// # Errors
    /// Refuses non-root authority, insecure namespaces, conflicting machine custody,
    /// concurrent publication or failed durability and permission normalization.
    pub fn import_system(&self, system_root: &Path) -> io::Result<()> {
        self.payload.validate(
            self.public_key(),
            self.device_public_key(),
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )?;
        let system = fs::Directory::open_system_namespace(system_root)?;
        refuse_human_root(&system)?;
        let files: Vec<_> = self
            .target_files()?
            .into_iter()
            .filter(|file| file.system)
            .collect();
        for file in &files {
            file.check(&system, &system)?;
        }
        let _lock = system.lock(".enrolled-device.lock")?;
        for file in &files {
            file.check(&system, &system)?;
        }
        for file in &files {
            file.publish(&system, &system)?;
        }
        normalize_public_projection(&system, self.public_key())?;
        for file in &files {
            file.check(&system, &system)?;
        }
        system.validate()?;
        let paths = tilo_user_identity::IdentityPaths::under(system_root);
        if !tilo_user_identity::persist::joined_device_is_ready(
            &paths,
            tilo_clock::try_unix_now_ns().ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn verify_public_system(&self, system_root: &Path) -> io::Result<()> {
        let system = fs::Directory::open_public_root(system_root)?;
        let user = system.child("user-identity", false)?;
        if user.file("pubkey.bin", false, 32)?.as_slice() != self.public_key() {
            return Err(invalid());
        }
        let bytes = system
            .child("device", false)?
            .file("cert.bin", false, 65_536)?;
        let cert: DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&bytes).map_err(|_| invalid())?;
        verify_device_cert_at(&cert, tilo_clock::try_unix_now_ns().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        if bytes.as_slice()
            != tilo_user_identity::canonical::canonical_bytes(&self.payload.certificate)
                .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        system.validate()
    }
    /// Import public member custody only after the privileged publisher has
    /// installed the exact public machine certificate. No machine-key read or
    /// write occurs here; the canonical broker proves device possession later.
    ///
    /// # Errors
    /// Refuses mismatched root-owned public machine certification, unavailable
    /// private persona custody, conflicting records or concurrent publication.
    pub fn import_public_identity(
        &self,
        system_root: &Path,
        identity_dir: &Path,
    ) -> io::Result<PathBuf> {
        self.verify_public_system(system_root)?;
        let identity = fs::Directory::open(identity_dir)?;
        let files: Vec<_> = self
            .target_files()?
            .into_iter()
            .filter(|file| !file.system)
            .collect();
        for file in &files {
            file.check(&identity, &identity)?;
        }
        let _lock = identity.lock(".enrolled-device.lock")?;
        for file in &files {
            file.check(&identity, &identity)?;
        }
        self.verify_public_system(system_root)?;
        for file in &files {
            file.publish(&identity, &identity)?;
        }
        identity.validate()?;
        Ok(identity_dir.join("primary.json"))
    }
}
