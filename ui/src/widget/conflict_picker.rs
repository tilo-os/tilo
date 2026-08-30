//! `ConflictPicker` — modal widget that shows the alternatives
//! produced by a [`crate::widget::merge_strategy::ConflictResolver`]
//! side-by-side and lets the user pick one.
//!
//! ```text
//!  +--- Kanban move conflict ----------------------------------+
//!  |  ALICE's choice              BOB's choice                 |
//!  |  Move to Done                Move to In Review            |
//!  |  [Pick Alice's]              [Pick Bob's]                 |
//!  |              [Pick neither (keep current)]                |
//!  +----------------------------------------------------------+
//! ```
//!
//! A [`crate::widget::merge_strategy::ConflictKind::MergeOrPick`] with
//! `merge_preview: Some(_)` adds a third `[Use merged]` card. Cards lay out
//! side-by-side above [`NARROW_THRESHOLD`]/[`MIN_CARD_W`], else stack
//! vertically.
//!
//! Outcomes flow through `on_pick`: [`PickerOutcome::Picked`],
//! [`PickerOutcome::PickedMerged`], [`PickerOutcome::KeepCurrent`],
//! [`PickerOutcome::Cancel`] (Esc / scrim click).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::merge_strategy::{Alternative, ConflictKind};

const PADDING: f64 = 16.0;
/// Vertical spacing between title row, cards, and the `[Pick neither]` row.
const ROW_GAP: f64 = 12.0;
const BUTTON_H: f64 = 32.0;
const TITLE_H: f64 = 28.0;
/// Width below which the picker wraps the alternatives into a
/// vertical stack instead of side-by-side cards. ~800 px viewport
/// minus 2*PADDING = 768 inner; below that we wrap.
const NARROW_THRESHOLD: f64 = 760.0;
/// Minimum width any single alternative card needs before it
/// becomes hard to read. Drives the wrap decision. Tuned so that:
/// - 2 cards at 800 px wide stay side-by-side (~378 px each)
/// - 3 cards at 800 px wide wrap to a vertical stack (~248 px
///   each would fall below this threshold)
/// - 2 or 3 cards at 1280 px wide stay side-by-side
const MIN_CARD_W: f64 = 260.0;
const CARD_H: f64 = 160.0;
/// Scrim alpha — matches the command-palette dim treatment.
const SCRIM_ALPHA: u8 = 153; // ~ 60%

/// What the user chose in the picker. Forwarded through the
/// `on_pick` callback so the host knows what to apply.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerOutcome {
    /// User clicked one of the `[Pick X's]` buttons; the index is into the
    /// alternatives list the picker was constructed with.
    Picked { alternative_index: usize },
    /// User clicked `[Use merged]`. Only emitted when the kind was
    /// `MergeOrPick` with `merge_preview: Some(_)`.
    PickedMerged,
    /// User clicked `[Pick neither (keep current)]`. Host should
    /// leave the current scalar / move / rename as-is.
    KeepCurrent,
    /// User pressed Esc or clicked the scrim. Host typically
    /// re-mounts the banner so the user can try again later.
    Cancel,
}

pub type PickCallback = Box<dyn FnMut(PickerOutcome)>;

/// Side-by-side conflict picker. See module-level docs for the
/// contract.
pub struct ConflictPicker {
    family: String,
    kind: ConflictKind,
    rect: Rect,
    last_cursor: Point,
    /// Card index the cursor is hovering, or `None` outside any card.
    hover_alt: Option<usize>,
    /// Whether the merged preview card is hovered — only meaningful when
    /// the kind is `MergeOrPick` with a preview.
    hover_merged: bool,
    /// Whether the `[Pick neither]` button is hovered.
    hover_keep: bool,
    on_pick: Option<PickCallback>,
    dirty: bool,
}

impl ConflictPicker {
    /// Build a picker for `family` (the conflict family display
    /// name) + `kind` (the classification the resolver produced).
    pub fn new(family: impl Into<String>, kind: ConflictKind) -> Self {
        Self {
            family: family.into(),
            kind,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            hover_alt: None,
            hover_merged: false,
            hover_keep: false,
            on_pick: None,
            dirty: true,
        }
    }

    /// Builder: install the pick callback. Fires for every outcome
    /// listed in [`PickerOutcome`].
    pub fn on_pick<F: FnMut(PickerOutcome) + 'static>(mut self, f: F) -> Self {
        self.on_pick = Some(Box::new(f));
        self
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn kind(&self) -> &ConflictKind {
        &self.kind
    }

    pub fn hover_alt(&self) -> Option<usize> {
        self.hover_alt
    }

    /// Empty for `ConflictKind::Auto`, which the picker should never be
    /// mounted for — defending the invariant is cheap.
    fn alternatives(&self) -> &[Alternative] {
        match &self.kind {
            ConflictKind::PickOne { alternatives } => alternatives,
            ConflictKind::MergeOrPick { alternatives, .. } => alternatives,
            ConflictKind::Auto => &[],
        }
    }

    pub fn has_merged_card(&self) -> bool {
        matches!(
            &self.kind,
            ConflictKind::MergeOrPick {
                merge_preview: Some(_),
                ..
            }
        )
    }

    fn merge_preview(&self) -> Option<&str> {
        match &self.kind {
            ConflictKind::MergeOrPick {
                merge_preview: Some(s),
                ..
            } => Some(s.as_str()),
            _ => None,
        }
    }

    /// Rect of each alternative card and the optional merged card. Reserves
    /// a title row at top and a `[Pick neither]` row at bottom; the middle
    /// band lays cards side-by-side unless the per-card width would fall
    /// below `MIN_CARD_W`, in which case it stacks vertically.
    pub fn card_rects(&self) -> Vec<Rect> {
        let mut rects = Vec::new();
        let alt_count = self.alternatives().len();
        let total_cards = alt_count + usize::from(self.has_merged_card());
        if total_cards == 0 {
            return rects;
        }

        let inner = self.inner_rect();
        let middle_top = inner.y0 + TITLE_H + ROW_GAP;
        let middle_bot = inner.y1 - BUTTON_H - ROW_GAP;
        let middle_h = (middle_bot - middle_top).max(0.0);

        // Cap to CARD_H; if the middle band is shorter, use what's there so
        // the picker still renders cleanly inside small modals.
        let card_h = CARD_H.min(middle_h.max(40.0));

        let gap = ROW_GAP;
        let available_w = inner.width();
        let side_by_side_card_w =
            (available_w - gap * (total_cards as f64 - 1.0).max(0.0)) / (total_cards as f64);

        if available_w >= NARROW_THRESHOLD && side_by_side_card_w >= MIN_CARD_W {
            let mut x = inner.x0;
            for _ in 0..total_cards {
                rects.push(Rect::new(
                    x,
                    middle_top,
                    x + side_by_side_card_w,
                    middle_top + card_h,
                ));
                x += side_by_side_card_w + gap;
            }
        } else {
            // Stacked layout: cap per-card height so the stack still fits the middle band.
            let stacked_card_h = ((middle_h - gap * (total_cards as f64 - 1.0).max(0.0))
                / (total_cards as f64))
                .min(card_h);
            let mut y = middle_top;
            for _ in 0..total_cards {
                rects.push(Rect::new(inner.x0, y, inner.x1, y + stacked_card_h));
                y += stacked_card_h + gap;
            }
        }
        rects
    }

    /// Rect of the `[Pick neither (keep current)]` button. Anchored
    /// to the bottom of the inner rect, centred horizontally.
    pub fn keep_current_rect(&self) -> Rect {
        let inner = self.inner_rect();
        let w = 240.0_f64.min(inner.width() - 2.0 * PADDING);
        let cx = f64::midpoint(inner.x0, inner.x1);
        let y1 = inner.y1;
        Rect::new(cx - w / 2.0, y1 - BUTTON_H, cx + w / 2.0, y1)
    }

    fn inner_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y1 - PADDING,
        )
    }

    fn title_rect(&self) -> Rect {
        let inner = self.inner_rect();
        Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + TITLE_H)
    }

    /// Per-alternative pick-button rect at card index `i`. The merged
    /// card, when present, occupies the trailing index and comes back
    /// through here too.
    pub fn pick_button_rect(&self, i: usize) -> Option<Rect> {
        let cards = self.card_rects();
        let card = cards.get(i)?;
        let inset = 8.0;
        Some(Rect::new(
            card.x0 + inset,
            card.y1 - BUTTON_H + (BUTTON_H - 24.0) / 2.0,
            card.x1 - inset,
            card.y1 - (BUTTON_H - 24.0) / 2.0,
        ))
    }

    /// Index into `card_rects()` of the card containing `p`, or `None`.
    pub fn card_at(&self, p: Point) -> Option<usize> {
        for (i, r) in self.card_rects().iter().enumerate() {
            if r.contains(p) {
                return Some(i);
            }
        }
        None
    }

    /// Paint one alternative card: title + author + timestamp + pick button.
    #[allow(clippy::too_many_arguments)]
    fn draw_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        label: &str,
        author: &str,
        timestamp_label: &str,
        summary: &str,
        button_label: &str,
        hovered_card: bool,
        pick_button: Rect,
    ) {
        // Hovered cards lift via accent border.
        let body = RoundedRect::from_rect(card, f64::from(theme.tokens.radii.md));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &body,
        );
        let border = if hovered_card {
            theme.tokens.colors.border_focus
        } else {
            theme.tokens.colors.border_standard
        };
        let border_stroke_w = if hovered_card { 2.0 } else { 1.0 };
        scene.stroke(
            &Stroke::new(border_stroke_w),
            transform,
            border,
            None,
            &body,
        );

        let inner = Rect::new(
            card.x0 + 12.0,
            card.y0 + 10.0,
            card.x1 - 12.0,
            card.y1 - 10.0,
        );
        let text_size_sm = theme.tokens.typography.scale.sm;
        let text_size_md = theme.tokens.typography.scale.base;

        text_renderer.draw_text(
            scene,
            author,
            text_size_md,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((inner.x0, inner.y0)),
            None,
        );
        let ts_y = inner.y0 + f64::from(text_size_md) + 4.0;
        text_renderer.draw_text(
            scene,
            timestamp_label,
            text_size_sm,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((inner.x0, ts_y)),
            None,
        );

        let label_y = ts_y + f64::from(text_size_sm) + 8.0;
        text_renderer.draw_text(
            scene,
            label,
            text_size_md,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((inner.x0, label_y)),
            None,
        );

        // Single-line, not wrapped — matches the picker's compact shape.
        let summary_y = label_y + f64::from(text_size_md) + 6.0;
        text_renderer.draw_text(
            scene,
            summary,
            text_size_sm,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((inner.x0, summary_y)),
            None,
        );

        let btn_shape = RoundedRect::from_rect(pick_button, f64::from(theme.tokens.radii.sm));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.accent[5],
            None,
            &btn_shape,
        );
        let btn_text_size = text_size_sm;
        let (bw, bh) = text_renderer.measure(button_label, btn_text_size, None);
        text_renderer.draw_text(
            scene,
            button_label,
            btn_text_size,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    pick_button.x0 + (pick_button.width() - f64::from(bw)) / 2.0,
                    pick_button.y0 + (pick_button.height() - f64::from(bh)) / 2.0,
                )),
            None,
        );
    }
}

impl Widget for ConflictPicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Sized by the host: expands to fill whatever rect the parent
        // assigns — typical use is a centered modal, e.g. 720×360.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
        if matches!(self.kind, ConflictKind::Auto) || self.alternatives().is_empty() {
            // Nothing to pick — degrade gracefully (paint nothing).
            self.dirty = false;
            return;
        }

        // The widget tree doesn't know the screen dims here, so the scrim
        // fills a very large rect around the modal instead; downstream
        // chrome clips it to the viewport in a full-screen container.
        let scrim_color = Color::from_rgba8(0, 0, 0, SCRIM_ALPHA);
        let scrim_rect = self.rect.inflate(2000.0, 2000.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            scrim_color,
            None,
            &scrim_rect,
        );

        let body = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.glass,
            None,
            &body,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.accent[5],
            None,
            &body,
        );

        let title = self.title_rect();
        let title_text = format!("{} conflict", self.family);
        let title_size = theme.tokens.typography.scale.lg;
        let (_tw, th) = text_renderer.measure(&title_text, title_size, None);
        text_renderer.draw_text(
            scene,
            &title_text,
            title_size,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((title.x0, title.y0 + (title.height() - f64::from(th)) / 2.0)),
            None,
        );

        let cards = self.card_rects();
        let alt_count = self.alternatives().len();
        let now = std::time::SystemTime::now();
        for (i, card) in cards.iter().enumerate() {
            let hovered = self.hover_alt == Some(i)
                || (i == alt_count && self.has_merged_card() && self.hover_merged);
            let Some(pick_btn) = self.pick_button_rect(i) else {
                continue;
            };
            if i < alt_count {
                let alt = &self.alternatives()[i];
                let ts_label = crate::widget::history::format_relative_time(alt.timestamp, now);
                let button_label = format!("Pick {}'s", alt.author);
                self.draw_card(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *card,
                    &alt.label,
                    &alt.author,
                    &ts_label,
                    &alt.summary,
                    &button_label,
                    hovered,
                    pick_btn,
                );
            } else {
                // `draw_card` paints the standard accent pick button; a
                // success-tinted overlay layers on top so merge reads distinct.
                let preview = self.merge_preview().unwrap_or("");
                self.draw_card(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *card,
                    "Merged preview",
                    "auto",
                    "concatenated",
                    preview,
                    "Use merged",
                    hovered,
                    pick_btn,
                );
                let btn_shape = RoundedRect::from_rect(pick_btn, f64::from(theme.tokens.radii.sm));
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.semantic.success,
                    None,
                    &btn_shape,
                );
                let bl = "Use merged";
                let bsize = theme.tokens.typography.scale.sm;
                let (bw, bh) = text_renderer.measure(bl, bsize, None);
                text_renderer.draw_text(
                    scene,
                    bl,
                    bsize,
                    theme.tokens.colors.text_primary,
                    transform
                        * Affine::translate((
                            pick_btn.x0 + (pick_btn.width() - f64::from(bw)) / 2.0,
                            pick_btn.y0 + (pick_btn.height() - f64::from(bh)) / 2.0,
                        )),
                    None,
                );
            }
        }

        let keep = self.keep_current_rect();
        let keep_shape = RoundedRect::from_rect(keep, f64::from(theme.tokens.radii.sm));
        let keep_body = if self.hover_keep {
            theme.tokens.colors.surface_white_20
        } else {
            theme.tokens.colors.surface_white_10
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            keep_body,
            None,
            &keep_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_white_20,
            None,
            &keep_shape,
        );
        let keep_label = "Pick neither (keep current)";
        let keep_size = theme.tokens.typography.scale.sm;
        let (kw, kh) = text_renderer.measure(keep_label, keep_size, None);
        text_renderer.draw_text(
            scene,
            keep_label,
            keep_size,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    keep.x0 + (keep.width() - f64::from(kw)) / 2.0,
                    keep.y0 + (keep.height() - f64::from(kh)) / 2.0,
                )),
            None,
        );

        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if matches!(self.kind, ConflictKind::Auto) {
            return;
        }
        let alt_count = self.alternatives().len();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let card_idx = self.card_at(p);
                let new_alt = card_idx.filter(|&i| i < alt_count);
                let new_merged = card_idx.is_some_and(|i| i == alt_count && self.has_merged_card());
                let new_keep = self.keep_current_rect().contains(p);
                if new_alt != self.hover_alt
                    || new_merged != self.hover_merged
                    || new_keep != self.hover_keep
                {
                    self.hover_alt = new_alt;
                    self.hover_merged = new_merged;
                    self.hover_keep = new_keep;
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Buttons sit *inside* the card rect, so a click on a button
                // is also a click on the card — test buttons first.
                let total = alt_count + usize::from(self.has_merged_card());
                for i in 0..total {
                    if let Some(btn) = self.pick_button_rect(i)
                        && btn.contains(self.last_cursor)
                    {
                        if i < alt_count {
                            self.fire(PickerOutcome::Picked {
                                alternative_index: i,
                            });
                        } else {
                            self.fire(PickerOutcome::PickedMerged);
                        }
                        *handled = true;
                        return;
                    }
                }
                if self.keep_current_rect().contains(self.last_cursor) {
                    self.fire(PickerOutcome::KeepCurrent);
                    *handled = true;
                    return;
                }
                // Click on the scrim (outside the modal rect) cancels.
                if !self.rect.contains(self.last_cursor) {
                    self.fire(PickerOutcome::Cancel);
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                let KeyEvent {
                    logical_key,
                    state: element_state,
                    ..
                } = key_event;
                if *element_state == ElementState::Pressed
                    && let Key::Named(NamedKey::Escape) = logical_key
                {
                    self.fire(PickerOutcome::Cancel);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ConflictPicker"
    }

    /// `Role::Group` named for the family; the description names the
    /// conflict kind so an AT can announce what choice is being asked for.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name(format!("Resolve conflict: {}", self.family))
                .with_description(format!("{:?}", self.kind)),
        )
    }

    fn test_id(&self) -> Option<&str> {
        Some("conflict_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

impl ConflictPicker {
    /// Fire the pick callback. Centralised so the event handler
    /// stays compact.
    fn fire(&mut self, outcome: PickerOutcome) {
        if let Some(cb) = self.on_pick.as_mut() {
            cb(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};
    use winit::event::Modifiers;
    use winit::keyboard::PhysicalKey;

    fn alt(label: &str, author: &str) -> Alternative {
        Alternative {
            label: label.into(),
            summary: format!("{} chose {}", author, label),
            author: author.into(),
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            payload: Arc::from(Vec::<u8>::new()),
        }
    }

    fn picker_at(rect: Rect, kind: ConflictKind) -> ConflictPicker {
        let mut p = ConflictPicker::new("Kanban move", kind);
        p.rect = rect;
        p
    }

    #[test]
    fn accessibility_reports_group_role_name_and_kind() {
        use crate::a11y::Role;
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Done", "Alice"), alt("In progress", "Bob")],
        };
        let p = picker_at(Rect::new(0.0, 0.0, 500.0, 300.0), kind);
        let node = Widget::accessibility(&p).expect("picker is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Resolve conflict: Kanban move"));
        // The description names the conflict kind (Debug form).
        assert!(
            node.description.as_deref().unwrap().starts_with("PickOne"),
            "description should name the conflict kind, got {:?}",
            node.description
        );
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

    /// Synthesise a `WindowEvent::KeyboardInput` carrying `key`
    /// in the `Pressed` state. Mirrors the zero-init workaround
    /// used in `command_palette::tests` — winit 0.30 keeps
    /// `KeyEventExtra` `pub(crate)` so we cannot construct a
    /// `KeyEvent` through a struct literal.
    fn key_press(key: NamedKey) -> WindowEvent {
        // SAFETY: all pub fields of `KeyEvent` tolerate the
        // all-zero bit pattern; `handle_event` only inspects
        // `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(key);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(p: &mut ConflictPicker, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        p.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
    }

    #[test]
    fn picker_renders_two_alternatives_side_by_side() {
        // 1280-wide typical layout — two cards lay out side-by-side
        // and both rects fit inside the picker.
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
        };
        let p = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind);
        let cards = p.card_rects();
        assert_eq!(cards.len(), 2);
        // Side-by-side: card[1] starts to the right of card[0].
        assert!(cards[1].x0 > cards[0].x0);
        assert!(p.rect.contains(cards[0].center()));
        assert!(p.rect.contains(cards[1].center()));
    }

    #[test]
    fn picker_pick_button_fires_with_correct_index() {
        // Click the second alternative's button — outcome must be Picked { alternative_index: 1 }.
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
        };
        let mut p = ConflictPicker::new("Kanban move", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

        let btn = p.pick_button_rect(1).expect("button 1 must exist");
        dispatch(&mut p, &cursor_moved(btn.center().x, btn.center().y));
        dispatch(&mut p, &mouse_press());
        assert_eq!(outcomes.borrow().len(), 1);
        assert_eq!(
            outcomes.borrow()[0],
            PickerOutcome::Picked {
                alternative_index: 1
            }
        );
    }

    #[test]
    fn picker_merged_button_only_renders_when_preview_present() {
        // MergeOrPick with `merge_preview: None` produces no merged
        // card; with `Some(_)` it adds a trailing card.
        let kind_none = ConflictKind::MergeOrPick {
            alternatives: vec![alt("Rename to A", "Alice")],
            merge_preview: None,
        };
        let p_none = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind_none);
        assert!(!p_none.has_merged_card());
        assert_eq!(p_none.card_rects().len(), 1);

        let kind_some = ConflictKind::MergeOrPick {
            alternatives: vec![alt("Rename to A", "Alice"), alt("Rename to B", "Bob")],
            merge_preview: Some("Combined: A / B".into()),
        };
        let p_some = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind_some);
        assert!(p_some.has_merged_card());
        assert_eq!(p_some.card_rects().len(), 3);
    }

    #[test]
    fn picker_keep_current_fires_keep_current() {
        // Click the bottom-centre "Pick neither" button.
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
        };
        let mut p = ConflictPicker::new("Kanban move", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

        let keep = p.keep_current_rect();
        dispatch(&mut p, &cursor_moved(keep.center().x, keep.center().y));
        dispatch(&mut p, &mouse_press());
        assert_eq!(outcomes.borrow().len(), 1);
        assert_eq!(outcomes.borrow()[0], PickerOutcome::KeepCurrent);
    }

    #[test]
    fn picker_esc_fires_cancel() {
        // Esc → Cancel, driving the chrome's "changed their mind" path.
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice")],
        };
        let mut p = ConflictPicker::new("Kanban move", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

        dispatch(&mut p, &key_press(NamedKey::Escape));
        assert_eq!(outcomes.borrow().len(), 1);
        assert_eq!(outcomes.borrow()[0], PickerOutcome::Cancel);
    }

    #[test]
    fn picker_hover_alt_tracks_cursor_in_alternative_card() {
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
        };
        let mut p = picker_at(Rect::new(0.0, 0.0, 1280.0, 360.0), kind);
        let cards = p.card_rects();
        dispatch(
            &mut p,
            &cursor_moved(cards[0].center().x, cards[0].center().y),
        );
        assert_eq!(p.hover_alt(), Some(0));
        dispatch(
            &mut p,
            &cursor_moved(cards[1].center().x, cards[1].center().y),
        );
        assert_eq!(p.hover_alt(), Some(1));
        // Cache the bounds first so the borrow checker doesn't see a live
        // `&p` while we hand `&mut p` to `dispatch`.
        let outside_x = p.rect.x1 + 200.0;
        let outside_y = p.rect.y1 + 200.0;
        dispatch(&mut p, &cursor_moved(outside_x, outside_y));
        assert_eq!(p.hover_alt(), None);
    }

    #[test]
    fn picker_merged_pick_button_fires_picked_merged() {
        // Click on the merged card's `[Use merged]` button —
        // outcome must be PickedMerged (not Picked{2}).
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::MergeOrPick {
            alternatives: vec![alt("Rename to A", "Alice"), alt("Rename to B", "Bob")],
            merge_preview: Some("Combined: A / B".into()),
        };
        let mut p = ConflictPicker::new("Rename collision", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        p.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

        // The merged card is at trailing index 2 (alt_count = 2).
        let btn = p.pick_button_rect(2).expect("merged button must exist");
        dispatch(&mut p, &cursor_moved(btn.center().x, btn.center().y));
        dispatch(&mut p, &mouse_press());
        assert_eq!(outcomes.borrow().len(), 1);
        assert_eq!(outcomes.borrow()[0], PickerOutcome::PickedMerged);
    }

    #[test]
    fn picker_fits_in_narrow_viewport_via_stack() {
        // At 800 px wide with three total cards (2 alternatives + merged
        // preview), the picker must wrap to a vertical stack rather than
        // producing sub-MIN_CARD_W cards that would be unreadable.
        let kind = ConflictKind::MergeOrPick {
            alternatives: vec![alt("A", "Alice"), alt("B", "Bob")],
            merge_preview: Some("merged".into()),
        };
        let p = picker_at(Rect::new(0.0, 0.0, 800.0, 600.0), kind);
        let cards = p.card_rects();
        assert_eq!(cards.len(), 3);
        for c in &cards {
            assert!(c.width() > MIN_CARD_W, "stacked cards must be wide");
        }
        assert!(cards[1].y0 > cards[0].y0);
        assert!(cards[2].y0 > cards[1].y0);
    }

    #[test]
    fn picker_scrim_click_fires_cancel() {
        // Click outside the modal rect — the scrim absorbs the click as Cancel.
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("A", "Alice")],
        };
        let mut p = ConflictPicker::new("Kanban move", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        p.rect = Rect::new(200.0, 200.0, 1080.0, 560.0);

        dispatch(&mut p, &cursor_moved(50.0, 50.0));
        dispatch(&mut p, &mouse_press());
        assert_eq!(outcomes.borrow().len(), 1);
        assert_eq!(outcomes.borrow()[0], PickerOutcome::Cancel);
    }

    #[test]
    fn banner_to_picker_round_trip_outcome() {
        // Combined host-flow test: chrome wires a banner's `[Review]`
        // callback to open a picker. Drive the banner click, observe
        // Review fires, then dispatch a Pick click and observe Picked.
        use crate::widget::conflict_banner::{BANNER_HEIGHT, ConflictBanner};

        // 1. Banner click drives the host to open a picker.
        let review_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        let rf = review_fired.clone();
        let mut banner =
            ConflictBanner::new("Kanban move").on_review(move || *rf.borrow_mut() = true);
        banner.set_rect_for_test(Rect::new(0.0, 0.0, 600.0, BANNER_HEIGHT));
        banner.set_conflict_count(2);

        let banner_chip = banner.review_chip_rect().unwrap();

        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        banner.handle_event(
            &cursor_moved(banner_chip.center().x, banner_chip.center().y),
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
        banner.handle_event(
            &mouse_press(),
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
        assert!(*review_fired.borrow(), "banner [Review] must fire");

        // 2. Host opens the picker and the user picks alternative 0.
        let outcomes: Rc<RefCell<Vec<PickerOutcome>>> = Rc::new(RefCell::new(Vec::new()));
        let outcomes_clone = outcomes.clone();
        let kind = ConflictKind::PickOne {
            alternatives: vec![alt("Move to Done", "Alice"), alt("Move to Review", "Bob")],
        };
        let mut picker = ConflictPicker::new("Kanban move", kind)
            .on_pick(move |o| outcomes_clone.borrow_mut().push(o));
        picker.rect = Rect::new(0.0, 0.0, 1280.0, 360.0);

        let btn = picker.pick_button_rect(0).unwrap();
        dispatch(&mut picker, &cursor_moved(btn.center().x, btn.center().y));
        dispatch(&mut picker, &mouse_press());
        assert_eq!(
            outcomes.borrow()[0],
            PickerOutcome::Picked {
                alternative_index: 0
            }
        );
    }
}
