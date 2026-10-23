//! Locations as portable values: an operating-system path, or a path inside a
//! virtual storage root the platform's file layer serves.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::caps::{Feature, Unsupported, WEB_NATIVE_PATHS};

/// Where something lives. Native targets give paths; the web gives virtual
/// roots, and so does a native process that has no home directory, whose
/// storage then lasts only as long as the process.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    Native(PathBuf),
    Virtual(VirtualRoot, VPath),
}

impl Location {
    /// The operating-system path, for code that must hand one to a native
    /// API. Portable code opens locations through the platform's file layer
    /// instead.
    ///
    /// # Errors
    ///
    /// [`Unsupported`] for a virtual location, which has no path.
    pub fn native_path(&self) -> Result<&Path, Unsupported> {
        match self {
            Self::Native(path) => Ok(path),
            Self::Virtual(root, _) => Err(Unsupported {
                feature: Feature::NativePaths,
                reason: match root {
                    VirtualRoot::Opfs { .. } => WEB_NATIVE_PATHS,
                    VirtualRoot::Memory => "Storage kept in memory has no file system path.",
                },
            }),
        }
    }

    /// `relative` below this location.
    #[must_use]
    pub fn join(&self, relative: &VPath) -> Self {
        match self {
            Self::Native(path) => Self::Native(
                relative
                    .components()
                    .fold(path.clone(), |path, component| path.join(component)),
            ),
            Self::Virtual(root, path) => Self::Virtual(root.clone(), path.join(relative)),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(path) => path.display().fmt(f),
            Self::Virtual(VirtualRoot::Opfs { profile }, path) => {
                write!(f, "opfs://{profile}/{path}")
            }
            Self::Virtual(VirtualRoot::Memory, path) => write!(f, "memory:///{path}"),
        }
    }
}

/// A storage tree that is not a native directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VirtualRoot {
    /// The browser's origin-private file system, one tree per Tilo profile.
    Opfs { profile: String },
    /// Memory that lasts as long as the page, or the native process.
    Memory,
}

/// A relative path inside a [`VirtualRoot`] or below a native location:
/// `/`-separated components, none of them empty, `.` or `..`. The empty path
/// is the root itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VPath(String);

/// Why a string is not a [`VPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidVPath {
    pub reason: &'static str,
}

impl fmt::Display for InvalidVPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid virtual path: {}", self.reason)
    }
}

impl std::error::Error for InvalidVPath {}

impl VPath {
    /// The root of whatever the path is relative to.
    #[must_use]
    pub const fn root() -> Self {
        Self(String::new())
    }

    pub fn new(path: &str) -> Result<Self, InvalidVPath> {
        if path.is_empty() {
            return Ok(Self::root());
        }
        for component in path.split('/') {
            check_component(component)?;
        }
        Ok(Self(path.to_owned()))
    }

    /// Joins components already known to be valid.
    pub(crate) fn from_components<'a>(components: impl IntoIterator<Item = &'a str>) -> Self {
        let mut path = Self::root();
        for component in components {
            debug_assert!(check_component(component).is_ok(), "{component:?}");
            path.push(component);
        }
        path
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|component| !component.is_empty())
    }

    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        for component in other.components() {
            joined.push(component);
        }
        joined
    }

    fn push(&mut self, component: &str) {
        if !self.0.is_empty() {
            self.0.push('/');
        }
        self.0.push_str(component);
    }
}

impl fmt::Display for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn check_component(component: &str) -> Result<(), InvalidVPath> {
    let reason = match component {
        "" => "empty component (absolute path, or doubled or trailing `/`)",
        "." | ".." => "`.` and `..` are not components",
        _ if component.contains('\\') => "backslash in a component",
        _ if component.contains('\0') => "NUL in a component",
        _ => return Ok(()),
    };
    Err(InvalidVPath { reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vpaths_are_relative_and_normal() {
        assert_eq!(VPath::new("").map(|path| path.is_root()), Ok(true));
        assert_eq!(
            VPath::new("notes/2026/a.md").map(|path| path.components().count()),
            Ok(3)
        );
        for invalid in ["/abs", "a//b", "a/", "./a", "a/../b", "..", "a\\b", "a\0"] {
            assert!(VPath::new(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn joining_keeps_the_root_and_appends_components() {
        let base = VPath::new("so.tilo.notes/data").unwrap();
        let child = VPath::new("drafts/a.md").unwrap();
        assert_eq!(base.join(&child).as_str(), "so.tilo.notes/data/drafts/a.md");
        assert_eq!(VPath::root().join(&child), child);
        assert_eq!(base.join(&VPath::root()), base);

        let native = Location::Native(PathBuf::from("/srv/app"));
        assert_eq!(
            native.join(&child),
            Location::Native(PathBuf::from("/srv/app/drafts/a.md"))
        );
        let opfs = Location::Virtual(
            VirtualRoot::Opfs {
                profile: "default".into(),
            },
            base,
        );
        assert_eq!(
            opfs.join(&child).to_string(),
            "opfs://default/so.tilo.notes/data/drafts/a.md"
        );
    }

    #[test]
    fn only_native_locations_have_a_path() {
        let native = Location::Native(PathBuf::from("/srv/app"));
        assert_eq!(native.native_path(), Ok(Path::new("/srv/app")));
        let memory = Location::Virtual(VirtualRoot::Memory, VPath::new("app/runtime").unwrap());
        assert_eq!(memory.to_string(), "memory:///app/runtime");
        assert_eq!(
            memory.native_path().map_err(|error| error.feature),
            Err(Feature::NativePaths)
        );
    }
}
