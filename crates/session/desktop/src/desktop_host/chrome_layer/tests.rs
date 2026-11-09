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
    WorkspaceSlot,
};
use tilo_desktop_surface::DesktopSurface;

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

/// The overview's Revoke on an agent's own row reaches the host, which is
/// where the card's Revoke goes too: the person's root signs the device's
/// revocation from there.
#[test]
fn an_agents_row_revokes_through_the_doors_own_path() {
    en();
    let mut chrome = chrome_with_an_agent();
    let pick = chrome
        .overview
        .tilo_target("overview-agent:g1:action")
        .expect("the agent's row offers to take its grant back");
    assert_eq!(pick, OverviewPick::RevokeGrant("g1".into()));
    chrome.apply_pick(&pick);
    assert_eq!(
        chrome.take_asks(),
        [DesktopAsk::RevokeAgentGrant("g1".into())]
    );
}
