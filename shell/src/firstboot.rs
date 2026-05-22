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
//! See `os/usr/lib/systemd/system/quick-desktop-firstboot.service` for
//! the boot chain and `CLAUDE.md`'s "Boot chain" section for the
//! lifecycle context.

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

fn ensure_dirs(config_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    Ok(())
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
        run_with_env(None, Some(OsString::from(tmp.path())))
            .expect("firstboot succeeds via HOME");
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
}
