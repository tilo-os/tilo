//! What a tree offers beyond its rows: a context menu, hover row actions,
//! an action beside the heading, tooltips, and the paint that makes a drag
//! legible (a floating copy of the dragged row, indent guides, a scroll
//! thumb). The tree owns the geometry, hit-testing, painting and semantics
//! of each; the host supplies content and receives intents.

use super::{
    Affine, Icon, NodeId, Point, Rect, Scene, TextRenderer, Theme, TreeModel, TreeRowKey, TreeView,
};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::num::f32_of_f64;
use crate::text_render::TextStyle;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{ChipFace, ChipLead, Widget};

/// Prefix of the context menu's semantic targets inside the tree.
const MENU_TARGET: &str = "menu:";
/// Semantic target of the heading action.
pub(super) const HEADING_ACTION_TARGET: &str = "heading-action";

/// A compact action shown at the trailing edge of the hovered row (a "new
/// child" plus, a "more" ellipsis). Assistive technology sees every row's
/// actions as buttons named `"{accessible_name} {row label}"`.
#[derive(Clone, Debug)]
pub struct TreeRowAction {
    pub icon: Icon,
    /// Shown after a dwell over the action.
    pub tooltip: String,
    /// Spoken before the row's label.
    pub accessible_name: String,
    /// Open the tree's context menu for the row, anchored under the action,
    /// instead of reporting [`TreeView::on_row_action`].
    pub opens_context_menu: bool,
}

impl TreeRowAction {
    #[must_use]
    pub fn new(icon: Icon, tooltip: impl Into<String>) -> Self {
        let tooltip = tooltip.into();
        Self {
            icon,
            accessible_name: tooltip.clone(),
            tooltip,
            opens_context_menu: false,
        }
    }

    #[must_use]
    pub fn accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    #[must_use]
    pub fn opens_context_menu(mut self) -> Self {
        self.opens_context_menu = true;
        self
    }
}

/// An icon button at the trailing edge of the heading band (a sidebar's
/// "new item").
#[derive(Clone, Debug)]
pub struct TreeHeadingAction {
    pub icon: Icon,
    pub tooltip: String,
    pub accessible_name: String,
    pub description: Option<String>,
}

impl TreeHeadingAction {
    #[must_use]
    pub fn new(icon: Icon, tooltip: impl Into<String>) -> Self {
        let tooltip = tooltip.into();
        Self {
            icon,
            accessible_name: tooltip.clone(),
            tooltip,
            description: None,
        }
    }

    #[must_use]
    pub fn accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

fn is_button_target(target: &str) -> bool {
    target == HEADING_ACTION_TARGET
        || target.starts_with("row-action:")
        || target.starts_with("section-action:")
        || target.starts_with("move:")
}

impl<T, M: TreeModel> TreeView<T, M> {
    /// Offer a context menu: right click on a row (which selects it) or on
    /// empty space (`None`), the `Menu` key or `Shift+F10` on the focused
    /// row, a row action that [opens the menu](TreeRowAction::opens_context_menu),
    /// and the accessibility `ShowContextMenu` verb. `items` builds the menu
    /// for its target each time it opens; an empty list opens nothing.
    #[must_use]
    pub fn with_context_menu<F>(mut self, items: F) -> Self
    where
        F: FnMut(&M, Option<NodeId>) -> Vec<MenuItem> + 'static,
    {
        self.context_menu_items = Some(Box::new(items));
        self
    }

    /// Receive the chosen context-menu item: the menu's target and the
    /// item's index in the list [`Self::with_context_menu`] returned
    /// (separators count).
    #[must_use]
    pub fn on_context_menu_item<F: FnMut(Option<NodeId>, usize) + 'static>(mut self, f: F) -> Self {
        self.on_context_menu_item = Some(Box::new(f));
        self
    }

    /// Show `actions` at the trailing edge of the hovered row.
    #[must_use]
    pub fn with_row_actions(mut self, actions: Vec<TreeRowAction>) -> Self {
        self.affordance_focus_valid = false;
        self.row_actions = actions;
        self
    }

    /// Receive a row action: the row and the action's index.
    #[must_use]
    pub fn on_row_action<F: FnMut(NodeId, usize) + 'static>(mut self, f: F) -> Self {
        self.on_row_action = Some(Box::new(f));
        self
    }

    /// Place an icon button at the trailing edge of the heading band.
    #[must_use]
    pub fn with_heading_action(mut self, action: TreeHeadingAction) -> Self {
        self.affordance_focus_valid = false;
        self.heading_action = Some(action);
        self
    }

    #[must_use]
    pub fn on_heading_action<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_heading_action = Some(Box::new(f));
        self
    }

    /// A second tooltip line under a row's label, naming what the row does
    /// ("Click to open · double-click to rename").
    #[must_use]
    pub fn with_row_hint(mut self, hint: impl Into<String>) -> Self {
        self.row_hint = Some(hint.into());
        self
    }

    /// Draw a hairline per depth level through nested rows, so deep
    /// hierarchies keep their structure legible.
    #[must_use]
    pub fn with_indent_guides(mut self) -> Self {
        self.indent_guides = true;
        self
    }

    /// Hide tooltips while something the host paints covers the tree.
    pub fn suppress_tooltips(&mut self, suppressed: bool) {
        self.tooltips_suppressed = suppressed;
    }

    #[must_use]
    pub fn context_menu_is_open(&self) -> bool {
        self.menu.is_open()
    }

    pub fn close_context_menu(&mut self) {
        self.menu.close();
        self.menu_target = None;
    }

    /// Open the context menu for `target` (`None`: the tree itself) with its
    /// top-left corner at `at`. Returns whether a menu opened.
    pub fn open_context_menu(&mut self, target: Option<NodeId>, at: Point) -> bool {
        let Some(build) = self.context_menu_items.as_mut() else {
            return false;
        };
        let items = build(&self.model, target);
        if items.is_empty() {
            return false;
        }
        let mut menu = ContextMenu::new();
        for item in items {
            menu.push(item);
        }
        menu.open_at([f32_of_f64(at.x), f32_of_f64(at.y)]);
        self.menu = menu;
        self.menu_target = target;
        self.rename = None;
        true
    }

    /// Open the menu on a row the way a right click does: the row becomes
    /// the selection and the focus first.
    pub(super) fn open_row_menu(&mut self, id: NodeId, at: Point) -> bool {
        if self.open_nav_menu(id, at) {
            return true;
        }
        if self.context_menu_items.is_none() {
            return false;
        }
        if self.selected() != Some(id) || !self.selection.is_selected(&id) {
            self.apply_selection_gesture(id, false, false);
        }
        self.focused = Some(TreeRowKey::Node(id));
        self.open_context_menu(Some(id), at)
    }

    /// Open the menu from the keyboard, anchored under the focused row, or
    /// under the heading when no row is focused.
    pub(super) fn open_menu_from_keyboard(&mut self) -> bool {
        let inset = f64::from(self.spacing.s6);
        match self.command_node() {
            Some(id) => {
                self.reveal_focus();
                let at = self
                    .row_rect(id)
                    .map_or(Point::new(self.rect.x0, self.body_top()), |row| {
                        Point::new(row.x0 + inset, row.y1)
                    });
                self.open_row_menu(id, at)
            }
            None => self.open_context_menu(None, Point::new(self.rect.x0 + inset, self.body_top())),
        }
    }

    /// Report the menu's chosen item, if one was taken.
    pub(super) fn drain_menu_selection(&mut self) {
        if let Some(index) = self.menu.take_selection() {
            let target = self.menu_target;
            if !self.menu.is_open() {
                self.menu_target = None;
            }
            if let Some(callback) = &mut self.on_context_menu_item {
                callback(target, index);
            }
        }
    }

    /// Route input to an open context menu. Returns whether the menu
    /// consumed it; a click outside closes the menu and falls through.
    pub(super) fn route_context_menu(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        let mut consumed = false;
        self.menu.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut consumed,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.drain_menu_selection();
        if !self.menu.is_open() {
            self.menu_target = None;
        }
        if consumed {
            *ctx.handled = true;
        }
        consumed
    }

    /// The heading action's square, at the heading band's trailing edge.
    #[must_use]
    pub fn heading_action_rect(&self) -> Option<Rect> {
        self.heading_action.as_ref()?;
        if self.heading_h <= 0.0 {
            return None;
        }
        let margin = f64::from(self.spacing.s1);
        let side = (self.heading_h - 2.0 * margin).max(1.0);
        let x1 = self.rect.x1 - f64::from(self.spacing.s2);
        let y0 = self.rect.y0 + (self.heading_h - side) / 2.0;
        Some(Rect::new(x1 - side, y0, x1, y0 + side))
    }

    /// The laid-out rect of `id`'s row, when it is in the viewport.
    #[must_use]
    pub fn row_rect(&self, id: NodeId) -> Option<Rect> {
        self.visible_row_rect(self.visible_index_of(id)?)
    }

    /// The disclosure of `id`'s row: the leading caret column of a tree row
    /// (whether or not it is a branch), the trailing chevron or the whole
    /// header of a navigation row.
    #[must_use]
    pub fn disclosure_rect(&self, id: NodeId) -> Option<Rect> {
        let index = self.visible_index_of(id)?;
        let row = self.visible_row_rect(index)?;
        if let Some(nav) = self.nav_disclosure_rect(id, row) {
            return nav;
        }
        let (_, depth) = self.index.row_at(&self.model, index)?;
        let x0 = row.x0 + self.row_inset + crate::num::f64_of_usize(depth) * self.indent_px;
        Some(Rect::new(x0, row.y0, x0 + self.disclosure_w, row.y1))
    }

    /// The rect of row action `action` on `id`'s row. Actions paint only
    /// while the row is hovered, but their geometry is stable.
    #[must_use]
    pub fn row_action_rect(&self, id: NodeId, action: usize) -> Option<Rect> {
        self.row_action_rects(self.row_rect(id)?)
            .get(action)
            .copied()
    }

    /// Trailing-edge squares for the row actions on `row`, in action order.
    pub(super) fn row_action_rects(&self, row: Rect) -> Vec<Rect> {
        let count = self.row_actions.len();
        if count == 0 {
            return Vec::new();
        }
        if self.is_nav() {
            let icons: Vec<Icon> = self.row_actions.iter().map(|action| action.icon).collect();
            return self.nav_action_rects(row, &icons);
        }
        let margin = f64::from(self.spacing.s1) / 2.0;
        let side = (row.height() - 2.0 * margin).max(1.0);
        let gap = f64::from(self.spacing.s1) / 2.0;
        let right = row.x1 - f64::from(self.spacing.s2);
        let top = row.y0 + (row.height() - side) / 2.0;
        (0..count)
            .map(|i| {
                let from_end = crate::num::f64_of_usize(count - 1 - i);
                let x1 = right - from_end * (side + gap);
                Rect::new(x1 - side, top, x1, top + side)
            })
            .collect()
    }

    /// Whether the row actions of `key`'s row are showing.
    pub(super) fn row_actions_visible(&self, key: TreeRowKey) -> bool {
        !self.row_actions.is_empty()
            && matches!(key, TreeRowKey::Node(id) if self.model.shows_row_actions(id))
            && (self.hovered == Some(key) || (self.is_focused && self.focused == Some(key)))
            && !self.reorder.is_tracking()
            && !self.menu.is_open()
            && self.rename.is_none()
    }

    /// Act on a press over the heading action or a visible row action.
    /// Returns whether the press was one.
    pub(super) fn press_affordance(&mut self, point: Point) -> bool {
        if let Some(rect) = self.heading_action_rect()
            && rect.contains(point)
        {
            if let Some(callback) = &mut self.on_heading_action {
                callback();
            }
            return true;
        }
        if self.is_nav() && self.press_nav_section_action(point) {
            return true;
        }
        let Some(key @ TreeRowKey::Node(id)) = self.row_key_at(point) else {
            return false;
        };
        if !self.row_actions_visible(key) {
            return false;
        }
        let Some(row) = self.row_rect(id) else {
            return false;
        };
        let Some(index) = self
            .row_action_rects(row)
            .iter()
            .position(|rect| rect.contains(point))
        else {
            return false;
        };
        self.trigger_row_action(id, index);
        true
    }

    /// Perform row action `index` on `id`.
    pub(super) fn trigger_row_action(&mut self, id: NodeId, index: usize) -> bool {
        let Some(action) = self.row_actions.get(index) else {
            return false;
        };
        if action.opens_context_menu {
            let anchor = self
                .row_action_rect(id, index)
                .map_or(Point::new(self.rect.x0, self.body_top()), |rect| {
                    Point::new(rect.x0, rect.y1)
                });
            return self.open_row_menu(id, anchor);
        }
        if let Some(callback) = &mut self.on_row_action {
            callback(id, index);
        }
        true
    }

    /// Paint the heading action as the shared icon chip.
    pub(super) fn paint_heading_action(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (Some(action), Some(rect)) = (&self.heading_action, self.heading_action_rect()) else {
            return;
        };
        let hovered = self.last_cursor.is_some_and(|point| rect.contains(point));
        ChipFace::new(rect, "")
            .lead(ChipLead::Glyph(action.icon))
            .hovered(hovered)
            .focused(
                self.is_focused
                    && self.affordance_focus_valid
                    && self.focused_affordance.as_deref() == Some(HEADING_ACTION_TARGET),
            )
            .paint(scene, transform, text_renderer, theme);
    }

    /// Paint the hovered row's actions; the one under the pointer lifts.
    pub(super) fn paint_row_actions(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        row: Rect,
        ground: vello::peniko::Color,
    ) {
        let tokens = &theme.tokens;
        let lift = crate::chrome::selection_tier_on(
            theme,
            crate::chrome::SelectionState::new(false, true, false),
            ground,
        )
        .fill;
        for (action, rect) in self.row_actions.iter().zip(self.row_action_rects(row)) {
            let active = self.last_cursor.is_some_and(|point| rect.contains(point));
            if active {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    lift,
                    None,
                    &rect.to_rounded_rect(f64::from(tokens.radii.detail)),
                );
            }
            let inset = (rect.width() * 0.2).max(1.0);
            action.icon.draw(
                scene,
                transform,
                rect.inset(-inset),
                if active {
                    tokens.colors.text_primary
                } else {
                    tokens.colors.text_dim
                },
            );
        }
    }

    /// Hairlines through the indent columns of a row at `depth`.
    pub(super) fn paint_indent_guides(
        &self,
        scene: &mut Scene,
        transform: Affine,
        row: Rect,
        depth: usize,
        color: vello::peniko::Color,
    ) {
        if !self.indent_guides {
            return;
        }
        for level in 0..depth {
            let x = row.x0
                + self.row_inset
                + crate::num::f64_of_usize(level) * self.indent_px
                + self.disclosure_w / 2.0;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &Rect::new(x - 0.5, row.y0, x + 0.5, row.y1),
            );
        }
    }

    /// The dragged row, lifted off the list under the pointer: an overlay
    /// surface with its label, casting the `e4` "lifted" shadow.
    pub(super) fn paint_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        text_style: &TextStyle,
        line_h: f32,
    ) {
        if !self.reorder.is_active() {
            return;
        }
        let (Some(source), Some(cursor)) = (self.reorder.source().copied(), self.last_cursor)
        else {
            return;
        };
        let Some(label) = self.model.label(source) else {
            return;
        };
        let tokens = &theme.tokens;
        let height = self
            .row_rect(source)
            .map_or(self.viewport.estimated_height(), |row| row.height());
        // The lifted copy hangs below and beside the pointer, clear of the
        // seam the pointer is on, so the drop line it is aiming at shows.
        let offset = f64::from(tokens.spacing.s4);
        let top = cursor.y + f64::from(tokens.spacing.s2);
        if self.paint_nav_drag_ghost(
            scene,
            transform,
            text_renderer,
            theme,
            source,
            Rect::new(
                self.rect.x0 + offset,
                top,
                self.rect.x1 + offset,
                top + height,
            ),
        ) {
            return;
        }
        let inset = f64::from(tokens.spacing.s1);
        let ghost = Rect::new(
            self.rect.x0 + inset + offset,
            top,
            self.rect.x1 - inset + offset,
            top + height,
        );
        let radius = f64::from(tokens.radii.detail);
        crate::surface::shadow(scene, transform, ghost, radius, &tokens.elevation.floating);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.overlay,
            None,
            &ghost.to_rounded_rect(radius),
        );
        let x = ghost.x0 + self.disclosure_w;
        let max_w = (ghost.x1 - x - self.row_gap).max(0.0);
        let label = text_renderer.elide_to_width_style(label, f32_of_f64(max_w), text_style);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
        )]
        let xf = transform
            * Affine::translate((
                x,
                crate::widget::data_table::centered_text_top(ghost.y0, height, line_h),
            ));
        text_renderer.draw_style(
            scene,
            &label,
            tokens.colors.text_primary,
            xf,
            None,
            text_style,
        );
    }

    /// The rows fade out toward an edge while more of them can scroll in
    /// from there, as every scrolled list does.
    pub(super) fn paint_edge_fades(
        &self,
        scene: &mut Scene,
        transform: Affine,
        ground: vello::peniko::Color,
    ) {
        use crate::surface::ContentEdge;
        if !self.fill || self.viewport.viewport() <= 0.0 {
            return;
        }
        let body = Rect::new(self.rect.x0, self.body_top(), self.rect.x1, self.rect.y1);
        let offset = self.viewport.offset();
        if offset > 0.5 {
            crate::surface::content_edge_fade(scene, transform, body, ContentEdge::Top, ground);
        }
        if self.viewport.max_offset() - offset > 0.5 {
            crate::surface::content_edge_fade(scene, transform, body, ContentEdge::Bottom, ground);
        }
    }

    /// A quiet scroll thumb at the trailing edge while the rows overflow a
    /// [`Self::fill`] viewport. Like every overlay bar it shows while the
    /// rows scroll and fades out at rest.
    pub(super) fn paint_scroll_thumb(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.fill || self.viewport.viewport() <= 0.0 {
            return;
        }
        let metrics = crate::widget::scrollbar::ScrollMetrics::new(
            self.viewport.content_extent(),
            self.viewport.viewport(),
            self.viewport.offset(),
        );
        if !metrics.overflows() {
            return;
        }
        let now = crate::time::now();
        let offset = self.viewport.offset();
        if self
            .bar_offset
            .get()
            .is_some_and(|painted| (painted - offset).abs() > 0.5)
        {
            let mut fade = self.bar_fade.get();
            fade.wake(now);
            self.bar_fade.set(fade);
        }
        self.bar_offset.set(Some(offset));
        let s1 = f64::from(theme.tokens.spacing.s1);
        let track = Rect::new(
            self.rect.x1 - s1 * 1.5,
            self.body_top() + s1,
            self.rect.x1 - s1 / 2.0,
            self.rect.y1 - s1,
        );
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let near = self.last_cursor.is_some_and(|point| {
            point.x >= self.rect.x1 - gutter
                && point.x <= self.rect.x1
                && point.y >= track.y0
                && point.y <= track.y1
        });
        let held = theme.reduce_motion || near;
        self.bar_held.set(held);
        let opacity = self.bar_fade.get().opacity_at(now, held);
        if opacity <= 0.003 {
            return;
        }
        let bar = crate::widget::scrollbar::Scrollbar::vertical();
        if let Some(thumb) = bar.thumb_rect(track, metrics) {
            let radius = thumb.width().min(thumb.height()) / 2.0;
            let colour = crate::color_compat::ColorCompat::with_alpha_factor(
                crate::widget::scrollbar::thumb_color(theme, false),
                opacity,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colour,
                None,
                &vello::kurbo::RoundedRect::from_rect(thumb, radius),
            );
        }
    }

    /// The open context menu, painted in the window's top layer.
    pub(super) fn paint_context_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if self.menu.is_open() {
            self.menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    /// What a pointer at `point` would do, for the tooltip host.
    pub(super) fn affordance_tooltip(&self, point: Point) -> Option<(Rect, String)> {
        if self.tooltips_suppressed
            || self.menu.is_open()
            || self.rename.is_some()
            || self.reorder.is_tracking()
        {
            return None;
        }
        if let (Some(action), Some(rect)) = (&self.heading_action, self.heading_action_rect())
            && rect.contains(point)
        {
            return Some((rect, action.tooltip.clone()));
        }
        let TreeRowKey::Node(id) = self.row_key_at(point)? else {
            return None;
        };
        let row = self.row_rect(id)?;
        if self.row_actions_visible(TreeRowKey::Node(id))
            && let Some((action, rect)) = self
                .row_actions
                .iter()
                .zip(self.row_action_rects(row))
                .find(|(_, rect)| rect.contains(point))
        {
            return Some((rect, action.tooltip.clone()));
        }
        if let Some(tooltip) = self.nav_tooltip(id, row) {
            return Some(tooltip);
        }
        let hint = self.row_hint.as_ref()?;
        let label = self.model.label(id)?;
        Some((row, format!("{label}\n{hint}")))
    }

    /// The heading action as a semantic button.
    pub(super) fn heading_action_node(&self) -> Option<AccessNode> {
        let action = self.heading_action.as_ref()?;
        let rect = self.heading_action_rect()?;
        let mut node = AccessNode::new(Role::Button)
            .with_name(action.accessible_name.clone())
            .with_state(AccessState::new().focusable(true))
            .with_action_target(HEADING_ACTION_TARGET)
            .with_bounds(rect);
        node.state.focused = self.is_focused
            && self.affordance_focus_valid
            && self.focused_affordance.as_deref() == Some(HEADING_ACTION_TARGET);
        if let Some(description) = &action.description {
            node = node.with_description(description.clone());
        }
        Some(node)
    }

    /// Semantic children of a node row: its rename field while renaming,
    /// its row actions and its structural moves.
    pub(super) fn row_semantic_children(
        &self,
        id: NodeId,
        label: &str,
        row: Option<Rect>,
    ) -> Vec<AccessNode> {
        let mut children = Vec::new();
        let rename_input = if let Some(field) = self.rename.as_ref().filter(|field| field.id == id)
            && let Some(mut input) = Widget::accessibility(&field.input)
        {
            input.name = Some(crate::i18n!("tree-rename-field"));
            input.bounds = Some(field.input.get_rect().into());
            Some(input.with_action_target(format!("rename:{id}")))
        } else {
            None
        };
        if let Some(node) = self.nav_section_action_node(id, label, row) {
            children.push(node);
        }
        if let Some(row) = row.filter(|_| self.model.shows_row_actions(id)) {
            for (index, (action, rect)) in self
                .row_actions
                .iter()
                .zip(self.row_action_rects(row))
                .enumerate()
            {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name(format!("{} {label}", action.accessible_name))
                        .with_state(AccessState::new().focusable(true))
                        .with_action_target(format!("row-action:{index}:{id}"))
                        .with_bounds(rect),
                );
            }
        }
        if self.reorder_validator.is_some() && self.reorder_commit.is_some() {
            for movement in super::TreeMove::ALL {
                let available = self.move_target(id, movement).is_some();
                let mut node = AccessNode::new(Role::Button)
                    .with_name(crate::i18n::I18nManager::format(movement.label_key(), &[]))
                    .with_state(AccessState::new().focusable(available).disabled(!available))
                    .with_action_target(format!("move:{}:{id}", movement.token()));
                if let Some(row) = row {
                    node = node.with_bounds(row);
                }
                children.push(node);
            }
        }
        let focused_target = self.focused_affordance_target();
        for child in &mut children {
            child.state.focused =
                focused_target.is_some_and(|target| child.action_target.as_deref() == Some(target));
        }
        // Virtual affordances use the tree's focus target. The real inline
        // text field already reports its own focus; keep that state intact.
        if let Some(input) = rename_input {
            children.insert(0, input);
        }
        children
    }

    /// The open context menu's semantics, its targets namespaced into the
    /// tree's.
    pub(super) fn menu_node(&self) -> Option<AccessNode> {
        fn namespace(node: &mut AccessNode) {
            if let Some(target) = node.action_target.take() {
                let target = format!("{MENU_TARGET}{target}");
                node.semantic_key = Some(target.clone());
                node.action_target = Some(target);
            }
            for child in &mut node.children {
                namespace(child);
            }
        }
        if !self.menu.is_open() {
            return None;
        }
        let mut node = Widget::accessibility(&self.menu)?;
        namespace(&mut node);
        Some(node)
    }

    /// Resolve only an actionable, currently published button. The visible
    /// model and its current availability remain authoritative after focus.
    pub(super) fn available_affordance(&self, target: &str) -> Option<AccessNode> {
        if !is_button_target(target) {
            return None;
        }
        let heading = self.heading_action_node().into_iter();
        let rows = self
            .visible_row_range()
            .filter_map(|index| {
                let (TreeRowKey::Node(id), _) = self.index.row_at(&self.model, index)? else {
                    return None;
                };
                let label = self.model.label(id)?;
                Some(self.row_semantic_children(id, label, self.row_rect(id)))
            })
            .flatten();
        heading.chain(rows).find(|node| {
            node.action_target.as_deref() == Some(target)
                && node.state.focusable
                && !node.state.disabled
                && !node.state.read_only
        })
    }

    fn focus_affordance(&mut self, target: &str) -> bool {
        if self.available_affordance(target).is_none() {
            return false;
        }
        if let Some(id) = target
            .rsplit(':')
            .next()
            .and_then(|raw| raw.parse::<NodeId>().ok())
        {
            self.focused = Some(TreeRowKey::Node(id));
        }
        self.focused_affordance = Some(target.to_owned());
        self.affordance_focus_valid = true;
        self.is_focused = true;
        true
    }

    /// Handle the semantic targets the affordances publish. `None` when the
    /// target is not one of theirs.
    pub(super) fn perform_affordance_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Option<bool> {
        use crate::a11y::Action;
        if matches!(action, Action::Focus) && is_button_target(target) {
            return Some(self.focus_affordance(target));
        }
        if let Some(target) = target.strip_prefix(MENU_TARGET) {
            let performed = self
                .menu
                .perform_semantic_action(target, action, state, path);
            if performed {
                self.drain_menu_selection();
            }
            return Some(performed);
        }
        if matches!(action, Action::Activate)
            && is_button_target(target)
            && self.available_affordance(target).is_none()
        {
            return Some(false);
        }
        if target == HEADING_ACTION_TARGET {
            if !matches!(action, Action::Activate) || self.heading_action.is_none() {
                return Some(false);
            }
            if let Some(callback) = &mut self.on_heading_action {
                callback();
            }
            return Some(true);
        }
        if let Some(rest) = target.strip_prefix("row-action:") {
            let (index, id) = rest.split_once(':')?;
            let (Ok(index), Ok(id)) = (index.parse::<usize>(), id.parse::<NodeId>()) else {
                return Some(false);
            };
            if !matches!(action, Action::Activate) || !self.model.contains(id) {
                return Some(false);
            }
            return Some(self.trigger_row_action(id, index));
        }
        if let Some(id) = target.strip_prefix("section-action:") {
            let Ok(id) = id.parse::<NodeId>() else {
                return Some(false);
            };
            if !matches!(action, Action::Activate) {
                return Some(false);
            }
            return Some(self.trigger_nav_section_action(id));
        }
        if let Some(rest) = target.strip_prefix("move:") {
            let (movement, id) = rest.split_once(':')?;
            let (Some(movement), Ok(id)) = (super::TreeMove::from_token(movement), id.parse())
            else {
                return Some(false);
            };
            if !matches!(action, Action::Activate) || !self.model.contains(id) {
                return Some(false);
            }
            return Some(self.move_node(id, movement));
        }
        if let Some(id) = target.strip_prefix("rename:") {
            let Some(field) = self
                .rename
                .as_mut()
                .filter(|field| field.id.to_string() == id)
            else {
                return Some(false);
            };
            return Some(field.input.perform_action(action, &[], state, path));
        }
        None
    }
}
