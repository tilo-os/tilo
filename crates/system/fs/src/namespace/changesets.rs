//! Changeset boundaries, so a peer applies one device's capture epoch as a whole:
//! a closed mark means every operation of the epoch arrived, an open one that it
//! may have only some.

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

use super::Namespace;

const CHANGESET_PREFIX: &str = "changeset/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesetMark {
    pub epoch: u64,
    pub open: bool,
    /// Earliest and latest version time (Unix ms) the epoch publishes.
    pub first_ms: u64,
    pub last_ms: u64,
    /// When the mark was last written, by the author's clock; a long publication
    /// rewrites it, so a peer can tell progress from a crash.
    pub at_ms: u64,
}

impl Namespace {
    /// The caller broadcasts the next incremental save.
    pub fn put_changeset_mark(&mut self, device: [u8; 32], mark: &ChangesetMark) -> bool {
        let Ok(bytes) = postcard::to_allocvec(mark) else {
            return false;
        };
        let key = format!("{CHANGESET_PREFIX}{}", hex::encode(device));
        self.doc
            .put(&ROOT, key.as_str(), ScalarValue::Bytes(bytes))
            .is_ok()
    }

    pub fn changeset_marks(&self) -> Vec<([u8; 32], ChangesetMark)> {
        self.doc
            .keys(&ROOT)
            .filter_map(|key| {
                let device = key.strip_prefix(CHANGESET_PREFIX)?;
                let device: [u8; 32] = hex::decode(device).ok()?.try_into().ok()?;
                match self.doc.get(&ROOT, key.as_str()) {
                    Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                        ScalarValue::Bytes(bytes) => {
                            Some((device, postcard::from_bytes(bytes).ok()?))
                        }
                        _ => None,
                    },
                    _ => None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_mark_arrives_after_everything_its_epoch_changed() {
        let mut a = Namespace::new("a").unwrap();
        let mut b = Namespace::from_bytes("b", &a.save()).unwrap();
        let open = ChangesetMark {
            epoch: 3,
            open: true,
            first_ms: 10,
            last_ms: 20,
            at_ms: 5,
        };
        assert!(a.put_changeset_mark([1; 32], &open));
        let d1 = a.save_incremental();
        let dir = a.create_dir(crate::namespace::ROOT_INO, "made-in-epoch", 1);
        assert!(dir.is_some());
        let d2 = a.save_incremental();
        a.put_changeset_mark(
            [1; 32],
            &ChangesetMark {
                open: false,
                ..open
            },
        );
        let d3 = a.save_incremental();
        b.apply_incremental(&d3).unwrap();
        assert!(b.changeset_marks().is_empty());
        b.apply_incremental(&d1).unwrap();
        assert_eq!(b.changeset_marks(), vec![([1; 32], open)]);
        b.apply_incremental(&d2).unwrap();
        let marks = b.changeset_marks();
        assert_eq!(marks.len(), 1);
        assert!(!marks[0].1.open);
        assert!(
            b.lookup(crate::namespace::ROOT_INO, b"made-in-epoch")
                .is_some()
        );
    }
}
