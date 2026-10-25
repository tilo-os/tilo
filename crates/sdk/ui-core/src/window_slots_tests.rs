//! A desktop's window slots: declarations, routing and the windows of apps
//! in the host's own process.

use super::*;
use crate::window_toolbar::{ToolbarAction, ToolbarLocation};

fn location(label: &str) -> WindowToolbar {
    WindowToolbar {
        sidebar: None,
        location: Some(ToolbarLocation {
            label: label.into(),
            editable: true,
            ..ToolbarLocation::default()
        }),
        ..WindowToolbar::default()
    }
}

#[test]
fn events_go_back_to_the_app_that_declared_the_tab_and_die_with_it() {
    let mut toolbars = WindowSlots::default();
    assert!(toolbars.declare(7, 41, Some(location("example.org"))));
    assert!(!toolbars.declare(7, 41, Some(location("example.org"))));
    assert!(toolbars.route(
        7,
        ToolbarEvent::Commit {
            text: "rust".into()
        }
    ));
    assert!(
        !toolbars.route(8, ToolbarEvent::Back),
        "no app declared tab 8"
    );
    assert_eq!(
        toolbars.take_outbox(),
        vec![(
            41,
            ToolbarEvent::Commit {
                text: "rust".into()
            }
        )]
    );
    assert!(toolbars.disconnected(41));
    assert!(toolbars.toolbar(7).is_none());
    assert_eq!(toolbars.cleared, vec![7]);
}

#[test]
fn the_bars_search_and_history_go_to_the_app_that_offered_its_window() {
    use crate::window_toolbar::{HistoryOffer, SearchOffer};
    let mut toolbars = WindowSlots::default();
    let offer = WindowOffer {
        search: SearchOffer::Document,
        count: None,
        history: Some(HistoryOffer::Closed),
        ..WindowOffer::default()
    };
    // Notes declares no slots, only what its window offers the bar.
    assert!(toolbars.offer(3, 52, offer.clone()));
    assert!(
        !toolbars.offer(3, 52, offer),
        "an equal offer is nothing new"
    );
    assert!(toolbars.toolbar(3).is_none());
    for event in [
        ToolbarEvent::Search {
            query: "cat".into(),
        },
        ToolbarEvent::SearchStep { forward: true },
        ToolbarEvent::SearchClose,
        ToolbarEvent::History,
    ] {
        assert!(toolbars.route(3, event));
    }
    assert!(
        !toolbars.route(3, ToolbarEvent::Back),
        "slots nobody declared go nowhere"
    );
    let outbox = toolbars.take_outbox();
    assert_eq!(outbox.len(), 4);
    assert!(outbox.iter().all(|(connection, _)| *connection == 52));
    assert!(toolbars.disconnected(52));
    assert!(toolbars.window_offer(3).is_none());
    assert_eq!(toolbars.offers_cleared, vec![3]);
    assert!(!toolbars.route(3, ToolbarEvent::History));
}

fn new_contact() -> WindowToolbar {
    WindowToolbar {
        actions: vec![ToolbarAction::new("new", "plus", "New contact")],
        ..WindowToolbar::default()
    }
}

fn group(tabs: &[TabId]) -> WindowGroup {
    use crate::widget::{TabDescriptor, TileContent, WindowWidth};
    let tiles = tabs
        .iter()
        .map(|id| {
            (
                TabDescriptor::new(*id, format!("Tab {id}")),
                TileContent::Notice {
                    title: String::new(),
                    detail: String::new(),
                },
            )
        })
        .collect();
    WindowGroup::new(1, tiles, tabs[0], WindowWidth::Normal)
}

#[test]
fn apps_sharing_a_thread_each_declare_on_their_own_window() {
    let contacts = HostedWindow::new();
    let mail = HostedWindow::new();
    assert!(!crate::window_toolbar::is_hosted());
    {
        let _running = crate::window_toolbar::enter(Some(&contacts));
        assert!(crate::window_toolbar::is_hosted(), "a bar shows its slots");
        crate::window_toolbar::declare(Some(new_contact()));
        {
            let _running = crate::window_toolbar::enter(Some(&mail));
            crate::window_toolbar::declare(Some(location("Inbox")));
        }
        assert_eq!(crate::window_toolbar::declared(), Some(new_contact()));
    }
    assert_eq!(contacts.declared(), Some(new_contact()));
    assert_eq!(mail.declared(), Some(location("Inbox")));
    assert_eq!(crate::window_toolbar::declared(), None, "the thread's own");
    assert!(!crate::window_toolbar::is_hosted());

    contacts.deliver(ToolbarEvent::Action { id: "new".into() });
    assert!(crate::window_toolbar::take_events().is_empty());
    let _running = crate::window_toolbar::enter(Some(&contacts));
    assert_eq!(
        crate::window_toolbar::take_events(),
        vec![ToolbarEvent::Action { id: "new".into() }]
    );
}

#[test]
fn a_hosted_windows_slots_reach_its_bar_and_presses_reach_its_app() {
    let window = HostedWindow::new();
    let mut groups = [group(&[5])];
    let mut slots = WindowSlots::default();
    window.declare(Some(new_contact()));
    let windows = [(5, window.clone())];
    let sync = slots.sync_hosted(&windows, groups.iter_mut());
    assert!(sync.changed);
    assert_eq!(groups[0].tab_toolbar(5), Some(&new_contact()));
    assert!(slots.route(5, ToolbarEvent::Action { id: "new".into() }));
    assert!(slots.take_outbox().is_empty(), "no connection carries it");
    assert!(slots.sync_hosted(&windows, groups.iter_mut()).changed);
    assert_eq!(
        window.take_events(),
        vec![ToolbarEvent::Action { id: "new".into() }]
    );
    // Its tile went (a rebuild gives the tab a fresh window): the old
    // window's slots leave the bar.
    let sync = slots.sync_hosted(&[], groups.iter_mut());
    assert!(sync.changed);
    assert_eq!(groups[0].tab_toolbar(5), None);
}

/// The desktop's own "Share to channel…" ends the menu of a window whose
/// app has a bar, and choosing it reaches the desktop, never the app.
#[test]
fn the_desktops_own_menu_items_join_an_apps_bar_and_come_back_to_the_desktop() {
    let window = HostedWindow::new();
    let bare = HostedWindow::new();
    let mut groups = [group(&[5]), group(&[6])];
    let mut slots = WindowSlots::default();
    window.declare(Some(new_contact()));
    let share = ToolbarAction::new("tilo.desktop.share", "screen-share", "Share to channel…");
    slots.set_host_items(5, vec![share.clone()]);
    slots.set_host_items(6, vec![share.clone()]);
    slots.set_host_items(5, vec![ToolbarAction::new("own", "x", "Not the desktop's")]);
    assert!(!slots.host_items.contains_key(&5), "only the desktop's ids");
    slots.set_host_items(5, vec![share]);
    let windows = [(5, window.clone()), (6, bare.clone())];
    slots.sync_hosted(&windows, groups.iter_mut());
    let shown = groups[0].tab_toolbar(5).expect("its bar");
    assert_eq!(shown.actions.len(), 2);
    assert!(
        shown.actions[1].menu,
        "in the overflow menu, after the app's"
    );
    assert!(
        groups[1]
            .tab_toolbar(6)
            .is_some_and(WindowToolbar::menu_only),
        "a window without a bar carries the menu alone"
    );

    assert!(slots.route(
        5,
        ToolbarEvent::Action {
            id: "tilo.desktop.share".into()
        }
    ));
    slots.sync_hosted(&windows, groups.iter_mut());
    assert!(window.take_events().is_empty(), "the app never hears it");
    assert_eq!(
        slots.take_host_actions(),
        vec![(5, "tilo.desktop.share".to_owned())]
    );

    slots.set_host_items(5, Vec::new());
    slots.sync_hosted(&windows, groups.iter_mut());
    assert_eq!(groups[0].tab_toolbar(5), Some(&new_contact()));
}

/// A window whose app declared no bar still offers the desktop's own items:
/// they are its only slots, a menu alone, which leaves its title where it
/// is. Choosing one reaches the desktop as from a bar, and taking them away
/// leaves the window bare again.
#[test]
fn a_window_without_a_bar_offers_the_desktops_items_in_a_menu_alone() {
    let bare = HostedWindow::new();
    let mut groups = [group(&[6])];
    let mut slots = WindowSlots::default();
    let windows = [(6, bare.clone())];
    slots.sync_hosted(&windows, groups.iter_mut());
    assert_eq!(groups[0].tab_toolbar(6), None);

    let share = ToolbarAction::new(
        "tilo.desktop.share-to-channel",
        "screen-share",
        "Share to channel…",
    );
    slots.set_host_items(6, vec![share]);
    slots.sync_hosted(&windows, groups.iter_mut());
    let shown = groups[0].tab_toolbar(6).expect("the menu");
    assert!(shown.menu_only(), "{shown:?}");
    assert_eq!(shown.actions.len(), 1);
    assert_eq!(shown.actions[0].id, "tilo.desktop.share-to-channel");
    assert!(shown.location.is_none(), "the title stays in its tab");

    assert!(slots.route(
        6,
        ToolbarEvent::Action {
            id: "tilo.desktop.share-to-channel".into()
        }
    ));
    slots.sync_hosted(&windows, groups.iter_mut());
    assert!(bare.take_events().is_empty(), "the app never hears it");
    assert_eq!(
        slots.take_host_actions(),
        vec![(6, "tilo.desktop.share-to-channel".to_owned())]
    );

    slots.set_host_items(6, Vec::new());
    slots.sync_hosted(&windows, groups.iter_mut());
    assert_eq!(groups[0].tab_toolbar(6), None, "bare again");
}
