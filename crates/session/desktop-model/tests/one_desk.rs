//! A workspace's desk is one document: the record an older build wrote opens as
//! that document, a build that knows only the old record keeps writing into
//! it, and peers that each change the desk keep both changes.
//!
//! Asserted through [`shared_record::read`], the function native and pages
//! both read a workspace's desk with, and [`desktop_doc::rebuild`], what the
//! desk's own document projects.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::{AutoCommit, ReadDoc as _};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_record;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceSlot,
};

fn identity() -> WorkspaceId {
    WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("a workspace identity")
}

fn tab(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: format!("so.tilo.{}", title.to_lowercase()),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

fn group(id: u64, tabs: &[u64]) -> WindowGroup {
    WindowGroup {
        id,
        tabs: tabs
            .iter()
            .map(|id| tab(*id, &format!("Tab {id}")))
            .collect(),
        active_tab: tabs[0],
        width: WindowWidth::Wide,
        maximized: true,
        focus_history: Vec::new(),
    }
}

/// One workspace with strip 1 holding group 10 [100, 101].
fn desk() -> DesktopTree {
    let mut workspace = Workspace::in_workspace(WorkspaceSlot(1), identity(), "Studio");
    workspace.set_owner("ada");
    let mut strip = Strip::new(StripId(1), "A");
    strip.window_groups = vec![group(10, &[100, 101])];
    workspace.strips = vec![strip];
    DesktopTree {
        workspaces: vec![workspace],
        active: WorkspaceSlot(1),
        ..DesktopTree::default()
    }
}

/// The desk as a workspace holds it, with one window on a record and two people
/// who opened windows of it.
fn shared(tree: DesktopTree) -> SharedDesktop {
    SharedDesktop {
        tree,
        records: vec![(100, "notes".into())],
        authors: [(100, "ada".to_owned()), (101, "grace".to_owned())].into(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// The windows of the first workspace, as `(title, tab)`.
fn windows(tree: &DesktopTree) -> Vec<(u64, String)> {
    tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab.title.clone()))
        .collect()
}

fn read_back(doc: &AutoCommit) -> DesktopTree {
    let mut fresh = DesktopTree::default();
    desktop_doc::rebuild(doc, &mut fresh);
    fresh
}

fn opened(doc: &AutoCommit) -> SharedDesktop {
    shared_record::read(doc, None).expect("a desk")
}

/// The record older builds wrote — a workspace, strip, group and window as
/// an entry each at the record's root — opens as the desk's one document
/// with its windows, records and people, and the entries it wrote stop
/// being what the record holds.
#[test]
fn an_old_shared_record_opens_as_the_desks_own_document() {
    let desk = shared(desk());
    let mut doc = AutoCommit::new();
    assert!(shared_record::write(&mut doc, &desk).expect("written"));
    doc.commit();
    assert!(
        !desktop_doc::holds(&doc),
        "an older build writes the record, entry by entry"
    );

    let as_written = opened(&doc);
    assert_eq!(windows(&as_written.tree), windows(&desk.tree));
    assert_eq!(as_written.records, desk.records);
    assert_eq!(as_written.authors, desk.authors);

    assert!(desktop_doc::write_shared(
        &mut doc,
        &as_written.tree,
        &as_written.records,
        &as_written.authors
    ));
    doc.commit();
    assert!(desktop_doc::holds(&doc), "the desk's own document now");
    for key in ["strips", "groups", "windows"] {
        assert!(
            doc.get(automerge::ROOT, key).ok().flatten().is_none(),
            "the old record's `{key}` entries are gone"
        );
    }

    let reopened = opened(&doc);
    assert_eq!(windows(&reopened.tree), windows(&desk.tree));
    assert_eq!(reopened.records, desk.records);
    assert_eq!(reopened.authors, desk.authors);
    assert_eq!(
        reopened.tree.workspaces[0].owner().map(String::as_str),
        Some("ada"),
        "who owns the workspace came with it"
    );
    assert_eq!(windows(&read_back(&doc)), windows(&desk.tree));

    // Read as of an earlier change, the desk is the desk as it was then:
    // what the activity pane steps back through.
    let then = doc.get_heads();
    let mut renamed = desk.tree.clone();
    renamed.workspaces[0].name = "Studio · renamed".into();
    assert!(shared_record::write(&mut doc, &shared(renamed)).expect("written"));
    doc.commit();
    assert_eq!(
        shared_record::read(&doc, Some(&then))
            .expect("the desk then")
            .tree
            .workspaces[0]
            .name,
        "Studio"
    );
    assert_eq!(
        opened(&doc).tree.workspaces[0].name,
        "Studio · renamed",
        "and now it is the desk as it is"
    );
}

/// A peer that knows only the old record — a build from before the desk was
/// one document — writes into the desk's document rather than beside it, so
/// both keep working on the same desk.
#[test]
fn a_build_from_before_writes_into_the_desk_document() {
    let desk = shared(desk());
    let mut doc = AutoCommit::new();
    assert!(shared_record::write(&mut doc, &desk).expect("written"));
    assert!(desktop_doc::write_shared(
        &mut doc,
        &desk.tree,
        &desk.records,
        &desk.authors
    ));
    doc.commit();

    let mut older = desk.clone();
    older.tree.workspaces[0].strips[0]
        .window_groups
        .push(group(11, &[110]));
    older.records.push((110, "sheet".into()));
    older.authors.insert(110, "linus".to_owned());
    assert!(shared_record::write(&mut doc, &older).expect("written"));
    doc.commit();

    assert!(desktop_doc::holds(&doc), "still the desk's own document");
    let held = opened(&doc);
    assert_eq!(windows(&held.tree), windows(&older.tree));
    assert_eq!(held.records, older.records);
    assert_eq!(held.authors, older.authors);
}

/// Two peers each change the desk and merge: the desk holds both changes,
/// by both of them, and each peer reads the same desk.
#[test]
fn two_peers_each_change_the_desk_and_keep_both_changes() {
    let desk = shared(desk());
    let mut local = AutoCommit::new();
    assert!(shared_record::write(&mut local, &desk).expect("written"));
    assert!(desktop_doc::write_shared(
        &mut local,
        &desk.tree,
        &desk.records,
        &desk.authors
    ));
    local.commit();
    let mut peer = local.fork();

    let mut mine = desk.clone();
    mine.tree.workspaces[0].name = "Studio · mine".into();
    assert!(shared_record::write(&mut local, &mine).expect("written"));
    local.commit();

    let mut theirs = desk.clone();
    theirs.tree.workspaces[0].strips[0]
        .window_groups
        .push(group(11, &[110]));
    theirs.authors.insert(110, "grace".to_owned());
    assert!(shared_record::write(&mut peer, &theirs).expect("written"));
    peer.commit();

    local.merge_incoming(&mut peer).expect("merged");
    peer.merge_incoming(&mut local).expect("merged");
    for desk in [opened(&local), opened(&peer)] {
        assert_eq!(
            desk.tree.workspaces[0].name, "Studio · mine",
            "the rename both peers made is one desk"
        );
        assert_eq!(windows(&desk.tree), windows(&theirs.tree));
        assert_eq!(
            desk.authors.get(&110).map(String::as_str),
            Some("grace"),
            "who opened the window came with it"
        );
    }
}
