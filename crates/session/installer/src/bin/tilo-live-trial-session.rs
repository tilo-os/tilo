//! `tilo-live-trial-session` — publish the trial account's empty home and run
//! the real Tilo desktop in it.
//!
//! [`crate::live_desktop`] is the wizard's side of a trial (asking for one, and
//! what it promises). This is the session manager's side: greetd runs it
//! through `tilo-session-select` once a request stands, `/usr/libexec/tilo-live-trial-session`
//! manages the home's lifetime, and this program does the one step that needs
//! the account's numeric identity — publishing the home and its first defaults
//! as that account — before handing over to the compositor.
//!
//! Nothing here touches a disk, a partition or a passphrase: the trial has no
//! way to ask for one. The only paths written are the trial's own.

use std::path::Path;
use std::process::Command;

/// The trial account, created once by the live medium's setup unit and never a
/// normal login account (it has no password and a nologin shell).
const TRIAL_USER: &str = "tilo-trial";

/// The real session compositor on this medium.
const SHELL_BIN: &str = "/usr/bin/shell";

/// Publishes an account's first defaults, owned by that account. It is the
/// same program the installed system's first boot uses, so a trial's home
/// looks exactly like a real first login's.
const HOME_SEEDS_BIN: &str = "/usr/libexec/tilo-firstboot-home-seeds";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tilo-live-trial-session: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let home = Path::new(installer::live_desktop::TRIAL_HOME);
    publish_home(home)?;
    run_desktop(home)
}

/// Create the trial account's home, owned by it, with nothing in it beyond the
/// image's own defaults, plus the empty identity directory that makes the
/// desktop show its first-run screen.
fn publish_home(home: &Path) -> Result<(), String> {
    let status = Command::new(HOME_SEEDS_BIN)
        .arg(TRIAL_USER)
        .status()
        .map_err(|error| format!("starting {HOME_SEEDS_BIN}: {error}"))?;
    if !status.success() {
        return Err(format!(
            "{HOME_SEEDS_BIN} could not publish the trial account's home ({status})"
        ));
    }
    if !home.is_dir() {
        return Err(format!("{} was not created", home.display()));
    }
    let identity = installer::live_desktop::identity_dir();
    std::fs::create_dir_all(&identity)
        .map_err(|error| format!("creating {}: {error}", identity.display()))?;
    // The home defaults helper chowns the tree it creates; the identity
    // directory is made here, so it takes the account's ids explicitly.
    chown_to_trial(&identity)
}

/// Run the desktop as the trial account until it ends.
///
/// The compositor takes VT 1 through `libseat` while it runs and gives it back
/// when it exits, which is how greetd gets the screen back for the wizard.
fn run_desktop(home: &Path) -> Result<(), String> {
    let (layout, variant) = installer::live_desktop::keymap();
    // `runuser -u` keeps the CLI form the invariant checks look for: the
    // account name is a constant, never anything the desktop can influence.
    let status = Command::new("/usr/sbin/runuser")
        .arg("-u")
        .arg(TRIAL_USER)
        .arg("--")
        .arg(SHELL_BIN)
        .arg("--backend")
        .arg("drm")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env(
            "TILO_IDENTITY_ROOT",
            installer::live_desktop::identity_dir(),
        )
        .env("LANG", installer::live_desktop::locale())
        .env("XKB_DEFAULT_LAYOUT", layout)
        .env("XKB_DEFAULT_VARIANT", variant)
        // The trial is the desktop, not a kiosk: it takes the seat the way an
        // authenticated login does.
        .env("LIBSEAT_BACKEND", "logind")
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_SESSION_CLASS", "user")
        .env("XDG_SESSION_DESKTOP", "tilo")
        .env("XDG_CURRENT_DESKTOP", "tilo")
        .env("XDG_SEAT", "seat0")
        .env("XDG_VTNR", "1")
        .status()
        .map_err(|error| format!("starting the desktop: {error}"))?;
    // However the desktop ended, the trial ends here: the wrapper deletes the
    // home and greetd puts the wizard back on screen. A non-zero status is the
    // compositor's own record, kept in the journal, not a reason to retry.
    if !status.success() {
        eprintln!("tilo-live-trial-session: the live desktop ended ({status})");
    }
    Ok(())
}

/// Give `path` to the trial account, with no access for anyone else.
fn chown_to_trial(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::chown;
    let (uid, gid) =
        account_ids(TRIAL_USER).ok_or_else(|| format!("no {TRIAL_USER} account on this medium"))?;
    chown(path, Some(uid), Some(gid)).map_err(|error| format!("chown {}: {error}", path.display()))
}

/// The numeric ids of `account`, from the system's own account database.
fn account_ids(account: &str) -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let mut fields = line.split(':');
        if fields.next() != Some(account) {
            continue;
        }
        return Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trial_account_is_found_in_the_account_database() {
        // The real database, because the lookup is what the production path
        // uses: an account the medium did not create means no trial at all.
        let root = account_ids("root").expect("root is in /etc/passwd");
        assert_eq!(root, (0, 0));
        assert_eq!(account_ids("tilo-trial-does-not-exist"), None);
    }
}
