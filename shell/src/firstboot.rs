//! First-boot setup, invoked by `shell-firstboot.service` as
//! `/usr/bin/shell --firstboot-only`.
//!
//! The systemd unit guards re-runs with a `ConditionPathExists` on
//! `/etc/qos/.firstboot-done`, so this module is a one-shot "make sure the
//! user-facing config dir exists" hook. It is cheap and idempotent. It never
//! creates a personal signing identity: that is an acknowledged interactive
//! ceremony owned by the compositor OOBE.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use qos_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths, persist};
use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;
use ui::widget::identity_flow::IdentityRecord;

/// Human-facing name of the Unix account running this session.
///
/// This is only used by the compatibility path where qDesktop is installed
/// on top of an existing Linux distribution. Native qOS setup has its own
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
const PRIMARY_IDENTITY_FILE: &str = "primary.json";

/// Sequence for same-directory temporary owner-claim files.  The final claim
/// is a hard link, not this name; `create_new` simply prevents a stale temp
/// file from ever being overwritten.
static OWNER_CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Process-global entry point. Resolves the user's XDG config dir from
/// `$XDG_CONFIG_HOME` (falling back to `$HOME/.config`) and creates
/// `quick-desktop/` under it. Returns `Ok(())` on success and on the "no HOME / no XDG" no-op path so boot continues.
///
/// # Errors
///
/// Returns an error when directory creation itself fails (permission denied, ENOSPC); the caller (`main`) logs and continues.
pub fn run() -> Result<(), Box<dyn Error>> {
    run_with_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Import existing GNOME input preferences on the first host-session login.
/// All keyboard, mouse, and touchpad values are collected and persisted in one
/// settings transaction. Existing qOS choices always win.
///
/// # Errors
///
/// Returns an error when the shared settings store cannot persist the imported
/// values. Missing schemas or unreadable host values are ignored individually.
pub fn import_host_input_settings() -> Result<(), Box<dyn Error>> {
    let existing = settings_store::load_section("input");
    let mut input = host_keyboard_updates(&existing, gsettings_get);
    input.extend(host_pointer_updates(&existing, gsettings_get));
    if input.is_empty() {
        return Ok(());
    }
    settings_store::update_section("input", input)?;
    tracing::info!("firstboot: imported host input settings");
    Ok(())
}

fn host_keyboard_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    mut read: impl FnMut(&str, &str) -> Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = serde_json::Map::new();
    if existing.contains_key("keyboard_layout") || existing.contains_key("keyboard_layouts") {
        return input;
    }
    let Some(raw_sources) = read("org.gnome.desktop.input-sources", "sources") else {
        return input;
    };
    let source_ids = parse_gnome_xkb_sources(&raw_sources);
    let Some(active) = source_ids.first() else {
        return input;
    };
    input.insert("keyboard_layout".into(), serde_json::json!(active));
    input.insert("keyboard_layouts".into(), serde_json::json!(source_ids));
    if !existing.contains_key("compose_key")
        && let Some(compose) = read("org.gnome.desktop.input-sources", "xkb-options")
            .and_then(|raw| parse_gnome_compose_key(&raw))
    {
        input.insert("compose_key".into(), serde_json::json!(compose));
    }
    input
}

fn parse_gnome_xkb_sources(value: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("('xkb', '") {
        rest = &rest[start + 9..];
        let Some(end) = rest.find('\'') else { break };
        let id = rest[..end].replacen('+', ":", 1);
        if settings_store::hardware::KeyboardInputSource::parse(&id).is_some()
            && !sources.contains(&id)
            && sources.len() < settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
        {
            sources.push(id);
        }
        rest = &rest[end + 1..];
    }
    sources
}

fn parse_gnome_compose_key(value: &str) -> Option<String> {
    ["ralt", "menu", "caps", "rwin", "lwin"]
        .into_iter()
        .find(|key| value.contains(&format!("compose:{key}")))
        .map(str::to_string)
}

fn host_pointer_updates(
    existing: &serde_json::Map<String, serde_json::Value>,
    mut read: impl FnMut(&str, &str) -> Option<String>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut input = serde_json::Map::new();
    {
        let mut insert_bool = |key: &str, legacy_key: Option<&str>, schema: &str, setting: &str| {
            if !existing.contains_key(key)
                && legacy_key.is_none_or(|legacy| !existing.contains_key(legacy))
                && let Some(value) = read(schema, setting).and_then(|raw| match raw.trim() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                })
            {
                input.insert(key.into(), serde_json::json!(value));
            }
        };
        insert_bool(
            "touchpad_natural_scroll",
            Some("natural_scroll"),
            "org.gnome.desktop.peripherals.touchpad",
            "natural-scroll",
        );
        insert_bool(
            "mouse_natural_scroll",
            Some("natural_scroll"),
            "org.gnome.desktop.peripherals.mouse",
            "natural-scroll",
        );
        insert_bool(
            "tap_to_click",
            None,
            "org.gnome.desktop.peripherals.touchpad",
            "tap-to-click",
        );
        insert_bool(
            "tap_drag",
            None,
            "org.gnome.desktop.peripherals.touchpad",
            "tap-and-drag",
        );
        insert_bool(
            "disable_while_typing",
            None,
            "org.gnome.desktop.peripherals.touchpad",
            "disable-while-typing",
        );
        insert_bool(
            "left_handed",
            None,
            "org.gnome.desktop.peripherals.mouse",
            "left-handed",
        );
    }

    for (key, schema) in [
        (
            "touchpad_pointer_speed",
            "org.gnome.desktop.peripherals.touchpad",
        ),
        ("mouse_pointer_speed", "org.gnome.desktop.peripherals.mouse"),
    ] {
        if existing.contains_key(key) || existing.contains_key("pointer_speed") {
            continue;
        }
        if let Some(speed) = read(schema, "speed")
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|speed| speed.is_finite())
        {
            input.insert(key.into(), serde_json::json!(host_pointer_speed(speed)));
        }
    }
    input
}

fn host_pointer_speed(speed: f64) -> f64 {
    speed.midpoint(1.0).clamp(0.0, 1.0)
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

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

/// Pure form of [`run`] that takes env-var values as arguments instead
/// of reading the real environment — tests drive this with a `tempdir`-rooted fixture so the suite never pokes the developer's real `$HOME`.
///
/// # Errors
///
/// Returns an error when directory creation fails (permission denied, ENOSPC).
pub fn run_with_env(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<(), Box<dyn Error>> {
    let Some(config_dir) = resolve_config_dir(xdg_config_home, home) else {
        tracing::warn!(
            "firstboot: neither XDG_CONFIG_HOME nor HOME set; skipping config-dir creation"
        );
        return Ok(());
    };
    ensure_dirs(&config_dir)?;
    tracing::info!(
        "firstboot: ensured quick-desktop config dir at {}",
        config_dir.display()
    );
    Ok(())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityProvisioningContext {
    /// Native qOS: the dedicated OOBE owns the identity and storage policy.
    NativeQos,
    /// qDesktop added to a pre-existing Linux distribution and user account.
    LegacyLinuxHost,
}

/// Compute the quick-desktop config dir (`<config-base>/quick-desktop`) following the same XDG fallback rules the peer-sync marker uses.
fn resolve_config_dir(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        let mut p = PathBuf::from(xdg);
        p.push("quick-desktop");
        return Some(p);
    }
    let home = home.filter(|v| !v.is_empty())?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("quick-desktop");
    Some(p)
}

fn ensure_dirs(config_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    Ok(())
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
pub fn ensure_primary_identity(identity_dir: &Path) -> Result<ProvisionOutcome, io::Error> {
    ensure_primary_identity_in_context(identity_dir, IdentityProvisioningContext::NativeQos)
}

fn ensure_primary_identity_in_context(
    identity_dir: &Path,
    context: IdentityProvisioningContext,
) -> Result<ProvisionOutcome, io::Error> {
    let path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    if path.exists() {
        if context == IdentityProvisioningContext::NativeQos {
            protect_identity_record(identity_dir)?;
        }
        return Ok(ProvisionOutcome::Existing);
    }

    // Mint through the same encoder the interactive ceremony uses, so
    // there's exactly one writer of the on-disk record — the two provisioning paths can't drift apart in what they store.
    let mut rng = OsRng;
    let identity = ThresholdIdentity::bootstrap_single_device(&mut rng)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let pubkey_hex = hex_encode(identity.group_public_key().as_bytes());
    let display_name = match context {
        IdentityProvisioningContext::NativeQos => String::new(),
        IdentityProvisioningContext::LegacyLinuxHost => login_account_display_name(),
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

    import_primary_identity(identity_dir, &bytes)?;
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
) -> Result<ProvisionOutcome, io::Error> {
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::NativeQos,
    )
}

/// Provision collaboration identity for qDesktop installed on an existing
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
) -> Result<ProvisionOutcome, io::Error> {
    ensure_collaboration_identity_in_context(
        identity_dir,
        paths,
        IdentityProvisioningContext::LegacyLinuxHost,
    )
}

/// Prepare the private per-user identity directory before either the key
/// transaction or its first-use crash journal is written. Keeping this as one
/// operation prevents the journal preflight and the identity writer from
/// disagreeing about directory permissions.
pub(crate) fn prepare_identity_directory(identity_dir: &Path) -> io::Result<()> {
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
) -> Result<ProvisionOutcome, io::Error> {
    prepare_identity_directory(identity_dir)?;

    let primary_path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    let (selected_path, outcome) =
        if let Some(path) = runtime_identity::resolve_active_identity(identity_dir) {
            if context == IdentityProvisioningContext::NativeQos {
                protect_identity_record_at(identity_dir, &path)?;
            }
            (path, ProvisionOutcome::Existing)
        } else {
            let outcome = ensure_primary_identity_in_context(identity_dir, context)?;
            (primary_path, outcome)
        };

    let signer = runtime_identity::load_from_path(selected_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
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
            .map_err(|e| io::Error::other(e.to_string()))?;
        qos_user_identity::verify_device_cert(&cert)
            .map_err(|e| io::Error::other(e.to_string()))?;
        if cert.user_public_key != root_public
            || cert.device_public_key != device.verifying_key().to_bytes()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "device certificate does not match the provisioned identity keys",
            ));
        }
    } else if device_owner {
        let issued_at_ns = now_ns_u64();
        let mut cert = DeviceCert {
            user_public_key: root_public,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Primary device".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
        };
        let digest = qos_user_identity::device_cert_signing_digest(&cert)
            .map_err(|e| io::Error::other(e.to_string()))?;
        cert.signature = signer
            .sign(&digest)
            .map_err(|e| io::Error::other(e.to_string()))?;
        qos_user_identity::verify_device_cert(&cert)
            .map_err(|e| io::Error::other(e.to_string()))?;
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
    )?;

    // Keep the legacy primary path working while making it the first entry in
    // the multi-identity registry. A registry write failure is a provisioning
    // failure: silently creating a key that no workspace can select strands
    // the identity immediately.
    let registry = crate::identity_registry::IdentityRegistryStore::at(identity_dir)
        .migrate_legacy_primary()?;
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
/// avoid.  qOS stores this identity tree on a local Linux filesystem, where
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
            ".qos-owner-claim-{}-{sequence}",
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
    ensure_device_certificate_for_identity(&identity, paths, "This device")
}

fn ensure_device_certificate_for_identity(
    identity: &runtime_identity::SigningIdentity,
    paths: &IdentityPaths,
    device_label: &str,
) -> Result<PathBuf, io::Error> {
    let (device, _) =
        persist::load_or_create_device_key(paths).map_err(|e| io::Error::other(e.to_string()))?;
    let user_public_key = identity.group_public_key();
    let cert_path = paths.device_cert_for_user(&user_public_key);
    if cert_path.exists() {
        let cert =
            persist::read_device_cert(&cert_path).map_err(|e| io::Error::other(e.to_string()))?;
        qos_user_identity::verify_device_cert(&cert)
            .map_err(|e| io::Error::other(e.to_string()))?;
        if cert.user_public_key != user_public_key
            || cert.device_public_key != device.verifying_key().to_bytes()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity-specific device certificate does not match its keys",
            ));
        }
        return Ok(cert_path);
    }

    let issued_at_ns = now_ns_u64();
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: device_label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    let digest = qos_user_identity::device_cert_signing_digest(&cert)
        .map_err(|e| io::Error::other(e.to_string()))?;
    cert.signature = identity
        .sign(&digest)
        .map_err(|e| io::Error::other(e.to_string()))?;
    qos_user_identity::verify_device_cert(&cert).map_err(|e| io::Error::other(e.to_string()))?;
    persist::write_device_cert(&cert_path, &cert).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(cert_path)
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
    let primary_path = identity_dir.join(PRIMARY_IDENTITY_FILE);
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
    let device = persist::read_device_key(&paths.device_privkey)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let cert = persist::read_device_cert(&paths.device_cert)
        .map_err(|e| io::Error::other(e.to_string()))?;
    qos_user_identity::verify_device_cert_at(&cert, now_ns)
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
/// used by the filesystem synchroniser.
pub fn collaboration_identity_paths() -> IdentityPaths {
    std::env::var_os("QOS_IDENTITY_ROOT")
        .filter(|value| !value.is_empty())
        .map_or_else(IdentityPaths::default_system, IdentityPaths::under)
}

fn import_primary_identity(identity_dir: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(identity_dir);
        runtime_identity::import_identity_with_provider(identity_dir, "primary", bytes, &provider)
    }
    #[cfg(not(test))]
    {
        runtime_identity::import_identity(identity_dir, "primary", bytes)
    }
}

fn protect_identity_record(identity_dir: &Path) -> io::Result<()> {
    protect_identity_record_at(identity_dir, &identity_dir.join(PRIMARY_IDENTITY_FILE))
}

fn protect_identity_record_at(identity_dir: &Path, record_path: &Path) -> io::Result<()> {
    let id = record_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "identity has no valid id"))?;
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(identity_dir);
        runtime_identity::secure_identity_with_provider(identity_dir, id, &provider)?;
    }
    #[cfg(not(test))]
    {
        runtime_identity::secure_identity(identity_dir, id)?;
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(b & 0xF), 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_pointer_speed_maps_libinput_range_to_qos_range() {
        assert_eq!(host_pointer_speed(-1.0), 0.0);
        assert_eq!(host_pointer_speed(0.0), 0.5);
        assert_eq!(host_pointer_speed(1.0), 1.0);
        assert_eq!(host_pointer_speed(4.0), 1.0);
    }

    #[test]
    fn host_pointer_import_preserves_legacy_qos_choices() {
        let existing = serde_json::Map::from_iter([
            ("natural_scroll".into(), serde_json::json!(false)),
            ("pointer_speed".into(), serde_json::json!(0.8)),
        ]);
        let updates = host_pointer_updates(&existing, |_, key| match key {
            "natural-scroll" => Some("true".into()),
            "speed" => Some("-0.5".into()),
            "tap-to-click" => Some("false".into()),
            _ => None,
        });
        assert!(!updates.contains_key("touchpad_natural_scroll"));
        assert!(!updates.contains_key("mouse_natural_scroll"));
        assert!(!updates.contains_key("touchpad_pointer_speed"));
        assert!(!updates.contains_key("mouse_pointer_speed"));
        assert_eq!(updates.get("tap_to_click"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn host_pointer_import_keeps_device_classes_independent() {
        let existing = serde_json::Map::new();
        let updates = host_pointer_updates(&existing, |schema, key| match (schema, key) {
            ("org.gnome.desktop.peripherals.touchpad", "natural-scroll") => Some("true".into()),
            ("org.gnome.desktop.peripherals.mouse", "natural-scroll") => Some("false".into()),
            ("org.gnome.desktop.peripherals.touchpad", "speed") => Some("0.4".into()),
            ("org.gnome.desktop.peripherals.mouse", "speed") => Some("-0.6".into()),
            _ => None,
        });
        assert_eq!(
            updates.get("touchpad_natural_scroll"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            updates.get("mouse_natural_scroll"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            updates.get("touchpad_pointer_speed"),
            Some(&serde_json::json!(0.7))
        );
        assert_eq!(
            updates.get("mouse_pointer_speed"),
            Some(&serde_json::json!(0.2))
        );
    }

    #[test]
    fn host_keyboard_import_is_complete_and_never_overwrites_qos_choices() {
        let empty = serde_json::Map::new();
        let imported = host_keyboard_updates(&empty, |_, key| match key {
            "sources" => Some("[('xkb', 'de+nodeadkeys'), ('xkb', 'us')]".into()),
            "xkb-options" => Some("['compose:rwin']".into()),
            _ => None,
        });
        assert_eq!(
            imported.get("keyboard_layout"),
            Some(&serde_json::json!("de:nodeadkeys"))
        );
        assert_eq!(
            imported.get("keyboard_layouts"),
            Some(&serde_json::json!(["de:nodeadkeys", "us"]))
        );
        assert_eq!(
            imported.get("compose_key"),
            Some(&serde_json::json!("rwin"))
        );

        let existing = serde_json::Map::from_iter([
            ("keyboard_layout".into(), serde_json::json!("us")),
            ("compose_key".into(), serde_json::json!("caps")),
        ]);
        assert!(
            host_keyboard_updates(&existing, |_, _| Some("[('xkb', 'de')]".into())).is_empty(),
            "an existing qOS keyboard selection must win as one coherent preference"
        );
    }
    use std::ffi::OsString;
    use std::sync::{Arc, Barrier};

    /// Minimal tempdir helper (avoids pulling `tempfile` as a dev-dep — the only test in `quick-shell` that needs one).
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = base.join(format!("quick-desktop-firstboot-{name}-{pid}-{nonce}"));
            std::fs::create_dir_all(&path).expect("create tmpdir");
            TmpDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn run_with_xdg_creates_quick_desktop_dir() {
        let tmp = TmpDir::new("xdg");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot succeeds");
        let expected = tmp.path().join("quick-desktop");
        assert!(
            expected.exists() && expected.is_dir(),
            "expected {expected:?} to be created"
        );
    }

    #[test]
    fn run_with_home_falls_back_to_dot_config() {
        let tmp = TmpDir::new("home");
        run_with_env(None, Some(OsString::from(tmp.path()))).expect("firstboot succeeds via HOME");
        let expected = tmp.path().join(".config").join("quick-desktop");
        assert!(
            expected.exists() && expected.is_dir(),
            "expected {expected:?} to be created"
        );
    }

    #[test]
    fn run_is_idempotent_on_existing_dir() {
        let tmp = TmpDir::new("idempotent");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("first run ok");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("second run ok");
        let expected = tmp.path().join("quick-desktop");
        assert!(expected.exists());
    }

    #[test]
    fn run_without_env_is_a_noop_not_an_error() {
        // Systemd `Type=oneshot` units fail the whole transaction if ExecStart
        // returns non-zero — first-boot must never break boot over missing env-vars in the unit context.
        run_with_env(None, None).expect("no-env case is a no-op, not an error");
    }

    #[test]
    fn gnome_xkb_sources_keep_order_and_variants() {
        assert_eq!(
            parse_gnome_xkb_sources(
                "[('xkb', 'de+nodeadkeys'), ('ibus', 'typing-booster'), ('xkb', 'us')]"
            ),
            ["de:nodeadkeys", "us"]
        );
    }

    #[test]
    fn gnome_compose_option_maps_to_the_shared_settings_schema() {
        assert_eq!(
            parse_gnome_compose_key("['grp:alt_shift_toggle', 'compose:rwin']").as_deref(),
            Some("rwin")
        );
        assert_eq!(parse_gnome_compose_key("@as []"), None);
    }

    #[test]
    fn run_does_not_provision_identity_when_missing() {
        let tmp = TmpDir::new("no-mint-identity");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot ok");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        assert!(
            !identity_file.exists(),
            "firstboot must leave personal identity creation to the OOBE"
        );
    }

    #[test]
    fn run_does_not_touch_an_existing_identity() {
        let tmp = TmpDir::new("existing-identity");
        let dir = tmp.path().join("qos").join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        let identity_file = dir.join(PRIMARY_IDENTITY_FILE);
        std::fs::write(&identity_file, b"MARKER: hands off").unwrap();
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot ok");
        assert_eq!(std::fs::read(identity_file).unwrap(), b"MARKER: hands off");
    }

    #[cfg(unix)]
    #[test]
    fn identity_directory_preflight_refuses_a_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TmpDir::new("identity-dir-symlink");
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let linked = tmp.path().join("linked");
        symlink(&target, &linked).unwrap();
        assert!(prepare_identity_directory(&linked).is_err());
    }

    #[test]
    fn ensure_primary_identity_reports_existing_vs_created() {
        let tmp = TmpDir::new("outcome");
        let dir = tmp.path().join("qos").join("identity");
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
            "native qOS must not infer personal identity from its Unix implementation account"
        );
        // Second run leaves it alone.
        assert_eq!(
            ensure_primary_identity(&dir).expect("provision ok (2nd)"),
            ProvisionOutcome::Existing
        );
    }

    #[test]
    fn legacy_linux_host_associates_a_fresh_identity_with_the_unix_account() {
        let tmp = TmpDir::new("legacy-host-account");
        let personal = tmp.path().join("config/qos/identity");
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

    #[test]
    fn legacy_linux_host_does_not_relabel_or_migrate_an_existing_identity() {
        let tmp = TmpDir::new("legacy-host-existing");
        let personal = tmp.path().join("config/qos/identity");
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

    #[test]
    fn collaboration_setup_creates_one_verified_identity_chain() {
        let tmp = TmpDir::new("collaboration-identity");
        let personal = tmp.path().join("config/qos/identity");
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
        qos_user_identity::verify_device_cert(&cert).unwrap();
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

    #[test]
    fn collaboration_verifier_rejects_a_mismatched_personal_root() {
        let tmp = TmpDir::new("collaboration-identity-mismatch");
        let personal = tmp.path().join("config/qos/identity");
        let other_personal = tmp.path().join("other/qos/identity");
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

    #[cfg(unix)]
    #[test]
    fn collaboration_verifier_rejects_a_permissive_primary_secret() {
        let tmp = TmpDir::new("collaboration-identity-permissions");
        let personal = tmp.path().join("config/qos/identity");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        ensure_collaboration_identity(&personal, &paths).unwrap();

        let primary = personal.join(PRIMARY_IDENTITY_FILE);
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = verify_collaboration_identity(&personal, &paths, u64::MAX)
            .expect_err("a group-readable personal seed must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn collaboration_setup_repairs_the_active_import_without_minting_primary() {
        let tmp = TmpDir::new("active-import");
        let personal = tmp.path().join("config/qos/identity");
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
        let signer = runtime_identity::load_from_path(imported).expect("load imported identity");
        let cert =
            persist::read_device_cert(&paths.device_cert_for_user(&signer.group_public_key()))
                .expect("identity-specific certificate");
        qos_user_identity::verify_device_cert(&cert).expect("valid imported chain");
    }

    #[test]
    fn multiple_os_users_share_one_device_without_replacing_its_owner() {
        let tmp = TmpDir::new("multi-user-device");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        let alice = tmp.path().join("alice/qos/identity");
        let bob = tmp.path().join("bob/qos/identity");
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
        qos_user_identity::verify_device_cert(&bob_cert).expect("valid bob chain");
        assert_eq!(bob_cert.user_public_key, bob_key);
        assert_eq!(
            bob_cert.device_public_key,
            persist::read_device_key(&paths.device_privkey)
                .expect("device key")
                .verifying_key()
                .to_bytes()
        );
    }

    #[test]
    fn simultaneous_user_provisioning_builds_one_consistent_device_chain() {
        let tmp = TmpDir::new("simultaneous-user-provisioning");
        let paths = IdentityPaths::under(tmp.path().join("system"));
        let alice = tmp.path().join("alice/qos/identity");
        let bob = tmp.path().join("bob/qos/identity");
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
        qos_user_identity::verify_device_cert(&legacy).expect("valid legacy certificate");

        for identity_dir in [&alice, &bob] {
            let signer = runtime_identity::load_from_path(identity_dir.join(PRIMARY_IDENTITY_FILE))
                .expect("load user identity");
            let cert =
                persist::read_device_cert(&paths.device_cert_for_user(&signer.group_public_key()))
                    .expect("identity-specific certificate");
            assert_eq!(cert.device_public_key, device);
            qos_user_identity::verify_device_cert(&cert)
                .expect("identity-specific chain remains valid");
        }
    }

    #[test]
    fn simultaneous_owner_claims_elect_one_valid_winner_without_clobbering() {
        let tmp = TmpDir::new("simultaneous-owner-election");
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

    #[cfg(unix)]
    #[test]
    fn ensure_primary_identity_writes_a_private_file() {
        let tmp = TmpDir::new("mode");
        let identity_dir = tmp.path().join("qos").join("identity");
        ensure_primary_identity(&identity_dir).expect("provision identity");
        let identity_file = identity_dir.join(PRIMARY_IDENTITY_FILE);
        let meta = std::fs::metadata(&identity_file).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
    }
}
