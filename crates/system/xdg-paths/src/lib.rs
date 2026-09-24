//! Where per-user data, configuration, state, cache and runtime files live.
//!
//! # Why one crate
//!
//! Tilo apps and services each used to re-derive these directories, and the
//! hand-rolled resolvers disagreed with the XDG Base Directory specification
//! and with each other: several joined a **relative** `XDG_DATA_HOME` against
//! the working directory, or took an empty variable as a path. The spec says a
//! relative value is invalid and must be ignored, and an empty one is unset.
//! Two readers of the same store that disagree on that rule silently read and
//! write different files. Every resolver here applies the rule once.
//!
//! # Two families
//!
//! - [`xdg`] applies the XDG rules on every platform: `$XDG_*_HOME` when set,
//!   non-empty and absolute, else the spec's `$HOME` default. Use it where a
//!   path is part of a contract with other XDG readers (the shared settings
//!   file, desktop entries, identity files).
//! - The top-level [`data_dir`], [`config_dir`], [`state_dir`], [`cache_dir`]
//!   and [`runtime_dir`] are the platform's per-user directories: the XDG
//!   rules on Linux and other Unix systems, and the native locations on macOS
//!   (`~/Library/…`) and Windows (`%APPDATA%`, `%LOCALAPPDATA%`), exactly the
//!   directories the `dirs` crate reports. Use them for an app's own storage.
//!
//! [`AppDirs`] joins an app's directory name onto either family.
//!
//! [`UserDir`] and its shorthands ([`document_dir`], [`download_dir`], …)
//! resolve the user's content folders, localized through `user-dirs.dirs`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::ffi::OsString;
use std::path::PathBuf;

/// The user's home directory: `$HOME` when set and non-empty, the account
/// database's entry when `$HOME` is unset (on Unix), and `None` for an empty
/// `$HOME` — an empty value names no directory, and resolving relative to the
/// working directory would scatter state wherever the process started. On
/// Windows it is the profile known folder, never an environment variable.
#[must_use]
pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        platform::home()
    }
    #[cfg(not(windows))]
    {
        home_from(&|name| std::env::var_os(name), std::env::home_dir)
    }
}

#[cfg_attr(
    windows,
    expect(dead_code, reason = "Windows asks the shell for the profile folder")
)]
fn home_from(
    var: &dyn Fn(&str) -> Option<OsString>,
    account_home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    #[cfg(unix)]
    match var("HOME") {
        Some(home) if home.is_empty() => None,
        Some(home) => Some(PathBuf::from(home)),
        None => account_home().filter(|home| !home.as_os_str().is_empty()),
    }
    #[cfg(not(unix))]
    {
        let _ = var;
        account_home().filter(|home| !home.as_os_str().is_empty())
    }
}

/// `$name` as a directory, following the XDG rule for every base-directory
/// variable: honoured only when set, non-empty and absolute.
#[must_use]
pub fn env_dir(name: &str) -> Option<PathBuf> {
    absolute(std::env::var_os(name))
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    // An empty path is not absolute, so this also rejects empty values.
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// The base directories as the XDG Base Directory specification defines them,
/// on every platform.
pub mod xdg {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    use super::{absolute, home};

    /// `$XDG_DATA_HOME`, else `$HOME/.local/share`.
    #[must_use]
    pub fn data_home() -> Option<PathBuf> {
        Base::Data.resolve(&env, home)
    }

    /// `$XDG_CONFIG_HOME`, else `$HOME/.config`.
    #[must_use]
    pub fn config_home() -> Option<PathBuf> {
        Base::Config.resolve(&env, home)
    }

    /// `$XDG_STATE_HOME`, else `$HOME/.local/state`.
    #[must_use]
    pub fn state_home() -> Option<PathBuf> {
        Base::State.resolve(&env, home)
    }

    /// `$XDG_CACHE_HOME`, else `$HOME/.cache`.
    #[must_use]
    pub fn cache_home() -> Option<PathBuf> {
        Base::Cache.resolve(&env, home)
    }

    /// `$XDG_RUNTIME_DIR`. The spec defines no fallback: a substitute would
    /// lack its guarantees (owner-only, removed at logout), so `None` makes
    /// callers decide what to do without one.
    #[must_use]
    pub fn runtime_dir() -> Option<PathBuf> {
        Base::Runtime.resolve(&env, home)
    }

    /// `$XDG_DATA_DIRS` in preference order, else `/usr/local/share` and
    /// `/usr/share`. Relative and empty entries are ignored, as the spec
    /// requires.
    #[must_use]
    pub fn data_dirs() -> Vec<PathBuf> {
        search_path(
            env("XDG_DATA_DIRS").as_deref(),
            &["/usr/local/share", "/usr/share"],
        )
    }

    /// `$XDG_CONFIG_DIRS` in preference order, else `/etc/xdg`. Relative and
    /// empty entries are ignored, as the spec requires.
    #[must_use]
    pub fn config_dirs() -> Vec<PathBuf> {
        search_path(env("XDG_CONFIG_DIRS").as_deref(), &["/etc/xdg"])
    }

    fn env(name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    /// One of the spec's per-user base directories.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Base {
        /// `XDG_DATA_HOME`, default `$HOME/.local/share`.
        Data,
        /// `XDG_CONFIG_HOME`, default `$HOME/.config`.
        Config,
        /// `XDG_STATE_HOME`, default `$HOME/.local/state`.
        State,
        /// `XDG_CACHE_HOME`, default `$HOME/.cache`.
        Cache,
        /// `XDG_RUNTIME_DIR`, no default.
        Runtime,
    }

    impl Base {
        /// The environment variable that names this directory.
        #[must_use]
        pub const fn env_var(self) -> &'static str {
            match self {
                Self::Data => "XDG_DATA_HOME",
                Self::Config => "XDG_CONFIG_HOME",
                Self::State => "XDG_STATE_HOME",
                Self::Cache => "XDG_CACHE_HOME",
                Self::Runtime => "XDG_RUNTIME_DIR",
            }
        }

        /// The default below `$HOME` when the variable is unusable.
        const fn home_default(self) -> Option<&'static str> {
            match self {
                Self::Data => Some(".local/share"),
                Self::Config => Some(".config"),
                Self::State => Some(".local/state"),
                Self::Cache => Some(".cache"),
                Self::Runtime => None,
            }
        }

        /// Resolve from explicit values of the variable and `$HOME` instead of
        /// the process environment — for callers that take them as parameters
        /// so tests need not mutate the environment. The variable counts only
        /// when non-empty and absolute; an empty `home` counts as unset.
        #[must_use]
        pub fn resolve_from(self, value: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
            self.resolve(&|_| value.map(OsStr::to_os_string), || {
                home.filter(|home| !home.is_empty()).map(PathBuf::from)
            })
        }

        pub(crate) fn resolve(
            self,
            var: &dyn Fn(&str) -> Option<OsString>,
            home: impl FnOnce() -> Option<PathBuf>,
        ) -> Option<PathBuf> {
            absolute(var(self.env_var())).or_else(|| {
                let default = self.home_default()?;
                Some(home()?.join(default))
            })
        }
    }

    pub(crate) fn search_path(value: Option<&OsStr>, default: &[&str]) -> Vec<PathBuf> {
        let listed: Vec<PathBuf> = value
            .map(|value| {
                std::env::split_paths(value)
                    .filter(|path| path.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        // An unset, empty or entirely invalid list means the defaults.
        if listed.is_empty() {
            default.iter().map(PathBuf::from).collect()
        } else {
            listed
        }
    }
}

/// The platform's per-user data directory: [`xdg::data_home`] on Linux and
/// other Unix, `~/Library/Application Support` on macOS, `%APPDATA%` on
/// Windows.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    platform::data()
}

/// The platform's per-user local (non-roaming) data directory:
/// [`xdg::data_home`] on Linux and other Unix, `~/Library/Application Support`
/// on macOS, `%LOCALAPPDATA%` on Windows.
#[must_use]
pub fn data_local_dir() -> Option<PathBuf> {
    platform::data_local()
}

/// The platform's per-user configuration directory: [`xdg::config_home`] on
/// Linux and other Unix, `~/Library/Application Support` on macOS,
/// `%APPDATA%` on Windows.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    platform::config()
}

/// The platform's per-user state directory: [`xdg::state_home`] on Linux and
/// other Unix; macOS and Windows define none.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    platform::state()
}

/// The platform's per-user cache directory: [`xdg::cache_home`] on Linux and
/// other Unix, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    platform::cache()
}

/// The per-login runtime directory: [`xdg::runtime_dir`] on Linux and other
/// Unix; macOS and Windows define none.
#[must_use]
pub fn runtime_dir() -> Option<PathBuf> {
    platform::runtime()
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    use super::{UserDir, home, xdg};

    pub fn data() -> Option<PathBuf> {
        xdg::data_home()
    }
    pub fn data_local() -> Option<PathBuf> {
        xdg::data_home()
    }
    pub fn config() -> Option<PathBuf> {
        xdg::config_home()
    }
    pub fn state() -> Option<PathBuf> {
        xdg::state_home()
    }
    pub fn cache() -> Option<PathBuf> {
        xdg::cache_home()
    }
    pub fn runtime() -> Option<PathBuf> {
        xdg::runtime_dir()
    }
    pub fn user_dir(dir: UserDir) -> Option<PathBuf> {
        dir.resolve_for_home(&home()?)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::{UserDir, home};

    pub fn data() -> Option<PathBuf> {
        Some(home()?.join("Library/Application Support"))
    }
    pub fn data_local() -> Option<PathBuf> {
        data()
    }
    pub fn config() -> Option<PathBuf> {
        data()
    }
    pub fn state() -> Option<PathBuf> {
        None
    }
    pub fn cache() -> Option<PathBuf> {
        Some(home()?.join("Library/Caches"))
    }
    pub fn runtime() -> Option<PathBuf> {
        None
    }
    pub fn user_dir(dir: UserDir) -> Option<PathBuf> {
        Some(home()?.join(dir.conventional_name()))
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
        FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Public,
        FOLDERID_RoamingAppData, FOLDERID_Templates, FOLDERID_Videos, SHGetKnownFolderPath,
    };
    use windows_sys::core::{GUID, PWSTR};

    use super::UserDir;

    /// The shell's current location for a known folder, which follows folder
    /// redirection and cannot be spoofed through the environment.
    fn known_folder(id: &GUID) -> Option<PathBuf> {
        let mut raw: PWSTR = std::ptr::null_mut();
        // SAFETY: `id` is a valid GUID reference, a null token selects the
        // current user, and `raw` is a valid out-pointer the call fills with
        // a CoTaskMem-allocated, NUL-terminated wide string (or leaves null).
        let status = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &raw mut raw) };
        let path = (status == 0 && !raw.is_null()).then(|| {
            let mut len = 0_usize;
            // SAFETY: on success `raw` points to a NUL-terminated wide string;
            // every offset read here is at or before its terminator.
            while unsafe { *raw.add(len) } != 0 {
                len = len.saturating_add(1);
            }
            // SAFETY: the `len` units before the terminator were just read.
            let wide = unsafe { std::slice::from_raw_parts(raw, len) };
            PathBuf::from(OsString::from_wide(wide))
        });
        // SAFETY: the buffer is owned by us after the call, whether or not it
        // succeeded; freeing null is a no-op.
        unsafe { CoTaskMemFree(raw.cast()) };
        path.filter(|path| !path.as_os_str().is_empty())
    }

    pub fn home() -> Option<PathBuf> {
        known_folder(&FOLDERID_Profile)
    }
    pub fn data() -> Option<PathBuf> {
        known_folder(&FOLDERID_RoamingAppData)
    }
    pub fn data_local() -> Option<PathBuf> {
        known_folder(&FOLDERID_LocalAppData)
    }
    pub fn config() -> Option<PathBuf> {
        data()
    }
    pub fn state() -> Option<PathBuf> {
        None
    }
    pub fn cache() -> Option<PathBuf> {
        data_local()
    }
    pub fn runtime() -> Option<PathBuf> {
        None
    }
    pub fn user_dir(dir: UserDir) -> Option<PathBuf> {
        known_folder(match dir {
            UserDir::Desktop => &FOLDERID_Desktop,
            UserDir::Documents => &FOLDERID_Documents,
            UserDir::Download => &FOLDERID_Downloads,
            UserDir::Music => &FOLDERID_Music,
            UserDir::Pictures => &FOLDERID_Pictures,
            UserDir::Videos => &FOLDERID_Videos,
            UserDir::Templates => &FOLDERID_Templates,
            UserDir::PublicShare => &FOLDERID_Public,
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::path::PathBuf;

    use super::{UserDir, env_dir, home};

    pub fn data() -> Option<PathBuf> {
        env_dir("APPDATA")
    }
    pub fn data_local() -> Option<PathBuf> {
        env_dir("LOCALAPPDATA")
    }
    pub fn config() -> Option<PathBuf> {
        data()
    }
    pub fn state() -> Option<PathBuf> {
        None
    }
    pub fn cache() -> Option<PathBuf> {
        env_dir("LOCALAPPDATA")
    }
    pub fn runtime() -> Option<PathBuf> {
        None
    }
    pub fn user_dir(dir: UserDir) -> Option<PathBuf> {
        Some(home()?.join(dir.conventional_name()))
    }
}

/// One of the user's well-known content directories (Documents, Downloads, …).
///
/// On Linux and other Unix these follow `xdg-user-dirs`: `$XDG_<NAME>_DIR` when
/// set, non-empty and absolute, else the entry in
/// `$XDG_CONFIG_HOME/user-dirs.dirs`, which is where a localized desktop
/// records `~/Dokumente` instead of `~/Documents`. An entry pointing at `$HOME`
/// itself means the directory is disabled and resolves to `None`, as does a
/// missing entry. macOS and Windows use their conventional folders below the
/// home directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UserDir {
    /// `XDG_DESKTOP_DIR`.
    Desktop,
    /// `XDG_DOCUMENTS_DIR`.
    Documents,
    /// `XDG_DOWNLOAD_DIR`.
    Download,
    /// `XDG_MUSIC_DIR`.
    Music,
    /// `XDG_PICTURES_DIR`.
    Pictures,
    /// `XDG_VIDEOS_DIR`.
    Videos,
    /// `XDG_TEMPLATES_DIR`.
    Templates,
    /// `XDG_PUBLICSHARE_DIR`.
    PublicShare,
}

impl UserDir {
    /// The environment variable, and `user-dirs.dirs` key, that names this
    /// directory.
    #[must_use]
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Desktop => "XDG_DESKTOP_DIR",
            Self::Documents => "XDG_DOCUMENTS_DIR",
            Self::Download => "XDG_DOWNLOAD_DIR",
            Self::Music => "XDG_MUSIC_DIR",
            Self::Pictures => "XDG_PICTURES_DIR",
            Self::Videos => "XDG_VIDEOS_DIR",
            Self::Templates => "XDG_TEMPLATES_DIR",
            Self::PublicShare => "XDG_PUBLICSHARE_DIR",
        }
    }

    /// The folder name the platform conventionally uses below the home
    /// directory (English, not localized). macOS and Windows resolve to it;
    /// on XDG platforms it is only a hint for callers that need a default.
    #[must_use]
    pub const fn conventional_name(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Download => "Downloads",
            Self::Music => "Music",
            Self::Pictures => "Pictures",
            #[cfg(target_os = "macos")]
            Self::Videos => "Movies",
            #[cfg(not(target_os = "macos"))]
            Self::Videos => "Videos",
            Self::Templates => "Templates",
            Self::PublicShare => "Public",
        }
    }

    /// Resolve this directory for the current user on this platform.
    #[must_use]
    pub fn resolve(self) -> Option<PathBuf> {
        platform::user_dir(self)
    }

    /// Resolve against an explicit home directory with the XDG rules:
    /// `$XDG_<NAME>_DIR` from the process environment, else the
    /// `user-dirs.dirs` file below `$XDG_CONFIG_HOME` (default `home/.config`).
    #[must_use]
    pub fn resolve_for_home(self, home: &std::path::Path) -> Option<PathBuf> {
        let config_home = xdg::Base::Config.resolve_from(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            Some(home.as_os_str()),
        )?;
        let file = std::fs::read(config_home.join("user-dirs.dirs")).ok();
        self.resolve_from(
            std::env::var_os(self.env_var()).as_deref(),
            file.as_deref(),
            home,
        )
    }

    /// Resolve from an explicit variable value and `user-dirs.dirs` contents
    /// instead of the process environment and file system. The variable counts
    /// only when non-empty and absolute.
    #[must_use]
    pub fn resolve_from(
        self,
        value: Option<&std::ffi::OsStr>,
        user_dirs: Option<&[u8]>,
        home: &std::path::Path,
    ) -> Option<PathBuf> {
        absolute(value.map(std::ffi::OsStr::to_os_string))
            .or_else(|| user_dirs_entry(user_dirs?, self.env_var(), home))
    }
}

/// The value of `key` in a `user-dirs.dirs` file.
///
/// The file is a shell fragment, but `xdg-user-dirs-update` writes only
/// `KEY="$HOME/relative"` or `KEY="/absolute"`, so exactly those two forms are
/// accepted; a value of `"$HOME/"` disables the directory. The last matching
/// line wins, as it would when the file is sourced.
fn user_dirs_entry(contents: &[u8], key: &str, home: &std::path::Path) -> Option<PathBuf> {
    let mut found = None;
    for line in contents.split(|&byte| byte == b'\n') {
        let Some(separator) = line.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let (name, value) = line.split_at(separator);
        if trim_blank(name) != key.as_bytes() {
            continue;
        }
        let value = trim_blank(value.get(1..).unwrap_or_default());
        let Some(value) = value
            .strip_prefix(b"\"")
            .and_then(|value| value.strip_suffix(b"\""))
        else {
            continue;
        };
        found = if value == b"$HOME/" || value == b"$HOME" {
            None
        } else if let Some(relative) = value.strip_prefix(b"$HOME/") {
            Some(home.join(path_from_bytes(shell_unescape(relative))))
        } else if value.starts_with(b"/") {
            Some(path_from_bytes(shell_unescape(value)))
        } else {
            continue;
        };
    }
    found
}

fn trim_blank(bytes: &[u8]) -> &[u8] {
    let blank = |byte: &u8| *byte == b' ' || *byte == b'\t' || *byte == b'\r';
    let start = bytes
        .iter()
        .position(|byte| !blank(byte))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !blank(byte))
        .map_or(start, |last| last.saturating_add(1));
    bytes.get(start..end).unwrap_or_default()
}

/// Undo POSIX double-quote escaping: a backslash quotes only `$`, `` ` ``,
/// `"`, `\` and a newline, and is kept literally before anything else.
fn shell_unescape(escaped: &[u8]) -> Vec<u8> {
    let mut unescaped = Vec::with_capacity(escaped.len());
    let mut bytes = escaped.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte == b'\\'
            && let Some(&next) = bytes.peek()
            && matches!(next, b'$' | b'`' | b'"' | b'\\' | b'\n')
        {
            unescaped.push(next);
            bytes.next();
        } else {
            unescaped.push(byte);
        }
    }
    unescaped
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    match String::from_utf8(bytes) {
        Ok(text) => PathBuf::from(text),
        Err(error) => PathBuf::from(String::from_utf8_lossy(error.as_bytes()).into_owned()),
    }
}

/// [`UserDir::Desktop`] for the current user.
#[must_use]
pub fn desktop_dir() -> Option<PathBuf> {
    UserDir::Desktop.resolve()
}

/// [`UserDir::Documents`] for the current user.
#[must_use]
pub fn document_dir() -> Option<PathBuf> {
    UserDir::Documents.resolve()
}

/// [`UserDir::Download`] for the current user.
#[must_use]
pub fn download_dir() -> Option<PathBuf> {
    UserDir::Download.resolve()
}

/// [`UserDir::Music`] for the current user.
#[must_use]
pub fn audio_dir() -> Option<PathBuf> {
    UserDir::Music.resolve()
}

/// [`UserDir::Pictures`] for the current user.
#[must_use]
pub fn picture_dir() -> Option<PathBuf> {
    UserDir::Pictures.resolve()
}

/// [`UserDir::Videos`] for the current user.
#[must_use]
pub fn video_dir() -> Option<PathBuf> {
    UserDir::Videos.resolve()
}

/// One app's subdirectories of the per-user base directories, e.g.
/// `AppDirs::new("tilo-chat").data()` is `~/.local/share/tilo-chat` on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppDirs<'a> {
    name: &'a str,
}

impl<'a> AppDirs<'a> {
    /// `name` is one path component (or a relative path) under each base.
    #[must_use]
    pub const fn new(name: &'a str) -> Self {
        Self { name }
    }

    /// `name` under [`data_dir`].
    #[must_use]
    pub fn data(&self) -> Option<PathBuf> {
        Some(data_dir()?.join(self.name))
    }

    /// `name` under [`config_dir`].
    #[must_use]
    pub fn config(&self) -> Option<PathBuf> {
        Some(config_dir()?.join(self.name))
    }

    /// `name` under [`state_dir`].
    #[must_use]
    pub fn state(&self) -> Option<PathBuf> {
        Some(state_dir()?.join(self.name))
    }

    /// `name` under [`cache_dir`].
    #[must_use]
    pub fn cache(&self) -> Option<PathBuf> {
        Some(cache_dir()?.join(self.name))
    }

    /// `name` under [`runtime_dir`].
    #[must_use]
    pub fn runtime(&self) -> Option<PathBuf> {
        Some(runtime_dir()?.join(self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::xdg::Base;
    #[cfg(unix)]
    use super::xdg::search_path;
    use super::*;
    #[cfg(unix)]
    use std::ffi::OsStr;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[cfg(unix)]
    fn home_is(path: &str) -> impl FnOnce() -> Option<PathBuf> + '_ {
        move || Some(PathBuf::from(path))
    }

    #[cfg(unix)]
    #[test]
    fn an_absolute_variable_wins_over_the_home_default() {
        let vars = [
            ("XDG_DATA_HOME", "/data"),
            ("XDG_CONFIG_HOME", "/config"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CACHE_HOME", "/cache"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ];
        let var = env(&vars);
        for (base, expected) in [
            (Base::Data, "/data"),
            (Base::Config, "/config"),
            (Base::State, "/state"),
            (Base::Cache, "/cache"),
            (Base::Runtime, "/run/user/1000"),
        ] {
            assert_eq!(
                base.resolve(&var, home_is("/home/u")),
                Some(PathBuf::from(expected))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unset_variables_fall_back_to_the_spec_defaults_under_home() {
        let var = env(&[]);
        for (base, expected) in [
            (Base::Data, Some("/home/u/.local/share")),
            (Base::Config, Some("/home/u/.config")),
            (Base::State, Some("/home/u/.local/state")),
            (Base::Cache, Some("/home/u/.cache")),
            (Base::Runtime, None),
        ] {
            assert_eq!(
                base.resolve(&var, home_is("/home/u")),
                expected.map(PathBuf::from),
                "{base:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn relative_and_empty_variables_are_ignored() {
        // A relative value would resolve against whatever directory the
        // process happened to start in; the spec says to ignore it.
        for value in ["relative/data", "", "./data", "~/data"] {
            let vars = [("XDG_DATA_HOME", value), ("XDG_RUNTIME_DIR", value)];
            let var = env(&vars);
            assert_eq!(
                Base::Data.resolve(&var, home_is("/home/u")),
                Some(PathBuf::from("/home/u/.local/share")),
                "{value:?}"
            );
            assert_eq!(
                Base::Runtime.resolve(&var, home_is("/home/u")),
                None,
                "{value:?}"
            );
        }
    }

    #[test]
    fn without_a_home_only_an_explicit_variable_resolves() {
        assert_eq!(Base::Config.resolve(&env(&[]), || None), None);
        #[cfg(unix)]
        assert_eq!(
            Base::Config.resolve(&env(&[("XDG_CONFIG_HOME", "/cfg")]), || None),
            Some(PathBuf::from("/cfg"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_values_follow_the_same_rules() {
        let home = Some(OsStr::new("/home/u"));
        assert_eq!(
            Base::State.resolve_from(Some(OsStr::new("/state")), home),
            Some(PathBuf::from("/state"))
        );
        for invalid in [None, Some(OsStr::new("")), Some(OsStr::new("rel"))] {
            assert_eq!(
                Base::State.resolve_from(invalid, home),
                Some(PathBuf::from("/home/u/.local/state"))
            );
        }
        assert_eq!(Base::State.resolve_from(None, Some(OsStr::new(""))), None);
        assert_eq!(Base::Runtime.resolve_from(None, home), None);
        assert_eq!(Base::Config.env_var(), "XDG_CONFIG_HOME");
    }

    #[cfg(unix)]
    #[test]
    fn home_prefers_the_variable_and_rejects_an_empty_one() {
        let account = || Some(PathBuf::from("/var/home/account"));
        assert_eq!(
            home_from(&env(&[("HOME", "/home/u")]), account),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(home_from(&env(&[("HOME", "")]), account), None);
        assert_eq!(
            home_from(&env(&[]), account),
            Some(PathBuf::from("/var/home/account"))
        );
        assert_eq!(home_from(&env(&[]), || Some(PathBuf::new())), None);
    }

    #[cfg(unix)]
    #[test]
    fn search_paths_drop_invalid_entries_and_default_when_nothing_is_left() {
        let defaults = ["/usr/local/share", "/usr/share"];
        let expected_defaults: Vec<PathBuf> = defaults.iter().map(PathBuf::from).collect();
        assert_eq!(search_path(None, &defaults), expected_defaults);
        assert_eq!(
            search_path(Some(OsStr::new("")), &defaults),
            expected_defaults
        );
        assert_eq!(
            search_path(Some(OsStr::new("relative:also/relative")), &defaults),
            expected_defaults
        );
        assert_eq!(
            search_path(
                Some(OsStr::new("/opt/share::relative:/usr/share")),
                &defaults
            ),
            vec![PathBuf::from("/opt/share"), PathBuf::from("/usr/share")]
        );
    }

    #[cfg(unix)]
    const USER_DIRS: &[u8] = b"# This file is written by xdg-user-dirs-update\n\
XDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n\
XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n\
  XDG_DOCUMENTS_DIR = \"$HOME/Dokumente\"  \n\
XDG_MUSIC_DIR=\"/srv/music\"\n\
XDG_PICTURES_DIR=\"$HOME/\"\n\
XDG_VIDEOS_DIR=$HOME/Videos\n\
XDG_TEMPLATES_DIR=\"relative/templates\"\n\
XDG_PUBLICSHARE_DIR=\"$HOME/My \\\"Public\\\" \\$hare\"\n";

    #[cfg(unix)]
    #[test]
    fn user_dirs_file_entries_resolve_below_home_or_as_written() {
        let home = std::path::Path::new("/home/u");
        let resolve = |dir: UserDir| dir.resolve_from(None, Some(USER_DIRS), home);
        assert_eq!(
            resolve(UserDir::Desktop),
            Some(PathBuf::from("/home/u/Schreibtisch"))
        );
        assert_eq!(
            resolve(UserDir::Download),
            Some(PathBuf::from("/home/u/Downloads"))
        );
        assert_eq!(
            resolve(UserDir::Documents),
            Some(PathBuf::from("/home/u/Dokumente")),
            "blanks around the key and value are not part of either"
        );
        assert_eq!(resolve(UserDir::Music), Some(PathBuf::from("/srv/music")));
        assert_eq!(
            resolve(UserDir::PublicShare),
            Some(PathBuf::from("/home/u/My \"Public\" $hare")),
            "double-quote escapes are undone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn disabled_unquoted_relative_and_missing_entries_are_absent() {
        let home = std::path::Path::new("/home/u");
        for dir in [UserDir::Pictures, UserDir::Videos, UserDir::Templates] {
            assert_eq!(
                dir.resolve_from(None, Some(USER_DIRS), home),
                None,
                "{dir:?}"
            );
        }
        assert_eq!(UserDir::Desktop.resolve_from(None, None, home), None);
        assert_eq!(UserDir::Desktop.resolve_from(None, Some(b""), home), None);
    }

    #[cfg(unix)]
    #[test]
    fn an_absolute_variable_overrides_the_user_dirs_file() {
        let home = std::path::Path::new("/home/u");
        assert_eq!(
            UserDir::Download.resolve_from(Some(OsStr::new("/data/dl")), Some(USER_DIRS), home),
            Some(PathBuf::from("/data/dl"))
        );
        for ignored in ["", "relative/dl"] {
            assert_eq!(
                UserDir::Download.resolve_from(Some(OsStr::new(ignored)), Some(USER_DIRS), home),
                Some(PathBuf::from("/home/u/Downloads")),
                "{ignored:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_last_matching_line_wins() {
        let home = std::path::Path::new("/home/u");
        let file = b"XDG_DESKTOP_DIR=\"$HOME/One\"\nXDG_DESKTOP_DIR=\"$HOME/Two\"\n";
        assert_eq!(
            UserDir::Desktop.resolve_from(None, Some(file), home),
            Some(PathBuf::from("/home/u/Two"))
        );
    }

    #[test]
    fn user_dir_names_match_the_spec_keys() {
        assert_eq!(UserDir::Download.env_var(), "XDG_DOWNLOAD_DIR");
        assert_eq!(UserDir::PublicShare.env_var(), "XDG_PUBLICSHARE_DIR");
        assert_eq!(UserDir::Download.conventional_name(), "Downloads");
    }

    #[test]
    fn app_dirs_join_the_app_name_onto_each_base() {
        let app = AppDirs::new("tilo-example");
        for (joined, base) in [
            (app.data(), data_dir()),
            (app.config(), config_dir()),
            (app.state(), state_dir()),
            (app.cache(), cache_dir()),
            (app.runtime(), runtime_dir()),
        ] {
            assert_eq!(joined, base.map(|base| base.join("tilo-example")));
        }
    }
}
