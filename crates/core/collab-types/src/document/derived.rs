//! Changes every holder of a record derives alike, byte for byte, and so names no one as having
//! made them: a record's founding identity ([`super::identity::founding`]) and the root maps of a
//! workspace's file tree ([`root_map`]). Two holders that make one of these apart make the one
//! change, so their copies merge as one document; a change naming its maker would differ per
//! maker, and the merge would hold two identities or two maps. A receiver takes such a change
//! naming no one only when it is the derived change itself ([`is_derived`]), never an edit
//! dressed as one.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_actions::FrameworkChange;

/// The writer every holder makes a root map of a file tree under: one per key.
const ROOT_MAP_WRITER: &str = "tilo.files.tree.v1/";

/// The change that makes the map at `key` of a file tree's root: one writer named for the key,
/// no dependencies, the time zero, so a computer that wrote files before it reached the
/// workspace, a page that put the first file in and a host that put one for an agent each hold
/// that one change.
#[must_use]
pub fn root_map(key: &str) -> AutoCommit {
    let writer = format!("{ROOT_MAP_WRITER}{key}");
    let mut made = AutoCommit::new().with_actor(automerge::ActorId::from(writer.as_bytes()));
    // A fresh document takes any key at its root.
    let _made = made.put_object(&ROOT, key, ObjType::Map);
    made.commit_with(
        CommitOptions::default()
            .with_message(FrameworkChange::Structure.change_message())
            .with_time(0),
    );
    made
}

/// Whether `change`, named as the framework change `named` and naming no one, is the change
/// every holder derives alike: a founding identity for a genesis, a file tree's root map for a
/// structure. Every other framework change names who made it. `load` loads a change alone, as
/// whoever received it may: a founding is read from what it writes.
#[must_use]
pub fn is_derived(
    change: &automerge::Change,
    named: FrameworkChange,
    load: impl FnOnce(&[u8]) -> Option<AutoCommit>,
) -> bool {
    if !change.deps().is_empty() || change.seq() != 1 || change.timestamp() != 0 {
        return false;
    }
    match named {
        FrameworkChange::Genesis => load(change.raw_bytes())
            .is_some_and(|doc| super::identity::is_derived_founding(&doc, change)),
        FrameworkChange::Structure => is_derived_root_map(change),
        _ => false,
    }
}

/// Whether `change` is a file tree's [`root_map`], for the key its writer names.
fn is_derived_root_map(change: &automerge::Change) -> bool {
    let Some(key) = std::str::from_utf8(change.actor_id().to_bytes())
        .ok()
        .and_then(|writer| writer.strip_prefix(ROOT_MAP_WRITER))
    else {
        return false;
    };
    root_map(key).get_heads() == [change.hash()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only_change(mut doc: AutoCommit) -> automerge::Change {
        let mut changes = doc.get_changes(&[]);
        assert_eq!(changes.len(), 1);
        changes.remove(0)
    }

    #[test]
    fn a_root_map_is_derived_and_an_edit_dressed_as_one_is_not() {
        assert!(is_derived(
            &only_change(root_map("nodes")),
            FrameworkChange::Structure,
            |_| None
        ));
        let mut dressed = AutoCommit::new().with_actor(automerge::ActorId::from(
            b"tilo.files.tree.v1/nodes".as_slice(),
        ));
        let nodes = dressed.put_object(&ROOT, "nodes", ObjType::Map).unwrap();
        dressed.put(&nodes, "a file", "smuggled").unwrap();
        dressed.commit_with(
            CommitOptions::default()
                .with_message(FrameworkChange::Structure.change_message())
                .with_time(0),
        );
        assert!(!is_derived(
            &only_change(dressed),
            FrameworkChange::Structure,
            |_| None
        ));
    }
}
