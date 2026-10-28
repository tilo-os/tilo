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
    use crate::namespace::ROOT_INO;
    use fuser::FileType;

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

    /// What the kernel is told about a record: its stored mode and, for a
    /// link, that it is one — through `getattr`, `lookup` and `readdir`
    /// alike, before and after the document is reloaded as a peer or a
    /// restart would.
    #[test]
    fn the_kernel_reads_a_records_mode_and_sees_a_link_as_one() {
        let mut ns = Namespace::new("posix-kernel").unwrap();
        let script = ns.create_file(ROOT_INO, "run.sh", None, 0, 1).unwrap();
        let script_record = ns.record_id_for(script).unwrap();
        assert!(ns.put_posix_attrs(
            script_record,
            &PosixAttrs::of_local(0o100_755, false, false)
        ));
        let link = ns.create_file(ROOT_INO, "latest", None, 0, 1).unwrap();
        let link_record = ns.record_id_for(link).unwrap();
        assert!(ns.put_posix_attrs(link_record, &PosixAttrs::of_local(0, false, true)));
        let saved = ns.save();

        for mut ns in [ns, Namespace::from_bytes("reloaded", &saved).unwrap()] {
            assert_eq!(
                ns.getattr(script).map(|attr| attr.perm),
                Some(0o755),
                "the executable bit a record stores is the one getattr reports",
            );
            assert_eq!(
                ns.getattr(link).map(|attr| attr.kind),
                Some(FileType::Symlink),
                "getattr reports a link as a link, not as the file holding it",
            );
            assert_eq!(
                ns.lookup(ROOT_INO, b"latest").map(|attr| attr.kind),
                Some(FileType::Symlink),
            );
            let listed = ns.readdir(ROOT_INO).expect("list root");
            let kind_of = |name: &str| {
                listed
                    .iter()
                    .find(|(_, _, entry)| entry.0 == name.as_bytes())
                    .map(|(_, kind, _)| *kind)
            };
            assert_eq!(kind_of("latest"), Some(FileType::Symlink));
            assert_eq!(
                kind_of("run.sh"),
                Some(FileType::RegularFile),
                "a file whose content is not a link is still a file",
            );
            assert_eq!(
                ns.structural_kind(link),
                Some(FileType::RegularFile),
                "a version describes the record, not the kernel's link view",
            );
        }
    }
}
