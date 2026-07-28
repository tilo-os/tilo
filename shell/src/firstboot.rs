//! First-boot setup, invoked by the `quick-desktop-firstboot.service`
//! system unit at first user login (`/usr/bin/quick-desktop --firstboot`).
//!
//! The systemd unit guards re-runs with a `ConditionPathExists` on
//! `/etc/quick-desktop/.firstboot-done`, so this module is a one-shot
//! "make sure the user-facing config + cache dirs exist before the
//! compositor expects them" hook. It must be cheap, idempotent, and
//! must NOT bail the compositor boot — `main` runs `run` and then
//! continues into the normal compositor entry point regardless.
//!
//! On top of the XDG-config bootstrap, this hook also provisions a
//! single-device signing identity at `<config>/qos/identity/primary.json`
//! when none is present. That file is what `runtime-identity::load`
//! reads inside sandboxed apps (via the `QOS_USER_IDENTITY` env var
//! set by the launcher), and its absence is what previously caused
//! every app to sit at "no signing identity" — join_channel then
//! carried empty identity bytes and no signed op could be minted.
//!
//! See `os/usr/lib/systemd/system/quick-desktop-firstboot.service` for
//! the boot chain and `CLAUDE.md`'s "Boot chain" section for the
//! lifecycle context.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rand_core::OsRng;
use threshold_identity::ThresholdIdentity;

/// File name for the primary single-device identity record. Matches
/// what `runtime-identity::load()` expects (via the `QOS_USER_IDENTITY`
/// env var pointing at this file) and what `DiskIdentityStore` writes
/// via the UI's identity-flow ceremony.
const PRIMARY_IDENTITY_FILE: &str = "primary.json";

/// Process-global entry point. Resolves the user's XDG config dir from
/// `$XDG_CONFIG_HOME` (falling back to `$HOME/.config`) and creates
/// `quick-desktop/` under it. Returns `Ok(())` on success and on the
/// "no HOME / no XDG" no-op path so the compositor boot continues.
///
/// Errors are surfaced only when the directory creation itself fails
/// (permission denied, ENOSPC, etc.); the caller (`main`) logs and
/// continues.
pub fn run() -> Result<(), Box<dyn Error>> {
    run_with_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Pure form of [`run`] that takes env-var values as arguments instead
/// of reading the real environment. Tests drive this directly with a
/// `tempdir`-rooted fixture so the test suite never pokes the
/// developer's actual `$HOME`.
pub fn run_with_env(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
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

    // Identity provisioning: create a fresh single-device Ed25519
    // record if none exists. Best-effort — a failure to write the
    // identity does NOT prevent the compositor from continuing to
    // boot; without a real record apps fall back to unsigned collab
    // ops, which is the pre-existing behaviour. We keep the surface
    // area small: this hook only handles the "no identity yet" first
    // run. Any subsequent enrolment / rotation goes through the
    // identity-flow ceremony in `ui::widget::identity_flow`.
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

/// Result of the "make sure a primary identity exists" step. Exposed
/// out of [`ensure_primary_identity`] so tests can distinguish "we
/// created one" from "one was already there and we left it alone".
#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// The identity file already existed; we did not touch it.
    Existing,
    /// A fresh single-device identity was minted and written. The
    /// derived Ed25519 pubkey is included so a wrapper (e.g. the
    /// firstboot log line, tests) can render the identity's public
    /// half without re-reading the file.
    Created { pubkey_hex: String },
}

/// Compute the quick-desktop config dir (`<config-base>/quick-desktop`)
/// following the same XDG fallback rules the peer-sync marker uses.
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

/// Compute the qOS identity dir (`<config-base>/qos/identity`) —
/// the same path `runtime-identity::load()` reads (when
/// `QOS_USER_IDENTITY` points at the primary file inside it) and the
/// same directory `DiskIdentityStore::from_env` writes to.
fn resolve_identity_dir(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        let mut p = PathBuf::from(xdg);
        p.push("qos");
        p.push("identity");
        return Some(p);
    }
    let home = home.filter(|v| !v.is_empty())?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("qos");
    p.push("identity");
    Some(p)
}

fn ensure_dirs(config_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    Ok(())
}

/// Create a fresh single-device identity JSON record at
/// `<identity_dir>/primary.json` when none exists, `0600`-mode. The
/// directory itself is created `0700` for the same rest-of-XDG
/// reason `DiskIdentityStore` uses (world-readable dirs leak
/// existence + atime even when the file itself is locked down).
///
/// Idempotent: an existing file is returned as
/// [`ProvisionOutcome::Existing`] with no modification.
pub fn ensure_primary_identity(identity_dir: &Path) -> Result<ProvisionOutcome, io::Error> {
    fs::create_dir_all(identity_dir)?;
    #[cfg(unix)]
    {
        let mut perm = fs::metadata(identity_dir)?.permissions();
        perm.set_mode(0o700);
        let _ = fs::set_permissions(identity_dir, perm);
    }
    let path = identity_dir.join(PRIMARY_IDENTITY_FILE);
    if path.exists() {
        return Ok(ProvisionOutcome::Existing);
    }

    // Generate a fresh dalek-seed-shaped single-device identity via
    // `threshold-identity`. We must NOT use the frost variant here:
    // the on-disk JSON schema treats `secret_bytes` as an
    // `ed25519_dalek::SigningKey::from_bytes` input (i.e. a SEED,
    // expanded via SHA-512 to reach the scalar), whereas frost stores
    // the scalar directly — round-tripping the frost value through
    // this schema derives a different signing key and produced sigs
    // stop verifying. See the seed helper's doc for the full story.
    let mut rng = OsRng;
    let (seed, group_pk) = ThresholdIdentity::bootstrap_single_device_seed(&mut rng);
    let pubkey_hex = hex_encode(group_pk.as_bytes());

    // Timestamp in RFC 3339 so a recovery flow can identify when the
    // record was minted — same format the UI identity-flow uses.
    let created_at = chrono::Utc::now().to_rfc3339();

    // Build the JSON body inline. Deliberately *not* routed through
    // `ui::widget::identity_flow::IdentityRecord::from_threshold_identity` —
    // that path pipes through `SerializableIdentity::from_identity`
    // which writes a placeholder SHA-512 digest as `secret_bytes`
    // (see the module comment). The bytes we write here are the real
    // dalek seed, so `runtime-identity::load()` derives the same
    // pubkey it finds in the record.
    let record = serde_json::json!({
        "display_name": "",
        "avatar_color": [128u8, 128u8, 128u8],
        "threshold_identity": {
            "single_device": true,
            "group_public_key": group_pk.as_bytes(),
            "threshold": 1u16,
            "total_shares": 1u16,
            "share_index": 1u16,
            "secret_bytes": seed.to_vec(),
        },
        "created_at": created_at,
        "schema_version": 1u32,
    });
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    // Atomic write: tmp → chmod → rename. Matches DiskIdentityStore's
    // torn-write mitigation.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        let mut perm = fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o600);
        fs::set_permissions(&tmp, perm)?;
    }
    fs::rename(&tmp, &path)?;
    Ok(ProvisionOutcome::Created { pubkey_hex })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Minimal tempdir helper (avoids pulling `tempfile` as a dev-dep —
    /// this is the only test in `quick-shell` that needs one).
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
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
        // First call creates the dir.
        run_with_env(Some(OsString::from(tmp.path())), None).expect("first run ok");
        // Second call must not error out.
        run_with_env(Some(OsString::from(tmp.path())), None).expect("second run ok");
        let expected = tmp.path().join("quick-desktop");
        assert!(expected.exists());
    }

    #[test]
    fn run_without_env_is_a_noop_not_an_error() {
        // Systemd `Type=oneshot` units fail the whole transaction if
        // ExecStart returns non-zero — first-boot must never break
        // boot just because env-vars are missing in the unit context.
        run_with_env(None, None).expect("no-env case is a no-op, not an error");
    }

    #[test]
    fn run_provisions_identity_when_missing() {
        let tmp = TmpDir::new("mint-identity");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot ok");
        let identity_file = tmp
            .path()
            .join("qos")
            .join("identity")
            .join(PRIMARY_IDENTITY_FILE);
        assert!(
            identity_file.exists(),
            "expected identity file at {identity_file:?}"
        );

        // File loads through runtime-identity's parser — real seed,
        // real derived pubkey, no soft-mismatch.
        let ident = runtime_identity::load_from_path(identity_file.clone())
            .expect("runtime-identity must parse the minted record");
        assert!(ident.is_single_device());
        assert_eq!(ident.threshold(), 1);
        assert_eq!(ident.total_shares(), 1);
        assert_eq!(ident.share_index(), 1);
        // Round-trip via a real sign+verify to confirm the seed and
        // pubkey stored in the file match — a placeholder record
        // would fail this check.
        let sig = ident.sign(b"firstboot-check").expect("sign ok");
        let vk =
            ed25519_dalek::VerifyingKey::from_bytes(&ident.group_public_key()).expect("vk decode");
        use ed25519_dalek::Verifier;
        let sig = ed25519_dalek::Signature::from_bytes(&sig);
        vk.verify(b"firstboot-check", &sig)
            .expect("dalek verify must succeed against derived pubkey");
    }

    #[test]
    fn run_leaves_existing_identity_alone() {
        let tmp = TmpDir::new("existing-identity");
        let dir = tmp.path().join("qos").join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(PRIMARY_IDENTITY_FILE);
        // Marker bytes: not a valid identity — the point is to prove
        // firstboot doesn't overwrite it. If firstboot ever starts
        // repairing malformed records, replace the marker with a
        // real valid record and assert the byte-content is preserved.
        std::fs::write(&file, b"MARKER: hands off").unwrap();
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot ok");
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
            other => panic!("expected Created on empty dir, got {other:?}"),
        }
        // Second run leaves it alone.
        assert_eq!(
            ensure_primary_identity(&dir).expect("provision ok (2nd)"),
            ProvisionOutcome::Existing
        );
    }

    #[cfg(unix)]
    #[test]
    fn provisioned_identity_file_is_0600() {
        let tmp = TmpDir::new("mode");
        run_with_env(Some(OsString::from(tmp.path())), None).expect("firstboot ok");
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
