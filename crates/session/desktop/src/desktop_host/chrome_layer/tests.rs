//! What the overview of the person's Tilo asks of the host: the rows it shows
//! are the host's answers, and a row's control is the ask that answer's own
//! path takes.

#![allow(clippy::unwrap_used, clippy::panic, reason = "a test's assertions")]

use std::rc::Rc;

use tilo_actions::remote::agent_door::{Door, Grant, Reach, Run};

use super::{DesktopAsk, DesktopChrome};
use crate::about::{AboutLinks, AboutVariant, TiloMenu};
use crate::desktop_host::{NoticeTiles, PermissiveRules};
use crate::overview_panel::OverviewPick;
use crate::overview_tilo::TiloDesk;
use crate::topbar::TopBarChrome;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceSlot,
};
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn desk_tree() -> DesktopTree {
    let tabs = vec![Tab {
        id: 20,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The chrome over a desk, its overview open on a door that let an agent in
/// and is working now.
fn chrome_with_an_agent() -> DesktopChrome {
    let tree = desk_tree();
    let (_, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle)),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_tilo(TiloDesk {
        door: Some(Door {
            endpoint: "https://mcp.tilo.sh".into(),
            grants: vec![Grant {
                id: "g1".into(),
                agent: "Claude Code".into(),
                reach: Reach::Account,
                runs: vec![Run {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }],
                ..Grant::default()
            }],
            ..Door::default()
        }),
        ..TiloDesk::default()
    });
    chrome.toggle_overview();
    chrome
}

/// The chrome over a desk whose person is in its one workspace, and the desk's handle.
fn chrome_over_a_desk() -> (DesktopChrome, DesktopHandle) {
    let tree = desk_tree();
    let (_, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    (chrome, handle)
}

/// The workspace in front and every workspace on the desk, by id.
fn front_and_held(handle: &DesktopHandle) -> (WorkspaceSlot, Vec<WorkspaceId>) {
    handle.read(|tree| {
        (
            tree.active,
            tree.workspaces.iter().map(Workspace::id).collect(),
        )
    })
}

/// A workspace put on the desk in the background stands after the others and the person stays
/// where they are: one their list brings from another of their devices arrives while they are in
/// another, and a window they open then is that one's. Asked for again in the background it
/// changes nothing; asked for without it, the person goes there, as when they join one.
#[test]
fn a_workspace_put_on_the_desk_in_the_background_leaves_the_person_where_they_are() {
    en();
    let (mut chrome, handle) = chrome_over_a_desk();
    let arrived = WorkspaceId::new();
    let put = |background| ChromeEffect::NewWorkspace {
        workspace: Some(arrived.to_string()),
        name: Some("Later".into()),
        background,
    };

    assert_eq!(chrome.perform(&put(Some(true))), Ok(true));
    let (front, held) = front_and_held(&handle);
    assert!(held.contains(&arrived), "it is on the desk: {held:?}");
    assert_eq!(front, WorkspaceSlot(1), "the person stays where they were");

    assert_eq!(chrome.perform(&put(Some(true))), Ok(true));
    let (front, again) = front_and_held(&handle);
    assert_eq!(again, held, "a workspace held already is not made twice");
    assert_eq!(front, WorkspaceSlot(1), "and still nobody went anywhere");

    assert_eq!(chrome.perform(&put(None)), Ok(true));
    let (front, joined) = front_and_held(&handle);
    assert_eq!(
        joined, held,
        "the workspace the person joins is the one held"
    );
    assert_eq!(
        front,
        WorkspaceSlot::of_workspace(arrived),
        "a person who joins a workspace lands in it"
    );
}

/// A workspace that arrives on a desk showing none is the one the desk shows, in the background or
/// not: there is nowhere else for the person to be.
#[test]
fn a_workspace_arriving_on_a_desk_that_shows_none_is_shown() {
    en();
    let (mut chrome, handle) = chrome_over_a_desk();
    handle.replace_tree(DesktopTree::default());
    let arrived = WorkspaceId::new();
    assert_eq!(
        chrome.perform(&ChromeEffect::NewWorkspace {
            workspace: Some(arrived.to_string()),
            name: Some("Later".into()),
            background: Some(true),
        }),
        Ok(true)
    );
    let (front, held) = front_and_held(&handle);
    assert_eq!(held, [arrived]);
    assert_eq!(front, WorkspaceSlot::of_workspace(arrived));
}

/// The overview's Revoke on an agent's own row reaches the host, which is
/// where the card's Revoke goes too: the person's root signs the device's
/// revocation from there. It asks first, and only the answer revokes.
#[test]
fn an_agents_row_revokes_through_the_doors_own_path() {
    en();
    let mut chrome = chrome_with_an_agent();
    assert!(
        chrome.overview.activate("overview-agent:g1:action"),
        "the agent's row offers to take its grant back"
    );
    assert_eq!(
        chrome.overview.take_pick(),
        None,
        "the press that asks takes nothing back"
    );
    assert!(chrome.overview.activate("overview-agent:g1:action"));
    let pick = chrome
        .overview
        .take_pick()
        .expect("the answer to its question is the grant's");
    assert_eq!(pick, OverviewPick::RevokeGrant("g1".into()));
    chrome.apply_pick(&pick);
    assert_eq!(
        chrome.take_asks(),
        [DesktopAsk::RevokeAgentGrant("g1".into())]
    );
}

/// The agent's row is the agent, not only its Revoke: a press on it opens the card its face opens
/// anywhere else, in Console, beside the work.
#[test]
fn an_agents_row_opens_the_agents_card() {
    en();
    let mut chrome = chrome_with_an_agent();
    assert!(chrome.overview.activate("overview-agent:g1"));
    let pick = chrome.overview.take_pick().expect("the row opens the card");
    assert_eq!(pick, OverviewPick::OpenAgent("Claude Code".into()));
    chrome.apply_pick(&pick);
    assert_eq!(
        chrome.take_asks(),
        [DesktopAsk::OpenAgent("Claude Code".into())]
    );
}
