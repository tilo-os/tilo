//! POSIX attributes that replicate with a record: permission bits and whether it
//! is a symbolic link. Owner and group never replicate, and a record without
//! attributes is a 0644 file or a 0755 directory.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use fs_protocol::FileRecordId;
use serde::{Deserialize, Serialize};

use super::Namespace;

const POSIX_PREFIX: &str = "posix/";

pub const DEFAULT_FILE_MODE: u32 = 0o644;
pub const DEFAULT_DIR_MODE: u32 = 0o755;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosixAttrs {
    /// Permission bits; setuid is never replicated.
    pub mode: u32,
    pub symlink: bool,
}

impl PosixAttrs {
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

    /// The caller broadcasts the next incremental save.
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

    #[test]
    fn a_file_is_presented_with_the_mode_it_was_given() {
        let mut ns = Namespace::new("a").unwrap();
        let ino = ns
            .create_file(crate::namespace::ROOT_INO, "tool", None, 0, 1)
            .unwrap();
        assert_eq!(ns.getattr(ino).unwrap().perm, 0o644);
        let record = ns.record_id_for(ino).unwrap();
        assert!(ns.put_posix_attrs(record, &PosixAttrs::of_local(0o104_755, false, false)));
        assert_eq!(
            ns.getattr(ino).unwrap().perm,
            0o755,
            "executable, and never setuid"
        );
        let mut b = Namespace::from_bytes("b", &ns.save()).unwrap();
        let there = b.lookup(crate::namespace::ROOT_INO, "tool").unwrap();
        assert_eq!(there.perm, 0o755, "the mode replicates with the file");
    }
}
