//! `ConflictBanner` — narrow horizontal strip a CRDT-backed widget mounts
//! above itself when one or more unresolved conflicts exist on the doc.
//!
//! ```text
//!  [!]  File rename collision — 3 conflicts in this doc        [Review] [Dismiss]
//! ```
//!
//! Decorative + actionable: it never mutates the CRDT itself. `[Review]`
//! fires `on_review` (host typically opens a
//! [`crate::widget::conflict_picker::ConflictPicker`]); `[Dismiss]` fires
//! `on_dismiss` and hides the banner for this session — the host re-mounts
//! it next time the doc loads if conflicts persist.
//!
//! At `conflict_count == 0` the widget paints nothing and reports
//! `Rect::ZERO`, so the host can leave it permanently in the layout tree.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{renderer_type_size, text_band, type_line_height};

/// Banner height at the default text scale. Matches the activity-timeline
/// filter strip (~36 px) so a banner stacked above a timeline feels
/// rhythmic; at larger text scales the band grows to hold the label line
/// plus its padding (see [`ConflictBanner::banner_height`]).
pub const BANNER_HEIGHT: f64 = 36.0;
/// Padding between the banner edge and any content.
const PADDING: f64 = 8.0;
/// Width of the warning glyph slot at the leading edge.
const GLYPH_W: f64 = 20.0;
/// Width budget for the trailing chip cluster (Review + Dismiss).
const CHIP_REVIEW_W: f64 = 64.0;
const CHIP_DISMISS_W: f64 = 64.0;
const CHIP_GAP: f64 = 6.0;
/// Alpha applied to the theme's warning color when used as the
/// banner background — low enough that text overlay still reads as
/// "warning-tinted neutral", not a fluorescent panel.
const BANNER_BG_ALPHA: u8 = 51; // ~ 20%

/// Composes `base` with a translation for text placement. Affine
/// composition is floating-point matrix multiply — it cannot overflow or
/// panic the way integer arithmetic can.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

/// Callbacks fired when the user clicks one of the trailing chips.
pub type ReviewCallback = Box<dyn FnMut()>;
pub type DismissCallback = Box<dyn FnMut()>;

/// Small banner widget. See module-level docs for the contract.
pub struct ConflictBanner {
    family_name: String,
    conflict_count: usize,
    rect: Rect,
    last_cursor: Point,
    /// Hover state for the Review chip — drives a subtle background
    /// tint on hover so the user knows it's interactive.
    review_hovered: bool,
    dismiss_hovered: bool,
    on_review: Option<ReviewCallback>,
    on_dismiss: Option<DismissCallback>,
    dirty: bool,
    /// Band height resolved from the theme at layout time
    /// (`BANNER_HEIGHT` at the default text scale).
    height: f64,
    /// The theme's `sm` label size, recorded at `build_layout` so the
    /// renderer-aware layout hook can measure the chip labels.
    label_size: f32,
    /// Measured `(width, height)` of the chip labels, resolved in layout by
    /// [`Widget::adjust_layout_with_measure`] so the chip a click lands in is
    /// the chip that is painted; `(0, 0)` until then, when the chips keep
    /// their pinned widths.
    review_label: (f32, f32),
    dismiss_label: (f32, f32),
}

impl ConflictBanner {
    /// Build a banner for `family` (the conflict-family display name, e.g.
    /// `"Kanban move"`). Starts with `conflict_count = 0`, invisible until
    /// the host calls [`Self::set_conflict_count`].
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            family_name: family.into(),
            conflict_count: 0,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            review_hovered: false,
            dismiss_hovered: false,
            on_review: None,
            on_dismiss: None,
            dirty: true,
            height: BANNER_HEIGHT,
            label_size: crate::tokens::TypeScale::default().sm,
            review_label: (0.0, 0.0),
            dismiss_label: (0.0, 0.0),
        }
    }

    /// A chip holds its measured label plus the banner padding each side,
    /// floored at the pinned width so the 1× labels keep 64 px.
    fn chip_width(pinned: f64, label: (f32, f32)) -> f64 {
        text_band(pinned, f64::from(label.0), PADDING * 2.0)
    }

    /// The band the banner reserves under `theme`: its `sm` label line plus
    /// the leading and trailing padding, floored at [`BANNER_HEIGHT`] so the
    /// default text scale keeps the pinned 36 px.
    pub fn banner_height(theme: &Theme) -> f64 {
        text_band(
            BANNER_HEIGHT,
            type_line_height(theme.tokens.typography.scale.sm),
            PADDING * 2.0,
        )
    }

    /// Update the conflict count. Setting `0` makes the banner
    /// paint nothing on the next frame.
    pub fn set_conflict_count(&mut self, n: usize) {
        if self.conflict_count != n {
            self.conflict_count = n;
            self.dirty = true;
        }
    }

    pub fn conflict_count(&self) -> usize {
        self.conflict_count
    }

    pub fn family_name(&self) -> &str {
        &self.family_name
    }

    /// Test-only escape hatch: set the banner's rect without running the
    /// taffy layout pass, so a sibling-module test can seed a banner
    /// geometry without spinning up a full layout tree.
    #[cfg(test)]
    pub(crate) fn set_rect_for_test(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Install the `[Review]` callback. Typically opens a `ConflictPicker`
    /// over the same artifact.
    #[must_use]
    pub fn on_review<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_review = Some(Box::new(f));
        self
    }

    /// Install the `[Dismiss]` callback. Typically hides the banner for
    /// the session.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Computed rect for the `[Review]` chip. `None` in zero-conflict mode
    /// (the banner paints nothing, so the hit-test short-circuits).
    pub fn review_chip_rect(&self) -> Option<Rect> {
        if self.conflict_count == 0 {
            return None;
        }
        let chip_h = (self.rect.height() - 8.0).max(20.0);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        // Right-anchored cluster: Dismiss is rightmost, Review sits
        // immediately to its left with a CHIP_GAP between.
        let x_dismiss_x1 = self.rect.x1 - PADDING;
        let x_review_x1 =
            x_dismiss_x1 - Self::chip_width(CHIP_DISMISS_W, self.dismiss_label) - CHIP_GAP;
        Some(Rect::new(
            x_review_x1 - Self::chip_width(CHIP_REVIEW_W, self.review_label),
            cy - chip_h / 2.0,
            x_review_x1,
            cy + chip_h / 2.0,
        ))
    }

    /// Computed rect for the `[Dismiss]` chip. `None` when in
    /// zero-conflict mode.
    pub fn dismiss_chip_rect(&self) -> Option<Rect> {
        if self.conflict_count == 0 {
            return None;
        }
        let chip_h = (self.rect.height() - 8.0).max(20.0);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);
        let x_dismiss_x1 = self.rect.x1 - PADDING;
        Some(Rect::new(
            x_dismiss_x1 - Self::chip_width(CHIP_DISMISS_W, self.dismiss_label),
            cy - chip_h / 2.0,
            x_dismiss_x1,
            cy + chip_h / 2.0,
        ))
    }

    /// The "N conflicts in this doc" label. Singular/plural handled here
    /// so the picker + banner stay consistent.
    fn count_label(&self) -> String {
        if self.conflict_count == 1 {
            format!("{} \u{2014} 1 conflict in this doc", self.family_name)
        } else {
            format!(
                "{} \u{2014} {} conflicts in this doc",
                self.family_name, self.conflict_count
            )
        }
    }

    /// Paint one trailing chip (`Review` / `Dismiss`).
    fn draw_chip(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: (&str, (f32, f32)),
        hovered: bool,
    ) {
        let (label, measured) = label;
        // A Neutral button face (HIG §9.5): raised at rest, the theme's
        // hover fill when hovered, edge derived from the face — not a white
        // veneer that is invisible on a light palette.
        let body = if hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.raised
        };
        crate::surface::card(
            scene,
            transform,
            rect,
            f64::from(theme.tokens.radii.detail),
            body,
        );
        let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
        // Centre on the measurement the chip was sized from; a host that
        // laid out without the renderer-aware hook measures here instead.
        let (lw, lh) = if measured.0 > 0.0 {
            measured
        } else {
            text_renderer.measure(label, text_size, None)
        };
        text_renderer.draw_text(
            scene,
            label,
            text_size,
            theme.tokens.colors.text_primary,
            translated(
                transform,
                rect.x0 + (rect.width() - f64::from(lw)) / 2.0,
                rect.y0 + (rect.height() - f64::from(lh)) / 2.0,
            ),
            None,
        );
    }
}

impl Widget for ConflictBanner {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.height = Self::banner_height(theme);
        self.label_size = theme.tokens.typography.scale.sm;
        // In zero-conflict mode we collapse to height 0 so the
        // banner doesn't reserve room in the column it sits inside.
        // Safe: the band is a small layout value (36 px at 1×), far below
        // f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the banner band is a small layout value (36px at the default scale)"
        )]
        let h: f32 = if self.conflict_count == 0 {
            0.0
        } else {
            self.height as f32
        };
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// The chip labels are measured here, during layout, so the chip rects
    /// hit-testing reads are the rects paint draws — never a stale guess
    /// from before the first frame. The banner's own style is unaffected.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let size = renderer_type_size(text, self.label_size);
        self.review_label = text.measure("Review", size, None);
        self.dismiss_label = text.measure("Dismiss", size, None);
        self.adjust_layout_style(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let prev = self.rect;
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if prev != self.rect {
            self.dirty = true;
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
        if self.conflict_count == 0 {
            self.dirty = false;
            return;
        }
        let warn = theme.tokens.colors.semantic.warning;
        let warn_rgba = warn.to_rgba8();
        let bg_color = Color::from_rgba8(warn_rgba.r, warn_rgba.g, warn_rgba.b, BANNER_BG_ALPHA);
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.detail));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            bg_color,
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(1.0), transform, warn, None, &shape);

        let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
        let glyph_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.base);
        let cy = f64::midpoint(self.rect.y0, self.rect.y1);

        let glyph = "!";
        let (gw, gh) = text_renderer.measure(glyph, glyph_size, None);
        let glyph_x = self.rect.x0 + PADDING + (GLYPH_W - f64::from(gw)) / 2.0;
        text_renderer.draw_text(
            scene,
            glyph,
            glyph_size,
            warn,
            translated(transform, glyph_x, cy - f64::from(gh) / 2.0),
            None,
        );

        let label = self.count_label();
        let label_x = self.rect.x0 + PADDING + GLYPH_W + 6.0;
        let (_lw, lh) = text_renderer.measure(&label, text_size, None);
        text_renderer.draw_text(
            scene,
            &label,
            text_size,
            theme.tokens.colors.text_primary,
            translated(transform, label_x, cy - f64::from(lh) / 2.0),
            None,
        );

        // Chip widths come from the labels measured in layout (see
        // `adjust_layout_with_measure`), the same measurement the chip
        // centres its label with.
        if let Some(rect) = self.review_chip_rect() {
            Self::draw_chip(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                ("Review", self.review_label),
                self.review_hovered,
            );
        }
        if let Some(rect) = self.dismiss_chip_rect() {
            Self::draw_chip(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                ("Dismiss", self.dismiss_label),
                self.dismiss_hovered,
            );
        }

        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.conflict_count == 0 {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let new_review = self.review_chip_rect().is_some_and(|r| r.contains(p));
                let new_dismiss = self.dismiss_chip_rect().is_some_and(|r| r.contains(p));
                if new_review != self.review_hovered || new_dismiss != self.dismiss_hovered {
                    self.review_hovered = new_review;
                    self.dismiss_hovered = new_dismiss;
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(rect) = self.review_chip_rect()
                    && rect.contains(self.last_cursor)
                {
                    if let Some(cb) = self.on_review.as_mut() {
                        cb();
                    }
                    *ctx.handled = true;
                    return;
                }
                if let Some(rect) = self.dismiss_chip_rect()
                    && rect.contains(self.last_cursor)
                {
                    if let Some(cb) = self.on_dismiss.as_mut() {
                        cb();
                    }
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ConflictBanner"
    }

    /// Accessibility: a conflict alert banner. `Role::Group` named for the
    /// conflict family, with the count in the description ("3 conflicts"). When
    /// the count is 0 the banner paints nothing, so it is marked `hidden` so it
    /// drops out of what an AT announces.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(format!("{} conflicts", self.family_name))
                .with_description(format!("{} unresolved", self.conflict_count))
                .with_state(AccessState {
                    hidden: self.conflict_count == 0,
                    ..Default::default()
                }),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("conflict_banner")
    }

    fn get_rect(&self) -> Rect {
        if self.conflict_count == 0 {
            Rect::ZERO
        } else {
            self.rect
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::Modifiers;

    fn banner_at(rect: Rect, count: usize) -> ConflictBanner {
        let mut b = ConflictBanner::new("Kanban move");
        b.rect = rect;
        b.set_conflict_count(count);
        b
    }

    #[test]
    fn conflict_banner_paints_and_hides_by_count() {
        fn accessibility_reports_group_role_and_hidden_when_empty() {
            use crate::a11y::Role;
            let b = banner_at(Rect::new(0.0, 0.0, 400.0, 40.0), 3);
            let node = Widget::accessibility(&b).expect("banner is not presentational");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Kanban move conflicts"));
            assert_eq!(node.description.as_deref(), Some("3 unresolved"));
            assert!(!node.state.hidden, "a banner with conflicts is visible");

            // Zero conflicts → the banner paints nothing, so it is hidden from ATs.
            let empty = banner_at(Rect::new(0.0, 0.0, 400.0, 40.0), 0);
            let node = Widget::accessibility(&empty).unwrap();
            assert!(node.state.hidden, "an empty banner must be hidden");
        }

        fn banner_paints_nothing_when_count_zero() {
            // The contract the chrome relies on to leave the banner in the
            // layout tree without affecting layout.
            let b = banner_at(Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT), 0);
            assert_eq!(b.conflict_count(), 0);
            assert_eq!(b.get_rect(), Rect::ZERO);
            assert!(b.review_chip_rect().is_none());
            assert!(b.dismiss_chip_rect().is_none());
        }

        fn banner_paints_warning_strip_when_count_positive() {
            // Both chip rects must be inside the banner rect (hit-testable).
            let b = banner_at(Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT), 3);
            assert_eq!(b.conflict_count(), 3);
            let r = b.get_rect();
            assert!(r.width() > 0.0);
            let review = b.review_chip_rect().expect("review chip must exist");
            let dismiss = b.dismiss_chip_rect().expect("dismiss chip must exist");
            assert!(r.contains(review.center()));
            assert!(r.contains(dismiss.center()));
            // Dismiss is the rightmost chip.
            assert!(dismiss.x0 > review.x0);
            // Count label uses the plural form for n != 1.
            assert!(b.count_label().contains("3 conflicts"));
        }

        fn count_updates_via_setter() {
            // Chip-rect existence flips at zero/non-zero.
            let mut b = ConflictBanner::new("Kanban move");
            b.rect = Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT);
            assert_eq!(b.conflict_count(), 0);
            assert!(b.review_chip_rect().is_none());
            b.set_conflict_count(7);
            assert_eq!(b.conflict_count(), 7);
            assert!(b.review_chip_rect().is_some());
            b.set_conflict_count(0);
            assert_eq!(b.conflict_count(), 0);
            assert!(b.review_chip_rect().is_none());
        }

        accessibility_reports_group_role_and_hidden_when_empty();
        banner_paints_nothing_when_count_zero();
        banner_paints_warning_strip_when_count_positive();
        count_updates_via_setter();
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(b: &mut ConflictBanner, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        b.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    #[test]
    fn conflict_banner_chips_fire_only_their_callbacks() {
        fn review_callback_fires_on_review_button_click() {
            // Move cursor over the review chip, click — callback fires
            // exactly once. Pins the hover→hit-test→callback wire.
            let fired: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
            let fired_clone = fired.clone();
            let mut b = ConflictBanner::new("Kanban move")
                .on_review(move || *fired_clone.borrow_mut() += 1);
            b.rect = Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT);
            b.set_conflict_count(2);

            let chip = b.review_chip_rect().unwrap();
            dispatch(&mut b, &cursor_moved(chip.center().x, chip.center().y));
            dispatch(&mut b, &mouse_press());
            assert_eq!(*fired.borrow(), 1);
        }

        fn dismiss_callback_fires_on_dismiss_button_click() {
            // Symmetrical wire for the Dismiss chip.
            let fired: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
            let fired_clone = fired.clone();
            let mut b = ConflictBanner::new("Rename collision")
                .on_dismiss(move || *fired_clone.borrow_mut() += 1);
            b.rect = Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT);
            b.set_conflict_count(1);

            let chip = b.dismiss_chip_rect().unwrap();
            dispatch(&mut b, &cursor_moved(chip.center().x, chip.center().y));
            dispatch(&mut b, &mouse_press());
            assert_eq!(*fired.borrow(), 1);
            // Singular form for n == 1.
            assert!(b.count_label().contains("1 conflict in this doc"));
        }

        fn cursor_outside_chips_does_not_fire_callbacks() {
            // Clicking inside the banner but outside both chips must
            // not fire either callback. Pins the hit-test boundary.
            let fired_review: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
            let fired_dismiss: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
            let fr = fired_review.clone();
            let fd = fired_dismiss.clone();
            let mut b = ConflictBanner::new("Kanban move")
                .on_review(move || *fr.borrow_mut() += 1)
                .on_dismiss(move || *fd.borrow_mut() += 1);
            b.rect = Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT);
            b.set_conflict_count(3);

            // Click on the leading-glyph area, well left of both chips.
            dispatch(&mut b, &cursor_moved(20.0, BANNER_HEIGHT / 2.0));
            dispatch(&mut b, &mouse_press());
            assert_eq!(*fired_review.borrow(), 0);
            assert_eq!(*fired_dismiss.borrow(), 0);
        }

        review_callback_fires_on_review_button_click();
        dismiss_callback_fires_on_dismiss_button_click();
        cursor_outside_chips_does_not_fire_callbacks();
    }
}
