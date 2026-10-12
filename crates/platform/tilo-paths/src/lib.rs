//! Where per-user data, configuration, state, cache and runtime files live.
//!
//! # Portable app locations
//!
//! [`AppPaths::for_app`] gives an app its data, config, cache, state and
//! runtime locations, the user's folders, the workspace and the font catalog
//! through one API on Linux, macOS and the web. A [`Location`] is either a
//! native path or a path inside a [`VirtualRoot`] (the browser's
//! origin-private file system, or memory); [`Location::native_path`] is the
//! explicit, fallible way back to a `Path`. What a target cannot provide is
//! an [`Unsupported`] error at run time and an entry of its [`Feature`]'s
//! targets, never a
//! missing item: this crate is where the target-specific code lives, so apps
//! need none.
//!
//! The native directory functions below are the Linux and macOS backends,
//! and stay public for code whose paths are a contract with other programs.
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
//!   (`~/Library/…`, `$TMPDIR`) and Windows (`%APPDATA%`, `%LOCALAPPDATA%`).
//!   Use them for an app's own storage.
//!
//! [`AppDirs`] joins an app's directory name onto either family.
//!
//! [`UserDir`] and its shorthands ([`document_dir`], [`download_dir`], …)
//! resolve the user's content folders, localized through `user-dirs.dirs`.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::ffi::OsString;
use std::path::PathBuf;

mod app;
mod caps;
mod location;
pub mod session;

pub use app::{AppPaths, DEFAULT_PROFILE, default_profile, set_default_profile};
pub use caps::{Availability, Feature, Unsupported, availability};
pub use location::{InvalidVPath, Location, VPath, VirtualRoot};

/// Where a Tilo Linux session mounts the collaborative workspace, and the
/// root every collaborator's documents name workspace files under, whatever
/// this device mounts. [`AppPaths::workspace_root`] says whether (and where)
/// this process has one mounted.
pub const WORKSPACE_MOUNT: &str = "/var/workspace";

/// The collaborative workspace mounted for this process, as a native path
/// ([`AppPaths::workspace_root`] for any app). `None` in a page and when no
/// workspace is mounted.
#[must_use]
pub fn workspace_root() -> Option<PathBuf> {
    let location = AppPaths::for_app("workspace").workspace_root().ok()?;
    location
        .native_path()
        .ok()
        .map(std::path::Path::to_path_buf)
}

#[cfg(windows)]
mod legacy {
    pub fn data_home() -> Option<std::path::PathBuf> {
        super::env_dir("LOCALAPPDATA")
    }
}

#[cfg(not(windows))]
mod legacy {}

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
        home_from(&|name| var(name), std::env::home_dir)
    }
}

#[cfg(not(windows))]
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
    absolute(var(name))
}

thread_local! {
    static PROFILE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Keeps a thread's per-user folders below one root until it is dropped:
/// see [`scope_profile`].
#[must_use = "the scope ends when the guard is dropped"]
pub struct ProfileScope {
    earlier: Option<PathBuf>,
}

impl Drop for ProfileScope {
    fn drop(&mut self) {
        PROFILE_ROOT.with(|root| *root.borrow_mut() = self.earlier.take());
    }
}

/// From now until the guard drops, this thread's home is `root`: [`home`],
/// the per-user base folders, [`env_dir`] of the XDG variables and
/// [`AppPaths::for_app`] all resolve below it, whatever the process
/// environment says, and no workspace is mounted. A host that runs a
/// participant (an agent) on its own thread gives it a profile apart from
/// the host user's this way. Threads the scoped thread starts do not
/// inherit it.
pub fn scope_profile(root: impl Into<PathBuf>) -> ProfileScope {
    let earlier = PROFILE_ROOT.with(|scoped| scoped.borrow_mut().replace(root.into()));
    ProfileScope { earlier }
}

/// Run `work` with this thread's own [`scope_profile`] lifted, seeing the
/// process's folders (a host reading its own settings for a scoped
/// participant), then restore the scope.
pub fn unscoped<R>(work: impl FnOnce() -> R) -> R {
    let _scope = ProfileScope {
        earlier: PROFILE_ROOT.with(|root| root.borrow_mut().take()),
    };
    work()
}

/// The root [`scope_profile`] set for this thread, if any.
#[must_use]
pub fn scoped_profile() -> Option<PathBuf> {
    PROFILE_ROOT.with(|root| root.borrow().clone())
}

/// `$name`, as this thread sees it: below its [`scope_profile`] root for
/// the per-user variables, else from the process environment.
fn var(name: &str) -> Option<OsString> {
    let Some(root) = scoped_profile() else {
        return std::env::var_os(name);
    };
    let below = match name {
        "HOME" => "",
        "XDG_DATA_HOME" => ".local/share",
        "XDG_CONFIG_HOME" => ".config",
        "XDG_STATE_HOME" => ".local/state",
        "XDG_CACHE_HOME" => ".cache",
        "XDG_RUNTIME_DIR" => "run",
        "TMPDIR" => "tmp",
        name if name.starts_with("TILO_WORKSPACE") => return None,
        name if name.starts_with("XDG_") && name.ends_with("_DIR") => return None,
        _ => return std::env::var_os(name),
    };
    Some(root.join(below).into_os_string())
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
        super::var(name)
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
/// other Unix, `~/Library/Application Support` on macOS (which keeps state
/// beside data); Windows defines none.
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
/// Unix, the per-user `$TMPDIR` on macOS when it is absolute; Windows defines
/// none.
#[must_use]
pub fn runtime_dir() -> Option<PathBuf> {
    platform::runtime()
}

/// The system's shared temporary directory (`$TMPDIR` or `/tmp`,
/// `%TEMP%`), and `None` where there is no native file system (the web),
/// where `std::env::temp_dir` panics instead. For scratch files that must
/// have a native path; an app's own scratch space is [`AppPaths::runtime`].
#[must_use]
pub fn temp_dir() -> Option<PathBuf> {
    (availability(Feature::NativePaths) == Availability::Available).then(std::env::temp_dir)
}

/// Directories the system and the user install fonts into, most general
/// first, whether or not they exist: the system font trees on Linux and
/// other Unix (not the user's own), `/System/Library/Fonts`, `/Library/Fonts` and
/// `~/Library/Fonts` on macOS, the Windows font folder, and none on the web.
/// For code that reads font files itself (a document renderer's fallback
/// fonts, a confined helper's read grant).
#[must_use]
pub fn system_font_dirs() -> Vec<PathBuf> {
    platform::font_dirs()
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
    pub fn font_dirs() -> Vec<PathBuf> {
        [
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/usr/share/X11/fonts/Type1",
            "/usr/share/X11/fonts/TTF",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::xdg::Base;
    use super::{UserDir, home, macos};

    fn base(base: Base) -> Option<PathBuf> {
        macos::base(base, &|name| crate::var(name), home)
    }

    pub fn data() -> Option<PathBuf> {
        base(Base::Data)
    }
    pub fn data_local() -> Option<PathBuf> {
        data()
    }
    pub fn config() -> Option<PathBuf> {
        base(Base::Config)
    }
    pub fn state() -> Option<PathBuf> {
        base(Base::State)
    }
    pub fn cache() -> Option<PathBuf> {
        base(Base::Cache)
    }
    pub fn runtime() -> Option<PathBuf> {
        base(Base::Runtime)
    }
    pub fn user_dir(dir: UserDir) -> Option<PathBuf> {
        Some(macos::user_dir(dir, &home()?))
    }
    pub fn font_dirs() -> Vec<PathBuf> {
        let mut dirs = vec![
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/Library/Fonts"),
        ];
        dirs.extend(home().map(|home| home.join("Library/Fonts")));
        dirs
    }
}

/// The macOS per-user locations as functions of the environment and home
/// directory, so the Linux test run checks them too.
#[cfg(any(target_os = "macos", all(test, unix)))]
mod macos {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::xdg::Base;
    use super::{UserDir, absolute};

    /// Data, configuration and state share `~/Library/Application Support`,
    /// which is where macOS keeps an app's durable files; caches go to
    /// `~/Library/Caches`. The runtime directory is `$TMPDIR`, the per-user,
    /// owner-only directory launchd assigns below `/var/folders` and the
    /// system cleans up. It counts only when absolute: the shared `/tmp`
    /// lacks those guarantees, so there is no fallback.
    pub(crate) fn base(
        base: Base,
        var: &dyn Fn(&str) -> Option<OsString>,
        home: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        match base {
            Base::Data | Base::Config | Base::State => {
                Some(home()?.join("Library/Application Support"))
            }
            Base::Cache => Some(home()?.join("Library/Caches")),
            Base::Runtime => absolute(var("TMPDIR")),
        }
    }

    pub(crate) fn user_dir(dir: UserDir, home: &Path) -> PathBuf {
        home.join(match dir {
            UserDir::Videos => "Movies",
            other => other.conventional_name(),
        })
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
    pub fn font_dirs() -> Vec<PathBuf> {
        super::env_dir("WINDIR")
            .map(|windows| windows.join("Fonts"))
            .into_iter()
            .collect()
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
    pub fn font_dirs() -> Vec<PathBuf> {
        Vec::new()
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
        let config_home = xdg::Base::Config
            .resolve_from(var("XDG_CONFIG_HOME").as_deref(), Some(home.as_os_str()))?;
        let file = std::fs::read(config_home.join("user-dirs.dirs")).ok();
        self.resolve_from(var(self.env_var()).as_deref(), file.as_deref(), home)
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

    #[test]
    fn a_native_target_has_the_system_temporary_directory() {
        assert_eq!(temp_dir(), Some(std::env::temp_dir()));
    }

    #[cfg(unix)]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn base_directories_follow_absolute_variables_or_home_defaults() {
        // Phase `an_absolute_variable_wins_over_the_home_default`.
        {
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
        // Phase `unset_variables_fall_back_to_the_spec_defaults_under_home`.
        {
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
        // Phase `relative_and_empty_variables_are_ignored`.
        {
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
        // Phase `explicit_values_follow_the_same_rules`.
        {
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
        // Phase `home_prefers_the_variable_and_rejects_an_empty_one`.
        {
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
        // Phase `search_paths_drop_invalid_entries_and_default_when_nothing_is_left`.
        {
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
    }

    #[test]
    fn app_and_user_dir_names_follow_the_spec_without_a_home() {
        // Phase `without_a_home_only_an_explicit_variable_resolves`.
        {
            assert_eq!(Base::Config.resolve(&env(&[]), || None), None);
            #[cfg(unix)]
            assert_eq!(
                Base::Config.resolve(&env(&[("XDG_CONFIG_HOME", "/cfg")]), || None),
                Some(PathBuf::from("/cfg"))
            );
        }
        // Phase `user_dir_names_match_the_spec_keys`.
        {
            assert_eq!(UserDir::Download.env_var(), "XDG_DOWNLOAD_DIR");
            assert_eq!(UserDir::PublicShare.env_var(), "XDG_PUBLICSHARE_DIR");
            assert_eq!(UserDir::Download.conventional_name(), "Downloads");
        }
        // Phase `app_dirs_join_the_app_name_onto_each_base`.
        {
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
    fn user_dirs_files_resolve_below_home_and_yield_to_variables() {
        // Phase `user_dirs_file_entries_resolve_below_home_or_as_written`.
        {
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
        // Phase `disabled_unquoted_relative_and_missing_entries_are_absent`.
        {
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
        // Phase `an_absolute_variable_overrides_the_user_dirs_file`.
        {
            let home = std::path::Path::new("/home/u");
            assert_eq!(
                UserDir::Download.resolve_from(Some(OsStr::new("/data/dl")), Some(USER_DIRS), home),
                Some(PathBuf::from("/data/dl"))
            );
            for ignored in ["", "relative/dl"] {
                assert_eq!(
                    UserDir::Download.resolve_from(
                        Some(OsStr::new(ignored)),
                        Some(USER_DIRS),
                        home
                    ),
                    Some(PathBuf::from("/home/u/Downloads")),
                    "{ignored:?}"
                );
            }
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

    #[cfg(unix)]
    #[test]
    fn macos_keeps_data_config_and_state_in_application_support() {
        let tmpdir = [("TMPDIR", "/var/folders/ab/xyz/T/")];
        let var = env(&tmpdir);
        for (base, expected) in [
            (Base::Data, "/Users/u/Library/Application Support"),
            (Base::Config, "/Users/u/Library/Application Support"),
            (Base::State, "/Users/u/Library/Application Support"),
            (Base::Cache, "/Users/u/Library/Caches"),
            (Base::Runtime, "/var/folders/ab/xyz/T/"),
        ] {
            assert_eq!(
                macos::base(base, &var, home_is("/Users/u")),
                Some(PathBuf::from(expected)),
                "{base:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn macos_runtime_needs_an_absolute_tmpdir_and_ignores_xdg() {
        for value in ["", "tmp", "./T"] {
            let vars = [("TMPDIR", value), ("XDG_RUNTIME_DIR", "/run/user/501")];
            assert_eq!(
                macos::base(Base::Runtime, &env(&vars), home_is("/Users/u")),
                None,
                "{value:?}"
            );
        }
        assert_eq!(
            macos::base(Base::Data, &env(&[("XDG_DATA_HOME", "/data")]), || None),
            None,
            "without a home only the runtime directory can resolve"
        );
    }

    #[cfg(unix)]
    #[test]
    fn macos_user_dirs_are_the_english_folders_below_home() {
        let home = std::path::Path::new("/Users/u");
        assert_eq!(macos::user_dir(UserDir::Videos, home), home.join("Movies"));
        assert_eq!(
            macos::user_dir(UserDir::Download, home),
            home.join("Downloads")
        );
        assert_eq!(
            macos::user_dir(UserDir::Documents, home),
            home.join("Documents")
        );
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
