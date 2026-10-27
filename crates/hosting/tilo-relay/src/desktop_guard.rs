//! The relay's check of each write to a room's shared desktop: nobody
//! closes, moves or renames a strip or window someone else made unless the
//! room lets them moderate it ([`tilo_desktop_model::ownership`]).
//!
//! The desktop record keeps every workspace, strip, group and window as an
//! entry of its own ([`tilo_desktop_model::shared_record`]), so each change
//! writes only the entries its author changed, and concurrent changes merge.
//! What a change did is read from the desktop its author saw (the record at
//! the change's dependencies) and the one it left (the record at the change
//! itself): someone who never saw a window cannot have closed it, and a
//! change cannot touch an entry its author never saw, since its own
//! dependencies are what that entry would have to be among.
//!
//! A write is admitted when every act on what its writer did not make is
//! one the room lets them do ([`tilo_desktop_model::ownership::act_node`]).

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::ownership::{SharedDesktop, Touched, foreign_acts};
use tilo_desktop_model::shared_record;

/// A room's shared desktop as the relay follows it.
pub(crate) struct DesktopGuard {
    doc: AutoCommit,
}

/// Why a write of the shared desktop is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DesktopRefusal {
    /// It does what its writer may not to something someone else made.
    #[error(
        "{writer} may not {verb} {id} on the shared desktop, which {owner} made",
        verb = act.act.verb(),
        id = act.id,
        owner = act.owner.as_deref().unwrap_or("nobody")
    )]
    NotTheirs { writer: String, act: Touched },
    /// It is not a desktop, or not one the relay can read.
    #[error("not a shared desktop: {0}")]
    Unreadable(String),
}

impl DesktopGuard {
    pub(crate) const fn new(doc: AutoCommit) -> Self {
        Self { doc }
    }

    /// Take in changes the relay's own device made, without judging them.
    #[cfg(feature = "agents")]
    pub(crate) fn absorb(&mut self, other: &mut AutoCommit) {
        let _ = self.doc.merge_incoming(other);
    }

    /// Decide `mutation`, a write by `writer` (a person id): each act of
    /// each of its changes on what `writer` did not make, and that is not
    /// released by its maker leaving the room (`members`: the room's
    /// current members), must be one `allows` lets them do. Takes the write in when it is admitted, and
    /// leaves the desktop as it was when it is refused.
    pub(crate) fn decide(
        &mut self,
        mutation: &DocumentMutation,
        writer: &str,
        members: Option<&std::collections::BTreeSet<String>>,
        allows: &dyn Fn(&Touched) -> bool,
    ) -> Result<(), DesktopRefusal> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        let mut next = self.doc.clone();
        let before = next.get_heads();
        next.load_incoming(bytes)
            .map_err(|error| DesktopRefusal::Unreadable(error.to_string()))?;
        for change in next.get_changes(&before) {
            let seen = shared_record::read(&next, Some(change.deps())).unwrap_or_default();
            let left = shared_record::read(&next, Some(&[change.hash()]))
                .unwrap_or_else(SharedDesktop::default);
            if let Some(act) = foreign_acts(&seen, &left, writer, members)
                .into_iter()
                .find(|act| !allows(act))
            {
                return Err(DesktopRefusal::NotTheirs {
                    writer: writer.to_owned(),
                    act,
                });
            }
        }
        self.doc = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
