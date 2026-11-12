//! The shell a person's terminal starts.
//!
//! A person names their shell once, in `$SHELL` or in the passwd database, and
//! they may uninstall it later. The terminal still has to open, so a shell
//! that this account can no longer run is passed over: the other source is
//! tried, and `/bin/sh` is the last resort.

/// The shell this account's terminal starts.
///
/// Unix resolution order:
/// 1. **`$SHELL`** when it names a program this account can run — the
///    person's explicit preference (this is how fish, when it is the login
///    shell, gets launched here).
/// 2. **The passwd database** (`getpwuid(getuid()).pw_shell`) — the login
///    shell the system records for this account, when it can run too.
/// 3. **`/bin/sh`** — last resort, so a session never fails to start.
///
/// Windows uses `COMSPEC`, falling back to `cmd.exe`.
#[must_use]
pub fn login_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC")
            .ok()
            .filter(|shell| !shell.trim().is_empty())
            .unwrap_or_else(|| "cmd.exe".into())
    }
    #[cfg(unix)]
    {
        login_shell_with(std::env::var("SHELL").ok(), login_shell_from_passwd(), runs)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // A page has no processes to start.
        "/bin/sh".into()
    }
}

/// [`login_shell`] with its two sources and the program check injected, so the
/// precedence is testable without mutating process-global state (which would
/// race parallel tests).
#[cfg(unix)]
#[must_use]
pub fn login_shell_with(
    shell_env: Option<String>,
    passwd_shell: Option<String>,
    runs: impl Fn(&str) -> bool,
) -> String {
    let named = |shell: Option<String>| shell.filter(|shell| !shell.is_empty() && runs(shell));
    named(shell_env)
        .or_else(|| named(passwd_shell))
        .unwrap_or_else(|| "/bin/sh".into())
}

/// The login shell recorded for the current real user in the passwd database
/// (`pw_shell`). `None` if the entry is missing or has an empty shell field.
#[cfg(unix)]
#[must_use]
pub fn login_shell_from_passwd() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: `getpwuid` returns a pointer into a static buffer owned by libc
    // (valid until the next passwd/`getpwuid` call, of which we make none
    // before copying out). We null-check before dereferencing, and copy the
    // `pw_shell` C string into an owned `String` while the pointer is live.
    unsafe {
        let uid = crate::uid();
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let shell = (*pw).pw_shell;
        if shell.is_null() {
            return None;
        }
        Some(CStr::from_ptr(shell).to_string_lossy().into_owned())
    }
}

/// Whether this account can execute `shell`: a path that exists and may be
/// run, or a name the process's `PATH` resolves to one.
#[cfg(unix)]
fn runs(shell: &str) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let named: Vec<std::path::PathBuf> = if shell.contains('/') {
        vec![std::path::PathBuf::from(shell)]
    } else {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(shell))
                    .collect()
            })
            .unwrap_or_default()
    };
    named.iter().any(|candidate| {
        let Ok(path) = std::ffi::CString::new(candidate.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a live NUL-terminated path and `access` only reads it.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn has(shells: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |shell| shells.contains(&shell)
    }

    /// The order: `$SHELL`, then the shell the system records, then `/bin/sh`
    /// — each taken only while this account can run it.
    #[test]
    fn the_shell_env_wins_then_the_passwd_shell_then_sh() {
        assert_eq!(
            login_shell_with(
                Some("/usr/bin/fish".into()),
                Some("/bin/bash".into()),
                has(&["/usr/bin/fish", "/bin/bash"])
            ),
            "/usr/bin/fish",
            "$SHELL wins when it is set and can run"
        );
        assert_eq!(
            login_shell_with(None, Some("/usr/bin/zsh".into()), has(&["/usr/bin/zsh"])),
            "/usr/bin/zsh",
            "with $SHELL unset the passwd login shell is used, not a hardcoded /bin/sh"
        );
        assert_eq!(
            login_shell_with(
                Some(String::new()),
                Some("/usr/bin/zsh".into()),
                has(&["/usr/bin/zsh"])
            ),
            "/usr/bin/zsh",
        );
        assert_eq!(
            login_shell_with(None, None, has(&[])),
            "/bin/sh",
            "with neither source available the session still starts on /bin/sh"
        );
    }

    /// A person who uninstalled the shell they chose still gets a terminal.
    #[test]
    fn a_shell_this_account_cannot_run_is_passed_over() {
        assert_eq!(
            login_shell_with(
                Some("/usr/bin/fish".into()),
                Some("/bin/bash".into()),
                has(&["/bin/bash"])
            ),
            "/bin/bash"
        );
        assert_eq!(
            login_shell_with(
                Some("/usr/bin/fish".into()),
                Some("/usr/bin/zsh".into()),
                has(&[])
            ),
            "/bin/sh"
        );
    }

    /// The lookups the resolution reads are the machine's own.
    #[test]
    fn a_program_this_account_runs_is_found_by_path_and_by_name() {
        let shell = login_shell_from_passwd().expect("this account has a login shell recorded");
        assert!(
            shell.starts_with('/'),
            "the passwd login shell is an absolute path, got {shell:?}"
        );
        assert!(runs("/bin/sh"));
        assert!(runs("sh"), "a bare name is looked up in PATH");
        assert!(!runs("/nonexistent/uninstalled-shell"));
        assert!(!runs("tilo-no-such-program"));
    }
}
