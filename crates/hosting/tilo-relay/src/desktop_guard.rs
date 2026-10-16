//! The relay's check of each write to a room's shared desktop: nobody
//! closes, moves or renames a strip or window someone else made unless the
//! room lets them moderate it ([`tilo_desktop_model::ownership`]).
//!
//! The desktop record is one Automerge document whose `desktop` value is
//! the shared JSON, written whole by every change, so the check reads what
//! a write did from the values themselves, two ways:
//!
//! - **What each change did**, from the desktop its author saw (the value at
//!   the change's dependencies) to the one it wrote. This is the author's
//!   own act, whatever else happened meanwhile: someone who never saw a
//!   window cannot have closed it.
//! - **What the write did to the room**, from the desktop before it to the
//!   merged one after. Concurrent writes resolve last-writer-wins, so a
//!   write made over an older desktop replaces what changed since. A strip
//!   or window that only just appeared or moved is exempt for
//!   [`SETTLE`], since an ordinary race against its opener does exactly
//!   that; one that has stayed put longer is not, so an old desktop cannot
//!   be replayed over everyone's windows.
//!
//! A write is admitted when every act on what its writer did not make is
//! one the room lets them do ([`tilo_desktop_model::ownership::act_node`]).

use std::collections::HashMap;
use std::time::Duration;

use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _, ScalarValue, Value};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::ownership::{Act, SharedDesktop, Touched, foreign_acts};
use tilo_desktop_model::{StripId, WorkspaceId};

/// How long a strip or window that just appeared, moved to another group or
/// strip, or was renamed is exempt from what a write does to the room
/// (not from what its author did): longer than a write takes to reach every
/// member, so a race against its opener is never refused.
pub(crate) const SETTLE: Duration = Duration::from_secs(10);

/// The most changes of one write judged one by one. A write carrying more
/// (a member's first snapshot of a desktop this relay saw little of) is
/// judged by what it does to the room alone.
const MAX_JUDGED_CHANGES: usize = 64;

/// The key of the desktop's value in its record.
const DESKTOP_KEY: &str = "desktop";

/// Where a strip or window is, to tell when it last moved.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Window(WorkspaceId, StripId, u64),
    Strip(WorkspaceId, String),
}

/// A room's shared desktop as the relay follows it, and when each strip and
/// window last appeared or moved.
pub(crate) struct DesktopGuard {
    doc: AutoCommit,
    /// By strip or tab id: where it is, and since when (Unix milliseconds).
    placed: HashMap<u64, (Place, u64)>,
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

fn text_at(doc: &AutoCommit, heads: Option<&[ChangeHash]>) -> Option<String> {
    let found = match heads {
        Some(heads) => doc.get_at(ROOT, DESKTOP_KEY, heads),
        None => doc.get(ROOT, DESKTOP_KEY),
    };
    match found.ok()?? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The desktop `doc` holds at `heads` (now, for `None`): `Ok(None)` when it
/// holds none there.
fn desktop_at(
    doc: &AutoCommit,
    heads: Option<&[ChangeHash]>,
) -> Result<Option<SharedDesktop>, String> {
    text_at(doc, heads)
        .map(|text| SharedDesktop::parse(&text))
        .transpose()
}

fn places(desktop: &SharedDesktop) -> HashMap<u64, Place> {
    let mut places = HashMap::new();
    for workspace in &desktop.tree.workspaces {
        for strip in &workspace.strips {
            places.insert(strip.id.0, Place::Strip(workspace.id, strip.name.clone()));
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    places.insert(tab.id, Place::Window(workspace.id, strip.id, group.id));
                }
            }
        }
    }
    places
}

impl DesktopGuard {
    /// Follow the desktop `doc` holds, taking everything on it as settled.
    pub(crate) fn new(doc: AutoCommit) -> Self {
        let mut guard = Self {
            doc,
            placed: HashMap::new(),
        };
        guard.note_places(0);
        guard
    }

    #[cfg(feature = "agents")]
    /// Take in changes the relay's own device made (or that reached the
    /// room another way), without judging them.
    pub(crate) fn absorb(&mut self, other: &mut AutoCommit, now_ms: u64) {
        if self.doc.merge_incoming(other).is_ok() {
            self.note_places(now_ms);
        }
    }

    fn note_places(&mut self, now_ms: u64) {
        let Ok(Some(desktop)) = desktop_at(&self.doc, None) else {
            return;
        };
        let now = places(&desktop);
        self.placed.retain(|id, _| now.contains_key(id));
        for (id, place) in now {
            match self.placed.get(&id) {
                Some((held, _)) if *held == place => {}
                _ => {
                    self.placed.insert(id, (place, now_ms));
                }
            }
        }
    }

    fn settled(&self, id: u64, now_ms: u64) -> bool {
        let settle = u64::try_from(SETTLE.as_millis()).unwrap_or(u64::MAX);
        self.placed
            .get(&id)
            .is_none_or(|(_, since)| since.saturating_add(settle) <= now_ms)
    }

    /// Decide `mutation`, a write by `writer` (a person id), at `now_ms`:
    /// each act on what `writer` did not make must be one `allows` lets
    /// them do. Takes the write in when it is admitted, and leaves the
    /// desktop as it was when it is refused.
    ///
    /// # Errors
    /// The first act `writer` may not do, or a write that is not a desktop.
    pub(crate) fn decide(
        &mut self,
        mutation: &DocumentMutation,
        writer: &str,
        now_ms: u64,
        allows: &dyn Fn(&Touched) -> bool,
    ) -> Result<(), DesktopRefusal> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        let refuse = |act: Touched| DesktopRefusal::NotTheirs {
            writer: writer.to_owned(),
            act,
        };
        let mut next = self.doc.clone();
        let before = next.get_heads();
        next.load_incoming(bytes)
            .map_err(|error| DesktopRefusal::Unreadable(error.to_string()))?;
        let current = desktop_at(&next, Some(&before)).unwrap_or_default();
        let merged = desktop_at(&next, None).map_err(DesktopRefusal::Unreadable)?;
        let changes = next.get_changes(&before);
        if changes.len() <= MAX_JUDGED_CHANGES {
            for change in &changes {
                let seen = desktop_at(&next, Some(change.deps())).unwrap_or_default();
                let wrote = desktop_at(&next, Some(&[change.hash()]))
                    .map_err(DesktopRefusal::Unreadable)?;
                let (Some(seen), wrote) = (seen, wrote) else {
                    continue;
                };
                let wrote = wrote.unwrap_or_default();
                if let Some(act) = foreign_acts(&seen, &wrote, writer)
                    .into_iter()
                    .find(|act| !allows(act))
                {
                    return Err(refuse(act));
                }
            }
        }
        if let Some(current) = current {
            let merged = merged.unwrap_or_default();
            if let Some(act) = foreign_acts(&current, &merged, writer)
                .into_iter()
                .filter(|act| act.act != Act::Reorder)
                .filter(|act| act.act == Act::Attribute || self.settled(act.id, now_ms))
                .find(|act| !allows(act))
            {
                return Err(refuse(act));
            }
        }
        self.doc = next;
        self.note_places(now_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
