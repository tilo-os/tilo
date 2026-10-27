//! The window menu (a right-click or long press on a window's bar, or the
//! window-menu shortcut) as a shell layer, and the menu a right-click on
//! anything else the desktop draws opens (a strip, a notification, a search
//! result): "Tell an agent…" about that thing.
//!
//! It lives in the overlay stack rather than the tab bar because it paints
//! above every window (the bar is clipped to its group), and because its
//! actions mutate the persisted tree, which only the host can reach: a
//! chosen row becomes a [`LayerIntent::TabMenu`] and the host answers with
//! [`TabMenuLayer::settle`]. The widget is the shared
//! `ui::widget::ContextMenu`: the same surface, rows, accent highlight and
//! keyboard model every app menu has.

use taffy::NodeId;
use ui::a11y::AccessNode;
use ui::widget::context_menu::ContextMenu;
use ui::widget::{EventCtx, Widget as _};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};

use super::layers::{LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Routed, ShellLayer};
use crate::shell::i18n::t_args;
use crate::shell::state::{GroupId, StripId};
use crate::shell::window_menu::{WindowMenuAction, WindowMenuRow, build_window_menu};

/// Which tab a right-click asked about, resolved when the request is
/// harvested so a later selection acts on that tab even if focus moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabMenuTarget {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: u64,
}

/// What the open menu acts on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MenuSubject {
    Window(TabMenuTarget),
    /// Something else the desktop draws, as an agent is told of it.
    Thing(Box<crate::shell::selection_agent::SelectionSeed>),
}

#[derive(Default)]
pub(crate) struct TabMenuLayer {
    menu: ContextMenu,
    /// The menu's rows, index-aligned with `menu.items()` by
    /// [`build_window_menu`].
    items: Vec<WindowMenuRow>,
    target: Option<MenuSubject>,
    title: String,
    /// The accessible name when the menu is not the tab's own: the bar's
    /// "Controls your desktop" chip names what it allows.
    name: Option<String>,
    /// The pointer position the menu opened at, so a refused row can
    /// re-open the menu exactly where it was.
    anchor: Point,
}

impl TabMenuLayer {
    /// Every pixel the menu painted in its last draw, in the coordinates it
    /// was drawn in; nothing while it is closed.
    pub(crate) fn painted_bounds(&self, theme: &ui::theme::Theme) -> Vec<Rect> {
        self.menu.painted_bounds(theme)
    }

    /// Open `rows` for `target` at `at` (the pointer, or the tab's corner
    /// for the keyboard).
    pub(crate) fn open(
        &mut self,
        target: MenuSubject,
        rows: Vec<WindowMenuRow>,
        at: Point,
        title: String,
        viewport: Rect,
    ) {
        let menu = build_window_menu(&rows);
        let items = rows;
        *self = Self {
            menu,
            items,
            target: Some(target),
            title,
            name: None,
            anchor: at,
        };
        // Anchored to the pointer as a zero-size rect: `open_anchored`
        // hangs the menu below-right of it and flips above / clamps inside
        // the host when the pointer is near an edge. Geometry is usable at
        // once for hit-testing; `draw` settles the rows at live text
        // metrics.
        self.menu
            .open_anchored(Rect::new(at.x, at.y, at.x, at.y), viewport);
    }

    /// Open the menu of the bar's "Controls your desktop" chip for `target`,
    /// hanging from the chip at `chip`: `captions` say what the grant allows
    /// and for how long, and its one row stops it.
    pub(crate) fn open_desktop_control(
        &mut self,
        target: TabMenuTarget,
        chip: Rect,
        captions: Vec<String>,
        viewport: Rect,
    ) {
        let name = captions.join(". ");
        let items = crate::shell::window_menu::desktop_control_menu_rows(captions);
        *self = Self {
            menu: build_window_menu(&items),
            items,
            target: Some(MenuSubject::Window(target)),
            title: String::new(),
            name: Some(name),
            anchor: Point::new(chip.x0, chip.y1),
        };
        self.menu.open_anchored(chip, viewport);
    }

    /// Where the menu was opened, which decides the display it sits on.
    pub(crate) const fn anchor_point(&self) -> Point {
        self.anchor
    }

    pub(crate) fn focus_first_action(&mut self) {
        self.menu.focus_first_action();
    }

    pub(crate) fn close(&mut self) {
        self.menu.close();
        self.target = None;
    }

    /// Painted rect of the open menu.
    pub(crate) fn rect(&self) -> Option<Rect> {
        self.is_open().then(|| self.menu.get_rect())
    }

    /// Close a menu whose tab `alive` no longer finds (closed by a peer, a
    /// revision, a chord): there is nothing left to act on, and a stale
    /// selection would fire against whatever now carries that id.
    pub(crate) fn prune(&mut self, alive: impl FnOnce(TabMenuTarget) -> bool) {
        let Some(target) = &self.target else {
            return;
        };
        if !self.menu.is_open() {
            self.target = None;
            return;
        }
        if let MenuSubject::Window(target) = target
            && !alive(*target)
        {
            self.close();
        }
    }

    /// Whether the menu is open on something other than a window.
    pub(crate) fn is_open_on_thing(&self) -> bool {
        self.menu.is_open() && matches!(self.target, Some(MenuSubject::Thing(_)))
    }

    /// The thing the menu was opened on, for the agent to be told of: the
    /// menu closes.
    pub(crate) fn take_thing(&mut self) -> Option<crate::shell::selection_agent::SelectionSeed> {
        let MenuSubject::Thing(seed) = self.target.take()? else {
            return None;
        };
        self.menu.close();
        Some(*seed)
    }

    /// The host's answer to a [`LayerIntent::TabMenu`]. Only a performed
    /// action releases the target and closes the menu. A refusal (the
    /// shared widget has already folded the menu on a click or Enter)
    /// re-opens it where it was with the same row armed; the refusal itself
    /// has been surfaced as a warning notification by `window_actions`.
    pub(crate) fn settle(
        &mut self,
        target: TabMenuTarget,
        action: WindowMenuAction,
        performed: bool,
        viewport: Rect,
    ) {
        if performed {
            self.close();
            return;
        }
        if self.menu.is_open() {
            return;
        }
        self.target = Some(MenuSubject::Window(target));
        let anchor = self.anchor;
        self.menu
            .open_anchored(Rect::new(anchor.x, anchor.y, anchor.x, anchor.y), viewport);
        if let Some(index) = self.item_index_of(action) {
            let _ = self.menu.focus_item(index);
        }
    }

    /// The `i`-th named row's item index (separators and captions publish
    /// no semantic node; the menu's own name carries the captions).
    fn item_index_of_action(&self, ordinal: usize) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                !matches!(item, WindowMenuRow::Separator | WindowMenuRow::Caption(_))
            })
            .nth(ordinal)
            .map(|(index, _)| index)
    }

    /// The row offering `action`, itself or in its nested menu.
    fn item_index_of(&self, action: WindowMenuAction) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.actions().contains(&action))
    }

    fn take_selected_intent(&mut self) -> Option<LayerIntent> {
        let action = if let Some(index) = self.menu.take_selection() {
            self.items.get(index)?.action()?
        } else {
            let (row, nested) = self.menu.take_submenu_selection()?;
            *self.items.get(row)?.actions().get(nested)?
        };
        self.intent_for(action)
    }

    /// What choosing `action` asks of the host, for what the menu is on.
    fn intent_for(&self, action: WindowMenuAction) -> Option<LayerIntent> {
        match self.target.as_ref()? {
            MenuSubject::Window(target) => Some(LayerIntent::TabMenu {
                target: *target,
                action,
            }),
            MenuSubject::Thing(_) => {
                (action == WindowMenuAction::TellAgent).then_some(LayerIntent::TellAgent)
            }
        }
    }
}

impl ShellLayer for TabMenuLayer {
    fn id(&self) -> LayerId {
        LayerId::TabContextMenu
    }

    fn is_open(&self) -> bool {
        self.menu.is_open() && self.target.is_some()
    }

    /// The widget's own model does the work — hover arms a row, Up/Down
    /// move it, Enter fires it, Escape closes, a left press inside selects
    /// and one outside dismisses (absorbed, so the thing under the menu is
    /// not also activated). Two shell-level rules sit on top: a Super chord
    /// closes the menu and still runs, and a second right-click outside it
    /// closes this menu and falls through so the tab bar under the pointer
    /// can open its own in the same dispatch.
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        // Holding the opening chord must preserve the armed row.
        if cx.shortcut == Some(settings_store::hardware::ShortcutAction::TabMenu) {
            return Routed::Consumed;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && cx.event.modifiers.state().super_key()
        {
            self.close();
            return Routed::Pass;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let inside = cx.cursor.is_some_and(|p| self.menu.contains([p.x, p.y]));
                self.close();
                // Outside: fall through so the tab bar under the pointer can
                // open its own menu in this same dispatch (and that menu
                // then absorbs the release, being open).
                return if inside {
                    Routed::Consumed
                } else {
                    Routed::Pass
                };
            }
            // The release of the opening right-click.
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => return Routed::Consumed,
            _ => {}
        }
        // `ContextMenu::handle_event` ignores the node id and the tree —
        // the menu is a layer, not a taffy child — but takes them by the
        // trait's shape; its `handled`/`focus` outputs are not needed
        // because the menu absorbs every event while it is up.
        let mut widget_handled = false;
        let mut widget_focus: Option<NodeId> = None;
        self.menu.handle_event(
            event,
            &mut EventCtx {
                handled: &mut widget_handled,
                focus_target: &mut widget_focus,
                ..cx.event.reborrow(cx.event.path, cx.event.node_id)
            },
        );
        if let Some(intent) = self.take_selected_intent() {
            cx.request(intent);
        }
        if !self.menu.is_open() {
            self.target = None;
        }
        // Modal while up: hover, wheel and stray keys stop here rather than
        // reaching the tab bar or the app under the menu.
        Routed::Consumed
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        if !self.is_open() {
            return;
        }
        // The menu is a floating Panel: this layer is the shell's top layer,
        // so it paints its whole Panel (shadow, material, rows) here.
        let anchor = Rect::new(self.anchor.x, self.anchor.y, self.anchor.x, self.anchor.y);
        self.menu
            .constrain_to(cx.geometry.display_for(Some(anchor)));
        self.menu.draw_overlay(
            cx.scene,
            cx.transform,
            cx.text_renderer,
            cx.theme,
            cx.presence,
        );
    }

    /// The menu as an assistive technology reads it: a `Menu` whose
    /// `MenuItem` children carry the shared widget's row bounds, the armed
    /// row as focused, and `tabmenu:*` targets that
    /// [`ShellLayer::semantic_intent`] resolves. A row opening a nested
    /// menu (Move to strip) carries the places as its own children.
    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        use ui::a11y::{AccessState, Role};
        if !self.is_open() {
            return None;
        }
        let hovered = self.menu.hovered();
        let rows: Vec<(usize, &WindowMenuRow)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                !matches!(item, WindowMenuRow::Separator | WindowMenuRow::Caption(_))
            })
            .collect();
        let count = rows.len();
        let children = rows
            .iter()
            .enumerate()
            .map(|(ordinal, &(index, row))| {
                let mut node = AccessNode::new(Role::MenuItem)
                    .with_name(row.label().unwrap_or_default().to_owned())
                    .with_position(ordinal.saturating_add(1), count);
                let mut state = AccessState {
                    focusable: true,
                    focused: hovered == Some(index),
                    ..AccessState::default()
                };
                match row {
                    WindowMenuRow::Entry(entry) => {
                        let key = entry.action.semantic_key();
                        node = node.with_semantic_key(key.clone()).with_action_target(key);
                    }
                    WindowMenuRow::Submenu { key, entries, .. } => {
                        state.expanded = Some(false);
                        state.disabled = entries.is_empty();
                        let places = entries.len();
                        // A place is reached through its row, which opens
                        // the nested menu: that row is where it stands.
                        let row_rect = self.menu.item_rect(index);
                        node = node.with_semantic_key(*key).with_children(
                            entries
                                .iter()
                                .enumerate()
                                .map(|(i, entry)| {
                                    let key = entry.action.semantic_key();
                                    let place = AccessNode::new(Role::MenuItem)
                                        .with_name(entry.label.clone())
                                        .with_semantic_key(key.clone())
                                        .with_action_target(key)
                                        .with_position(i.saturating_add(1), places);
                                    match row_rect {
                                        Some(rect) => place.with_bounds(rect),
                                        None => place,
                                    }
                                })
                                .collect(),
                        );
                    }
                    WindowMenuRow::Separator | WindowMenuRow::Caption(_) => {}
                }
                let node = node.with_state(state);
                match self.menu.item_rect(index) {
                    Some(rect) => node.with_bounds(rect),
                    None => node,
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_name(self.name.clone().unwrap_or_else(|| {
                    t_args(
                        "host-a11y-tab-menu",
                        &[("title", self.title.clone().into())],
                    )
                }))
                .with_semantic_key("tabmenu")
                .with_bounds(self.menu.get_rect())
                .with_state(AccessState {
                    expanded: Some(true),
                    ..AccessState::default()
                })
                .with_children(children),
        )
    }

    fn accessibility_rect(&self, _geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        match path {
            [] => Some(self.menu.get_rect()),
            [ordinal] | [ordinal, _] => self
                .item_index_of_action(*ordinal)
                .and_then(|index| self.menu.item_rect(index)),
            _ => None,
        }
    }

    fn focus_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [ordinal] => self
                .item_index_of_action(*ordinal)
                .is_some_and(|index| self.menu.focus_item(index)),
            _ => false,
        }
    }

    /// Only a row the open menu actually offers may fire — a single-tab
    /// group has no split, and only the strips and workspaces listed.
    fn semantic_intent(&self, target: &str) -> Option<LayerIntent> {
        let action = WindowMenuAction::from_semantic_key(target)?;
        self.item_index_of(action)?;
        self.intent_for(action)
    }

    fn dismiss(&mut self) -> bool {
        let open = self.is_open();
        self.close();
        open
    }
}
