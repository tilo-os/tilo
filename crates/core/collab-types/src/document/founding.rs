//! The founding an app's record is derived from: what every holder makes of it alike.
//!
//! An app's record holds its content alone. Two devices that make the same record before they
//! sync (a person's vault opened on a phone and a laptop at once) would each make the containers
//! its fields keep their items in, and the items one of them adds first would land in a
//! container the merge leaves behind. So the containers come first, in one change every holder
//! derives from the record's kind and id alone: an actor derived from them makes it at time zero.
//! The store admits that change only as the one it derives itself ([`structure`]); the app's
//! own first values follow as any change of its.

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjType, ROOT};

/// The derived founding of the record `id` of `kind`, whose fields keep their items in the
/// `containers` named at its root: `None` for a kind that keeps none.
#[must_use]
pub fn structure(
    kind: &str,
    id: &str,
    containers: &[(String, ObjType)],
) -> Option<automerge::Change> {
    if containers.is_empty() {
        return None;
    }
    let mut doc = AutoCommit::new().with_actor(tilo_resource::genesis::actor(id, kind, ""));
    for (field, container) in containers {
        doc.put_object(ROOT, field.as_str(), *container).ok()?;
    }
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    doc.get_last_local_change()
}
