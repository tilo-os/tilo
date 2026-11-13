//! What a person's devices need to find each other's work, kept in the
//! layout record beside the windows it is about.
//!
//! Every workspace is on every one of a person's devices, and each device
//! writes its own part of the layout. So where the person is on their laptop
//! is read from the laptop's part of the record, which every other device of
//! the person already holds:
//!
//! - Each device's **focus**: the window it is on in this workspace, under
//!   the device's id namespace (its salt), naming the person at the device.
//!   Only that person writes or removes it ([`foreign_changes`]).
//!
//! Which record each window shows is noted beside the window itself, and what
//! a window's app resumes is reported by the app to the session that runs it
//! (`ContinuationServices`); nothing of it is written here.
//!
//! Shape, beside the layout ([`super`]):
//!
//! ```text
//! focus: { <salt>: { window: <id>, author } }
//! ```

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjType, ROOT, ReadDoc};

use super::{AUTHOR, At};
use crate::ids::TabId;

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
/// did to a focus that is not `writer`'s: a focus naming someone else,
/// before or after.
pub(super) fn foreign_changes(
    doc: &AutoCommit,
    seen: &[ChangeHash],
    left: &[ChangeHash],
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
    let (was, now) = (focus_at(&before), focus_at(&after));
    was.iter()
        .chain(now.iter())
        .find(|(salt, focus)| was.get(*salt) != now.get(*salt) && focus.author != writer)
        .map(|(_, focus)| focus.window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{StripId, WorkspaceSlot};
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::ownership::SharedDesktop;
    use crate::tree::DesktopTree;
    use crate::width::WindowWidth;
    use crate::workspace::Workspace;

    fn desktop(windows: &[(u64, &str)]) -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
        let mut strip = Strip::new(StripId(10), "Lounge");
        for (id, _) in windows {
            strip.window_groups.push(WindowGroup {
                id: id.wrapping_add(100),
                tabs: vec![Tab {
                    id: *id,
                    title: format!("tab {id}"),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".into(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
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
                active: workspace.slot,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            authors: windows
                .iter()
                .map(|(id, who)| (*id, (*who).to_owned()))
                .collect(),
            records: Vec::new(),
            opened_by: std::collections::BTreeMap::new(),
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
    fn a_focus_survives_layout_writes_and_stays_its_person() {
        let mut doc = AutoCommit::new();
        super::super::write(&mut doc, &desktop(&[(1, "ada"), (2, "bo")])).unwrap();
        doc.commit();

        // Ada says where her device is.
        let before = doc.get_heads();
        let ada = Focus {
            window: 1,
            author: "ada".into(),
        };
        assert!(write_focus(&mut doc, 7, Some(&ada)).unwrap());
        assert!(!write_focus(&mut doc, 7, Some(&ada)).unwrap());
        doc.commit();
        assert!(judged(&doc, &before, "ada"));
        assert!(!judged(&doc, &before, "bo"));

        // A layout write keeps it.
        super::super::write(&mut doc, &desktop(&[(1, "ada"), (2, "bo"), (3, "ada")])).unwrap();
        doc.commit();
        assert_eq!(focus(&doc).get(&7), Some(&ada));

        // Bo may not move Ada's focus.
        let mut doc_b = doc.fork();
        let before = doc_b.get_heads();
        write_focus(&mut doc_b, 7, None).unwrap();
        doc_b.commit();
        assert!(!judged(&doc_b, &before, "bo"));
        assert!(judged(&doc_b, &before, "ada"));
    }
}
