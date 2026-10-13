//! What a peer's window becomes here, and what keeps it that way.
//!
//! Asserted against `projection::merge_group`, the function the
//! room-sync path actually runs, rather than against a helper written
//! to agree with it.

use tilo_desktop_model::projection::{ReplicatedGroup, ReplicatedTab, merge_group};
use tilo_desktop_model::{AppKind, Tab, TileContent, WindowGroup, WindowWidth};

const APP: &str = "so.tilo.word-processor";

/// Somebody else's tab, as the room document delivers it.
fn remote_tab(id: u64, surface: u64) -> ReplicatedTab {
    ReplicatedTab {
        id,
        order: 0,
        title: "Word Processor".into(),
        tile: Some(TileContent::Foreign {
            app_id: APP.into(),
            surface_id: surface,
        }),
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

#[test]
fn a_peers_window_arrives_as_its_app_without_clobbering_local_instances() {
    // Phase `a_peers_window_arrives_as_the_app_it_names`.
    // A peer's `Foreign` names a surface on *their* compositor. Kept
    // verbatim it would resolve here to nothing, or to an unrelated
    // window of ours; what survives instead is the app id, which means
    // the same thing on both machines and is what a local instance can be
    // started from.
    {
        let local = group_with(Tab {
            id: 7,
            title: String::new(),
            tile: TileContent::Stub {
                kind: AppKind::Other(String::new()),
                label: String::new(),
            },
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            tabs: vec![remote_tab(7, 4242)],
        };
        let mut changed = false;
        let merged = merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::PeerWindow {
                app_id,
                label,
                note,
                ..
            } => {
                assert_eq!(app_id, APP);
                assert_eq!(label, "Word Processor", "the peer's own name for it");
                assert!(note.is_empty(), "nobody has answered for it yet");
            }
            other => panic!("expected a peer window, got {other:?}"),
        }
    }
    // Phase `a_local_instance_is_not_clobbered_by_the_peers_own_surface`.
    // The binding survives the peer's next projection: once bound, the
    // tile names *our* surface while the peer keeps re-projecting
    // theirs, so a naive "the document wins" merge would take the
    // window away again every sync interval, forever.
    {
        let local = group_with(Tab {
            id: 7,
            title: "Word Processor".into(),
            // What `bind_into_tab` left here after our instance mapped.
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 99,
            },
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            // The peer's surface id, which is not ours.
            tabs: vec![remote_tab(7, 4242)],
        };
        let mut changed = false;
        let merged = merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::Foreign { app_id, surface_id } => {
                assert_eq!(app_id, APP);
                assert_eq!(
                    *surface_id, 99,
                    "the tab must keep hosting this machine's own window"
                );
            }
            other => panic!("expected our own surface, got {other:?}"),
        }
        assert!(!changed, "a merge that changed nothing must not say it did");
    }
    // Phase `a_different_app_in_the_tab_still_replaces_what_is_here`.
    // A different app taking the tab over *is* news, and is taken.
    // The rule is "a local binding for the same app wins", not "whatever
    // is here wins" — the second would strand the tab on a window whose
    // tab now holds something else.
    {
        let local = group_with(Tab {
            id: 7,
            title: "Word Processor".into(),
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 99,
            },
        });
        let mut rep_tab = remote_tab(7, 4242);
        rep_tab.title = "Terminal".into();
        rep_tab.tile = Some(TileContent::Foreign {
            app_id: "so.tilo.terminal".into(),
            surface_id: 4242,
        });
        let rep = ReplicatedGroup {
            id: local.id,
            order: 0,
            tabs: vec![rep_tab],
        };
        let mut changed = false;
        let merged = merge_group(local, rep, &|_| false, &mut changed);
        match &merged.tabs[0].tile {
            TileContent::PeerWindow { app_id, .. } => assert_eq!(app_id, "so.tilo.terminal"),
            other => panic!("expected the new app, got {other:?}"),
        }
        assert!(changed);
    }
}
