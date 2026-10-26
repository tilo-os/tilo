//! A room's layout as its room document carries it: one workspace per
//! document, every strip, window group and window in the room's `contents`
//! map, each entry written by the device whose id namespace it carries.
//!
//! This is the browser's, the relay's and an agent's way into the one room
//! schema ([`crate::projection`]) the native shell already writes: a page
//! publishes the windows it opened under its own salt
//! ([`write`]), reads everyone's back at any heads ([`read`]) and folds the
//! result into its own tree, and an agent opens a view the same way. The
//! document holds one workspace — the room's — so a page with several rooms
//! keeps one document per room, and nothing of another room's windows can
//! land in this one.
//!
//! What only one device decides for itself (which window is focused,
//! widths, maximization) is never written: [`read`] hands back the room's
//! topology, and the reader keeps its own presentation.
//!
//! Requests to rearrange a window another device owns are room state, not a
//! write into someone else's namespace ([`request`]): the host retains each
//! one, the owner applies it once when it next holds the room, and records
//! which it applied in its own namespace ([`claim`]), so a restart neither
//! drops nor repeats one and a person can arrange the windows of their other
//! devices, offline ones included.

use std::collections::BTreeSet;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId, ROOT, ReadDoc as _, ScalarValue, Value};

use crate::apps::{bind_tab_record, record_routes_at};
use crate::ids::{RoomId, WorkspaceId, peer_salt_of};
use crate::ownership::SharedDesktop;
use crate::projection::{
    EmptyStrips, LayoutRequest, ROOM_ID_KEY, ROOM_LAYOUT_APPLIED_KEY, ROOM_LAYOUT_REQUESTS_KEY,
    ROOM_NAME_KEY, contents_at, layout_applied_key, layout_authors_at,
};

/// The workspace a room document describes: the room it declares, its name
/// and its windows. `None` when the document describes no room.
#[must_use]
pub fn read(doc: &impl ReadDoc, heads: Option<&[ChangeHash]>) -> Option<SharedDesktop> {
    let room = declared_room(doc, heads)?;
    let name = text(doc, heads, &ROOT, ROOM_NAME_KEY).unwrap_or_default();
    let workspace =
        crate::workspace::Workspace::in_room(WorkspaceId::of_room(room), room, name.as_str());
    let strips = contents_at(doc, heads);
    let mut workspace_tree = crate::tree::DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..crate::tree::DesktopTree::default()
    };
    workspace_tree.workspaces[0].strips = strips
        .into_iter()
        .map(crate::projection::hydrate_strip)
        .collect();
    let mut records: Vec<(crate::ids::TabId, String)> =
        record_routes_at(doc, heads).into_iter().collect();
    records.sort_unstable();
    Some(SharedDesktop {
        tree: workspace_tree,
        records,
        authors: layout_authors_at(doc, heads),
    })
}

/// The room `doc` declares it describes, at `heads` (`None`: now). `None`
/// when it names no room: a document of an older shape, which is refused and
/// fetched again rather than read as an empty layout.
#[must_use]
pub fn declared_room(doc: &impl ReadDoc, heads: Option<&[ChangeHash]>) -> Option<RoomId> {
    RoomId::parse(&text(doc, heads, &ROOT, ROOM_ID_KEY)?)
}

/// Write `layout`'s workspace into `doc` as `salt`'s entries, naming
/// `author` (a person id) on each, and bind each window this machine owns to
/// the record it shows. Only entries in `salt`'s namespace are written or
/// removed; every other device's entries are left exactly as they are, and
/// an unchanged layout writes nothing. Returns whether it changed anything.
///
/// # Errors
///
/// The document's error when a map this projection needs cannot be created.
pub fn write(
    doc: &mut AutoCommit,
    layout: &SharedDesktop,
    salt: u16,
    author: Option<&str>,
) -> Result<bool, automerge::AutomergeError> {
    let Some(workspace) = layout.tree.workspaces.first() else {
        return Ok(false);
    };
    if salt == 0 {
        return Ok(false);
    }
    let before = doc.get_heads();
    declare(doc, workspace.room(), &workspace.name)?;
    crate::projection::project_owned_contents_as(
        doc,
        &workspace.strips,
        salt,
        None,
        EmptyStrips::Skip,
        author,
    )?;
    for strip in &workspace.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if peer_salt_of(tab.id) != salt {
                    continue;
                }
                let Some((_, record)) = layout.records.iter().find(|(bound, _)| *bound == tab.id)
                else {
                    continue;
                };
                // A binding is permanent: the first record a window showed is
                // the one the others mount. A different one is refused, and
                // the window keeps showing the record it was opened with.
                let _ = bind_tab_record(doc, strip.id, group.id, tab.id, record);
            }
        }
    }
    Ok(doc.get_heads() != before)
}

/// Name the room and its workspace in `doc`, when it does not already.
fn declare(
    doc: &mut AutoCommit,
    room: RoomId,
    name: &str,
) -> Result<(), automerge::AutomergeError> {
    let set = |doc: &mut AutoCommit,
               key: &str,
               value: &str|
     -> Result<(), automerge::AutomergeError> {
        let unchanged = matches!(
            doc.get(&ROOT, key).ok().flatten(),
            Some((Value::Scalar(v), _)) if matches!(v.as_ref(), ScalarValue::Str(t) if t.as_str() == value)
        );
        if !unchanged {
            doc.put(&ROOT, key, value)?;
        }
        Ok(())
    };
    set(doc, ROOM_ID_KEY, &room.to_string())?;
    set(doc, ROOM_NAME_KEY, name)
}

fn text(doc: &AutoCommit, heads: Option<&[ChangeHash]>, obj: &ObjId, key: &str) -> Option<String> {
    let held = match heads {
        Some(heads) => doc.get_at(obj, key, heads),
        None => doc.get(obj, key),
    };
    match held.ok().flatten()? {
        (Value::Scalar(value), _) => match value.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Offer `request` to the room: it is kept as room state (one per id) for
/// the window's owner to apply, whether the owner is here or not. Returns
/// whether the document changed — asking the same thing twice changes
/// nothing.
///
/// # Errors
///
/// The document's error when the requests map cannot be created or written.
pub fn request(
    doc: &mut AutoCommit,
    request: &LayoutRequest,
) -> Result<bool, automerge::AutomergeError> {
    let requests = match doc.get(&ROOT, ROOM_LAYOUT_REQUESTS_KEY).ok().flatten() {
        Some((_, map)) => map,
        None => doc.put_object(&ROOT, ROOM_LAYOUT_REQUESTS_KEY, automerge::ObjType::Map)?,
    };
    let blob = match serde_json::to_vec(request) {
        Ok(blob) => blob,
        Err(_) => return Ok(false),
    };
    let unchanged = matches!(
        doc.get(&requests, request.id.as_str()).ok().flatten(),
        Some((Value::Scalar(value), _))
            if matches!(value.as_ref(), ScalarValue::Bytes(current) if current == &blob)
    );
    if unchanged {
        return Ok(false);
    }
    doc.put(&requests, request.id.as_str(), ScalarValue::Bytes(blob))?;
    Ok(true)
}

/// Every request the room retains, in the order it accepted them.
#[must_use]
pub fn requests(doc: &impl ReadDoc) -> Vec<LayoutRequest> {
    let Some((_, requests)) = doc.get(&ROOT, ROOM_LAYOUT_REQUESTS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut out: Vec<LayoutRequest> = doc
        .keys(&requests)
        .filter_map(|id| match doc.get(&requests, id.as_str()).ok().flatten() {
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(blob) => serde_json::from_slice(blob).ok(),
                _ => None,
            },
            _ => None,
        })
        .collect();
    out.sort_by(|left, right| left.at.cmp(&right.at).then(left.id.cmp(&right.id)));
    out
}

/// The requests `salt`'s owner still has to apply: those addressed to it
/// and not yet recorded as applied, whether a colleague or another of the
/// owner's own devices asked.
#[must_use]
pub fn pending(doc: &impl ReadDoc, salt: u16) -> Vec<LayoutRequest> {
    if salt == 0 {
        return Vec::new();
    }
    let applied = applied(doc, salt);
    requests(doc)
        .into_iter()
        .filter(|request| request.to_salt == salt && !applied.contains(&request.id))
        .collect()
}

/// The requests `salt`'s owner recorded as applied, by id.
#[must_use]
pub fn applied(doc: &impl ReadDoc, salt: u16) -> BTreeSet<String> {
    let Some((_, marks)) = doc.get(&ROOT, ROOM_LAYOUT_APPLIED_KEY).ok().flatten() else {
        return BTreeSet::new();
    };
    let prefix = format!("{salt:04x}/");
    let mut out = BTreeSet::new();
    for key in doc.keys(&marks) {
        if let Some(id) = key.strip_prefix(prefix.as_str()) {
            out.insert(id.to_owned());
        }
    }
    out
}

/// Claim `request` for `salt`'s owner before applying it: `true` for the
/// first claim, `false` once it is recorded. The mark is written in the
/// owner's own namespace under the same change that writes the result, so a
/// restart applies a retained request once and never twice.
///
/// # Errors
///
/// The document's error when the applied map cannot be created or written.
pub fn claim(
    doc: &mut AutoCommit,
    salt: u16,
    request: &LayoutRequest,
) -> Result<bool, automerge::AutomergeError> {
    if salt == 0 || request.to_salt != salt {
        return Ok(false);
    }
    let key = layout_applied_key(salt, &request.id);
    let marks = match doc.get(&ROOT, ROOM_LAYOUT_APPLIED_KEY).ok().flatten() {
        Some((_, map)) => map,
        None => doc.put_object(&ROOT, ROOM_LAYOUT_APPLIED_KEY, automerge::ObjType::Map)?,
    };
    if doc.get(&marks, key.as_str()).ok().flatten().is_some() {
        return Ok(false);
    }
    doc.put(&marks, key.as_str(), 1_u64)?;
    Ok(true)
}
