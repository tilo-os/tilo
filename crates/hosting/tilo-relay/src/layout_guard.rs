//! The relay's check of each write to a room's layout document: one writer
//! per entry, and rearrangement of someone else's windows as a retained
//! request rather than a write into their namespace.
//!
//! The layout document carries every strip, window group and window as an
//! entry of its own in the room's `contents` map
//! ([`tilo_desktop_model::room_layout`]), each minted under the id namespace
//! of the device that published it. A change may only touch entries
//! carrying its writer's namespace (and only mark applied requests of that
//! namespace), so two devices' concurrent edits merge and nobody rewrites a
//! colleague's window. That replaced an earlier rule that read each change
//! as before/after desktops and judged what it did to other people's
//! entries: a moderator now asks ([`LayoutRequest`]) instead of writing.
//!
//! A request is room state: the relay keeps it while the window's owner is
//! away. It is judged where it enters — its id must be its own content, its
//! asker must be its writer, and what it asks of an entry that is not its
//! asker's must be an act `allows` grants them on that entry — and the owner
//! applies it once, recording that in its own namespace.

use automerge::{AutoCommit, ChangeHash};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::commands::BoundCommand;
use tilo_desktop_model::ids::{namespace_of_device, peer_salt_of, room_strips_salt};
use tilo_desktop_model::ownership::{Touched, commanded_act};
use tilo_desktop_model::projection::{
    LayoutRequest, layout_authors_at, layout_digest_at, layout_marks_at, layout_requests_at,
};
use tilo_desktop_model::room_layout;

/// A room's layout document as the relay follows it.
pub(crate) struct LayoutGuard {
    doc: AutoCommit,
}

/// Why a write of the room's layout is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LayoutRefusal {
    /// It writes, closes or renames an entry a device that is not its
    /// writer made.
    #[error("{writer} may not write {entry} on the room's layout, minted by another device")]
    NotTheirs { writer: String, entry: u64 },
    /// It marks applied a request in a namespace that is not its writer's.
    #[error("{writer} may not record {mark} as applied")]
    NotItsMark { writer: String, mark: String },
    /// It records a request that is not its asker's, or asks something its
    /// asker may not do to that window.
    #[error("{writer} may not ask {request} on the room's layout: {why}")]
    Request {
        writer: String,
        request: String,
        why: String,
    },
    /// It writes the room's layout in a shape this build does not read (a
    /// document that declares no room, or a top-level key that is no part of
    /// the layout). A page from before the one layout schema writes these,
    /// and its windows are its own until it updates.
    #[error("{writer} writes no room layout: {what}")]
    NotALayout { writer: String, what: String },
    /// It is not a layout document, or not one the relay can read.
    #[error("not a room layout: {0}")]
    Unreadable(String),
}

impl LayoutGuard {
    pub(crate) const fn new(doc: AutoCommit) -> Self {
        Self { doc }
    }

    /// Take in changes the relay's own device made, without judging them.
    #[cfg(feature = "agents")]
    pub(crate) fn absorb(&mut self, other: &mut AutoCommit) {
        let _ = self.doc.merge_incoming(other);
    }

    /// Decide `mutation`, a write by the device `writer` (its key, hex) for
    /// the person `person` in `room`: every entry it changes must be minted
    /// in `writer`'s namespace (never the room's own, where its operator's
    /// strips live), every mark it records in its own, and every request it
    /// makes its own, with `allows` deciding what it may ask of a window
    /// someone else made. Takes the write in when it is admitted, and leaves
    /// the layout as it was when it is refused.
    pub(crate) fn decide(
        &mut self,
        mutation: &DocumentMutation,
        room: &str,
        writer: &str,
        person: &str,
        allows: &dyn Fn(&Touched) -> bool,
    ) -> Result<(), LayoutRefusal> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        let mut next = self.doc.clone();
        let before = next.get_heads();
        next.load_incoming(bytes)
            .map_err(|error| LayoutRefusal::Unreadable(error.to_string()))?;
        let write = Write {
            writer,
            person,
            salt: namespace_of_device(writer),
            room: room_strips_salt(room),
        };
        if room_layout::declared_room(&next, None).is_none() {
            return Err(LayoutRefusal::NotALayout {
                writer: writer.to_owned(),
                what: "it declares no room".into(),
            });
        }
        for change in next.get_changes(&before) {
            let deps = change.deps().to_vec();
            let added = added_room_keys(&next, &deps, change.hash());
            if let Some(key) = added
                .iter()
                .find(|key| !LAYOUT_KEYS.contains(&key.as_str()))
            {
                return Err(LayoutRefusal::NotALayout {
                    writer: writer.to_owned(),
                    what: format!("it writes {key}, which is not the layout's"),
                });
            }
            judge(&next, &write, &deps, change.hash(), allows)?;
        }
        self.doc = next;
        Ok(())
    }
}

/// The top-level keys a room's layout document holds: its windows, the
/// requests for them, the marks their owners keep, and the room it names.
/// A write that adds any other key is not writing the layout this build
/// reads.
const LAYOUT_KEYS: [&str; 5] = [
    tilo_desktop_model::projection::ROOM_CONTENTS_KEY,
    tilo_desktop_model::projection::ROOM_LAYOUT_REQUESTS_KEY,
    tilo_desktop_model::projection::ROOM_LAYOUT_APPLIED_KEY,
    tilo_desktop_model::projection::ROOM_ID_KEY,
    tilo_desktop_model::projection::ROOM_NAME_KEY,
];

/// The top-level keys `doc` holds one change after `deps` that it did not
/// hold before.
fn added_room_keys(doc: &AutoCommit, deps: &[ChangeHash], at: ChangeHash) -> Vec<String> {
    let before: std::collections::BTreeSet<String> = doc.keys_at(&automerge::ROOT, deps).collect();
    doc.keys_at(&automerge::ROOT, &[at])
        .filter(|key| !before.contains(key))
        .collect()
}

/// One admitted batch's writer: its device key (hex), the person it writes
/// for, and the layout namespace its windows are minted in.
#[derive(Clone, Copy)]
struct Write<'a> {
    writer: &'a str,
    person: &'a str,
    salt: u16,
    /// The room's own namespace, where its operator's strips live: nobody
    /// but the room's host writes there.
    room: u16,
}

/// Whether one change of an admitted batch, from `deps` to `at`, stays
/// inside its writer's namespace, and whether the requests it makes are its
/// own and allowed.
fn judge(
    next: &AutoCommit,
    write: &Write<'_>,
    deps: &[ChangeHash],
    at: ChangeHash,
    allows: &dyn Fn(&Touched) -> bool,
) -> Result<(), LayoutRefusal> {
    let before_entries = layout_digest_at(next, Some(deps));
    let after_entries = layout_digest_at(next, Some(&[at]));
    let not_theirs = |entry: u64| LayoutRefusal::NotTheirs {
        writer: write.writer.to_owned(),
        entry,
    };
    let theirs = |id: u64| peer_salt_of(id) == write.salt && write.salt != write.room;
    for (id, digest) in &after_entries {
        if before_entries.get(id) != Some(digest) && !theirs(*id) {
            return Err(not_theirs(*id));
        }
    }
    for id in before_entries.keys() {
        if !after_entries.contains_key(id) && !theirs(*id) {
            return Err(not_theirs(*id));
        }
    }

    let prefix = format!("{:04x}/", write.salt);
    let marks = layout_marks_at(next, Some(deps));
    for mark in layout_marks_at(next, Some(&[at])) {
        if !marks.contains(&mark) && !mark.starts_with(&prefix) {
            return Err(LayoutRefusal::NotItsMark {
                writer: write.writer.to_owned(),
                mark,
            });
        }
    }

    let seen = layout_requests_at(next, Some(deps));
    let authors = layout_authors_at(next, Some(&[at]));
    for (key, request) in layout_requests_at(next, Some(&[at])) {
        if seen.get(&key) == Some(&request) {
            continue;
        }
        if let Some(why) = request_refusal(&key, &request, write.person) {
            return Err(LayoutRefusal::Request {
                writer: write.writer.to_owned(),
                request: key,
                why,
            });
        }
        let Some(act) = request_act(&request).filter(|act| peer_salt_of(act.id) != write.salt)
        else {
            continue;
        };
        let touched = Touched {
            owner: authors.get(&act.id).cloned(),
            ..act
        };
        if !allows(&touched) {
            return Err(LayoutRefusal::Request {
                writer: write.writer.to_owned(),
                request: key,
                why: format!(
                    "it may not {} {}, which {} made",
                    touched.act.verb(),
                    touched.id,
                    touched.owner.as_deref().unwrap_or("nobody")
                ),
            });
        }
    }
    Ok(())
}

/// Why `request`, filed under `key`, is not one `person` may make.
fn request_refusal(key: &str, request: &LayoutRequest, person: &str) -> Option<String> {
    if request.id != key {
        return Some("it is filed under another request's id".into());
    }
    if request.id != LayoutRequest::id(&request.from, request.to_salt, request.at, &request.command)
    {
        return Some("its id is not its own content".into());
    }
    if request.from != person {
        return Some(format!("it is asked in the name of {}", request.from));
    }
    if request.to_salt == 0 {
        return Some("it names no owner".into());
    }
    None
}

/// The act `request` asks of an entry, when the owner's own device is asked
/// to apply a command whose JSON this build reads.
fn request_act(request: &LayoutRequest) -> Option<Touched> {
    let bound: BoundCommand = serde_json::from_slice(&request.command).ok()?;
    let (id, act) = commanded_act(&bound.command)?;
    Some(Touched {
        id,
        act,
        owner: None,
    })
}

#[cfg(test)]
mod tests;
