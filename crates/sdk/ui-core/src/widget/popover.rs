//! `Popover` — an anchored overlay **menu of selectable options**.
//!
//! Where [`ContextMenu`](crate::widget::context_menu::ContextMenu) is an
//! *action* list, this is the primitive for picking one option out of a
//! short set next to the control that owns the value — a cycle-on-click
//! pill, a units selector, a refresh-cadence menu.
//!
//! Provides once, for every consumer: anchored placement (flips above /
//! clamps horizontally when there's no room below), a current selection
//! distinct from the keyboard/hover highlight (so re-opening lands on the
//! active value), full keyboard nav, focus capture while open
//! ([`modal_capture`](Widget::modal_capture) / [`traps_focus`](Widget::traps_focus)),
//! `Role::Menu` accessibility, and dismiss-on-outside-click.
//!
//! Usable two ways: mounted as a real tree widget, or driven by a
//! hand-drawn host via [`Popover::open_at`], [`Popover::item_at`],
//! [`Popover::move_highlight`], [`Popover::select`] and
//! [`Popover::take_selection`].

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Height of one option row.
const ROW_H: f64 = 44.0;
/// Vertical padding above the first / below the last row.
const PAD_Y: f64 = 6.0;
/// Horizontal padding inside a row (left of the check column, right of text).
const PAD_X: f64 = 12.0;
/// Width reserved at the left of every row for the selected check mark.
const CHECK_W: f64 = 20.0;
/// Gap between the anchor edge and the menu body.
const GAP: f64 = 6.0;
/// Floor on the menu width so a single short label still reads as a menu.
const MIN_W: f64 = 160.0;

/// An anchored overlay menu of selectable string options. See the module
/// docs for the full contract.
pub struct Popover {
    items: Vec<String>,
    /// Accessible name for the menu as a whole (e.g. "Refresh interval").
    label: Option<String>,
    open: bool,
    /// The control the menu is anchored to (positions the body).
    anchor: Rect,
    /// Bounds used for flip-above / horizontal-clamp resolution. Defaults to
    /// a very large region so an un-configured popover honours "below".
    viewport: Rect,
    /// Resolved body rect, computed by [`Popover::place`] / `draw`.
    rect: Rect,
    /// Row the keyboard / hover highlight sits on (index into `items`).
    highlighted: usize,
    /// The currently-applied option (drawn check-marked), if any.
    selected: Option<usize>,
    /// Last cursor position in logical coords (from `CursorMoved`).
    last_cursor: Point,
    /// One-shot channel: the row the user just chose, drained via
    /// [`Popover::take_selection`].
    pending: Option<usize>,
    layout_theme: Theme,
}

impl Default for Popover {
    fn default() -> Self {
        Self::new()
    }
}

impl Popover {
    /// A closed, empty popover with an infinite default viewport.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            label: None,
            open: false,
            anchor: Rect::ZERO,
            viewport: Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
            rect: Rect::ZERO,
            highlighted: 0,
            selected: None,
            last_cursor: Point::ZERO,
            pending: None,
            layout_theme: Theme::dark(),
        }
    }

    #[must_use]
    pub fn with_items<I, S>(mut self, items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.set_items(items);
        self
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Replace the option labels. Clamps the highlight into range.
    pub fn set_items<I, S>(&mut self, items: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.items = items.into_iter().map(Into::into).collect();
        if self.highlighted >= self.items.len() {
            self.highlighted = self.items.len().saturating_sub(1);
        }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = Some(label.into());
    }

    /// Override the viewport used for flip-above / clamp resolution — pass the
    /// host window's inner rect so the menu never opens off-screen.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Open the menu anchored to `anchor`, with `selected` pre-highlighted.
    /// The highlight starts on the selected row (or the first row when there
    /// is no current selection) so keyboard use and the drawn check agree.
    pub fn open_at(&mut self, anchor: Rect, selected: Option<usize>) {
        self.anchor = anchor;
        self.selected = selected.filter(|&i| i < self.items.len());
        self.highlighted = self
            .selected
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        self.open = true;
        self.pending = None;
    }

    /// Close the menu (safe when already closed).
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    /// The resolved body rect (valid after `place`/`draw`).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Move the highlight by `dir` (clamped to the ends, no wrap).
    pub fn move_highlight(&mut self, dir: i32) {
        if self.items.is_empty() {
            return;
        }
        let max = self.items.len().saturating_sub(1);
        self.highlighted = if dir.is_negative() {
            let step = usize::try_from(dir.unsigned_abs()).unwrap_or(usize::MAX);
            self.highlighted.saturating_sub(step)
        } else {
            let step = usize::try_from(dir).unwrap_or(0);
            self.highlighted.saturating_add(step).min(max)
        };
    }

    pub fn highlight_first(&mut self) {
        self.highlighted = 0;
    }

    pub fn highlight_last(&mut self) {
        self.highlighted = self.items.len().saturating_sub(1);
    }

    /// Record `idx` as selected + pending and close. Out-of-range indices
    /// are ignored.
    pub fn select(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.selected = Some(idx);
            self.highlighted = idx;
            self.pending = Some(idx);
            self.open = false;
        }
    }

    /// Choose the currently-highlighted option (Enter / click on the row).
    pub fn select_highlighted(&mut self) {
        let idx = self.highlighted;
        self.select(idx);
    }

    /// Drain the index of the option the user just chose. Returns `Some(idx)`
    /// exactly once per selection, then `None`.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.pending.take()
    }

    /// The option row under a logical `point`, if the menu is open and the
    /// point lands inside the body.
    pub fn item_at(&self, point: Point) -> Option<usize> {
        if !self.open || !self.rect.contains(point) {
            return None;
        }
        let rel = point.y - self.rect.y0 - PAD_Y;
        if rel < 0.0 {
            return None;
        }
        let row = (rel / ROW_H).floor();
        // `point` is already checked against `self.rect` above, so `rel` —
        // and therefore `row` — is bounded by the popover's own on-screen
        // height and never approaches usize::MAX.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "row is non-negative (checked via `rel < 0.0` above) and \
                      bounded by the popover's own on-screen height"
        )]
        let idx = row as usize;
        (idx < self.items.len()).then_some(idx)
    }

    /// Bounds of one option row after [`Self::place`] has resolved the menu.
    /// Hand-drawn hosts and tests can synthesize a click without copying the
    /// popover's private padding and row-height constants.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        if !self.open || index >= self.items.len() {
            return None;
        }
        Some(self.row_rect(index))
    }

    pub fn contains(&self, point: Point) -> bool {
        self.open && self.rect.contains(point)
    }

    /// Resolve `self.rect` from the anchor, measured option widths, and the
    /// viewport. Callable standalone so a hand-drawn host — or a test — can
    /// place the menu without a full draw.
    #[allow(
        clippy::cast_precision_loss,
        reason = "n is the popover's own row count — never remotely close to \
                  f64's exact-integer range"
    )]
    pub fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let fs = renderer_type_size(tr, theme.tokens.typography.scale.sm);
        let mut w = MIN_W.max(self.anchor.width());
        for label in &self.items {
            let tw = f64::from(tr.measure(label, fs, None).0);
            w = w.max(tw + CHECK_W + PAD_X * 2.0);
        }
        let n = self.items.len().max(1);
        let h = PAD_Y * 2.0 + n as f64 * ROW_H;

        let mut x0 = self.anchor.x0;
        let mut y0 = self.anchor.y1 + GAP;
        // Flip above the anchor when opening below would spill past the
        // viewport and there IS room above.
        if y0 + h > self.viewport.y1 && (self.anchor.y0 - GAP - h) >= self.viewport.y0 {
            y0 = self.anchor.y0 - GAP - h;
        }
        // Clamp horizontally so the body stays fully inside the viewport.
        if x0 + w > self.viewport.x1 {
            x0 = (self.viewport.x1 - w).max(self.viewport.x0);
        }
        if x0 < self.viewport.x0 {
            x0 = self.viewport.x0;
        }
        self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "idx is a row index into this popover's own (short) item \
                  list — never remotely close to f64's exact-integer range"
    )]
    fn row_rect(&self, idx: usize) -> Rect {
        let y0 = self.rect.y0 + PAD_Y + idx as f64 * ROW_H;
        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + ROW_H)
    }

    fn draw_check(scene: &mut Scene, t: Affine, row: Rect, color: vello::peniko::Color) {
        let cx = row.x0 + PAD_X + CHECK_W / 2.0 - 6.0;
        let cy = row.center().y;
        let mut path = BezPath::new();
        path.move_to((cx - 4.0, cy));
        path.line_to((cx - 1.0, cy + 3.5));
        path.line_to((cx + 5.0, cy - 4.5));
        scene.stroke(
            &Stroke::new(1.8)
                .with_caps(vello::kurbo::Cap::Round)
                .with_join(vello::kurbo::Join::Round),
            t,
            color,
            None,
            &path,
        );
    }
}

impl Widget for Popover {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // A zero-extent leaf: the body is positioned manually against the
        // anchor + viewport in `place`, not by the parent's flexbox. Register
        // in the node map so the host's overlay/hit sweeps still reach us.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // Resolve the body rect so hit-testing works after layout even before
        // the first draw. Preserve the active theme and text scale so the hit
        // target matches the first painted frame under large text.
        if self.open {
            let theme = self.layout_theme.clone();
            crate::text_render::with_layout_renderer(theme.text_scale, |tr| self.place(tr, &theme));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        self.place(text_renderer, theme);
        let c = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.md);

        // A floating Panel casts the `e4` tier (HIG §9.1) through the one
        // shadow painter, so its shadow follows the panel's own outline and
        // its strength comes from the theme rather than a literal black
        // ring stack that weighed the same on a paper-white ground.
        crate::surface::shadow(
            scene,
            transform,
            self.rect,
            radius,
            &theme.tokens.elevation.e4,
        );

        // Panel role (HIG §9.1): the panel ground, walked clear of both the
        // desktop and the window card, with the derived panel edge as a
        // crisp 1 px inset stroke — never a declared `border_standard`,
        // which is invisible on the dark preset's raised tier.
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        let fs = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
        for (idx, label) in self.items.iter().enumerate() {
            let row = self.row_rect(idx);
            let is_hl = idx == self.highlighted;
            if is_hl {
                let hl = Rect::new(row.x0 + 4.0, row.y0 + 1.0, row.x1 - 4.0, row.y1 - 1.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    c.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
                    None,
                    &RoundedRect::from_rect(hl, f64::from(theme.tokens.radii.sm)),
                );
            }
            let text_color = if is_hl {
                c.on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST)
            } else {
                c.text_primary
            };
            if self.selected == Some(idx) {
                // The tick is a mark, not text, so it answers to the lower
                // non-text floor — but it still has to answer to something.
                let check_color = if is_hl {
                    c.on_accent(crate::tokens::colors::NON_TEXT_MIN_CONTRAST)
                } else {
                    c.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
                };
                Self::draw_check(scene, transform, row, check_color);
            }
            let (_lw, lh) = text_renderer.measure_weighted(label, fs, None, TextWeight::Medium);
            let ty = row.center().y - f64::from(lh) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                label,
                fs,
                text_color,
                transform.pre_translate(Vec2::new(row.x0 + PAD_X + CHECK_W, ty)),
                None,
                TextWeight::Medium,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(idx) = self.item_at(self.last_cursor) {
                    self.highlighted = idx;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                match key {
                    Key::Named(NamedKey::ArrowDown) => self.move_highlight(1),
                    Key::Named(NamedKey::ArrowUp) => self.move_highlight(-1),
                    Key::Named(NamedKey::Home) => self.highlight_first(),
                    Key::Named(NamedKey::End) => self.highlight_last(),
                    Key::Named(NamedKey::Enter) => self.select_highlighted(),
                    Key::Named(NamedKey::Escape) => self.close(),
                    _ => {}
                }
                // Focus capture: an open menu owns the keyboard. EVERY key press
                // is consumed so nothing leaks to the app behind the overlay.
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.item_at(self.last_cursor) {
                    self.select(idx);
                } else {
                    // Outside click dismisses; swallow it so the press that
                    // closed the menu doesn't also hit whatever sits beneath.
                    self.close();
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Popover"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// An open popover traps the keyboard + swallows click-through, so it
    /// participates in the host's modal-capture contract.
    fn modal_capture(&self) -> Option<Rect> {
        self.open.then_some(self.rect)
    }

    /// An open popover captures focus so Tab/arrow shortcuts don't drive the
    /// app behind it.
    fn traps_focus(&self) -> bool {
        self.open
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let n = self.items.len();
        let children = self
            .items
            .iter()
            .enumerate()
            .map(|(i, label)| {
                AccessNode::new(Role::MenuItem)
                    .with_name(label.clone())
                    .with_position(i.saturating_add(1), n)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.open && i == self.highlighted,
                        selected: self.selected == Some(i),
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_optional_name(self.label.clone())
                .with_state(AccessState {
                    expanded: Some(self.open),
                    hidden: !self.open,
                    ..Default::default()
                })
                .with_children(children),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::{DeviceId, KeyEvent};
    use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

    #[test]
    fn menu_pointer_selection_dismissal_and_targets() {
        // Case `click_on_a_row_selects_it_click_outside_dismisses`.
        {
            let mut p = opened();
            let row2 = p.item_rect(2).expect("published item geometry");
            assert_eq!(p.item_at(row2.center()), Some(2));
            assert!(p.item_rect(p.items().len()).is_none());
            dispatch(&mut p, &cursor_at(row2.center()));
            assert!(dispatch(&mut p, &left_press()), "row click consumed");
            assert_eq!(p.take_selection(), Some(2), "clicking a row selects it");
            assert!(!p.is_open());

            // Re-open and click well outside → dismiss, no selection.
            p.open_at(Rect::new(100.0, 40.0, 180.0, 68.0), Some(2));
            let mut tr = TextRenderer::new();
            p.place(&mut tr, &Theme::dark());
            let outside = Point::new(p.rect().x1 + 80.0, p.rect().y1 + 80.0);
            dispatch(&mut p, &cursor_at(outside));
            assert!(dispatch(&mut p, &left_press()), "outside click swallowed");
            assert!(!p.is_open(), "outside click dismisses");
            assert_eq!(p.take_selection(), None, "no selection on dismiss");
        }

        // Case `option_rows_meet_the_shared_touch_target_floor`.
        {
            let p = opened();
            assert!(p.row_rect(0).height() >= 44.0);
            assert!(p.row_rect(4).height() >= 44.0);
        }

        // Case `retained_layout_keeps_the_active_theme_before_first_paint`.
        {
            let mut popover = opened();
            let mut taffy = TaffyTree::new();
            let mut map = HashMap::new();
            popover.build_layout(&mut taffy, &Theme::dark().with_text_scale(2.0), &mut map);
            assert_eq!(popover.layout_theme.text_scale, 2.0);
        }
    }

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn opened() -> Popover {
        let mut p = Popover::new()
            .with_label("Refresh interval")
            .with_items(["1s", "1.5s", "2s", "3s", "5s"]);
        p.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        p.open_at(Rect::new(100.0, 40.0, 180.0, 68.0), Some(1));
        // Resolve the body rect for hit-testing (no GPU needed).
        let mut tr = TextRenderer::new();
        p.place(&mut tr, &Theme::dark());
        p
    }

    fn cursor_at(pt: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(pt.x, pt.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn key(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn char_key(s: &str) -> WindowEvent {
        use winit::keyboard::SmolStr;
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(p: &mut Popover, event: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        p.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn menu_keyboard_moves_clamps_selects_and_swallows_keys() {
        // Case `arrow_keys_move_highlight_and_are_consumed`.
        {
            let mut p = opened();
            assert_eq!(p.highlighted(), 1, "opens highlighting the selected row");
            assert!(
                dispatch(&mut p, &key(NamedKey::ArrowDown)),
                "ArrowDown consumed"
            );
            assert_eq!(p.highlighted(), 2);
            assert!(
                dispatch(&mut p, &key(NamedKey::ArrowUp)),
                "ArrowUp consumed"
            );
            assert_eq!(p.highlighted(), 1);
        }

        // Case `home_and_end_jump_to_the_ends`.
        {
            let mut p = opened();
            assert!(dispatch(&mut p, &key(NamedKey::End)));
            assert_eq!(p.highlighted(), 4, "End jumps to the last row");
            assert!(dispatch(&mut p, &key(NamedKey::Home)));
            assert_eq!(p.highlighted(), 0, "Home jumps to the first row");
        }

        // Case `highlight_clamps_at_the_ends_without_wrapping`.
        {
            let mut p = opened();
            for _ in 0..10 {
                dispatch(&mut p, &key(NamedKey::ArrowDown));
            }
            assert_eq!(p.highlighted(), 4, "ArrowDown clamps at the last row");
            for _ in 0..10 {
                dispatch(&mut p, &key(NamedKey::ArrowUp));
            }
            assert_eq!(p.highlighted(), 0, "ArrowUp clamps at the first row");
        }

        // Case `enter_selects_the_highlight_closes_and_reports_it`.
        {
            let mut p = opened();
            dispatch(&mut p, &key(NamedKey::ArrowDown)); // 1 -> 2
            dispatch(&mut p, &key(NamedKey::ArrowDown)); // 2 -> 3
            assert!(dispatch(&mut p, &key(NamedKey::Enter)), "Enter consumed");
            assert!(!p.is_open(), "selecting closes the menu");
            assert_eq!(
                p.take_selection(),
                Some(3),
                "the chosen index is reported once"
            );
            assert_eq!(p.take_selection(), None, "selection drains exactly once");
            assert_eq!(
                p.selected(),
                Some(3),
                "the applied option sticks as selected"
            );
        }

        // Case `escape_closes_without_selecting`.
        {
            let mut p = opened();
            assert!(dispatch(&mut p, &key(NamedKey::Escape)), "Esc consumed");
            assert!(!p.is_open());
            assert_eq!(p.take_selection(), None, "Esc makes no selection");
        }

        // Case `unrelated_keys_are_swallowed_not_leaked`.
        {
            fn unrelated_keys_are_swallowed_not_leaked() {
                // Focus capture: any key press while open is consumed so it can't
                // reach the app behind the overlay. A letter that maps to no action
                // must still return `handled == true` and leave the menu open.
                let mut p = opened();
                assert!(
                    dispatch(&mut p, &char_key("x")),
                    "an unrelated key is swallowed"
                );
                assert!(p.is_open(), "a no-op key does not close the menu");
                assert_eq!(p.take_selection(), None);
                assert!(
                    dispatch(&mut p, &key(NamedKey::Tab)),
                    "Tab is swallowed too (no focus escape)"
                );
                assert!(p.is_open());
            }
            unrelated_keys_are_swallowed_not_leaked();
        }
    }

    #[test]
    fn menu_visibility_focus_trap_and_semantics() {
        // Case `accessibility_reports_menu_with_positioned_items`.
        {
            use crate::a11y::Role;
            let p = opened();
            let node = Widget::accessibility(&p).expect("popover is semantic");
            assert_eq!(node.role, Role::Menu);
            assert_eq!(node.name.as_deref(), Some("Refresh interval"));
            assert_eq!(node.state.expanded, Some(true), "open menu is expanded");
            assert!(!node.state.hidden, "open menu is visible in the tree");
            assert_eq!(node.children.len(), 5, "one MenuItem per option");
            for (i, child) in node.children.iter().enumerate() {
                assert_eq!(child.role, Role::MenuItem);
                assert_eq!(child.pos_in_set, Some(i + 1));
                assert_eq!(child.set_size, Some(5));
            }
            // The highlighted row carries focus; the selected row carries selected.
            assert!(
                node.children[1].state.focused,
                "row 1 opens highlighted (== selected)"
            );
            assert!(
                node.children[1].state.selected,
                "row 1 is the current selection"
            );
            assert!(!node.children[0].state.focused);
        }

        // Case `closed_menu_is_hidden_and_paints_nothing`.
        {
            use vello_encoding::DrawTag;
            let mut p = Popover::new().with_items(["a", "b"]);
            let node = Widget::accessibility(&p).unwrap();
            assert!(node.state.hidden, "closed menu is hidden from the tree");
            assert_eq!(node.state.expanded, Some(false));

            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert_eq!(fills, 0, "a closed popover emits no fills");
        }

        // Case `open_menu_traps_focus_and_captures_input`.
        {
            let p = opened();
            assert!(Widget::traps_focus(&p), "open popover traps focus");
            assert!(
                Widget::modal_capture(&p).is_some(),
                "open popover reports modal capture"
            );
            let closed = Popover::new();
            assert!(!Widget::traps_focus(&closed));
            assert!(Widget::modal_capture(&closed).is_none());
        }

        // Case `reopen_lands_the_highlight_on_the_current_selection`.
        {
            let mut p = opened();
            p.select(3);
            assert_eq!(p.take_selection(), Some(3));
            p.open_at(Rect::new(100.0, 40.0, 180.0, 68.0), p.selected());
            assert_eq!(
                p.highlighted(),
                3,
                "re-opening highlights the applied option"
            );
        }
    }
}
