//! Where a person's command-line tools live.
//!
//! A desktop session often hands its apps a narrower PATH than the person's terminal: a
//! tool installed the usual way must still be found, so [`ToolDirs::current`] looks in
//! the process's own PATH, the per-user install folders, and the PATH the login shell
//! sets up. The page has no programs to run.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static LOGIN_PATH: OnceLock<Option<OsString>> = OnceLock::new();

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolDirs {
    dirs: Vec<PathBuf>,
}

impl ToolDirs {
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

    #[must_use]
    pub fn current() -> Self {
        backend::current()
    }

    #[must_use]
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        self.dirs
            .iter()
            .find_map(|dir| backend::program(&dir.join(name)))
    }
}

#[must_use]
pub fn runs(program: &str) -> bool {
    if program.contains('/') {
        return backend::program(Path::new(program)).is_some();
    }
    ToolDirs::current().find(program).is_some()
}

#[must_use]
pub fn login_path_read() -> bool {
    LOGIN_PATH.get().is_some()
}

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

    // A profile that waits on something must not hold a thread and a shell forever.
    const PROFILE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);
    const PATH_LIMIT: usize = 64 * 1024;

    pub(super) fn current() -> super::ToolDirs {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let login = super::LOGIN_PATH.get().and_then(Option::as_deref);
        super::ToolDirs::of(std::env::var_os("PATH").as_deref(), home.as_deref(), login)
    }

    pub(super) fn login_shell_path() -> Option<OsString> {
        let shell = crate::login_shell::login_shell();
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
