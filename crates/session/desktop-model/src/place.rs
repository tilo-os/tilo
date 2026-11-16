//! A view placed on a workspace's layout by someone other than the person
//! arranging it: an agent's window, or a window a member's open asks for.
//!
//! The view becomes a tab of the layout the device holds, beside the group
//! showing the record it works in already, and the change travels to every
//! device as any change of the layout does. A device that holds no layout
//! places nothing: the layout is started only by a page that shows it. A record
//! a window already shows is left where the people put it. Records are named as
//! every page notes its windows' (the record's wire spelling, the empty one for
//! an app's primary record) and told apart by their app as well.

use automerge::AutoCommit;

use crate::commands::{
    DesktopCommand, OpenContext, apply_command, namespace_counter, open_app_placement,
};
use crate::ownership::{OpenedBy, SharedDesktop};
use crate::{IdMinter, TileContent, shared_record};

pub struct View<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    /// The record the view shows, in the wire spelling pages note their
    /// windows' records in: empty for the app's primary record.
    pub record: &'a str,
    /// The app and record whose view the agent works in already, to open
    /// beside.
    pub beside: Option<(&'a str, &'a str)>,
}

/// Who opens a view: the person whose window it is, and the agent run that
/// opened it for them.
pub struct Opener<'a> {
    pub person: &'a str,
    pub run: Option<OpenedBy>,
}

/// The change that puts `view` on `layout`, opened by `opener`; `None` when a
/// tab already shows the record, or `layout` holds no layout.
#[must_use]
pub fn place_view(layout: &AutoCommit, opener: &Opener<'_>, view: &View<'_>) -> Option<Vec<u8>> {
    let mut doc = layout.clone();
    let mut desktop = shared_record::read(&doc, None)?;
    if shown_at(&desktop, view.app_id, view.record).is_some()
        || (view.record.is_empty() && opening(&desktop, view.app_id))
    {
        return None;
    }
    let author = opener.person;
    let tab = add_tab(&mut desktop, author, view)?;
    desktop.records.push((tab, view.record.to_owned()));
    desktop.records.sort_unstable();
    // The window is the person's to close, as anyone's window is theirs;
    // the run says their agent opened it.
    desktop.authors.insert(tab, author.to_owned());
    if let Some(run) = &opener.run {
        desktop.opened_by.insert(tab, run.clone());
    }
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(author.as_bytes()));
    shared_record::write(&mut doc, &desktop).ok()?;
    doc.commit();
    Some(doc.save_after(&before))
}

/// The tab of `desktop` that shows `app`'s `record`: the record and the app
/// its window runs both, since the primary record of every app is spelled the
/// same (empty).
fn shown_at(desktop: &SharedDesktop, app: &str, record: &str) -> Option<u64> {
    desktop
        .records
        .iter()
        .find(|(tab, shown)| {
            shown == record
                && desktop.tree.tab(*tab).is_some_and(|tab| {
                    matches!(&tab.tile, TileContent::LocalApp { app_id, .. } if app_id == app)
                })
        })
        .map(|(tab, _)| *tab)
}

/// Whether a window of `app` stands on `desktop` with no record noted for it yet: a window still
/// opening, whose app has not bound what it shows. An app opens its primary record when nothing
/// names another, so that window is about to show it: a person whose window loads slowly is not
/// given a second one beside it.
fn opening(desktop: &SharedDesktop, app: &str) -> bool {
    desktop.tree.workspaces.iter().any(|workspace| {
        workspace.tabs().any(|tab| {
            matches!(&tab.tile, TileContent::LocalApp { app_id, .. } if app_id == app)
                && !desktop.records.iter().any(|(noted, _)| *noted == tab.id)
        })
    })
}

/// Add a tab for `view` to the desktop's first workspace, beside the group
/// showing `view.beside`, in `author`'s id namespace. Returns the tab id.
fn add_tab(desktop: &mut SharedDesktop, author: &str, view: &View<'_>) -> Option<u64> {
    let beside_tab = view
        .beside
        .and_then(|(app, record)| shown_at(desktop, app, record));
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

/// `author`'s share of the desktop id space: FNV-1a folded to 16 bits, never
/// `0` or the reserved namespace.
fn salt_of(author: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in author.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash >> 16) ^ (hash & 0xffff);
    crate::ids::device_salt(u16::try_from(folded).unwrap_or(1))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceId};
    use tilo_automerge::ApplyIncoming as _;

    const PERSON: &str = "ada";

    fn opener() -> Opener<'static> {
        Opener {
            person: PERSON,
            run: Some(OpenedBy {
                run: "agent-session:7".into(),
                label: "Scribe".into(),
                title: "File the receipts".into(),
            }),
        }
    }

    fn tab(id: u64, app: &str) -> Tab {
        Tab {
            id,
            title: app.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn group(id: u64, tab: Tab) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: tab.id,
            tabs: vec![tab],
            width: crate::width::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A layout with the person's strip (Chat) and the strip the agent's task
    /// runs on (Notes, which it works in), as one shared desktop change.
    fn layout() -> Vec<u8> {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Workspace");
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
        let desktop = SharedDesktop {
            tree: crate::Layout {
                workspaces: vec![workspace],
                ..crate::Layout::default()
            },
            records: vec![(21, "chat-record".into()), (31, "notes-record".into())],
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_record::write(&mut doc, &desktop).unwrap();
        doc.commit();
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
        shared_record::read(&merged(changes), None).expect("a desktop")
    }

    #[test]
    fn the_agents_view_opens_on_its_tasks_strip_beside_its_last_view() {
        let mut changes = vec![layout()];
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: Some(("so.tilo.notes", "notes-record")),
        };
        let change = place_view(&merged(&changes), &opener(), &view).expect("a tab is added");
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
            place_view(&merged(&changes), &opener(), &view).is_none(),
            "a record already on the layout stays where it is"
        );
    }

    /// Notes keeps its notes in its primary record, which a person's own Notes window shows and
    /// notes on the layout in the wire spelling (empty). A note the agent makes there is in that
    /// window already: no second window opens on the same record. The empty spelling is every
    /// app's primary record, so a primary of another app still gets its window.
    #[test]
    fn a_primary_record_a_persons_window_shows_opens_no_second_window() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Workspace");
        workspace.strips = vec![Strip {
            id: StripId(10),
            name: "Main".into(),
            window_groups: vec![group(20, tab(21, "so.tilo.notes"))],
        }];
        let desktop = SharedDesktop {
            tree: crate::Layout {
                workspaces: vec![workspace],
                ..crate::Layout::default()
            },
            records: vec![(21, String::new())],
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_record::write(&mut doc, &desktop).unwrap();
        doc.commit();
        let notes = View {
            app_id: "so.tilo.notes",
            title: "Notes",
            record: "",
            beside: None,
        };
        assert!(
            place_view(&doc, &opener(), &notes).is_none(),
            "the person's Notes window shows the note"
        );
        let chat = View {
            app_id: "so.tilo.chat",
            title: "Chat",
            record: "",
            beside: Some(("so.tilo.notes", "")),
        };
        let change = place_view(&doc, &opener(), &chat)
            .expect("another app's primary record opens its own window");
        let desktop = read(&[doc.save(), change]);
        let groups = &desktop.tree.workspaces[0].strips[0].window_groups;
        assert_eq!(groups.len(), 2, "beside the Notes window: {desktop:?}");
        assert_eq!(groups[0].id, 20);
    }

    /// A person's Notes window still loading has noted no record yet: it is about to show Notes'
    /// primary record, so the note the agent makes there opens no second window beside it.
    #[test]
    fn a_window_still_opening_its_apps_primary_record_gets_no_second() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Workspace");
        workspace.strips = vec![Strip {
            id: StripId(10),
            name: "Main".into(),
            window_groups: vec![group(20, tab(21, "so.tilo.notes"))],
        }];
        let desktop = SharedDesktop {
            tree: crate::Layout {
                workspaces: vec![workspace],
                ..crate::Layout::default()
            },
            records: Vec::new(),
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_record::write(&mut doc, &desktop).unwrap();
        doc.commit();
        let notes = View {
            app_id: "so.tilo.notes",
            title: "Notes",
            record: "",
            beside: None,
        };
        assert!(place_view(&doc, &opener(), &notes).is_none());
        let page = View {
            record: "a-page-record",
            ..notes
        };
        assert!(
            place_view(&doc, &opener(), &page).is_some(),
            "a record of its own still gets its window"
        );
    }

    /// A device that holds no layout of the workspace starts none: the view is placed by a device
    /// that holds it, so it lands beside the people's own windows.
    #[test]
    fn a_device_without_the_layout_starts_none() {
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: None,
        };
        assert!(place_view(&AutoCommit::new(), &opener(), &view).is_none());
    }
}
