//! One app's locations, resolved by the backend for the target it runs on.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::UserDir;
use crate::caps::{Feature, Unsupported};
use crate::location::{Location, VPath, VirtualRoot};

/// The browser profile an app's web storage belongs to unless the host
/// chose another with [`set_default_profile`], or a caller with
/// [`AppPaths::with_profile`].
pub const DEFAULT_PROFILE: &str = "default";

static PROFILE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Make `profile` the one [`AppPaths::for_app`] resolves web storage in, for
/// the rest of the process (on the web, the page). A browser host calls this
/// once, with the profile it mounts, before any app resolves its paths.
/// Native per-user directories belong to one profile already and do not
/// change.
pub fn set_default_profile(profile: &str) {
    *PROFILE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(component(profile));
}

/// The profile [`AppPaths::for_app`] resolves web storage in, as it appears
/// in locations.
#[must_use]
pub fn default_profile() -> String {
    PROFILE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned())
}

/// Where one app keeps its files, the same API on every target:
///
/// | | Linux | macOS | Web |
/// |---|---|---|---|
/// | `data` | `$XDG_DATA_HOME/<id>` | `~/Library/Application Support/<id>` | `opfs://<profile>/<id>/data` |
/// | `config` | `$XDG_CONFIG_HOME/<id>` | `~/Library/Application Support/<id>` | `opfs://<profile>/<id>/config` |
/// | `cache` | `$XDG_CACHE_HOME/<id>` | `~/Library/Caches/<id>` | `opfs://<profile>/<id>/cache` |
/// | `state` | `$XDG_STATE_HOME/<id>` | `~/Library/Application Support/<id>` | `opfs://<profile>/<id>/state` |
/// | `runtime` | `$XDG_RUNTIME_DIR/<id>` | `$TMPDIR/<id>` | memory, for the page's lifetime |
/// | `user_dir` | `xdg-user-dirs` | `~/Documents`, … | unsupported |
///
/// On native targets these are exactly [`data_dir`](crate::data_dir) and the
/// other per-user directories with the id joined on. When one does not
/// resolve (no home directory), durable storage falls back to memory rather
/// than to a path relative to wherever the process started.
///
/// The id is one path component, typically the app's reverse-DNS id. Bytes
/// that cannot appear in one (`/`, `\`, `:`, `%`, control characters) are
/// percent-escaped, as are the ids `.`, `..` and the empty id, so no id can
/// name a location outside its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    app: String,
    scope: Scope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    System { profile: String },
    Under(PathBuf),
    Memory,
}

/// Per-app durable storage kinds, named as their web subdirectories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Data,
    Config,
    Cache,
    State,
    Runtime,
}

impl Slot {
    const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Config => "config",
            Self::Cache => "cache",
            Self::State => "state",
            Self::Runtime => "runtime",
        }
    }
}

const SCOPED: &str = "App paths scoped to one directory have no user folders, workspace or fonts.";

impl AppPaths {
    /// The app's locations in this user's (or on the web, the page's
    /// profile's) storage.
    #[must_use]
    pub fn for_app(id: &str) -> Self {
        Self {
            app: component(id),
            scope: Scope::System {
                profile: default_profile(),
            },
        }
    }

    /// Everything below `root/<id>`: `data`, `config`, `cache`, `state` and
    /// `runtime` subdirectories, and no user folders, workspace or fonts.
    /// For tests and for hosts that give each app a sandbox directory.
    #[must_use]
    pub fn under(root: impl Into<PathBuf>, id: &str) -> Self {
        Self {
            app: component(id),
            scope: Scope::Under(root.into()),
        }
    }

    /// Everything in memory, for tests; nothing outlives the process.
    #[must_use]
    pub fn memory(id: &str) -> Self {
        Self {
            app: component(id),
            scope: Scope::Memory,
        }
    }

    /// Keep web storage in `profile`'s tree. Native per-user directories
    /// belong to one profile already, so native targets and scoped paths
    /// resolve as before.
    #[must_use]
    pub fn with_profile(mut self, profile: &str) -> Self {
        if let Scope::System { profile: current } = &mut self.scope {
            *current = component(profile);
        }
        self
    }

    /// The id as it appears in locations, escaped where needed.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.app
    }

    /// Durable files the app owns: documents, databases, downloads it manages.
    #[must_use]
    pub fn data(&self) -> Location {
        self.storage(Slot::Data)
    }

    /// Settings the user chose.
    #[must_use]
    pub fn config(&self) -> Location {
        self.storage(Slot::Config)
    }

    /// Files the app can rebuild, which the system may delete.
    #[must_use]
    pub fn cache(&self) -> Location {
        self.storage(Slot::Cache)
    }

    /// State worth keeping across restarts but not worth backing up: history,
    /// window layout, logs.
    #[must_use]
    pub fn state(&self) -> Location {
        self.storage(Slot::State)
    }

    /// Scratch space for this login session (on the web, this page).
    ///
    /// # Errors
    ///
    /// [`Unsupported`] when the session has no private runtime directory
    /// (Linux without `$XDG_RUNTIME_DIR`, macOS without `$TMPDIR`).
    pub fn runtime(&self) -> Result<Location, Unsupported> {
        match &self.scope {
            Scope::System { .. } => system::runtime(&self.app),
            Scope::Under(root) => Ok(under(root, &self.app, Slot::Runtime)),
            Scope::Memory => Ok(memory(&self.app, Slot::Runtime)),
        }
    }

    /// One of the user's content folders.
    ///
    /// # Errors
    ///
    /// [`Unsupported`] on the web, for scoped paths, and when the user has
    /// no such folder (disabled in `user-dirs.dirs`, or no home directory).
    pub fn user_dir(&self, dir: UserDir) -> Result<Location, Unsupported> {
        match self.scope {
            Scope::System { .. } => system::user_dir(dir),
            Scope::Under(_) | Scope::Memory => Err(scoped(Feature::UserDirs)),
        }
    }

    /// The collaborative workspace the host mounted for this app:
    /// `$TILO_WORKSPACE_ROOT` when absolute, else `/var/workspace` on a Tilo
    /// Linux session (`$TILO_WORKSPACE_ID` set).
    ///
    /// # Errors
    ///
    /// [`Unsupported`] on the web, for scoped paths, and when no workspace
    /// is mounted.
    pub fn workspace_root(&self) -> Result<Location, Unsupported> {
        match self.scope {
            Scope::System { .. } => system::workspace_root(),
            Scope::Under(_) | Scope::Memory => Err(scoped(Feature::WorkspaceRoot)),
        }
    }

    /// The directory holding the Tilo font catalog: where the OS image
    /// installs it on Linux, the app bundle's `Contents/Resources/fonts` on
    /// macOS. Only the location is resolved; readers verify what they use.
    ///
    /// # Errors
    ///
    /// [`Unsupported`] on the web (fonts arrive as data), for scoped paths,
    /// and on macOS when the app does not run from a bundle.
    pub fn font_root(&self) -> Result<Location, Unsupported> {
        match self.scope {
            Scope::System { .. } => system::font_root(),
            Scope::Under(_) | Scope::Memory => Err(scoped(Feature::FontRoot)),
        }
    }

    fn storage(&self, slot: Slot) -> Location {
        match &self.scope {
            Scope::System { profile } => system::storage(slot, &self.app, profile),
            Scope::Under(root) => under(root, &self.app, slot),
            Scope::Memory => memory(&self.app, slot),
        }
    }
}

const fn scoped(feature: Feature) -> Unsupported {
    Unsupported {
        feature,
        reason: SCOPED,
    }
}

fn under(root: &std::path::Path, app: &str, slot: Slot) -> Location {
    Location::Native(root.join(app).join(slot.name()))
}

fn memory(app: &str, slot: Slot) -> Location {
    Location::Virtual(
        VirtualRoot::Memory,
        VPath::from_components([app, slot.name()]),
    )
}

/// `raw` as one path component, percent-escaping what a component cannot
/// hold. The mapping is injective: `%` itself is escaped, so the bare `%`
/// that stands for the empty id and the `%2E` forms of `.` and `..` cannot
/// arise from any other id.
fn component(raw: &str) -> String {
    match raw {
        "" => return "%".to_owned(),
        "." => return "%2E".to_owned(),
        ".." => return "%2E%2E".to_owned(),
        _ => {}
    }
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        if matches!(character, '/' | '\\' | ':' | '%') || character.is_control() {
            let mut utf8 = [0; 4];
            for byte in character.encode_utf8(&mut utf8).bytes() {
                // Writing to a `String` cannot fail.
                let _ = write!(escaped, "%{byte:02X}");
            }
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// The native backend: the crate's per-user directories with the id joined.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod system {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{Slot, memory};
    use crate::caps::{Feature, Unsupported};
    use crate::location::Location;
    use crate::{UserDir, absolute};

    /// Where the Tilo image mounts the workspace when the launcher names one
    /// by id only.
    const DEFAULT_WORKSPACE_ROOT: Option<&str> = if cfg!(target_os = "linux") {
        Some(crate::WORKSPACE_MOUNT)
    } else {
        None
    };

    pub(super) fn storage(slot: Slot, app: &str, _profile: &str) -> Location {
        let base = match slot {
            Slot::Data => crate::data_dir(),
            Slot::Config => crate::config_dir(),
            Slot::Cache => crate::cache_dir(),
            Slot::State => crate::state_dir(),
            Slot::Runtime => crate::runtime_dir(),
        };
        joined(base, app, slot)
    }

    /// `base/app`, or memory when the base directory does not resolve.
    pub(super) fn joined(base: Option<PathBuf>, app: &str, slot: Slot) -> Location {
        base.map_or_else(
            || memory(app, slot),
            |base| Location::Native(base.join(app)),
        )
    }

    pub(super) fn runtime(app: &str) -> Result<Location, Unsupported> {
        crate::runtime_dir()
            .map(|dir| Location::Native(dir.join(app)))
            .ok_or(Unsupported {
                feature: Feature::Runtime,
                reason: "This session has no private runtime directory.",
            })
    }

    pub(super) fn user_dir(dir: UserDir) -> Result<Location, Unsupported> {
        dir.resolve().map(Location::Native).ok_or(Unsupported {
            feature: Feature::UserDirs,
            reason: "This folder is not set up for this user.",
        })
    }

    pub(super) fn workspace_root() -> Result<Location, Unsupported> {
        workspace_root_from(&|name| crate::var(name), DEFAULT_WORKSPACE_ROOT)
    }

    pub(super) fn workspace_root_from(
        var: &dyn Fn(&str) -> Option<OsString>,
        default: Option<&str>,
    ) -> Result<Location, Unsupported> {
        absolute(var("TILO_WORKSPACE_ROOT"))
            .or_else(|| {
                var("TILO_WORKSPACE_ID")
                    .filter(|id| !id.is_empty())
                    .and(default)
                    .map(PathBuf::from)
            })
            .map(Location::Native)
            .ok_or(Unsupported {
                feature: Feature::WorkspaceRoot,
                reason: "No workspace is mounted for this app.",
            })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn font_root() -> Result<Location, Unsupported> {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(bundle_fonts)
            .map(Location::Native)
            .ok_or(Unsupported {
                feature: Feature::FontRoot,
                reason: "Fonts are bundled only when the app runs from its app bundle.",
            })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the signature every backend shares"
    )]
    pub(super) fn font_root() -> Result<Location, Unsupported> {
        // The catalog's installed location, `font_catalog::INSTALLED_ROOT`.
        Ok(Location::Native(PathBuf::from("/usr/share/fonts/tilo")))
    }

    #[cfg(not(unix))]
    pub(super) fn font_root() -> Result<Location, Unsupported> {
        Err(Unsupported {
            feature: Feature::FontRoot,
            reason: "This system has no Tilo font catalog.",
        })
    }

    /// `Contents/Resources/fonts` of the bundle whose `Contents/MacOS` holds
    /// `exe`.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn bundle_fonts(exe: &std::path::Path) -> Option<PathBuf> {
        let macos = exe.parent()?;
        let contents = macos.parent()?;
        (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents")
            .then(|| contents.join("Resources/fonts"))
    }
}

/// The web backend: virtual roots in the origin-private file system, and
/// page memory for runtime files.
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
mod web {
    use super::{Slot, memory};
    use crate::UserDir;
    use crate::caps::{Feature, Unsupported, WEB_FONTS, WEB_USER_DIRS, WEB_WORKSPACE};
    use crate::location::{Location, VPath, VirtualRoot};

    pub(super) fn storage(slot: Slot, app: &str, profile: &str) -> Location {
        Location::Virtual(
            VirtualRoot::Opfs {
                profile: profile.to_owned(),
            },
            VPath::from_components([app, slot.name()]),
        )
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the signature every backend shares"
    )]
    pub(super) fn runtime(app: &str) -> Result<Location, Unsupported> {
        Ok(memory(app, Slot::Runtime))
    }

    pub(super) const fn user_dir(_: UserDir) -> Result<Location, Unsupported> {
        Err(Unsupported {
            feature: Feature::UserDirs,
            reason: WEB_USER_DIRS,
        })
    }

    pub(super) const fn workspace_root() -> Result<Location, Unsupported> {
        Err(Unsupported {
            feature: Feature::WorkspaceRoot,
            reason: WEB_WORKSPACE,
        })
    }

    pub(super) const fn font_root() -> Result<Location, Unsupported> {
        Err(Unsupported {
            feature: Feature::FontRoot,
            reason: WEB_FONTS,
        })
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use web as system;

// Every backend's rules are pure, so the native test run checks them all.
#[cfg(test)]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::*;

    const ID: &str = "so.tilo.notes";

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn native(path: &str) -> Location {
        Location::Native(PathBuf::from(path))
    }

    #[cfg(unix)]
    #[test]
    fn linux_joins_the_id_onto_the_xdg_base_directories() {
        use crate::xdg::Base;
        use std::ffi::OsStr;

        let home = Some(OsStr::new("/home/u"));
        let resolve = |base: Base, value: Option<&str>, slot| {
            system::joined(base.resolve_from(value.map(OsStr::new), home), ID, slot)
        };
        assert_eq!(
            resolve(Base::Data, None, Slot::Data),
            native("/home/u/.local/share/so.tilo.notes")
        );
        assert_eq!(
            resolve(Base::Config, Some("/cfg"), Slot::Config),
            native("/cfg/so.tilo.notes")
        );
        assert_eq!(
            resolve(Base::Cache, Some("relative"), Slot::Cache),
            native("/home/u/.cache/so.tilo.notes")
        );
        assert_eq!(
            resolve(Base::State, None, Slot::State),
            native("/home/u/.local/state/so.tilo.notes")
        );
        assert_eq!(
            resolve(Base::Runtime, Some("/run/user/1000"), Slot::Runtime),
            native("/run/user/1000/so.tilo.notes")
        );
    }

    #[cfg(unix)]
    #[test]
    fn macos_joins_the_id_onto_the_library_folders() {
        use crate::xdg::Base;

        let tmpdir = [("TMPDIR", "/var/folders/ab/xyz/T/")];
        let resolve = |base: Base, slot| {
            let dir = crate::macos::base(base, &env(&tmpdir), || Some(PathBuf::from("/Users/u")));
            system::joined(dir, ID, slot)
        };
        let support = native("/Users/u/Library/Application Support/so.tilo.notes");
        assert_eq!(resolve(Base::Data, Slot::Data), support);
        assert_eq!(resolve(Base::Config, Slot::Config), support);
        assert_eq!(resolve(Base::State, Slot::State), support);
        assert_eq!(
            resolve(Base::Cache, Slot::Cache),
            native("/Users/u/Library/Caches/so.tilo.notes")
        );
        assert_eq!(
            resolve(Base::Runtime, Slot::Runtime),
            native("/var/folders/ab/xyz/T/so.tilo.notes")
        );
    }

    #[test]
    fn a_missing_base_directory_keeps_storage_in_memory() {
        let location = system::joined(None, ID, Slot::Data);
        assert_eq!(location.to_string(), "memory:///so.tilo.notes/data");
        assert!(location.native_path().is_err());
    }

    #[test]
    fn the_web_keeps_storage_in_the_profiles_opfs_tree() {
        for (slot, expected) in [
            (Slot::Data, "opfs://default/so.tilo.notes/data"),
            (Slot::Config, "opfs://default/so.tilo.notes/config"),
            (Slot::Cache, "opfs://default/so.tilo.notes/cache"),
            (Slot::State, "opfs://default/so.tilo.notes/state"),
        ] {
            assert_eq!(
                web::storage(slot, ID, DEFAULT_PROFILE).to_string(),
                expected
            );
        }
        assert_eq!(
            web::storage(Slot::Data, ID, "work").to_string(),
            "opfs://work/so.tilo.notes/data"
        );
        assert_eq!(
            web::runtime(ID).map(|location| location.to_string()),
            Ok("memory:///so.tilo.notes/runtime".to_owned())
        );
    }

    #[test]
    fn the_web_has_no_user_folders_workspace_or_font_directory() {
        assert_eq!(
            web::user_dir(UserDir::Documents).map_err(|error| error.feature),
            Err(Feature::UserDirs)
        );
        assert_eq!(
            web::workspace_root().map_err(|error| error.feature),
            Err(Feature::WorkspaceRoot)
        );
        assert_eq!(
            web::font_root().map_err(|error| error.feature),
            Err(Feature::FontRoot)
        );
        assert!(
            web::storage(Slot::Data, ID, "default")
                .native_path()
                .is_err()
        );
    }

    #[test]
    fn the_workspace_root_comes_from_the_launcher() {
        let resolve = |pairs: &[(&str, &str)], default| {
            system::workspace_root_from(&env(pairs), default).ok()
        };
        let linux = Some("/var/workspace");
        assert_eq!(
            resolve(&[("TILO_WORKSPACE_ROOT", "/mnt/ws")], None),
            Some(native("/mnt/ws"))
        );
        assert_eq!(
            resolve(&[("TILO_WORKSPACE_ID", "w1")], linux),
            Some(native("/var/workspace"))
        );
        assert_eq!(
            resolve(
                &[("TILO_WORKSPACE_ROOT", "rel"), ("TILO_WORKSPACE_ID", "w1")],
                linux
            ),
            Some(native("/var/workspace")),
            "a relative root is ignored"
        );
        assert_eq!(resolve(&[("TILO_WORKSPACE_ID", "w1")], None), None);
        assert_eq!(resolve(&[("TILO_WORKSPACE_ID", "")], linux), None);
        assert_eq!(resolve(&[], linux), None);
    }

    #[test]
    fn macos_fonts_come_from_the_running_bundle() {
        assert_eq!(
            system::bundle_fonts(Path::new(
                "/Applications/Tilo Notes.app/Contents/MacOS/notes"
            )),
            Some(PathBuf::from(
                "/Applications/Tilo Notes.app/Contents/Resources/fonts"
            ))
        );
        assert_eq!(
            system::bundle_fonts(Path::new("/usr/local/bin/notes")),
            None
        );
        assert_eq!(system::bundle_fonts(Path::new("notes")), None);
    }

    #[test]
    fn scoped_paths_stay_below_their_root() {
        let app = AppPaths::under("/sandbox", ID);
        assert_eq!(app.data(), native("/sandbox/so.tilo.notes/data"));
        assert_eq!(app.state(), native("/sandbox/so.tilo.notes/state"));
        assert_eq!(app.runtime(), Ok(native("/sandbox/so.tilo.notes/runtime")));
        assert!(app.user_dir(UserDir::Download).is_err());
        assert!(app.workspace_root().is_err());
        assert!(app.font_root().is_err());
        assert_eq!(app.clone().with_profile("work"), app);

        let memory = AppPaths::memory(ID);
        assert_eq!(memory.cache().to_string(), "memory:///so.tilo.notes/cache");
    }

    #[test]
    fn the_system_backend_agrees_with_the_per_user_directories() {
        let app = AppPaths::for_app(ID);
        for (location, base) in [
            (app.data(), crate::data_dir()),
            (app.config(), crate::config_dir()),
            (app.cache(), crate::cache_dir()),
            (app.state(), crate::state_dir()),
        ] {
            if let Some(base) = base {
                assert_eq!(location, Location::Native(base.join(ID)));
            }
        }
        assert_eq!(
            app.runtime().ok(),
            crate::runtime_dir().map(|dir| Location::Native(dir.join(ID)))
        );
    }

    #[test]
    fn ids_are_escaped_into_one_component() {
        for (raw, expected) in [
            (ID, ID),
            ("tilo-chat", "tilo-chat"),
            ("a/b", "a%2Fb"),
            ("..\\x", "..%5Cx"),
            ("50%", "50%25"),
            ("c:\n", "c%3A%0A"),
            (".", "%2E"),
            ("..", "%2E%2E"),
            ("", "%"),
        ] {
            assert_eq!(AppPaths::for_app(raw).id(), expected, "{raw:?}");
        }
        assert_eq!(
            AppPaths::memory("../escape").data().to_string(),
            "memory:///..%2Fescape/data"
        );
    }
}
