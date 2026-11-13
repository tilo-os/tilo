//! What a peer's window becomes here, and what keeps it that way.
//!
//! Asserted against `shared_record::device::fold`, the function every
//! device folds its workspace's layout record with, rather than against a
//! helper written to agree with it.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_record::{self, device};
use tilo_desktop_model::{
    AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceSlot,
};

const APP: &str = "so.tilo.word-processor";

/// Somebody else's tab, as the record holds it.
fn remote_tab(title: &str, app: &str) -> Tab {
    Tab {
        id: 7,
        title: title.into(),
        tile: TileContent::Foreign {
            app_id: app.into(),
            surface_id: 0,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

fn group_with(tab: Tab) -> WindowGroup {
    let active_tab = tab.id;
    WindowGroup {
        id: 1,
        tabs: vec![tab],
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

fn workspace(identity: WorkspaceId, tab: Tab) -> Workspace {
    let mut workspace =
        Workspace::in_workspace(WorkspaceSlot::of_workspace(identity), identity, "Home");
    let mut strip = Strip::new(StripId(2), "Main");
    strip.window_groups.push(group_with(tab));
    workspace.strips.push(strip);
    workspace
}

/// The record of `workspace` holding `tab`, as a peer wrote it.
fn record(identity: WorkspaceId, tab: Tab) -> AutoCommit {
    let workspace = workspace(identity, tab);
    let mut doc = AutoCommit::new();
    shared_record::write(
        &mut doc,
        &SharedDesktop {
            tree: DesktopTree {
                active: workspace.slot,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            ..SharedDesktop::default()
        },
    )
    .expect("written");
    doc
}

/// Fold `doc` into a local workspace holding `local`; the tab it shows, and
/// whether the fold changed anything.
fn fold(identity: WorkspaceId, doc: &AutoCommit, local: Tab) -> (Tab, bool) {
    let mut here = workspace(identity, local);
    let changed = device::fold(doc, &mut here, "here", &device::Kept::NOTHING);
    (here.strips[0].window_groups[0].tabs[0].clone(), changed)
}

#[test]
fn a_peers_window_arrives_as_its_app_without_clobbering_local_instances() {
    let identity = WorkspaceId::new();
    let doc = record(identity, remote_tab("Word Processor", APP));
    // Phase `a_peers_window_arrives_as_the_app_it_names`.
    // A peer's `Foreign` names a surface on *their* compositor. What
    // survives is the app id, which means the same thing on both machines
    // and is what a local instance can be started from.
    {
        let (tab, _) = fold(
            identity,
            &doc,
            Tab {
                id: 7,
                title: String::new(),
                tile: TileContent::Stub {
                    kind: AppKind::Other(String::new()),
                    label: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            },
        );
        match &tab.tile {
            TileContent::LocalApp {
                app_id,
                label,
                note,
            } => {
                assert_eq!(app_id, APP);
                assert_eq!(label, "Word Processor", "the peer's own name for it");
                assert!(note.is_empty(), "nobody has answered for it yet");
            }
            other => panic!("expected a window to open here, got {other:?}"),
        }
    }
    // Phase `a_local_instance_is_not_clobbered_by_the_peers_own_surface`.
    // Once bound, the tile names *our* surface; a naive "the record wins"
    // fold would take the window away again on every merge.
    {
        let (tab, changed) = fold(
            identity,
            &doc,
            Tab {
                id: 7,
                title: "Word Processor".into(),
                // What `bind_into_tab` left here after our instance mapped.
                tile: TileContent::Foreign {
                    app_id: APP.into(),
                    surface_id: 99,
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            },
        );
        match &tab.tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, APP);
                assert_eq!(
                    *surface_id, 99,
                    "the tab must keep hosting this machine's own window"
                );
            }
            other => panic!("expected our own surface, got {other:?}"),
        }
        assert!(!changed, "a fold that changed nothing must not say it did");
    }
    // Phase `a_different_app_in_the_tab_still_replaces_what_is_here`.
    // A different app taking the tab over *is* news, and is taken.
    {
        let doc = record(identity, remote_tab("Terminal", "so.tilo.terminal"));
        let (tab, changed) = fold(
            identity,
            &doc,
            Tab {
                id: 7,
                title: "Word Processor".into(),
                tile: TileContent::Foreign {
                    app_id: APP.into(),
                    surface_id: 99,
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            },
        );
        match &tab.tile {
            TileContent::LocalApp { app_id, .. } => assert_eq!(app_id, "so.tilo.terminal"),
            other => panic!("expected the new app, got {other:?}"),
        }
        assert!(changed);
    }
}
