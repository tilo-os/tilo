//! The tab right-click menu as a shell layer.
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
use crate::shell::{TabMenuAction, build_tab_context_menu};

/// Which tab a right-click asked about, resolved when the request is
/// harvested so a later selection acts on that tab even if focus moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabMenuTarget {
    pub strip: StripId,
    pub group: GroupId,
    pub tab: u64,
}

#[derive(Default)]
pub(crate) struct TabMenuLayer {
    menu: ContextMenu,
    /// Row index → action; `None` is a separator. Index-aligned with
    /// `menu.items()` by [`build_tab_context_menu`]. (A `MenuItem` id on
    /// the shared widget would replace this parallel list — recorded for
    /// F4.)
    items: Vec<Option<TabMenuAction>>,
    target: Option<TabMenuTarget>,
    /// Title of the tab the menu is for, for the accessible name.
    title: String,
    /// The pointer position the menu opened at, so a refused row can
    /// re-open the menu exactly where it was.
    anchor: Point,
}

impl TabMenuLayer {
    /// Open for `target`, a tab in a group of `tab_count`, at the pointer.
    pub(crate) fn open(
        &mut self,
        target: TabMenuTarget,
        tab_count: usize,
        at: Point,
        title: String,
        viewport: Rect,
    ) {
        let (menu, items) = build_tab_context_menu(tab_count);
        *self = Self {
            menu,
            items,
            target: Some(target),
            title,
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
        let Some(target) = self.target else {
            return;
        };
        if !self.menu.is_open() {
            self.target = None;
            return;
        }
        if !alive(target) {
            self.close();
        }
    }

    /// The host's answer to a [`LayerIntent::TabMenu`]. Only a performed
    /// action releases the target and closes the menu. A refusal (the
    /// shared widget has already folded the menu on a click or Enter)
    /// re-opens it where it was with the same row armed; the refusal itself
    /// has been surfaced as a warning notification by `window_actions`.
    pub(crate) fn settle(
        &mut self,
        target: TabMenuTarget,
        action: TabMenuAction,
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
        self.target = Some(target);
        let anchor = self.anchor;
        self.menu
            .open_anchored(Rect::new(anchor.x, anchor.y, anchor.x, anchor.y), viewport);
        if let Some(index) = self.item_index_of(action) {
            let _ = self.menu.focus_item(index);
        }
    }

    /// The `i`-th action row's item index (separators are not actions and
    /// publish no semantic node).
    fn item_index_of_action(&self, ordinal: usize) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_some())
            .nth(ordinal)
            .map(|(index, _)| index)
    }

    fn item_index_of(&self, action: TabMenuAction) -> Option<usize> {
        self.items.iter().position(|item| *item == Some(action))
    }

    fn take_selected_intent(&mut self) -> Option<LayerIntent> {
        let index = self.menu.take_selection()?;
        let action = (*self.items.get(index)?)?;
        let target = self.target?;
        Some(LayerIntent::TabMenu { target, action })
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
        // Settle the rows at the live text metrics before reading the rect
        // (`ContextMenu::draw` does the same), so the shadow follows exactly
        // the outline the widget is about to paint.
        self.menu.layout_within(cx.text_renderer, cx.theme);
        let rect = self.menu.get_rect();
        // A popover is a Panel (HIG §9.1): `e4`, following the radius the
        // shared widget paints itself with (`radii.md` today — F4-16).
        ui::surface::shadow(
            cx.scene,
            cx.transform,
            rect,
            f64::from(cx.theme.tokens.radii.md),
            &cx.theme.tokens.elevation.e4,
        );
        self.menu.draw(
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
    /// [`ShellLayer::semantic_intent`] resolves.
    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        use ui::a11y::{AccessState, Role};
        if !self.is_open() {
            return None;
        }
        let hovered = self.menu.hovered();
        let actions: Vec<(usize, TabMenuAction)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.map(|action| (index, action)))
            .collect();
        let count = actions.len();
        let children = actions
            .iter()
            .enumerate()
            .map(|(ordinal, &(index, action))| {
                let node = AccessNode::new(Role::MenuItem)
                    .with_name(action.label())
                    .with_semantic_key(action.semantic_key())
                    .with_action_target(action.semantic_key())
                    .with_position(ordinal.saturating_add(1), count)
                    .with_state(AccessState {
                        focusable: true,
                        focused: hovered == Some(index),
                        ..AccessState::default()
                    });
                match self.menu.item_rect(index) {
                    Some(rect) => node.with_bounds(rect),
                    None => node,
                }
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_name(t_args(
                    "host-a11y-tab-menu",
                    &[("title", self.title.clone().into())],
                ))
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
            [ordinal] => self
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
    /// group has no split.
    fn semantic_intent(&self, target: &str) -> Option<LayerIntent> {
        let action = TabMenuAction::from_semantic_key(target)?;
        self.item_index_of(action)?;
        Some(LayerIntent::TabMenu {
            target: self.target?,
            action,
        })
    }

    fn dismiss(&mut self) -> bool {
        let open = self.is_open();
        self.close();
        open
    }
}
