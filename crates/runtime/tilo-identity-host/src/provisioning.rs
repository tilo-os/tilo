//! Provisioning of the collaboration identity chain: the personal identity
//! record, the per-device key and its root-signed certificate, and their
//! registration in the identity registry and device fleet.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use getrandom::{SysRng, rand_core::UnwrapErr};
use threshold_identity::ThresholdIdentity;
use tilo_user_identity::persist::DeviceKeyCustody;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths, persist};
use ui::widget::identity_flow::IdentityRecord;

/// Where a newly provisioned personal identity's private share is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStorage {
    /// The platform's secret store, or its explicitly reported private-file
    /// fallback: what a real session uses. On macOS the device key and the
    /// runtime's channel-state key are in the login Keychain too, with no
    /// file fallback.
    Platform,
    /// Plain private files beside the record, for isolated test fixtures.
    Files,
}

/// Set (to anything), it keeps every identity secret of the session in
/// [`IdentityStorage::Files`], out of the user's login Keychain: for tests,
/// and for a release built without a Developer ID signature
/// ([`runtime_identity::file_secrets_requested`]).
pub use runtime_identity::FILE_SECRETS_ENV;

impl IdentityStorage {
    /// What this session keeps its identity secrets in.
    #[must_use]
    pub fn for_session() -> Self {
        if !runtime_identity::file_secrets_requested() {
            return Self::Platform;
        }
        // A test asks for files and needs no word about it; anyone else is
        // told where the secrets went.
        if !(cfg!(debug_assertions) && std::env::var_os(FILE_SECRETS_ENV).is_some()) {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                tracing::warn!(
                    "this build has no Developer ID signature, and the Keychain would keep \
                     its identity secrets for this exact build only; they are in owner-only \
                     files in the profile instead"
                );
            });
        }
        Self::Files
    }

    /// Where the device key is kept under this storage.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn device_key_custody(self) -> DeviceKeyCustody {
        match self {
            Self::Platform => DeviceKeyCustody::Keychain,
            Self::Files => DeviceKeyCustody::File,
        }
    }

    /// Where the device key is kept under this storage: a file, which Tilo
    /// OS shares between the root filesystem daemon and the session.
    #[cfg(not(target_os = "macos"))]
    #[must_use]
    pub fn device_key_custody(self) -> DeviceKeyCustody {
        match self {
            Self::Platform | Self::Files => DeviceKeyCustody::File,
        }
    }
}

fn identity_load_error(error: runtime_identity::NotAvailable) -> io::Error {
    match error {
        runtime_identity::NotAvailable::ConnectionFailed(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// Human-facing name of the Unix account running this session.
///
/// This is only used by the compatibility path where Tilo Desktop is installed
/// on top of an existing Linux distribution. Native Tilo setup has its own
/// identity ceremony and must not derive identity metadata from a Unix
/// account that happens to run the session.
#[cfg(unix)]
pub fn login_account_display_name() -> String {
    use std::ffi::CStr;

    let uid = unsafe { libc::geteuid() };
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            std::ptr::addr_of_mut!(result),
        )
    };
    if status == 0 && !result.is_null() {
        let pwd = unsafe { pwd.assume_init() };
        let username = unsafe { CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .trim()
            .to_string();
        let gecos = unsafe { CStr::from_ptr(pwd.pw_gecos) }
            .to_string_lossy()
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if !gecos.is_empty() {
            return gecos;
        }
        if !username.is_empty() {
            return username;
        }
    }
    std::env::var("USER")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("User {uid}"))
}

#[cfg(not(unix))]
pub fn login_account_display_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Local user".to_string())
}

/// File name for the primary single-device identity record. Matches
/// what the runtime identity broker migrates and what `DiskIdentityStore`
/// writes via the UI's identity-flow ceremony.
pub const PRIMARY_IDENTITY_FILE: &str = "primary.json";

/// Sequence for same-directory temporary owner-claim files.  The final claim
/// is a hard link, not this name; `create_new` simply prevents a stale temp
/// file from ever being overwritten.
static OWNER_CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Current wall-clock time as nanoseconds since the epoch, clamped to
/// `u64::MAX` rather than truncated — a value that large is centuries past
/// any real device-cert issuance this crate computes against.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to 0..=u64::MAX immediately above, so it always fits u64"
)]
fn now_ns_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

/// Result of the "make sure a primary identity exists" step, exposed so
/// tests can distinguish "we created one" from "one was already there".
#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// The identity file already existed; we did not touch it.
    Existing,
    /// A fresh single-device identity was minted and written — the derived
    /// Ed25519 pubkey is included so a wrapper (log line, tests) can render the public half without re-reading the file.
    Created { pubkey_hex: String },
}

/// Whether this device runs the Tilo OS desktop or a Tilo runtime on an
/// operating system Tilo does not control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceClass {
    Native,
    Foreign,
}

impl DeviceClass {
    fn capabilities(self) -> DeviceCapabilities {
        DeviceCapabilities {
            foreign: self == Self::Foreign,
            ..DeviceCapabilities::primary()
        }
    }

    /// A foreign device never keeps a native certificate it finds on disk.
    /// Native provisioning keeps whatever verified certificate exists.
    fn must_reissue(self, cert: &DeviceCert) -> bool {
        self == Self::Foreign && !cert.capabilities.foreign
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityProvisioningContext {
    /// Native Tilo: the dedicated OOBE owns the identity and storage policy.
    NativeTilo,
    /// Tilo Desktop added to a pre-existing Linux distribution and user account.
    LegacyLinuxHost,
    /// A disposable identity a dev-image test session mints for itself.
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    ThrowawayTest,
}

/// Create a fresh single-device identity at `<identity_dir>/primary.json`.
/// The final JSON is public schema-2 metadata; its private share is imported
/// into Secret Service or the explicitly reported private-file fallback.
///
/// Idempotent: an existing file is returned as [`ProvisionOutcome::Existing`] with no modification.
///
/// # Errors
///
/// Returns an error if file system operations fail (permission denied, I/O error, etc.).
pub fn ensure_primary_identity(
    identity_dir: &Path,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    ensure_primary_identity_in_context(
        identity_dir,
        IdentityProvisioningContext::NativeTilo,
        storage,
    )
}

fn ensure_primary_identity_in_context(
    identity_dir: &Path,
    context: IdentityProvisioningContext,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    let path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    if path.exists() {
        if context != IdentityProvisioningContext::LegacyLinuxHost {
            protect_identity_record(identity_dir, storage)?;
        }
        return Ok(ProvisionOutcome::Existing);
    }

    // Mint through the same encoder the interactive ceremony uses, so
    // there's exactly one writer of the on-disk record — the two provisioning paths can't drift apart in what they store.
    let mut rng = UnwrapErr(SysRng);
    let identity = ThresholdIdentity::bootstrap_single_device(&mut rng)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let pubkey_hex = hex::encode(identity.group_public_key().as_bytes());
    let display_name = match context {
        IdentityProvisioningContext::NativeTilo => String::new(),
        IdentityProvisioningContext::LegacyLinuxHost => login_account_display_name(),
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        IdentityProvisioningContext::ThrowawayTest => THROWAWAY_TEST_IDENTITY_NAME.to_owned(),
    };
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        &display_name,
        [128, 128, 128],
        std::time::SystemTime::now(),
    )
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let bytes = record
        .to_json_bytes()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    import_primary_identity(identity_dir, &bytes, storage)?;
    Ok(ProvisionOutcome::Created { pubkey_hex })
}

/// Provision the complete identity chain used by collaboration.
///
/// The personal identity remains protected by the runtime identity provider.
/// It signs the certificate for a distinct per-device key without exporting
/// its private share into the system identity tree. That device key is the
/// single transport identity used by the shell, filesystem synchroniser, and
/// hosted relay. Existing records are migrated idempotently; an existing
/// device key is never replaced.
///
/// # Errors
///
/// Returns an error if the identity directory can't be created or its
/// permissions set, if the primary identity can't be provisioned or loaded,
/// if it isn't a single-device identity, if the device key or certificate
/// can't be created, loaded, verified, or signed, or if the identity
/// registry can't be migrated or saved.
pub fn ensure_collaboration_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::NativeTilo,
        storage,
        DeviceClass::Native,
    )
}

/// [`ensure_collaboration_identity`] for a Tilo runtime on an operating system
/// Tilo does not control. Its device certificates are marked foreign, so
/// workspace policy caps its role; a native certificate left on disk by an
/// earlier build is re-issued as foreign for the same device key.
///
/// # Errors
///
/// As [`ensure_collaboration_identity`].
pub fn ensure_foreign_collaboration_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::NativeTilo,
        storage,
        DeviceClass::Foreign,
    )
}

/// Provision collaboration identity for Tilo Desktop installed on an existing
/// Linux distribution.
///
/// A newly minted identity is labelled from the current passwd entry so it is
/// visibly associated with the already-authenticated Unix user. An existing
/// identity record is used as-is: this compatibility path does not introduce
/// another login, password, or storage migration.
///
/// # Errors
///
/// Returns an error if the identity directory cannot be created or its
/// permissions set, or if the identity and its certified device key cannot be
/// provisioned, loaded, verified, migrated, or saved.
pub fn ensure_collaboration_identity_for_legacy_linux_host(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::LegacyLinuxHost,
        storage,
        DeviceClass::Native,
    )
}

/// Paths for installing a joined device onto a target tree from this machine.
pub struct JoinedDeviceLayout<'a> {
    /// Filesystem root of the system being set up.
    pub target_root: &'a Path,
    /// Account whose session will present the enrolled person.
    pub username: &'a str,
    /// This machine's per-user identity directory.
    pub local_identity_dir: &'a Path,
    /// This machine's system identity paths, including its device certificate.
    pub local_system: &'a IdentityPaths,
    /// This machine's `home-peers.json`, copied when it is present.
    pub local_home_peers: &'a Path,
    /// This machine's exported workspace room ids, copied when the file exists.
    pub local_rooms: &'a Path,
}

/// Certify a new device onto `layout.target_root` and make both sides accept it.
///
/// The human root secret is never written. The target receives a public
/// identity record, a fleet that includes this machine, and this machine's
/// home peers. This machine's fleet remembers the new device, so each side
/// can admit the other when they meet.
///
/// # Errors
///
/// Returns an error if the account name is unsafe, this machine's device
/// certificate cannot be read, or any target file cannot be written.
pub fn install_joined_device(
    layout: &JoinedDeviceLayout<'_>,
    user_public_key: [u8; 32],
    display_name: &str,
    device_label: &str,
    now_ns: u64,
    sign: impl FnOnce(&[u8]) -> Result<[u8; 64], String>,
) -> Result<DeviceCert, io::Error> {
    if !safe_account_name(layout.username) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target account name is not a single safe path component",
        ));
    }
    let identity_id = hex::encode(user_public_key);
    let local_cert = persist::read_device_cert(&layout.local_system.device_cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if local_cert.user_public_key != user_public_key {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "this machine's device certificate belongs to a different person",
        ));
    }
    let target_system = IdentityPaths::under(layout.target_root.join("var/lib/tilo"));
    let cert = persist::provision_joined_device(
        &target_system,
        user_public_key,
        sign,
        device_label,
        now_ns,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;

    let identity_dir = layout
        .target_root
        .join("var/home")
        .join(layout.username)
        .join(".config/tilo/identity");
    prepare_identity_directory(&identity_dir)?;
    let record_id = public_record_id(&identity_id);
    let record = runtime_identity::public_member_record(
        if display_name.trim().is_empty() {
            "My Tilo identity"
        } else {
            display_name.trim()
        },
        [88, 101, 242],
        user_public_key,
    )?;
    tilo_files::native::atomic::write_with_mode(
        &identity_dir.join(format!("{record_id}.json")),
        &record,
        0o600,
    )?;
    tilo_files::native::atomic::write_with_mode(
        &identity_dir.join("active"),
        format!("{record_id}\n").as_bytes(),
        0o600,
    )?;

    let local_store = crate::device_fleet::DeviceFleetStore::at(
        layout.local_identity_dir,
        layout.local_system.clone(),
    );
    let mut certificates = local_store.load(&identity_id)?.certificates;
    if !certificates
        .iter()
        .any(|known| known.device_public_key == local_cert.device_public_key)
    {
        certificates.push(local_cert.clone());
    }
    certificates.push(cert.clone());
    let target_store =
        crate::device_fleet::DeviceFleetStore::at(&identity_dir, target_system.clone());
    for known in certificates {
        target_store.remember_certificate(&identity_id, known)?;
    }
    local_store.remember_certificate(&identity_id, cert.clone())?;
    let local_device = persist::read_device_key(&layout.local_system.device_privkey)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if local_device.verifying_key().to_bytes() != local_cert.device_public_key {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "this machine's device key does not match its certificate",
        ));
    }
    let relay_service = tilo_relay_protocol::relay_service_node_id(&local_device.to_bytes());
    copy_home_peers(
        layout.local_home_peers,
        &target_system,
        &local_cert,
        relay_service,
        cert.device_public_key,
    )?;
    let rooms_dest = target_system
        .user_pubkey
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target identity root is missing",
            )
        })?
        .join("personal-rooms.json");
    copy_personal_rooms(layout.local_rooms, &rooms_dest)?;
    Ok(cert)
}

/// Certify a new device using this machine's active personal identity.
///
/// Loads the active record under `layout.local_identity_dir`, signs with that
/// person's non-exportable share, and otherwise matches
/// [`install_joined_device`].
///
/// # Errors
///
/// Returns an error if the active identity cannot be loaded or signing fails.
pub fn enroll_from_local_identity(
    layout: &JoinedDeviceLayout<'_>,
    display_name: &str,
    device_label: &str,
    now_ns: u64,
) -> Result<DeviceCert, io::Error> {
    let path =
        runtime_identity::resolve_active_identity(layout.local_identity_dir).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "this machine has no active personal identity to enroll",
            )
        })?;
    let identity = runtime_identity::load_from_path(path).map_err(identity_load_error)?;
    if !identity.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "threshold identity requires the multi-device certificate ceremony",
        ));
    }
    let user_public_key = identity.group_public_key();
    install_joined_device(
        layout,
        user_public_key,
        display_name,
        device_label,
        now_ns,
        |digest| identity.sign(digest).map_err(|error| error.to_string()),
    )
}

fn public_record_id(identity_id: &str) -> String {
    let prefix: String = identity_id.chars().take(55).collect();
    format!("identity-{prefix}")
}

fn safe_account_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes
            .first()
            .is_some_and(|byte| matches!(byte, b'a'..=b'z' | b'_'))
        && bytes
            .iter()
            .skip(1)
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn valid_room_id(value: &str) -> bool {
    let mut parts = value.split('-');
    let expected = [8, 4, 4, 4, 12];
    expected.into_iter().all(|len| {
        parts.next().is_some_and(|part| {
            part.len() == len && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }) && parts.next().is_none()
}

fn copy_personal_rooms(source: &Path, dest: &Path) -> io::Result<()> {
    let bytes = match fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if bytes.len() > 64 * 1024 {
        return Ok(());
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return Ok(());
    };
    if ids.len() > 256 || ids.iter().any(|id| !valid_room_id(id)) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(
        dest,
        &serde_json::to_vec_pretty(&ids)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    )?;
    Ok(())
}

/// Where this session publishes the workspace rooms a newly enrolled device
/// should join. Absent when the process has no home or state directory.
#[must_use]
pub fn personal_rooms_export_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("tilo").join("personal-rooms.json"))
}

fn relay_is_dialable(
    entries: &[tilo_relay_protocol::HomePeerRegistration],
    registration: &tilo_relay_protocol::HomePeerRegistration,
) -> bool {
    entries.iter().any(|entry| {
        entry.device_node_id == registration.device_node_id
            && entry.relay_service_node_id == registration.relay_service_node_id
            && registration
                .relay_workspace_ids
                .iter()
                .all(|topic| entry.relay_workspace_ids.contains(topic))
    })
}

fn copy_home_peers(
    source: &Path,
    target_system: &IdentityPaths,
    local_cert: &DeviceCert,
    relay_service_node_id: [u8; 32],
    joined_device: [u8; 32],
) -> io::Result<()> {
    let identity_id = hex::encode(local_cert.user_public_key);
    let bytes = match fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "home-peer registry exceeds 1 MiB",
        ));
    }
    let mut entries: Vec<tilo_relay_protocol::HomePeerRegistration> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    // Clients reject a relay whose certificate cannot act as a host. A normal
    // setup machine is dialed directly instead; only a host-capable machine is
    // recorded as a relay.
    if local_cert.capabilities.can_act_as_host {
        let device_id = hex::encode(local_cert.device_public_key);
        let service_id = hex::encode(relay_service_node_id);
        let topic =
            tilo_relay_protocol::personal_continuity_workspace_id(&local_cert.user_public_key);
        let registration = tilo_relay_protocol::HomePeerRegistration {
            schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: device_id,
            relay_service_node_id: service_id,
            relay_endpoint: None,
            label: if local_cert.device_label.trim().is_empty() {
                "Setup machine".into()
            } else {
                local_cert.device_label.trim().to_string()
            },
            user_public_key: identity_id.clone(),
            device_cert: Some(local_cert.clone()),
            relay_workspace_ids: vec![topic],
        };
        if !relay_is_dialable(&entries, &registration) {
            entries.push(registration);
        }
        let local_encoded = serde_json::to_vec_pretty(&entries)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent)?;
        }
        tilo_files::native::atomic::write(source, &local_encoded)?;
    }
    let own: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.user_public_key == identity_id)
        .collect();
    let encoded = serde_json::to_vec_pretty(&own)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let parent = target_system
        .user_pubkey
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target identity root is missing",
            )
        })?;
    fs::create_dir_all(parent)?;
    tilo_files::native::atomic::write(&parent.join("home-peers.json"), &encoded)?;
    remember_personal_device(
        &parent.join("personal-device-peers.json"),
        local_cert.device_public_key,
    )?;
    let local_peers = source.with_file_name("personal-device-peers.json");
    remember_personal_device(&local_peers, joined_device)?;
    Ok(())
}

fn remember_personal_device(path: &Path, device_public_key: [u8; 32]) -> io::Result<()> {
    let id = hex::encode(device_public_key);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personal device list exceeds 64 KiB",
        ));
    }
    let mut ids: Vec<String> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    if ids.len() > 128
        || ids.iter().any(|known| {
            known.len() != 64
                || hex::decode(known)
                    .ok()
                    .is_none_or(|decoded| decoded.len() != 32)
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personal device list is invalid",
        ));
    }
    if !ids.iter().any(|known| known == &id) {
        ids.push(id);
    }
    let encoded = serde_json::to_vec_pretty(&ids)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    tilo_files::native::atomic::write(path, &encoded)?;
    Ok(())
}

/// Display name of an identity minted by [`ensure_throwaway_test_identity`].
pub const THROWAWAY_TEST_IDENTITY_NAME: &str = "Throwaway test identity";

/// Present beside the identity record when that identity is a throwaway
/// minted by [`ensure_throwaway_test_identity`].
pub const THROWAWAY_TEST_IDENTITY_MARKER: &str = "throwaway-test-identity";

/// Whether the identity in `identity_dir` is a throwaway test identity.
#[must_use]
pub fn is_throwaway_test_identity(identity_dir: &Path) -> bool {
    identity_dir.join(THROWAWAY_TEST_IDENTITY_MARKER).is_file()
}

/// Provision the collaboration identity chain around a disposable identity
/// named [`THROWAWAY_TEST_IDENTITY_NAME`] and marked with
/// [`THROWAWAY_TEST_IDENTITY_MARKER`], for a test session on a dev image.
///
/// Never replaces or adopts a real identity: an identity without the marker
/// is refused. Idempotent for a throwaway identity it minted earlier.
///
/// # Errors
///
/// [`io::ErrorKind::Unsupported`], touching nothing, in a release build
/// without the `throwaway-test-identity` feature;
/// [`io::ErrorKind::AlreadyExists`] when the directory already holds an
/// identity that is not a throwaway one; otherwise as
/// [`ensure_collaboration_identity`].
pub fn ensure_throwaway_test_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    {
        mint_throwaway_test_identity(identity_dir, paths, storage)
    }
    #[cfg(not(any(debug_assertions, feature = "throwaway-test-identity")))]
    {
        let _ = (identity_dir, paths, storage);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "throwaway test identities exist only in dev builds",
        ))
    }
}

#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn mint_throwaway_test_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    prepare_identity_directory(identity_dir)?;
    let marker = identity_dir.join(THROWAWAY_TEST_IDENTITY_MARKER);
    if runtime_identity::resolve_active_identity(identity_dir).is_some() {
        if !marker.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "this user already has an identity that is not a throwaway test identity",
            ));
        }
    } else {
        // The marker goes first: a marker without an identity is harmless,
        // an unmarked throwaway would be refused from then on.
        fs::write(&marker, b"")?;
    }
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::ThrowawayTest,
        storage,
        DeviceClass::Native,
    )
}

/// Prepare the private per-user identity directory before either the key
/// transaction or its first-use crash journal is written. Keeping this as one
/// operation prevents the journal preflight and the identity writer from
/// disagreeing about directory permissions.
/// # Errors
///
/// Rejects a symlink or a directory owned by another user, and returns any
/// error creating it or setting its permissions.
pub fn prepare_identity_directory(identity_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(identity_dir)?;
    let metadata = fs::symlink_metadata(identity_dir)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity directory must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity directory must be owned by this user",
            ));
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(identity_dir, permissions)?;
    }
    Ok(())
}

fn ensure_collaboration_identity_in_context(
    identity_dir: &Path,
    paths: &IdentityPaths,
    context: IdentityProvisioningContext,
    storage: IdentityStorage,
    class: DeviceClass,
) -> Result<ProvisionOutcome, io::Error> {
    prepare_identity_directory(identity_dir)?;

    match persist::joined_device_is_ready(paths, now_ns_u64()) {
        Ok(true) => return Ok(ProvisionOutcome::Existing),
        Ok(false) => {}
        Err(error) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("pre-enrolled device identity is not usable: {error}"),
            ));
        }
    }

    let primary_path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    let (selected_path, outcome) =
        if let Some(path) = runtime_identity::resolve_active_identity(identity_dir) {
            if context != IdentityProvisioningContext::LegacyLinuxHost {
                protect_identity_record_at(identity_dir, &path, storage)?;
            }
            (path, ProvisionOutcome::Existing)
        } else {
            let outcome = ensure_primary_identity_in_context(identity_dir, context, storage)?;
            (primary_path, outcome)
        };

    let signer = runtime_identity::load_from_path(selected_path).map_err(identity_load_error)?;
    if !signer.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "threshold identity requires the multi-device certificate ceremony",
        ));
    }
    paths.ensure_directories()?;

    let root_public = signer.group_public_key();
    // The first identity to claim the machine owns its legacy system-facing
    // certificate. Later OS users keep their own root and receive a separate
    // identity-keyed certificate for the same device key.
    let device_owner = claim_device_owner(&paths.user_pubkey, &root_public)?;

    let (device, _) =
        persist::load_or_create_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    if device_owner && paths.device_cert.exists() {
        let cert = persist::read_device_cert(&paths.device_cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        tilo_user_identity::verify_device_cert(&cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if cert.user_public_key != root_public
            || cert.device_public_key != device.verifying_key().to_bytes()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "device certificate does not match the provisioned identity keys",
            ));
        }
        if class.must_reissue(&cert) {
            let cert = issue_device_certificate(
                &signer,
                cert.device_public_key,
                &cert.device_label,
                class,
            )?;
            persist::write_device_cert(&paths.device_cert, &cert)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
    } else if device_owner {
        let cert = issue_device_certificate(
            &signer,
            device.verifying_key().to_bytes(),
            "Primary device",
            class,
        )?;
        persist::write_device_cert(&paths.device_cert, &cert)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    // Keep an identity-keyed copy as well. A device has one keypair, but it
    // needs a separate root-signed certificate for every identity it may
    // present in a workspace.
    ensure_device_certificate_for_identity(
        &signer,
        paths,
        if device_owner {
            "Primary device"
        } else {
            "Shared device"
        },
        class,
    )?;

    // Register the primary record in the multi-identity registry. A registry
    // write failure is a provisioning failure: silently creating a key that no
    // workspace can select strands the identity immediately.
    let registry = crate::registry::IdentityRegistryStore::at(identity_dir).load_reconciled()?;
    if let Some(identity_id) = registry.default_identity {
        crate::device_fleet::DeviceFleetStore::at(identity_dir, paths.clone())
            .register_current_device(&identity_id)?;
    }

    Ok(outcome)
}

/// Atomically elect the legacy system-facing identity owner.
///
/// The owner key is shared by every OS user on a machine.  An `exists` check
/// followed by a normal write lets two first logins both observe an empty path
/// and lets the later write replace the first owner's key.  Write the complete
/// candidate to a same-directory temporary file, then hard-link it into its
/// final name: link creation is an atomic no-clobber claim on the target
/// directory entry.  A competing caller reads (and therefore validates) the
/// winner rather than ever replacing it.
///
/// A hard link is deliberately used instead of `rename`: `rename` atomically
/// replaces an existing target, which is exactly the election race we must
/// avoid.  Tilo stores this identity tree on a local Linux filesystem, where
/// hard links are available and both paths share a directory/filesystem.
fn claim_device_owner(path: &Path, root_public: &[u8; 32]) -> io::Result<bool> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "system identity public-key path has no parent",
        )
    })?;

    let temp = create_owner_claim_temp(parent, root_public)?;
    let claim = fs::hard_link(&temp, path);
    // The final path is the claim.  The temp link contains no secret and is
    // removed in all outcomes; a removal error must not obscure a successful
    // (or failed) election result.
    let _ = fs::remove_file(&temp);

    match claim {
        Ok(()) => {
            // `hard_link` made the complete, synced file visible.  Sync its
            // parent before treating the election as successful, so the
            // winner's directory entry survives a sudden power loss.
            fs::File::open(parent)?.sync_all()?;
            // Validate our own published winner too: a filesystem anomaly or
            // future refactor must fail closed instead of issuing a legacy
            // certificate against an unverified owner record.
            let stored = read_claimed_owner(path)?;
            if stored != *root_public {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "atomic owner claim did not contain the claimed public key",
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // Someone else won.  Preserve the existing owner exactly as the
            // old sequential path did, but validate it before deciding this
            // identity is a non-owner.
            Ok(read_claimed_owner(path)? == *root_public)
        }
        Err(error) => Err(error),
    }
}

/// Create and synchronously fill a uniquely named same-directory temporary
/// claim file.  The bounded retry is only for stale files left by a killed
/// process; the process-local sequence makes normal collisions impossible.
fn create_owner_claim_temp(parent: &Path, root_public: &[u8; 32]) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let sequence = OWNER_CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(
            ".tilo-owner-claim-{}-{sequence}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(root_public).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&temp);
                    return Err(error);
                }
                return Ok(temp);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an owner-claim temporary file",
    ))
}

fn read_claimed_owner(path: &Path) -> io::Result<[u8; 32]> {
    persist::read_public_key(path).map_err(|error| io::Error::other(error.to_string()))
}

/// Enrol the machine's existing device key under an imported local identity.
///
/// This keeps app signing and collaboration transport authentication on the
/// same `identity -> device` chain immediately after setup/import.
///
/// # Errors
///
/// Returns an error if the record at `record_path` can't be loaded, or if
/// the device key or its certificate can't be created, loaded, verified, or
/// signed.
pub fn ensure_device_certificate_for_record(
    record_path: &Path,
    paths: &IdentityPaths,
) -> Result<PathBuf, io::Error> {
    let identity = runtime_identity::load_from_path(record_path.to_path_buf())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    ensure_device_certificate_for_identity(&identity, paths, "This device", DeviceClass::Native)
}

fn ensure_device_certificate_for_identity(
    identity: &runtime_identity::SigningIdentity,
    paths: &IdentityPaths,
    device_label: &str,
    class: DeviceClass,
) -> Result<PathBuf, io::Error> {
    let (device, _) =
        persist::load_or_create_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    let user_public_key = identity.group_public_key();
    let cert_path = paths.device_cert_for_user(&user_public_key);
    if cert_path.exists() {
        let cert = persist::read_device_cert(&cert_path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        tilo_user_identity::verify_device_cert(&cert)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if cert.user_public_key != user_public_key
            || cert.device_public_key != device.verifying_key().to_bytes()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity-specific device certificate does not match its keys",
            ));
        }
        if !class.must_reissue(&cert) {
            return Ok(cert_path);
        }
    }

    let cert = issue_device_certificate(
        identity,
        device.verifying_key().to_bytes(),
        device_label,
        class,
    )?;
    persist::write_device_cert(&cert_path, &cert).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(cert_path)
}

fn issue_device_certificate(
    identity: &runtime_identity::SigningIdentity,
    device_public_key: [u8; 32],
    device_label: &str,
    class: DeviceClass,
) -> Result<DeviceCert, io::Error> {
    let mut cert = DeviceCert {
        user_public_key: identity.group_public_key(),
        device_public_key,
        device_label: device_label.into(),
        capabilities: class.capabilities(),
        issued_at_ns: now_ns_u64(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    let digest = tilo_user_identity::device_cert_signing_digest(&cert)
        .map_err(|e| io::Error::other(e.to_string()))?;
    cert.signature = identity
        .sign(&digest)
        .map_err(|e| io::Error::other(e.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(cert)
}

/// Verify the complete personal-root/device chain without changing it.
///
/// Login uses this after the human-facing ceremony. Every persisted secret,
/// public key and certificate must describe the same root and device, and the
/// certificate must be valid at `now_ns`. Missing, malformed, weakly-permissioned
/// or mismatched state is an error; this function never creates or repairs it.
///
/// # Errors
///
/// Returns an I/O error when any identity artifact is missing, insecure,
/// malformed, expired, or inconsistent with the root/device chain.
pub fn verify_collaboration_identity(
    identity_dir: &Path,
    paths: &IdentityPaths,
    now_ns: u64,
) -> Result<(), io::Error> {
    // Imports are stored under a fingerprinted filename and selected through
    // `active`; legacy identities remain at `primary.json`. Verification must
    // follow the same resolver as provisioning/runtime startup or a freshly
    // imported host identity is created successfully and then immediately
    // reported missing.
    let primary_path = runtime_identity::resolve_active_identity(identity_dir)
        .unwrap_or_else(|| identity_dir.join(PRIMARY_IDENTITY_FILE));
    #[cfg(unix)]
    {
        let dir_metadata = fs::metadata(identity_dir)?;
        if !dir_metadata.is_dir() || dir_metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "personal identity directory must not be accessible by group or other users",
            ));
        }
        let primary_metadata = fs::symlink_metadata(&primary_path)?;
        if !primary_metadata.file_type().is_file()
            || primary_metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "primary identity must be a regular file with mode 0600",
            ));
        }
    }
    let identity = runtime_identity::load_from_path(primary_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if !identity.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "threshold identity requires the multi-device certificate ceremony",
        ));
    }
    let root_public = persist::read_public_key(&paths.user_pubkey)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let device = persist::load_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|e| io::Error::other(e.to_string()))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns)
        .map_err(|e| io::Error::other(e.to_string()))?;

    let expected_root = identity.group_public_key();
    if root_public != expected_root || cert.user_public_key != expected_root {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "personal identity and persisted certification root do not match",
        ));
    }
    if cert.device_public_key != device.verifying_key().to_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "certified device key does not match the persisted device secret",
        ));
    }
    Ok(())
}

/// Resolve the system identity root, with the same test/development override
/// used by the filesystem synchroniser, and this session's device-key
/// custody ([`IdentityStorage::for_session`]).
pub fn collaboration_identity_paths() -> IdentityPaths {
    std::env::var_os("TILO_IDENTITY_ROOT")
        .filter(|value| !value.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under)
        .with_device_key_custody(IdentityStorage::for_session().device_key_custody())
}

fn import_primary_identity(
    identity_dir: &Path,
    bytes: &[u8],
    storage: IdentityStorage,
) -> io::Result<PathBuf> {
    match storage {
        IdentityStorage::Files => {
            let provider = runtime_identity::BrokerFileProvider::new(identity_dir);
            runtime_identity::import_identity_with_provider(
                identity_dir,
                "primary",
                bytes,
                &provider,
            )
        }
        IdentityStorage::Platform => {
            runtime_identity::import_identity(identity_dir, "primary", bytes)
        }
    }
}

fn protect_identity_record(identity_dir: &Path, storage: IdentityStorage) -> io::Result<()> {
    protect_identity_record_at(
        identity_dir,
        &identity_dir.join(PRIMARY_IDENTITY_FILE),
        storage,
    )
}

fn protect_identity_record_at(
    identity_dir: &Path,
    record_path: &Path,
    storage: IdentityStorage,
) -> io::Result<()> {
    let id = record_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "identity has no valid id"))?;
    match storage {
        IdentityStorage::Files => {
            let provider = runtime_identity::BrokerFileProvider::new(identity_dir);
            runtime_identity::secure_identity_with_provider(identity_dir, id, &provider)?;
        }
        IdentityStorage::Platform => {
            runtime_identity::secure_identity(identity_dir, id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: IdentityStorage = IdentityStorage::Files;

    #[test]
    fn prejoined_device_keeps_the_setup_identity() {
        use ed25519_dalek::Signer;

        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let dir = tempfile::tempdir().unwrap();
        let identity_dir = dir.path().join("identity");
        let paths = IdentityPaths::under(dir.path().join("system"));
        persist::provision_joined_device(
            &paths,
            root.verifying_key().to_bytes(),
            |digest| Ok(root.sign(digest).to_bytes()),
            "orange-pi",
            1_000,
        )
        .unwrap();

        let outcome = ensure_collaboration_identity(&identity_dir, &paths).unwrap();
        assert_eq!(outcome, ProvisionOutcome::Existing);
        assert!(!identity_dir.join(PRIMARY_IDENTITY_FILE).exists());
        assert!(!paths.user_privkey.exists());
        assert!(persist::joined_device_is_ready(&paths, now_ns_u64()).unwrap());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one enrollment scenario keeps the two fleets, the copied rooms, and the absent root secret together"
    )]
    fn installed_device_joins_the_setup_fleet_without_the_root_secret() {
        use ed25519_dalek::Signer;
        use tilo_relay_protocol::HomePeerRegistration;

        let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let local_device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let user = root.verifying_key().to_bytes();
        let identity_id = hex::encode(user);
        let dir = tempfile::tempdir().unwrap();
        let local_system = IdentityPaths::under(dir.path().join("local-system"));
        local_system.ensure_directories().unwrap();
        let mut local_cert = DeviceCert {
            user_public_key: user,
            device_public_key: local_device.verifying_key().to_bytes(),
            device_label: "setup".into(),
            capabilities: DeviceCapabilities {
                can_own_workspaces: true,
                can_admit_devices: true,
                can_act_as_host: false,
                can_read_all_workspaces: true,
                foreign: false,
            },
            issued_at_ns: 1_000,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut local_cert, &root).unwrap();
        persist::write_device_cert(&local_system.device_cert, &local_cert).unwrap();
        persist::write_device_key(&local_system.device_privkey, &local_device).unwrap();
        let peers = dir.path().join("home-peers.json");
        let own = HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: hex::encode(local_cert.device_public_key),
            relay_service_node_id: hex::encode([7_u8; 32]),
            relay_endpoint: None,
            label: "home".into(),
            user_public_key: identity_id.clone(),
            device_cert: None,
            relay_workspace_ids: vec![1],
        };
        let other = HomePeerRegistration {
            user_public_key: hex::encode([9_u8; 32]),
            label: "other".into(),
            ..own.clone()
        };
        std::fs::write(
            &peers,
            serde_json::to_vec_pretty(&vec![own, other]).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("personal-rooms.json"),
            br#"["11111111-1111-4111-8111-111111111111"]"#,
        )
        .unwrap();

        let cert = install_joined_device(
            &JoinedDeviceLayout {
                target_root: dir.path(),
                username: "quser",
                local_identity_dir: &dir.path().join("local-identity"),
                local_system: &local_system,
                local_home_peers: &peers,
                local_rooms: &dir.path().join("personal-rooms.json"),
            },
            user,
            "ada",
            "orange-pi",
            1_000,
            |digest| Ok(root.sign(digest).to_bytes()),
        )
        .unwrap();

        assert_ne!(
            cert.device_public_key, local_cert.device_public_key,
            "joined device must not reuse the setup machine's device key"
        );
        let target_system = IdentityPaths::under(dir.path().join("var/lib/tilo"));
        assert!(persist::joined_device_is_ready(&target_system, now_ns_u64()).unwrap());
        assert!(!target_system.user_privkey.exists());
        let identity_dir = dir.path().join("var/home/quser/.config/tilo/identity");
        let registry = crate::registry::IdentityRegistryStore::at(&identity_dir)
            .load_reconciled()
            .unwrap();
        assert_eq!(
            registry.default_identity.as_deref(),
            Some(identity_id.as_str())
        );
        let record =
            std::fs::read(&registry.identities.get(&identity_id).unwrap().record_path).unwrap();
        assert!(!record.windows(32).any(|window| window == root.to_bytes()));
        let target_fleet = crate::device_fleet::DeviceFleetStore::at(&identity_dir, target_system)
            .load(&identity_id)
            .unwrap();
        assert!(
            target_fleet
                .certificates
                .iter()
                .any(|item| { item.device_public_key == cert.device_public_key })
        );
        assert!(
            target_fleet
                .certificates
                .iter()
                .any(|item| { item.device_public_key == local_cert.device_public_key })
        );
        let local_fleet = crate::device_fleet::DeviceFleetStore::at(
            dir.path().join("local-identity"),
            local_system,
        )
        .load(&identity_id)
        .unwrap();
        assert!(
            local_fleet
                .certificates
                .iter()
                .any(|item| { item.device_public_key == cert.device_public_key })
        );
        let copied: Vec<HomePeerRegistration> = serde_json::from_slice(
            &std::fs::read(dir.path().join("var/lib/tilo/home-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(copied[0].user_public_key, identity_id);
        let service = hex::encode(tilo_relay_protocol::relay_service_node_id(
            &local_device.to_bytes(),
        ));
        assert!(
            copied
                .iter()
                .all(|entry| entry.relay_service_node_id != service),
            "a machine that cannot host is not recorded as a relay"
        );
        let direct: Vec<String> = serde_json::from_slice(
            &std::fs::read(dir.path().join("var/lib/tilo/personal-device-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(direct, vec![hex::encode(local_cert.device_public_key)]);
        let local_direct: Vec<String> = serde_json::from_slice(
            &std::fs::read(peers.with_file_name("personal-device-peers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(local_direct, vec![hex::encode(cert.device_public_key)]);
        let rooms: Vec<String> = serde_json::from_slice(
            &std::fs::read(dir.path().join("var/lib/tilo/personal-rooms.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(rooms, vec!["11111111-1111-4111-8111-111111111111"]);
        let peers_bytes = std::fs::read(dir.path().join("var/lib/tilo/home-peers.json")).unwrap();
        assert!(
            !peers_bytes
                .windows(32)
                .any(|window| window == local_device.to_bytes()),
            "the setup machine's device secret must stay off the target"
        );
        let local_registry: Vec<HomePeerRegistration> =
            serde_json::from_slice(&std::fs::read(&peers).unwrap()).unwrap();
        assert!(
            local_registry
                .iter()
                .any(|entry| entry.user_public_key == hex::encode([9_u8; 32]))
        );

        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", dir.path().join("var/home/quser/.config"));
        env.set("TILO_IDENTITY_ROOT", dir.path().join("var/lib/tilo"));
        crate::certificates::authorize_join("workspace", &local_cert)
            .expect("the setup machine is admitted for remote actions");
        let stranger_device = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
        let mut stranger = local_cert.clone();
        stranger.device_public_key = stranger_device.verifying_key().to_bytes();
        stranger.signature = [0; 64];
        tilo_user_identity::sign_device_cert(&mut stranger, &root).unwrap();
        assert!(
            crate::certificates::authorize_join("workspace", &stranger).is_err(),
            "a device that was not enrolled cannot invoke remote actions"
        );
    }

    #[test]
    fn enroll_from_local_identity_signs_with_the_active_person_not_a_copied_root() {
        use tilo_relay_protocol::HomePeerRegistration;

        let dir = tempfile::tempdir().unwrap();
        let local_identity = dir.path().join("local-identity");
        let local_system = IdentityPaths::under(dir.path().join("local-system"));
        ensure_collaboration_identity(&local_identity, &local_system).unwrap();
        let local_cert = persist::read_device_cert(&local_system.device_cert).unwrap();
        let identity_id = hex::encode(local_cert.user_public_key);
        let peers = dir.path().join("home-peers.json");
        std::fs::write(
            &peers,
            serde_json::to_vec_pretty(&[HomePeerRegistration {
                schema_version: HomePeerRegistration::CURRENT_SCHEMA,
                device_node_id: hex::encode(local_cert.device_public_key),
                relay_service_node_id: hex::encode([7_u8; 32]),
                relay_endpoint: None,
                label: "home".into(),
                user_public_key: identity_id,
                device_cert: None,
                relay_workspace_ids: vec![1],
            }])
            .unwrap(),
        )
        .unwrap();

        let cert = enroll_from_local_identity(
            &JoinedDeviceLayout {
                target_root: dir.path(),
                username: "quser",
                local_identity_dir: &local_identity,
                local_system: &local_system,
                local_home_peers: &peers,
                local_rooms: &dir.path().join("missing-rooms.json"),
            },
            "ada",
            "orange-pi",
            1_000,
        )
        .unwrap();

        assert_ne!(cert.device_public_key, local_cert.device_public_key);
        assert_eq!(cert.user_public_key, local_cert.user_public_key);
        let target_system = IdentityPaths::under(dir.path().join("var/lib/tilo"));
        assert!(persist::joined_device_is_ready(&target_system, now_ns_u64()).unwrap());
        assert!(!target_system.user_privkey.exists());
        assert!(cert.capabilities.can_read_all_workspaces);
    }
    fn ensure_primary_identity(identity_dir: &Path) -> Result<ProvisionOutcome, io::Error> {
        super::ensure_primary_identity(identity_dir, FILES)
    }
    fn ensure_collaboration_identity(
        identity_dir: &Path,
        paths: &IdentityPaths,
    ) -> Result<ProvisionOutcome, io::Error> {
        super::ensure_collaboration_identity(identity_dir, paths, FILES)
    }
    fn ensure_collaboration_identity_for_legacy_linux_host(
        identity_dir: &Path,
        paths: &IdentityPaths,
    ) -> Result<ProvisionOutcome, io::Error> {
        super::ensure_collaboration_identity_for_legacy_linux_host(identity_dir, paths, FILES)
    }

    use std::sync::{Arc, Barrier};

    #[test]
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    fn a_throwaway_test_identity_is_marked_and_never_replaces_a_real_one() {
        // Phase `a_throwaway_identity_is_named_and_marked`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            assert!(matches!(
                ensure_throwaway_test_identity(&personal, &paths, FILES).expect("provision"),
                ProvisionOutcome::Created { .. }
            ));
            assert!(is_throwaway_test_identity(&personal));
            let identity = runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("load throwaway identity");
            assert_eq!(identity.display_name(), THROWAWAY_TEST_IDENTITY_NAME);
            assert!(
                paths.device_cert.exists(),
                "the device chain is provisioned too"
            );
            // Phase `a_second_run_keeps_the_same_throwaway`.
            assert_eq!(
                ensure_throwaway_test_identity(&personal, &paths, FILES).expect("again"),
                ProvisionOutcome::Existing
            );
        }
        // Phase `a_real_identity_is_refused_and_left_untouched`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_collaboration_identity(&personal, &paths).expect("real identity");
            let record = personal.join(PRIMARY_IDENTITY_FILE);
            let before = fs::read(&record).unwrap();
            let error = ensure_throwaway_test_identity(&personal, &paths, FILES)
                .expect_err("a real identity must not be adopted as a throwaway");
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&record).unwrap(), before);
            assert!(!is_throwaway_test_identity(&personal));
        }
    }

    #[test]
    #[cfg(not(any(debug_assertions, feature = "throwaway-test-identity")))]
    fn a_release_build_refuses_a_throwaway_test_identity() {
        // Only compiled into a plain `--release` test run, the profile the
        // OS image's binaries are built with.
        let tmp = tempfile::tempdir().expect("tempdir");
        let personal = tmp.path().join("config/tilo/identity");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        let error = ensure_throwaway_test_identity(&personal, &paths, FILES)
            .expect_err("a release build must not mint a throwaway identity");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(!personal.exists(), "the refusal writes nothing");
        assert!(!paths.device_cert.exists());
    }

    /// Minimal tempdir helper (avoids pulling `tempfile` as a dev-dep — the only test in `quick-shell` that needs one).
    #[cfg(unix)]
    #[test]
    fn identity_files_are_private_and_refuse_symlinks() {
        // Phase `identity_directory_preflight_refuses_a_symlink`.
        {
            use std::os::unix::fs::symlink;

            let tmp = tempfile::tempdir().expect("tempdir");
            let target = tmp.path().join("target");
            fs::create_dir(&target).unwrap();
            let linked = tmp.path().join("linked");
            symlink(&target, &linked).unwrap();
            assert!(prepare_identity_directory(&linked).is_err());
        }
        // Phase `collaboration_verifier_rejects_a_permissive_primary_secret`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_collaboration_identity(&personal, &paths).unwrap();

            let primary = personal.join(PRIMARY_IDENTITY_FILE);
            std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o640)).unwrap();
            let error = verify_collaboration_identity(&personal, &paths, u64::MAX)
                .expect_err("a group-readable personal seed must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        // Phase `ensure_primary_identity_writes_a_private_file`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let identity_dir = tmp.path().join("tilo").join("identity");
            ensure_primary_identity(&identity_dir).expect("provision identity");
            let identity_file = identity_dir.join(PRIMARY_IDENTITY_FILE);
            let meta = std::fs::metadata(&identity_file).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
        }
    }

    #[test]
    fn primary_identity_is_created_once_and_legacy_hosts_are_associated_not_relabelled() {
        // Phase `ensure_primary_identity_reports_existing_vs_created`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let dir = tmp.path().join("tilo").join("identity");
            // Fresh run creates.
            match ensure_primary_identity(&dir).expect("provision ok") {
                ProvisionOutcome::Created { pubkey_hex } => {
                    assert_eq!(pubkey_hex.len(), 64, "hex of 32 bytes is 64 chars");
                }
                other @ ProvisionOutcome::Existing => {
                    panic!("expected Created on empty dir, got {other:?}")
                }
            }
            let identity = runtime_identity::load_from_path(dir.join(PRIMARY_IDENTITY_FILE))
                .expect("load created identity");
            assert_eq!(
                identity.display_name(),
                "",
                "native Tilo must not infer personal identity from its Unix implementation account"
            );
            // Second run leaves it alone.
            assert_eq!(
                ensure_primary_identity(&dir).expect("provision ok (2nd)"),
                ProvisionOutcome::Existing
            );
        }
        // Phase `legacy_linux_host_associates_a_fresh_identity_with_the_unix_account`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));

            assert!(matches!(
                ensure_collaboration_identity_for_legacy_linux_host(&personal, &paths)
                    .expect("provision host identity"),
                ProvisionOutcome::Created { .. }
            ));
            let identity = runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("load host identity");
            assert_eq!(identity.display_name(), login_account_display_name());
        }
        // Phase `legacy_linux_host_does_not_relabel_or_migrate_an_existing_identity`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_primary_identity(&personal).expect("seed existing identity");
            let record_path = personal.join(PRIMARY_IDENTITY_FILE);
            let before = std::fs::read(&record_path).expect("read identity before host setup");

            assert_eq!(
                ensure_collaboration_identity_for_legacy_linux_host(&personal, &paths)
                    .expect("associate existing host identity"),
                ProvisionOutcome::Existing
            );
            assert_eq!(
                std::fs::read(record_path).expect("read identity after host setup"),
                before,
                "the legacy-host compatibility path must use an existing identity as-is"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_host_resumes_partial_identity_beneath_atomic_fedora_home_link() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let real_home = tmp.path().join("var/home/alice");
        let personal = real_home.join(".config/tilo/identity");
        let identity_root = real_home.join(".local/share/tilo");
        std::fs::create_dir_all(&real_home).unwrap();
        symlink(tmp.path().join("var/home"), tmp.path().join("home")).unwrap();

        // This is the exact durable state left by the former ELOOP: the
        // personal record and owner claim landed, but reading the device key
        // through /home -> /var/home failed before cert creation.
        ensure_primary_identity(&personal).expect("seed personal identity");
        let signer = runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
            .expect("load seeded identity");
        let linked_root = tmp.path().join("home/alice/.local/share/tilo");
        let paths = IdentityPaths::under(&linked_root);
        paths.ensure_directories().unwrap();
        persist::write_public_key(&paths.user_pubkey, &signer.group_public_key()).unwrap();
        assert!(!paths.device_privkey.exists());

        assert_eq!(
            ensure_collaboration_identity_for_legacy_linux_host(&personal, &paths)
                .expect("resume the partial host identity"),
            ProvisionOutcome::Existing
        );
        let cert = persist::read_device_cert(&paths.device_cert).expect("read completed cert");
        verify_collaboration_identity(&personal, &paths, cert.issued_at_ns)
            .expect("resumed identity chain verifies");
        assert_eq!(
            runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("reload personal identity")
                .group_public_key(),
            signer.group_public_key(),
            "resuming must never mint a replacement personal identity"
        );
        assert!(identity_root.join("device-private/keypair.bin").is_file());
    }

    fn device_certificates(paths: &IdentityPaths, personal: &Path) -> [(PathBuf, DeviceCert); 2] {
        let root = runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
            .unwrap()
            .group_public_key();
        [paths.device_cert.clone(), paths.device_cert_for_user(&root)].map(|path| {
            let cert = persist::read_device_cert(&path).unwrap();
            tilo_user_identity::verify_device_cert(&cert).unwrap();
            (path, cert)
        })
    }

    #[test]
    fn the_shell_issues_native_certificates_and_the_runtime_foreign_ones() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let personal = tmp.path().join("config/tilo/identity");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        ensure_collaboration_identity(&personal, &paths).unwrap();
        let native = device_certificates(&paths, &personal);
        for (_, cert) in &native {
            assert_eq!(cert.capabilities, DeviceCapabilities::primary());
        }
        let native_bytes = native.clone().map(|(path, _)| fs::read(path).unwrap());
        ensure_collaboration_identity(&personal, &paths).unwrap();
        assert_eq!(
            native.clone().map(|(path, _)| fs::read(path).unwrap()),
            native_bytes
        );

        // A runtime profile left with native certificates is re-issued as
        // foreign, keeping the device key and label.
        super::ensure_foreign_collaboration_identity(&personal, &paths, FILES).unwrap();
        let foreign = device_certificates(&paths, &personal);
        for ((_, before), (_, after)) in native.iter().zip(&foreign) {
            assert!(after.capabilities.foreign);
            assert_eq!(after.device_public_key, before.device_public_key);
            assert_eq!(after.device_label, before.device_label);
        }
        let foreign_bytes = foreign.clone().map(|(path, _)| fs::read(path).unwrap());
        super::ensure_foreign_collaboration_identity(&personal, &paths, FILES).unwrap();
        assert_eq!(
            foreign.map(|(path, _)| fs::read(path).unwrap()),
            foreign_bytes
        );

        let fresh = tempfile::tempdir().expect("tempdir");
        let personal = fresh.path().join("config/tilo/identity");
        let paths = IdentityPaths::under(fresh.path().join("system"));
        super::ensure_foreign_collaboration_identity(&personal, &paths, FILES).unwrap();
        for (_, cert) in device_certificates(&paths, &personal) {
            assert!(cert.capabilities.foreign);
        }
    }

    #[test]
    fn collaboration_setup_builds_and_verifies_one_identity_chain() {
        // Phase `collaboration_setup_creates_one_verified_identity_chain`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));

            assert!(matches!(
                ensure_collaboration_identity(&personal, &paths).expect("provision identity"),
                ProvisionOutcome::Created { .. }
            ));
            let signer = runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("load protected identity");
            let device = persist::read_device_key(&paths.device_privkey).unwrap();
            let cert = persist::read_device_cert(&paths.device_cert).unwrap();

            assert!(
                !paths.user_privkey.exists(),
                "new installs must not duplicate the user-root private key"
            );
            assert_eq!(cert.user_public_key, signer.group_public_key());
            assert_eq!(cert.device_public_key, device.verifying_key().to_bytes());
            tilo_user_identity::verify_device_cert(&cert).unwrap();
            assert_eq!(
                ensure_collaboration_identity(&personal, &paths).unwrap(),
                ProvisionOutcome::Existing
            );
            let before = [
                std::fs::read(personal.join(PRIMARY_IDENTITY_FILE)).unwrap(),
                std::fs::read(&paths.user_pubkey).unwrap(),
                std::fs::read(&paths.device_privkey).unwrap(),
                std::fs::read(&paths.device_cert).unwrap(),
            ];
            verify_collaboration_identity(&personal, &paths, cert.issued_at_ns)
                .expect("complete chain verifies without mutation");
            let after = [
                std::fs::read(personal.join(PRIMARY_IDENTITY_FILE)).unwrap(),
                std::fs::read(&paths.user_pubkey).unwrap(),
                std::fs::read(&paths.device_privkey).unwrap(),
                std::fs::read(&paths.device_cert).unwrap(),
            ];
            assert_eq!(before, after, "login verification must be read-only");
        }
        // Phase `collaboration_verifier_rejects_a_mismatched_personal_root`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let other_personal = tmp.path().join("other/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_collaboration_identity(&personal, &paths).unwrap();
            ensure_primary_identity(&other_personal).unwrap();
            std::fs::copy(
                other_personal.join(PRIMARY_IDENTITY_FILE),
                personal.join(PRIMARY_IDENTITY_FILE),
            )
            .unwrap();

            let error = verify_collaboration_identity(&personal, &paths, u64::MAX)
                .expect_err("a different personal root must not pass login");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        // Phase `collaboration_setup_repairs_the_active_import_without_minting_primary`.
        {
            // Selecting an identity secures it with the session's store; this
            // fixture's store is files.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(FILE_SECRETS_ENV, "1");
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_primary_identity(&personal).expect("seed exported identity");
            let imported = personal.join("identity-imported.json");
            std::fs::rename(personal.join(PRIMARY_IDENTITY_FILE), &imported)
                .expect("give imported identity its managed name");
            runtime_identity::set_active_identity(&personal, "identity-imported")
                .expect("select imported identity");

            assert_eq!(
                ensure_collaboration_identity(&personal, &paths).expect("repair imported chain"),
                ProvisionOutcome::Existing
            );
            assert!(imported.exists());
            assert!(
                !personal.join(PRIMARY_IDENTITY_FILE).exists(),
                "repair must not mint a second personal identity"
            );
            let signer =
                runtime_identity::load_from_path(imported).expect("load imported identity");
            let cert =
                persist::read_device_cert(&paths.device_cert_for_user(&signer.group_public_key()))
                    .expect("identity-specific certificate");
            tilo_user_identity::verify_device_cert(&cert).expect("valid imported chain");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn concurrent_and_multi_user_provisioning_keeps_one_device_owner() {
        // Phase `multiple_os_users_share_one_device_without_replacing_its_owner`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            let alice = tmp.path().join("alice/tilo/identity");
            let bob = tmp.path().join("bob/tilo/identity");
            ensure_collaboration_identity(&alice, &paths).expect("alice provisions owner");
            let owner = persist::read_public_key(&paths.user_pubkey).expect("owner key");

            ensure_collaboration_identity(&bob, &paths).expect("bob joins shared device");
            assert_eq!(
                persist::read_public_key(&paths.user_pubkey).expect("owner remains"),
                owner
            );
            let bob_record = IdentityRecord::from_json_bytes(
                &std::fs::read(bob.join(PRIMARY_IDENTITY_FILE)).expect("bob record"),
            )
            .expect("bob identity");
            let bob_key = bob_record.threshold_identity.group_public_key;
            assert_ne!(bob_key, owner);
            let bob_cert = persist::read_device_cert(&paths.device_cert_for_user(&bob_key))
                .expect("bob device cert");
            tilo_user_identity::verify_device_cert(&bob_cert).expect("valid bob chain");
            assert_eq!(bob_cert.user_public_key, bob_key);
            assert_eq!(
                bob_cert.device_public_key,
                persist::read_device_key(&paths.device_privkey)
                    .expect("device key")
                    .verifying_key()
                    .to_bytes()
            );
        }
        // Phase `simultaneous_user_provisioning_builds_one_consistent_device_chain`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            let alice = tmp.path().join("alice/tilo/identity");
            let bob = tmp.path().join("bob/tilo/identity");
            ensure_primary_identity(&alice).expect("seed alice identity");
            ensure_primary_identity(&bob).expect("seed bob identity");

            let start = Arc::new(Barrier::new(3));
            let alice_start = Arc::clone(&start);
            let alice_paths = paths.clone();
            let alice_dir = alice.clone();
            let alice_thread = std::thread::spawn(move || {
                alice_start.wait();
                ensure_collaboration_identity(&alice_dir, &alice_paths)
            });
            let bob_start = Arc::clone(&start);
            let bob_paths = paths.clone();
            let bob_dir = bob.clone();
            let bob_thread = std::thread::spawn(move || {
                bob_start.wait();
                ensure_collaboration_identity(&bob_dir, &bob_paths)
            });
            start.wait();
            alice_thread
                .join()
                .expect("alice provisioning thread")
                .expect("alice provisioning");
            bob_thread
                .join()
                .expect("bob provisioning thread")
                .expect("bob provisioning");

            let device = persist::read_device_key(&paths.device_privkey)
                .expect("one complete shared device key")
                .verifying_key()
                .to_bytes();
            let owner = persist::read_public_key(&paths.user_pubkey).expect("elected owner");
            let legacy = persist::read_device_cert(&paths.device_cert).expect("legacy owner cert");
            assert_eq!(legacy.user_public_key, owner);
            assert_eq!(legacy.device_public_key, device);
            tilo_user_identity::verify_device_cert(&legacy).expect("valid legacy certificate");

            for identity_dir in [&alice, &bob] {
                let signer =
                    runtime_identity::load_from_path(identity_dir.join(PRIMARY_IDENTITY_FILE))
                        .expect("load user identity");
                let cert = persist::read_device_cert(
                    &paths.device_cert_for_user(&signer.group_public_key()),
                )
                .expect("identity-specific certificate");
                assert_eq!(cert.device_public_key, device);
                tilo_user_identity::verify_device_cert(&cert)
                    .expect("identity-specific chain remains valid");
            }
        }
        // Phase `simultaneous_owner_claims_elect_one_valid_winner_without_clobbering`.
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("system/user-identity/pubkey.bin");
            std::fs::create_dir_all(path.parent().expect("claim parent")).expect("claim parent");

            // Distinct roots model two OS users completing their first identity
            // ceremony at once.  Start both claim attempts together, rather than
            // relying on an incidental scheduler race.
            let alice = [0xA1; 32];
            let bob = [0xB2; 32];
            let start = Arc::new(Barrier::new(3));
            let alice_start = Arc::clone(&start);
            let alice_path = path.clone();
            let alice_thread = std::thread::spawn(move || {
                alice_start.wait();
                (alice, claim_device_owner(&alice_path, &alice))
            });
            let bob_start = Arc::clone(&start);
            let bob_path = path.clone();
            let bob_thread = std::thread::spawn(move || {
                bob_start.wait();
                (bob, claim_device_owner(&bob_path, &bob))
            });
            start.wait();

            let (alice_key, alice_claim) = alice_thread.join().expect("alice claim thread");
            let (bob_key, bob_claim) = bob_thread.join().expect("bob claim thread");
            let alice_claim = alice_claim.expect("alice claim result");
            let bob_claim = bob_claim.expect("bob claim result");
            assert_ne!(
                alice_claim, bob_claim,
                "exactly one distinct root owns the device"
            );

            let winner = persist::read_public_key(&path).expect("valid owner key");
            let claimed_winner = if alice_claim { alice_key } else { bob_key };
            assert_eq!(
                winner, claimed_winner,
                "the elected owner was never overwritten"
            );
            assert_eq!(
                std::fs::read(&path).expect("owner key bytes").len(),
                32,
                "concurrent readers can only observe a complete winner file"
            );
        }
    }
}
