//! POSIX attributes that replicate with a record: permission bits and
//! whether a file is a symbolic link (`storage.md` §6.6).
//!
//! Owner and group never replicate: every materialised path belongs to the
//! local person. A symbolic link is a file record whose content is the link
//! target, byte for byte, so its target is versioned, signed and fetched
//! like any other content; only the fact that it is a link and the mode
//! bits are carried here, as one root-level scalar per record, in causal
//! order with the record's own changes. A record without one is a regular
//! file (0644) or a directory (0755).

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::FileRecordId;
use serde::{Deserialize, Serialize};

use super::Namespace;

const POSIX_PREFIX: &str = "posix/";

/// Mode of a file nobody gave another one.
pub const DEFAULT_FILE_MODE: u32 = 0o644;
/// Mode of a directory nobody gave another one.
pub const DEFAULT_DIR_MODE: u32 = 0o755;

/// The replicated POSIX attributes of one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosixAttrs {
    /// Permission bits. Files keep `rwx` for user, group and other;
    /// directories also keep setgid. Setuid is never replicated.
    pub mode: u32,
    /// The record's content is a symbolic link's target.
    pub symlink: bool,
}

impl PosixAttrs {
    /// The attributes a record has when none were published.
    pub fn default_for(dir: bool) -> Self {
        Self {
            mode: if dir {
                DEFAULT_DIR_MODE
            } else {
                DEFAULT_FILE_MODE
            },
            symlink: false,
        }
    }

    /// Attributes of a local path with mode bits `mode` (from `st_mode`),
    /// keeping only what replicates.
    pub fn of_local(mode: u32, dir: bool, symlink: bool) -> Self {
        let mode = if symlink {
            // A link's own mode means nothing on Linux.
            0o777
        } else if dir {
            mode & 0o2777
        } else {
            mode & 0o777
        };
        Self { mode, symlink }
    }
}

impl Namespace {
    /// The published POSIX attributes of `record`, if any.
    pub fn posix_attrs(&self, record: FileRecordId) -> Option<PosixAttrs> {
        let key = format!("{POSIX_PREFIX}{}", record.0);
        match self.doc.get(&ROOT, key.as_str()) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Bytes(bytes) => postcard::from_bytes(bytes).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    /// Records the POSIX attributes of `record`. The caller broadcasts the
    /// next incremental save.
    pub fn put_posix_attrs(&mut self, record: FileRecordId, attrs: &PosixAttrs) -> bool {
        let Ok(bytes) = postcard::to_allocvec(attrs) else {
            return false;
        };
        let key = format!("{POSIX_PREFIX}{}", record.0);
        self.doc
            .put(&ROOT, key.as_str(), ScalarValue::Bytes(bytes))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_replicate_and_keep_only_what_is_portable() {
        let mut a = Namespace::new("a").unwrap();
        let record = FileRecordId::new();
        assert!(a.posix_attrs(record).is_none());
        let exec = PosixAttrs::of_local(0o104_755, false, false);
        assert_eq!(exec.mode, 0o755, "setuid and the file type are dropped");
        assert!(a.put_posix_attrs(record, &exec));
        let b = Namespace::from_bytes("b", &a.save()).unwrap();
        assert_eq!(b.posix_attrs(record), Some(exec));
        assert_eq!(PosixAttrs::of_local(0o42_775, true, false).mode, 0o2775);
        let link = PosixAttrs::of_local(0o120_777, false, true);
        assert!(link.symlink);
        assert_eq!(PosixAttrs::default_for(true).mode, 0o755);
    }
}
