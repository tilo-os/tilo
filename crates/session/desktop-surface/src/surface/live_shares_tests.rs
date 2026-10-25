//! Windows shared live: the desktop lists the windows on a record, offers
//! to share them from their menus, and draws the views a window embeds.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use ui::kurbo::{Affine, Rect};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{AppKind, AppWidget, TaffyTreeExt as _, TileContent, TileHost, Widget};
use ui::window_share::{Ask, Embed, EmbedState, ResourceView};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

use crate::surface::{DesktopSurface, TileSource, seed_tree};

const NOTES: &str = "so.tilo.notes";

fn plan() -> ResourceView {
    ResourceView {
        app: NOTES.into(),
        record: "plan".into(),
        resource: "plan".into(),
    }
}

/// A test app: the conversation embeds the plan's view in the middle of its
/// window; the editor declares a bar, so its menu can hold the desktop's
/// items.
struct Window {
    embeds: bool,
    rect: Rect,
    states: Rc<RefCell<Vec<Option<EmbedState>>>>,
}

impl Widget for Window {
    fn update(&mut self, _: &ui::widget::AppModel, _: &ui::widget::ModelPath) -> bool {
        if self.embeds {
            let rect = Rect::from_center_size(self.rect.center(), (320.0, 200.0));
            ui::window_share::declare(vec![Embed {
                key: "m1".into(),
                view: plan(),
                rect,
                clip: self.rect,
            }]);
            self.states.borrow_mut().push(ui::window_share::state("m1"));
        } else {
            ui::window_toolbar::declare(Some(WindowToolbar {
                actions: vec![ToolbarAction::new("bold", "bold", "Bold")],
                ..WindowToolbar::default()
            }));
        }
        false
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

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: ui::kurbo::Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Window"
    }
}

impl AppWidget for Window {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Window"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("W")
    }
}

/// The embedded view's app: where it was laid out and how it was drawn.
struct View {
    rect: Rect,
    drawn: Rc<RefCell<Vec<(Rect, Affine)>>>,
}

impl Widget for View {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &ui::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                flex_grow: 1.0,
                size: ui::taffy::Size {
                    width: ui::taffy::Dimension::percent(1.0),
                    height: ui::taffy::Dimension::percent(1.0),
                },
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: ui::kurbo::Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        _: &mut ui::vello::Scene,
        transform: Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::presence::PresenceCtx,
    ) {
        self.drawn.borrow_mut().push((self.rect, transform));
    }

    fn name(&self) -> &'static str {
        "View"
    }
}

impl AppWidget for View {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Notes"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("N")
    }
}

#[derive(Clone, Default)]
struct Seen {
    states: Rc<RefCell<Vec<Option<EmbedState>>>>,
    drawn: Rc<RefCell<Vec<(Rect, Affine)>>>,
    embedded: Rc<Cell<u32>>,
    asked: Rc<RefCell<Vec<ResourceView>>>,
}

struct ShareTiles(Seen);

impl TileSource for ShareTiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::App(Box::new(Window {
            embeds: tab.title == "Chat",
            rect: Rect::ZERO,
            states: Rc::clone(&self.0.states),
        })))
    }

    fn window_record(&self, tab: u64) -> Option<(String, String)> {
        tab.is_multiple_of(2)
            .then(|| ("plan".into(), "notes.page".into()))
    }

    fn draws_embeds(&self) -> bool {
        true
    }

    fn embed(&mut self, view: &ResourceView) -> Result<TileHost, String> {
        self.0.asked.borrow_mut().push(view.clone());
        self.0.embedded.set(self.0.embedded.get().saturating_add(1));
        Ok(TileHost::new(TileContent::App(Box::new(View {
            rect: Rect::ZERO,
            drawn: Rc::clone(&self.0.drawn),
        }))))
    }
}

fn desk(seen: &Seen) -> (ui::headless::FrameHarness, u64) {
    let mut minter = IdMinter::new(8);
    let mut tabs = Vec::new();
    let mut notes = 0;
    for (title, app) in [("Chat", ui::window_share::SHARING_APP), ("Plan", NOTES)] {
        // The notes window shows a record (an even id here).
        let mut id = minter.mint();
        while id.is_multiple_of(2) != (app == NOTES) {
            id = minter.mint();
        }
        if app == NOTES {
            notes = id;
        }
        tabs.push(Tab {
            id,
            title: title.into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
        });
    }
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, _handle) = DesktopSurface::new(tree, 8, Box::new(ShareTiles(seen.clone())));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 800),
        ui::Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    (frame, notes)
}

/// The conversation's window asks for the plan's view: the desktop runs the
/// plan's app once, lays it out at a window's size and draws it scaled into
/// the place asked for, and says it shows.
#[test]
fn a_view_a_window_embeds_is_its_own_app_drawn_scaled_into_its_place() {
    let seen = Seen::default();
    let (mut frame, _) = desk(&seen);
    assert_eq!(seen.embedded.get(), 1, "one app for the view, kept");
    assert_eq!(*seen.asked.borrow(), vec![plan()]);
    assert_eq!(
        seen.states.borrow().last().cloned().flatten(),
        Some(EmbedState::Showing)
    );
    let (rect, transform) = *seen.drawn.borrow().last().expect("the view is drawn");
    assert_eq!(
        rect.size(),
        ui::kurbo::Size::new(1000.0, 625.0),
        "a window's size"
    );
    let placed = transform.transform_rect_bbox(rect);
    assert!((placed.width() - 320.0).abs() < 0.5, "{placed:?}");
    assert!((placed.height() - 200.0).abs() < 0.5, "{placed:?}");
    frame.relayout();
    assert_eq!(seen.embedded.get(), 1);
}

/// A window on a record is listed for the sharing app, and its menu offers
/// to share it; choosing that asks the sharing app, and once it is shared
/// the menu offers to stop.
#[test]
fn a_window_on_a_record_is_offered_for_sharing_from_its_menu() {
    let seen = Seen::default();
    let (mut frame, notes) = desk(&seen);
    let windows = ui::window_share::windows();
    assert_eq!(windows.len(), 1, "the conversation itself is not offered");
    assert_eq!(windows[0].tab, notes);
    assert_eq!(windows[0].record, "plan");
    let menu = |frame: &mut ui::headless::FrameHarness| {
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .and_then(|row| {
                row.groups
                    .iter()
                    .find(|group| group.active_tab_id() == notes)
                    .and_then(|group| group.tab_toolbar(notes).cloned())
            })
            .expect("the editor's bar")
            .actions
    };
    let actions = menu(&mut frame);
    assert_eq!(actions[0].id, "bold", "the app's own first");
    assert_eq!(actions[1].name, "Share to channel…");
    assert!(actions[1].menu);

    frame.root_mut::<DesktopSurface>().window_slots.route(
        notes,
        ToolbarEvent::Action {
            id: actions[1].id.clone(),
        },
    );
    frame.relayout();
    assert_eq!(
        ui::window_share::take_ask(),
        Some(Ask::Share(windows[0].clone()))
    );

    ui::window_share::publish_shared(vec![ui::window_share::SharedWindow {
        message: "m1".into(),
        app: NOTES.into(),
        record: "plan".into(),
        place: "#general".into(),
    }]);
    frame.relayout();
    let actions = menu(&mut frame);
    assert_eq!(actions[1].name, "Stop sharing in #general");
    frame.root_mut::<DesktopSurface>().window_slots.route(
        notes,
        ToolbarEvent::Action {
            id: actions[1].id.clone(),
        },
    );
    frame.relayout();
    assert_eq!(
        ui::window_share::take_ask(),
        Some(Ask::Stop {
            message: "m1".into()
        })
    );
    ui::window_share::publish_shared(Vec::new());
    ui::window_share::set_drawn(false);
}
