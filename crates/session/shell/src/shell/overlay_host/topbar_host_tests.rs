use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use tilo_desktop::chrome::ClockSnapshot;

fn host_over(chrome: &crate::shell::chrome_state::Chrome) -> ShellOverlayHost {
    ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone())
}

fn demo() -> (ShellOverlayHost, crate::shell::chrome_state::Chrome) {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    (host_over(&chrome), chrome)
}

/// The feed as it would be published now, whether or not it changed.
fn feed(host: &mut ShellOverlayHost, chrome: &crate::shell::chrome_state::Chrome) -> BarFeed {
    host.topbar.published = None;
    host.topbar_feed(chrome).expect("a feed")
}

/// One frame's update of the host, as the compositor runs it.
fn frame(host: &mut ShellOverlayHost) {
    let doc = automerge::AutoCommit::new();
    host.update(&doc, &automerge::ROOT);
}

#[test]
fn a_fixture_owns_the_clock_and_a_live_desktop_reads_the_wall() {
    let parent = tempfile::tempdir().unwrap();
    let inputs = crate::shell::hosted_inputs::HostedTileInputs::fixture_in(parent.path()).unwrap();
    let chrome =
        ChromeState::new_with_fixture_handle(WorkspaceTree::bootstrap_default().shared(), inputs);
    let mut host = host_over(&chrome);
    assert_eq!(
        feed(&mut host, &chrome).clock,
        Some(ClockSnapshot::default())
    );

    let (mut live, chrome) = demo();
    if std::env::var_os("TILO_CLOCK").is_none() {
        assert_eq!(
            feed(&mut live, &chrome).clock,
            None,
            "the bar reads the wall clock"
        );
    }
}

#[test]
fn each_fixture_pins_its_own_clock_on_every_feed() {
    let parent = tempfile::tempdir().unwrap();
    for (unix_seconds, display) in [(1_700_000_000, "09:41"), (1_700_000_060, "09:42")] {
        let clock = ClockSnapshot {
            unix_seconds,
            clock: display.into(),
            weekday: "Tuesday".into(),
            date: "14 November".into(),
        };
        let inputs = crate::shell::hosted_inputs::HostedTileInputs::fixture_in(parent.path())
            .unwrap()
            .with_fixture_clock(clock.clone())
            .unwrap();
        let chrome = ChromeState::new_with_fixture_handle(
            WorkspaceTree::bootstrap_default().shared(),
            inputs,
        );
        let mut host = host_over(&chrome);
        for _ in 0..3 {
            assert_eq!(feed(&mut host, &chrome).clock, Some(clock.clone()));
        }
    }
}

#[test]
fn the_feed_is_published_again_only_when_the_chrome_changed() {
    let (mut host, chrome) = demo();
    assert!(host.topbar_feed(&chrome).is_some());
    assert!(host.topbar_feed(&chrome).is_none(), "nothing changed");
    chrome.borrow_mut().bump();
    assert!(host.topbar_feed(&chrome).is_some());
}

#[test]
fn the_feed_reports_what_chrome_holds() {
    let (mut host, chrome) = demo();
    let feed = feed(&mut host, &chrome);
    assert_eq!(
        feed.snapshot.status.battery_percent,
        chrome.borrow().quick_settings.status.battery_percent()
    );
    assert_eq!(feed.tree.active, chrome.borrow().tree().desktop.active);
    assert_eq!(
        feed.workspace_menu.workspaces.len(),
        chrome.borrow().tree().desktop.workspaces.len()
    );
}

#[test]
fn asks_take_the_compositors_own_mutation_paths() {
    let (mut host, chrome) = demo();
    assert!(host.topbar_intent(&chrome, ChromeIntent::ToggleOverview));
    assert!(chrome.borrow().overlays.overview);
    assert!(host.topbar_intent(&chrome, ChromeIntent::ShowQuickSettings));
    assert!(chrome.borrow().overlays.quick_settings);
    assert!(host.topbar_intent(&chrome, ChromeIntent::ToggleCollaborators));
    assert!(chrome.borrow().overlays.collaborators);
    assert!(!chrome.borrow().overlays.room_activity);
    assert!(
        !host.topbar_intent(&chrome, ChromeIntent::ToggleTiloMenu),
        "the desktop's bar carries no Tilo mark"
    );
}

/// A click on the strip's name in the top bar asks for the strip
/// switcher, not the overview.
#[test]
fn the_strip_name_asks_for_the_strip_switcher() {
    let (mut host, chrome) = demo();
    assert!(host.topbar_intent(&chrome, ChromeIntent::ShowStrips));
    assert!(!chrome.borrow().overlays.overview, "not the overview");
    assert!(chrome.borrow_mut().take_strip_switcher_request());
    assert!(
        !chrome.borrow_mut().take_strip_switcher_request(),
        "one request, taken once"
    );
}

#[test]
fn the_floating_clock_is_clocks_live_activity_and_its_controls_go_back() {
    let board = crate::shell::live_activities::board();
    let (mut host, chrome) = demo();
    board.show(
        bar_data::CLOCKS_APP,
        Some(serde_json::json!({
            "active": true,
            "activity_id": "timer-1",
            "label": "Tea",
            "remaining_secs": 90,
            "stopwatch": false,
            "overlay": true,
        })),
    );
    let shown = feed(&mut host, &chrome);
    let activity = shown.snapshot.clock_activity.expect("the timer is shown");
    assert_eq!(activity.label, "Tea");
    assert_eq!(activity.remaining_secs, 90);
    assert!(shown.snapshot.overlays.clock, "it floats");

    assert!(host.topbar_intent(
        &chrome,
        ChromeIntent::ClockCommand {
            command: "toggle".into(),
            activity_id: Some("timer-1".into()),
        }
    ));
    board.show(bar_data::CLOCKS_APP, None);
    // A fresh poll, as a later frame takes it.
    host.topbar.clock = ClockPoll::default();
    assert!(
        feed(&mut host, &chrome).snapshot.clock_activity.is_none(),
        "it ended"
    );
    assert!(!chrome.borrow().overlays.clock);
}

#[test]
fn without_displays_the_bar_runs_along_the_desktops_top_edge() {
    let (host, _chrome) = demo();
    // Without displays the bar runs along the desktop's own top edge.
    assert_eq!(host.topbar_placement(), Placement::default());
}

#[test]
fn the_frame_mounts_the_bar_and_carries_out_what_it_opens() {
    let (mut host, chrome) = demo();
    frame(&mut host);
    assert!(host.topbar.app.is_some(), "the bar is mounted on the frame");
    chrome.borrow_mut().overlays.collaborators = true;
    host.toggle_workspace_menu();
    frame(&mut host);
    assert!(
        !chrome.borrow().overlays.collaborators,
        "People closes for the bar's menu"
    );
    assert!(host.workspace_menu_labels().is_some(), "the menu is open");
    assert!(host.topbar_owns_keyboard(), "its keys are the menu's");

    // Quick Settings opening over the bar closes its menu.
    chrome.borrow_mut().open_quick_settings();
    frame(&mut host);
    frame(&mut host);
    assert!(host.workspace_menu_labels().is_none());
}

#[test]
fn a_choice_in_the_workspace_menu_takes_the_shells_path() {
    let (mut host, chrome) = demo();
    frame(&mut host);
    host.toggle_workspace_menu();
    frame(&mut host);
    assert!(host.choose_in_workspace_menu("Overview"));
    frame(&mut host);
    assert!(chrome.borrow().overlays.overview);
}

mod continuity {
    //! Another device's focus becomes the bar's offer; accepting it focuses
    //! the same tab here, and nothing moves until the person accepts.

    use super::*;
    use crate::shell::continuation::AppInstanceId;
    use crate::shell::continuity_suggestion::{ContinuityFeed, DeviceFocus, FocusRecency};
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};
    use std::collections::BTreeMap;
    use tilo_network_actor::fabric::DeviceId;

    const TERMINAL: u64 = 91_001;
    const NOTES: u64 = 91_002;

    fn tab(id: u64, title: &str, app_id: &str) -> Tab {
        Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: app_id.into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
        }
    }

    fn group(id: u64, tab: Tab) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// Terminal is focused here; Notes (mirrored from the laptop) sits
    /// unfocused in the next group.
    fn desktop() -> WorkspaceTree {
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let workspace_id = workspace.id;
        let strip = workspace.strips[0].id;
        workspace.strips[0].window_groups = vec![
            group(1, tab(TERMINAL, "Terminal", "so.tilo.terminal")),
            group(2, tab(NOTES, "Notes", "so.tilo.notes")),
        ];
        tree.desktop.active = workspace_id;
        tree.desktop.active_strip.insert(workspace_id, strip);
        tree.desktop.set_active_group(strip, 1);
        tree
    }

    /// The laptop is on Notes, seen after it first looked.
    fn laptop_on_notes(chrome: &crate::shell::chrome_state::Chrome, local: Option<DeviceId>) {
        let laptop = DeviceId::new("laptop-device").unwrap();
        let focus = vec![DeviceFocus {
            device: laptop.clone(),
            room: chrome.borrow().tree().desktop.workspaces[0].room(),
            app: AppInstanceId(NOTES),
            withdrawn: false,
        }];
        let mut recency = FocusRecency::default();
        recency.observe(&[], WallInstant::now());
        recency.observe(&focus, WallInstant::now());
        chrome.borrow_mut().set_continuity_feed(ContinuityFeed {
            local_device: local,
            observations: recency.observations(),
            devices: BTreeMap::from([(laptop, "Laptop".to_string())]),
        });
    }

    fn focused_tab(chrome: &crate::shell::chrome_state::Chrome) -> u64 {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().unwrap();
        let strip = tree.desktop.active_strip[&workspace.id];
        let group = tree.desktop.active_group[&strip];
        workspace.strips[0]
            .window_groups
            .iter()
            .find(|candidate| candidate.id == group)
            .unwrap()
            .active_tab
    }

    #[test]
    fn peer_focus_offers_the_bar_a_chip_and_accepting_it_moves_focus_here() {
        let chrome = ChromeState::new(desktop());
        let mut host = host_over(&chrome);
        assert_eq!(
            feed(&mut host, &chrome).continuity,
            None,
            "no offer before the feed"
        );
        laptop_on_notes(&chrome, Some(DeviceId::new("desk-device").unwrap()));
        assert_eq!(
            focused_tab(&chrome),
            TERMINAL,
            "an arriving offer never steals focus"
        );
        assert_eq!(
            feed(&mut host, &chrome).continuity.as_deref(),
            Some("Continue Notes from Laptop")
        );
        assert!(host.topbar_intent(&chrome, ChromeIntent::AcceptContinuity));
        assert_eq!(focused_tab(&chrome), NOTES);
        assert_eq!(
            feed(&mut host, &chrome).continuity,
            None,
            "an accepted offer retires"
        );
        // Moving back to Terminal here does not resurrect the accepted offer.
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let strip = tree.desktop.workspaces[0].strips[0].id;
            tree.desktop.set_active_group(strip, 1);
        }
        assert_eq!(feed(&mut host, &chrome).continuity, None);
    }

    #[test]
    fn dismissing_the_chip_leaves_focus_alone() {
        let chrome = ChromeState::new(desktop());
        let mut host = host_over(&chrome);
        laptop_on_notes(&chrome, None);
        assert!(feed(&mut host, &chrome).continuity.is_some());
        assert!(host.topbar_intent(&chrome, ChromeIntent::DismissContinuity));
        assert_eq!(feed(&mut host, &chrome).continuity, None);
        assert_eq!(focused_tab(&chrome), TERMINAL);
    }
}
