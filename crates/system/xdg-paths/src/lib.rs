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
/// working directory would scatter state wherever the process started.
#[must_use]
pub fn home() -> Option<PathBuf> {
    home_from(&|name| std::env::var_os(name), std::env::home_dir)
}

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

    use super::xdg;

    pub fn data() -> Option<PathBuf> {
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
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    use super::home;

    pub fn data() -> Option<PathBuf> {
        Some(home()?.join("Library/Application Support"))
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
}

#[cfg(not(unix))]
mod platform {
    use std::path::PathBuf;

    use super::env_dir;

    pub fn data() -> Option<PathBuf> {
        env_dir("APPDATA")
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
    use super::xdg::{Base, search_path};
    use super::*;
    use std::ffi::OsStr;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

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
