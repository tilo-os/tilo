/// The shell this account's terminal starts: **`$SHELL`** when it names a
/// program this account can run (the person's preference, and how fish starts
/// here when it is the login shell), then the login shell the passwd database
/// records when that runs, then **`/bin/sh`** — a shell a person has since
/// uninstalled never leaves them without a terminal. Windows uses `COMSPEC`,
/// falling back to `cmd.exe`.
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
        login_shell_with(
            std::env::var("SHELL").ok(),
            login_shell_from_passwd(),
            crate::tools::runs,
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        // A page has no processes to start.
        "/bin/sh".into()
    }
}

/// [`login_shell`] with its sources and the program check injected, testable
/// without mutating process-global state (which would race parallel tests).
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

/// The login shell the passwd database records for the current real user
/// (`pw_shell`); `None` when the entry is missing or names no shell.
#[cfg(unix)]
#[must_use]
pub fn login_shell_from_passwd() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: `getpwuid` returns a pointer into a static buffer owned by libc
    // (valid until the next passwd call, of which we make none before copying
    // out); both pointers are null-checked and the shell is copied out while
    // live.
    unsafe {
        let pw = libc::getpwuid(crate::uid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*pw).pw_shell)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn has(shells: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |shell| shells.contains(&shell)
    }

    /// The order — `$SHELL`, then the shell the system records, then `/bin/sh`
    /// — passing over any shell this account cannot run, and the lookups it
    /// reads are the machine's own.
    #[test]
    fn the_shell_env_wins_then_the_passwd_shell_then_sh() {
        let fish = || Some("/usr/bin/fish".to_owned());
        let zsh = || Some("/usr/bin/zsh".to_owned());
        let bash = || Some("/bin/bash".to_owned());
        assert_eq!(
            login_shell_with(fish(), bash(), has(&["/usr/bin/fish"])),
            "/usr/bin/fish",
            "$SHELL wins when it is set and can run"
        );
        assert_eq!(
            login_shell_with(None, zsh(), has(&["/usr/bin/zsh"])),
            "/usr/bin/zsh",
            "with $SHELL unset the recorded login shell starts, not /bin/sh"
        );
        assert_eq!(
            login_shell_with(Some(String::new()), zsh(), has(&["/usr/bin/zsh"])),
            "/usr/bin/zsh"
        );
        // The shell the person chose was uninstalled; the recorded one runs.
        assert_eq!(
            login_shell_with(fish(), bash(), has(&["/bin/bash"])),
            "/bin/bash"
        );
        assert_eq!(login_shell_with(fish(), None, has(&[])), "/bin/sh");
        let passwd = login_shell_from_passwd().expect("this account has a login shell recorded");
        assert!(
            passwd.starts_with('/'),
            "the passwd login shell is absolute: {passwd:?}"
        );
        assert!(crate::tools::runs("/bin/sh"));
        assert!(crate::tools::runs("sh"), "a bare name is looked up in PATH");
        assert!(!crate::tools::runs("/nonexistent/uninstalled-shell"));
        assert!(!crate::tools::runs("tilo-no-such-program"));
    }
}
