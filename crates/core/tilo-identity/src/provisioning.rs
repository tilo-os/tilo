//! Provisioning of the collaboration identity chain: the personal identity
//! record, the per-device key and its root-signed certificate, and their
//! registration in the identity registry and device fleet.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::store::IdentityRecord;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_device_identity::persist::DeviceKeyOwnership;
use tilo_device_identity::{IdentityPaths, persist};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use zeroize::{Zeroize, Zeroizing};

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
/// ([`crate::store::file_secrets_requested`]).
pub use crate::store::FILE_SECRETS_ENV;

impl IdentityStorage {
    /// What this session keeps its identity secrets in.
    #[must_use]
    pub fn for_session() -> Self {
        if !crate::store::file_secrets_requested() {
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
    pub fn device_key_ownership(self) -> DeviceKeyOwnership {
        match self {
            Self::Platform => DeviceKeyOwnership::Keychain,
            Self::Files => DeviceKeyOwnership::File,
        }
    }

    /// Where the device key is kept under this storage: a file, which Tilo
    /// OS shares between the root filesystem daemon and the session.
    #[cfg(not(target_os = "macos"))]
    #[must_use]
    pub fn device_key_ownership(self) -> DeviceKeyOwnership {
        match self {
            Self::Platform | Self::Files => DeviceKeyOwnership::File,
        }
    }
}

fn identity_load_error(error: tilo_identity_client::NotAvailable) -> io::Error {
    match error {
        tilo_identity_client::NotAvailable::ConnectionFailed(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// Human-facing name of the Unix account running this process
/// ([`account_display_name`]).
#[cfg(unix)]
pub fn login_account_display_name() -> String {
    account_display_name(rustix::process::geteuid().as_raw())
}

/// Human-facing name of the Unix account `uid`: its full name (the first
/// field of its GECOS), else its login name. A new person takes it as their
/// name until they rename themselves.
#[cfg(unix)]
pub fn account_display_name(uid: u32) -> String {
    use std::ffi::CStr;

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
        .filter(|name| !name.trim().is_empty() && uid == rustix::process::geteuid().as_raw())
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
/// what the runtime identity broker migrates and what the identity store
/// writes when setup mints the person.
pub const PRIMARY_IDENTITY_FILE: &str = "primary.json";

/// The empty file a device leaves in the identity directory when it generated the person's root
/// itself: that device founds the person's own workspace when its workspace service first starts.
/// A root that came from elsewhere (a page, recovery words, another of the person's devices)
/// leaves none, since its person's workspace exists already and the device takes it in.
const FOUNDING_MARK: &str = "founds-person-workspace";

/// Mark that this device generated the person's root, so it founds their own workspace.
///
/// # Errors
/// The mark cannot be written.
pub fn mark_founding(identity_dir: &Path) -> io::Result<()> {
    tilo_files::native::atomic::write_private(&identity_dir.join(FOUNDING_MARK), b"")
}

/// Whether this device generated the person's root and has not founded their own workspace yet.
#[must_use]
pub fn founds_person_workspace(identity_dir: &Path) -> bool {
    identity_dir.join(FOUNDING_MARK).is_file()
}

/// Forget the founding mark, once the person's own workspace is founded and written.
///
/// # Errors
/// The mark is there and cannot be removed.
pub fn clear_founding_mark(identity_dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(identity_dir.join(FOUNDING_MARK)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
/// The id of [`PRIMARY_IDENTITY_FILE`] in the store.
const PRIMARY_IDENTITY_ID: &str = "primary";

/// Sequence for same-directory temporary owner-claim files.  The final claim
/// is a hard link, not this name; `create_new` simply prevents a stale temp
/// file from ever being overwritten.
static OWNER_CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

/// The certificate a device holding the person's root gives itself: one
/// that acts with the root's authority wherever the device runs, or a
/// throwaway test identity's, which expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceClass {
    Native,
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    Throwaway,
}

impl DeviceClass {
    fn capabilities(self) -> DeviceCapabilities {
        DeviceCapabilities::primary()
    }

    /// A device holding the root never keeps a capped (foreign) certificate
    /// an earlier build gave it, and a throwaway never one that does not
    /// expire. Any certificate that
    /// expires is reissued once it is due for renewal ([`renewal_due_at`]):
    /// an expiring certificate is never how a device loses its workspaces.
    fn must_reissue(self, cert: &DeviceCert, now_ns: u64) -> bool {
        let due = renewal_due_at(cert).is_some_and(|due| now_ns >= due);
        due || cert.capabilities.foreign
            || match self {
                Self::Native => false,
                #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
                Self::Throwaway => cert.expires_at_ns.is_none(),
            }
    }

    /// The class of the identity in `identity_dir` provisioned as `class`:
    /// a throwaway test identity stays one whoever provisions it again, so
    /// its renewed certificate expires as the first did.
    fn of(identity_dir: &Path, class: Self) -> Self {
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        if class == Self::Native && is_throwaway_test_identity(identity_dir) {
            return Self::Throwaway;
        }
        let _ = identity_dir;
        class
    }
}

/// Whether this computer runs Tilo OS, whose system unit runs the device's
/// daemon, rather than an operating system Tilo does not control: what a
/// computer logging in with a ticket reports, so the root certifies it
/// native or foreign. A test build plays the platform
/// [`TEST_PLATFORM_ENV`] names.
#[must_use]
pub fn runs_tilo_os() -> bool {
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    if let Some(platform) = std::env::var_os(TEST_PLATFORM_ENV) {
        return platform == TILO_OS_PLATFORM;
    }
    tilo_paths::machine::system_daemon()
}

/// The platform a test plays, for the class of the device it certifies
/// ([`TILO_OS_PLATFORM`] for Tilo OS, anything else for an operating system
/// Tilo does not control). Read only by a test build: a release build's tests
/// name it, and the computer decides.
pub const TEST_PLATFORM_ENV: &str = "TILO_TEST_PLATFORM";
/// [`TEST_PLATFORM_ENV`]'s word for Tilo OS.
pub const TILO_OS_PLATFORM: &str = "tilo-os";

mod renewal;

pub use renewal::{
    RENEWAL_CAUSES, RenewalCauses, RenewalClock, joined_device_renewal_due,
    keep_device_certificates_renewed, next_device_certificate_renewal, renewal_due_at,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityProvisioningContext {
    /// Native Tilo: the dedicated OOBE owns the identity and storage policy.
    NativeTilo,
    /// A disposable identity a dev-image test session mints for itself.
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    ThrowawayTest,
}

/// Create a fresh single-device identity at `<identity_dir>/primary.json`.
/// The final JSON is public schema-2 metadata; its private share is imported
/// into Secret Service or the explicitly reported private-file fallback.
///
/// Idempotent: an existing file is returned as [`ProvisionOutcome::Existing`] with no modification.
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

/// Create the personal identity explicitly chosen during installer preparation.
/// Unlike automatic first-login provisioning, an existing identity is refused.
/// The signing share immediately enters the canonical platform secret provider.
pub fn create_primary_identity(
    identity_dir: &Path,
    display_name: &str,
    avatar_color: [u8; 3],
    storage: IdentityStorage,
) -> io::Result<ProvisionOutcome> {
    if display_name.trim().is_empty()
        || display_name.trim().len() > 1024
        || display_name.trim().chars().any(char::is_control)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "An identity needs a bounded display name without control characters.",
        ));
    }
    create_named_primary_identity(identity_dir, display_name.trim(), avatar_color, storage)
}

fn create_named_primary_identity(
    identity_dir: &Path,
    display_name: &str,
    avatar_color: [u8; 3],
    storage: IdentityStorage,
) -> io::Result<ProvisionOutcome> {
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let pubkey_hex = hex::encode(root.verifying_key().as_bytes());
    import_whole_identity(
        identity_dir,
        PRIMARY_IDENTITY_ID,
        &root,
        display_name,
        avatar_color,
        storage,
    )?;
    mark_founding(identity_dir)?;
    Ok(ProvisionOutcome::Created { pubkey_hex })
}

/// Keep a person's whole root, opened elsewhere (a person made on a page, a
/// person made by the command line), as this account's single-device
/// identity: `<identity_dir>/primary.json`, made active, its secret in
/// `storage`. The store is the one home of a root: nothing else keeps it.
///
/// # Errors
///
/// The directory is not private to this account, an identity is already
/// there, or the identity cannot be written and secured.
pub fn import_single_device_root(
    identity_dir: &Path,
    root: &ed25519_dalek::SigningKey,
    display_name: &str,
    storage: IdentityStorage,
) -> io::Result<PathBuf> {
    import_single_device_root_as(
        identity_dir,
        PRIMARY_IDENTITY_ID,
        root,
        display_name,
        storage,
    )
}

/// [`import_single_device_root`] as the record `id`, beside the identities
/// already kept, which stay as they are: the one this account now acts as.
///
/// # Errors
///
/// As [`import_single_device_root`], and when `id` or this root is kept
/// already.
pub fn import_single_device_root_as(
    identity_dir: &Path,
    id: &str,
    root: &ed25519_dalek::SigningKey,
    display_name: &str,
    storage: IdentityStorage,
) -> io::Result<PathBuf> {
    prepare_identity_directory(identity_dir)?;
    let path = import_whole_identity(
        identity_dir,
        id,
        root,
        display_name,
        crate::store::DEFAULT_AVATAR_COLOR,
        storage,
    )?;
    crate::store::set_active_identity(identity_dir, id)?;
    Ok(path)
}

fn import_whole_identity(
    identity_dir: &Path,
    id: &str,
    root: &ed25519_dalek::SigningKey,
    display_name: &str,
    avatar_color: [u8; 3],
    storage: IdentityStorage,
) -> io::Result<PathBuf> {
    let mut record = IdentityRecord::from_root(
        root,
        display_name,
        avatar_color,
        std::time::SystemTime::now(),
    );
    let encoded = record.to_json_bytes();
    record.threshold_identity.secret_bytes.zeroize();
    let bytes = Zeroizing::new(encoded?);
    import_identity_record(identity_dir, id, &bytes, storage)
}

fn ensure_primary_identity_in_context(
    identity_dir: &Path,
    context: IdentityProvisioningContext,
    storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    let path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    if path.exists() {
        protect_identity_record(identity_dir, storage)?;
        return Ok(ProvisionOutcome::Existing);
    }

    // Mint through the same encoder the interactive ceremony uses, so
    // there's exactly one writer of the on-disk record — the two provisioning paths can't drift apart in what they store.
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    let pubkey_hex = hex::encode(root.verifying_key().as_bytes());
    let display_name = match context {
        IdentityProvisioningContext::NativeTilo => String::new(),
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        IdentityProvisioningContext::ThrowawayTest => THROWAWAY_TEST_IDENTITY_NAME.to_owned(),
    };
    let mut record = IdentityRecord::from_root(
        &root,
        &display_name,
        crate::store::DEFAULT_AVATAR_COLOR,
        std::time::SystemTime::now(),
    );
    let encoded = record.to_json_bytes();
    record.threshold_identity.secret_bytes.zeroize();
    let bytes = Zeroizing::new(encoded?);

    import_identity_record(identity_dir, PRIMARY_IDENTITY_ID, &bytes, storage)?;
    mark_founding(identity_dir)?;
    Ok(ProvisionOutcome::Created { pubkey_hex })
}

/// Provision the complete identity chain used by collaboration.
///
/// The personal identity remains protected by the runtime identity provider.
/// It signs the certificate for a distinct per-device key without exporting
/// its private share into the system identity tree. That device key is the
/// single transport identity used by the shell, filesystem synchroniser, and
/// hosted server. Existing records are migrated idempotently; an existing
/// device key is never replaced.
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
        None,
    )
}

/// Provision the native chain with the installer's approved device label.
/// Existing verified certificates and their labels remain unchanged.
///
/// # Errors
/// Refuses empty, oversized or control-containing labels before provisioning,
/// and all canonical identity/key/certificate/provider failures.
pub fn ensure_collaboration_identity_with_device_label(
    identity_dir: &Path,
    paths: &IdentityPaths,
    storage: IdentityStorage,
    label: &str,
) -> Result<ProvisionOutcome, io::Error> {
    approved_label(label)?;
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::NativeTilo,
        storage,
        DeviceClass::Native,
        Some(label),
    )
}

/// Refuse a device label that is empty, longer than 64 bytes or holds a control character.
fn approved_label(label: &str) -> io::Result<()> {
    if label.trim().is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device label must contain 1 to 64 bytes without control characters",
        ));
    }
    Ok(())
}

/// Restore the person encoded in their recovery words and certify this device.
/// The recovery key supplies the root alone. Any presentation already kept
/// for that identity stays with the record.
///
/// # Errors
/// Invalid words, words for a different active identity, or a failure to
/// restore the record or certify the device.
#[cfg(unix)]
pub fn restore_from_recovery_words(
    identity_dir: &Path,
    paths: &IdentityPaths,
    words: &str,
    label: Option<&str>,
    storage: IdentityStorage,
) -> io::Result<crate::store::IdentityProfile> {
    let profile = crate::store::IdentityRegistryStore::at(identity_dir)
        .import_recovery_words(words, "", storage)?;
    let runtime_id = profile
        .record_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid identity id"))?;
    crate::store::set_active_identity(identity_dir, runtime_id)?;
    if let Some(label) = label {
        ensure_collaboration_identity_with_device_label(identity_dir, paths, storage, label)?;
    } else {
        ensure_collaboration_identity(identity_dir, paths, storage)?;
    }
    crate::device_fleet::DeviceFleetStore::at(identity_dir, paths.clone())
        .register_current_device(&profile.id)?;
    Ok(profile)
}

/// Complete this device's own side of an enrollment made without the root: the
/// certificate filed for its person, a public record naming that person as
/// the account's active identity under `identity_dir`, and a fleet that knows
/// this device. A node's session is enrolled so when its node is installed,
/// by a server that writes it only the certificate and the person's public
/// key. Answers whether `paths` holds such a device; idempotent, and never
/// writes a root or replaces an active identity.
pub fn complete_enrolled_device(
    identity_dir: &Path,
    paths: &IdentityPaths,
    now_ns: u64,
) -> Result<bool, io::Error> {
    complete_enrolled(identity_dir, paths, now_ns, |person| {
        crate::store::public_member_record(
            crate::store::UNNAMED_DISPLAY_NAME,
            [88, 101, 242],
            person,
        )
    })
}

/// [`complete_enrolled_device`] for a device certified before it ever ran
/// (an image the person's own machine wrote), which names the person as that
/// machine knows them, rather than unnamed until their record arrives.
///
/// # Errors
/// As [`complete_enrolled_device`].
pub fn complete_enrolled_device_named(
    identity_dir: &Path,
    paths: &IdentityPaths,
    now_ns: u64,
    display_name: &str,
    avatar_color: [u8; 3],
    photo: Option<tilo_identity_client::ProfilePhoto>,
) -> Result<bool, io::Error> {
    complete_enrolled(identity_dir, paths, now_ns, |person| {
        crate::store::public_member_record_with_presentation(
            if display_name.trim().is_empty() {
                crate::store::UNNAMED_DISPLAY_NAME
            } else {
                display_name
            },
            avatar_color,
            person,
            photo,
        )
    })
}

fn complete_enrolled(
    identity_dir: &Path,
    paths: &IdentityPaths,
    now_ns: u64,
    record: impl FnOnce([u8; 32]) -> io::Result<Vec<u8>>,
) -> Result<bool, io::Error> {
    if !persist::joined_device_is_ready(paths, now_ns)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
    {
        return Ok(false);
    }
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    persist::install_device_cert(paths, &cert, now_ns)
        .map_err(|error| io::Error::other(error.to_string()))?;
    prepare_identity_directory(identity_dir)?;
    let identity_id = hex::encode(cert.user_public_key);
    // A computer that holds its person's root keeps the record it signs as them with: the public
    // projection a device enrolled without the root takes up would name the same person a second
    // time and could not sign, so it is not written here.
    let holds_their_root = crate::store::resolve_active_identity(identity_dir)
        .and_then(|active| crate::store::load_metadata_from_path(&active).ok())
        .is_some_and(|active| {
            active.single_device && active.group_public_key == cert.user_public_key
        });
    if !holds_their_root {
        let record_id = public_record_id(&identity_id);
        let record_path = identity_dir.join(format!("{record_id}.json"));
        if !record_path.exists() {
            let record = record(cert.user_public_key)?;
            tilo_files::native::atomic::write_with_mode(&record_path, &record, 0o600)?;
        }
        if crate::store::resolve_active_identity(identity_dir).is_none() {
            tilo_files::native::atomic::write_with_mode(
                &identity_dir.join("active"),
                format!("{record_id}\n").as_bytes(),
                0o600,
            )?;
        }
    }
    crate::device_fleet::DeviceFleetStore::at(identity_dir, paths.clone())
        .remember_certificate(&identity_id, cert)?;
    Ok(true)
}

/// Make the person of `cert`, the certificate a login with a ticket brought, this account's
/// identity: the device's enrolment becomes theirs, replacing one of another person (the root a
/// computer made for itself before it logged in, or an earlier login's), since the person just
/// chose this one; and the account acts as them from now on. An account that holds this person's
/// root keeps acting through it: logging in as yourself on the computer that keeps your root does
/// not make it a mere device of yours.
///
/// # Errors
/// The certificate is not one this device may take, or the identity cannot be written.
pub fn adopt_login_certificate(
    identity_dir: &Path,
    paths: &IdentityPaths,
    cert: &tilo_user_identity::DeviceCert,
    now_ns: u64,
) -> io::Result<()> {
    let install = || {
        persist::install_device_cert(paths, cert, now_ns)
            .map_err(|error| io::Error::other(error.to_string()))
    };
    match persist::install_device_cert(paths, cert, now_ns) {
        Err(persist::PersistError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
            tracing::info!(person = %hex::encode(cert.user_public_key), "this computer logged in as another person than it was enrolled for: it is theirs now");
            for path in [&paths.user_pubkey, &paths.device_cert] {
                match fs::remove_file(path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            install()?;
        }
        installed => installed.map_err(|error| io::Error::other(error.to_string()))?,
    }
    complete_enrolled_device(identity_dir, paths, now_ns)?;
    let holds_their_root = crate::store::resolve_active_identity(identity_dir)
        .and_then(|active| crate::store::load_metadata_from_path(&active).ok())
        .is_some_and(|active| {
            active.single_device && active.group_public_key == cert.user_public_key
        });
    if holds_their_root {
        return Ok(());
    }
    crate::store::set_active_identity(
        identity_dir,
        &public_record_id(&hex::encode(cert.user_public_key)),
    )
}

fn public_record_id(identity_id: &str) -> String {
    let prefix: String = identity_id.chars().take(55).collect();
    format!("identity-{prefix}")
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

/// Whether the primary record still carries the label assigned by dev provisioning.
#[must_use]
pub fn is_throwaway_test_display_name(record: &Path, name: &str) -> bool {
    name == THROWAWAY_TEST_IDENTITY_NAME
        && record
            .file_name()
            .is_some_and(|file| file == PRIMARY_IDENTITY_FILE)
        && record.parent().is_some_and(is_throwaway_test_identity)
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
/// identity that is not a throwaway one; [`io::ErrorKind::PermissionDenied`]
/// for the shared device tree. Test roots must be dedicated and disposable;
/// their certificates expire after 24 hours. Disposable private shares use
/// owner-only files in `identity_dir`, regardless of the requested storage,
/// so deleting the fixture also deletes its secrets. Otherwise as
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
    _storage: IdentityStorage,
) -> Result<ProvisionOutcome, io::Error> {
    validate_throwaway_identity_paths(paths)?;
    prepare_identity_directory(identity_dir)?;
    let marker = identity_dir.join(THROWAWAY_TEST_IDENTITY_MARKER);
    if crate::store::resolve_active_identity(identity_dir).is_some() {
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
        IdentityStorage::Files,
        DeviceClass::Throwaway,
        None,
    )
}

/// A throwaway must never certify the real machine's shared device key.
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn validate_throwaway_identity_paths(paths: &IdentityPaths) -> io::Result<()> {
    let shared = Path::new("/var/lib/tilo");
    let shared_real = shared
        .canonicalize()
        .unwrap_or_else(|_| shared.to_path_buf());
    for path in [
        &paths.user_pubkey,
        &paths.device_privkey,
        &paths.device_cert,
    ] {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "throwaway identity paths must be absolute without parent traversal",
            ));
        }
        if path.starts_with(shared) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "throwaway identities require a private TILO_IDENTITY_ROOT; refusing the shared /var/lib/tilo tree",
            ));
        }
        // Resolve the nearest existing ancestor, including symlink aliases.
        for ancestor in path.ancestors() {
            if fs::read_link(ancestor).is_ok_and(|target| target.starts_with(shared)) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "throwaway identity root aliases the shared device tree",
                ));
            }
            match ancestor.canonicalize() {
                Ok(real) if real.starts_with(&shared_real) => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "throwaway identity root resolves to the shared device tree",
                    ));
                }
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

/// Prepare the private per-user identity directory before either the key
/// transaction or its first-use crash journal is written. Keeping this as one
/// operation prevents the journal preflight and the identity writer from
/// disagreeing about directory permissions.
pub fn prepare_identity_directory(identity_dir: &Path) -> io::Result<()> {
    tilo_files::native::private::directory(identity_dir)
}

/// This device's certificate, when it verifies but the clock's reading at
/// `now_ns` is outside its validity: the identity is intact and the clock is
/// wrong. `None` when the certificate is valid now, missing, or refused for
/// any other reason (which the identity checks report themselves).
#[must_use]
pub fn device_certificate_clock_refusal(
    paths: &IdentityPaths,
    now_ns: u64,
) -> Option<tilo_user_identity::CertificateClockError> {
    let cert = persist::read_device_cert(&paths.device_cert).ok()?;
    let error = tilo_user_identity::verify_device_cert_at(&cert, now_ns).err()?;
    tilo_user_identity::CertificateClockError::of(&cert, now_ns, &error)
}

/// Why an enrolled device's identity cannot be used. A certificate refused
/// for the clock's reading alone keeps its
/// [`tilo_user_identity::CertificateClockError`], so startup tells a wrong
/// clock apart from a broken identity and never offers to replace one that
/// is intact.
fn joined_device_unusable(paths: &IdentityPaths, error: persist::PersistError) -> io::Error {
    match error {
        persist::PersistError::Io(error)
            if tilo_user_identity::CertificateClockError::in_io(&error).is_some() =>
        {
            error
        }
        error => io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "pre-enrolled device identity in {} is not usable: {error}",
                paths.device_cert.display()
            ),
        ),
    }
}

fn ensure_collaboration_identity_in_context(
    identity_dir: &Path,
    paths: &IdentityPaths,
    context: IdentityProvisioningContext,
    storage: IdentityStorage,
    class: DeviceClass,
    approved_label: Option<&str>,
) -> Result<ProvisionOutcome, io::Error> {
    prepare_identity_directory(identity_dir)?;
    let class = DeviceClass::of(identity_dir, class);

    let now_ns = tilo_clock::unix_now_ns();
    drop_unreadable_certificates_of_held_root(identity_dir, paths);
    match persist::joined_device_is_ready(paths, now_ns) {
        Ok(true) => {
            // `Existing` promises a record this account can resolve. The
            // enrollment (person's key, device key, certificate) lives in the
            // system identity tree, the public record in this account's
            // config, and the two can part: a login account that never ran
            // the join, or a config tree reset while the enrollment
            // survived. Take the person up again here rather than report an
            // identity that nothing can read.
            if crate::store::resolve_active_identity(identity_dir).is_none() {
                complete_enrolled_device(identity_dir, paths, now_ns)?;
            }
            if let Some(record) = crate::store::resolve_active_identity(identity_dir)
                && let Ok(signer) = crate::store::load_from_path(record)
            {
                prove_local_person(identity_dir, &signer, paths);
            }
            return Ok(ProvisionOutcome::Existing);
        }
        Ok(false) => {}
        Err(error) => return Err(joined_device_unusable(paths, error)),
    }

    let primary_path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    let (selected_path, outcome) =
        if let Some(path) = crate::store::resolve_active_identity(identity_dir) {
            protect_identity_record_at(identity_dir, &path, storage)?;
            (path, ProvisionOutcome::Existing)
        } else {
            let outcome = ensure_primary_identity_in_context(identity_dir, context, storage)?;
            (primary_path, outcome)
        };

    let signer = crate::store::load_from_path(selected_path).map_err(identity_load_error)?;
    if !signer.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            crate::store::CANNOT_SIGN_ALONE,
        ));
    }
    paths.ensure_directories()?;

    let root_public = signer.group_public_key();
    // The first identity to claim the machine owns its legacy system-facing
    // certificate. Later OS users keep their own root and receive a separate
    // identity-keyed certificate for the same device key.
    let device_owner = claim_device_owner(&paths.user_pubkey, &root_public)?;

    let machine = machine_label(Path::new("/"));
    let device_label = approved_label.unwrap_or(match &machine {
        Some(name) if device_owner => name.as_str(),
        _ if device_owner => tilo_user_identity::UNNAMED_PRIMARY_DEVICE,
        _ => tilo_user_identity::UNNAMED_SHARED_DEVICE,
    });

    // The device's certificate for this identity: the owner's is also the
    // device's system-facing certificate, one certificate in both places.
    ensure_device_certificate_for_identity(&signer, paths, device_label, class)?;
    prove_local_person(identity_dir, &signer, paths);

    // Register the primary record in the multi-identity registry. A registry
    // write failure is a provisioning failure: silently creating a key that no
    // workspace can select strands the identity immediately.
    let registry = crate::store::IdentityRegistryStore::at(identity_dir).load_reconciled()?;
    if let Some(identity_id) = registry.default_identity {
        crate::device_fleet::DeviceFleetStore::at(identity_dir, paths.clone())
            .register_current_device(&identity_id)?;
    }

    Ok(outcome)
}

/// Drop this device's certificates for a person whose root this computer holds when they are in
/// an encoding no build reads any more: the root certifies the device key again below, so an old
/// certificate never stops the computer of the person who can make a new one. One no build reads
/// proves nothing to anyone, so the device's own goes too; a certificate that reads stays.
fn drop_unreadable_certificates_of_held_root(identity_dir: &Path, paths: &IdentityPaths) {
    let Some(root) = crate::store::resolve_active_identity(identity_dir)
        .and_then(|record| crate::store::load_metadata_from_path(&record).ok())
        .filter(|metadata| metadata.single_device)
        .map(|metadata| metadata.group_public_key)
    else {
        return;
    };
    for path in [paths.device_cert.clone(), paths.device_cert_for_user(&root)] {
        if let Err(persist::PersistError::Canonical(error)) = persist::read_device_cert(&path) {
            tracing::info!(path = %path.display(), %error, "this device's certificate is in an encoding no build reads; the person's root certifies the device again");
            if let Err(error) = fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), %error, "the unreadable certificate could not be dropped");
            }
        }
    }
}

/// Keep this account's proof that it is the device's person
/// ([`crate::local_person`]) when `signer` is the identity the device is
/// bound to: written with the binding, and at the next sign-in on a device
/// bound before proofs existed. One that cannot be signed here (the key is
/// held on another device) is logged; the device's services then fall back
/// to who is signed in.
fn prove_local_person(
    identity_dir: &Path,
    signer: &crate::store::SigningIdentity,
    paths: &IdentityPaths,
) {
    // Device services that run as root find the device's person by uid. On
    // Windows no such service runs: the daemon runs as the person's account.
    #[cfg(unix)]
    {
        let uid = rustix::process::geteuid().as_raw();
        if let Err(error) = crate::local_person::ensure_proof(identity_dir, signer, paths, uid) {
            tracing::warn!(%error, "could not prove this account is the device's person");
        }
    }
    #[cfg(not(unix))]
    let _ = (identity_dir, signer, paths);
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

    // An established device was claimed long ago. Reading the claim needs no
    // free space; staging a new one would, so every login on a full disk
    // would otherwise fail here.
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(read_claimed_owner(path)? == *root_public),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

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
            tilo_files::native::atomic::sync_directory(parent)?;
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

/// Whether the identity this account signs in with owns the device.
///
/// The owner is the identity that claimed the machine at its first login
/// (`claim_device_owner`) and holds the device's primary certificate; the
/// account that signs in with it is the device admin. Another account on the
/// same machine has its own identity and a shared-device certificate, and an
/// account that has not finished first login has no identity at all: both
/// answer `false`.
///
/// Only public material is read — the active record's metadata, the owner
/// claim and the certificate — so the answer needs no unlocked secret.
pub fn active_identity_owns_device(identity_dir: &Path, paths: &IdentityPaths) -> io::Result<bool> {
    let Some(record) = crate::store::resolve_active_identity(identity_dir) else {
        return Ok(false);
    };
    if !paths.user_pubkey.exists() {
        return Ok(false);
    }
    let owner = read_claimed_owner(&paths.user_pubkey)?;
    let identity = tilo_identity_client::load_metadata_from_path(&record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if identity.group_public_key != owner {
        return Ok(false);
    }
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let device = persist::load_device_key(paths)
        .map_err(|error| io::Error::other(error.to_string()))?
        .verifying_key()
        .to_bytes();
    Ok(cert.user_public_key == owner && cert.device_public_key == device)
}

/// Ensure this device holds its certificate for `identity`, labelled
/// `device_label` when it is issued now. A device presents one certificate
/// per identity: for the identity that owns the device it is written both
/// as the device's system-facing certificate (`device_cert`) and as the
/// identity-keyed one, the same bytes, so the network's identity store and the
/// broker's trusted device are the same certificate. A certificate already
/// held for the device's key is kept (the system-facing one first) and
/// written wherever the other copy differs; a new one is issued only when
/// none is held or the held one is due for renewal, and then written to
/// every place at once.
fn ensure_device_certificate_for_identity(
    identity: &crate::store::SigningIdentity,
    paths: &IdentityPaths,
    device_label: &str,
    class: DeviceClass,
) -> Result<PathBuf, io::Error> {
    let (device, _) =
        persist::load_or_create_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    let device_public_key = device.verifying_key().to_bytes();
    let user_public_key = identity.group_public_key();
    let cert_path = paths.device_cert_for_user(&user_public_key);
    let owns_device =
        read_claimed_owner(&paths.user_pubkey).is_ok_and(|owner| owner == user_public_key);
    let mut places = Vec::with_capacity(2);
    if owns_device {
        places.push((
            paths.device_cert.clone(),
            "device certificate does not match the provisioned identity keys",
        ));
    }
    places.push((
        cert_path.clone(),
        "identity-specific device certificate does not match its keys",
    ));
    let mut held = Vec::with_capacity(places.len());
    for (path, mismatch) in places {
        let cert = if path.exists() {
            let cert = persist::read_device_cert(&path)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            tilo_user_identity::verify_device_cert(&cert)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            if cert.user_public_key != user_public_key
                || cert.device_public_key != device_public_key
            {
                return Err(io::Error::new(io::ErrorKind::InvalidData, mismatch));
            }
            Some(cert)
        } else {
            None
        };
        held.push((path, cert));
    }
    let current = held.iter().find_map(|(_, cert)| cert.clone());
    let cert = match current {
        Some(cert) if !class.must_reissue(&cert, tilo_clock::unix_now_ns()) => cert,
        Some(cert) => {
            issue_device_certificate(identity, device_public_key, &cert.device_label, class)?
        }
        // A machine copied from another one re-keyed at boot and named
        // itself after it; its certificate carries that name.
        None => {
            let branch = tilo_device_identity::branch::pending_label(paths)
                .ok()
                .flatten();
            let label = branch.as_deref().unwrap_or(device_label);
            issue_device_certificate(identity, device_public_key, label, class)?
        }
    };
    for (path, existing) in &held {
        if existing.as_ref() != Some(&cert) {
            persist::write_device_cert(path, &cert).map_err(|e| io::Error::other(e.to_string()))?;
        }
    }
    if let Err(error) = tilo_device_identity::branch::clear_pending_label(paths) {
        tracing::warn!(%error, "could not clear the name a copied machine waited with");
    }
    Ok(cert_path)
}

fn issue_device_certificate(
    identity: &crate::store::SigningIdentity,
    device_public_key: [u8; 32],
    device_label: &str,
    class: DeviceClass,
) -> Result<DeviceCert, io::Error> {
    issue_device_certificate_at(
        identity,
        device_public_key,
        device_label,
        class,
        tilo_clock::unix_now_ns(),
    )
}

fn issue_device_certificate_at(
    identity: &crate::store::SigningIdentity,
    device_public_key: [u8; 32],
    device_label: &str,
    class: DeviceClass,
    issued_at_ns: u64,
) -> Result<DeviceCert, io::Error> {
    let expires_at_ns = match class {
        DeviceClass::Native => None,
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        DeviceClass::Throwaway => Some(issued_at_ns.saturating_add(24 * 60 * 60 * 1_000_000_000)),
    };
    let mut cert = DeviceCert {
        user_public_key: identity.group_public_key(),
        device_public_key,
        device_label: device_label.into(),
        capabilities: class.capabilities(),
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, identity)
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
    let primary_path = crate::store::resolve_active_identity(identity_dir)
        .unwrap_or_else(|| identity_dir.join(PRIMARY_IDENTITY_FILE));
    let dir_metadata = fs::metadata(identity_dir)?;
    if !dir_metadata.is_dir() || !tilo_files::native::private::is_owner_only(&dir_metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "personal identity directory must not be accessible by group or other users",
        ));
    }
    match tilo_files::native::private::read(&primary_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(error),
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "primary identity must be this user's own private regular file",
            ));
        }
    }
    // A joined device holds no share of the identity's key: its record only
    // names the identity, and its certificate binds this device's key to it.
    let joined = persist::joined_device_is_ready(paths, now_ns)
        .map_err(|error| joined_device_unusable(paths, error))?;
    if joined {
        let metadata = tilo_identity_client::load_metadata_from_path(&primary_path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let root_public = persist::read_public_key(&paths.user_pubkey)
            .map_err(|e| io::Error::other(e.to_string()))?;
        if metadata.group_public_key != root_public {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "personal identity and persisted certification root do not match",
            ));
        }
        return Ok(());
    }
    let identity = crate::store::load_from_path(primary_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if !identity.is_single_device() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            crate::store::CANNOT_SIGN_ALONE,
        ));
    }
    let root_public = persist::read_public_key(&paths.user_pubkey)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let device = persist::load_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|e| io::Error::other(e.to_string()))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns).map_err(|error| {
        tilo_user_identity::CertificateClockError::of(&cert, now_ns, &error).map_or_else(
            || io::Error::other(error.to_string()),
            tilo_user_identity::CertificateClockError::into_io,
        )
    })?;

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

/// Where this device's key and certificates live, for every program of this
/// account (the session, its setup, `tilo`, and the shell runtime that proves to the session):
/// `$TILO_IDENTITY_ROOT` when set, else Tilo's system tree (`/var/lib/tilo`) on a machine that has
/// one, else the account's own data directory (`$XDG_DATA_HOME/tilo`), as on
/// a computer running Tilo beside another operating system. The rule is
/// [`tilo_device_identity::persist::collaboration_identity_root`]'s, which every program reads it
/// from.
#[must_use]
pub fn collaboration_identity_root() -> PathBuf {
    tilo_device_identity::persist::collaboration_identity_root()
}

/// This device's identity paths under [`collaboration_identity_root`], with
/// this session's device-key identity store ([`IdentityStorage::for_session`]).
pub fn collaboration_identity_paths() -> IdentityPaths {
    IdentityPaths::under(collaboration_identity_root())
        .with_device_key_ownership(IdentityStorage::for_session().device_key_ownership())
}

/// Make a new person whose root this account holds: a fresh root kept as this
/// account's identity in `identity_dir`, named `display_name` (unnamed while
/// it is empty), and this device certified by it as `device_label` in `paths`
/// (named for the machine while that is empty), with the root's authority
/// wherever it runs. The one way a person is made: the device's
/// daemon does it (`devices#found`), for onboarding and `tilo login
/// --new-identity` alike. Answers the person's user public key.
///
/// # Errors
///
/// This account already has an identity (one it holds the root of, or one
/// joined from another device), or the identity cannot be written, secured
/// or certified.
pub fn create_person(
    identity_dir: &Path,
    paths: &IdentityPaths,
    display_name: &str,
    device_label: &str,
    storage: IdentityStorage,
) -> io::Result<[u8; 32]> {
    if let Some(existing) = crate::store::resolve_active_identity(identity_dir) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "this account already has a Tilo identity ({})",
                existing.display()
            ),
        ));
    }
    let label = device_label.trim();
    if !label.is_empty() {
        approved_label(label)?;
    }
    let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
    import_single_device_root(identity_dir, &root, display_name, storage)?;
    mark_founding(identity_dir)?;
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::NativeTilo,
        storage,
        DeviceClass::Native,
        (!label.is_empty()).then_some(label),
    )?;
    Ok(root.verifying_key().to_bytes())
}

fn import_identity_record(
    identity_dir: &Path,
    id: &str,
    bytes: &[u8],
    storage: IdentityStorage,
) -> io::Result<PathBuf> {
    match storage {
        IdentityStorage::Files => {
            let provider = crate::store::BrokerFileProvider::new(identity_dir);
            crate::store::import_identity_with_provider(identity_dir, id, bytes, &provider)
        }
        IdentityStorage::Platform => crate::store::import_identity(identity_dir, id, bytes),
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
            let provider = crate::store::BrokerFileProvider::new(identity_dir);
            crate::store::secure_identity_with_provider(identity_dir, id, &provider)?;
        }
        IdentityStorage::Platform => {
            crate::store::secure_identity(identity_dir, id)?;
        }
    }
    Ok(())
}

/// What the machine under `root` calls itself, for a device nobody named yet: its pretty hostname,
/// else a hostname that is not a distribution's default, else what kind of machine it is.
fn machine_label(root: &Path) -> Option<String> {
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap_or_default();
    let pretty = read("etc/machine-info").lines().find_map(|line| {
        let value = line.trim().strip_prefix("PRETTY_HOSTNAME=")?;
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        Some(
            value
                .replace("\\\\", "\u{0}")
                .replace('\\', "")
                .replace('\u{0}', "\\"),
        )
    });
    let host = read("proc/sys/kernel/hostname");
    let host = host.trim();
    let named = (!matches!(host, "" | "localhost" | "localhost.localdomain" | "fedora"))
        .then(|| host.to_owned());
    let chassis = match read("sys/class/dmi/id/chassis_type").trim() {
        "8" | "9" | "10" | "14" | "31" | "32" => Some("Laptop".to_owned()),
        "3" | "4" | "5" | "6" | "7" | "13" | "15" | "16" | "35" => Some("Desktop".to_owned()),
        _ => None,
    };
    pretty
        .filter(|name| !name.trim().is_empty())
        .or(named)
        .or(chassis)
}

#[cfg(test)]
mod one_cert_tests;

#[cfg(all(test, unix))]
mod recovery_words_tests {
    use super::*;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the test asserts what it restored before it returns"
    )]
    fn recovery_words_restore_the_person_on_a_headless_device()
    -> Result<(), Box<dyn std::error::Error>> {
        use tilo_device_identity::persist::read_device_cert;

        let source = tempfile::tempdir()?;
        let identity = source.path().join("identity");
        let source_paths = IdentityPaths::under(source.path().join("device"));
        let storage = IdentityStorage::Files;
        let user = create_person(&identity, &source_paths, "Ada", "Original", storage)?;
        let words = crate::store::recovery_words(identity.join(PRIMARY_IDENTITY_FILE))?;
        let original = read_device_cert(&source_paths.device_cert)?;

        let target = tempfile::tempdir()?;
        let identity = target.path().join("identity");
        let target_paths = IdentityPaths::under(target.path().join("device"));
        let profile = restore_from_recovery_words(
            &identity,
            &target_paths,
            &words,
            Some("Headless"),
            storage,
        )?;
        let cert = read_device_cert(&target_paths.device_cert)?;
        assert_eq!(profile.user_public_key, user);
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.device_label, "Headless");
        assert_ne!(cert.device_public_key, original.device_public_key);
        let now_ns = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
        )?;
        verify_collaboration_identity(&identity, &target_paths, now_ns)?;

        let repeated =
            restore_from_recovery_words(&identity, &target_paths, &words, None, storage)?;
        assert_eq!(repeated, profile);
        assert_eq!(read_device_cert(&target_paths.device_cert)?, cert);
        let wrong = restore_from_recovery_words(
            source.path().join("identity").as_path(),
            &source_paths,
            "not a valid recovery key",
            None,
            storage,
        )
        .expect_err("invalid words");
        assert!(crate::store::recovery_words_error(&wrong).is_some());
        assert_eq!(read_device_cert(&source_paths.device_cert)?, original);
        Ok(())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    const FILES: IdentityStorage = IdentityStorage::Files;

    tilo_test_env::scenario! {
        fn only_a_device_that_generated_the_root_founds_the_persons_workspace;

        phase a_root_made_here_leaves_the_mark_until_the_founding_is_written;
        {
            let temp = tempfile::tempdir().unwrap();
            let identity = temp.path().join("identity");
            let paths = IdentityPaths::under(temp.path().join("device"));
            create_person(&identity, &paths, "Ada", "laptop", FILES).unwrap();
            assert!(founds_person_workspace(&identity));
            clear_founding_mark(&identity).unwrap();
            assert!(!founds_person_workspace(&identity));
            clear_founding_mark(&identity).expect("clearing twice is no error");

            let first_login = temp.path().join("first-login");
            assert!(matches!(
                ensure_primary_identity(&first_login).unwrap(),
                ProvisionOutcome::Created { .. }
            ));
            assert!(founds_person_workspace(&first_login));
            clear_founding_mark(&first_login).unwrap();
            assert!(matches!(
                ensure_primary_identity(&first_login).unwrap(),
                ProvisionOutcome::Existing
            ));
            assert!(
                !founds_person_workspace(&first_login),
                "an identity already there is not made again"
            );

            let chosen = temp.path().join("chosen");
            create_primary_identity(&chosen, "Bea", [1, 2, 3], FILES).unwrap();
            assert!(founds_person_workspace(&chosen));
        }

        phase a_root_from_elsewhere_leaves_none;
        {
            let temp = tempfile::tempdir().unwrap();
            let identity = temp.path().join("identity");
            let root = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
            import_single_device_root(&identity, &root, "Ada", FILES).unwrap();
            assert!(
                !founds_person_workspace(&identity),
                "a root opened on a page founded its workspace there"
            );
        }
    }

    #[test]
    fn logging_in_as_the_person_whose_root_is_kept_here_keeps_acting_through_the_root() {
        let temp = tempfile::tempdir().unwrap();
        let identity = temp.path().join("identity");
        let paths = IdentityPaths::under(temp.path().join("device"));
        let person = create_person(&identity, &paths, "Ada", "laptop", FILES).unwrap();
        let mut cert = persist::read_device_cert(&paths.device_cert).unwrap();
        assert_eq!(cert.user_public_key, person);
        // The certificate a login ticket brings: this computer's key, as a device that admits none.
        cert.capabilities.can_admit_devices = false;
        let root = crate::store::load_from_path(identity.join(PRIMARY_IDENTITY_FILE)).unwrap();
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();

        adopt_login_certificate(&identity, &paths, &cert, tilo_clock::unix_now_ns()).unwrap();

        let active = crate::store::resolve_active_identity(&identity).unwrap();
        let active = crate::store::load_metadata_from_path(&active).unwrap();
        assert_eq!(active.group_public_key, person);
        assert!(
            active.single_device,
            "the login demoted the root kept here to a device record"
        );
    }

    tilo_test_env::scenario! {
        fn device_certificates_at_provisioning;

        phase a_device_certificate_the_clock_reads_before_is_a_clock_refusal_and_nothing_else;
        {
            let temp = tempfile::tempdir().unwrap();
            let paths = IdentityPaths::under(temp.path());
            assert_eq!(
                device_certificate_clock_refusal(&paths, 10),
                None,
                "no certificate"
            );
            let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1_000,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            persist::write_device_cert(&paths.device_cert, &cert).unwrap();

            let refusal = device_certificate_clock_refusal(&paths, 500).expect("the clock is behind");
            assert_eq!(refusal.valid_at_ns(), Some(1_000));
            assert_eq!(device_certificate_clock_refusal(&paths, 1_000), None);

            cert.device_label.push_str(" (tampered)");
            persist::write_device_cert(&paths.device_cert, &cert).unwrap();
            assert_eq!(
                device_certificate_clock_refusal(&paths, 500),
                None,
                "a forged certificate is not a clock problem"
            );
        }

        phase installer_device_label_is_signed_once_and_existing_ownership_is_unchanged;
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(crate::store::FILE_SECRETS_ENV, "1");
            let temp = tempfile::tempdir().unwrap();
            let identity = temp.path().join("identity");
            let paths = IdentityPaths::under(temp.path().join("system"));
            for label in [String::new(), "bad\nlabel".into(), "x".repeat(65)] {
                assert!(
                    ensure_collaboration_identity_with_device_label(&identity, &paths, FILES, &label)
                        .is_err()
                );
                assert!(!identity.exists());
                assert!(!paths.device_privkey.exists());
            }
            ensure_collaboration_identity_with_device_label(&identity, &paths, FILES, "Ada's laptop")
                .unwrap();
            let cert = persist::read_device_cert(&paths.device_cert).unwrap();
            tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns()).unwrap();
            assert_eq!(cert.device_label, "Ada's laptop");
            let keyed_path = paths.device_cert_for_user(&cert.user_public_key);
            assert_eq!(
                persist::read_device_cert(&keyed_path).unwrap().device_label,
                "Ada's laptop"
            );
            let originals: Vec<_> = [
                identity.join("primary.json"),
                paths.device_privkey.clone(),
                paths.user_pubkey.clone(),
                paths.device_cert.clone(),
                keyed_path,
            ]
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
            assert_eq!(
                ensure_collaboration_identity_with_device_label(
                    &identity,
                    &paths,
                    FILES,
                    "Replacement name"
                )
                .unwrap(),
                ProvisionOutcome::Existing
            );
            for (path, bytes) in originals {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
        }

        phase a_device_holding_the_root_is_never_capped_wherever_it_runs;
        {
            // The runtime plays an operating system Tilo does not control.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(super::TEST_PLATFORM_ENV, "macos");
            assert!(!super::runs_tilo_os());
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            ensure_collaboration_identity(&personal, &paths).unwrap();
            let held = device_certificates(&paths, &personal);
            for (_, cert) in &held {
                assert_eq!(cert.capabilities, DeviceCapabilities::primary());
            }

            // A capped certificate an earlier build gave the root's own device
            // is re-issued uncapped, keeping the device key and label.
            let (path, cert) = held.first().unwrap();
            let mut capped = cert.clone();
            capped.capabilities.foreign = true;
            let root = crate::store::load_from_path(
                crate::store::resolve_active_identity(&personal).unwrap(),
            )
            .unwrap();
            tilo_user_identity::sign_device_cert(&mut capped, &root).unwrap();
            for (path, _) in &held {
                persist::write_device_cert(path, &capped).unwrap();
            }
            ensure_collaboration_identity(&personal, &paths).unwrap();
            let after = persist::read_device_cert(path).unwrap();
            assert!(!after.capabilities.foreign);
            assert_eq!(after.device_public_key, cert.device_public_key);
            assert_eq!(after.device_label, cert.device_label);

            env.set(super::TEST_PLATFORM_ENV, super::TILO_OS_PLATFORM);
            assert!(super::runs_tilo_os());
        }
    }

    tilo_test_env::scenario! {
        fn an_unnamed_device_is_named_for_the_machine;

        phase the_pretty_hostname_then_the_hostname_then_the_chassis;
        {
            let root = tempfile::tempdir().unwrap();
            let write = |path: &str, text: &str| {
                let path = root.path().join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            };
            assert_eq!(machine_label(root.path()), None);
            write("sys/class/dmi/id/chassis_type", "10\n");
            assert_eq!(machine_label(root.path()).as_deref(), Some("Laptop"));
            write("proc/sys/kernel/hostname", "fedora\n");
            assert_eq!(machine_label(root.path()).as_deref(), Some("Laptop"));
            write("proc/sys/kernel/hostname", "tower\n");
            assert_eq!(machine_label(root.path()).as_deref(), Some("tower"));
            write("etc/machine-info", "CHASSIS=laptop\nPRETTY_HOSTNAME=\"Ada's \\\"work\\\" laptop\"\n");
            assert_eq!(machine_label(root.path()).as_deref(), Some("Ada's \"work\" laptop"));
        }
    }

    tilo_test_env::scenario! {
        fn setup_identities_and_device_ownership;

        phase explicit_prepared_identity_creation_refuses_existing_and_invalid_names;
        {
            let root = tempfile::tempdir().unwrap();
            assert!(matches!(
                create_primary_identity(root.path(), "Ada", [1, 2, 3], IdentityStorage::Files).unwrap(),
                ProvisionOutcome::Created { .. }
            ));
            let path = root.path().join(PRIMARY_IDENTITY_FILE);
            let original = std::fs::read(&path).unwrap();
            let metadata = tilo_identity_client::load_metadata_from_path(&path).unwrap();
            assert_eq!(metadata.display_name, "Ada");
            assert_eq!(metadata.avatar_color, [1, 2, 3]);
            for name in ["Replacement", "", "Ada\nOther"] {
                assert!(
                    create_primary_identity(root.path(), name, [9; 3], IdentityStorage::Files).is_err()
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            assert_eq!(
                tilo_identity_client::load_metadata_from_path(&path)
                    .unwrap()
                    .group_public_key,
                metadata.group_public_key
            );
        }

        phase prejoined_device_keeps_the_setup_identity;
        {
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

            // Every consumer resolves the person through the account's active
            // record: an `Existing` with nothing to resolve strands the session
            // in recovery although the enrollment is sound.
            let held_by_account = || {
                let record = crate::store::resolve_active_identity(&identity_dir)
                    .expect("the joined person has a record the account resolves");
                tilo_identity_client::load_metadata_from_path(&record)
                    .unwrap()
                    .group_public_key
            };
            let outcome = ensure_collaboration_identity(&identity_dir, &paths).unwrap();
            assert_eq!(outcome, ProvisionOutcome::Existing);
            assert_eq!(held_by_account(), root.verifying_key().to_bytes());
            assert!(!identity_dir.join(PRIMARY_IDENTITY_FILE).exists());
            assert!(persist::joined_device_is_ready(&paths, tilo_clock::unix_now_ns()).unwrap());

            // The account's config is reset while the system-wide enrollment
            // survives: the person is taken up again, never replaced.
            fs::remove_dir_all(&identity_dir).unwrap();
            let outcome = ensure_collaboration_identity(&identity_dir, &paths).unwrap();
            assert_eq!(outcome, ProvisionOutcome::Existing);
            assert_eq!(held_by_account(), root.verifying_key().to_bytes());
        }

        phase only_the_identity_that_claimed_the_device_owns_it;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            let alice = tmp.path().join("alice/tilo/identity");
            let bob = tmp.path().join("bob/tilo/identity");
            let carol = tmp.path().join("carol/tilo/identity");
            // Before anyone's first login there is no owner.
            assert!(!active_identity_owns_device(&alice, &paths).expect("no identity yet"));
            ensure_collaboration_identity(&alice, &paths).expect("alice claims the device");
            ensure_collaboration_identity(&bob, &paths).expect("bob shares it");
            assert!(active_identity_owns_device(&alice, &paths).expect("alice"));
            assert!(!active_identity_owns_device(&bob, &paths).expect("bob"));
            // An account that never finished first login is not the owner either.
            assert!(!active_identity_owns_device(&carol, &paths).expect("carol"));
            // A primary certificate that no longer verifies is an error, not a yes.
            let mut cert = persist::read_device_cert(&paths.device_cert).expect("cert");
            cert.device_label = "Tampered".into();
            persist::write_device_cert(&paths.device_cert, &cert).expect("rewrite cert");
            assert!(active_identity_owns_device(&alice, &paths).is_err());
        }
    }

    tilo_test_env::scenario! {
        #[cfg(unix)]
        fn a_joined_device_takes_up_its_person;

        phase a_nearby_paired_member_verifies_through_its_certificate;
        // A device approved from a nearby device: a public-only record selected
        // as the account's identity and an ordinary member certificate.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let dir = tempfile::tempdir().unwrap();
            let identity_dir = dir.path().join("identity");
            let paths = IdentityPaths::under(dir.path().join("system"));
            let (device, _) = persist::load_or_create_device_key(&paths).unwrap();
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            persist::install_device_cert(&paths, &cert, 2_000).unwrap();
            prepare_identity_directory(&identity_dir).unwrap();
            let write_record = |root_public: [u8; 32]| {
                let record = identity_dir.join("identity-joined.json");
                fs::write(
                    &record,
                    crate::store::public_member_record("Member", [1, 2, 3], root_public).unwrap(),
                )
                .unwrap();
                fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
                let active = identity_dir.join("active");
                fs::write(&active, "identity-joined").unwrap();
                fs::set_permissions(&active, fs::Permissions::from_mode(0o600)).unwrap();
            };

            write_record(root.verifying_key().to_bytes());
            verify_collaboration_identity(&identity_dir, &paths, 2_000).unwrap();
            assert_eq!(
                ensure_collaboration_identity(&identity_dir, &paths).unwrap(),
                ProvisionOutcome::Existing
            );

            // A record naming another identity is not this device's.
            write_record([7; 32]);
            let error = verify_collaboration_identity(&identity_dir, &paths, 2_000).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        phase an_enrolled_session_takes_up_its_person_as_a_joined_device;
        // A node's session, given only its certificate and the person's key by
        // the server its page installed, holds that person as a joined device.
        {
            let root = ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng));
            let user = root.verifying_key().to_bytes();
            let dir = tempfile::tempdir().unwrap();
            let identity_dir = dir.path().join("identity");
            let paths = IdentityPaths::under(dir.path().join("session"));
            assert!(!complete_enrolled_device(&identity_dir, &paths, 2_000).unwrap());

            let (device, _) = persist::load_or_create_device_key(&paths).unwrap();
            let mut cert = DeviceCert {
                user_public_key: user,
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Apps on node".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1_000,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            persist::write_device_cert(&paths.device_cert, &cert).unwrap();
            persist::write_public_key(&paths.user_pubkey, &user).unwrap();

            assert!(complete_enrolled_device(&identity_dir, &paths, 2_000).unwrap());
            assert!(complete_enrolled_device(&identity_dir, &paths, 2_000).unwrap());
            verify_collaboration_identity(&identity_dir, &paths, 2_000).unwrap();
            let registry = crate::store::IdentityRegistryStore::at(&identity_dir)
                .load_reconciled()
                .unwrap();
            assert_eq!(registry.default_identity, Some(hex::encode(user)));
            let fleet = crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths)
                .load(&hex::encode(user))
                .unwrap();
            assert!(fleet.certificates.contains(&cert));
        }
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

    use std::sync::{Arc, Barrier};

    tilo_test_env::scenario! {
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        fn throwaway_identities_keep_apart_from_real_trust;

        phase throwaway_refuses_the_shared_device_tree_before_writing_any_identity;
        {
            let tmp = tempfile::tempdir().unwrap();
            let personal = tmp.path().join("personal");
            let error =
                ensure_throwaway_test_identity(&personal, &IdentityPaths::default_system(), FILES)
                    .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("private TILO_IDENTITY_ROOT"));
            assert!(!personal.exists());
            #[cfg(unix)]
            {
                let alias = tmp.path().join("shared-alias");
                std::os::unix::fs::symlink("/var/lib/tilo", &alias).unwrap();
                let error =
                    ensure_throwaway_test_identity(&personal, &IdentityPaths::under(alias), FILES)
                        .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert!(
                    !personal.exists(),
                    "a shared-tree alias also writes nothing"
                );
            }
        }

        phase disposable_home_contains_its_own_expiring_chain_and_preserves_real_trust;
        {
            let tmp = tempfile::tempdir().unwrap();
            let real = IdentityPaths::under(tmp.path().join("real-system"));
            let real_personal = tmp.path().join("owner/identity");
            ensure_collaboration_identity(&real_personal, &real).unwrap();
            let before = fs::read(&real.device_cert).unwrap();
            let real_key = fs::read(&real.device_privkey).unwrap();
            let real_cert = persist::read_device_cert(&real.device_cert).unwrap();
            assert_eq!(real_cert.expires_at_ns, None);
            let mut keys = Vec::new();
            for slot in ["a", "b"] {
                let home = tmp.path().join(slot);
                let personal = home.join(".config/tilo/identity");
                let paths = IdentityPaths::under(home.join(".local/share/tilo/test-identity"));
                ensure_throwaway_test_identity(&personal, &paths, IdentityStorage::Platform).unwrap();
                let record: serde_json::Value =
                    serde_json::from_slice(&fs::read(personal.join(PRIMARY_IDENTITY_FILE)).unwrap())
                        .unwrap();
                assert_eq!(record["secret_ref"]["backend"], "broker-file-v1");
                let cert = persist::read_device_cert(&paths.device_cert).unwrap();
                let deadline = cert.issued_at_ns + 24 * 60 * 60 * 1_000_000_000;
                assert_eq!(cert.expires_at_ns, Some(deadline));
                tilo_user_identity::verify_device_cert_at(&cert, cert.issued_at_ns).unwrap();
                assert!(tilo_user_identity::verify_device_cert_at(&cert, deadline).is_err());
                assert_ne!(cert.device_public_key, real_cert.device_public_key);
                keys.push(cert.device_public_key);
                assert!(paths.device_cert_for_user(&cert.user_public_key).is_file());
                assert_eq!(
                    ensure_throwaway_test_identity(&personal, &paths, FILES).unwrap(),
                    ProvisionOutcome::Existing
                );
            }
            assert_ne!(
                keys[0], keys[1],
                "each test slot is a separate disposable device"
            );
            fs::remove_dir_all(tmp.path().join("a")).unwrap();
            assert!(
                !tmp.path()
                    .join("a/.local/share/tilo/test-identity")
                    .exists()
            );
            assert!(
                tmp.path()
                    .join("b/.local/share/tilo/test-identity/device/cert.bin")
                    .exists()
            );
            assert_eq!(fs::read(&real.device_cert).unwrap(), before);
            assert_eq!(fs::read(&real.device_privkey).unwrap(), real_key);
            assert_eq!(
                fs::read_dir(real.device_cert.parent().unwrap().join("certs"))
                    .unwrap()
                    .count(),
                1
            );
        }
    }

    tilo_test_env::scenario! {
        #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
        fn a_throwaway_test_identity_is_marked_and_never_replaces_a_real_one;

        phase a_throwaway_identity_is_named_and_marked;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));
            assert!(matches!(
                ensure_throwaway_test_identity(&personal, &paths, FILES).expect("provision"),
                ProvisionOutcome::Created { .. }
            ));
            assert!(is_throwaway_test_identity(&personal));
            let identity = crate::store::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
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

        phase a_real_identity_is_refused_and_left_untouched;
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

    tilo_test_env::scenario! {
        /// Minimal tempdir helper (avoids pulling `tempfile` as a dev-dep — the only test in `quick-shell` that needs one).
        #[cfg(unix)]
        fn identity_files_are_private_and_refuse_symlinks;

        phase identity_directory_preflight_refuses_a_symlink;
        {
            use std::os::unix::fs::symlink;

            let tmp = tempfile::tempdir().expect("tempdir");
            let target = tmp.path().join("target");
            fs::create_dir(&target).unwrap();
            let linked = tmp.path().join("linked");
            symlink(&target, &linked).unwrap();
            assert!(prepare_identity_directory(&linked).is_err());
        }

        phase collaboration_verifier_rejects_a_permissive_primary_secret;
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

        phase ensure_primary_identity_writes_a_private_file;
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

    tilo_test_env::scenario! {
        fn primary_identity_is_created_once_and_legacy_hosts_are_associated_not_relabelled;

        phase ensure_primary_identity_reports_existing_vs_created;
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
            let identity = crate::store::load_from_path(dir.join(PRIMARY_IDENTITY_FILE))
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
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_partial_identity_resumes_beneath_atomic_fedora_home_link() {
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
        let signer = crate::store::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
            .expect("load seeded identity");
        let linked_root = tmp.path().join("home/alice/.local/share/tilo");
        let paths = IdentityPaths::under(&linked_root);
        paths.ensure_directories().unwrap();
        persist::write_public_key(&paths.user_pubkey, &signer.group_public_key()).unwrap();
        assert!(!paths.device_privkey.exists());

        assert_eq!(
            ensure_collaboration_identity(&personal, &paths)
                .expect("resume the partial host identity"),
            ProvisionOutcome::Existing
        );
        let cert = persist::read_device_cert(&paths.device_cert).expect("read completed cert");
        verify_collaboration_identity(&personal, &paths, cert.issued_at_ns)
            .expect("resumed identity chain verifies");
        assert_eq!(
            crate::store::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("reload personal identity")
                .group_public_key(),
            signer.group_public_key(),
            "resuming must never mint a replacement personal identity"
        );
        assert!(identity_root.join("device-private/keypair.bin").is_file());
    }

    fn device_certificates(paths: &IdentityPaths, personal: &Path) -> [(PathBuf, DeviceCert); 2] {
        let root = crate::store::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
            .unwrap()
            .group_public_key();
        [paths.device_cert.clone(), paths.device_cert_for_user(&root)].map(|path| {
            let cert = persist::read_device_cert(&path).unwrap();
            tilo_user_identity::verify_device_cert(&cert).unwrap();
            (path, cert)
        })
    }

    tilo_test_env::scenario! {
        fn collaboration_setup_builds_and_verifies_one_identity_chain;

        phase collaboration_setup_creates_one_verified_identity_chain;
        {
            let tmp = tempfile::tempdir().expect("tempdir");
            let personal = tmp.path().join("config/tilo/identity");
            let paths = IdentityPaths::under(tmp.path().join("system"));

            assert!(matches!(
                ensure_collaboration_identity(&personal, &paths).expect("provision identity"),
                ProvisionOutcome::Created { .. }
            ));
            let signer = crate::store::load_from_path(personal.join(PRIMARY_IDENTITY_FILE))
                .expect("load protected identity");
            let device = persist::read_device_key(&paths.device_privkey).unwrap();
            let cert = persist::read_device_cert(&paths.device_cert).unwrap();

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

        phase collaboration_verifier_rejects_a_mismatched_personal_root;
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

        phase collaboration_setup_repairs_the_active_import_without_minting_primary;
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
            crate::store::set_active_identity(&personal, "identity-imported")
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
                crate::store::load_from_path(imported).expect("load imported identity");
            let cert =
                persist::read_device_cert(&paths.device_cert_for_user(&signer.group_public_key()))
                    .expect("identity-specific certificate");
            tilo_user_identity::verify_device_cert(&cert).expect("valid imported chain");
        }
    }

    tilo_test_env::scenario! {
        fn concurrent_and_multi_user_provisioning_keeps_one_device_owner;

        phase multiple_os_users_share_one_device_without_replacing_its_owner;
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

        phase simultaneous_user_provisioning_builds_one_consistent_device_chain;
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
                    crate::store::load_from_path(identity_dir.join(PRIMARY_IDENTITY_FILE))
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

        phase simultaneous_owner_claims_elect_one_valid_winner_without_clobbering;
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

        phase an_established_claim_is_answered_without_writing;
        // Phase `an_established_claim_is_answered_without_writing`: every
        // login asks again, and a full disk must not fail it. With the claim
        // directory read-only, staging a new claim would fail; reading the
        // established one does not.
        {
            use std::os::unix::fs::PermissionsExt as _;
            let tmp = tempfile::tempdir().expect("tempdir");
            let parent = tmp.path().join("system/user-identity");
            std::fs::create_dir_all(&parent).expect("claim parent");
            let path = parent.join("pubkey.bin");
            let owner = [0xC3; 32];
            assert!(claim_device_owner(&path, &owner).expect("first claim"));
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500))
                .expect("read-only claim directory");
            let again = claim_device_owner(&path, &owner);
            let other = claim_device_owner(&path, &[0xD4; 32]);
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
                .expect("writable again for cleanup");
            assert!(again.expect("the owner is recognised without a write"));
            assert!(!other.expect("another root is refused without a write"));
            let entries: Vec<_> = std::fs::read_dir(&parent)
                .expect("claim directory")
                .map(|entry| entry.expect("entry").file_name())
                .collect();
            assert_eq!(entries, vec![std::ffi::OsString::from("pubkey.bin")]);
        }
    }
}
