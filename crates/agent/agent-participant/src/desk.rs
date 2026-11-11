//! An agent's view on the person's desk.
//!
//! No action is performed out of sight: when an agent makes or changes a
//! record that no window on the workspace's shared desktop shows, it adds a
//! tab for that record to the desktop, right after the group showing the
//! record it worked in before (the view its task already occupies), on
//! whichever strip holds that. The window is the person's the agent acts
//! for, and says which run of the agent opened it. Every page of the
//! workspace draws it from the synced desktop as it draws its own tabs,
//! mounting the app over the record locally: nothing is streamed. A record
//! already on the desk is left where the people put it. Records are named as
//! every page notes its windows' (the record's wire spelling, the empty one
//! for an app's primary record) and told apart by their app as well, so a
//! person's own window of an app's primary record is the window that shows
//! it, not a reason to open a second. A workspace nobody
//! has opened a desk in yet gets one, as the first page in would make it,
//! holding the agent's window.

use automerge::AutoCommit;
use tilo_desktop_model::commands::{
    DesktopCommand, OpenContext, apply_command, namespace_counter, open_app_placement,
};
use tilo_desktop_model::ownership::{OpenedBy, SharedDesktop};
use tilo_desktop_model::{
    DesktopTree, IdMinter, TileContent, Workspace, WorkspaceId, WorkspaceSlot, shared_record,
};

pub(crate) struct View<'a> {
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
pub(crate) struct Opener<'a> {
    pub person: &'a str,
    pub run: Option<OpenedBy>,
}

/// The change that puts `view` on the shared `desktop` of `workspace` (its
/// id and, to start a desktop there, its name), opened by `opener`; `None`
/// when a tab already shows the record, or the workspace has no shared
/// desktop and no name to start one under.
pub(crate) fn place_view(
    desktop: &AutoCommit,
    (workspace, name): (&str, Option<&str>),
    opener: &Opener<'_>,
    view: &View<'_>,
) -> Option<Vec<u8>> {
    let mut doc = desktop.clone();
    let mut desktop = shared_record::read(&doc, None).or_else(|| fresh(workspace, name?))?;
    if shown_at(&desktop, view.app_id, view.record).is_some() {
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

/// The desktop the first page in `workspace` makes: the workspace itself,
/// by its id and in the slot every device derives from it, under `name`.
/// The strip the first window opens on is made with it ("Main"), so a page
/// that wrote its own before this reached it folds into the same one.
fn fresh(workspace: &str, name: &str) -> Option<SharedDesktop> {
    let identity = WorkspaceId::parse(workspace)?;
    let desk = Workspace::in_workspace(WorkspaceSlot::of_workspace(identity), identity, name);
    Some(SharedDesktop {
        tree: DesktopTree {
            active: desk.slot,
            workspaces: vec![desk],
            ..DesktopTree::default()
        },
        ..SharedDesktop::default()
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
    tilo_desktop_model::ids::device_salt(u16::try_from(folded).unwrap_or(1))
}

/// What publishes `change`, a change of the workspace's desk: the change alone where the
/// workspace `holds` a desk, the whole desk where it holds none yet, since a record starts whole.
pub(crate) fn desk_mutation(holds: bool, change: Vec<u8>) -> collab_types::DocumentMutation {
    if holds {
        return collab_types::DocumentMutation::Incremental(change);
    }
    let mut desk = AutoCommit::new();
    if let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut desk, &change) {
        tracing::warn!(%error, "agent: a new desk did not take its first view");
    }
    collab_types::DocumentMutation::Snapshot(desk.save())
}

#[cfg(test)]
mod tests {

    use super::*;
    use tilo_automerge::ApplyIncoming as _;
    use tilo_desktop_model::{Strip, StripId, Tab, WindowGroup, Workspace, WorkspaceSlot};

    const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
    const PERSON: &str = "ada";

    fn opener() -> Opener<'static> {
        Opener {
            person: PERSON,
            run: Some(OpenedBy {
                run: "agent-run:7".into(),
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
            width: tilo_desktop_model::width::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A desk with the person's strip (Chat) and the strip the agent's task
    /// runs on (Notes, which it works in), as one shared desktop change.
    fn desk() -> Vec<u8> {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Workspace");
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
            tree: tilo_desktop_model::DesktopTree {
                workspaces: vec![workspace],
                ..tilo_desktop_model::DesktopTree::default()
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
        let mut changes = vec![desk()];
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: Some(("so.tilo.notes", "notes-record")),
        };
        let change = place_view(&merged(&changes), (WORKSPACE, None), &opener(), &view)
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
            place_view(&merged(&changes), (WORKSPACE, None), &opener(), &view).is_none(),
            "a record already on the desk stays where it is"
        );
    }

    /// Notes keeps its notes in its primary record, which a person's own Notes window shows and
    /// notes on the desk in the wire spelling (empty). A note the agent makes there is in that
    /// window already: no second window opens on the same record. The empty spelling is every
    /// app's primary record, so a primary of another app still gets its window.
    #[test]
    fn a_primary_record_a_persons_window_shows_opens_no_second_window() {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Workspace");
        workspace.strips = vec![Strip {
            id: StripId(10),
            name: "Main".into(),
            window_groups: vec![group(20, tab(21, "so.tilo.notes"))],
        }];
        let desktop = SharedDesktop {
            tree: tilo_desktop_model::DesktopTree {
                workspaces: vec![workspace],
                ..tilo_desktop_model::DesktopTree::default()
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
            place_view(&doc, (WORKSPACE, None), &opener(), &notes).is_none(),
            "the person's Notes window shows the note"
        );
        let chat = View {
            app_id: "so.tilo.chat",
            title: "Chat",
            record: "",
            beside: Some(("so.tilo.notes", "")),
        };
        let change = place_view(&doc, (WORKSPACE, None), &opener(), &chat)
            .expect("another app's primary record opens its own window");
        let desktop = read(&[doc.save(), change]);
        let groups = &desktop.tree.workspaces[0].strips[0].window_groups;
        assert_eq!(groups.len(), 2, "beside the Notes window: {desktop:?}");
        assert_eq!(groups[0].id, 20);
    }

    #[test]
    fn a_workspace_without_a_shared_desktop_gets_nothing() {
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: None,
        };
        assert!(place_view(&AutoCommit::new(), (WORKSPACE, None), &opener(), &view).is_none());
    }

    #[test]
    fn a_workspace_nobody_opened_a_desk_in_gets_one_holding_the_view() {
        let view = View {
            app_id: "so.tilo.files",
            title: "Files",
            record: "files-record",
            beside: None,
        };
        let change = place_view(
            &AutoCommit::new(),
            (WORKSPACE, Some("Visa")),
            &opener(),
            &view,
        )
        .expect("a desk is started");
        let desktop = read(&[change]);
        let workspace = &desktop.tree.workspaces[0];
        assert_eq!(workspace.name, "Visa");
        assert_eq!(
            workspace.slot,
            WorkspaceSlot::of_workspace(WorkspaceId::parse(WORKSPACE).unwrap()),
            "the slot every device derives from the workspace"
        );
        let tab = workspace.strips[0].window_groups[0].tabs[0].id;
        assert_eq!(desktop.records, vec![(tab, "files-record".to_owned())]);
        assert_eq!(desktop.authors.get(&tab).map(String::as_str), Some(PERSON));
        assert_eq!(desktop.opened_by.get(&tab), opener().run.as_ref());
    }
}
