//! The desktop draws the slots its apps declare in their windows' bars.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{AppKind, AppWidget, TaffyTreeExt as _, TileContent, TileHost, Widget};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use crate::surface::{DesktopSurface, TileSource, seed_tree};

/// An app as the apps moved to the window bar's slots are written: it
/// declares its slots with [`ui::window_toolbar::declare`] and reads what the
/// person did with [`ui::window_toolbar::take_events`], knowing nothing of
/// the process it shares with the desktop and the other apps.
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
    fn uses_host_record(&self) -> bool {
        false
    }
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

/// Two apps in the desktop's own process, as every app of the browser
/// desktop runs: each one's slots stand in its own window's bar, and a press
/// on one reaches that app and no other.
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
