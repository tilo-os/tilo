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
use crate::{IdMinter, TileContent, shared_resource};

pub struct View<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    /// The document the view shows, in the wire spelling pages note their
    /// windows' documents in: empty for the app's primary document.
    pub document: &'a str,
    /// The app and record whose view the agent works in already, to open
    /// beside.
    pub beside: Option<(&'a str, &'a str)>,
}

/// Who opens a view: the person whose window it is, and the agent session that
/// opened it for them.
pub struct Opener<'a> {
    pub person: &'a str,
    pub opened_by: Option<OpenedBy>,
}

/// Why a view could not be placed on a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotPlaced {
    /// The document holds no layout: no page showing the workspace has started one yet.
    NoLayout,
    /// The layout holds no workspace to put a window in.
    NoWorkspace,
    /// The layout had no place for the window.
    NoPlace(String),
    /// The layout with the window did not write.
    Unwritten(String),
}

impl std::fmt::Display for NotPlaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLayout => f.write_str("the workspace has no layout yet: no page has shown it"),
            Self::NoWorkspace => f.write_str("the workspace's layout holds no workspace"),
            Self::NoPlace(why) => write!(f, "the layout has no place for the window: {why}"),
            Self::Unwritten(why) => write!(f, "the layout with the window did not write: {why}"),
        }
    }
}

/// The change that puts `view` on `layout`, opened by `opener`: `None` when a
/// tab already shows the document, which stays where the people put it.
///
/// # Errors
///
/// Why the view could not be placed ([`NotPlaced`]).
pub fn place_view(
    layout: &AutoCommit,
    opener: &Opener<'_>,
    view: &View<'_>,
    time: i64,
) -> Result<Option<Vec<u8>>, NotPlaced> {
    let mut doc = layout.clone();
    let mut desktop = shared_resource::read(&doc, None).ok_or(NotPlaced::NoLayout)?;
    if shown_at(&desktop, view.app_id, view.document).is_some()
        || (view.document.is_empty() && opening(&desktop, view.app_id))
    {
        return Ok(None);
    }
    let author = opener.person;
    let tab = add_tab(&mut desktop, author, view)?;
    desktop.records.push((tab, view.document.to_owned()));
    desktop.records.sort_unstable();
    // The window is the person's to close, as anyone's window is theirs;
    // the agent session says their agent opened it.
    desktop.authors.insert(tab, author.to_owned());
    if let Some(opened_by) = &opener.opened_by {
        desktop.opened_by.insert(tab, opened_by.clone());
    }
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(author.as_bytes()));
    shared_resource::write(&mut doc, &desktop)
        .map_err(|error| NotPlaced::Unwritten(error.to_string()))?;
    shared_resource::commit_change(&mut doc, author, time, None);
    Ok(Some(doc.save_after(&before)))
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
fn add_tab(desktop: &mut SharedDesktop, author: &str, view: &View<'_>) -> Result<u64, NotPlaced> {
    let beside_tab = view
        .beside
        .and_then(|(app, record)| shown_at(desktop, app, record));
    let workspace = desktop
        .tree
        .workspaces
        .first_mut()
        .ok_or(NotPlaced::NoWorkspace)?;
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
    let placement = open_app_placement(workspace, &at, |_| true)
        .map_err(|error| NotPlaced::NoPlace(format!("{error:?}")))?;
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
    .map_err(|error| NotPlaced::NoPlace(format!("{error:?}")))?;
    opened.ok_or_else(|| NotPlaced::NoPlace("no tab was opened".to_owned()))
}

/// The change that closes the window `tab` on `layout` as `person` closes it, everywhere the
/// layout is shown: `None` when no window of the layout is `tab` (closed already, by someone
/// else or on another device). A window on a shared layout is every member's to close
/// ([`crate::ownership::shares_windows`]); its record stays in the workspace.
///
/// # Errors
///
/// The layout holds none, or the layout without the window did not write.
pub fn close_window(
    layout: &AutoCommit,
    person: &str,
    tab: u64,
    time: i64,
) -> Result<Option<Vec<u8>>, NotPlaced> {
    let mut doc = layout.clone();
    let mut desktop = shared_resource::read(&doc, None).ok_or(NotPlaced::NoLayout)?;
    let found = desktop.tree.workspaces.iter_mut().find_map(|workspace| {
        let location = workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group.tabs.iter().any(|shown| shown.id == tab).then_some(
                    crate::commands::TabLocation {
                        strip: strip.id.0,
                        group: group.id,
                        tab,
                    },
                )
            })
        })?;
        Some((workspace, location))
    });
    let Some((workspace, location)) = found else {
        return Ok(None);
    };
    apply_command(
        workspace,
        &DesktopCommand::CloseTab { location },
        &mut || Err("closing a window makes nothing".to_owned()),
        &mut |_, _| Err("closing a window opens nothing".to_owned()),
    )
    .map_err(NotPlaced::NoPlace)?;
    desktop.records.retain(|(shown, _)| *shown != tab);
    desktop.authors.remove(&tab);
    desktop.opened_by.remove(&tab);
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(person.as_bytes()));
    shared_resource::write(&mut doc, &desktop)
        .map_err(|error| NotPlaced::Unwritten(error.to_string()))?;
    shared_resource::commit_change(&mut doc, person, time, None);
    Ok(Some(doc.save_after(&before)))
}

/// The change that stands the window `tab` at `(x, y)` on its workspace's canvas, as a drag
/// that let it go there commits: one arranging step for the whole gesture, attributed to
/// `person`. `None` when the layout holds no such window, or the window stood there already.
///
/// # Errors
///
/// The layout holds no desk, or the layout with the window moved did not write.
pub fn move_window(
    layout: &AutoCommit,
    person: &str,
    tab: u64,
    x: i32,
    y: i32,
    time: i64,
) -> Result<Option<Vec<u8>>, NotPlaced> {
    let mut doc = layout.clone();
    let mut desktop = shared_resource::read(&doc, None).ok_or(NotPlaced::NoLayout)?;
    let Some((workspace, group)) = group_of(&desktop.tree, tab) else {
        return Ok(None);
    };
    let Some(held) = desktop.tree.workspaces.get_mut(workspace) else {
        return Ok(None);
    };
    if !held.layout.set_position(group, x, y) {
        return Ok(None);
    }
    write_as(&mut doc, &desktop, person, time).map(Some)
}

/// The change that gives the window `tab` the canvas size `width` × `height`, as dragging its
/// edge does: one arranging step for the whole gesture, attributed to `person`. `None` when the
/// layout holds no such window, or the window had that size already.
///
/// # Errors
///
/// The layout holds no desk, or the layout with the window resized did not write.
pub fn size_window(
    layout: &AutoCommit,
    person: &str,
    tab: u64,
    width: u32,
    height: u32,
    time: i64,
) -> Result<Option<Vec<u8>>, NotPlaced> {
    let mut doc = layout.clone();
    let mut desktop = shared_resource::read(&doc, None).ok_or(NotPlaced::NoLayout)?;
    let Some((workspace, group)) = group_of(&desktop.tree, tab) else {
        return Ok(None);
    };
    let Some(held) = desktop.tree.workspaces.get_mut(workspace) else {
        return Ok(None);
    };
    if !held.layout.set_size(group, width, height) {
        return Ok(None);
    }
    write_as(&mut doc, &desktop, person, time).map(Some)
}

/// The change that brings the window `tab` in front of its strip and the workspace it stands in
/// forward, as a press on its bar does. `None` when the layout holds no such window, or it was in
/// front already.
///
/// # Errors
///
/// The layout holds no desk, or the layout with the window in front did not write.
pub fn activate_window(
    layout: &AutoCommit,
    person: &str,
    tab: u64,
    time: i64,
) -> Result<Option<Vec<u8>>, NotPlaced> {
    let mut doc = layout.clone();
    let mut desktop = shared_resource::read(&doc, None).ok_or(NotPlaced::NoLayout)?;
    let Some((workspace, strip, group)) = locate_of(&desktop.tree, tab) else {
        return Ok(None);
    };
    let Some((workspace_id, strip_id, group_id, active)) =
        desktop.tree.workspaces.get(workspace).and_then(|held| {
            let row = held.strips.get(strip)?;
            let window = row.window_groups.get(group)?;
            Some((held.id(), row.id, window.id, window.active_tab))
        })
    else {
        return Ok(None);
    };
    let in_front = desktop.tree.active == workspace_id
        && desktop.tree.active_strip.get(&workspace_id) == Some(&strip_id)
        && active == tab;
    if in_front {
        return Ok(None);
    }
    desktop.tree.active = workspace_id;
    desktop.tree.active_strip.insert(workspace_id, strip_id);
    desktop.tree.set_active_group(strip_id, group_id);
    if let Some(window) = desktop
        .tree
        .workspaces
        .get_mut(workspace)
        .and_then(|held| held.strips.get_mut(strip))
        .and_then(|row| row.window_groups.get_mut(group))
    {
        window.activate_tab(tab);
    }
    write_as(&mut doc, &desktop, person, time).map(Some)
}

/// The group holding the window `tab`, with the workspace it stands in, on `tree`.
fn group_of(tree: &crate::Layout, tab: u64) -> Option<(usize, u64)> {
    tree.workspaces
        .iter()
        .enumerate()
        .find_map(|(at, workspace)| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|shown| shown.id == tab))
                .map(|group| (at, group.id))
        })
}

/// The workspace, row and group the window `tab` stands in, on `tree`.
fn locate_of(tree: &crate::Layout, tab: u64) -> Option<(usize, usize, usize)> {
    tree.workspaces
        .iter()
        .enumerate()
        .find_map(|(workspace, held)| {
            held.strips.iter().enumerate().find_map(|(strip, row)| {
                row.window_groups
                    .iter()
                    .position(|group| group.tabs.iter().any(|shown| shown.id == tab))
                    .map(|group| (workspace, strip, group))
            })
        })
}

/// Write `desktop` into `doc` as `person`'s change, and answer it.
fn write_as(
    doc: &mut AutoCommit,
    desktop: &SharedDesktop,
    person: &str,
    time: i64,
) -> Result<Vec<u8>, NotPlaced> {
    let before = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(person.as_bytes()));
    shared_resource::write(doc, desktop)
        .map_err(|error| NotPlaced::Unwritten(error.to_string()))?;
    shared_resource::commit_change(doc, person, time, None);
    Ok(doc.save_after(&before))
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

    /// When the tests' changes are made.
    const AT: i64 = 1_763_000_000;
    use crate::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceId};
    use tilo_automerge::ApplyIncoming as _;

    const PERSON: &str = "ada";

    fn opener() -> Opener<'static> {
        Opener {
            person: PERSON,
            opened_by: Some(OpenedBy {
                run: "agent-session:7".into(),
                agent: tilo_member_types::MemberId::agent("scribe", PERSON),
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
            items: Vec::new(),
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_resource::write(&mut doc, &desktop).unwrap();
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
        shared_resource::read(&merged(changes), None).expect("a desktop")
    }

    #[test]
    fn the_agents_view_opens_on_its_tasks_strip_beside_its_last_view() {
        let mut changes = vec![layout()];
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            document: "files-record",
            beside: Some(("so.tilo.notes", "notes-record")),
        };
        let change = place_view(&merged(&changes), &opener(), &view, AT)
            .expect("the view is placed")
            .expect("a tab is added");
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
            place_view(&merged(&changes), &opener(), &view, AT) == Ok(None),
            "a record already on the layout stays where it is"
        );
    }

    /// A member closes a window of the shared layout: it leaves the layout with what was noted
    /// beside it, the rest stays where it was, and closing it again changes nothing.
    #[test]
    fn a_closed_window_leaves_the_layout_with_its_record_note() {
        let mut changes = vec![layout()];
        let change = close_window(&merged(&changes), PERSON, 21, AT)
            .expect("the layout writes")
            .expect("the window was there");
        changes.push(change);
        let desktop = read(&changes);
        assert!(
            desktop.tree.tab(21).is_none(),
            "the window is gone: {desktop:?}"
        );
        assert!(
            !desktop.records.iter().any(|(tab, _)| *tab == 21),
            "its record note went with it"
        );
        assert!(desktop.tree.tab(31).is_some(), "the other window stays");
        assert_eq!(close_window(&merged(&changes), PERSON, 21, AT), Ok(None));
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
            items: Vec::new(),
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_resource::write(&mut doc, &desktop).unwrap();
        doc.commit();
        let notes = View {
            app_id: "so.tilo.notes",
            title: "Notes",
            document: "",
            beside: None,
        };
        assert!(
            place_view(&doc, &opener(), &notes, AT) == Ok(None),
            "the person's Notes window shows the note"
        );
        let chat = View {
            app_id: "so.tilo.chat",
            title: "Chat",
            document: "",
            beside: Some(("so.tilo.notes", "")),
        };
        let change = place_view(&doc, &opener(), &chat, AT)
            .expect("the view is placed")
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
            items: Vec::new(),
            authors: std::collections::BTreeMap::new(),
            opened_by: std::collections::BTreeMap::new(),
        };
        let mut doc = AutoCommit::new();
        shared_resource::write(&mut doc, &desktop).unwrap();
        doc.commit();
        let notes = View {
            app_id: "so.tilo.notes",
            title: "Notes",
            document: "",
            beside: None,
        };
        assert_eq!(place_view(&doc, &opener(), &notes, AT), Ok(None));
        let page = View {
            document: "a-page-record",
            ..notes
        };
        assert!(
            matches!(place_view(&doc, &opener(), &page, AT), Ok(Some(_))),
            "a record of its own still gets its window"
        );
    }

    /// A device that holds no layout of the workspace starts none, and says so: the view is placed
    /// by a device that holds it, so it lands beside the people's own windows.
    #[test]
    fn a_device_without_the_layout_starts_none() {
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            document: "files-record",
            beside: None,
        };
        assert_eq!(
            place_view(&AutoCommit::new(), &opener(), &view, AT),
            Err(NotPlaced::NoLayout)
        );
    }
}
