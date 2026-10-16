//! Another device's focus becomes a top-bar offer; accepting it focuses the
//! same tab here, and nothing moves until the person accepts.

use std::collections::BTreeMap;
use std::time::Instant;

use automerge::{AutoCommit, ROOT};
use shell::shell::chrome_state::ChromeState;
use shell::shell::continuity_suggestion::{ContinuityFeed, FocusRecency};
use shell::shell::personal_continuity::{
    AppInstanceId, AppInstanceState, ContinuityStamp, DeviceContinuationSnapshot, DeviceFocus,
    DeviceId, LaunchIntent, PersonalContinuity, TabPlacement, WorkspaceContinuation,
};
use shell::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
use shell::shell::topbar_chrome::TopBarChrome;
use ui::a11y::Action;
use ui::widget::Widget;

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
    }
}

/// Terminal is focused here; Notes (mirrored from the laptop) sits unfocused
/// in the next group.
fn desktop() -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_default();
    let workspace = &mut tree.desktop.workspaces[0];
    let workspace_id = workspace.id;
    let strip = workspace.strips[0].id;
    workspace.strips[0].window_groups = vec![
        WindowGroup {
            id: 1,
            tabs: vec![tab(TERMINAL, "Terminal", "so.tilo.terminal")],
            active_tab: TERMINAL,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
        WindowGroup {
            id: 2,
            tabs: vec![tab(NOTES, "Notes", "so.tilo.notes")],
            active_tab: NOTES,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    ];
    tree.desktop.active = workspace_id;
    tree.desktop.active_strip.insert(workspace_id, strip);
    tree.desktop.set_active_group(strip, 1);
    tree
}

/// The laptop's published snapshot, focused on Notes.
fn laptop_focused_on_notes(tree: &WorkspaceTree, laptop: &DeviceId) -> PersonalContinuity {
    let room = tree.desktop.workspaces[0].room;
    let stamp = ContinuityStamp {
        counter: 12,
        device: laptop.clone(),
    };
    let notes = AppInstanceState::new(
        AppInstanceId(NOTES),
        TabPlacement {
            strip: 0,
            group: 1,
            tab: 0,
        },
        LaunchIntent::app("so.tilo.notes").unwrap(),
        None,
        stamp,
    );
    let snapshot = DeviceContinuationSnapshot::new(
        laptop.clone(),
        12,
        Some(DeviceFocus {
            room,
            app: Some(AppInstanceId(NOTES)),
        }),
        vec![WorkspaceContinuation {
            room,
            apps: vec![notes],
        }],
    )
    .unwrap();
    let mut continuity = PersonalContinuity::default();
    continuity.upsert(snapshot).unwrap();
    continuity
}

fn continuity_control(bar: &TopBarChrome) -> Option<String> {
    bar.accessibility()?
        .children
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some("topbar:continuity"))
        .and_then(|node| node.name)
}

fn focused_tab(chrome: &ChromeState) -> u64 {
    let tree = chrome.tree();
    let workspace = tree.desktop.active_workspace().unwrap();
    let strip = tree.desktop.active_strip[&workspace.id];
    let group = tree.desktop.active_group[&strip];
    let tab = workspace.strips[0]
        .window_groups
        .iter()
        .find(|candidate| candidate.id == group)
        .unwrap()
        .active_tab;
    drop(tree);
    tab
}

#[test]
fn peer_focus_offers_a_chip_and_accepting_it_moves_focus_here() {
    let tree = desktop();
    let laptop = DeviceId::new("laptop-device").unwrap();
    let mut recency = FocusRecency::default();
    recency.observe(&laptop_focused_on_notes(&tree, &laptop), Instant::now());

    let chrome = ChromeState::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree.desktop)
        .with_chrome(shell::shell::topbar_chrome::native_source(chrome.clone()));
    assert_eq!(continuity_control(&bar), None, "no offer before the feed");

    chrome.borrow_mut().set_continuity_feed(ContinuityFeed {
        local_device: Some(DeviceId::new("desk-device").unwrap()),
        observations: recency.observations(),
        devices: BTreeMap::from([(laptop, "Laptop".to_string())]),
    });
    assert_eq!(
        focused_tab(&chrome.borrow()),
        TERMINAL,
        "an arriving offer never steals focus"
    );
    assert_eq!(
        continuity_control(&bar).as_deref(),
        Some("Continue Notes from Laptop")
    );

    let mut doc = AutoCommit::new();
    assert!(bar.perform_semantic_action(
        "topbar:continuity",
        &Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &ROOT
    ));
    assert_eq!(focused_tab(&chrome.borrow()), NOTES);
    assert_eq!(continuity_control(&bar), None, "an accepted offer retires");

    // Moving back to Terminal here does not resurrect the accepted offer.
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.set_active_group(strip, 1);
    }
    assert_eq!(continuity_control(&bar), None);
}

#[test]
fn dismissing_the_chip_leaves_focus_alone() {
    let tree = desktop();
    let laptop = DeviceId::new("laptop-device").unwrap();
    let mut recency = FocusRecency::default();
    recency.observe(&laptop_focused_on_notes(&tree, &laptop), Instant::now());
    let chrome = ChromeState::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree.desktop)
        .with_chrome(shell::shell::topbar_chrome::native_source(chrome.clone()));
    chrome.borrow_mut().set_continuity_feed(ContinuityFeed {
        local_device: None,
        observations: recency.observations(),
        devices: BTreeMap::from([(laptop, "Laptop".to_string())]),
    });
    assert!(continuity_control(&bar).is_some());

    let mut doc = AutoCommit::new();
    assert!(bar.perform_semantic_action(
        "topbar:continuity-dismiss",
        &Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &ROOT
    ));
    assert_eq!(continuity_control(&bar), None);
    assert_eq!(focused_tab(&chrome.borrow()), TERMINAL);
}
