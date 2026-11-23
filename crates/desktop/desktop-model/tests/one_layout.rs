//! A workspace's layout is one document: the record an older build wrote opens as
//! that document, a build that knows only the old record keeps writing into
//! it, and peers that each change the layout keep both changes.
//!
//! Asserted through [`shared_resource::read`], the function native and pages
//! both read a workspace's layout with, and [`desktop_doc::rebuild`], what the
//! layout's own document projects.

use automerge::{AutoCommit, ReadDoc as _};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::desktop_doc;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_resource;
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
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
fn layout() -> Layout {
    let mut workspace = Workspace::new(identity(), "Studio");
    workspace.policy.set_owner("ada");
    let mut strip = Strip::new(StripId(1), "A");
    strip.window_groups = vec![group(10, &[100, 101])];
    workspace.strips = vec![strip];
    Layout {
        workspaces: vec![workspace],
        active: WorkspaceId::from_u128(1),
        ..Layout::default()
    }
}

/// The layout as a workspace holds it, with one window on a record and two people
/// who opened windows of it.
fn shared(tree: Layout) -> SharedDesktop {
    SharedDesktop {
        tree,
        records: vec![(100, "notes".into())],
        items: Vec::new(),
        authors: [(100, "ada".to_owned()), (101, "grace".to_owned())].into(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// The windows of the first workspace, as `(title, tab)`.
fn windows(tree: &Layout) -> Vec<(u64, String)> {
    tree.workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab.title.clone()))
        .collect()
}

fn read_back(doc: &AutoCommit) -> Layout {
    let mut fresh = Layout::default();
    desktop_doc::rebuild(doc, &mut fresh);
    fresh
}

fn opened(doc: &AutoCommit) -> SharedDesktop {
    shared_resource::read(doc, None).expect("a layout")
}

tilo_test_env::scenario! {
    fn the_shell_is_one_document;

    phase an_old_shared_record_opens_as_the_shells_own_document;
    // The record older builds wrote — a workspace, strip, group and window as
    // an entry each at the record's root — opens as the layout's one document
    // with its windows, records and people, and the entries it wrote stop
    // being what the record holds.
    {
        let held = shared(layout());
        let mut doc = AutoCommit::new();
        assert!(shared_resource::write(&mut doc, &held).expect("written"));
        doc.commit();
        assert!(
            !desktop_doc::holds(&doc),
            "an older build writes the record, entry by entry"
        );

        let as_written = opened(&doc);
        assert_eq!(windows(&as_written.tree), windows(&held.tree));
        assert_eq!(as_written.records, held.records);
        assert_eq!(as_written.authors, held.authors);

        assert!(desktop_doc::write_shared(
            &mut doc,
            &as_written.tree,
            &as_written.records,
            &as_written.items,
            &as_written.authors
        ));
        doc.commit();
        assert!(desktop_doc::holds(&doc), "the layout's own document now");
        for key in ["strips", "groups", "windows"] {
            assert!(
                doc.get(automerge::ROOT, key).ok().flatten().is_none(),
                "the old record's `{key}` entries are gone"
            );
        }

        let reopened = opened(&doc);
        assert_eq!(windows(&reopened.tree), windows(&held.tree));
        assert_eq!(reopened.records, held.records);
        assert_eq!(reopened.authors, held.authors);
        assert_eq!(
            reopened.tree.workspaces[0].policy.owner(),
            Some("ada"),
            "who owns the workspace came with it"
        );
        assert_eq!(windows(&read_back(&doc)), windows(&held.tree));

        // Read as of an earlier change, the layout is the layout as it was then:
        // what the activity pane steps back through.
        let then = doc.get_heads();
        let mut renamed = held.tree.clone();
        renamed.workspaces[0].strips[0].name = "A · renamed".into();
        assert!(shared_resource::write(&mut doc, &shared(renamed)).expect("written"));
        doc.commit();
        assert_eq!(
            shared_resource::read(&doc, Some(&then))
                .expect("the layout then")
                .tree
                .workspaces[0]
                .strips[0]
                .name,
            "A"
        );
        assert_eq!(
            opened(&doc).tree.workspaces[0].strips[0].name,
            "A · renamed",
            "and now it is the layout as it is"
        );
    }

    phase a_build_from_before_writes_into_the_shell_document;
    // A peer that knows only the old record — a build from before the layout was
    // one document — writes into the layout's document rather than beside it, so
    // both keep working on the same layout.
    {
        let held = shared(layout());
        let mut doc = AutoCommit::new();
        assert!(shared_resource::write(&mut doc, &held).expect("written"));
        assert!(desktop_doc::write_shared(
            &mut doc,
            &held.tree,
            &held.records,
            &held.items,
            &held.authors
        ));
        doc.commit();

        let mut older = held.clone();
        older.tree.workspaces[0].strips[0]
            .window_groups
            .push(group(11, &[110]));
        older.records.push((110, "sheet".into()));
        older.authors.insert(110, "linus".to_owned());
        assert!(shared_resource::write(&mut doc, &older).expect("written"));
        doc.commit();

        assert!(desktop_doc::holds(&doc), "still the layout's own document");
        let held = opened(&doc);
        assert_eq!(windows(&held.tree), windows(&older.tree));
        assert_eq!(held.records, older.records);
        assert_eq!(held.authors, older.authors);
    }

    phase a_person_who_only_looks_keeps_their_view_to_themselves;
    // A person whose role is Viewer brings another tab of a window group in front and gives the
    // group their own width. Nothing of that reaches the workspace's layout, which carries no
    // view of it; their own view keeps it.
    {
        use tilo_action_policy::Role;
        use tilo_desktop_model::commands::policy;
        use tilo_desktop_model::person_view::PersonView;

        let held = shared(layout());
        let SharedDesktop { tree, records, authors, .. } = held;
        let mut doc = AutoCommit::new();
        // A page of an older build wrote the layout and how it shows it into one document.
        desktop_doc::seed(&mut doc, &tree);
        doc.commit();
        assert!(
            desktop_doc::write_shared(&mut doc, &tree, &records, &[], &authors),
            "the layout sheds the view the older page put in it"
        );
        doc.commit();
        for key in ["view_v1", "active"] {
            assert!(
                doc.get(automerge::ROOT, key).ok().flatten().is_none(),
                "the workspace's layout holds no `{key}`"
            );
        }

        let mut theirs_tree = tree.clone();
        theirs_tree.workspaces[0].strips[0].window_groups[0].active_tab = 101;
        theirs_tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Normal;
        theirs_tree.workspaces[0].strips[0].window_groups[0].maximized = false;

        assert!(
            !policy::writes(Some(Role::Viewer)),
            "a viewer writes nothing in the workspace"
        );
        assert!(
            !desktop_doc::write_shared(&mut doc, &theirs_tree, &records, &[], &authors),
            "changing their view writes nothing of the workspace's layout"
        );

        // Their own view keeps what they changed: the other tab in front, the width theirs.
        let theirs = PersonView::of(&theirs_tree, identity());
        let mut view = AutoCommit::new();
        assert!(theirs.write(&mut view).expect("the view is kept"));
        assert_eq!(PersonView::read(&view), theirs);
        assert_eq!(theirs.front_tab.get(&10), Some(&101));
        assert_eq!(theirs.widths.get(&10), Some(&WindowWidth::Normal));
        assert_eq!(
            windows(&opened(&doc).tree),
            windows(&tree),
            "every member still shows the layout as the workspace has it"
        );
    }

    phase a_page_reloaded_in_a_workspace_still_shows_that_workspace;
    // A page in a workspace keeps the workspace's layout as its profile's copy, and a reload
    // opens the page from that copy. The workspace's layout names no workspace in front — that is
    // how each person shows it — so the reloaded page shows the one workspace it holds, the one
    // it showed before the reload.
    {
        let mut tree = layout();
        tree.active = identity();
        let held = shared(tree);
        let mut workspace = AutoCommit::new();
        desktop_doc::seed(&mut workspace, &held.tree);
        workspace.commit();
        shared_resource::write(&mut workspace, &held).expect("the workspace keeps its layout");
        workspace.commit();
        assert!(
            workspace.get(automerge::ROOT, "active").ok().flatten().is_none(),
            "the workspace's layout names no workspace in front"
        );

        // The reload: the profile's bytes, read as the page's mount reads them.
        let reloaded = AutoCommit::load(&workspace.save()).expect("the profile's copy loads");
        let mut shown = shared_resource::read(&reloaded, None)
            .expect("the reloaded page holds the workspace's layout")
            .tree;
        desktop_doc::rebuild_view(&reloaded, &mut shown);
        assert_eq!(
            shown.active_workspace().map(Workspace::id),
            Some(identity()),
            "the reloaded page shows the workspace it showed"
        );
        assert_eq!(windows(&shown), windows(&held.tree), "with its windows");
    }

    phase two_peers_each_change_the_shell_and_keep_both_changes;
    // Two peers each change the layout and merge: the layout holds both changes,
    // by both of them, and each peer reads the same layout.
    {
        let held = shared(layout());
        let mut local = AutoCommit::new();
        assert!(shared_resource::write(&mut local, &held).expect("written"));
        assert!(desktop_doc::write_shared(
            &mut local,
            &held.tree,
            &held.records,
            &held.items,
            &held.authors
        ));
        local.commit();
        let mut peer = local.fork();

        let mut mine = held.clone();
        mine.tree.workspaces[0].strips[0].name = "A · mine".into();
        assert!(shared_resource::write(&mut local, &mine).expect("written"));
        local.commit();

        let mut theirs = held.clone();
        theirs.tree.workspaces[0].strips[0]
            .window_groups
            .push(group(11, &[110]));
        theirs.authors.insert(110, "grace".to_owned());
        assert!(shared_resource::write(&mut peer, &theirs).expect("written"));
        peer.commit();

        local.merge_incoming(&mut peer).expect("merged");
        peer.merge_incoming(&mut local).expect("merged");
        for held in [opened(&local), opened(&peer)] {
            assert_eq!(
                held.tree.workspaces[0].strips[0].name, "A · mine",
                "the rename both peers made is one layout"
            );
            assert_eq!(windows(&held.tree), windows(&theirs.tree));
            assert_eq!(
                held.authors.get(&110).map(String::as_str),
                Some("grace"),
                "who opened the window came with it"
            );
        }
    }
}
