//! [`AbsolutePath`]: the one type an action takes a file or folder on this
//! device as.
//!
//! A relative path names a different place depending on where the app's
//! process happens to run, so an app that wrote to one would scatter files
//! into its working directory. An argument of this type is refused when it
//! is decoded, before any app sees it: every effect taking a path refuses a
//! relative one the same way, with the same message.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

/// An absolute path on this device. It cannot be made from a relative one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AbsolutePath(PathBuf);

/// Why a path was refused: it was relative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativePath(pub PathBuf);

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the path must be absolute, not relative to where the app runs: {}",
            self.0.display()
        )
    }
}

impl std::error::Error for RelativePath {}

impl From<RelativePath> for String {
    fn from(error: RelativePath) -> Self {
        error.to_string()
    }
}

impl AbsolutePath {
    /// `path`, refused when it is relative.
    ///
    /// # Errors
    ///
    /// [`RelativePath`] when `path` is not absolute.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, RelativePath> {
        let path = path.into();
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(RelativePath(path))
        }
    }

    /// The path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The path, owned.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    /// The path as text, lossily for one that is not UTF-8.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl std::ops::Deref for AbsolutePath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for AbsolutePath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl From<AbsolutePath> for PathBuf {
    fn from(path: AbsolutePath) -> Self {
        path.0
    }
}

impl TryFrom<PathBuf> for AbsolutePath {
    type Error = RelativePath;

    fn try_from(path: PathBuf) -> Result<Self, RelativePath> {
        Self::new(path)
    }
}

impl TryFrom<&Path> for AbsolutePath {
    type Error = RelativePath;

    fn try_from(path: &Path) -> Result<Self, RelativePath> {
        Self::new(path)
    }
}

impl TryFrom<String> for AbsolutePath {
    type Error = RelativePath;

    fn try_from(path: String) -> Result<Self, RelativePath> {
        Self::new(path)
    }
}

impl TryFrom<&str> for AbsolutePath {
    type Error = RelativePath;

    fn try_from(path: &str) -> Result<Self, RelativePath> {
        Self::new(path)
    }
}

impl std::str::FromStr for AbsolutePath {
    type Err = RelativePath;

    fn from_str(path: &str) -> Result<Self, RelativePath> {
        Self::new(path)
    }
}

impl fmt::Display for AbsolutePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

impl<'de> Deserialize<'de> for AbsolutePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(PathBuf::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl crate::JsonSchema for AbsolutePath {
    fn json_schema() -> Value {
        json!({"type": "string", "description": "An absolute path on this device"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_is_refused_when_decoded() {
        let error = serde_json::from_value::<AbsolutePath>(json!("notes/out.txt")).unwrap_err();
        assert!(
            error.to_string().contains("must be absolute"),
            "got {error}"
        );
        assert!(serde_json::from_value::<AbsolutePath>(json!("")).is_err());
        let path = serde_json::from_value::<AbsolutePath>(json!("/home/you/out.txt")).unwrap();
        assert_eq!(path.as_path(), Path::new("/home/you/out.txt"));
        assert_eq!(
            serde_json::to_value(&path).unwrap(),
            json!("/home/you/out.txt")
        );
    }
}
