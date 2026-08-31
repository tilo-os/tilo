//! First-boot setup, invoked by the `quick-desktop-firstboot.service`
//! system unit at first user login (`/usr/bin/quick-desktop --firstboot`).
//!
//! The systemd unit guards re-runs with a `ConditionPathExists` on
//! `/etc/quick-desktop/.firstboot-done`, so this module is a one-shot
//! "make sure the user-facing config + cache dirs exist" hook. Cheap,
//! idempotent, and must NOT bail the compositor boot — `main` runs `run` then continues regardless.
//!
//! On top of the XDG-config bootstrap, this also provisions a
//! single-device signing identity at `<config>/qos/identity/primary.json`
//! when none is present — protected before provisioning returns and later used
//! by the launcher through an app-scoped signing broker, encoded by the same
//! `ui::widget::identity_flow::IdentityRecord` type the interactive ceremony writes.
//!
//! See `os/usr/lib/systemd/system/quick-desktop-firstboot.service` and
//! `CLAUDE.md`'s "Boot chain" section for the lifecycle context.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use qos_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths, persist};
use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;
use ui::widget::identity_flow::IdentityRecord;

/// File name for the primary single-device identity record. Matches
/// what the runtime identity broker migrates and what `DiskIdentityStore`
/// writes via the UI's identity-flow ceremony.
const PRIMARY_IDENTITY_FILE: &str = "primary.json";

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

/// Effective uid of this process; `0` means the identity step must not run — see [`run_as`].
#[cfg(unix)]
fn effective_uid() -> u32 {
    // Safety: `geteuid` is always successful and takes no arguments.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    1000
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
    run_as(xdg_config_home, home, effective_uid())
}

/// [`run_with_env`] with the effective uid supplied, so the "root must not
/// mint an identity" rule is testable without running the suite as root.
///
/// This hook is reachable from a *system* unit running before anyone has
/// logged in, where `$HOME` is root's — minting here would put the
/// user's signing key in `/root/.config`, owned by root and unreadable
/// by the person it belongs to. The session would then find no identity
/// and boot without one, exactly the failure this guard prevents. A
/// signing identity is *personal*: minted in the user's own session, by [`crate::onboarding`], after the user has been told what it is.
///
/// # Errors
///
/// Returns an error when directory creation fails (permission denied, ENOSPC).
pub fn run_as(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
    uid: u32,
) -> Result<(), Box<dyn Error>> {
    let Some(config_dir) = resolve_config_dir(xdg_config_home.clone(), home.clone()) else {
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

    // Identity provisioning happens in the user's session, never here as root — see `run_as`'s doc comment for why.
    if uid == 0 {
        tracing::info!(
            "firstboot: running as root, so no signing identity is minted here — \
             the identity belongs to the user and is created in their session"
        );
        return Ok(());
    }

    // Create a fresh single-device Ed25519 record if none exists.
    // Best-effort — a failure to write does NOT prevent boot from
    // continuing; without a real record apps fall back to unsigned collab
    // ops. Any later enrolment/rotation goes through the identity-flow ceremony in `ui::widget::identity_flow`.
    if let Some(identity_dir) = resolve_identity_dir(xdg_config_home, home) {
        match ensure_primary_identity(&identity_dir) {
            Ok(ProvisionOutcome::Existing) => {
                tracing::info!(
                    "firstboot: identity already present at {}",
                    identity_dir.join(PRIMARY_IDENTITY_FILE).display()
                );
            }
            Ok(ProvisionOutcome::Created { pubkey_hex }) => {
                tracing::info!(
                    "firstboot: minted single-device identity at {} (pubkey={pubkey_hex})",
                    identity_dir.join(PRIMARY_IDENTITY_FILE).display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "firstboot: could not provision identity at {}: {e} (apps will run \
                     without a signing identity)",
                    identity_dir.display()
                );
            }
        }
    }
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

/// Compute the qOS identity dir (`<config-base>/qos/identity`) — the
/// same path `runtime-identity::load()` reads and `DiskIdentityStore::from_env` writes to.
fn resolve_identity_dir(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        let mut p = PathBuf::from(xdg);
        if p.is_absolute() {
            p.push("qos");
            p.push("identity");
            return Some(p);
        }
    }
    let home = home.filter(|v| !v.is_empty())?;
    let mut p = PathBuf::from(home);
    if !p.is_absolute() {
        return None;
    }
    p.push(".config");
    p.push("qos");
    p.push("identity");
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
    let path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    if path.exists() {
        protect_identity_record(identity_dir)?;
        return Ok(ProvisionOutcome::Existing);
    }

    // Mint through the same encoder the interactive ceremony uses, so
    // there's exactly one writer of the on-disk record — the two provisioning paths can't drift apart in what they store.
    let mut rng = OsRng;
    let identity = ThresholdIdentity::bootstrap_single_device(&mut rng)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let pubkey_hex = hex_encode(identity.group_public_key().as_bytes());
    let record = IdentityRecord::from_threshold_identity(
        &identity,
        "",
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
    fs::create_dir_all(identity_dir)?;
    #[cfg(unix)]
    {
        let mut perm = fs::metadata(identity_dir)?.permissions();
        perm.set_mode(0o700);
        fs::set_permissions(identity_dir, perm)?;
    }

    let primary_path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    let outcome = if primary_path.exists() {
        protect_identity_record(identity_dir)?;
        ProvisionOutcome::Existing
    } else {
        ensure_primary_identity(identity_dir)?
    };

    let signer = runtime_identity::load_from_path(primary_path)
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
    let device_owner = if paths.user_pubkey.exists() {
        let stored = persist::read_public_key(&paths.user_pubkey)
            .map_err(|e| io::Error::other(e.to_string()))?;
        stored == root_public
    } else {
        persist::write_public_key(&paths.user_pubkey, &root_public)?;
        true
    };

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
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(identity_dir);
        runtime_identity::secure_identity_with_provider(identity_dir, "primary", &provider)?;
    }
    #[cfg(not(test))]
    {
        runtime_identity::secure_identity(identity_dir, "primary")?;
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
    use std::ffi::OsString;

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
    fn run_provisions_identity_when_missing() {
        use ed25519_dalek::Verifier;

        let tmp = TmpDir::new("mint-identity");
        // An explicit non-root uid: `run_with_env` reads the real one, and as
        // root the guard returns before minting, so this asserted nothing on a
        // runner that builds in a root container.
        run_as(Some(OsString::from(tmp.path())), None, 1000).expect("firstboot ok");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        assert!(
            identity_file.exists(),
            "expected identity file at {identity_file:?}"
        );

        // File loads through runtime-identity's parser — real seed, real derived pubkey, no soft-mismatch.
        let ident = runtime_identity::load_from_path(identity_file)
            .expect("runtime-identity must parse the minted record");
        assert!(ident.is_single_device());
        assert_eq!(ident.threshold(), 1);
        assert_eq!(ident.total_shares(), 1);
        assert_eq!(ident.share_index(), 1);
        // Round-trip via a real sign+verify to confirm the seed and pubkey
        // stored in the file match — a placeholder record would fail this.
        let sig = ident.sign(b"firstboot-check").expect("sign ok");
        let vk =
            ed25519_dalek::VerifyingKey::from_bytes(&ident.group_public_key()).expect("vk decode");
        let sig = ed25519_dalek::Signature::from_bytes(&sig);
        vk.verify(b"firstboot-check", &sig)
            .expect("dalek verify must succeed against derived pubkey");
    }

    /// Running as root must not mint an identity — see `run_as`'s doc comment for why.
    #[test]
    fn root_does_not_mint_an_identity() {
        let tmp = TmpDir::new("root-refuses");
        run_as(Some(OsString::from(tmp.path())), None, 0)
            .expect("root path is a no-op, not an error");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        assert!(
            !identity_file.exists(),
            "root must not mint {identity_file:?} — the identity belongs to the user's session"
        );
    }

    /// The same call as a normal user does mint, so the guard is about the uid and not about the function having quietly stopped working.
    #[test]
    fn a_normal_user_still_mints() {
        let tmp = TmpDir::new("user-mints");
        run_as(Some(OsString::from(tmp.path())), None, 1000).expect("user path mints");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        assert!(
            identity_file.exists(),
            "a non-root uid mints {identity_file:?}"
        );
    }

    #[test]
    fn run_leaves_existing_identity_alone() {
        let tmp = TmpDir::new("existing-identity");
        let dir = tmp.path().join("qos").join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PRIMARY_IDENTITY_FILE);
        // Marker bytes: not a valid identity — proves firstboot doesn't
        // overwrite it. If firstboot starts repairing malformed records, swap in a real record and assert byte-content is preserved.
        std::fs::write(&file, b"MARKER: hands off").unwrap();
        // Non-root, so the run actually reaches the "identity already exists"
        // branch this test is about rather than returning at the uid guard.
        run_as(Some(OsString::from(tmp.path())), None, 1000).expect("firstboot ok");
        let got = std::fs::read(&file).unwrap();
        assert_eq!(got, b"MARKER: hands off");
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
        // Second run leaves it alone.
        assert_eq!(
            ensure_primary_identity(&dir).expect("provision ok (2nd)"),
            ProvisionOutcome::Existing
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

    #[cfg(unix)]
    #[test]
    fn provisioned_identity_file_is_0600() {
        let tmp = TmpDir::new("mode");
        // Non-root: as root nothing is minted and there is no file to stat.
        run_as(Some(OsString::from(tmp.path())), None, 1000).expect("firstboot ok");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        let meta = std::fs::metadata(&identity_file).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
    }
}
