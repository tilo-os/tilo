//! Persistent device enrollment and revocation for each local user identity.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, SystemTime};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, RevocationEntry, RevocationReason,
    decode_enrollment_token, encode_enrollment_token, mint_enrollment_token,
    revocation_signing_digest, verify_device_cert, verify_device_cert_at, verify_revocation_entry,
};

const FLEET_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFleet {
    pub schema_version: u32,
    pub identity_id: String,
    pub certificates: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    /// Enrollment nonces an older build recorded here. Redeemed nonces now
    /// live in [`crate::enrollment::NonceStore`], which the nearby-pairing
    /// broker shares; the next redemption moves these there and saves the
    /// fleet without them. Omitted while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    consumed_enrollments: Vec<ConsumedEnrollment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumedEnrollment {
    nonce: [u8; 16],
}

impl DeviceFleet {
    /// An empty fleet of `identity_id`: no certificates, revocations or
    /// consumed enrollments.
    #[must_use]
    pub fn new(identity_id: String) -> Self {
        Self {
            schema_version: FLEET_SCHEMA_VERSION,
            identity_id,
            certificates: Vec::new(),
            revocations: Vec::new(),
            consumed_enrollments: Vec::new(),
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
        let registry = crate::registry::IdentityRegistryStore::from_env()?;
        Ok(Self::at(
            registry.identity_dir(),
            crate::provisioning::collaboration_identity_paths(),
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
        #[cfg(test)]
        LOADS.with(|loads| loads.set(loads.get().saturating_add(1)));
        let path = self.path(identity_id);
        let mut fleet: DeviceFleet = if path.exists() {
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
        for cert in &fleet.certificates {
            verify_device_cert(cert)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        for entry in &fleet.revocations {
            verify_revocation_entry(entry)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        }
        // Home peers are enrolled by the privileged LAN broker rather than the
        // ordinary device-completion UI. Their root-signed certificates live
        // in the shared enrollment registry and are projected into the same
        // fleet so list/revoke works identically for headless devices.
        if let Some(registry) = self.home_peer_registry()
            && let Ok(metadata) = fs::metadata(&registry)
            && metadata.len() <= 1024 * 1024
        {
            let registrations: Vec<tilo_relay_protocol::HomePeerRegistration> =
                serde_json::from_slice(&fs::read(&registry)?)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let identity = decode_hex_32(identity_id)?;
            for cert in registrations
                .into_iter()
                .filter_map(|registration| registration.device_cert)
                .filter(|cert| cert.user_public_key == identity)
            {
                verify_device_cert(&cert)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if !fleet
                    .certificates
                    .iter()
                    .any(|known| known.device_public_key == cert.device_public_key)
                {
                    fleet.certificates.push(cert);
                }
            }
        }
        Ok(fleet)
    }

    /// [`Self::load`] for authorization checks that run on every remote
    /// action and broker call. The verified fleet is served from memory while
    /// none of its backing files (the fleet file and the home-peer registry)
    /// changed: one `stat` each instead of reading, parsing and verifying
    /// them. Every writer replaces the fleet file by rename and [`Self::save`]
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
        let cert = tilo_user_identity::persist::read_device_cert(&cert_path)
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
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> io::Result<String> {
        let (_, profile) = self.identity(identity_id)?;
        let device = tilo_user_identity::persist::load_device_key(&self.identity_paths)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let cert_path = self
            .identity_paths
            .device_cert_for_user(&profile.user_public_key);
        let cert = tilo_user_identity::persist::read_device_cert(&cert_path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        verify_device_cert_at(&cert, issued_at_ns)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if cert.device_public_key != device.verifying_key().to_bytes()
            || !cert.capabilities.can_admit_devices
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this device may not admit another device",
            ));
        }
        let mut nonce = [0_u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
        let token = mint_enrollment_token(
            profile.user_public_key,
            capabilities,
            issued_at_ns,
            expires_at_ns,
            nonce,
            &device,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        encode_enrollment_token(&token).map_err(|error| io::Error::other(error.to_string()))
    }

    /// Complete enrollment after the new device returns its public key:
    /// [`crate::enrollment::redeem`] with this identity's root, for a token
    /// this device received as text.
    ///
    /// # Errors
    ///
    /// Returns an error if the token can't be decoded or names another
    /// identity than `identity_id`, if the identity can't be loaded, or as
    /// [`crate::enrollment::redeem`].
    pub fn complete_enrollment(
        &self,
        identity_id: &str,
        encoded_token: &str,
        role: crate::enrollment::EnrollmentRole,
        new_device_public_key: [u8; 32],
        device_label: &str,
        now_ns: u64,
    ) -> io::Result<DeviceCert> {
        let token = decode_enrollment_token(encoded_token)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let (root, profile) = self.identity(identity_id)?;
        if token.user_public_key != profile.user_public_key {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "enrollment token belongs to another identity",
            ));
        }
        crate::enrollment::redeem(
            Some(self),
            &self.nonces(),
            &crate::enrollment::Redemption {
                token: &token,
                role,
                device_public_key: new_device_public_key,
                label: device_label,
                now_ns,
                valid_from_ns: now_ns,
                local_issuer: None,
            },
            &|digest| {
                root.sign(digest)
                    .map_err(|error| io::Error::other(error.to_string()))
            },
        )
    }

    /// The fleet half of [`crate::enrollment::redeem`]: under the fleet lock,
    /// validate the issuer against the fleet, spend the nonce and publish the
    /// certificate. The nonce is spent durably before the certificate is
    /// saved or returned, so a crash in between burns the token rather than
    /// leaving it redeemable.
    pub(crate) fn redeem_locked(
        &self,
        nonces: &crate::enrollment::NonceStore,
        request: &crate::enrollment::Redemption<'_>,
        sign: crate::enrollment::RootSign<'_>,
    ) -> io::Result<DeviceCert> {
        let token = request.token;
        let identity_id = hex::encode(token.user_public_key);
        // Serialize nonce consumption with certificate publication. Atomic file
        // replacement alone prevents torn JSON, but without this directory
        // lock two concurrent redeemers could both load the nonce as unused.
        let _fleet_lock = self.lock_fleets()?;
        let mut fleet = self.load(&identity_id)?;
        let issuer = fleet
            .certificates
            .iter()
            .find(|cert| cert.device_public_key == token.issuer_device_public_key)
            .or(request.local_issuer)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "issuer not enrolled")
            })?;
        crate::enrollment::check_issuer(
            token,
            issuer,
            fleet.is_revoked(&token.issuer_device_public_key),
            request.now_ns,
        )?;
        if fleet
            .consumed_enrollments
            .iter()
            .any(|consumed| consumed.nonce == token.nonce)
            || nonces.is_consumed(&token.nonce)?
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "enrollment token has already been consumed",
            ));
        }
        let cert = crate::enrollment::certify(request, sign)?;
        // Nonces an older build kept in the fleet move to the one store.
        for consumed in &fleet.consumed_enrollments {
            nonces.import(&consumed.nonce)?;
        }
        fleet.consumed_enrollments.clear();
        nonces.claim(&token.nonce, request.device_public_key)?;
        fleet
            .certificates
            .retain(|existing| existing.device_public_key != cert.device_public_key);
        fleet.certificates.push(cert.clone());
        self.save(&fleet)?;
        Ok(cert)
    }

    /// The consumed enrollment nonces of this device's identity root.
    #[must_use]
    pub fn nonces(&self) -> crate::enrollment::NonceStore {
        crate::enrollment::NonceStore::under(&self.identity_root())
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
        let _fleet_lock = self.lock_fleets()?;
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

    /// Certify a disk-unlock boot key for an enrolled, unrevoked device of
    /// `identity_id`: the root signs `cert` in place.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity cannot sign, the certificate names
    /// another root, or the device is not an unrevoked member of the fleet.
    pub fn certify_boot_key(
        &self,
        identity_id: &str,
        cert: &mut tilo_unlock_protocol::BootKeyCert,
    ) -> io::Result<()> {
        let (root, profile) = self.identity(identity_id)?;
        let fleet = self.load(identity_id)?;
        if cert.user_public_key != profile.user_public_key
            || fleet.is_revoked(&cert.device_public_key)
            || !fleet
                .certificates
                .iter()
                .any(|enrolled| enrolled.device_public_key == cert.device_public_key)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the device is not an unrevoked member of this identity's fleet",
            ));
        }
        let digest = cert
            .signing_digest()
            .map_err(|error| io::Error::other(error.to_string()))?;
        cert.signature = root
            .sign(&digest)
            .map_err(|error| io::Error::other(error.to_string()))?;
        cert.verify()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// The person's secret-sync key, when this device holds the root of
    /// `identity_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity cannot be loaded or holds no root.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the certificate is invalid, belongs to another
    /// person, or the fleet cannot be saved.
    pub fn remember_certificate(
        &self,
        identity_id: &str,
        cert: DeviceCert,
    ) -> io::Result<DeviceFleet> {
        self.insert_certificate(identity_id, cert)
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

    pub(crate) fn identity(
        &self,
        identity_id: &str,
    ) -> io::Result<(
        runtime_identity::SigningIdentity,
        crate::registry::IdentityProfile,
    )> {
        let store = crate::registry::IdentityRegistryStore::at(&self.identity_dir);
        let registry = store.load_reconciled()?;
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
        let written = tilo_files::native::atomic::write_creating_parent(&path, &bytes)
            .and_then(|()| self.keep_owned_by_person(&self.identity_dir.join("fleets")))
            .and_then(|()| self.keep_owned_by_person(&path));
        if let Ok(mut cache) = FLEET_CACHE.write() {
            cache.remove(&self.cache_key(&fleet.identity_id));
        }
        // An enrolment or a revocation changes who may act: re-check now,
        // not when the identity directory's watch reports it.
        crate::policy_change::announce();
        written
    }

    fn lock_fleets(&self) -> io::Result<advisory_lock::FileLock> {
        let directory = self.identity_dir.join("fleets");
        fs::create_dir_all(&directory)?;
        self.keep_owned_by_person(&directory)?;
        advisory_lock::FileLock::exclusive(File::open(directory)?)
    }

    /// Give `path` the owner and group of the person's identity directory
    /// when this process created it as someone else. Only a privileged
    /// redeemer can: the nearby-pairing broker approving with a root seed
    /// runs as root, and must leave the person's fleet readable and writable
    /// by the person's own session.
    fn keep_owned_by_person(&self, path: &Path) -> io::Result<()> {
        let person = fs::metadata(&self.identity_dir)?;
        let current = fs::symlink_metadata(path)?;
        if current.uid() == person.uid() {
            return Ok(());
        }
        std::os::unix::fs::lchown(path, Some(person.uid()), Some(person.gid()))
    }

    /// The directory holding `user-identity/` and `device-private/`.
    fn identity_root(&self) -> PathBuf {
        self.identity_paths
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .map_or_else(PathBuf::new, Path::to_path_buf)
    }

    fn path(&self, identity_id: &str) -> PathBuf {
        self.identity_dir
            .join("fleets")
            .join(format!("{identity_id}.json"))
    }

    /// The shared home-peer enrollment registry projected into every fleet.
    fn home_peer_registry(&self) -> Option<PathBuf> {
        self.identity_paths
            .user_pubkey
            .parent()
            .and_then(Path::parent)
            .map(tilo_relay_protocol::home_peer_registry_path)
    }

    /// Stamps of the files [`Self::load_cached`] reads for `identity_id`, or
    /// `None` when one can't be examined.
    pub(crate) fn stamps(&self, identity_id: &str) -> Option<FleetStamps> {
        self.cache_key(identity_id).stamps()
    }

    fn cache_key(&self, identity_id: &str) -> FleetKey {
        FleetKey {
            fleet: self.path(identity_id),
            home_peers: self.home_peer_registry(),
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

/// The files one fleet is loaded from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FleetKey {
    fleet: PathBuf,
    home_peers: Option<PathBuf>,
}

impl FleetKey {
    /// Stamps of both files, or `None` when either can't be examined (the
    /// caller then loads from disk and caches nothing).
    fn stamps(&self) -> Option<FleetStamps> {
        Some(FleetStamps {
            fleet: FileStamp::of(&self.fleet).ok()?,
            home_peers: match &self.home_peers {
                Some(path) => FileStamp::of(path).ok()?,
                None => None,
            },
        })
    }
}

struct CachedFleet {
    stamps: FleetStamps,
    fleet: Arc<DeviceFleet>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FleetStamps {
    fleet: Option<FileStamp>,
    home_peers: Option<FileStamp>,
}

impl FleetStamps {
    pub(crate) fn settled(self) -> bool {
        [self.fleet, self.home_peers]
            .into_iter()
            .flatten()
            .all(FileStamp::settled)
    }
}

/// Identity of one file's contents, as far as a writer can change it: a
/// rename gives a new inode, an in-place rewrite a new length or time, and a
/// permission change or a reset modification time a new status-change time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified: SystemTime,
    changed: (i64, i64),
}

impl FileStamp {
    /// `Ok(None)` for a missing file; an error when it can't be examined.
    pub(crate) fn of(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(meta) => Ok(Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
                len: meta.len(),
                modified: meta.modified()?,
                changed: (meta.ctime(), meta.ctime_nsec()),
            })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Old enough that a later write, which stamps the current time, cannot
    /// share its modification time.
    pub(crate) fn settled(self) -> bool {
        SystemTime::now()
            .duration_since(self.modified)
            .is_ok_and(|age| age >= Duration::from_secs(2))
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
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn fleet_enrollment_loads_revokes_and_resists_clock_rollback() {
        // Phase `a_fleet_without_consumed_enrollments_loads_empty`.
        {
            let fleet: DeviceFleet = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "identity_id": "00",
                "certificates": [],
                "revocations": []
            }))
            .expect("fleet without consumed enrollments");
            assert!(fleet.consumed_enrollments.is_empty());
        }
        // Phase `enroll_list_and_revoke_a_secondary_device`.
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
            let registry = crate::registry::IdentityRegistryStore::at(&identity_dir)
                .load()
                .expect("registry");
            let identity_id = registry.default_identity.expect("default identity");
            let store = DeviceFleetStore::at(&identity_dir, paths);

            let before = store.load(&identity_id).expect("fleet");
            assert_eq!(before.certificates.len(), 1);
            let issued_at_ns = before.certificates[0].issued_at_ns;
            let token = store
                .mint_enrollment(
                    &identity_id,
                    DeviceCapabilities::member(),
                    issued_at_ns,
                    issued_at_ns + 5_000,
                )
                .expect("token");
            let phone = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
            let cert = store
                .complete_enrollment(
                    &identity_id,
                    &token,
                    crate::enrollment::EnrollmentRole::PersonalDevice,
                    phone.verifying_key().to_bytes(),
                    "Phone",
                    issued_at_ns,
                )
                .expect("complete");
            assert_eq!(cert.device_label, "Phone");
            assert!(!cert.capabilities.can_admit_devices);
            assert_eq!(
                store.load(&identity_id).expect("fleet").certificates.len(),
                2
            );
            let replay_device = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
            let replay = store.complete_enrollment(
                &identity_id,
                &token,
                crate::enrollment::EnrollmentRole::PersonalDevice,
                replay_device.verifying_key().to_bytes(),
                "Replay",
                issued_at_ns + 1,
            );
            assert_eq!(
                replay
                    .expect_err("a redeemed nonce must be single-use")
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                store.load(&identity_id).expect("fleet").certificates.len(),
                2,
                "replay must not publish another root-signed certificate"
            );

            let expires_with_issuer = store
                .mint_enrollment(
                    &identity_id,
                    DeviceCapabilities::member(),
                    issued_at_ns + 1,
                    issued_at_ns + 5_000,
                )
                .expect("second token");
            let (root_signer, _) = store.identity(&identity_id).expect("root signer");
            let mut fleet = store.load(&identity_id).expect("fleet");
            let issuer = fleet
                .certificates
                .iter_mut()
                .find(|cert| cert.capabilities.can_admit_devices)
                .expect("issuer certificate");
            issuer.expires_at_ns = Some(issued_at_ns + 2);
            let digest =
                tilo_user_identity::device_cert_signing_digest(issuer).expect("cert digest");
            issuer.signature = root_signer.sign(&digest).expect("root signature");
            store.save(&fleet).expect("expired issuer fleet");
            let after_issuer_expiry = store.complete_enrollment(
                &identity_id,
                &expires_with_issuer,
                crate::enrollment::EnrollmentRole::PersonalDevice,
                replay_device.verifying_key().to_bytes(),
                "Expired issuer",
                issued_at_ns + 3,
            );
            assert_eq!(
                after_issuer_expiry
                    .expect_err("expired issuer must not admit devices")
                    .kind(),
                io::ErrorKind::PermissionDenied
            );

            store
                .revoke(
                    &identity_id,
                    phone.verifying_key().to_bytes(),
                    RevocationReason::Lost,
                    issued_at_ns + 2,
                )
                .expect("revoke");
            assert!(
                store
                    .load(&identity_id)
                    .expect("fleet")
                    .is_revoked(&phone.verifying_key().to_bytes())
            );
        }
        // Phase `clock_rollback_cannot_revive_consumed_enrollment`.
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
            let registry = crate::registry::IdentityRegistryStore::at(&identity_dir)
                .load()
                .expect("registry");
            let identity_id = registry.default_identity.expect("default identity");
            let store = DeviceFleetStore::at(&identity_dir, paths);
            let issued_at_ns =
                store.load(&identity_id).expect("fleet").certificates[0].issued_at_ns;

            let first_token = store
                .mint_enrollment(
                    &identity_id,
                    DeviceCapabilities::member(),
                    issued_at_ns,
                    issued_at_ns + 5_000,
                )
                .expect("first token");
            let first_device = ed25519_dalek::SigningKey::from_bytes(&[95; 32]);
            store
                .complete_enrollment(
                    &identity_id,
                    &first_token,
                    crate::enrollment::EnrollmentRole::PersonalDevice,
                    first_device.verifying_key().to_bytes(),
                    "First device",
                    issued_at_ns,
                )
                .expect("first enrollment");

            let after_first_expiry = issued_at_ns + 5_001;
            let second_token = store
                .mint_enrollment(
                    &identity_id,
                    DeviceCapabilities::member(),
                    after_first_expiry,
                    after_first_expiry + 5_000,
                )
                .expect("second token");
            let second_device = ed25519_dalek::SigningKey::from_bytes(&[96; 32]);
            store
                .complete_enrollment(
                    &identity_id,
                    &second_token,
                    crate::enrollment::EnrollmentRole::PersonalDevice,
                    second_device.verifying_key().to_bytes(),
                    "Second device",
                    after_first_expiry,
                )
                .expect("second enrollment");

            let replay_device = ed25519_dalek::SigningKey::from_bytes(&[97; 32]);
            let replay = store.complete_enrollment(
                &identity_id,
                &first_token,
                crate::enrollment::EnrollmentRole::PersonalDevice,
                replay_device.verifying_key().to_bytes(),
                "Rollback replay",
                issued_at_ns + 1,
            );
            assert_eq!(
                replay
                    .expect_err("clock rollback must not revive a consumed token")
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            let fleet = store.load(&identity_id).expect("fleet");
            assert_eq!(fleet.certificates.len(), 3);
            for token in [&first_token, &second_token] {
                let nonce = decode_enrollment_token(token).expect("token").nonce;
                assert!(store.nonces().is_consumed(&nonce).expect("marker"));
            }
        }
    }

    #[test]
    fn concurrent_enrollment_redeemers_publish_exactly_one_certificate() {
        let root = tempfile::tempdir().expect("temp root");
        let identity_dir = root.path().join("config/identity");
        let paths = IdentityPaths::under(root.path().join("system"));
        crate::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            crate::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let registry = crate::registry::IdentityRegistryStore::at(&identity_dir)
            .load()
            .expect("registry");
        let identity_id = registry.default_identity.expect("default identity");
        let store = DeviceFleetStore::at(&identity_dir, paths);
        let issued_at_ns = store.load(&identity_id).expect("fleet").certificates[0].issued_at_ns;
        let token = store
            .mint_enrollment(
                &identity_id,
                DeviceCapabilities::member(),
                issued_at_ns,
                issued_at_ns + 5_000,
            )
            .expect("token");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let results = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for seed in [93_u8, 94_u8] {
                let store = store.clone();
                let identity_id = identity_id.clone();
                let token = token.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                handles.push(scope.spawn(move || {
                    let device = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
                    barrier.wait();
                    store.complete_enrollment(
                        &identity_id,
                        &token,
                        crate::enrollment::EnrollmentRole::PersonalDevice,
                        device.verifying_key().to_bytes(),
                        &format!("Device {seed}"),
                        issued_at_ns,
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("redeemer thread"))
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|error| error.kind() == io::ErrorKind::PermissionDenied))
                .count(),
            1
        );
        let fleet = store.load(&identity_id).expect("fleet");
        assert_eq!(fleet.certificates.len(), 2);
        let nonce = decode_enrollment_token(&token).expect("token").nonce;
        assert!(store.nonces().is_consumed(&nonce).expect("marker"));
    }

    /// A nonce an older build spent in the fleet stays spent, and the next
    /// redemption moves it into the shared nonce store.
    #[test]
    fn nonces_spent_in_the_fleet_move_to_the_shared_store() {
        let root = tempfile::tempdir().expect("temp root");
        let identity_dir = root.path().join("config/identity");
        let paths = IdentityPaths::under(root.path().join("system"));
        crate::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            crate::provisioning::IdentityStorage::Files,
        )
        .expect("provision");
        let identity_id = crate::registry::IdentityRegistryStore::at(&identity_dir)
            .load()
            .expect("registry")
            .default_identity
            .expect("default identity");
        let store = DeviceFleetStore::at(&identity_dir, paths);
        let issued_at_ns = store.load(&identity_id).expect("fleet").certificates[0].issued_at_ns;
        let mint = || {
            store
                .mint_enrollment(
                    &identity_id,
                    DeviceCapabilities::member(),
                    issued_at_ns,
                    issued_at_ns + 5_000,
                )
                .expect("token")
        };
        let spent_earlier = mint();
        let spent_nonce = decode_enrollment_token(&spent_earlier)
            .expect("token")
            .nonce;
        let mut fleet = store.load(&identity_id).expect("fleet");
        fleet
            .consumed_enrollments
            .push(ConsumedEnrollment { nonce: spent_nonce });
        store.save(&fleet).expect("older fleet");

        let replay = store.complete_enrollment(
            &identity_id,
            &spent_earlier,
            crate::enrollment::EnrollmentRole::PersonalDevice,
            [71; 32],
            "Replay",
            issued_at_ns,
        );
        assert_eq!(
            replay
                .expect_err("a nonce the fleet recorded stays spent")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let phone = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
        store
            .complete_enrollment(
                &identity_id,
                &mint(),
                crate::enrollment::EnrollmentRole::PersonalDevice,
                phone.verifying_key().to_bytes(),
                "Phone",
                issued_at_ns,
            )
            .expect("a fresh token");
        assert!(store.nonces().is_consumed(&spent_nonce).expect("marker"));
        assert!(
            store
                .load(&identity_id)
                .expect("fleet")
                .consumed_enrollments
                .is_empty()
        );
        let saved = fs::read_to_string(store.path(&identity_id)).expect("fleet file");
        assert!(!saved.contains("consumed_enrollments"), "{saved}");
    }
}
