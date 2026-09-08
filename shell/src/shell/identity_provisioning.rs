//! Shell-side host for the identity ceremony: persistence
//! (`~/.config/qos/identity/primary.json`, `0600`), the first-run
//! trigger, invite-URL parsing, and the session pairing-broker boundary.
//!
//! New ceremonies emit schema-1 JSON matching
//! [`ui::widget::identity_flow::IdentityRecord`] in memory, then the runtime
//! lifecycle store atomically imports it as public schema-2 metadata backed by
//! Secret Service or a host-only broker file before the ceremony returns.
//!
//! Intentionally thin: cryptographic logic stays in
//! `threshold-identity`, visual logic stays in `ui`.

use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg(test)]
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use ui::widget::identity_flow::{
    IdentityFlowError, IdentityRecord, IdentityStore, InviteEnvelope, PairingBrokerRequest,
    PairingBrokerResponse, PairingHandshake,
};

pub struct PairingBrokerServer {
    endpoint: std::net::SocketAddr,
    certificate: Vec<u8>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone)]
enum BrokerPurpose {
    Pairing([u8; 16]),
    Invites(Arc<PathBuf>),
}

const BROKER_IDENTITY_FILE: &str = "broker-identity.postcard";
const MAX_BROKER_IDENTITY_BYTES: u64 = 128 * 1024;

#[derive(Serialize, Deserialize)]
struct StoredBrokerIdentity {
    version: u8,
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
}

impl PairingBrokerServer {
    /// Start a short-lived localhost broker for one pairing ceremony.
    ///
    /// # Errors
    ///
    /// Returns an error if TLS identity generation or socket binding fails.
    pub fn start(authority: SharedPairingChannel, nonce: [u8; 16]) -> io::Result<Self> {
        Self::start_on(authority, nonce, (std::net::Ipv4Addr::LOCALHOST, 0).into())
    }

    /// Bind the authority to an explicitly selected network interface. The
    /// caller advertises the resulting address in the QR payload; the random
    /// channel nonce is the bearer authenticator checked on every request.
    ///
    /// # Errors
    ///
    /// Returns an error if TLS identity generation or socket binding fails.
    pub fn start_on(
        authority: SharedPairingChannel,
        nonce: [u8; 16],
        bind: std::net::SocketAddr,
    ) -> io::Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec!["qos-pairing.invalid".into()])
            .map_err(io::Error::other)?;
        let certificate = certified.cert.der().to_vec();
        let key = certified.signing_key.serialize_der();
        Self::start_with_material(
            authority,
            BrokerPurpose::Pairing(nonce),
            bind,
            certificate,
            key,
        )
    }

    /// Start an ephemeral localhost broker limited to invite redemption.
    ///
    /// # Errors
    ///
    /// Returns an error if TLS identity generation or socket binding fails.
    pub fn start_invites(
        authority: SharedPairingChannel,
        invite_authority_root: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec!["qos-pairing.invalid".into()])
            .map_err(io::Error::other)?;
        Self::start_with_material(
            authority,
            BrokerPurpose::Invites(Arc::new(invite_authority_root.into())),
            (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            certified.cert.der().to_vec(),
            certified.signing_key.serialize_der(),
        )
    }

    /// Long-lived broker identity for invites. Certificate and private key
    /// survive a shell restart so outstanding signed invite URLs keep their
    /// exact TLS pin.
    ///
    /// # Errors
    ///
    /// Returns an error if private identity storage is unsafe/corrupt or the
    /// socket/TLS configuration cannot be created.
    pub fn start_persistent(
        authority: SharedPairingChannel,
        nonce: [u8; 16],
        bind: std::net::SocketAddr,
        identity_dir: &Path,
    ) -> io::Result<Self> {
        prepare_private_directory(identity_dir, "pairing TLS identity")?;
        let identity_path = identity_dir.join(BROKER_IDENTITY_FILE);
        let identity = match read_private_file(&identity_path, MAX_BROKER_IDENTITY_BYTES)? {
            Some(bytes) => decode_broker_identity(&bytes)?,
            None => create_broker_identity(identity_dir, &identity_path)?,
        };
        // `with_single_cert` below proves that the stored private key matches
        // the pinned certificate. Corruption is never "repaired" by rotating
        // the identity, because that would silently invalidate outstanding
        // signed invites.
        Self::start_with_material(
            authority,
            BrokerPurpose::Pairing(nonce),
            bind,
            identity.certificate_der,
            identity.private_key_der,
        )
    }

    /// Start a restart-stable broker limited to invite redemption.
    ///
    /// # Errors
    ///
    /// Returns an error if private identity storage is unsafe/corrupt or the
    /// socket/TLS configuration cannot be created.
    pub fn start_persistent_invites(
        authority: SharedPairingChannel,
        bind: std::net::SocketAddr,
        identity_dir: &Path,
        invite_authority_root: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        prepare_private_directory(identity_dir, "invite TLS identity")?;
        let identity_path = identity_dir.join(BROKER_IDENTITY_FILE);
        let identity = match read_private_file(&identity_path, MAX_BROKER_IDENTITY_BYTES)? {
            Some(bytes) => decode_broker_identity(&bytes)?,
            None => create_broker_identity(identity_dir, &identity_path)?,
        };
        Self::start_with_material(
            authority,
            BrokerPurpose::Invites(Arc::new(invite_authority_root.into())),
            bind,
            identity.certificate_der,
            identity.private_key_der,
        )
    }

    fn start_with_material(
        authority: SharedPairingChannel,
        purpose: BrokerPurpose,
        bind: std::net::SocketAddr,
        certificate: Vec<u8>,
        key: Vec<u8>,
    ) -> io::Result<Self> {
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key);
        let tls = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(certificate.clone())],
                    key.into(),
                )
                .map_err(io::Error::other)?,
        );
        let listener = std::net::TcpListener::bind(bind)?;
        let endpoint = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((tcp, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let previous = active_connections.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                if previous >= 16 {
                    active_connections.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    continue;
                }
                let tls = Arc::clone(&tls);
                let authority = Arc::clone(&authority);
                let purpose = purpose.clone();
                let active = Arc::clone(&active_connections);
                std::thread::spawn(move || {
                    let _guard = ActiveConnectionGuard(active);
                    let _ = tcp.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = tcp.set_write_timeout(Some(Duration::from_secs(2)));
                    let Ok(connection) = rustls::ServerConnection::new(tls) else {
                        return;
                    };
                    let mut stream = rustls::StreamOwned::new(connection, tcp);
                    let mut len = [0; 4];
                    if stream.read_exact(&mut len).is_err() {
                        return;
                    }
                    let len = u32::from_be_bytes(len) as usize;
                    if len > 256 * 1024 {
                        return;
                    }
                    let mut bytes = vec![0; len];
                    if stream.read_exact(&mut bytes).is_err() {
                        return;
                    }
                    let response = match postcard::from_bytes::<PairingBrokerRequest>(&bytes) {
                        Ok(request) => serve_pairing_request(&authority, &purpose, request),
                        Err(_) => PairingBrokerResponse::Denied,
                    };
                    if let Ok(bytes) = postcard::to_allocvec(&response) {
                        let Ok(response_len) = u32::try_from(bytes.len()) else {
                            return;
                        };
                        let _ = stream.write_all(&response_len.to_be_bytes());
                        let _ = stream.write_all(&bytes);
                    }
                });
            }
        });
        Ok(Self {
            endpoint,
            certificate,
            stop,
            thread: Some(thread),
        })
    }

    pub fn endpoint(&self) -> std::net::SocketAddr {
        self.endpoint
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }
}

struct ActiveConnectionGuard(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn prepare_private_directory(path: &Path, label: &str) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{label} must be a user-owned real directory"),
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700).create(path)?;
        }
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn read_private_file(path: &Path, limit: u64) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > limit
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "secret file must be private, user-owned, regular, and bounded",
        ));
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(
        usize::try_from(metadata.len()).unwrap_or_default(),
    ));
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "secret file exceeds its size limit",
        ));
    }
    Ok(Some(bytes))
}

fn decode_broker_identity(bytes: &[u8]) -> io::Result<StoredBrokerIdentity> {
    let identity: StoredBrokerIdentity = postcard::from_bytes(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if identity.version != 1
        || identity.certificate_der.is_empty()
        || identity.certificate_der.len() > 64 * 1024
        || identity.private_key_der.is_empty()
        || identity.private_key_der.len() > 64 * 1024
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "persistent pairing TLS identity is invalid",
        ));
    }
    Ok(identity)
}

fn create_broker_identity(
    identity_dir: &Path,
    identity_path: &Path,
) -> io::Result<StoredBrokerIdentity> {
    use std::os::unix::fs::OpenOptionsExt;
    let certified = rcgen::generate_simple_self_signed(vec!["qos-pairing.invalid".into()])
        .map_err(io::Error::other)?;
    let identity = StoredBrokerIdentity {
        version: 1,
        certificate_der: certified.cert.der().to_vec(),
        private_key_der: certified.signing_key.serialize_der(),
    };
    let bytes =
        zeroize::Zeroizing::new(postcard::to_allocvec(&identity).map_err(io::Error::other)?);
    if bytes.len() > usize::try_from(MAX_BROKER_IDENTITY_BYTES).unwrap_or(usize::MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generated pairing TLS identity is too large",
        ));
    }
    let mut random = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut random);
    let temporary = identity_dir.join(format!(".broker-identity-{}.tmp", hex::encode(random)));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        // A hard link publishes the complete inode without replacing a
        // concurrently-created identity. The loser loads the winner, keeping
        // one stable pin even if two shell instances race during startup.
        match std::fs::hard_link(&temporary, identity_path) {
            Ok(()) => {
                OpenOptions::new()
                    .read(true)
                    .open(identity_dir)?
                    .sync_all()?;
                Ok(identity)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let stored = read_private_file(identity_path, MAX_BROKER_IDENTITY_BYTES)?
                    .ok_or_else(|| io::Error::other("published TLS identity disappeared"))?;
                decode_broker_identity(&stored)
            }
            Err(error) => Err(error),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

impl Drop for PairingBrokerServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_pairing_request(
    authority: &PairingBrokerChannel,
    purpose: &BrokerPurpose,
    request: PairingBrokerRequest,
) -> PairingBrokerResponse {
    if matches!(purpose, BrokerPurpose::Pairing(_)) && !authority.is_accepting() {
        return PairingBrokerResponse::Denied;
    }
    match request {
        PairingBrokerRequest::PutPairerRound1(n, value) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) =>
        {
            authority.publish_pairer_round1(&n, value);
            PairingBrokerResponse::Ok
        }
        PairingBrokerRequest::PutPairerRound2(n, value, mac) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) =>
        {
            let Some(mac) = mac.into_bytes() else {
                return PairingBrokerResponse::Denied;
            };
            authority.publish_pairer_reply(&n, value, mac);
            PairingBrokerResponse::Ok
        }
        PairingBrokerRequest::PutPairerExchange(n, round1, round2, mac) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) =>
        {
            let Some(mac) = mac.into_bytes() else {
                return PairingBrokerResponse::Denied;
            };
            if authority.publish_pairer_exchange(&n, round1, round2, mac) {
                PairingBrokerResponse::Ok
            } else {
                PairingBrokerResponse::Denied
            }
        }
        PairingBrokerRequest::TakePairerExchange(n) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) => {
            PairingBrokerResponse::PairerExchange(
                authority
                    .take_pairer_exchange(&n)
                    .map(|(a, b, m)| (a, b, ui::widget::identity_flow::PairingMac::from_bytes(m))),
            )
        }
        PairingBrokerRequest::PutHostRound2(n, value, mac) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) =>
        {
            let Some(mac) = mac.into_bytes() else {
                return PairingBrokerResponse::Denied;
            };
            authority.publish_host_round2(&n, value, mac);
            PairingBrokerResponse::Ok
        }
        PairingBrokerRequest::TakeHostRound2(n) if matches!(purpose, BrokerPurpose::Pairing(allowed) if n == *allowed) => {
            PairingBrokerResponse::HostRound2(
                authority
                    .take_host_round2(&n)
                    .map(|(r, m)| (r, ui::widget::identity_flow::PairingMac::from_bytes(m))),
            )
        }
        PairingBrokerRequest::RedeemInvite(capability) => {
            let BrokerPurpose::Invites(authority_root) = purpose else {
                return PairingBrokerResponse::Denied;
            };
            // Expiry is decided by the trusted authority clock. Letting the
            // remote bearer supply this timestamp would make an expired
            // capability redeemable simply by replaying an older time.
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(u64::MAX, |duration| duration.as_secs());
            let secret =
                ui::widget::identity_flow::InviteRedemptionAuthority::at(&**authority_root)
                    .and_then(|authority| authority.redeem(&capability, now))
                    .ok()
                    .map(|secret| secret.to_vec());
            PairingBrokerResponse::InviteSecret(secret)
        }
        _ => PairingBrokerResponse::Denied,
    }
}

/// Resolve the user-owned device-identity root used by a qOS desktop session
/// installed on another Linux distribution.
///
/// XDG's data directory is the portable contract across Ubuntu, Fedora, Arch
/// and other hosts. Relative values are rejected: turning one into an
/// identity path using the compositor's working directory would make key
/// placement depend on the display manager.
///
/// # Errors
///
/// Returns an error if neither input identifies a home data directory, or if
/// the selected directory is relative.
pub fn host_identity_root(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> io::Result<PathBuf> {
    let base = if let Some(value) = xdg_data_home.filter(|value| !value.is_empty()) {
        PathBuf::from(value)
    } else {
        let home = home.filter(|value| !value.is_empty()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "neither XDG_DATA_HOME nor HOME is set for this login",
            )
        })?;
        PathBuf::from(home).join(".local/share")
    };
    if !base.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the host session data directory must be absolute",
        ));
    }
    Ok(base.join("qos"))
}

/// Select the per-user identity tree for a host-installed desktop session.
///
/// Must run while process startup is still single-threaded. The environment
/// variable is also inherited by launcher/broker children, keeping every
/// identity consumer on the same tree. An explicit existing override remains
/// available for hermetic development and tests.
///
/// # Errors
///
/// Returns an error if the configured identity root is relative or its
/// user-private directory tree cannot be safely prepared.
pub fn configure_host_identity_root() -> io::Result<PathBuf> {
    let root = match std::env::var_os("QOS_IDENTITY_ROOT").filter(|value| !value.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => host_identity_root(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))?,
    };
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "QOS_IDENTITY_ROOT must be absolute",
        ));
    }
    prepare_private_host_identity_tree(&root)?;
    // SAFETY: `main` calls this before it starts the warm-up thread or any
    // compositor/runtime worker. No other thread can read the environment.
    unsafe { std::env::set_var("QOS_IDENTITY_ROOT", &root) };
    Ok(root)
}

/// Create the host-session identity tree as a user-private trust boundary.
/// The full qOS image intentionally shares its device key with a dedicated
/// system group; a distro session has no such group and must not inherit a
/// host's broad primary-group membership instead.
#[cfg(unix)]
fn prepare_private_host_identity_tree(root: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let parent = root.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "identity root has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let parent_meta = std::fs::metadata(parent)?;
    if !parent_meta.is_dir()
        || parent_meta.uid() != unsafe { libc::geteuid() }
        || parent_meta.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host-session identity parent must be user-owned and not group/world writable",
        ));
    }

    for directory in [
        root.to_path_buf(),
        root.join("user-identity"),
        root.join("device"),
    ] {
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "host-session identity directory is not a user-owned real directory: {}",
                            directory.display()
                        ),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700).create(&directory)?;
            }
            Err(error) => return Err(error),
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_private_host_identity_tree(_root: &std::path::Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host desktop sessions require Unix filesystem permissions",
    ))
}

/// Writes public metadata to `$XDG_CONFIG_HOME/qos/identity/primary.json`
/// (falling back to `$HOME/.config/...`) and imports its private share into
/// the runtime identity provider before returning.
pub struct DiskIdentityStore {
    path: PathBuf,
}

impl DiskIdentityStore {
    /// Uses the canonical runtime resolver shared by Settings, first boot,
    /// and the launcher, so the boot chain cannot disagree because of a
    /// relative XDG path.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity directory can't be resolved or
    /// created.
    pub fn from_env() -> Result<Self, io::Error> {
        let base = runtime_identity::identity_dir_from_env()?;
        // Imported identities keep a fingerprinted filename. Resolve the
        // active runtime identity first so every consumer shares one
        // canonical selection and a successful import remains ready on the
        // next login.
        let path = runtime_identity::resolve_active_identity(&base)
            .unwrap_or_else(|| base.join("primary.json"));
        Ok(Self { path })
    }

    /// Used by tests to point at a tempdir without `$HOME` shimming.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity_present(&self) -> bool {
        self.path.exists()
    }
}

impl IdentityStore for DiskIdentityStore {
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(IdentityFlowError::Io(error)),
        };
        let metadata = file.metadata().map_err(IdentityFlowError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 1024 * 1024
        {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity record must be a private user-owned regular file",
            )));
        }
        // `metadata.len()` was already checked <= 1 MiB above; this cast is a
        // capacity hint, so a lossy fallback on exotic 32-bit targets is harmless.
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        (&mut file)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(IdentityFlowError::Io)?;
        if bytes.len() > 1024 * 1024 {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity record exceeds the size limit",
            )));
        }
        Ok(Some(IdentityRecord::from_json_bytes(&bytes)?))
    }

    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        let bytes = record.to_json_bytes()?;
        let parent = self.path.parent().ok_or_else(|| {
            IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity path has no parent",
            ))
        })?;
        let id = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                IdentityFlowError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "identity path has no valid id",
                ))
            })?;
        import_identity_record(parent, id, &bytes).map_err(IdentityFlowError::Io)?;
        Ok(())
    }
}

fn import_identity_record(parent: &Path, id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(parent);
        runtime_identity::import_identity_with_provider(parent, id, bytes, &provider)
    }
    #[cfg(not(test))]
    {
        runtime_identity::import_identity(parent, id, bytes)
    }
}

/// A free function, not a method on the store, so the boot path can
/// call it without owning the store first.
pub fn needs_first_run_flow(store: &impl IdentityStore) -> bool {
    match store.load() {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(e) => {
            // A corrupt, unsafe, or unreadable record is not the same as no
            // record. Keep trusted setup in front of the desktop so the user
            // can connect a valid backup; the creation path still validates
            // the existing record and refuses to overwrite it.
            tracing::error!("identity store load failed; refusing to replace it: {e}");
            true
        }
    }
}

/// Wraps `InviteEnvelope::from_url` plus the signature check so a
/// caller doesn't need to remember both steps.
///
/// # Errors
///
/// Returns an error if `url` doesn't parse as an invite envelope, or if
/// its signature doesn't verify.
pub fn parse_invite_url(url: &str) -> Result<InviteEnvelope, IdentityFlowError> {
    let env = InviteEnvelope::from_url(url)?;
    env.verify_signature()?;
    Ok(env)
}

/// Session-scoped pairing broker handle. Wire transports feed authenticated
/// DKG frames into the contained protocol state; expiry and cancellation are
/// enforced at the shell authority boundary rather than by either UI.
pub struct PairingBrokerChannel {
    protocol: PairingHandshake,
}

impl PairingBrokerChannel {
    pub fn cancel(&self) {
        self.protocol.cancel();
    }
}

impl std::ops::Deref for PairingBrokerChannel {
    type Target = PairingHandshake;

    fn deref(&self) -> &Self::Target {
        &self.protocol
    }
}

pub type SharedPairingChannel = Arc<PairingBrokerChannel>;

/// # Errors
///
/// Returns an error when the durable pairing protocol cannot be created at
/// `root`.
pub fn pairing_channel_at(
    root: impl Into<PathBuf>,
) -> Result<SharedPairingChannel, IdentityFlowError> {
    let expiry = SystemTime::now()
        .checked_add(Duration::from_secs(5 * 60))
        .unwrap_or_else(SystemTime::now);
    Ok(Arc::new(PairingBrokerChannel {
        protocol: PairingHandshake::durable(root, expiry)?,
    }))
}

/// Create a private, bounded broker spool under the runtime identity root.
/// The returned handle can be dropped and reopened with [`pairing_channel_at`]
/// by another process; protocol state is not owned by the UI lifetime.
///
/// # Errors
///
/// Returns an error when the identity root is unavailable or the durable
/// pairing protocol cannot be initialized.
pub fn new_pairing_channel() -> Result<SharedPairingChannel, IdentityFlowError> {
    let root = std::env::var_os("QOS_IDENTITY_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "pairing broker unavailable: QOS_IDENTITY_ROOT is not configured",
            ))
        })?;
    let mut id = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut id);
    pairing_channel_at(root.join("pairing-broker").join(hex::encode(id)))
}

/// Start the long-lived invite broker when the administrator has provided a
/// reachable bind/advertise pair. With neither variable set the feature is
/// honestly unavailable; a half-configuration is rejected.
///
/// # Errors
///
/// Returns an error for an incomplete or invalid bind/advertise
/// configuration, or when the broker cannot start.
pub fn start_configured_invite_broker() -> io::Result<Option<PairingBrokerServer>> {
    let bind = std::env::var("QOS_INVITE_BROKER_BIND").ok();
    let advertise = std::env::var("QOS_INVITE_BROKER_ADVERTISE").ok();
    let (bind, advertise) = match (bind, advertise) {
        (Some(bind), Some(advertise)) => (bind, advertise),
        (None, None) => return Ok(None),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "both QOS_INVITE_BROKER_BIND and QOS_INVITE_BROKER_ADVERTISE are required",
            ));
        }
    };
    let bind = bind
        .parse::<std::net::SocketAddr>()
        .map_err(io::Error::other)?;
    if bind.port() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "QOS_INVITE_BROKER_BIND must use a stable non-zero port so signed invites survive restart",
        ));
    }
    let mut advertised = advertise
        .parse::<std::net::SocketAddr>()
        .map_err(io::Error::other)?;
    if advertised.ip().is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "QOS_INVITE_BROKER_ADVERTISE must be a reachable address, not a wildcard",
        ));
    }
    let root = std::env::var_os("QOS_IDENTITY_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "QOS_IDENTITY_ROOT is not configured",
            )
        })?;
    let authority =
        pairing_channel_at(root.join("invite-broker-pairing-spool")).map_err(io::Error::other)?;
    let invite_authority_root = root.join("invite-redemption-authority");
    let server = PairingBrokerServer::start_persistent_invites(
        authority,
        bind,
        &root.join("invite-broker-tls"),
        &invite_authority_root,
    )?;
    if advertised.port() == 0 {
        advertised.set_port(server.endpoint().port());
    }
    ui::widget::identity_flow::configure_invite_broker(
        advertised.to_string(),
        server.certificate_der().to_vec(),
        invite_authority_root,
    )
    .map_err(io::Error::other)?;
    Ok(Some(server))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use std::time::SystemTime;
    use threshold_identity::ThresholdIdentity;
    use ui::widget::identity_flow::MemoryIdentityStore;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = base.join(format!("qos-identity-prov-{name}-{pid}-{nonce}"));
            fs::create_dir_all(&path).expect("tempdir");
            Self(path)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_record() -> IdentityRecord {
        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        IdentityRecord::from_threshold_identity(
            &id,
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(100))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
        .unwrap()
    }

    #[test]
    fn save_and_load_round_trip_on_disk() {
        let tmp = TmpDir::new("rt");
        let store = DiskIdentityStore::at(tmp.0.join("primary.json"));
        assert!(store.load().unwrap().is_none());
        let rec = fixture_record();
        store.save(&rec).unwrap();
        assert!(store.identity_present());
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.display_name, rec.display_name);
        assert_eq!(back.public_key(), rec.public_key());
        assert_eq!(back.schema_version, 2);
        assert!(back.threshold_identity.secret_bytes.is_empty());
        assert!(back.secret_ref.is_some());
    }

    #[test]
    fn save_sets_0600_mode_on_unix() {
        let tmp = TmpDir::new("mode");
        let store = DiskIdentityStore::at(tmp.0.join("primary.json"));
        store.save(&fixture_record()).unwrap();
        #[cfg(unix)]
        {
            let meta = fs::metadata(store.path()).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_an_identity_path_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TmpDir::new("symlink");
        let target = tmp.0.join("elsewhere.json");
        let bytes = fixture_record().to_json_bytes().unwrap();
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let path = tmp.0.join("primary.json");
        symlink(&target, &path).unwrap();

        let store = DiskIdentityStore::at(path);
        assert!(store.load().is_err());
    }

    #[test]
    fn first_run_flow_triggers_only_on_missing_identity() {
        let store = MemoryIdentityStore::new();
        assert!(needs_first_run_flow(&store));
        store.save(&fixture_record()).unwrap();
        assert!(!needs_first_run_flow(&store));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_existing_identity_keeps_trusted_setup_in_front_of_the_desktop() {
        let tmp = TmpDir::new("invalid-existing");
        let path = tmp.0.join("primary.json");
        fs::write(&path, b"not an identity").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let store = DiskIdentityStore::at(path);
        assert!(
            needs_first_run_flow(&store),
            "an invalid identity must not let an identity-less desktop start"
        );
    }

    #[test]
    fn host_identity_root_uses_the_xdg_data_contract() {
        assert_eq!(
            host_identity_root(Some("/home/alice/.data".into()), Some("/ignored".into())).unwrap(),
            PathBuf::from("/home/alice/.data/qos")
        );
        assert_eq!(
            host_identity_root(None, Some("/home/alice".into())).unwrap(),
            PathBuf::from("/home/alice/.local/share/qos")
        );
    }

    #[test]
    fn host_identity_root_never_depends_on_the_working_directory() {
        assert!(host_identity_root(Some("relative".into()), Some("/home/alice".into())).is_err());
        assert!(host_identity_root(None, Some("relative".into())).is_err());
        assert!(host_identity_root(None, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_identity_tree_is_private_and_refuses_a_symlink_root() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let tmp = TmpDir::new("host-private-root");
        let root = tmp.0.join("data/qos");
        prepare_private_host_identity_tree(&root).unwrap();
        for directory in [&root, &root.join("user-identity"), &root.join("device")] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        let target = tmp.0.join("target");
        fs::create_dir(&target).unwrap();
        let linked = tmp.0.join("linked");
        symlink(&target, &linked).unwrap();
        assert!(prepare_private_host_identity_tree(&linked).is_err());
    }

    #[test]
    fn parse_invite_url_validates_signature() {
        use collab_types::{ChannelId, Role, generate_keypair};
        use ui::widget::identity_flow::mint_invite_for_broker;
        let tmp = TmpDir::new("parse-invite");
        let authority = pairing_channel_at(tmp.0.join("pairing")).unwrap();
        let invite_root = tmp.0.join("invite-authority");
        let server = PairingBrokerServer::start_invites(authority, &invite_root).unwrap();
        let (sk, pk) = generate_keypair();
        let env = mint_invite_for_broker(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + std::time::Duration::from_secs(3600),
            server.endpoint().to_string(),
            server.certificate_der().to_vec(),
            invite_root,
        )
        .unwrap();
        let url = env.to_url().unwrap();
        let back = parse_invite_url(&url).expect("valid URL must parse");
        assert_eq!(back.channel_name, "engineering");
    }

    #[test]
    fn parse_invite_url_rejects_garbage() {
        let err = parse_invite_url("https://example.com").unwrap_err();
        assert!(matches!(err, IdentityFlowError::DecodeFailed(_)));
    }

    #[test]
    fn load_on_missing_file_returns_none() {
        let tmp = TmpDir::new("missing");
        let store = DiskIdentityStore::at(tmp.0.join("does-not-exist.json"));
        assert!(store.load().unwrap().is_none());
        assert!(!store.identity_present());
    }

    #[test]
    fn new_pairing_channel_round_trips_a_message() {
        use threshold_identity::EnrolmentRound2;
        let tmp = TmpDir::new("pairing-roundtrip");
        let ch = pairing_channel_at(tmp.0.join("broker")).unwrap();
        let nonce = [0x55u8; 16];
        let mac = [0x11u8; 64];
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1, 2, 3],
        };
        ch.publish_pairer_reply(&nonce, reply.clone(), mac);
        let (back, tag) = ch.take_pairer_reply(&nonce).expect("must have reply");
        assert_eq!(back.from_share_index, reply.from_share_index);
        assert_eq!(tag, mac);
    }

    #[test]
    fn pairing_broker_survives_handle_restart_and_consumes_once() {
        use threshold_identity::EnrolmentRound2;
        let tmp = TmpDir::new("pairing-restart");
        let root = tmp.0.join("broker");
        let nonce = [0x42; 16];
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![7, 8, 9],
        };
        pairing_channel_at(&root)
            .unwrap()
            .publish_pairer_reply(&nonce, reply.clone(), [3; 64]);

        let restarted = pairing_channel_at(&root).unwrap();
        let (received, _) = restarted.take_pairer_reply(&nonce).unwrap();
        assert_eq!(received.package_bytes, reply.package_bytes);
        assert!(restarted.take_pairer_reply(&nonce).is_none());
    }

    #[test]
    fn pairing_broker_cancellation_survives_restart() {
        let tmp = TmpDir::new("pairing-cancel");
        let root = tmp.0.join("broker");
        pairing_channel_at(&root).unwrap().cancel();
        let restarted = pairing_channel_at(&root).unwrap();
        let reply = threshold_identity::EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1],
        };
        restarted.publish_pairer_reply(&[9; 16], reply, [1; 64]);
        assert!(restarted.take_pairer_reply(&[9; 16]).is_none());

        let server = PairingBrokerServer::start(restarted, [9; 16]).unwrap();
        let response = PairingHandshake::remote_call(
            &server.endpoint().to_string(),
            server.certificate_der(),
            &PairingBrokerRequest::TakePairerExchange([9; 16]),
        );
        assert!(matches!(response, Some(PairingBrokerResponse::Denied)));
    }

    #[test]
    fn remote_pairer_uses_only_authenticated_transport_endpoint() {
        let tmp = TmpDir::new("pairing-remote");
        let authority = pairing_channel_at(tmp.0.join("authority")).unwrap();
        let nonce = [4; 16];
        let server = PairingBrokerServer::start(authority.clone(), nonce).unwrap();
        let remote = PairingHandshake::remote(
            server.endpoint().to_string(),
            server.certificate_der().to_vec(),
        )
        .unwrap();
        let reply = threshold_identity::EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![5, 6],
        };
        remote.publish_pairer_reply(&nonce, reply.clone(), [8; 64]);
        assert_eq!(
            authority.take_pairer_reply(&nonce).unwrap().0.package_bytes,
            reply.package_bytes
        );
        remote.publish_pairer_reply(&[7; 16], reply, [8; 64]);
        assert!(authority.take_pairer_reply(&[7; 16]).is_none());

        let unrelated =
            rcgen::generate_simple_self_signed(vec!["qos-pairing.invalid".into()]).unwrap();
        assert!(
            PairingHandshake::remote_call(
                &server.endpoint().to_string(),
                unrelated.cert.der(),
                &PairingBrokerRequest::TakePairerExchange(nonce),
            )
            .is_none(),
            "the client must require the exact certificate embedded in the QR"
        );
    }

    #[test]
    fn invite_redeems_through_independent_tls_endpoint_once() {
        let tmp = TmpDir::new("invite-remote");
        let authority = pairing_channel_at(tmp.0.join("pairing-authority")).unwrap();
        let invite_root = tmp.0.join("invite-redemption-authority");
        let server = PairingBrokerServer::start_invites(authority, &invite_root).unwrap();
        let (owner_secret, owner_public) = collab_types::generate_keypair();
        let invite = ui::widget::identity_flow::mint_invite_for_broker(
            collab_types::ChannelId::new(),
            "remote",
            owner_public,
            &owner_secret,
            "Alice",
            collab_types::Role::Editor,
            SystemTime::now() + Duration::from_secs(60),
            server.endpoint().to_string(),
            server.certificate_der().to_vec(),
            invite_root,
        )
        .unwrap();
        let (_, invitee) = collab_types::generate_keypair();
        ui::widget::identity_flow::build_attestation_from_invite(
            &invite,
            invitee,
            SystemTime::now(),
        )
        .unwrap();
        let (_, replay) = collab_types::generate_keypair();
        assert!(
            ui::widget::identity_flow::build_attestation_from_invite(
                &invite,
                replay,
                SystemTime::now()
            )
            .is_err()
        );
    }

    #[test]
    fn invite_broker_uses_its_own_clock_and_rejects_pairing_frames() {
        let tmp = TmpDir::new("invite-expiry-authority");
        let pairing = pairing_channel_at(tmp.0.join("pairing")).unwrap();
        let invite_root = tmp.0.join("invite-redemption-authority");
        let invite_authority =
            ui::widget::identity_flow::InviteRedemptionAuthority::at(&invite_root).unwrap();
        invite_authority.issue([3; 32], [4; 32], 1).unwrap();
        let server = PairingBrokerServer::start_invites(pairing, invite_root).unwrap();
        let response = PairingHandshake::remote_call(
            &server.endpoint().to_string(),
            server.certificate_der(),
            &PairingBrokerRequest::RedeemInvite(vec![3; 32]),
        );
        assert!(matches!(
            response,
            Some(PairingBrokerResponse::InviteSecret(None))
        ));

        let response = PairingHandshake::remote_call(
            &server.endpoint().to_string(),
            server.certificate_der(),
            &PairingBrokerRequest::TakePairerExchange([0; 16]),
        );
        assert!(matches!(response, Some(PairingBrokerResponse::Denied)));
    }

    #[test]
    fn persistent_broker_keeps_tls_pin_across_restart() {
        let tmp = TmpDir::new("broker-tls-restart");
        let authority = pairing_channel_at(tmp.0.join("pairing")).unwrap();
        let first = PairingBrokerServer::start_persistent(
            authority.clone(),
            [2; 16],
            (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            &tmp.0.join("tls"),
        )
        .unwrap();
        let endpoint = first.endpoint();
        let certificate = first.certificate_der().to_vec();
        drop(first);
        let restarted =
            PairingBrokerServer::start_persistent(authority, [2; 16], endpoint, &tmp.0.join("tls"))
                .unwrap();
        assert_eq!(restarted.certificate_der(), certificate);
        let remote = PairingHandshake::remote(endpoint.to_string(), certificate).unwrap();
        let reply = threshold_identity::EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![3],
        };
        remote.publish_pairer_reply(&[2; 16], reply, [4; 64]);
        assert!(restarted.endpoint().ip().is_loopback());
        let metadata = fs::metadata(tmp.0.join("tls").join(BROKER_IDENTITY_FILE)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn signed_invite_redeems_after_broker_restart_with_the_same_pin() {
        let tmp = TmpDir::new("invite-broker-restart");
        let authority = pairing_channel_at(tmp.0.join("pairing")).unwrap();
        let tls_dir = tmp.0.join("tls");
        let first = PairingBrokerServer::start_persistent_invites(
            authority.clone(),
            (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            &tls_dir,
            tmp.0.join("invite-redemption-authority"),
        )
        .unwrap();
        let endpoint = first.endpoint();
        let certificate = first.certificate_der().to_vec();
        let (owner_secret, owner_public) = collab_types::generate_keypair();
        let invite = ui::widget::identity_flow::mint_invite_for_broker(
            collab_types::ChannelId::new(),
            "restart-safe",
            owner_public,
            &owner_secret,
            "Alice",
            collab_types::Role::Editor,
            SystemTime::now() + Duration::from_secs(60),
            endpoint.to_string(),
            certificate.clone(),
            tmp.0.join("invite-redemption-authority"),
        )
        .unwrap();
        drop(first);

        let restarted = PairingBrokerServer::start_persistent_invites(
            authority,
            endpoint,
            &tls_dir,
            tmp.0.join("invite-redemption-authority"),
        )
        .unwrap();
        assert_eq!(restarted.certificate_der(), certificate);
        let (_, invitee) = collab_types::generate_keypair();
        ui::widget::identity_flow::build_attestation_from_invite(
            &invite,
            invitee,
            SystemTime::now(),
        )
        .unwrap();
        let (_, replay) = collab_types::generate_keypair();
        assert!(
            ui::widget::identity_flow::build_attestation_from_invite(
                &invite,
                replay,
                SystemTime::now()
            )
            .is_err()
        );
    }

    #[test]
    fn persistent_broker_refuses_corrupt_identity_instead_of_rotating_pin() {
        let tmp = TmpDir::new("broker-corrupt-pin");
        let authority = pairing_channel_at(tmp.0.join("pairing")).unwrap();
        let tls_dir = tmp.0.join("tls");
        let server = PairingBrokerServer::start_persistent_invites(
            authority.clone(),
            (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            &tls_dir,
            tmp.0.join("invite-redemption-authority"),
        )
        .unwrap();
        drop(server);
        let identity = tls_dir.join(BROKER_IDENTITY_FILE);
        fs::write(&identity, b"truncated").unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            PairingBrokerServer::start_persistent_invites(
                authority,
                (std::net::Ipv4Addr::LOCALHOST, 0).into(),
                &tls_dir,
                tmp.0.join("invite-redemption-authority"),
            )
            .is_err()
        );
        assert_eq!(fs::read(identity).unwrap(), b"truncated");
    }

    #[test]
    fn full_dkg_crosses_only_the_pinned_tls_broker() {
        let tmp = TmpDir::new("pairing-full-tls");
        let authority = pairing_channel_at(tmp.0.join("authority")).unwrap();
        let mut rng = OsRng;
        let mut bootstrap = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (host_session, host_round1) = bootstrap.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let mut host = ui::widget::identity_flow::add_device_host::AddDeviceHostScreen::new();
        host.begin(host_round1, host_session, *bootstrap.group_public_key())
            .unwrap();
        let nonce = host.handshake().unwrap().nonce;
        let server = PairingBrokerServer::start(authority.clone(), nonce).unwrap();
        host.attach_broker(server.endpoint().to_string(), server.certificate_der())
            .unwrap();
        let payload = host.handshake().unwrap().payload.clone();
        let pairer = std::thread::spawn(move || {
            let mut screen = ui::widget::identity_flow::pair_device::PairDeviceScreen::new();
            screen.perform_pairing_from_payload(&payload).unwrap()
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let completed = loop {
            if host.poll_handshake(&authority).unwrap() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(completed, "pinned TLS broker DKG did not finish within 10s");
        let host_identity = host
            .take_completed_identity()
            .expect("completed broker DKG must retain the host identity");
        let pairer_identity = pairer.join().unwrap();
        assert_eq!(
            host_identity.group_public_key(),
            pairer_identity.group_public_key()
        );
    }
}
