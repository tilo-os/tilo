//! The retained parts of a column header the shell owns itself: the title
//! (with its count) and the More menu the actions overflow into.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::chrome::header_row;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{AppModel, Button, ModelPath, TaffyTreeExt, Widget};

/// A column's title in the header row: `Heading`, and an optional count in
/// a quiet pill after it. Measured by the host, so the actions after it
/// start where the title ends.
pub(super) struct HeaderTitle {
    pub(super) title: Option<String>,
    pub(super) count: Option<usize>,
    theme: Theme,
    rect: Rect,
    /// The title and count the last measure saw; a change needs another.
    measured: Option<(Option<String>, Option<usize>)>,
}

impl HeaderTitle {
    pub(super) fn new() -> Self {
        Self {
            title: None,
            count: None,
            theme: Theme::dark(),
            rect: Rect::ZERO,
            measured: None,
        }
    }

    pub(super) fn is_shown(&self) -> bool {
        self.title.is_some()
    }
}

impl Widget for HeaderTitle {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        self.measured = None;
        let style = Style {
            flex_shrink: 1.0,
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.measured = Some((self.title.clone(), self.count));
        let Some(title) = self.title.as_deref() else {
            return taffy::Size::ZERO;
        };
        let (w, h) = header_row::measure_title(text, &self.theme, title, self.count);
        taffy::Size {
            width: known.width.unwrap_or(to_f32(w.ceil())),
            height: known.height.unwrap_or(to_f32(h.ceil())),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if let Some(title) = self.title.as_deref() {
            header_row::paint_title_in(scene, transform, text, theme, self.rect, title, self.count);
        }
    }

    fn name(&self) -> &'static str {
        "NavShellTitle"
    }

    fn is_layout_dirty(&self) -> bool {
        self.measured
            .as_ref()
            .is_none_or(|(title, count)| *title != self.title || *count != self.count)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let title = self.title.clone()?;
        let mut node = AccessNode::new(Role::Heading { level: 2 }).with_name(title);
        if let Some(count) = self.count {
            node = node.with_value(count.to_string());
        }
        Some(node)
    }
}

/// The trailing More button of a header whose actions do not all fit, and
/// the menu it opens with the actions that did not.
pub(super) struct MoreActions {
    button: Button,
    menu: ContextMenu,
    requested: Rc<Cell<bool>>,
    chosen: Option<usize>,
    rect: Rect,
    viewport: Rect,
    /// Some actions did not fit: the button stands in the row. Otherwise it
    /// is laid out at nothing and the host meets no button.
    shown: bool,
}

impl MoreActions {
    pub(super) fn new(id: &str) -> Self {
        let requested = Rc::new(Cell::new(false));
        let button = Button::icon_only(id, Icon::MoreHorizontal, "More actions").on_view({
            let requested = Rc::clone(&requested);
            move || requested.set(true)
        });
        Self {
            button,
            menu: ContextMenu::new(),
            requested,
            chosen: None,
            rect: Rect::ZERO,
            viewport: Rect::ZERO,
            shown: false,
        }
    }

    /// Whether some actions moved into the menu (the button shows).
    pub(super) fn set_shown(&mut self, shown: bool) {
        self.shown = shown;
        if !shown {
            self.menu.close();
        }
    }

    /// The More button was pressed since the last call.
    pub(super) fn take_open_request(&self) -> bool {
        self.requested.replace(false)
    }

    /// Open the menu with `items` (one per action that did not fit, in
    /// order), anchored at the button, inside `viewport`.
    pub(super) fn open(&mut self, items: Vec<MenuItem>, viewport: Rect) {
        self.menu = ContextMenu::new();
        for item in items {
            self.menu.push(item);
        }
        self.viewport = viewport;
        self.menu.open_anchored(self.rect, viewport);
    }

    pub(super) fn close(&mut self) {
        self.menu.close();
    }

    /// The row the person chose in the menu, once.
    pub(super) fn take_choice(&mut self) -> Option<usize> {
        self.chosen.take()
    }

    /// The menu's rows, while it is open.
    #[cfg(test)]
    pub(super) fn menu_labels(&self) -> Vec<String> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.menu
            .items()
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Choose `row`, as a click on it does.
    #[cfg(test)]
    pub(super) fn choose(&mut self, row: usize) {
        self.menu.close();
        self.chosen = Some(row);
    }
}

impl Widget for MoreActions {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.button.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(Style::default(), &[child]);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(child) = taffy.children(node).ok().and_then(|c| c.first().copied()) {
            self.button
                .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
        if self.menu.is_open() {
            self.menu.reanchor(self.rect, self.viewport);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.menu.is_open() {
            self.menu.layout_within(text, theme);
        }
        self.button.draw(scene, transform, text, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.menu
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            if let Some(row) = self.menu.take_selection() {
                self.chosen = Some(row);
                *ctx.handled = true;
            }
            return;
        }
        let child = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|c| c.first().copied());
        if let Some(child) = child {
            let path = ctx.path;
            self.button
                .handle_event(event, &mut ctx.reborrow(path, child));
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.button.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.button.update(state, path)
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.shown {
            vec![&self.button]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.shown {
            visitor(&self.button);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.shown {
            vec![&mut self.button]
        } else {
            Vec::new()
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.shown && self.button.is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.shown && self.button.layout_needs_measure()
    }

    fn name(&self) -> &'static str {
        "NavShellMore"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout extents fit f32 exactly at every UI scale"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}
