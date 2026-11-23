//! Windows shared live: the desktop lists the windows on a record, offers to share them from their
//! menus, draws the views a window embeds, and hands a view the window asks for to work in the
//! person's input where it stands.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Tab, Workspace};
use ui::kurbo::{Affine, Rect};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{AppKind, AppWidget, TaffyTreeExt as _, TileContent, TileHost, Widget};
use ui::window_share::{Ask, Embed, EmbedState, ResourceView};
use ui::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use crate::surface::{DesktopSurface, TileSource};

const NOTES: &str = "so.tilo.notes";

fn plan() -> ResourceView {
    ResourceView {
        app: NOTES.into(),
        record: "plan".into(),
        resource: "plan".into(),
    }
}

/// A test app: the conversation embeds the plan's view in the middle of its window, as a
/// miniature or (`Seen::working`) to work in; the editor declares a bar, so its menu can hold the
/// desktop's items, unless it is `bare` (a sketch pad that declares none). It takes the keyboard on
/// a press, as an app does, and says what input reached it.
struct Window {
    title: String,
    embeds: bool,
    bare: bool,
    rect: Rect,
    pointer: ui::kurbo::Point,
    focused: bool,
    seen: Seen,
}

impl Widget for Window {
    fn update(&mut self, _: &ui::widget::AppModel, _: &ui::widget::ModelPath) -> bool {
        if self.embeds {
            let embed = if self.seen.working.get() {
                let rect = Rect::from_center_size(self.rect.center(), (640.0, 400.0));
                Embed::miniature("m1", plan(), rect, self.rect).to_work_in("The shared plan")
            } else {
                let rect = Rect::from_center_size(self.rect.center(), (320.0, 200.0));
                Embed::miniature("m1", plan(), rect, self.rect)
            };
            self.seen.place.set(embed.rect);
            self.seen.window.set(self.rect);
            ui::window_share::declare(vec![embed]);
            self.seen
                .states
                .borrow_mut()
                .push(ui::window_share::state("m1"));
        } else if !self.bare {
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let (seen, title) = (self.seen.clone(), self.title.clone());
        input(
            event,
            ctx,
            (self.rect, &mut self.pointer, self.focused),
            |what| seen.input.borrow_mut().push(format!("{title}:{what}")),
        );
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn name(&self) -> &'static str {
        "Window"
    }
}

/// What a test app does with `event`: a press inside `rect` is its own and takes the keyboard, a
/// key is its own while it has the keyboard, and so is a wheel over it. `heard` is told each.
fn input(
    event: &WindowEvent,
    ctx: &mut ui::widget::EventCtx<'_>,
    (rect, pointer, focused): (Rect, &mut ui::kurbo::Point, bool),
    heard: impl Fn(&str),
) {
    match event {
        WindowEvent::CursorMoved { position, .. } => *pointer = ctx.logical(position),
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } if rect.contains(*pointer) && !*ctx.handled => {
            heard("press");
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
        }
        WindowEvent::KeyboardInput { event, .. }
            if focused && event.state == ElementState::Pressed && !*ctx.handled =>
        {
            if let Some(text) = &event.text {
                heard(&format!("key {text}"));
                *ctx.handled = true;
            }
        }
        WindowEvent::MouseWheel { .. } if rect.contains(*pointer) && !*ctx.handled => {
            heard("wheel");
            *ctx.handled = true;
        }
        _ => {}
    }
}

impl AppWidget for Window {
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

/// The embedded view's app: where it was laid out, how it was drawn and what input reached it.
struct View {
    rect: Rect,
    pointer: ui::kurbo::Point,
    focused: bool,
    seen: Seen,
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
        self.seen.drawn.borrow_mut().push((self.rect, transform));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let seen = self.seen.clone();
        input(
            event,
            ctx,
            (self.rect, &mut self.pointer, self.focused),
            |what| seen.input.borrow_mut().push(format!("Plan:{what}")),
        );
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name("The plan's text")
                .with_bounds(self.rect),
        )
    }

    fn name(&self) -> &'static str {
        "View"
    }
}

impl AppWidget for View {
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
    /// Whether the conversation asks for the plan's view to work in, not as a miniature.
    working: Rc<Cell<bool>>,
    /// Where the conversation asked for the view, and where the conversation's window is.
    place: Rc<Cell<Rect>>,
    window: Rc<Cell<Rect>>,
    /// What input reached which app, in order: `Chat:press`, `Plan:key a`.
    input: Rc<RefCell<Vec<String>>>,
}

impl Seen {
    /// What input reached the apps since the last call.
    fn heard(&self) -> Vec<String> {
        std::mem::take(&mut *self.input.borrow_mut())
    }
}

struct ShareTiles(Seen);

impl TileSource for ShareTiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::App(Box::new(Window {
            title: tab.title.clone(),
            embeds: tab.title == "Chat",
            bare: tab.title == "Sketch",
            rect: Rect::ZERO,
            pointer: ui::kurbo::Point::ZERO,
            focused: false,
            seen: self.0.clone(),
        })))
    }

    fn window_resource(&self, tab: u64) -> Option<(String, String)> {
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
            pointer: ui::kurbo::Point::ZERO,
            focused: false,
            seen: self.0.clone(),
        }))))
    }
}

fn shell(seen: &Seen) -> (ui::headless::FrameHarness, u64) {
    shell_with(seen, "Plan")
}

/// The conversation and a notes window titled `notes` , and the notes window's tab.
fn shell_with(seen: &Seen, notes_title: &str) -> (ui::headless::FrameHarness, u64) {
    let mut minter = IdMinter::new(8);
    let mut tabs = Vec::new();
    let mut notes = 0;
    for (title, app) in [
        ("Chat", ui::window_share::SHARING_APP),
        (notes_title, NOTES),
    ] {
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
            view: String::new(),
            pinned: false,
        });
    }
    let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Office", tabs);
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

/// The conversation's window asks for the plan's view: the desktop runs the plan's app once, lays
/// it out at a window's size and draws it scaled into the place asked for, and says it shows.
#[test]
fn a_view_a_window_embeds_is_its_own_app_drawn_scaled_into_its_place() {
    let seen = Seen::default();
    let (mut frame, _) = shell(&seen);
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

/// Move the pointer to `at` and press and release the left button there.
fn press(frame: &mut ui::headless::FrameHarness, at: ui::kurbo::Point) {
    frame.send(&ui::synthetic_input::cursor_moved(at.x, at.y));
    frame.send(&ui::synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Left,
    ));
    frame.send(&ui::synthetic_input::mouse_input(
        ElementState::Released,
        MouseButton::Left,
    ));
    frame.relayout();
}

/// A miniature is a picture of the view: a press on it, a key and the wheel over it are the
/// window's that shows it.
#[test]
fn a_miniature_takes_no_input() {
    let seen = Seen::default();
    let (mut frame, _) = shell(&seen);
    let _ = seen.heard();
    press(&mut frame, seen.place.get().center());
    frame.send(&ui::synthetic_input::char_key("a"));
    frame.send(&ui::synthetic_input::mouse_wheel(0.0, -1.0));
    assert_eq!(
        seen.heard(),
        ["Chat:press", "Chat:key a", "Chat:wheel"],
        "the conversation's own"
    );
    ui::window_share::set_drawn(false);
}

/// A view the window asks for to work in is the plan's own app laid out at the size of its place,
/// where the window shows it. A press in it is its own and gives it the keyboard; while it has the
/// keyboard the keys and the wheel over it are its own too. A press outside gives the keyboard
/// back, and the wheel over the view then moves the conversation it stands in.
#[test]
fn a_view_to_work_in_stands_at_its_place_and_takes_the_press_the_keys_and_the_wheel() {
    let seen = Seen::default();
    seen.working.set(true);
    let (mut frame, _) = shell(&seen);
    assert_eq!(seen.embedded.get(), 1, "one app for the view");
    assert_eq!(
        seen.states.borrow().last().cloned().flatten(),
        Some(EmbedState::Showing)
    );
    let place = seen.place.get();
    let (rect, transform) = *seen.drawn.borrow().last().expect("the view is drawn");
    assert_eq!(
        rect.size(),
        ui::kurbo::Size::new(640.0, 400.0),
        "laid out at its place's size, not a window's scaled down"
    );
    let drawn = transform.transform_rect_bbox(rect);
    assert!(
        (drawn.x0 - place.x0).abs() < 0.5 && (drawn.y0 - place.y0).abs() < 0.5,
        "drawn where the window asked: {drawn:?} for {place:?}"
    );
    assert!(
        (rect.x0 - place.x0).abs() < 0.5 && (rect.y0 - place.y0).abs() < 0.5,
        "and laid out there, so a press lands where it is seen: {rect:?} for {place:?}"
    );

    // A screen reader reads the view by the name the conversation gave it, and what it holds
    // under that.
    let named = |frame: &ui::headless::FrameHarness, name: &str| {
        frame
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(name))
    };
    let (group, _) =
        named(&frame, "The shared plan").expect("the view, by the conversation's name for it");
    let (text, _) = named(&frame, "The plan's text").expect("what the view holds");
    let nodes: HashMap<_, _> = frame.a11y_nodes().into_iter().collect();
    let mut under = vec![group];
    let mut found = false;
    while let Some(id) = under.pop() {
        found |= id == text;
        if let Some(node) = nodes.get(&id) {
            under.extend(node.children().iter().copied());
        }
    }
    assert!(found, "what the view holds is read under its name");

    let _ = seen.heard();
    // The wheel over a view nobody works in yet moves the conversation.
    frame.send(&ui::synthetic_input::cursor_moved(
        place.center().x,
        place.center().y,
    ));
    frame.send(&ui::synthetic_input::mouse_wheel(0.0, -1.0));
    assert_eq!(seen.heard(), ["Chat:wheel"]);

    press(&mut frame, place.center());
    assert_eq!(
        seen.heard(),
        ["Plan:press"],
        "the press is the view's, not the conversation's under it"
    );
    frame.send(&ui::synthetic_input::char_key("a"));
    frame.send(&ui::synthetic_input::mouse_wheel(0.0, -1.0));
    assert_eq!(
        seen.heard(),
        ["Plan:key a", "Plan:wheel"],
        "with the keyboard, the keys and the wheel over it are its own"
    );

    // A press in the conversation beside the view gives the keyboard back.
    let window = seen.window.get();
    let beside = ui::kurbo::Point::new(window.x0 + 24.0, place.center().y);
    assert!(!place.contains(beside) && window.contains(beside));
    press(&mut frame, beside);
    frame.send(&ui::synthetic_input::char_key("b"));
    frame.send(&ui::synthetic_input::cursor_moved(
        place.center().x,
        place.center().y,
    ));
    frame.send(&ui::synthetic_input::mouse_wheel(0.0, -1.0));
    assert_eq!(
        seen.heard(),
        ["Chat:press", "Chat:key b", "Chat:wheel"],
        "the conversation has the keyboard again, and the wheel moves it past the view"
    );
    ui::window_share::set_drawn(false);
}

/// The same view goes from a miniature to a place to work in and back without its app being
/// started again: what the person saw in it stays where it was.
#[test]
fn a_view_grows_to_work_in_and_back_with_the_same_app() {
    let seen = Seen::default();
    let (mut frame, _) = shell(&seen);
    let size = |seen: &Seen| seen.drawn.borrow().last().expect("drawn").0.size();
    assert_eq!(size(&seen), ui::kurbo::Size::new(1000.0, 625.0));

    seen.working.set(true);
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(size(&seen), ui::kurbo::Size::new(640.0, 400.0));
    press(&mut frame, seen.place.get().center());
    assert_eq!(seen.heard().last().map(String::as_str), Some("Plan:press"));

    seen.working.set(false);
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(size(&seen), ui::kurbo::Size::new(1000.0, 625.0));
    let _ = seen.heard();
    press(&mut frame, seen.place.get().center());
    assert_eq!(seen.heard(), ["Chat:press"], "a miniature again");
    assert_eq!(seen.embedded.get(), 1, "the one app, never started again");
    ui::window_share::set_drawn(false);
}

/// A window on a record is listed for the sharing app, and its menu offers to share it; choosing
/// that asks the sharing app, and once it is shared the menu offers to stop.
#[test]
fn a_window_on_a_record_is_offered_for_sharing_from_its_menu() {
    let seen = Seen::default();
    let (mut frame, notes) = shell(&seen);
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

    frame.root_mut::<DesktopSurface>().window_parts.route(
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
    frame.root_mut::<DesktopSurface>().window_parts.route(
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

/// A window on a record whose app declared no bar is offered for sharing too: its only slots are
/// the desktop's menu, a button at the end of its title row, and choosing the item asks the sharing
/// app as from a bar.
#[test]
fn a_window_without_a_bar_is_offered_for_sharing_from_its_title_row() {
    let seen = Seen::default();
    let (mut frame, sketch) = shell_with(&seen, "Sketch");
    let windows = ui::window_share::windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].tab, sketch);
    let shown = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| {
            row.groups
                .iter()
                .find(|group| group.active_tab_id() == sketch)
                .and_then(|group| group.tab_toolbar(sketch).cloned())
        })
        .expect("the desktop's menu");
    assert!(shown.menu_only(), "no bar of the app's: {shown:?}");
    assert_eq!(shown.actions.len(), 1);
    assert_eq!(shown.actions[0].name, "Share to channel…");

    frame.root_mut::<DesktopSurface>().window_parts.route(
        sketch,
        ToolbarEvent::Action {
            id: shown.actions[0].id.clone(),
        },
    );
    frame.relayout();
    assert_eq!(
        ui::window_share::take_ask(),
        Some(Ask::Share(windows[0].clone()))
    );
    ui::window_share::set_drawn(false);
}
