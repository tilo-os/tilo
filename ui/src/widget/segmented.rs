//! `TriStateControl` + `ColorSwatchPicker` — two small value-out widgets.
//!
//! * [`TriStateControl`]: a three-segment selector (e.g. allow / inherit /
//!   deny). Clicking a segment selects that exact value; clicking the
//!   already-selected segment clears back to the middle ("inherit"). The
//!   chosen value is exposed by index via [`TriStateControl::take_choice`] —
//!   the same `&mut self` + CRDT-friendly channel the [`super::ContextMenu`]
//!   uses, rather than a callback the host can't splice into Automerge.
//!
//! * [`ColorSwatchPicker`]: a fixed-palette grid of colour swatches. Clicking
//!   a swatch records its index; the host reads it via
//!   [`ColorSwatchPicker::take_choice`] and writes the colour onto its model.
//!
//! Both are hand-laid-out (rect hit-testing) leaf widgets — they hold only
//! render/selection caches, never the canonical value (which lives in the
//! host's CRDT), matching the toolkit's CRDT-native pattern.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::{ON_FILL_MIN_CONTRAST, over};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Drives the active-segment colour. `Neutral` is the de-emphasised
/// middle ("inherit").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentTone {
    Positive,
    Neutral,
    Negative,
}

const SEGMENT_HEIGHT: f32 = 28.0;

/// Narrows a layout/measurement value to `f32` for vello/taffy. Every call
/// site in this file is a pixel size or offset for a small on-screen
/// control, always many orders of magnitude below `f32::MAX`, so the
/// truncation the lint warns about can't actually occur here.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in these widgets never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

pub struct TriStateControl {
    labels: [String; 3],
    tones: [SegmentTone; 3],
    selected: usize,
    /// Drained selection: the index the user just clicked.
    choice: Option<usize>,
    rect: Rect,
    seg_rects: [Rect; 3],
    last_cursor: Point,
}

impl TriStateControl {
    pub fn allow_inherit_deny() -> Self {
        Self::new(
            ["Allow", "Inherit", "Deny"],
            [
                SegmentTone::Positive,
                SegmentTone::Neutral,
                SegmentTone::Negative,
            ],
        )
    }

    pub fn new(labels: [&str; 3], tones: [SegmentTone; 3]) -> Self {
        Self {
            labels: [
                labels[0].to_string(),
                labels[1].to_string(),
                labels[2].to_string(),
            ],
            tones,
            selected: 1,
            choice: None,
            rect: Rect::ZERO,
            seg_rects: [Rect::ZERO; 3],
            last_cursor: Point::ZERO,
        }
    }

    /// Out-of-range values are clamped.
    #[must_use]
    pub fn with_selected(mut self, idx: usize) -> Self {
        self.selected = idx.min(2);
        self
    }

    pub fn set_selected(&mut self, idx: usize) {
        self.selected = idx.min(2);
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Re-clicking the active segment yields the middle index (1,
    /// "inherit") — toggle-to-neutral.
    pub fn take_choice(&mut self) -> Option<usize> {
        self.choice.take()
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "i ranges over exactly the 3 fixed segments (0..=2)"
    )]
    fn recompute_segments(&mut self) {
        let w = self.rect.width() / 3.0;
        for (i, seg) in self.seg_rects.iter_mut().enumerate() {
            let x0 = self.rect.x0 + i as f64 * w;
            *seg = Rect::new(x0, self.rect.y0, x0 + w, self.rect.y1);
        }
    }

    fn tone_color(theme: &Theme, tone: SegmentTone) -> Color {
        match tone {
            SegmentTone::Positive => theme.tokens.colors.semantic.success,
            SegmentTone::Neutral => theme.text_dim,
            SegmentTone::Negative => theme.tokens.colors.semantic.danger,
        }
    }
}

impl Widget for TriStateControl {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(SEGMENT_HEIGHT),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // 3 × 56, so it works in an auto slot.
        taffy::Size {
            width: 168.0,
            height: SEGMENT_HEIGHT,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.recompute_segments();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.sm);
        let track = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &track,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &track,
        );

        let Some(&active) = self.seg_rects.get(self.selected) else {
            return;
        };
        let Some(&tone) = self.tones.get(self.selected) else {
            return;
        };
        let fill = Self::tone_color(theme, tone);
        let alpha = if tone == SegmentTone::Neutral {
            0.18
        } else {
            0.9
        };
        scene.fill(
            Fill::NonZero,
            transform,
            fill.with_alpha_factor(alpha),
            None,
            &RoundedRect::from_rect(active, radius),
        );
        // What the selected segment's label is actually drawn on: the tone at
        // its alpha over the track, not the tone token. Measuring against the
        // token would report a contrast the screen never holds.
        let active_ground = over(theme.tokens.colors.surfaces.raised, fill, alpha);

        let text_size = theme.tokens.typography.scale.sm;
        for (i, (label, seg_rect)) in self.labels.iter().zip(self.seg_rects.iter()).enumerate() {
            let on = i == self.selected;
            let col = if on && tone != SegmentTone::Neutral {
                theme
                    .tokens
                    .colors
                    .ink_on_any(&[active_ground], Color::WHITE, ON_FILL_MIN_CONTRAST)
            } else if on {
                theme.tokens.colors.ink_on_any(
                    &[active_ground],
                    theme.tokens.colors.text_primary,
                    ON_FILL_MIN_CONTRAST,
                )
            } else {
                theme.text_dim
            };
            let (tw, _) = text_renderer.measure(label, text_size, None);
            let cx = seg_rect.center().x - f64::from(tw) / 2.0;
            let cy = self.rect.y0 + (self.rect.height() - f64::from(text_size)) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                text_size,
                col,
                transform.pre_translate(Vec2::new(cx, cy)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self
                    .seg_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor))
                {
                    let target = if i == self.selected { 1 } else { i };
                    self.selected = target;
                    self.choice = Some(target);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "TriStateControl"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Segments are drawn, not child widgets, so each is a synthetic
        // `RadioButton` child. Labels are host-provided runtime strings,
        // not FTL keys → no `i18n!`.
        let count = self.labels.len();
        let value = self.labels.get(self.selected).cloned();
        let segments: Vec<AccessNode> = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                AccessNode::new(Role::RadioButton)
                    .with_action_target(format!("segment:{i}"))
                    .with_name(label.clone())
                    .with_state(AccessState {
                        checked: Some(i == self.selected),
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count)
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    disabled: !self.is_enabled(),
                    ..Default::default()
                })
                .with_children(segments),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [segment] = path else {
            return None;
        };
        self.seg_rects.get(*segment).copied()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        let Some(index) = target
            .strip_prefix("segment:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.labels.len())
        else {
            return false;
        };
        if !matches!(
            action,
            crate::a11y::Action::Activate | crate::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let next = if index == self.selected { 1 } else { index };
        self.selected = next;
        self.choice = Some(next);
        true
    }
}

pub struct ColorSwatchPicker {
    /// Packed `0xRRGGBB` palette.
    palette: Vec<u32>,
    columns: usize,
    cell: f64,
    selected: Option<usize>,
    choice: Option<usize>,
    rect: Rect,
    swatch_rects: Vec<Rect>,
    last_cursor: Point,
    hovered: Option<usize>,
}

/// Matches the permissions editor.
pub const DEFAULT_PALETTE: [u32; 12] = [
    0xE6_4A_4A, 0xE6_8A_3A, 0xE6_C8_4A, 0x6F_C7_4A, 0x4A_C7_7A, 0x4A_C7_B8, 0x4A_8F_E6, 0x6A_5A_E6,
    0x9A_4A_E6, 0xE6_4A_B8, 0x9A_9A_9A, 0x5A_64_72,
];

pub fn packed_color(rgb: u32) -> Color {
    Color::from_rgb8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

impl ColorSwatchPicker {
    pub fn new() -> Self {
        Self::with_palette(DEFAULT_PALETTE.to_vec(), 4)
    }

    pub fn with_palette(palette: Vec<u32>, columns: usize) -> Self {
        Self {
            palette,
            columns: columns.max(1),
            cell: 30.0,
            selected: None,
            choice: None,
            rect: Rect::ZERO,
            swatch_rects: Vec::new(),
            last_cursor: Point::ZERO,
            hovered: None,
        }
    }

    #[must_use]
    pub fn with_selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn take_choice(&mut self) -> Option<usize> {
        self.choice.take()
    }

    pub fn color_at(&self, idx: usize) -> Option<u32> {
        self.palette.get(idx).copied()
    }

    fn rows(&self) -> usize {
        self.palette.len().div_ceil(self.columns)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "row/column indices are bounded by the on-screen swatch \
                  grid, far below f64's exact-integer range"
    )]
    fn recompute_swatches(&mut self) {
        self.swatch_rects.clear();
        let pad = 8.0;
        for i in 0..self.palette.len() {
            let r = i.checked_div(self.columns).unwrap_or(0) as f64;
            let c = i.checked_rem(self.columns).unwrap_or(0) as f64;
            let x = self.rect.x0 + pad + c * self.cell;
            let y = self.rect.y0 + pad + r * self.cell;
            self.swatch_rects.push(Rect::new(
                x + 2.0,
                y + 2.0,
                x + self.cell - 2.0,
                y + self.cell - 2.0,
            ));
        }
    }

    /// `(width, height)` of the swatch grid in px — shared by `build_layout`
    /// and `measure` so they can't disagree.
    #[allow(
        clippy::cast_precision_loss,
        reason = "column/row counts describe a small, on-screen swatch grid, \
                  far below f32's exact-integer range"
    )]
    fn grid_size(&self) -> (f32, f32) {
        let pad = 8.0_f32;
        let cell = px(self.cell);
        let w = self.columns as f32 * cell + pad * 2.0;
        let h = self.rows() as f32 * cell + pad * 2.0;
        (w, h)
    }
}

impl Default for ColorSwatchPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for ColorSwatchPicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (w, h) = self.grid_size();
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: length(w),
                height: length(h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let (width, height) = self.grid_size();
        taffy::Size { width, height }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.recompute_swatches();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        let panel = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &panel,
        );

        for (i, (&rgb, sr)) in self
            .palette
            .iter()
            .zip(self.swatch_rects.iter())
            .enumerate()
        {
            let shape = RoundedRect::from_rect(*sr, 6.0);
            scene.fill(Fill::NonZero, transform, packed_color(rgb), None, &shape);
            if self.selected == Some(i) {
                scene.stroke(
                    &Stroke::new(2.5),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            } else if self.hovered == Some(i) {
                scene.stroke(&Stroke::new(2.0), transform, Color::WHITE, None, &shape);
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hovered = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor))
                {
                    self.selected = Some(i);
                    self.choice = Some(i);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ColorSwatchPicker"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Each swatch is drawn (not a child widget), so it becomes a
        // synthetic `ListItem` named by its hex value.
        let hex = |rgb: u32| format!("#{rgb:06X}");
        let count = self.palette.len();
        let value = self
            .selected
            .and_then(|i| self.palette.get(i))
            .map(|&rgb| hex(rgb));
        let items: Vec<AccessNode> = self
            .palette
            .iter()
            .enumerate()
            .map(|(i, &rgb)| {
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!("swatch:{i}"))
                    .with_name(hex(rgb))
                    .with_state(AccessState {
                        selected: self.selected == Some(i),
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count)
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    ..Default::default()
                })
                .with_children(items),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [swatch] = path else {
            return None;
        };
        self.swatch_rects.get(*swatch).copied()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        let Some(index) = target
            .strip_prefix("swatch:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.palette.len())
        else {
            return false;
        };
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.selected = Some(index);
        self.choice = Some(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay_out(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
                |k, a, id, _, _| {
                    if let Some(&ptr) = node_map.get(&id) {
                        unsafe { (*ptr).measure(k, a, &mut tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn synth_device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn tri_state_click_selects_segment() {
        let mut t = TriStateControl::allow_inherit_deny(); // starts on middle
        lay_out(&mut t);
        assert_eq!(t.synthetic_accessibility_rect(&[0]), Some(t.seg_rects[0]));
        let p = t.seg_rects[0].center();
        dispatch(&mut t, &cursor_at(p));
        assert!(dispatch(&mut t, &left_press()), "segment click consumed");
        assert_eq!(t.selected(), 0);
        assert_eq!(t.take_choice(), Some(0));
        assert_eq!(t.take_choice(), None, "choice drains once");
    }

    #[test]
    fn tri_state_reclick_active_toggles_to_neutral() {
        let mut t = TriStateControl::allow_inherit_deny().with_selected(2); // Deny
        lay_out(&mut t);
        let p = t.seg_rects[2].center();
        dispatch(&mut t, &cursor_at(p));
        dispatch(&mut t, &left_press());
        assert_eq!(
            t.selected(),
            1,
            "re-clicking the active segment falls to neutral"
        );
        assert_eq!(t.take_choice(), Some(1));
    }

    #[test]
    fn synthetic_segment_and_swatch_actions_use_the_pointer_choice_paths() {
        let mut tri = TriStateControl::allow_inherit_deny().with_selected(2);
        assert!(Widget::perform_semantic_action(
            &mut tri,
            "segment:0",
            &crate::a11y::Action::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert_eq!(tri.selected(), 0);
        assert_eq!(tri.take_choice(), Some(0));

        let mut picker = ColorSwatchPicker::new();
        assert!(Widget::perform_semantic_action(
            &mut picker,
            "swatch:5",
            &crate::a11y::Action::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert_eq!(picker.take_choice(), Some(5));
        assert!(
            Widget::accessibility(&picker).unwrap().children[5]
                .state
                .selected
        );
    }

    #[test]
    fn swatch_picker_records_clicked_index() {
        let mut p = ColorSwatchPicker::new();
        lay_out(&mut p);
        assert_eq!(
            p.synthetic_accessibility_rect(&[5]),
            Some(p.swatch_rects[5])
        );
        let c = p.swatch_rects[5].center();
        dispatch(&mut p, &cursor_at(c));
        assert!(dispatch(&mut p, &left_press()), "swatch click consumed");
        assert_eq!(p.take_choice(), Some(5));
        assert_eq!(p.color_at(5), Some(DEFAULT_PALETTE[5]));
    }

    #[test]
    fn swatch_picker_grid_has_all_swatches() {
        let mut p = ColorSwatchPicker::new();
        lay_out(&mut p);
        assert_eq!(p.swatch_rects.len(), 12, "all 12 palette swatches laid out");
        // 4 columns → 3 rows; row 1 swatch sits below row 0.
        assert!(p.swatch_rects[4].y0 > p.swatch_rects[0].y0);
    }
}
