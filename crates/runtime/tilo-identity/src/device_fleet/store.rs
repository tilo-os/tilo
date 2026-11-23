//! The device fleet's files: one JSON document per identity, its certificates and revocations,
//! kept in the identity directory and shared with the home peers the LAN broker enrolls.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

use tilo_files::native::FileStamp;

use tilo_device_identity::IdentityPaths;
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason,
    sign_device_cert, sign_revocation_entry, verify_device_cert, verify_device_cert_at,
    verify_revocation_entry,
};

use super::{DeviceFleet, FLEET_SCHEMA_VERSION};

#[derive(Clone, Debug)]
pub struct DeviceFleetStore {
    identity_dir: PathBuf,
    identity_paths: IdentityPaths,
    person: Option<u32>,
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
        let registry = crate::store::IdentityRegistryStore::from_env()?;
        Ok(Self::at(
            registry.identity_dir(),
            crate::provisioning::collaboration_identity_paths(),
        ))
    }

    pub fn at(identity_dir: impl Into<PathBuf>, identity_paths: IdentityPaths) -> Self {
        Self {
            identity_dir: identity_dir.into(),
            identity_paths,
            person: None,
        }
    }

    /// The store this device keeps `identity_id`'s fleet in: its account's
    /// identity directory when that holds one, otherwise the identity root
    /// that holds this device's key and certificate. A server has no identity
    /// directory of its own, and the ticket it logged in with wrote the
    /// fleet into its identity root, so both sides find it there.
    #[must_use]
    pub fn of_device(identity_paths: IdentityPaths, identity_id: &str) -> Self {
        if let Ok(store) = Self::from_env()
            && store.fleet_file(identity_id).exists()
        {
            return store;
        }
        let root = identity_paths
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .map_or_else(PathBuf::new, Path::to_path_buf);
        Self::at(root, identity_paths)
    }

    /// The store of another account's identity directory, written by a root
    /// process acting for that account (uid): every write runs as them and
    /// only into a directory they own ([`crate::person_files::as_person`]).
    #[must_use]
    pub const fn for_person(mut self, uid: u32) -> Self {
        self.person = Some(uid);
        self
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
        #[cfg(test)]
        LOADS.with(|loads| loads.set(loads.get().saturating_add(1)));
        let path = self.path(identity_id);
        let fleet: DeviceFleet = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        } else {
            DeviceFleet::new(identity_id.to_string())
        };
        if fleet.schema_version != FLEET_SCHEMA_VERSION || fleet.identity_id != identity_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "device fleet identity/schema mismatch",
            ));
        }
        fleet.check(identity_id)?;
        Ok(fleet)
    }

    /// [`Self::load`] for authorization checks that run on every remote
    /// action and broker call. The verified fleet is served from memory while
    /// its file did not change: one `stat` instead of reading, parsing and verifying
    /// them. Every writer replaces the fleet file by rename and `Self::save`
    /// drops the entry, and a file another process rewrites in place changes
    /// its length or modification time, so a revocation is seen by the very
    /// next call. A fleet whose files changed within the last two seconds is
    /// loaded again on each call: a coarse filesystem clock can give a write
    /// that lands right after the load the same time. Failures are never
    /// cached.
    ///
    /// Writers keep using [`Self::load`] under the fleet lock.
    ///
    /// # Errors
    ///
    /// As [`Self::load`].
    pub fn load_cached(&self, identity_id: &str) -> io::Result<Arc<DeviceFleet>> {
        let key = self.cache_key(identity_id);
        let before = key.stamps();
        if let Some(stamps) = before
            && let Ok(cache) = FLEET_CACHE.read()
            && let Some(entry) = cache.get(&key)
            && entry.stamps == stamps
        {
            return Ok(Arc::clone(&entry.fleet));
        }
        let fleet = Arc::new(self.load(identity_id)?);
        if let Some(stamps) = before
            && stamps.settled()
            && key.stamps() == Some(stamps)
            && let Ok(mut cache) = FLEET_CACHE.write()
        {
            if cache.len() >= FLEET_CACHE_LIMIT && !cache.contains_key(&key) {
                cache.clear();
            }
            cache.insert(
                key,
                CachedFleet {
                    stamps,
                    fleet: Arc::clone(&fleet),
                },
            );
        }
        Ok(fleet)
    }

    /// Ensure the current machine appears in the fleet for `identity_id`.
    pub fn register_current_device(&self, identity_id: &str) -> io::Result<DeviceFleet> {
        let (_, profile) = self.identity(identity_id)?;
        let cert_path = self
            .identity_paths
            .device_cert_for_user(&profile.user_public_key);
        let cert = tilo_device_identity::persist::read_device_cert(&cert_path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.insert_certificate(identity_id, cert)
    }

    /// Root-certify `device_public_key` as a device of `identity_id`, named
    /// `label`, with `capabilities`, valid from `issued_at_ns`, and keep it
    /// with the person's fleet.
    ///
    /// This device must hold the identity's root; a device added from another
    /// computer goes through a ticket instead, where the approving device's
    /// root signs the certificate.
    ///
    /// # Errors
    ///
    /// This device does not hold the identity's root, the certificate is one
    /// a verifier does not take, or the fleet cannot be written.
    pub fn certify_device(
        &self,
        identity_id: &str,
        device_public_key: [u8; 32],
        label: &str,
        capabilities: DeviceCapabilities,
        issued_at_ns: u64,
    ) -> io::Result<DeviceCert> {
        self.certify_with_root(
            identity_id,
            device_public_key,
            label,
            capabilities,
            issued_at_ns,
            None,
        )
    }

    /// Root-sign a permanent revocation of an enrolled device and keep it
    /// with the fleet.
    pub fn revoke(
        &self,
        identity_id: &str,
        device_public_key: [u8; 32],
        reason: RevocationReason,
        revoked_at_ns: u64,
    ) -> io::Result<RevocationEntry> {
        let (root, profile) = self.identity(identity_id)?;
        if !self
            .load(identity_id)?
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
        sign_revocation_entry(&mut entry, &root)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.record_revocation(identity_id, &entry)?;
        Ok(entry)
    }

    /// The fleet of `identity_id` as a verifier sees it: every certificate
    /// and revocation it holds, each checked again.
    ///
    /// # Errors
    ///
    /// As [`Self::load`], or when a stored record is not the identity's.
    pub fn certified_fleet(
        &self,
        identity_id: &str,
        user_public_key: [u8; 32],
    ) -> io::Result<tilo_user_identity::CertifiedFleet> {
        let fleet = self.load(identity_id)?;
        let mut certified = tilo_user_identity::CertifiedFleet::new(user_public_key);
        for cert in fleet.certificates {
            if cert.user_public_key == user_public_key {
                certified
                    .certify(cert)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            }
        }
        for entry in fleet.revocations {
            if entry.user_public_key == user_public_key {
                certified
                    .revoke(entry)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            }
        }
        Ok(certified)
    }

    /// Sign `digest` with `identity_id`'s root. The caller builds the record
    /// whose digest this is; identity-host learns no policy of its own from
    /// it, and a device that does not hold the root cannot sign.
    ///
    /// # Errors
    ///
    /// This device does not hold the identity's root, or signing fails.
    pub fn sign_root(&self, identity_id: &str, digest: &[u8; 32]) -> io::Result<[u8; 64]> {
        let (root, _profile) = self.identity(identity_id)?;
        root.sign(digest)
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// Root-certify `agent_public_key` as an agent's grant reaching `scope`, named `label`, from
    /// `issued_at_ns` until `expires_at_ns`, and keep it with `identity_id`'s fleet: a key the
    /// person gave an agent when they started a session for it. It is listed with their devices
    /// and ends as one does, by a revocation their root signs ([`Self::revoke`]).
    ///
    /// # Errors
    ///
    /// This device does not hold the identity's root, the grant is one no verifier takes (it
    /// outlives an agent's grant), or the fleet cannot be written.
    pub fn certify_agent_grant(
        &self,
        identity_id: &str,
        agent_public_key: [u8; 32],
        scope: AgentScope,
        label: &str,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> io::Result<DeviceCert> {
        self.certify_with_root(
            identity_id,
            agent_public_key,
            label,
            DeviceCapabilities::agent_grant(scope),
            issued_at_ns,
            Some(expires_at_ns),
        )
    }

    /// The one place this store's root certifies a key and keeps the
    /// certificate with the person's fleet.
    fn certify_with_root(
        &self,
        identity_id: &str,
        device_public_key: [u8; 32],
        label: &str,
        capabilities: DeviceCapabilities,
        issued_at_ns: u64,
        expires_at_ns: Option<u64>,
    ) -> io::Result<DeviceCert> {
        let (root, profile) = self.identity(identity_id)?;
        let mut cert = DeviceCert {
            user_public_key: profile.user_public_key,
            device_public_key,
            device_label: label.to_owned(),
            capabilities,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).map_err(|error| io::Error::other(error.to_string()))?;
        verify_device_cert_at(&cert, issued_at_ns)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.insert_certificate(identity_id, cert.clone())?;
        Ok(cert)
    }

    /// The person's secret-sync key, when this device holds the root of
    /// `identity_id`.
    pub fn secret_sync_key(&self, identity_id: &str) -> io::Result<tilo_seal::SecretKey> {
        let (root, _) = self.identity(identity_id)?;
        root.secret_sync_key()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// Remember a certificate this machine did not issue locally.
    ///
    /// The setup path uses this so an existing machine accepts a device the
    /// imager certified, and so the new machine accepts the devices it should
    /// join. The certificate must already verify and belong to `identity_id`.
    pub fn remember_certificate(
        &self,
        identity_id: &str,
        cert: DeviceCert,
    ) -> io::Result<DeviceFleet> {
        self.insert_certificate(identity_id, cert)
    }

    /// The fleet of `identity_id` as an approving device hands it to a device
    /// it admits, with `cert` in it: what [`Self::accept_transferred`] takes
    /// on the joining device. Nothing is written.
    ///
    /// # Errors
    ///
    /// As [`Self::load`], when `cert` is not `identity_id`'s or is not valid
    /// now.
    pub fn transferred(&self, identity_id: &str, cert: &DeviceCert) -> io::Result<Vec<u8>> {
        let now_ns = tilo_clock::try_unix_now_ns()
            .ok_or_else(|| io::Error::other("the clock is before the unix epoch"))?;
        verify_device_cert_at(cert, now_ns).map_err(|error| io::Error::other(error.to_string()))?;
        if cert.user_public_key != decode_hex_32(identity_id)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device certificate belongs to another identity",
            ));
        }
        let mut fleet = self.load(identity_id)?;
        fleet
            .certificates
            .retain(|known| known.device_public_key != cert.device_public_key);
        fleet.certificates.push(cert.clone());
        serde_json::to_vec_pretty(&fleet)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// The fleet document `bytes` another device handed this one when it
    /// joined, checked as a fleet of `identity_id`: within its bounds, every
    /// certificate and revocation in it verified, and `enrolled` among its
    /// certificates.
    ///
    /// # Errors
    ///
    /// Returns an error when the document is larger than a fleet may be,
    /// is no fleet of `identity_id`, does not verify, or does not list the
    /// enrolling device.
    pub fn accept_transferred(
        &self,
        identity_id: &str,
        bytes: &[u8],
        enrolled: [u8; 32],
    ) -> io::Result<DeviceFleet> {
        const MAX_FLEET_BYTES: usize = 1024 * 1024;
        const MAX_RECORDS: usize = 256;
        if bytes.len() > MAX_FLEET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "transferred device fleet exceeds 1 MiB",
            ));
        }
        let fleet: DeviceFleet = serde_json::from_slice(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fleet.check(identity_id)?;
        if fleet.certificates.len() > MAX_RECORDS
            || fleet.revocations.len() > MAX_RECORDS
            || !fleet
                .certificates
                .iter()
                .any(|cert| cert.device_public_key == enrolled)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "transferred device fleet violates its bounded schema",
            ));
        }
        Ok(fleet)
    }

    /// Install `fleet` as this device's record of `fleet.identity_id`'s
    /// devices: the fleet another device handed it when it joined. Written
    /// the way every fleet write is — as the person, under the fleet lock —
    /// so a device that joined reads the fleet it was given.
    pub fn adopt_transferred(&self, fleet: &DeviceFleet) -> io::Result<()> {
        fleet.check(&fleet.identity_id)?;
        let _fleet_lock = self.lock_fleets()?;
        self.save(fleet)
    }

    /// Record a root-signed revocation another host of `identity_id` withdrew
    /// a device with: kept once, and refused from then on.
    ///
    /// # Errors
    ///
    /// The entry is not a revocation a verifier takes or is another
    /// identity's, or the fleet cannot be written.
    pub fn record_revocation(
        &self,
        identity_id: &str,
        entry: &RevocationEntry,
    ) -> io::Result<DeviceFleet> {
        verify_revocation_entry(entry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if hex::encode(entry.user_public_key) != identity_id {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "revocation belongs to another identity",
            ));
        }
        let _fleet_lock = self.lock_fleets()?;
        let mut fleet = self.load(identity_id)?;
        if fleet.is_revoked(&entry.revoked_device_public_key) {
            return Ok(fleet);
        }
        fleet.revocations.push(entry.clone());
        self.save(&fleet)?;
        Ok(fleet)
    }

    /// The file this store keeps `identity_id`'s fleet in.
    #[must_use]
    pub fn fleet_file(&self, identity_id: &str) -> PathBuf {
        self.path(identity_id)
    }

    fn insert_certificate(&self, identity_id: &str, cert: DeviceCert) -> io::Result<DeviceFleet> {
        verify_device_cert(&cert).map_err(|error| io::Error::other(error.to_string()))?;
        let _fleet_lock = self.lock_fleets()?;
        let mut fleet = self.load(identity_id)?;
        if cert.user_public_key != decode_hex_32(identity_id)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device certificate belongs to another identity",
            ));
        }
        // Every login registers this device again. When the fleet already
        // holds exactly this certificate there is nothing to write, and a
        // login on a full disk must not fail on a rewrite that changes nothing.
        if fleet.certificates.contains(&cert) {
            return Ok(fleet);
        }
        fleet
            .certificates
            .retain(|existing| existing.device_public_key != cert.device_public_key);
        fleet.certificates.push(cert);
        self.save(&fleet)?;
        Ok(fleet)
    }

    /// This device's material of `identity_id`'s key.
    pub fn identity_key(&self, identity_id: &str) -> io::Result<crate::store::SigningIdentity> {
        self.identity(identity_id).map(|(key, _)| key)
    }

    pub(crate) fn identity(
        &self,
        identity_id: &str,
    ) -> io::Result<(crate::store::SigningIdentity, crate::store::IdentityProfile)> {
        let store = crate::store::IdentityRegistryStore::at(&self.identity_dir);
        let registry = store.load_reconciled()?;
        let profile = registry
            .identities
            .get(identity_id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "identity is not registered"))?;
        let identity = crate::store::load_from_path(profile.record_path.clone())
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
        let written = crate::person_files::as_person(self.person, &self.identity_dir, || {
            tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        });
        if let Ok(mut cache) = FLEET_CACHE.write() {
            cache.remove(&self.cache_key(&fleet.identity_id));
        }
        // A certificate added or a revocation changes who may act: re-check
        // now, not when the identity directory's watch reports it.
        crate::policy_change::announce();
        written
    }

    /// Fleet writes run as `self.person` when one is set, so a root process
    /// acting for a person writes into a directory they own
    /// ([`Self::for_person`]).
    fn lock_fleets(&self) -> io::Result<advisory_lock::FileLock> {
        let directory = self.identity_dir.join("fleets");
        crate::person_files::as_person(self.person, &self.identity_dir, || {
            fs::create_dir_all(&directory)?;
            File::open(&directory)
        })
        .and_then(advisory_lock::FileLock::exclusive)
    }

    fn path(&self, identity_id: &str) -> PathBuf {
        self.identity_dir
            .join("fleets")
            .join(format!("{identity_id}.json"))
    }

    /// Stamps of the files [`Self::load_cached`] reads for `identity_id`, or
    /// `None` when one can't be examined: what a reader that derives something
    /// from the fleet — the person's record keeps the keys it certifies —
    /// compares from pass to pass, so a fleet that moved is noticed without
    /// reading it.
    pub fn stamps(&self, identity_id: &str) -> Option<FleetStamps> {
        self.cache_key(identity_id).stamps()
    }

    fn cache_key(&self, identity_id: &str) -> FleetKey {
        FleetKey {
            fleet: self.path(identity_id),
        }
    }
}

/// Fleets [`DeviceFleetStore::load_cached`] verified, by their backing files.
static FLEET_CACHE: LazyLock<RwLock<HashMap<FleetKey, CachedFleet>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// A process serves a handful of identities; the bound only stops a caller
/// that opens stores at ever new paths from growing the cache without limit.
const FLEET_CACHE_LIMIT: usize = 64;

#[cfg(test)]
thread_local! {
    /// [`DeviceFleetStore::load`] calls made on this thread.
    static LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread ran [`DeviceFleetStore::load`].
#[cfg(test)]
pub(crate) fn loads_on_this_thread() -> usize {
    LOADS.with(std::cell::Cell::get)
}

/// The file one fleet is loaded from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FleetKey {
    fleet: PathBuf,
}

impl FleetKey {
    /// The file's stamp, or `None` when it can't be examined (the caller
    /// then loads from disk and caches nothing).
    fn stamps(&self) -> Option<FleetStamps> {
        Some(FleetStamps {
            fleet: FileStamp::of(&self.fleet).ok()?,
        })
    }
}

struct CachedFleet {
    stamps: FleetStamps,
    fleet: Arc<DeviceFleet>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FleetStamps {
    fleet: Option<FileStamp>,
}

impl FleetStamps {
    pub(crate) fn settled(self) -> bool {
        self.fleet.is_none_or(FileStamp::settled)
    }

    /// When its file settles ([`FileStamp::settles_at`]).
    pub(crate) fn settles_at(self) -> Option<SystemTime> {
        self.fleet.map(FileStamp::settles_at)
    }
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
#[cfg(unix)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_device_fleet_store;

        phase an_agent_grant_is_certified_into_the_fleet_and_revoked_as_a_device;
        {
            let root = tempfile::tempdir().expect("temp root");
            let identity_dir = root.path().join("config/identity");
            let paths = IdentityPaths::under(root.path().join("system"));
            crate::provisioning::ensure_collaboration_identity(
                &identity_dir,
                &paths,
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let identity_id = crate::store::IdentityRegistryStore::at(&identity_dir)
                .load()
                .expect("registry")
                .default_identity
                .expect("default identity");
            let store = DeviceFleetStore::at(&identity_dir, paths);
            let now = store.load(&identity_id).expect("fleet").certificates[0].issued_at_ns;
            let agent = ed25519_dalek::SigningKey::from_bytes(&[17; 32])
                .verifying_key()
                .to_bytes();
            let day = 24 * 60 * 60 * 1_000_000_000_u64;

            let cert = store
                .certify_agent_grant(
                    &identity_id,
                    agent,
                    AgentScope::AllWorkspaces,
                    "Agent key",
                    now,
                    now + 30 * day,
                )
                .expect("the root certifies it");
            assert_eq!(cert.capabilities.agent, Some(AgentScope::AllWorkspaces));
            assert!(!cert.capabilities.can_admit_devices && !cert.capabilities.server_key);
            verify_device_cert_at(&cert, now).expect("a verifier takes it");
            let fleet = store.load(&identity_id).expect("fleet");
            assert!(
                fleet
                    .certificates
                    .iter()
                    .any(|kept| kept.device_public_key == agent),
                "kept with the person's devices"
            );

            // An agent's grant expires within what any verifier takes.
            assert!(
                store
                    .certify_agent_grant(
                        &identity_id,
                        agent,
                        AgentScope::AllWorkspaces,
                        "Agent key",
                        now,
                        now + 400 * day,
                    )
                    .is_err()
            );

            store
                .revoke(&identity_id, agent, RevocationReason::Retired, now + 1)
                .expect("revoked as a device is");
            assert!(store.load(&identity_id).expect("fleet").is_revoked(&agent));
        }

        phase a_computer_that_carries_its_persons_identity_directory_signs_with_its_root;
        // A person's second computer carries the account's identity directory over from the
        // computer that made their root, the secret that signs with the root included, as a disk
        // copy carries it. It also keeps the public projection a device enrolled without the root
        // takes up under the same person, as every device that booted before this build did. It
        // signs as the person there: the record that holds the root is theirs.
        {
            let first = tempfile::tempdir().expect("the first computer");
            let first_identity = first.path().join("config/identity");
            crate::provisioning::ensure_collaboration_identity(
                &first_identity,
                &IdentityPaths::under(first.path().join("system")),
                crate::provisioning::IdentityStorage::Files,
            )
            .expect("provision");
            let record =
                crate::store::resolve_active_identity(&first_identity).expect("a record");
            let root = crate::store::load_from_path(record).expect("the person's root");
            let person = hex::encode(root.group_public_key());

            let here = tempfile::tempdir().expect("another computer");
            let identity_dir = here.path().join("config/identity");
            copy_tree(&first_identity, &identity_dir);
            let projection = identity_dir.join(format!("identity-{}", &person[..55]));
            let public = crate::store::public_member_record("Ada", [1, 2, 3], root.group_public_key())
                .expect("a public record");
            tilo_files::native::atomic::write_with_mode(&projection, &public, 0o600)
                .expect("the projection is kept");
            assert!(
                !crate::store::load_metadata_from_path(&projection)
                    .expect("the projection reads")
                    .single_device,
                "the projection holds no share of the root"
            );

            let store = DeviceFleetStore::at(
                &identity_dir,
                IdentityPaths::under(here.path().join("system")),
            );
            let signed = store
                .sign_root(&person, &[9; 32])
                .expect("the record that holds the root signs");
            assert_eq!(signed.len(), 64, "the person's own signature");
            let profile = crate::store::IdentityRegistryStore::at(&identity_dir)
                .load()
                .expect("the registry")
                .identities
                .get(&person)
                .cloned()
                .expect("the person's identity");
            assert_eq!(
                profile.record_path,
                identity_dir.join("primary.json"),
                "the record holding the root is the person's, not the projection"
            );
        }

    }

    /// Copy `from`'s files to `to`, as a disk copy of a computer carries what its account's
    /// identity directory holds.
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).expect("a copied directory");
        for entry in fs::read_dir(from).expect("a readable directory") {
            let entry = entry.expect("a directory entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("an entry's kind").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).expect("a copied file");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_persons_fleet_is_not_written_through_a_planted_symlink() {
        let root = tempfile::tempdir().expect("temp root");
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir(&elsewhere).expect("target");
        let identity_dir = root.path().join("identity");
        std::os::unix::fs::symlink(&elsewhere, &identity_dir).expect("planted link");
        let store = DeviceFleetStore::at(&identity_dir, IdentityPaths::under(root.path()))
            .for_person(rustix::process::geteuid().as_raw());

        let saved = store.save(&DeviceFleet::new("07".repeat(32)));
        assert_eq!(
            saved.expect_err("a symlinked identity directory").kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(store.lock_fleets().is_err());
        assert_eq!(fs::read_dir(&elsewhere).expect("target").count(), 0);
    }
}
