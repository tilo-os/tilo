//! The desktop draws the slots its apps declare in their windows' bars.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{AppKind, AppWidget, TaffyTreeExt as _, TileContent, TileHost, Widget};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use crate::surface::{DesktopSurface, TileSource, seed_tree};

/// An app as the apps moved to the window bar's slots are written: it declares its slots with
/// [`ui::window_toolbar::declare`] and reads what the person did with
/// [`ui::window_toolbar::take_events`], knowing nothing of the process it shares with the desktop
/// and the other apps.
struct SlotApp {
    action: &'static str,
    heard: Rc<RefCell<Vec<ToolbarEvent>>>,
}

impl Widget for SlotApp {
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
        "SlotApp"
    }
}

impl AppWidget for SlotApp {
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
struct SlotApps(Vec<(&'static str, Rc<RefCell<Vec<ToolbarEvent>>>)>);

impl TileSource for SlotApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        let index = usize::try_from(tab.id % 2).unwrap_or(0);
        let (action, heard) = self.0[index].clone();
        TileHost::new(TileContent::App(Box::new(SlotApp { action, heard })))
    }
}

/// Two apps in the desktop's own process, as every app of the browser desktop runs: each one's
/// slots stand in its own window's bar, and a press on one reaches that app and no other.
#[test]
fn each_apps_slots_stand_in_its_windows_bar_and_a_press_reaches_it() {
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
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, _handle) = DesktopSurface::new(tree, 7, Box::new(SlotApps(apps.clone())));
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
        let toolbar = toolbar.as_ref().expect("the app's slots are in its bar");
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

/// An app in a process of its own declares its slots over its connection: the next pass shows them
/// in its window's bar, what the person did there goes back on that same connection, and the slots
/// go when it closes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test, from the declaration to the press it sends back"
)]
fn an_apps_own_process_declares_its_slots_and_hears_the_press_on_its_connection() {
    let (surface, handle) = crate::surface::tests::desktop(2);
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
            "the app's slot stands in its window's bar"
        );
        let (_, _, _, actions) = group.bar_for_tests().toolbar_rects();
        (
            actions.first().copied().unwrap_or(ui::kurbo::Rect::ZERO),
            group.editing_location(),
        )
    };
    assert!(!edited, "nothing asked for the location yet");
    assert!(button.area() > 0.0, "the slot is a button in the bar");

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
            41,
            ToolbarEvent::Action {
                id: "save".to_owned(),
            }
        )],
        "the press goes back on the connection that declared the slot"
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

    // The app's connection closes: its slots go with it.
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

/// A window's app describes the controls of its own content over its connection: the desk lists
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
            rect: [10.0, 20.0, 110.0, 40.0],
            actions: vec![TargetAction::Activate, TargetAction::ShowContextMenu],
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
    let (name, (x, y, w, h)) = listed.first().expect("the window's control is listed");
    assert_eq!(name, "Documents");
    assert_eq!(
        (*x, *y),
        (content.x0 + 10.0, content.y0 + 20.0),
        "listed where it is drawn in its window"
    );
    assert_eq!((*w, *h), (100.0, 20.0));

    assert_eq!(
        handle.press_window_control("Documents", TargetAction::ShowContextMenu),
        Ok(())
    );
    assert_eq!(
        handle.take_target_presses(),
        vec![(
            41,
            "Documents".to_owned(),
            "ListItem".to_owned(),
            TargetAction::ShowContextMenu
        )],
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

/// What an app in a process of its own asks of its window's bar is desk work
/// the desk is asked to take up: each one brings it the pass that shows it,
/// so a bar the person is not touching shows what its app declared rather
/// than waiting for the desk to draw something of its own.
#[test]
fn every_bar_ask_brings_the_desk_its_next_pass() {
    let (_surface, handle) = crate::surface::tests::desktop(1);
    let asked = Rc::new(Cell::new(false));
    let woken = Rc::clone(&asked);
    handle.set_wake(Rc::new(move || woken.set(true)));
    let toolbar = WindowToolbar {
        actions: vec![ToolbarAction::new("save", "plus", "Save")],
        ..WindowToolbar::default()
    };

    handle.declare_bar(7, 41, Some(toolbar));
    assert!(asked.replace(false), "a declaration asks for its pass");
    handle.offer_bar(7, 41, ui::window_toolbar::WindowOffer::default());
    assert!(asked.replace(false), "an offer asks for its pass");
    handle.declare_bar(7, 41, None);
    assert!(asked.replace(false), "taking slots away asks for its pass");
    handle.edit_bar_location(7);
    assert!(
        asked.replace(false),
        "editing the location asks for its pass"
    );
    handle.open_bar_search(7, ui::window_toolbar::SearchOpening::Find);
    assert!(asked.replace(false), "opening the search asks for its pass");
    handle.declare_targets(7, 41, Vec::new());
    assert!(
        asked.replace(false),
        "the window's controls ask for their pass"
    );
    handle.bar_connection_closed(41);
    assert!(
        asked.replace(false),
        "dropping what a closed connection asked for asks for its pass"
    );
}
