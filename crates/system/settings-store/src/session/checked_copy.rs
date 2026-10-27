//! Settings JSON on top of descriptor-anchored native byte access.

use std::io;
use std::path::Path;

use serde_json::{Map, Value};
use tilo_files::native::checked;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) len: u64,
    pub(super) modified: tilo_clock::SystemTime,
    file: checked::Identity,
}

impl From<checked::Identity> for Identity {
    fn from(file: checked::Identity) -> Self {
        Self {
            len: file.len,
            modified: tilo_clock::from_std(file.modified),
            file,
        }
    }
}

/// The platform retains the parent descriptor across every operation.
#[derive(Debug)]
pub(super) struct CheckedCopy(checked::CheckedCopy);

impl CheckedCopy {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        checked::CheckedCopy::open(path).map(Self)
    }

    pub(super) fn lock(&self) -> io::Result<tilo_files::native::lock::FileLock> {
        self.0.lock()
    }

    /// Only a missing copy means an empty object. Invalid UTF-8, JSON and
    /// non-object documents are errors, as are unsafe native entries.
    pub(super) fn read(&self) -> io::Result<(Map<String, Value>, Option<Identity>)> {
        let (bytes, identity) = self.0.read()?;
        let Some(identity) = identity else {
            return Ok((Map::new(), None));
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let Value::Object(document) = value else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "settings copy must contain a JSON object",
            ));
        };
        Ok((document, Some(identity.into())))
    }

    pub(super) fn identity(&self) -> io::Result<Option<Identity>> {
        self.0.identity().map(|identity| identity.map(Into::into))
    }

    pub(super) fn write(&self, document: &Map<String, Value>) -> io::Result<Identity> {
        let mut bytes = serde_json::to_vec_pretty(document).map_err(io::Error::other)?;
        bytes.push(b'\n');
        self.0.write(&bytes).map(Into::into)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn missing_is_empty_but_malformed_input_is_not() {
        let dir = crate::test_dir();
        let root = dir.path().canonicalize().unwrap();
        let path = root.join("settings.json");
        let copy = CheckedCopy::open(&path).unwrap();
        assert_eq!(copy.read().unwrap(), (Map::new(), None));
        for contents in [b"".as_slice(), b"{", b"[]", b"null", b"\xff"] {
            std::fs::write(&path, contents).unwrap();
            assert_eq!(copy.read().unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }
}
