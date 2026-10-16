//! A context menu over any element: right-click it, or press the Menu key
//! or Shift+F10 while focus is inside it, and the shared [`ContextMenu`]
//! opens with the commands its owner offers there and then.
//!
//! The entries are built when the menu opens, so they reflect what is
//! true at that moment (what is selected, which layer the row is), and
//! each runs a closure on the UI thread: an app dispatches its actions
//! from there, the same actions its buttons and keys use.

use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{ContextMenu, Element, EventCtx, MenuItem, Widget};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// One row of a context menu: what it shows, and what choosing it does.
pub struct MenuEntry {
    item: MenuItem,
    run: Option<Rc<dyn Fn()>>,
}

impl MenuEntry {
    /// A command: `label` shown, `run` called when it is chosen.
    pub fn new(label: impl Into<String>, run: impl Fn() + 'static) -> Self {
        Self {
            item: MenuItem::selection(label),
            run: Some(Rc::new(run)),
        }
    }

    /// A command that takes something away, styled as such.
    pub fn destructive(label: impl Into<String>, run: impl Fn() + 'static) -> Self {
        Self {
            item: MenuItem::destructive_selection(label),
            run: Some(Rc::new(run)),
        }
    }

    /// A rule between groups of commands.
    pub fn separator() -> Self {
        Self {
            item: MenuItem::separator(),
            run: None,
        }
    }

    /// The keys that do the same, shown at the row's end.
    #[must_use]
    pub fn with_shortcut(mut self, keys: impl Into<crate::platform::Shortcut>) -> Self {
        self.item = self.item.with_shortcut(keys);
        self
    }

    /// Shown but not available now.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.item = self.item.with_enabled(enabled);
        self
    }

    /// A leading icon for recognition; the label stays its name.
    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.item = self.item.with_icon(icon);
        self
    }

    /// The row as the menu shows it.
    pub fn item(&self) -> &MenuItem {
        &self.item
    }
}

/// "Ask agent…" about what is selected under `content` right now, while a
/// desktop agent can be asked. The selection is taken when the menu opens,
/// so it is what the person saw selected when they asked.
pub fn ask_agent_entry(content: &dyn Widget) -> Option<MenuEntry> {
    if !crate::shared_selection::agent_host_available() {
        return None;
    }
    let selection = crate::shared_selection::subtree_selection(content)?;
    Some(
        MenuEntry::new(
            crate::i18n::I18nManager::lookup_plain("ask-agent-about-selection"),
            move || crate::shared_selection::ask_agent(Some(selection.clone())),
        )
        .with_icon(crate::icons::Icon::Sparkles),
    )
}

/// The entries a menu offers where it opens, at the pointer in window
/// coordinates (`None` when the keyboard opened it).
pub type MenuEntries = Box<dyn Fn(Option<Point>) -> Vec<MenuEntry>>;

/// An element with a context menu.
pub struct ContextMenuArea {
    content: Element,
    entries: MenuEntries,
    menu: ContextMenu,
    runs: Vec<Option<Rc<dyn Fn()>>>,
    cursor: Point,
    /// The pointer's last move, replayed to the content once a command
    /// ran, so what the command asked of it (a rename beginning) happens
    /// at once rather than at the pointer's next move.
    moved: Option<WindowEvent>,
    /// The entry pressed with the pointer, run when the button comes up.
    pending: Option<usize>,
    viewport: Option<Rect>,
}

impl ContextMenuArea {
    /// `content` with a menu of what `entries` offers when it opens.
    pub fn new(
        content: Element,
        entries: impl Fn(Option<Point>) -> Vec<MenuEntry> + 'static,
    ) -> Self {
        Self {
            content,
            entries: Box::new(entries),
            menu: ContextMenu::new(),
            runs: Vec::new(),
            cursor: Point::ZERO,
            moved: None,
            pending: None,
            viewport: None,
        }
    }

    /// Whether the menu is showing.
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The menu, for inspection.
    pub fn menu(&self) -> &ContextMenu {
        &self.menu
    }

    /// Open the menu at `at` (window coordinates) with what is offered
    /// there now; whether there was anything to offer.
    pub fn open_at(&mut self, at: Point, pointer: bool) -> bool {
        let mut entries = (self.entries)(pointer.then_some(at));
        if entries.iter().all(|entry| entry.run.is_none()) {
            return false;
        }
        if let Some(entry) = ask_agent_entry(self.content.as_ref()) {
            entries.push(MenuEntry::separator());
            entries.push(entry);
        }
        let mut menu = ContextMenu::new();
        self.runs.clear();
        for entry in entries {
            menu.push(entry.item);
            self.runs.push(entry.run);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "window coordinates are far inside f32's range"
        )]
        let position = [at.x as f32, at.y as f32];
        match self.viewport {
            Some(viewport) => menu.open_at_clamped(position, viewport),
            None => menu.open_at(position),
        }
        menu.focus_first_action();
        self.menu = menu;
        true
    }

    /// Choose the entry at `index` as a click would.
    pub fn choose(&mut self, index: usize) {
        self.menu.close();
        if let Some(Some(run)) = self.runs.get(index) {
            let run = Rc::clone(run);
            run();
        }
    }

    /// Run the chosen entry, then let the content see the pointer where
    /// it is, so what the command asked of it happens now.
    fn run(&mut self, index: usize, ctx: &mut EventCtx<'_>) {
        self.choose(index);
        if let Some(moved) = self.moved.clone() {
            self.content.handle_event(&moved, ctx);
        }
    }

    fn opens(&self, event: &WindowEvent, ctx: &EventCtx<'_>) -> Option<(Point, bool)> {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.content.get_rect().contains(self.cursor) => Some((self.cursor, true)),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if (matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (matches!(logical_key, Key::Named(NamedKey::F10))
                    && ctx.modifiers.state().shift_key()))
                && focus_within(self.content.as_ref()) =>
            {
                let rect = self.content.get_rect();
                Some((Point::new(rect.x0, rect.y1), false))
            }
            _ => None,
        }
    }
}

/// Whether `widget` or anything inside it has the keyboard focus.
fn focus_within(widget: &dyn Widget) -> bool {
    if widget.is_focused() {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| found |= focus_within(child));
    found
}

impl super::Decorator for ContextMenuArea {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
            self.moved = Some(event.clone());
        }
        // A row pressed with the pointer runs when the button comes up,
        // and that release is the menu's: what the command started (a
        // rename's field) never sees half a click.
        if let Some(index) = self.pending {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
            ) {
                self.pending = None;
                self.run(index, ctx);
                *ctx.handled = true;
                return;
            }
            if matches!(event, WindowEvent::MouseInput { .. }) {
                self.pending = None;
            }
        }
        if self.menu.is_open() {
            // The menu itself tells the host's replay of the still opening
            // pointer from a real move, so the keyboard keeps its row.
            self.menu.handle_event(event, ctx);
            if let Some(index) = self.menu.take_selection() {
                self.menu.close();
                if matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                ) {
                    self.pending = Some(index);
                } else {
                    self.run(index, ctx);
                }
                *ctx.handled = true;
            }
            if self.menu.is_open() || *ctx.handled {
                return;
            }
        }
        if let Some((at, pointer)) = self.opens(event, ctx) {
            // A right-click reaches the content first, so it can take it
            // (a field's own menu) or act on where it landed (pick what is
            // under the pointer, as the menu is then about it).
            if pointer {
                self.content.handle_event(event, ctx);
                if *ctx.handled {
                    return;
                }
            }
            if self.open_at(at, pointer) {
                *ctx.handled = true;
            } else if !pointer {
                self.content.handle_event(event, ctx);
            }
            return;
        }
        self.content.handle_event(event, ctx);
    }

    fn concrete_type_name(&self) -> &'static str {
        "ContextMenuArea"
    }

    // The content's node is this area's: named targets and hit tests that
    // find the node meet the area, which answers as the content does.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.content.build_layout(taffy, theme, node_map);
        super::decorator::register_node(node_map, node, self);
        node
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || self.pending.is_some() || self.content.wants_pointer_capture()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu
            .modal_capture()
            .or_else(|| self.content.modal_capture())
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The overlay pass sees the whole window, where the menu may go;
        // the content's own pass may be scoped to a scrolled list.
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        self.content
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        if self.menu.is_open() {
            self.menu
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    // An open menu is part of the tree, so assistive technology and
    // inspection see it.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.content.children();
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.content.for_each_child(visitor);
        if self.menu.is_open() {
            visitor(&self.menu);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.menu.is_open();
        let mut children = self.content.children_mut();
        if open {
            children.push(&mut self.menu);
        }
        children
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::widget::Spacer;

    fn moved(at: Point) -> WindowEvent {
        crate::synthetic_input::cursor_moved(at.x, at.y)
    }

    fn right_release() -> WindowEvent {
        crate::synthetic_input::mouse_input(ElementState::Released, MouseButton::Right)
    }

    fn opening_menu() -> (ContextMenuArea, Rc<Cell<usize>>, Point) {
        let chosen = Rc::new(Cell::new(0));
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), {
            let chosen = Rc::clone(&chosen);
            move |_| {
                (1..=4)
                    .map(|index| {
                        let chosen = Rc::clone(&chosen);
                        MenuEntry::new(format!("Action {index}"), move || chosen.set(index))
                    })
                    .collect()
            }
        });
        let at = Point::new(20.0, 20.0);
        crate::testing::dispatch(&mut area, &moved(at));
        assert!(area.open_at(at, true));
        (area, chosen, at)
    }

    fn named_key(key: NamedKey) -> WindowEvent {
        crate::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed)
    }

    #[test]
    fn movement_during_opening_keeps_the_pointer_row_for_enter() {
        let (mut area, chosen, _) = opening_menu();
        let Some(row) = area.menu().item_rect(1) else {
            panic!("the second action must have a hit target");
        };
        crate::testing::dispatch(&mut area, &moved(row.center()));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen.get(), 2);
    }

    #[test]
    fn keyboard_choice_before_opening_release_survives_stationary_replay() {
        let (mut area, chosen, at) = opening_menu();
        crate::testing::dispatch(&mut area, &named_key(NamedKey::ArrowDown));
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &moved(at));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen.get(), 2);
    }

    #[test]
    fn entries_are_built_when_it_opens_and_choosing_runs_one() {
        let chosen = Rc::new(Cell::new(0));
        let offered = Rc::new(Cell::new(1));
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), {
            let chosen = Rc::clone(&chosen);
            let offered = Rc::clone(&offered);
            move |_| {
                let n = offered.get();
                let chosen = Rc::clone(&chosen);
                vec![
                    MenuEntry::new(format!("Take {n}"), move || chosen.set(n))
                        .with_shortcut("Ctrl+T"),
                    MenuEntry::separator(),
                ]
            }
        });
        offered.set(7);
        assert!(area.open_at(Point::new(5.0, 5.0), true));
        assert!(area.is_open());
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::label),
            Some("Take 7")
        );
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::shortcut),
            Some(&crate::platform::Shortcut::command("T"))
        );
        area.choose(0);
        assert_eq!(chosen.get(), 7);
        assert!(!area.is_open());
    }

    type Chosen = Rc<std::cell::RefCell<Vec<&'static str>>>;

    /// Content that a right-click changes, as a canvas picking the mark
    /// under it does: the next frame rebuilds the tree.
    struct PicksOnRightPress {
        inner: Spacer,
        picked: bool,
    }

    impl Widget for PicksOnRightPress {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.inner.build_layout(taffy, theme, node_map)
        }
        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: Point,
        ) {
            self.inner.apply_layout(taffy, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.inner
                .draw(scene, transform, text_renderer, theme, presence);
        }
        fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                }
            ) {
                self.picked = true;
            }
        }
        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.picked)
        }
        fn get_rect(&self) -> Rect {
            self.inner.get_rect()
        }
        fn name(&self) -> &'static str {
            "PicksOnRightPress"
        }
    }

    /// A 300 px area offering First to Fourth, in a window whose host
    /// replays a still pointer after the widgets change, the pointer resting
    /// at (100, 100).
    fn four_entries() -> (crate::headless::FrameHarness, Chosen) {
        let chosen = Chosen::default();
        let content = PicksOnRightPress {
            inner: Spacer::fixed(300.0),
            picked: false,
        };
        let area = ContextMenuArea::new(Box::new(content), {
            let chosen = Rc::clone(&chosen);
            move |_| {
                ["First", "Second", "Third", "Fourth"]
                    .into_iter()
                    .map(|label| {
                        let chosen = Rc::clone(&chosen);
                        MenuEntry::new(label, move || chosen.borrow_mut().push(label))
                    })
                    .collect()
            }
        });
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            area,
            (400, 400),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        harness.send(&crate::synthetic_input::cursor_moved(100.0, 100.0));
        (harness, chosen)
    }

    fn mouse(
        harness: &mut crate::headless::FrameHarness,
        state: ElementState,
        button: MouseButton,
    ) {
        harness.send(&crate::synthetic_input::mouse_input(state, button));
    }

    /// Right-click where the pointer rests, with the frame that rebuilds
    /// the changed content and paints the opened menu landing between press
    /// and release, as it does live.
    fn right_click(harness: &mut crate::headless::FrameHarness) {
        mouse(harness, ElementState::Pressed, MouseButton::Right);
        assert!(
            harness.root::<ContextMenuArea>().is_open(),
            "a right-click opens the menu"
        );
        harness.relayout();
        mouse(harness, ElementState::Released, MouseButton::Right);
        assert!(
            harness.root::<ContextMenuArea>().is_open(),
            "the release that ends the right-click leaves the menu open"
        );
    }

    /// The fourth entry is three rows down, as Canvas's Duplicate is: a
    /// keyboard row one off lands on another entry.
    #[test]
    fn a_right_click_menu_runs_the_entry_the_keyboard_chooses() {
        let (mut harness, chosen) = four_entries();
        right_click(&mut harness);
        assert_eq!(
            harness.root::<ContextMenuArea>().menu().hovered(),
            Some(0),
            "the menu opens on its first entry, though the still pointer was replayed"
        );
        for _ in 0..3 {
            harness.key("ArrowDown");
        }
        harness.key("Enter");
        assert_eq!(*chosen.borrow(), vec!["Fourth"]);
        assert!(!harness.root::<ContextMenuArea>().is_open());
    }

    #[test]
    fn a_right_click_menu_runs_the_entry_the_pointer_clicks() {
        let (mut harness, chosen) = four_entries();
        right_click(&mut harness);
        let second = harness
            .root::<ContextMenuArea>()
            .menu()
            .item_rect(1)
            .expect("the second entry is laid out")
            .center();
        harness.send(&crate::synthetic_input::cursor_moved(second.x, second.y));
        assert_eq!(
            harness.root::<ContextMenuArea>().menu().hovered(),
            Some(1),
            "hover follows the pointer once it moves"
        );
        mouse(&mut harness, ElementState::Pressed, MouseButton::Left);
        harness.relayout();
        mouse(&mut harness, ElementState::Released, MouseButton::Left);
        assert_eq!(*chosen.borrow(), vec!["Second"]);
        assert!(!harness.root::<ContextMenuArea>().is_open());
    }

    #[test]
    fn nothing_to_offer_opens_nothing() {
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), |_| {
            vec![MenuEntry::separator()]
        });
        assert!(!area.open_at(Point::new(5.0, 5.0), true));
        assert!(!area.is_open());
    }
}
