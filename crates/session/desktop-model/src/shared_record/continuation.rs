//! What a person's devices need to continue each other's work, kept in the
//! layout record beside the windows it is about.
//!
//! Every workspace is on every one of a person's devices, and each device
//! writes its own part of the layout. So "what my laptop has open here, and
//! where it left off" is read from the laptop's part of the record, which
//! every other device of the person already holds. Two things are kept:
//!
//! - A window's **continuation**: a small, path-free note of how to resume
//!   it (the app's own resume state, the record it shows), written by the
//!   device that last used it. Only the person who made a window writes its
//!   continuation.
//! - Each device's **focus**: the window it is on in this workspace, under
//!   the device's id namespace (its salt), naming the person at the device.
//!   Only that person writes or removes it.
//!
//! The text of a continuation is the writer's own encoding; this record only
//! keeps it and decides who may change it ([`foreign_changes`]).
//!
//! Shape, beside the layout ([`super`]):
//!
//! ```text
//! windows: { <id>: { …, continuation? } }
//! focus:   { <salt>: { window: <id>, author } }
//! ```

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, ROOT, ReadDoc};

use super::{AUTHOR, At, WINDOWS};
use crate::ids::TabId;

const CONTINUATION: &str = "continuation";
const FOCUS: &str = "focus";
const WINDOW: &str = "window";

/// A device's focus as the record holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    /// The window the device is on.
    pub window: TabId,
    /// The person at the device.
    pub author: String,
}

/// The continuation of every window that has one, by window.
#[must_use]
pub fn windows(doc: &impl ReadDoc) -> BTreeMap<TabId, String> {
    windows_at(&At { doc, heads: None })
}

fn windows_at<D: ReadDoc>(at: &At<'_, D>) -> BTreeMap<TabId, String> {
    at.entries(WINDOWS)
        .into_iter()
        .filter_map(|(id, entry)| Some((id, at.text(&entry, CONTINUATION)?)))
        .collect()
}

/// Every device's focus, by the device's salt.
#[must_use]
pub fn focus(doc: &impl ReadDoc) -> BTreeMap<u16, Focus> {
    focus_at(&At { doc, heads: None })
}

fn focus_at<D: ReadDoc>(at: &At<'_, D>) -> BTreeMap<u16, Focus> {
    let mut out = BTreeMap::new();
    for container in at.containers(FOCUS) {
        for key in at.keys(&container) {
            let Ok(salt) = key.parse::<u16>() else {
                continue;
            };
            if out.contains_key(&salt) {
                continue;
            }
            let Some((automerge::Value::Object(ObjType::Map), entry)) = at.get(&container, &key)
            else {
                continue;
            };
            let (Some(window), Some(author)) = (
                at.text(&entry, WINDOW).and_then(|id| id.parse().ok()),
                at.text(&entry, AUTHOR),
            ) else {
                continue;
            };
            out.insert(salt, Focus { window, author });
        }
    }
    out
}

/// Set (`Some`) or remove (`None`) the continuation of `window`. A window
/// the record does not hold has none. Returns whether it changed the record.
///
/// # Errors
/// The document refused a write.
pub fn write_window(
    doc: &mut AutoCommit,
    window: TabId,
    continuation: Option<&str>,
) -> Result<bool, automerge::AutomergeError> {
    let at = At {
        doc: &*doc,
        heads: None,
    };
    let Some(entry) = at.entries(WINDOWS).remove(&window) else {
        return Ok(false);
    };
    let held = at.text(&entry, CONTINUATION);
    match continuation {
        Some(text) if held.as_deref() != Some(text) => {
            doc.put(&entry, CONTINUATION, text)?;
            Ok(true)
        }
        None if held.is_some() => {
            doc.delete(&entry, CONTINUATION)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Set (`Some`) or remove (`None`) the focus of the device whose salt is
/// `salt`. Returns whether it changed the record.
///
/// # Errors
/// The document refused a write.
pub fn write_focus(
    doc: &mut AutoCommit,
    salt: u16,
    focus: Option<&Focus>,
) -> Result<bool, automerge::AutomergeError> {
    let key = salt.to_string();
    let at = At {
        doc: &*doc,
        heads: None,
    };
    let containers = at.containers(FOCUS);
    if focus_at(&at).get(&salt) == focus {
        return Ok(false);
    }
    let Some(focus) = focus else {
        for container in &containers {
            if doc.get(container, key.as_str())?.is_some() {
                doc.delete(container, key.as_str())?;
            }
        }
        return Ok(true);
    };
    let container = match containers.first() {
        Some(container) => container.clone(),
        None => doc.put_object(ROOT, FOCUS, ObjType::Map)?,
    };
    let entry = doc.put_object(&container, key.as_str(), ObjType::Map)?;
    doc.put(&entry, WINDOW, focus.window.to_string())?;
    doc.put(&entry, AUTHOR, focus.author.as_str())?;
    Ok(true)
}

/// What a change (the record at `left`, written on the record at `seen`)
/// did to continuations or focus that are not `writer`'s: a continuation of
/// a window someone else made, or a focus naming someone else, before or
/// after. The windows' makers are read from `seen` and `left`.
pub(super) fn foreign_changes(
    doc: &AutoCommit,
    seen: &[ChangeHash],
    left: &[ChangeHash],
    authors: &crate::ownership::Authors,
    writer: &str,
) -> Option<TabId> {
    let before = At {
        doc,
        heads: Some(seen),
    };
    let after = At {
        doc,
        heads: Some(left),
    };
    let (was, now) = (windows_at(&before), windows_at(&after));
    let foreign = |id: &TabId| authors.get(id).is_some_and(|author| author != writer);
    let changed_window = was
        .keys()
        .chain(now.keys())
        .find(|id| was.get(*id) != now.get(*id) && foreign(id))
        .copied();
    if changed_window.is_some() {
        return changed_window;
    }
    let (was, now) = (focus_at(&before), focus_at(&after));
    was.iter()
        .chain(now.iter())
        .find(|(salt, focus)| was.get(*salt) != now.get(*salt) && focus.author != writer)
        .map(|(_, focus)| focus.window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{StripId, WorkspaceId};
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::ownership::SharedDesktop;
    use crate::tree::DesktopTree;
    use crate::width::WindowWidth;
    use crate::workspace::Workspace;

    fn desktop(windows: &[(u64, &str)]) -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
        let mut strip = Strip::new(StripId(10), "Lounge");
        for (id, _) in windows {
            strip.window_groups.push(WindowGroup {
                id: id + 100,
                tabs: vec![Tab {
                    id: *id,
                    title: format!("tab {id}"),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".into(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                    app_title: None,
                }],
                active_tab: *id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        workspace.strips.push(strip);
        SharedDesktop {
            tree: DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            authors: windows
                .iter()
                .map(|(id, who)| (*id, (*who).to_owned()))
                .collect(),
            records: Vec::new(),
        }
    }

    fn judged(doc: &AutoCommit, before: &[ChangeHash], person: &str) -> bool {
        super::super::guard::judge(
            doc,
            before,
            &super::super::guard::Writer {
                person,
                members: None,
                allows: &|_| true,
            },
        )
        .is_ok()
    }

    #[test]
    fn continuations_and_focus_survive_layout_writes_and_stay_their_persons() {
        let mut doc = AutoCommit::new();
        super::super::write(&mut doc, &desktop(&[(1, "ada"), (2, "bo")])).unwrap();
        doc.commit();

        // Ada notes how to resume her window and where her device is.
        let before = doc.get_heads();
        assert!(write_window(&mut doc, 1, Some("resume-1")).unwrap());
        assert!(!write_window(&mut doc, 1, Some("resume-1")).unwrap());
        let ada = Focus {
            window: 1,
            author: "ada".into(),
        };
        assert!(write_focus(&mut doc, 7, Some(&ada)).unwrap());
        assert!(!write_focus(&mut doc, 7, Some(&ada)).unwrap());
        doc.commit();
        assert!(judged(&doc, &before, "ada"));
        assert!(!judged(&doc, &before, "bo"));

        // A layout write keeps both.
        super::super::write(&mut doc, &desktop(&[(1, "ada"), (2, "bo"), (3, "ada")])).unwrap();
        doc.commit();
        assert_eq!(windows(&doc).get(&1).map(String::as_str), Some("resume-1"));
        assert_eq!(focus(&doc).get(&7), Some(&ada));

        // Bo may not resume Ada's window, nor move her focus.
        let before = doc.get_heads();
        write_window(&mut doc, 1, Some("hijack")).unwrap();
        doc.commit();
        assert!(!judged(&doc, &before, "bo"));
        let mut doc_b = doc.fork();
        let before = doc_b.get_heads();
        write_focus(&mut doc_b, 7, None).unwrap();
        doc_b.commit();
        assert!(!judged(&doc_b, &before, "bo"));
        assert!(judged(&doc_b, &before, "ada"));

        // A window without a record entry has nothing to resume.
        assert!(!write_window(&mut doc, 99, Some("x")).unwrap());
    }
}
