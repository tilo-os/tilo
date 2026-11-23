//! The desktop draws the parts its apps declare in their windows' bars.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{AppKind, AppWidget, TaffyTreeExt as _, TileContent, TileHost, Widget};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use crate::surface::{DesktopSurface, TileSource};

/// An app as the apps moved to the window bar's parts are written: it declares its parts with
/// [`ui::window_toolbar::declare`] and reads what the person did with
/// [`ui::window_toolbar::take_events`], knowing nothing of the process it shares with the desktop
/// and the other apps.
struct PartApp {
    action: &'static str,
    heard: Rc<RefCell<Vec<ToolbarEvent>>>,
}

impl Widget for PartApp {
    fn update(&mut self, _: &ui::widget::AppModel, _: &ui::widget::ModelPath) -> bool {
        ui::window_toolbar::declare(Some(WindowToolbar {
            actions: vec![ToolbarAction::new(self.action, "plus", self.action)],
            ..WindowToolbar::default()
        }));
        let events = ui::window_toolbar::take_events();
        let heard = !events.is_empty();
        self.heard.borrow_mut().extend(events);
        heard
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &ui::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                flex_grow: 1.0,
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn name(&self) -> &'static str {
        "PartApp"
    }
}

impl AppWidget for PartApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        self.action
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("C")
    }
}

/// Every window's app, each with its own action, and what each heard.
struct PartApps(Vec<(&'static str, Rc<RefCell<Vec<ToolbarEvent>>>)>);

impl TileSource for PartApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        let index = usize::try_from(tab.id % 2).unwrap_or(0);
        let (action, heard) = self.0[index].clone();
        TileHost::new(TileContent::App(Box::new(PartApp { action, heard })))
    }
}

/// Two apps in the desktop's own process, as every app of the browser desktop runs: each one's
/// parts stand in its own window's bar, and a press on one reaches that app and no other.
#[test]
fn each_apps_parts_stand_in_its_windows_bar_and_a_press_reaches_it() {
    let mut minter = IdMinter::new(7);
    let tabs: Vec<Tab> = (0..2)
        .map(|i| Tab {
            id: minter.mint(),
            title: format!("App{i}"),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.app{i}"),
                label: format!("App{i}"),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
        .collect();
    let apps: Vec<(&'static str, Rc<RefCell<Vec<ToolbarEvent>>>)> =
        vec![("new-contact", Rc::default()), ("compose", Rc::default())];
    let action_of = |tab: u64| apps[usize::try_from(tab % 2).unwrap_or(0)].0;
    let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Office", tabs);
    let (surface, _handle) = DesktopSurface::new(tree, 7, Box::new(PartApps(apps.clone())));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let shown: Vec<(u64, Option<WindowToolbar>, ui::kurbo::Rect)> = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the strip is shown")
        .groups
        .iter()
        .map(|group| {
            let tab = group.active_tab_id();
            let (_, _, _, actions) = group.bar_for_tests().toolbar_rects();
            (
                tab,
                group.tab_toolbar(tab).cloned(),
                actions.first().copied().unwrap_or(ui::kurbo::Rect::ZERO),
            )
        })
        .collect();
    assert_eq!(shown.len(), 2, "both windows are shown");
    for (tab, toolbar, button) in &shown {
        let toolbar = toolbar.as_ref().expect("the app's parts are in its bar");
        assert_eq!(toolbar.actions[0].id, action_of(*tab), "its own app's");
        assert!(button.area() > 0.0, "its action is a button in the bar");
    }

    let (tab, _, button) = shown[0];
    frame.click_at(button.center().x, button.center().y);
    for _ in 0..3 {
        frame.relayout();
    }
    let pressed = action_of(tab);
    for (action, heard) in &apps {
        let expected = if *action == pressed {
            vec![ToolbarEvent::Action {
                id: pressed.to_owned(),
            }]
        } else {
            Vec::new()
        };
        assert_eq!(*heard.borrow(), expected, "what {action}'s app heard");
    }
}

/// A press on a window bar's part, released where it went down with nothing after it (a person who
/// clicks and holds still), reaches the window's app in the frames the page asks for: the apps ran
/// their pass before the bar was read, so the pass that hands the press over asks for the one that
/// hears it, rather than leaving it for the next pointer move or clock tick.
#[test]
fn a_bar_press_with_nothing_after_it_reaches_its_app_in_the_frames_asked_for() {
    let mut minter = IdMinter::new(7);
    let tabs = vec![Tab {
        id: minter.mint(),
        title: "Tasks".into(),
        tile: tilo_desktop_model::TileContent::LocalApp {
            app_id: "so.tilo.tasks".into(),
            label: "Tasks".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
    let heard: Rc<RefCell<Vec<ToolbarEvent>>> = Rc::default();
    let apps = vec![("back", Rc::clone(&heard)), ("back", Rc::clone(&heard))];
    let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Office", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(PartApps(apps)));
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    handle.set_wake({
        let asked = std::sync::Arc::clone(&asked);
        std::sync::Arc::new(move || {
            asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    });
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let button = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the strip is shown")
        .groups[0]
        .bar_for_tests()
        .toolbar_rects()
        .3
        .first()
        .copied()
        .expect("the app's part is a button in its bar");
    asked.store(0, std::sync::atomic::Ordering::Relaxed);

    frame.click_at(button.center().x, button.center().y);
    // Only the frames something asked for, as the page draws: no forced pass, no further input.
    for _ in 0..8 {
        if asked.swap(0, std::sync::atomic::Ordering::Relaxed) == 0 {
            break;
        }
        frame.woken();
    }
    assert_eq!(
        *heard.borrow(),
        vec![ToolbarEvent::Action { id: "back".into() }],
        "the app heard the press without another input or a forced frame"
    );
}

/// The tabs of the shown strip, with the id of the one each group shows.
fn shown_tabs(frame: &ui::headless::FrameHarness) -> Vec<(u64, u64)> {
    frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the strip is shown")
        .groups
        .iter()
        .map(|group| (group.active_tab_id(), group.tabs()[0].id))
        .collect()
}

/// An app in a process of its own declares its parts over its connection: the next pass shows them
/// in its window's bar, what the person did there goes back on that same connection, and the parts
/// go when it closes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test, from the declaration to the press it sends back"
)]
fn an_apps_own_process_declares_its_parts_and_hears_the_press_on_its_connection() {
    let (surface, handle) = crate::surface::tests::shell_with(2, Box::new(WithHistory));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let shown = shown_tabs(&frame);
    let (front, _) = shown[0];
    let toolbar = WindowToolbar {
        location: Some(ui::window_toolbar::ToolbarLocation {
            label: "plan.md".to_owned(),
            editable: true,
            ..ui::window_toolbar::ToolbarLocation::default()
        }),
        actions: vec![ToolbarAction::new("save", "plus", "Save")],
        ..WindowToolbar::default()
    };
    handle.declare_bar(front, 41, Some(toolbar));
    for _ in 0..3 {
        frame.relayout();
    }
    let (button, edited) = {
        let group = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("the strip is shown")
            .groups
            .iter()
            .find(|group| group.active_tab_id() == front)
            .expect("the window it declared for");
        assert_eq!(
            group
                .tab_toolbar(front)
                .map(|shown| shown.actions[0].id.clone()),
            Some("save".to_owned()),
            "the app's part stands in its window's bar"
        );
        let (_, _, _, actions) = group.bar_for_tests().toolbar_rects();
        (
            actions.first().copied().unwrap_or(ui::kurbo::Rect::ZERO),
            group.editing_location(),
        )
    };
    assert!(!edited, "nothing asked for the location yet");
    assert!(button.area() > 0.0, "the part is a button in the bar");

    // The window is named by what it shows: its app's location label names the tab, so its bar, the
    // overview and search say the same.
    let named = handle.read(|tree| tree.tab(front).map(|tab| tab.title.clone()));
    assert_eq!(
        named.as_deref(),
        Some("plan.md"),
        "the tab is called what its window shows"
    );
    assert_eq!(handle.window_name(front).as_deref(), Some("plan.md"));

    frame.click_at(button.center().x, button.center().y);
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(
        handle.take_bar_events(),
        vec![(
            front,
            41,
            ToolbarEvent::Action {
                id: "save".to_owned(),
            }
        )],
        "the press goes back on the connection that declared the part"
    );

    // The app asked for its location to be edited, as its own Ctrl+L does.
    handle.edit_bar_location(front);
    for _ in 0..3 {
        frame.relayout();
    }
    let editing = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the strip is shown")
        .groups
        .iter()
        .find(|group| group.active_tab_id() == front)
        .expect("the window it asked for")
        .editing_location();
    assert!(editing, "the location is a field now");

    // A tab behind the one its window shows is nobody's to edit.
    if let Some((_, behind)) = shown.get(1) {
        handle.edit_bar_location(*behind);
        for _ in 0..3 {
            frame.relayout();
        }
        let background = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("the strip is shown")
            .groups
            .iter()
            .find(|group| group.active_tab_id() == front)
            .expect("the window in front");
        assert!(
            background.editing_location(),
            "a tab behind stays as it was"
        );
    }

    // The app's connection closes: its parts go with it.
    handle.bar_connection_closed(41);
    for _ in 0..3 {
        frame.relayout();
    }
    let gone = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .expect("the strip is shown")
        .groups
        .iter()
        .find(|group| group.active_tab_id() == front)
        .expect("the window")
        .tab_toolbar(front)
        .cloned();
    assert_eq!(gone, None, "nothing of the app's is left in the bar");

    // An app names its window again and then names nothing — the note it showed is closed: the
    // window goes back to being called by its app, not by a place it has left.
    handle.declare_bar(
        front,
        43,
        Some(WindowToolbar {
            location: Some(ui::window_toolbar::ToolbarLocation {
                label: "Garden plan".to_owned(),
                ..ui::window_toolbar::ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }),
    );
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(handle.window_name(front).as_deref(), Some("Garden plan"));
    handle.declare_bar(front, 43, Some(WindowToolbar::default()));
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(handle.window_name(front), None, "it shows no place now");
    let back = handle.read(|tree| tree.tab(front).map(|tab| tab.title.clone()));
    assert_eq!(
        back.as_deref(),
        Some("App0"),
        "a window whose app names nothing is called by its app"
    );
}

/// A window's app describes the controls of its own content over its connection: the shell lists
/// them where they are drawn, a press goes back on that connection, and they go with it.
#[test]
fn a_windows_app_describes_its_controls_and_the_press_goes_back_on_its_connection() {
    use ui::window_targets::{TargetAction, WindowTarget};

    let (surface, handle) = crate::surface::tests::desktop(1);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (front, _) = shown_tabs(&frame)[0];

    // Nothing of a window's own content is named until its app says so.
    assert!(handle.window_controls().is_empty());

    handle.declare_targets(
        front,
        41,
        vec![WindowTarget {
            role: "ListItem".to_owned(),
            name: "Documents".to_owned(),
            identity: None,
            rect: [10.0, 20.0, 110.0, 40.0],
            actions: vec![TargetAction::Activate, TargetAction::ShowContextMenu],
            level: None,
            selected: false,
        }],
    );
    for _ in 0..3 {
        frame.relayout();
    }

    let content = handle
        .shown_tabs()
        .into_iter()
        .find(|(tab, _)| *tab == front)
        .map(|(_, content)| content)
        .expect("the window is drawn");
    let listed = handle.window_controls();
    let control = listed.first().expect("the window's control is listed");
    assert_eq!(control.name, "Documents");
    let (x, y, w, h) = control.rect;
    assert_eq!(
        (x, y),
        (content.x0 + 10.0, content.y0 + 20.0),
        "listed where it is drawn in its window"
    );
    assert_eq!((w, h), (100.0, 20.0));

    assert_eq!(
        handle.press_window_control("Documents", TargetAction::ShowContextMenu),
        Ok(())
    );
    assert_eq!(
        handle.take_target_presses(),
        vec![crate::surface::window_bars::TargetPress {
            connection: 41,
            tab: front,
            name: "Documents".to_owned(),
            role: "ListItem".to_owned(),
            identity: None,
            action: TargetAction::ShowContextMenu,
        }],
        "the press goes back on the connection that described the control"
    );

    // A control nobody drew, and one the app answers nothing of, is not pressed.
    assert!(
        handle
            .press_window_control("Nowhere", TargetAction::Activate)
            .is_err()
    );
    assert!(handle.take_target_presses().is_empty());

    // The app's connection closes: what it described goes with it.
    handle.bar_connection_closed(41);
    for _ in 0..3 {
        frame.relayout();
    }
    assert!(
        handle.window_controls().is_empty(),
        "nothing of the closed app's is listed"
    );
}

/// What an app in a process of its own asks of its window's bar is shell work
/// the shell is asked to take up: each one brings it the pass that shows it,
/// so a bar the person is not touching shows what its app declared rather
/// than waiting for the shell to draw something of its own.
#[test]
fn every_bar_ask_brings_the_shell_its_next_pass() {
    let (_surface, handle) = crate::surface::tests::desktop(1);
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let woken = std::sync::Arc::clone(&asked);
    handle.set_wake(std::sync::Arc::new(move || {
        woken.store(true, std::sync::atomic::Ordering::Relaxed);
    }));
    let toolbar = WindowToolbar {
        actions: vec![ToolbarAction::new("save", "plus", "Save")],
        ..WindowToolbar::default()
    };

    handle.declare_bar(7, 41, Some(toolbar));
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "a declaration asks for its pass"
    );
    handle.offer_bar(7, 41, ui::window_toolbar::WindowOffer::default());
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "an offer asks for its pass"
    );
    handle.declare_bar(7, 41, None);
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "taking parts away asks for its pass"
    );
    handle.edit_bar_location(7);
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "editing the location asks for its pass"
    );
    handle.open_bar_search(7, ui::window_toolbar::SearchOpening::Find);
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "opening the search asks for its pass"
    );
    handle.declare_targets(7, 41, Vec::new());
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "the window's controls ask for their pass"
    );
    handle.bar_connection_closed(41);
    assert!(
        asked.swap(false, std::sync::atomic::Ordering::Relaxed),
        "dropping what a closed connection asked for asks for its pass"
    );
}

/// Windows that show their title, and History when it is opened.
struct WithHistory;

impl TileSource for WithHistory {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str, _: Option<u64>) -> Option<Tab> {
        (app == tilo_actions::remote::workspace_activity::APP_ID).then(|| Tab {
            id,
            title: "History".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.into(),
                label: "History".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }
}

/// A window an agent opened for its person names the agent in its bar: a button, in the agent's
/// colour, described by the run that opened it, which brings that run up in History. It is the
/// keyboard's stop right after the window's tab list. A window a person opened names no agent.
#[test]
fn a_window_an_agent_opened_names_the_agent_and_its_run_and_one_a_person_opened_does_not() {
    const AGENT: &str = "Jane Doe's agent";
    let (surface, handle) = crate::surface::tests::shell_with(2, Box::new(WithHistory));
    let tabs: Vec<u64> = handle.read(|tree| tree.tabs().map(|tab| tab.id).collect());
    let run = tilo_desktop_model::ownership::OpenedBy {
        run: "agent-session:7".into(),
        agent: "scribe".into(),
        label: AGENT.into(),
        title: "Fill Jane Doe's visa application".into(),
    };
    assert!(handle.set_opened_by(std::collections::BTreeMap::from([(tabs[0], run)])));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let named = |frame: &ui::headless::FrameHarness| -> Vec<ui::accesskit::Node> {
        frame
            .a11y_nodes()
            .into_iter()
            .map(|(_, node)| node)
            .filter(|node| node.label() == Some(AGENT))
            .collect()
    };
    let agents = named(&frame);
    assert_eq!(agents.len(), 1, "only the agent's window names it");
    let mark = &agents[0];
    assert_eq!(mark.role(), ui::accesskit::Role::Button);
    assert_eq!(
        mark.description(),
        Some("Jane Doe's agent opened this window\nFill Jane Doe's visa application"),
        "who opened it, then its step on a line of its own: the mark names the agent already"
    );

    // It is the keyboard's stop right after the window's tab list.
    let stops: Vec<(ui::a11y::Role, Option<String>)> = frame
        .focus_access_nodes()
        .into_iter()
        .map(|node| (node.role, node.name))
        .collect();
    let at = stops
        .iter()
        .position(|(_, name)| name.as_deref() == Some(AGENT))
        .unwrap_or_else(|| panic!("the mark is a stop of the keyboard's: {stops:?}"));
    assert!(
        at > 0 && stops[at - 1].0 == ui::a11y::Role::TabBar,
        "right after the window's tab list: {stops:?}"
    );

    // Pressing it brings the run up in History.
    let bounds = mark.bounds().expect("the mark stands in the bar");
    let (x, y) = (bounds.x0.midpoint(bounds.x1), bounds.y0.midpoint(bounds.y1));
    frame.click_at(x, y);
    for _ in 0..3 {
        frame.relayout();
    }
    let history = tilo_actions::remote::workspace_activity::APP_ID;
    assert!(
        handle.read(|tree| tree.tabs().any(|tab| matches!(
            &tab.tile,
            tilo_desktop_model::TileContent::LocalApp { app_id, .. } if app_id == history
        ))),
        "History opened for the run"
    );

    // Once the shell says no agent opened it, the window names none.
    assert!(handle.set_opened_by(std::collections::BTreeMap::new()));
    for _ in 0..3 {
        frame.relayout();
    }
    assert!(named(&frame).is_empty(), "no window names an agent");
}

/// An app of two buttons, named `first-<tab>` and `last-<tab>`, so a test can tell whose window
/// the keyboard is in.
struct Form(ui::widget::Column);

impl ui::widget::Decorator for Form {
    type Inner = ui::widget::Column;
    fn inner(&self) -> &ui::widget::Column {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut ui::widget::Column {
        &mut self.0
    }
}

impl AppWidget for Form {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Form"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("F")
    }
}

/// Windows that each show a [`Form`], and History when it is opened.
struct Forms;

impl TileSource for Forms {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        if matches!(&tab.tile, tilo_desktop_model::TileContent::LocalApp { app_id, .. }
            if app_id == tilo_actions::remote::workspace_activity::APP_ID)
        {
            return WithHistory.tile(workspace, tab);
        }
        let id = tab.id;
        TileHost::new(TileContent::App(Box::new(Form(
            ui::widget::Column::new()
                .push(Box::new(ui::widget::Button::new(
                    &format!("first-{id}"),
                    &format!("first-{id}"),
                )))
                .push(Box::new(ui::widget::Button::new(
                    &format!("last-{id}"),
                    &format!("last-{id}"),
                ))),
        ))))
    }

    fn launch(
        &mut self,
        workspace: &Workspace,
        id: u64,
        app: &str,
        beside: Option<u64>,
    ) -> Option<Tab> {
        WithHistory.launch(workspace, id, app, beside)
    }
}

/// Tab travels the focused window, its bar and its app alike: from the app's last control it reaches
/// the window's tab list, then the agent that opened the window, through the bar's other controls
/// and back into the app, never into the other window. Shift+Tab walks back the same way, and Enter
/// on the agent brings its run up in History.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one walk round the window, forward, back and onto the agent"
)]
fn tab_walks_the_focused_windows_bar_and_app_and_enter_on_the_agent_opens_its_run() {
    const AGENT: &str = "Jane Doe's agent";
    let (surface, handle) = crate::surface::tests::shell_with(2, Box::new(Forms));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (focused, other): (u64, u64) = {
        let row = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .expect("the strip is shown");
        let of = |want: bool| {
            row.groups
                .iter()
                .find(|group| group.focused() == want)
                .map(ui::widget::WindowGroup::active_tab_id)
                .expect("one focused window and one other")
        };
        (of(true), of(false))
    };
    assert!(handle.set_opened_by(std::collections::BTreeMap::from([(
        focused,
        tilo_desktop_model::ownership::OpenedBy {
            run: "agent-session:7".into(),
            agent: "scribe".into(),
            label: AGENT.into(),
            title: "Fill Jane Doe's visa application".into(),
        },
    )])));
    for _ in 0..3 {
        frame.relayout();
    }
    let stop = |frame: &ui::headless::FrameHarness| {
        frame
            .focused_access_node()
            .map(|node| (node.role, node.name.unwrap_or_default()))
    };
    let (first, last) = (format!("first-{focused}"), format!("last-{focused}"));
    let id_of = |frame: &mut ui::headless::FrameHarness| {
        stop(frame).map(|(_, name)| name).unwrap_or_default()
    };

    // The keyboard starts on the app's last control.
    frame.click_named(&last).expect("the app's last control");
    frame.relayout();
    assert_eq!(id_of(&mut frame), last, "the click put the keyboard there");

    // Tab from there reaches the window's bar: its tab list, then the agent.
    let mut walked = Vec::new();
    for _ in 0..12 {
        frame.key("Tab");
        frame.relayout();
        let at = id_of(&mut frame);
        assert!(
            !at.ends_with(&format!("-{other}")),
            "Tab never leaves the focused window: {walked:?} then {at}"
        );
        walked.push(stop(&frame));
        if at == first {
            break;
        }
    }
    assert_eq!(
        id_of(&mut frame),
        first,
        "Tab goes round the bar and back into the app: {walked:?}"
    );
    let tab_list = walked
        .iter()
        .position(|stop| {
            stop.as_ref()
                .is_some_and(|(role, _)| *role == ui::a11y::Role::TabBar)
        })
        .unwrap_or_else(|| panic!("Tab reaches the window's tab list: {walked:?}"));
    assert_eq!(
        walked
            .get(tab_list + 1)
            .cloned()
            .flatten()
            .map(|(_, name)| name)
            .as_deref(),
        Some(AGENT),
        "the agent right after the tab list: {walked:?}"
    );

    // Shift+Tab walks back the same way, to the agent and then the tab list.
    for expected in walked
        .iter()
        .rev()
        .skip(1)
        .take(walked.len() - tab_list - 2)
    {
        frame.key("Shift+Tab");
        frame.relayout();
        assert_eq!(&stop(&frame), expected, "Shift+Tab walks back: {walked:?}");
    }
    assert_eq!(
        stop(&frame).map(|(_, name)| name).as_deref(),
        Some(AGENT),
        "Shift+Tab is back on the agent"
    );
    frame.key("Shift+Tab");
    frame.relayout();
    assert_eq!(stop(&frame), walked[tab_list], "and then on the tab list");

    // Enter on the agent brings its run up in History.
    frame.key("Tab");
    frame.relayout();
    frame.key("Enter");
    for _ in 0..3 {
        frame.relayout();
    }
    let history = tilo_actions::remote::workspace_activity::APP_ID;
    assert!(
        handle.read(|tree| tree.tabs().any(|tab| matches!(
            &tab.tile,
            tilo_desktop_model::TileContent::LocalApp { app_id, .. } if app_id == history
        ))),
        "Enter on the agent opened History for its run"
    );
}

/// A bar that names a part of its app ("Issues", "Presets") says where the window is, and the
/// window goes on being called by its app: a person looks for Tasks, not Issues. A window it named
/// by an open item before comes back to its app's name, too.
#[test]
fn a_window_whose_bar_names_a_part_of_its_app_is_called_by_its_app() {
    let (surface, handle) = crate::surface::tests::shell_with(1, Box::new(WithHistory));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (front, _) = shown_tabs(&frame)[0];
    let declare = |label: &str, section: bool| {
        handle.declare_bar(
            front,
            44,
            Some(WindowToolbar {
                location: Some(ui::window_toolbar::ToolbarLocation {
                    label: label.to_owned(),
                    section,
                    ..ui::window_toolbar::ToolbarLocation::default()
                }),
                ..WindowToolbar::default()
            }),
        );
    };
    let title = |handle: &crate::surface::DesktopHandle| {
        handle.read(|tree| tree.tab(front).map(|tab| tab.title.clone()))
    };
    declare("TSK-4 Walk the launch", false);
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(title(&handle).as_deref(), Some("TSK-4 Walk the launch"));
    declare("Issues", true);
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(title(&handle).as_deref(), Some("App0"), "called by its app");
    assert_eq!(
        handle.window_name(front),
        None,
        "a part of the app names no window"
    );
}

/// A window renamed by what its app shows, as a note's title follows the first line typed into it,
/// is a change of the shell the page shares, but nothing is drawn again for it: the shell's pass
/// that took the new name asks the page for no other frame, so a key costs the frame it drew.
#[test]
fn a_window_renamed_by_its_app_asks_for_no_frame_of_its_own() {
    let (surface, handle) = crate::surface::tests::shell_with(1, Box::new(WithHistory));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (front, _) = shown_tabs(&frame)[0];
    let declare = |label: &str| {
        handle.declare_bar(
            front,
            45,
            Some(WindowToolbar {
                location: Some(ui::window_toolbar::ToolbarLocation {
                    label: label.to_owned(),
                    ..ui::window_toolbar::ToolbarLocation::default()
                }),
                ..WindowToolbar::default()
            }),
        );
    };
    declare("Seeds");
    for _ in 0..3 {
        frame.relayout();
    }
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let woken = std::sync::Arc::clone(&asked);
    handle.set_wake(std::sync::Arc::new(move || {
        woken.store(true, std::sync::atomic::Ordering::Relaxed);
    }));
    declare("Seeds for spring");
    asked.store(false, std::sync::atomic::Ordering::Relaxed);
    let revision = handle.revision();
    for _ in 0..3 {
        frame.relayout();
    }
    assert_eq!(
        handle.window_name(front).as_deref(),
        Some("Seeds for spring")
    );
    assert_eq!(
        handle
            .read(|tree| tree.tab(front).map(|tab| tab.title.clone()))
            .as_deref(),
        Some("Seeds for spring"),
        "the window is called what it shows"
    );
    assert_ne!(
        handle.revision(),
        revision,
        "the new name is the page's to share"
    );
    assert!(
        !asked.load(std::sync::atomic::Ordering::Relaxed),
        "the pass that took the name asked for another frame"
    );
}
