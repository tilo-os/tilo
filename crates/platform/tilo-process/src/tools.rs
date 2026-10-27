//! Where a person's command-line tools live.
//!
//! A desktop session often hands its apps a narrower PATH than the person's
//! terminal has: per-user installers put tools in `~/.cargo/bin` (rustup) and
//! `~/.local/bin` (pipx, npm's user prefix), and shell profiles add those and
//! more to PATH only for login shells. A tool installed the usual way must
//! still be found, so [`ToolDirs::current`] looks in all three: the process's
//! own PATH, those folders, and the PATH the login shell sets up. The page has
//! no programs to run: there, nothing is found.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The login shell's PATH, once read; `None` inside when it could not be.
static LOGIN_PATH: OnceLock<Option<OsString>> = OnceLock::new();

/// Folders to look for a tool in, in the order they are searched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolDirs {
    dirs: Vec<PathBuf>,
}

impl ToolDirs {
    /// The folders of `path` (a PATH value), then `home`'s per-user tool
    /// folders, then those of `login_path`.
    #[must_use]
    pub fn of(path: Option<&OsStr>, home: Option<&Path>, login_path: Option<&OsStr>) -> Self {
        let mut dirs: Vec<PathBuf> = path
            .map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default();
        if let Some(home) = home {
            dirs.push(home.join(".cargo").join("bin"));
            dirs.push(home.join(".local").join("bin"));
        }
        if let Some(login) = login_path {
            dirs.extend(std::env::split_paths(login));
        }
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|dir| dir.is_absolute() && seen.insert(dir.clone()));
        Self { dirs }
    }

    /// This process's folders, with the login shell's PATH once
    /// [`read_login_path`] has read it.
    #[must_use]
    pub fn current() -> Self {
        backend::current()
    }

    /// The first program named `name` in these folders.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.dirs
            .iter()
            .find_map(|dir| backend::program(&dir.join(name)))
    }
}

/// Whether the login shell's PATH has been read, or found unreadable.
#[must_use]
pub fn login_path_read() -> bool {
    LOGIN_PATH.get().is_some()
}

/// Read the login shell's PATH on a thread of its own, once per process: a
/// shell profile can take a while, and no caller may wait for it.
pub fn read_login_path() {
    if login_path_read() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("login-path".into())
        .spawn(|| {
            let _ = LOGIN_PATH.set(backend::login_shell_path());
        });
    if spawned.is_err() {
        let _ = LOGIN_PATH.set(None);
    }
}

#[cfg(unix)]
mod backend {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// A profile that waits on something must not hold a thread and a shell
    /// forever.
    const PROFILE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);
    const PATH_LIMIT: usize = 64 * 1024;

    pub(super) fn current() -> super::ToolDirs {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let login = super::LOGIN_PATH.get().and_then(Option::as_deref);
        super::ToolDirs::of(std::env::var_os("PATH").as_deref(), home.as_deref(), login)
    }

    pub(super) fn login_shell_path() -> Option<OsString> {
        let shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .or_else(passwd_shell)
            .unwrap_or_else(|| "/bin/sh".into());
        let output = bounded_command::run_os(
            shell,
            &["-l", "-c", "printf %s \"$PATH\""],
            None,
            PATH_LIMIT,
            PROFILE_BOUND,
        )
        .ok()?;
        (output.status.success() && !output.stdout.is_empty())
            .then(|| OsString::from_vec(output.stdout))
    }

    /// The login shell the passwd database records for this user.
    fn passwd_shell() -> Option<OsString> {
        // SAFETY: `getpwuid` returns a pointer into libc's static buffer,
        // valid until the next passwd call; both pointers are null-checked and
        // the shell is copied out before anything else touches the buffer.
        unsafe {
            let entry = libc::getpwuid(crate::uid());
            if entry.is_null() || (*entry).pw_shell.is_null() {
                return None;
            }
            let shell = std::ffi::CStr::from_ptr((*entry).pw_shell).to_bytes();
            (!shell.is_empty()).then(|| OsString::from_vec(shell.to_vec()))
        }
    }

    pub(super) fn program(path: &Path) -> Option<PathBuf> {
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .then(|| path.to_path_buf())
    }
}

/// Windows: the process's PATH, where a program is its `.exe`.
#[cfg(all(not(unix), not(target_family = "wasm")))]
mod backend {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    pub(super) fn current() -> super::ToolDirs {
        super::ToolDirs::of(std::env::var_os("PATH").as_deref(), None, None)
    }

    pub(super) fn login_shell_path() -> Option<OsString> {
        None
    }

    pub(super) fn program(path: &Path) -> Option<PathBuf> {
        [path.with_extension("exe"), path.to_path_buf()]
            .into_iter()
            .find(|candidate| candidate.is_file())
    }
}

#[cfg(target_family = "wasm")]
mod backend {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    pub(super) fn current() -> super::ToolDirs {
        super::ToolDirs::default()
    }

    pub(super) fn login_shell_path() -> Option<OsString> {
        None
    }

    pub(super) fn program(_: &Path) -> Option<PathBuf> {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::ToolDirs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_tool_in_the_persons_cargo_folder_is_found_without_it_on_path() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("rust-analyzer");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let empty = std::ffi::OsString::new();

        let dirs = ToolDirs::of(Some(&empty), Some(home.path()), None);
        assert_eq!(dirs.find("rust-analyzer"), Some(tool.clone()));
        assert_eq!(
            ToolDirs::of(Some(&empty), None, None).find("rust-analyzer"),
            None
        );

        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            dirs.find("rust-analyzer"),
            None,
            "a file that can't run is no tool"
        );
    }
}
