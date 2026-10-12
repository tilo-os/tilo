//! An agent's view on the person's desk.
//!
//! No action is performed out of sight: when an agent starts working in a
//! record that no window on the room's shared desktop shows, it adds a tab
//! for that record to the desktop, right after the group showing the record
//! it worked in before (the view its task already occupies), on whichever
//! strip holds that. Every page of the room draws it from the synced desktop
//! as it draws its own tabs, mounting the app over the record locally:
//! nothing is streamed. A record already on the desk is left where the
//! people put it.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _, ScalarValue, Value};
use tilo_desktop_model::commands::{
    DesktopCommand, OpenContext, apply_command, namespace_counter, open_app_placement,
};
use tilo_desktop_model::{DesktopTree, IdMinter, TileContent};

/// The key of the shared desktop document holding the desktop as JSON.
const DESKTOP_KEY: &str = "desktop";

/// The desktop as pages share it: the tree and each tab's record.
#[derive(serde::Serialize, serde::Deserialize)]
struct SharedDesktop {
    tree: DesktopTree,
    records: Vec<(u64, String)>,
}

/// One view to put on the desk.
pub(crate) struct View<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    /// The record the view shows, as tabs bind it.
    pub record: &'a str,
    /// The record whose view the agent works in already, to open beside.
    pub beside: Option<&'a str>,
}

/// The change that puts `view` on the shared `desktop`, written by
/// `author`; `None` when a tab already shows the record, or the room has no
/// shared desktop.
pub(crate) fn place_view(desktop: &AutoCommit, author: &str, view: &View<'_>) -> Option<Vec<u8>> {
    let mut doc = desktop.clone();
    let Ok(Some((Value::Scalar(value), _))) = doc.get(automerge::ROOT, DESKTOP_KEY) else {
        return None;
    };
    let ScalarValue::Str(text) = value.as_ref() else {
        return None;
    };
    let mut desktop: SharedDesktop = serde_json::from_str(text).ok()?;
    if desktop
        .records
        .iter()
        .any(|(_, record)| record == view.record)
    {
        return None;
    }
    let tab = add_tab(&mut desktop, author, view)?;
    desktop.records.push((tab, view.record.to_owned()));
    desktop.records.sort_unstable();
    let json = serde_json::to_string(&desktop).ok()?;
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(author.as_bytes()));
    doc.put(automerge::ROOT, DESKTOP_KEY, json).ok()?;
    doc.commit();
    Some(doc.save_after(&before))
}

/// Add a tab for `view` to the desktop's first workspace, beside the group
/// showing `view.beside`, in `author`'s id namespace. Returns the tab id.
fn add_tab(desktop: &mut SharedDesktop, author: &str, view: &View<'_>) -> Option<u64> {
    let beside_tab = view.beside.and_then(|beside| {
        desktop
            .records
            .iter()
            .find(|(_, record)| record == beside)
            .map(|(tab, _)| *tab)
    });
    let workspace = desktop.tree.workspaces.first_mut()?;
    let beside_group = beside_tab.and_then(|tab| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
            .map(|group| group.id)
    });
    let at = beside_group
        .and_then(|group| OpenContext::beside(workspace, group))
        .unwrap_or_default();
    let placement = open_app_placement(workspace, &at, |_| true).ok()?;
    let salt = salt_of(author);
    let mut ids = IdMinter::resume(salt, namespace_counter(workspace, salt, 1));
    let mut opened = None;
    apply_command(
        workspace,
        &DesktopCommand::OpenTab {
            app: view.app_id.to_owned(),
            placement,
        },
        &mut || ids.mint_in_namespace(),
        &mut |app, tab| {
            opened = Some(tab);
            Ok((
                view.title.to_owned(),
                TileContent::LocalApp {
                    app_id: app.to_owned(),
                    label: view.title.to_owned(),
                    note: String::new(),
                },
            ))
        },
    )
    .ok()?;
    opened
}

/// `author`'s share of the desktop id space: FNV-1a folded to 16 bits, never 0.
fn salt_of(author: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in author.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash >> 16) ^ (hash & 0xffff);
    u16::try_from(folded)
        .ok()
        .filter(|salt| *salt != 0)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::*;
    use tilo_automerge::ApplyIncoming as _;
    use tilo_desktop_model::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceId};

    fn tab(id: u64, app: &str) -> Tab {
        Tab {
            id,
            title: app.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
        }
    }

    fn group(id: u64, tab: Tab) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: tab.id,
            tabs: vec![tab],
            width: tilo_desktop_model::width::WindowWidth::Normal,
            maximized: false,
        }
    }

    /// A desk with the person's strip (Chat) and the strip the agent's task
    /// runs on (Notes, which it works in), as one shared desktop change.
    fn desk() -> Vec<u8> {
        let mut workspace = Workspace::new(WorkspaceId(1), "Room");
        workspace.strips = vec![
            Strip {
                id: StripId(10),
                name: "Mine".into(),
                window_groups: vec![group(20, tab(21, "so.tilo.chat"))],
            },
            Strip {
                id: StripId(11),
                name: "Task".into(),
                window_groups: vec![group(30, tab(31, "so.tilo.notes"))],
            },
        ];
        let mut tree = DesktopTree::default();
        tree.workspaces.push(workspace);
        let desktop = SharedDesktop {
            tree,
            records: vec![(21, "chat-record".into()), (31, "notes-record".into())],
        };
        let mut doc = AutoCommit::new();
        doc.put(
            automerge::ROOT,
            DESKTOP_KEY,
            serde_json::to_string(&desktop).unwrap(),
        )
        .unwrap();
        doc.save()
    }

    fn merged(changes: &[Vec<u8>]) -> AutoCommit {
        let mut doc = AutoCommit::new();
        for bytes in changes {
            doc.load_incoming(bytes).unwrap();
        }
        doc
    }

    fn read(changes: &[Vec<u8>]) -> SharedDesktop {
        let doc = merged(changes);
        let text = doc
            .get(automerge::ROOT, DESKTOP_KEY)
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok())
            .expect("the desktop is a string");
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn the_agents_view_opens_on_its_tasks_strip_beside_its_last_view() {
        let mut changes = vec![desk()];
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: Some("notes-record"),
        };
        let change = place_view(&merged(&changes), "agent:scribe", &view).expect("a tab is added");
        changes.push(change);
        let desktop = read(&changes);
        let task = &desktop.tree.workspaces[0].strips[1];
        assert_eq!(task.window_groups.len(), 2, "on the task's strip");
        assert_eq!(
            task.window_groups[0].id, 30,
            "right after the view it works in"
        );
        let added = &task.window_groups[1].tabs[0];
        assert!(
            matches!(&added.tile, TileContent::LocalApp { app_id, .. } if app_id == "so.tilo.files")
        );
        assert!(
            desktop
                .records
                .contains(&(added.id, "files-record".to_owned())),
            "the tab shows the record the agent works in"
        );
        assert_eq!(
            desktop.tree.workspaces[0].strips[0].window_groups.len(),
            1,
            "the person's strip is untouched"
        );
        assert!(
            place_view(&merged(&changes), "agent:scribe", &view).is_none(),
            "a record already on the desk stays where it is"
        );
    }

    #[test]
    fn a_room_without_a_shared_desktop_gets_nothing() {
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: None,
        };
        assert!(place_view(&AutoCommit::new(), "agent:scribe", &view).is_none());
    }
}
