//! What the overview of the person's Tilo asks of the host: the rows it shows
//! are the host's answers, and a row's control is the ask that answer's own
//! path takes.

use std::rc::Rc;

use tilo_actions::remote::mcp_server::{AgentSession, Grant, Reach, Server};

use super::{DesktopAsk, DesktopChrome};
use crate::about::{AboutLinks, AboutSheet, AboutVariant};
use crate::desktop_host::{NoticeTiles, PermissiveRules};
use crate::overview_panel::OverviewPick;
use crate::overview_tilo::TiloOverview;
use crate::topbar::TopBarChrome;
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, HostAction, HostActionSource};
use tilo_session_core::desktop::navigation::ChromeEffect;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

/// An action a session publishes for the desktop's menus and search, of `source`.
fn action(id: &str, title: &str, source: HostActionSource) -> HostAction {
    HostAction {
        id: id.into(),
        title: title.into(),
        label: None,
        subtitle: None,
        icon: None,
        destructive: false,
        unasked: false,
        source: Some(source),
    }
}

fn layout_tree() -> Layout {
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
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The chrome over a shell, its overview open on an API that let an agent in
/// and is working now.
fn chrome_with_an_agent() -> DesktopChrome {
    let tree = layout_tree();
    let (_, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle)),
        TopBarChrome::from_tree(&tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    chrome.set_tilo(TiloOverview {
        server: Some(Server {
            endpoint: "https://mcp.tilo.sh".into(),
            grants: vec![Grant {
                id: "g1".into(),
                agent: "Claude Code".into(),
                reach: Reach::Account,
                sessions: vec![AgentSession {
                    name: "Claude Code #2".into(),
                    started_ms: 1,
                }],
                ..Grant::default()
            }],
            ..Server::default()
        }),
        ..TiloOverview::default()
    });
    chrome.toggle_overview();
    chrome
}

/// The chrome over a shell whose person is in its one workspace, and the shell's handle.
fn chrome_over_a_shell() -> (DesktopChrome, DesktopHandle) {
    chrome_over(&layout_tree())
}

/// [`chrome_over_a_shell`] over `tree` as the test hands it.
fn chrome_over(tree: &Layout) -> (DesktopChrome, DesktopHandle) {
    let (_, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    (chrome, handle)
}

/// The workspace in front and every workspace on the shell, by id.
fn front_and_held(handle: &DesktopHandle) -> (WorkspaceId, Vec<WorkspaceId>) {
    handle.read(|tree| {
        (
            tree.active,
            tree.workspaces.iter().map(Workspace::id).collect(),
        )
    })
}

/// A workspace put on the shell in the background stands after the others and the person stays
/// where they are: one their list brings from another of their devices arrives while they are in
/// another, and a window they open then is that one's. Asked for again in the background it
/// changes nothing; asked for without it, the person goes there, as when they join one.
#[test]
fn a_workspace_put_on_the_shell_in_the_background_leaves_the_person_where_they_are() {
    en();
    let (mut chrome, handle) = chrome_over_a_shell();
    let arrived = WorkspaceId::new();
    let put = |background| ChromeEffect::NewWorkspace {
        workspace: Some(arrived.to_string()),
        name: Some("Later".into()),
        background,
    };

    assert_eq!(chrome.perform(&put(Some(true))), Ok(true));
    let (front, held) = front_and_held(&handle);
    assert!(held.contains(&arrived), "it is on the shell: {held:?}");
    assert_eq!(
        front,
        WorkspaceId::from_u128(1),
        "the person stays where they were"
    );

    assert_eq!(chrome.perform(&put(Some(true))), Ok(true));
    let (front, again) = front_and_held(&handle);
    assert_eq!(again, held, "a workspace held already is not made twice");
    assert_eq!(
        front,
        WorkspaceId::from_u128(1),
        "and still nobody went anywhere"
    );

    assert_eq!(chrome.perform(&put(None)), Ok(true));
    let (front, joined) = front_and_held(&handle);
    assert_eq!(
        joined, held,
        "the workspace the person joins is the one held"
    );
    assert_eq!(front, arrived, "a person who joins a workspace lands in it");
}

/// Naming the workspace in front asks the host to write the name where the
/// workspace keeps it, its governance, so every device and list of it names it
/// the same; the shell keeps no name of its own and shows the governance's,
/// projected onto it, until the governance takes the new one.
#[test]
fn renaming_the_workspace_asks_the_host_to_keep_the_name() {
    en();
    let (mut chrome, handle) = chrome_over_a_shell();
    let workspace = front_and_held(&handle).0;
    let named = handle.read(|tree| tree.active_workspace().map(|held| held.name.clone()));
    let rename = ChromeEffect::Rename {
        tab: None,
        group: None,
        strip: None,
        workspace: Some(workspace.to_string()),
        name: "Tilo".into(),
    };
    assert_eq!(chrome.perform(&rename), Ok(true));
    assert_eq!(
        handle.read(|tree| tree.active_workspace().map(|held| held.name.clone())),
        named
    );
    assert_eq!(
        chrome.take_asks(),
        vec![DesktopAsk::RenamedWorkspace {
            workspace,
            name: "Tilo".into()
        }]
    );
}

/// A workspace that arrives on a shell showing none is the one the shell shows, in the background or
/// not: there is nowhere else for the person to be.
#[test]
fn a_workspace_arriving_on_a_shell_that_shows_none_is_shown() {
    en();
    let (mut chrome, handle) = chrome_over_a_shell();
    handle.replace_tree(Layout::default());
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
    assert_eq!(front, arrived);
}

/// The overview's Revoke on an agent's own row reaches the host, which is
/// where the card's Revoke goes too: the person's root signs the device's
/// revocation from there. It asks first, and only the answer revokes.
#[test]
fn an_agents_row_revokes_through_the_apis_own_path() {
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

/// The host's surfaces open the overview through the flag they share with the chrome: the panel
/// opens on the workspace the person is in, with its strip and window, and closes the same way.
/// What the panel does itself is told back on the flag.
#[test]
fn the_overview_the_host_opens_shows_the_workspace_in_front() {
    en();
    let (mut chrome, _handle) = chrome_over_a_shell();
    let open = Rc::new(std::cell::Cell::new(false));
    chrome.set_overview(crate::window_flight::WindowFlight::new(), Rc::clone(&open));

    open.set(true);
    assert!(chrome.follow_overview());
    assert!(chrome.overview.is_open());
    let cards = &chrome.overview.shell().cards;
    let here = cards.iter().find(|card| card.is_current).unwrap();
    assert_eq!(here.id, WorkspaceId::from_u128(1));
    assert_eq!(here.strips[0].groups[0].tabs[0].title, "Notes");

    open.set(false);
    assert!(chrome.follow_overview());
    assert!(!chrome.overview.is_open());

    chrome.toggle_overview();
    assert!(!chrome.follow_overview(), "the panel moved itself");
    assert!(open.get(), "and says so");
}

/// A strip nobody named is named by where it stands, as the bar's switch and the overview name
/// it: the shell reaches it by that word, and by its id, and by no name it was never given.
#[test]
fn a_strip_nobody_named_is_reached_by_the_word_the_shell_reads() {
    en();
    let mut tree = layout_tree();
    tree.workspaces[0].strips[0].name = String::new();
    let (chrome, _handle) = chrome_over(&tree);

    assert_eq!(
        chrome.strip_named("Strip 1"),
        Ok(StripId(2)),
        "the first strip nobody named is Strip 1"
    );
    assert_eq!(
        chrome.strip_named("strip 1"),
        Ok(StripId(2)),
        "in any case the person types it"
    );
    assert_eq!(
        chrome.strip_named("2"),
        Ok(StripId(2)),
        "and its decimal id reaches it"
    );
    assert!(
        chrome.strip_named("Main").is_err(),
        "the name it kept before the person named it is not its name"
    );
}

/// A confirmation in passing is said politely over the windows, wakes the host for its fade and
/// end, and is no notification.
#[test]
fn a_confirmation_passes_over_the_windows_without_a_notification() {
    use ui::widget::Widget as _;
    let (chrome, handle) = chrome_over_a_shell();
    let said = |chrome: &DesktopChrome| {
        chrome
            .children()
            .iter()
            .filter_map(|layer| layer.accessibility())
            .find_map(|node| node.live.zip(node.value))
    };
    assert_eq!(
        said(&chrome),
        Some((ui::a11y::Politeness::Polite, String::new())),
        "present and silent before anything is said"
    );
    handle.acknowledge("Link copied");
    assert_eq!(
        said(&chrome),
        Some((ui::a11y::Politeness::Polite, "Link copied".to_owned()))
    );
    assert!(chrome.needs_redraw());
    assert!(chrome.next_wake_deadline().is_some());
}

/// The About sheet is a surface of the stack: while it is up it holds the keyboard, so Escape
/// reaches it and closes it, and search opening over the shell takes it down.
#[test]
fn the_about_sheet_holds_the_keys_and_goes_on_escape_or_search() {
    use crate::overlays::{Part, Target};
    en();
    let (chrome, _handle) = chrome_over_a_shell();
    let overlays = chrome.overlays().clone();
    let mut harness = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::theme::Theme::default(),
    );
    let open = |harness: &mut ui::headless::FrameHarness| {
        let mut chrome = harness.root_mut::<DesktopChrome>();
        chrome
            .perform(&ChromeEffect::About { open: Some(true) })
            .unwrap();
        chrome.publish_overlays();
    };

    open(&mut harness);
    harness.poll_frame().unwrap();
    assert_eq!(overlays.target(), Target::Surface(Part::About));
    harness.key("Escape");
    harness.poll_frame().unwrap();
    assert!(!harness.root::<DesktopChrome>().about.is_about_open());
    assert_eq!(
        overlays.target(),
        Target::Window,
        "the window has the keys back"
    );

    open(&mut harness);
    overlays.open(Part::Search, None);
    harness.root_mut::<DesktopChrome>().publish_overlays();
    assert!(!harness.root::<DesktopChrome>().about.is_about_open());
    assert!(!overlays.is_open(Part::About));
}

/// A computer's sheet leads to its Settings pages: the one pressed opens and the sheet goes.
#[test]
fn the_about_sheets_updates_link_opens_settings() {
    en();
    let (chrome, _handle) = chrome_over_a_shell();
    let mut harness = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::theme::Theme::default(),
    );
    {
        let mut chrome = harness.root_mut::<DesktopChrome>();
        chrome
            .perform(&ChromeEffect::About { open: Some(true) })
            .unwrap();
        chrome.publish_overlays();
    }
    harness.poll_frame().unwrap();
    harness.click_named("Updates").unwrap();
    harness.poll_frame().unwrap();
    let mut chrome = harness.root_mut::<DesktopChrome>();
    assert_eq!(
        chrome.take_asks(),
        [DesktopAsk::OpenSettings("settings://updates".into())]
    );
    assert!(!chrome.about.is_about_open());
}

/// The menu behind the bar's workspace name is the switcher: every workspace of the desktop, the
/// one shown marked, then the person's others the host publishes as `Workspaces` actions by their
/// name, then the ways to another one. Nothing about the person or what the host offers besides.
#[test]
fn the_workspace_menu_offers_the_persons_other_workspaces() {
    en();
    let (mut chrome, handle) = chrome_over_a_shell();
    handle.set_host_actions(vec![
        HostAction {
            label: Some("Tilo".into()),
            ..action(
                "open-workspace:https://tilo.sh/abc",
                "Open Tilo",
                HostActionSource::Workspaces,
            )
        },
        action(
            "new-workspace",
            "New workspace",
            HostActionSource::Workspace,
        ),
        action(
            "save-recovery-key",
            "Save your recovery key",
            HostActionSource::Identity,
        ),
    ]);
    let model = chrome.workspace_menu_model(crate::workspace_menu::MenuPlace::Bar);
    let ids: Vec<&str> = model
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["new-workspace"],
        "the switcher offers the ways to another workspace, and only those"
    );
    let mut menu = crate::workspace_menu::WorkspaceMenu::new();
    menu.toggle(
        &model,
        ui::kurbo::Rect::new(0.0, 0.0, 80.0, 24.0),
        ui::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0),
    );
    let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
    let shown = handle.read(|tree| {
        tree.active_workspace()
            .map(|workspace| tilo_session_core::desktop::names::workspace(&workspace.name))
    });
    assert_eq!(menu.marked(), shown, "the workspace shown is marked");
    assert!(
        labels.iter().any(|label| label == "Tilo"),
        "another workspace is listed by its name: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("recovery")),
        "nothing about the person: {labels:?}"
    );
    // Choosing it is the host's action, as joining is.
    assert!(menu.choose("Tilo"));
    assert_eq!(
        menu.take_choice(),
        Some(crate::workspace_menu::WorkspaceMenuChoice::Action(
            "open-workspace:https://tilo.sh/abc".into()
        ))
    );
    // The overview lists the same workspaces, from the same actions.
    chrome.toggle_overview();
    let elsewhere = chrome.overview().tilo.elsewhere;
    assert_eq!(
        elsewhere,
        [(
            "open-workspace:https://tilo.sh/abc".to_owned(),
            "Open Tilo".to_owned()
        )]
    );
}

/// The person's own desktop is offered above their workspaces and apart from them, with its own
/// picture: it is their private place, not another workspace, and the overview never lists it as
/// one.
#[test]
fn the_persons_own_desktop_is_offered_apart_from_their_workspaces() {
    en();
    let (mut chrome, handle) = chrome_over_a_shell();
    handle.set_host_actions(vec![
        HostAction {
            label: Some("Your desktop".into()),
            icon: Some("user".into()),
            ..action(
                "open-workspace:own",
                "Open your desktop",
                HostActionSource::Desktop,
            )
        },
        HostAction {
            label: Some("Tilo".into()),
            ..action(
                "open-workspace:https://tilo.sh/abc",
                "Open Tilo",
                HostActionSource::Workspaces,
            )
        },
    ]);
    let model = chrome.workspace_menu_model(crate::workspace_menu::MenuPlace::Bar);
    assert_eq!(
        model
            .own_desktop
            .as_ref()
            .map(|own| (own.id.as_str(), own.icon)),
        Some(("open-workspace:own", Some(ui::icons::Icon::User))),
        "the person's own desktop, with the person's own picture"
    );
    assert!(
        model
            .workspaces
            .iter()
            .all(|entry| entry.name != "Your desktop"),
        "never among the workspaces"
    );
    let mut menu = crate::workspace_menu::WorkspaceMenu::new();
    menu.toggle(
        &model,
        ui::kurbo::Rect::new(0.0, 0.0, 80.0, 24.0),
        ui::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0),
    );
    let rows = menu.labels();
    assert_eq!(
        (
            rows.first().cloned().flatten().as_deref(),
            rows.get(1).cloned().flatten()
        ),
        (Some("Open your desktop"), None),
        "above the workspaces, with a rule between: {rows:?}"
    );
    assert!(menu.choose("Open your desktop"));
    assert_eq!(
        menu.take_choice(),
        Some(crate::workspace_menu::WorkspaceMenuChoice::Action(
            "open-workspace:own".into()
        ))
    );
    // The overview lists workspaces elsewhere, never the person's own desktop.
    chrome.toggle_overview();
    assert_eq!(
        chrome.overview().tilo.elsewhere,
        [(
            "open-workspace:https://tilo.sh/abc".to_owned(),
            "Open Tilo".to_owned()
        )]
    );
}

/// The overview's ground is the shell's wallpaper, not the flat fallback: the chrome draws it
/// through the one source the session hands every host, so the picture the backdrop under it shows
/// is the picture the overview draws over — blurred and dimmed as the scrim says — though the
/// chrome is a host of its own and never draws the backdrop.
#[test]
fn the_overviews_ground_is_the_wallpapers_and_not_the_fallback() {
    en();
    // Two displays, so the overview stands on the one the person works on and has an area of its
    // own: without one it would be drawn nowhere and this test would prove nothing.
    let mut tree = layout_tree();
    tree.outputs.reconcile(
        &tree.workspaces,
        &[
            tilo_desktop_model::partition::Display {
                id: 0,
                monitor: "panel".into(),
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: false,
            },
            tilo_desktop_model::partition::Display {
                id: 1,
                monitor: "monitor".into(),
                x: 1280.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: false,
            },
        ],
    );
    tree.outputs.set_focused(1);
    let (mut chrome, _handle) = chrome_over(&tree);
    let wallpaper = ui::backdrop::wallpaper::Source::new();
    chrome.set_wallpaper(wallpaper.clone());
    let theme = ui::Theme::dark();
    let draw = |chrome: &mut DesktopChrome| {
        use ui::widget::DrawChild as _;
        let mut scene = ui::vello::Scene::new();
        chrome.draw_child(
            &mut scene,
            ui::kurbo::Affine::IDENTITY,
            &mut ui::TextRenderer::new(),
            &theme,
            &ui::presence::PresenceCtx::default(),
        );
        scene
    };
    // The picture the ground draws is one image patch; the flat fallback draws a gradient and no
    // patch at all, whatever else the chrome shows.
    let image_fills = |scene: &ui::vello::Scene| {
        scene
            .encoding()
            .resources
            .patches
            .iter()
            .filter(|patch| matches!(patch, ui::vello_encoding::Patch::Image { .. }))
            .count()
    };
    let bare = image_fills(&draw(&mut chrome));
    chrome.toggle_overview();
    assert_eq!(
        image_fills(&draw(&mut chrome)),
        bare,
        "a shell with no wallpaper stands on its ground"
    );
    // A picture a host decodes once, as the session's is.
    let mut pixels = Vec::new();
    for y in 0..64_u32 {
        for x in 0..64_u32 {
            let v = if (x / 8 + y / 8) % 2 == 0 { 10 } else { 240 };
            pixels.extend_from_slice(&[v, v / 2, 255 - v, 255]);
        }
    }
    let picture = ui::raster::rgba8(pixels, 64, 64);
    wallpaper.set(Some(picture));
    assert_eq!(
        image_fills(&draw(&mut chrome)),
        bare + 1,
        "the overview draws the shell's wallpaper, and the one it reads is the backdrop's"
    );
}

/// The strips ribbon's own controls are named where they stand: the ribbon at rest offers New
/// strip by name, and the labels Super+S opens name each strip, so a harness or an agent presses
/// the one it sees rather than a guessed point.
#[test]
fn the_strips_ribbon_names_each_strip_where_it_stands() {
    en();
    let (chrome, _handle) = chrome_over_a_shell();
    let mut harness = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        chrome,
        (1280, 800),
        ui::theme::Theme::default(),
    );
    harness.relayout();
    assert!(
        harness
            .targets()
            .keys()
            .any(|name| name.starts_with("strip_switcher")),
        "the ribbon is named at rest: {:?}",
        harness.targets().keys().collect::<Vec<_>>()
    );
    harness
        .root_mut::<DesktopChrome>()
        .fixed_shortcut(ui::shortcut_table::FixedAction::StripSwitcher);
    harness.relayout();
    let targets = harness.targets();
    assert!(
        targets.contains_key("strip_switcher.strip.1"),
        "the open ribbon names its strip: {:?}",
        targets.keys().collect::<Vec<_>>()
    );
}
