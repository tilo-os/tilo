//! Concrete source-consent authority and portable journey business state.
//! Frontend workers and application payloads are deliberately not stored here.
use crate::nofollow::{
    metadata_nofollow, open_directory_nofollow, reject_windows_placeholder, same_directory_identity,
};
use cap_primitives::fs::Metadata;
use std::{
    ffi::OsString,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported;
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Native media preparation is unsupported on this platform")
    }
}
impl std::error::Error for Unsupported {}
impl From<Unsupported> for io::Error {
    fn from(error: Unsupported) -> Self {
        Self::new(io::ErrorKind::Unsupported, error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnReason {
    ReviewedHomeChanged,
    ReviewedHomeUnavailable,
}
impl ReturnReason {
    pub fn from_state(state: &str) -> Option<Self> {
        match state {
            "source-changed" => Some(Self::ReviewedHomeChanged),
            "source-unavailable" => Some(Self::ReviewedHomeUnavailable),
            _ => None,
        }
    }
    pub fn error_key(self) -> &'static str {
        match self {
            Self::ReviewedHomeChanged => "installer-source-changed",
            Self::ReviewedHomeUnavailable => "installer-source-unavailable",
        }
    }
    pub fn button_key(self) -> &'static str {
        match self {
            Self::ReviewedHomeChanged => "installer-source-review",
            Self::ReviewedHomeUnavailable => "installer-source-refresh",
        }
    }
    pub fn state(self) -> &'static str {
        match self {
            Self::ReviewedHomeChanged => "source-changed",
            Self::ReviewedHomeUnavailable => "source-unavailable",
        }
    }
}

struct Edge {
    parent: File,
    name: OsString,
    identity: Metadata,
}
struct Directory {
    file: File,
    identity: Metadata,
    edges: Vec<Edge>,
}
impl Directory {
    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir if names.is_empty() => {
                    anchor.push(component);
                }
                Component::Normal(name) => names.push(name.to_os_string()),
                _ => return Err(io::ErrorKind::PermissionDenied.into()),
            }
        }
        let mut file =
            cap_primitives::fs::open_ambient_dir(&anchor, cap_primitives::ambient_authority())?;
        let mut edges = Vec::new();
        for name in names {
            let identity = metadata_nofollow(&file, Path::new(&name))?;
            let child = open_directory_nofollow(&file, Path::new(&name), &identity)?;
            edges.push(Edge {
                parent: file,
                name,
                identity,
            });
            file = child;
        }
        let identity = Metadata::from_file(&file)?;
        let directory = Self {
            file,
            identity,
            edges,
        };
        directory.validate()?;
        Ok(directory)
    }
    fn validate(&self) -> io::Result<()> {
        for edge in &self.edges {
            let observed = metadata_nofollow(&edge.parent, Path::new(&edge.name))?;
            reject_windows_placeholder(&observed)?;
            if observed.is_symlink()
                || !observed.is_dir()
                || !same_directory_identity(&observed, &edge.identity)
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        if !same_directory_identity(&Metadata::from_file(&self.file)?, &self.identity) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}

/// A completed matching-directory grant. Owns both complete ancestor chains;
/// paths are descriptive and never reconstruct a previously admitted grant.
/// This proves directory identity, not a capture, immutable epoch or raw medium.
pub struct SourceGrant {
    selected: PathBuf,
    expected: PathBuf,
    selected_directory: Directory,
    expected_directory: Directory,
}
impl SourceGrant {
    /// Admit an explicit selection only when it names the expected directory.
    pub fn admit(selected: &Path, expected: &Path) -> io::Result<Self> {
        let grant = Self {
            selected: selected.into(),
            expected: expected.into(),
            selected_directory: Directory::open(selected)?,
            expected_directory: Directory::open(expected)?,
        };
        grant.validate()?;
        Ok(grant)
    }
    pub fn selected(&self) -> &PathBuf {
        &self.selected
    }
    pub fn expected(&self) -> &PathBuf {
        &self.expected
    }
    /// Recheck both held directory chains and their matching identity.
    pub fn validate(&self) -> io::Result<()> {
        self.selected_directory.validate()?;
        self.expected_directory.validate()?;
        if !same_directory_identity(
            &self.selected_directory.identity,
            &self.expected_directory.identity,
        ) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
    /// A refresh must retain the original identity, not merely its path.
    ///
    /// # Errors
    /// Returns an error if either grant fails [`Self::validate`]. Valid grants
    /// referring to different directories return `Ok(false)`.
    pub fn matches(&self, other: &Self) -> io::Result<bool> {
        self.validate()?;
        other.validate()?;
        Ok(same_directory_identity(
            &self.selected_directory.identity,
            &other.selected_directory.identity,
        ) && same_directory_identity(
            &self.expected_directory.identity,
            &other.expected_directory.identity,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_grant_rejects_replaced_ancestor() {
        let temporary = tempfile::tempdir().unwrap();
        let ancestor = temporary.path().join("ancestor");
        let selected = ancestor.join("home");
        std::fs::create_dir_all(&selected).unwrap();
        let grant = SourceGrant::admit(&selected, &selected).unwrap();
        std::fs::rename(&ancestor, temporary.path().join("retained")).unwrap();
        std::fs::create_dir_all(&selected).unwrap();
        assert!(grant.validate().is_err());
        let replacement = SourceGrant::admit(&selected, &selected).unwrap();
        assert!(grant.matches(&replacement).is_err());
    }
    #[test]
    fn refresh_matches_the_held_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let first = SourceGrant::admit(temporary.path(), temporary.path()).unwrap();
        let refreshed = SourceGrant::admit(temporary.path(), temporary.path()).unwrap();
        assert!(first.matches(&refreshed).unwrap());
        assert_eq!(first.selected(), temporary.path());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_selection_is_not_a_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        std::os::unix::fs::symlink(temporary.path(), &selected).unwrap();
        assert!(SourceGrant::admit(&selected, temporary.path()).is_err());
    }

    #[test]
    fn mismatched_selection_is_not_a_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected");
        let expected = temporary.path().join("expected");
        std::fs::create_dir(&selected).unwrap();
        std::fs::create_dir(&expected).unwrap();
        assert!(SourceGrant::admit(&selected, &expected).is_err());
    }
}
