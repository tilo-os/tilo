//! The check of each write to a workspace's shared desktop: nobody closes, moves
//! or renames a strip or window someone else made unless the workspace lets them
//! moderate it ([`crate::ownership`]).
//!
//! The desktop record keeps every workspace, strip, group and window as an
//! entry of its own ([`super`]), so each change writes only the entries its
//! author changed, and concurrent changes merge. What a change did is read
//! from the desktop its author saw (the record at the change's dependencies)
//! and the one it left (the record at the change itself): someone who never
//! saw a window cannot have closed it, and a change cannot touch an entry its
//! author never saw, since its own dependencies are what that entry would
//! have to be among.
//!
//! A write is admitted when every act on what its writer did not make, and
//! every strip or window it adds in someone else's name, is one the workspace
//! lets them do ([`crate::ownership::act_node`]), and it binds no window to
//! another record than the one it showed: a record route is a window's
//! identity, so not even its maker rebinds it. Whoever holds the
//! record decides a write with it before taking it in: a relay at its door,
//! a governor admitting it, and every device that receives it.

use std::collections::BTreeSet;

use automerge::{AutoCommit, ChangeHash};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;

use crate::ids::TabId;
use crate::ownership::{
    DESKTOP_APP, DESKTOP_RECORD, Touched, claimed, foreign_acts, rebound_windows,
};

/// Whether `(app, record)` is a workspace's shared desktop.
#[must_use]
pub fn is_shared_desktop(app: &str, record: &str) -> bool {
    app == DESKTOP_APP && record == DESKTOP_RECORD
}

/// Whether `(app, record)` is a record of the desktop app: the workspace's layout, or one person's
/// view of it.
#[must_use]
pub fn is_desktop(app: &str, record: &str) -> bool {
    app == DESKTOP_APP
        && (record == DESKTOP_RECORD || crate::person_view::person_of(record).is_some())
}

/// Who may write `(app, record)`: every member writes the workspace's layout, and a person's view
/// of it is that person's alone, so the others' devices and their pages show where they are looking
/// and never move it.
///
/// # Errors
/// Why `person` may not write it.
pub fn may_write(app: &str, record: &str, person: &str) -> Result<(), NotTheirView> {
    let owner = crate::person_view::person_of(record).filter(|_| app == DESKTOP_APP);
    match owner {
        Some(owner) if owner != person => Err(NotTheirView {
            writer: person.to_owned(),
            owner: owner.to_owned(),
        }),
        _ => Ok(()),
    }
}

/// Why a write of a person's view of the layout was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{writer} may not write {owner}'s view of the layout: a view is its person's")]
pub struct NotTheirView {
    /// The writer's person id.
    pub writer: String,
    /// The person whose view it is.
    pub owner: String,
}

/// A workspace's shared desktop as a door that decides its writes follows it.
pub struct DesktopGuard {
    doc: AutoCommit,
}

/// Why a write of the shared desktop is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DesktopRefusal {
    /// It does what its writer may not to something someone else made.
    #[error(
        "{writer} may not {verb} {id} on the shared desktop, which {owner} made",
        verb = act.act.verb(),
        id = act.id,
        owner = act.owner.as_deref().unwrap_or("nobody")
    )]
    NotTheirs { writer: String, act: Touched },
    /// It moves another person's device focus ([`super::continuation`]).
    #[error("{writer} may not move another person's focus, which is on window {window}")]
    NotTheirFocus { writer: String, window: TabId },
    /// It binds a window to another record than the one it showed.
    #[error(
        "{writer} may not bind window {window} to another record: it shows its record for good"
    )]
    Rebound { writer: String, window: TabId },
    /// It places a person's own window ([`crate::personal`]) on the workspace's desktop.
    #[error(
        "{writer} may not place window {window} on the shared desktop: it is a person's own and stays on their desk"
    )]
    Personal { writer: String, window: TabId },
    /// It is not a desktop, or not one that can be read.
    #[error("not a shared desktop: {0}")]
    Unreadable(String),
}

/// Who wrote a write, and what the workspace lets them do.
pub struct Writer<'a> {
    /// The writer's person id.
    pub person: &'a str,
    /// The workspace's current members, when known: what someone no longer
    /// among them made is released ([`crate::ownership::released`]).
    pub members: Option<&'a BTreeSet<String>>,
    /// Whether the workspace lets the writer do `act` to what someone else made.
    pub allows: &'a dyn Fn(&Touched) -> bool,
}

/// Decide the changes `doc` holds beyond `before`, each by what it did to
/// the desktop its author saw.
///
/// # Errors
/// The first act of the first change that `writer` may not do.
pub fn judge(
    doc: &AutoCommit,
    before: &[ChangeHash],
    writer: &Writer<'_>,
) -> Result<(), DesktopRefusal> {
    for change in doc.committed_document().get_changes(before) {
        let seen_kept = super::read_as_kept(doc, Some(change.deps())).unwrap_or_default();
        let left_kept = super::read_as_kept(doc, Some(&[change.hash()])).unwrap_or_default();
        // One that was there before (an older build's) is no new placement: the write may be
        // the one taking it out, or about something else.
        let was_personal = crate::personal::personal_tabs(&seen_kept.tree);
        if let Some(window) = crate::personal::personal_tabs(&left_kept.tree)
            .into_iter()
            .find(|window| !was_personal.contains(window))
        {
            return Err(DesktopRefusal::Personal {
                writer: writer.person.to_owned(),
                window,
            });
        }
        let (mut seen, mut left) = (seen_kept, left_kept);
        crate::personal::leave_out(&mut seen);
        crate::personal::leave_out(&mut left);
        if let Some(window) = rebound_windows(&seen, &left).first() {
            return Err(DesktopRefusal::Rebound {
                writer: writer.person.to_owned(),
                window: *window,
            });
        }
        if let Some(act) = foreign_acts(&seen, &left, writer.person, writer.members)
            .into_iter()
            .chain(claimed(&seen, &left, writer.person))
            .find(|act| !(writer.allows)(act))
        {
            return Err(DesktopRefusal::NotTheirs {
                writer: writer.person.to_owned(),
                act,
            });
        }
        if let Some(window) = super::continuation::foreign_changes(
            doc,
            change.deps(),
            &[change.hash()],
            writer.person,
        ) {
            return Err(DesktopRefusal::NotTheirFocus {
                writer: writer.person.to_owned(),
                window,
            });
        }
    }
    Ok(())
}

/// Take `mutation`, a write by `writer`, into `doc` when [`judge`] admits
/// it, and leave `doc` as it was when it does not. Returns whether `doc`
/// changed.
///
/// # Errors
/// Why the write is refused.
pub fn admit(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
    writer: &Writer<'_>,
) -> Result<bool, DesktopRefusal> {
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    let mut next = doc.clone();
    let before = next.get_heads();
    next.load_incoming(bytes)
        .map_err(|error| DesktopRefusal::Unreadable(error.to_string()))?;
    judge(&next, &before, writer)?;
    let changed = next.get_heads() != before;
    *doc = next;
    Ok(changed)
}

/// What [`admit_complete`] did with a write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// Taken in; whether it changed the desktop.
    Merged(bool),
    /// It builds on changes the desktop does not hold yet: nothing of it
    /// is taken in until they are here.
    Waiting,
}

/// [`admit`] for a holder that takes a write only once it holds every
/// change the write builds on. A write that arrives before them waits:
/// nothing of it applies, and its holder offers it again later. So each of
/// its changes is decided as a write of the author who signed it, never
/// taken in later with someone else's.
///
/// # Errors
/// Why the write is refused.
pub fn admit_complete(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
    writer: &Writer<'_>,
) -> Result<Admitted, DesktopRefusal> {
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    let mut next = doc.clone();
    let before = next.get_heads();
    next.load_incoming(bytes)
        .map_err(|error| DesktopRefusal::Unreadable(error.to_string()))?;
    if !next.get_missing_deps(&[]).is_empty() {
        return Ok(Admitted::Waiting);
    }
    judge(&next, &before, writer)?;
    let changed = next.get_heads() != before;
    *doc = next;
    Ok(Admitted::Merged(changed))
}

impl DesktopGuard {
    #[must_use]
    pub const fn new(doc: AutoCommit) -> Self {
        Self { doc }
    }

    /// The desktop as the door holds it.
    #[must_use]
    pub const fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// Take in changes the door's own device made, without judging them.
    pub fn absorb(&mut self, other: &mut AutoCommit) {
        let _ = self.doc.merge_incoming(other);
    }

    /// Take in `mutation`, a write the door's own device made, without judging it: the
    /// writes the door decides later build on it, and would otherwise be decided against
    /// a desktop that lacks it.
    ///
    /// # Errors
    /// The write is not one of the desktop's documents.
    pub fn absorb_write(&mut self, mutation: &DocumentMutation) -> Result<(), DesktopRefusal> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        self.doc
            .load_incoming(bytes)
            .map(drop)
            .map_err(|error| DesktopRefusal::Unreadable(error.to_string()))
    }

    /// Decide `mutation`, a write by `writer` ([`admit`]): taken in when it
    /// is admitted, and the desktop left as it was when it is refused.
    ///
    /// # Errors
    /// Why the write is refused.
    pub fn decide(
        &mut self,
        mutation: &DocumentMutation,
        writer: &Writer<'_>,
    ) -> Result<(), DesktopRefusal> {
        admit(&mut self.doc, mutation, writer).map(drop)
    }
}

#[cfg(test)]
mod tests;
