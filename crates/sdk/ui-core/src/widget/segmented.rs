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

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::{ON_FILL_MIN_CONTRAST, over};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::text::centered_text_top;

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
        // Track with its derived rest edge (HIG §9.5), stroked half a pixel
        // inside so the hairline never bleeds onto the ground.
        crate::surface::card(
            scene,
            transform,
            self.rect,
            radius,
            theme.tokens.colors.surfaces.raised,
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

        let text_size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
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
            let (tw, th) = text_renderer.measure(label, text_size, None);
            let cx = seg_rect.center().x - f64::from(tw) / 2.0;
            // The measured line, not the font size, is what gets centred —
            // see `widget::text::centered_text_top`.
            let cy = centered_text_top(self.rect.y0, self.rect.height(), f64::from(th));
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    *ctx.handled = true;
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
        _state: &mut AppModel,
        _path: &ModelPath,
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
    /// Optional human-friendly names for the palette entries. Apps can use
    /// these when a raw hexadecimal value is not meaningful to a screen-reader
    /// user (for example, "blue text" versus "yellow highlight").
    accessible_labels: Option<Vec<String>>,
    columns: usize,
    cell: f64,
    selected: Option<usize>,
    choice: Option<usize>,
    rect: Rect,
    swatch_rects: Vec<Rect>,
    last_cursor: Point,
    hovered: Option<usize>,
    focused: bool,
    focus_index: usize,
    /// Told each chosen index, as well as [`ColorSwatchPicker::take_choice`].
    on_change: Option<Box<dyn FnMut(usize)>>,
    /// Lay the swatches out on their own, without the raised panel and its
    /// padding: for a palette inline in a form.
    bare: bool,
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
            accessible_labels: None,
            columns: columns.max(1),
            cell: 30.0,
            selected: None,
            choice: None,
            rect: Rect::ZERO,
            swatch_rects: Vec::new(),
            last_cursor: Point::ZERO,
            hovered: None,
            focused: false,
            focus_index: 0,
            on_change: None,
            bare: false,
        }
    }

    /// Run `f` with each index the user chooses.
    #[must_use]
    pub fn on_change(mut self, f: impl FnMut(usize) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// The side of one swatch's cell, in logical px (default 30).
    #[must_use]
    pub fn with_cell(mut self, cell: f64) -> Self {
        self.cell = cell.max(12.0);
        self
    }

    /// Without the raised panel and its padding.
    #[must_use]
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Show `idx` as chosen (`None`: none is), without telling `on_change`.
    pub fn set_selected(&mut self, idx: Option<usize>) {
        self.selected = idx.filter(|idx| *idx < self.palette.len());
    }

    fn chose(&mut self, idx: usize) {
        self.selected = Some(idx);
        self.choice = Some(idx);
        if let Some(on_change) = self.on_change.as_mut() {
            on_change(idx);
        }
    }

    fn pad(&self) -> f64 {
        if self.bare { 0.0 } else { 8.0 }
    }

    #[must_use]
    pub fn with_selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    /// Supply one accessible name per palette entry. Mismatched label lists are
    /// ignored so callers can safely use a generated palette and retain the
    /// stable hexadecimal fallback rather than creating partially named items.
    #[must_use]
    pub fn with_accessible_labels(mut self, labels: Vec<String>) -> Self {
        if labels.len() == self.palette.len() {
            self.accessible_labels = Some(labels);
        }
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
        let pad = self.pad();
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
        let pad = px(self.pad());
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
        // Panel with its derived rest edge (HIG §9.5), crisp 1 px inset.
        if !self.bare {
            crate::surface::card(
                scene,
                transform,
                self.rect,
                f64::from(theme.tokens.radii.md),
                theme.tokens.colors.surfaces.raised,
            );
        }

        for (i, (&rgb, sr)) in self
            .palette
            .iter()
            .zip(self.swatch_rects.iter())
            .enumerate()
        {
            let shape = RoundedRect::from_rect(*sr, 6.0);
            let swatch = packed_color(rgb);
            scene.fill(Fill::NonZero, transform, swatch, None, &shape);
            // A rest edge, so a swatch the colour of the panel still shows.
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(theme.tokens.colors.surfaces.raised),
                None,
                &crate::surface::edge_outline(*sr, 6.0),
            );
            if self.selected == Some(i) || (self.focused && self.focus_index == i) {
                scene.stroke(
                    &Stroke::new(2.5),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            } else if self.hovered == Some(i) {
                // Hover ring derived against the swatch it rings (HIG §9.5):
                // a literal white ring disappeared on the pale swatches.
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    crate::tokens::colors::Colors::hairline_on(swatch, crate::chrome::GLOW_ALPHA),
                    None,
                    &shape,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    self.chose(i);
                    self.focused = true;
                    self.focus_index = i;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused && !self.palette.is_empty() => {
                let last = self.palette.len().saturating_sub(1);
                let next = match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.focus_index.saturating_sub(1),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.focus_index.saturating_add(1).min(last)
                    }
                    Key::Named(NamedKey::ArrowUp) => self.focus_index.saturating_sub(self.columns),
                    Key::Named(NamedKey::ArrowDown) => {
                        self.focus_index.saturating_add(self.columns).min(last)
                    }
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => last,
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.chose(self.focus_index);
                        *ctx.handled = true;
                        return;
                    }
                    _ => return,
                };
                self.focus_index = next;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ColorSwatchPicker"
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let labels = self.accessible_labels.as_ref()?;
        let index = self.swatch_rects.iter().position(|r| r.contains(point))?;
        Some((*self.swatch_rects.get(index)?, labels.get(index)?.clone()))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.focus_index = self
                .selected
                .unwrap_or(0)
                .min(self.palette.len().saturating_sub(1));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Each swatch is drawn (not a child widget), so it becomes a
        // synthetic `ListItem`. Apps may provide a meaningful label; generic
        // consumers retain the stable hexadecimal fallback.
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
                let name = self
                    .accessible_labels
                    .as_ref()
                    .and_then(|labels| labels.get(i))
                    .cloned()
                    .unwrap_or_else(|| hex(rgb));
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!("swatch:{i}"))
                    .with_name(name)
                    // The cells are custom-painted synthetic descendants, so
                    // export their real logical-pixel rects for AT hit testing
                    // and overlay inspection just like the shared pointer path.
                    .with_bounds(self.swatch_rects.get(i).copied().unwrap_or(Rect::ZERO))
                    .with_state(AccessState {
                        focusable: true,
                        selected: self.selected == Some(i),
                        focused: self.focused && self.focus_index == i,
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let Some(index) = target
            .strip_prefix("swatch:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.palette.len())
        else {
            return false;
        };
        if matches!(action, crate::a11y::Action::Focus) {
            self.focused = true;
            self.focus_index = index;
            return true;
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.chose(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

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

    #[test]
    fn tri_state_segments_centre_labels_select_and_toggle_neutral() {
        // Case `tri_state_labels_centre_their_measured_line`.
        // Segment labels are centred on their measured line inside the 28 px
        // strip; centring on the 12 px font size alone dropped them 1.5 px.
        {
            let labels = ["Yes", "Maybe", "No"];
            let mut control = TriStateControl::new(
                labels,
                [
                    SegmentTone::Positive,
                    SegmentTone::Neutral,
                    SegmentTone::Negative,
                ],
            );
            lay_out(&mut control);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            control.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &PresenceCtx::default(),
            );
            let rect = control.rect;
            assert!(rect.height() > 0.0);
            assert_eq!(scene.encoding().resources.glyph_runs.len(), labels.len());

            let size = theme.tokens.typography.scale.sm;
            for (index, label) in labels.into_iter().enumerate() {
                crate::widget::text::assert_glyph_run_centred_in_band(
                    &scene,
                    index,
                    label,
                    size,
                    rect.y0,
                    rect.height(),
                    &mut tr,
                    theme.text_primary,
                );
            }
        }

        // Case `tri_state_click_selects_segment`.
        {
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

        // Case `tri_state_reclick_active_toggles_to_neutral`.
        {
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
    fn named_press(key: NamedKey) -> WindowEvent {
        use winit::keyboard::{NativeKeyCode, PhysicalKey};

        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.logical_key = Key::Named(key);
        event.text = None;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event,
            is_synthetic: false,
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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn swatch_picker_choices_match_across_pointer_keyboard_and_semantics() {
        // Case `synthetic_segment_and_swatch_actions_use_the_pointer_choice_paths`.
        {
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

        // Case `swatch_picker_records_clicked_index`.
        {
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

        // Case `swatch_picker_keyboard_navigation_and_activation_match_pointer_choice`.
        {
            let mut picker =
                ColorSwatchPicker::with_palette(vec![0x11_22_33, 0x44_55_66, 0x77_88_99], 2);
            lay_out(&mut picker);
            Widget::set_focus(&mut picker, true);
            assert!(dispatch(&mut picker, &named_press(NamedKey::ArrowRight)));
            assert!(dispatch(&mut picker, &named_press(NamedKey::ArrowDown)));
            assert!(dispatch(&mut picker, &named_press(NamedKey::Space)));
            assert_eq!(picker.take_choice(), Some(2));
            let tree = Widget::accessibility(&picker).expect("picker semantics");
            assert!(tree.children[2].state.selected);
            assert!(tree.children[2].state.focusable);
            assert_eq!(tree.children[2].bounds, Some(picker.swatch_rects[2].into()));
            assert!(Widget::perform_semantic_action(
                &mut picker,
                "swatch:1",
                &crate::a11y::Action::Focus,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert!(
                Widget::accessibility(&picker)
                    .expect("focused picker")
                    .children[1]
                    .state
                    .focused
            );
        }

        // Case `swatch_picker_grid_has_all_swatches`.
        {
            let mut p = ColorSwatchPicker::new();
            lay_out(&mut p);
            assert_eq!(p.swatch_rects.len(), 12, "all 12 palette swatches laid out");
            // 4 columns → 3 rows; row 1 swatch sits below row 0.
            assert!(p.swatch_rects[4].y0 > p.swatch_rects[0].y0);
        }

        // Case `swatch_picker_uses_supplied_accessible_labels_and_keeps_hex_fallback`.
        {
            let named = ColorSwatchPicker::with_palette(vec![0x11_22_33, 0x44_55_66], 2)
                .with_accessible_labels(vec!["blue text".into(), "green text".into()]);
            let named_tree = Widget::accessibility(&named).expect("picker semantics");
            assert_eq!(named_tree.children[0].name.as_deref(), Some("blue text"));
            assert_eq!(named_tree.children[1].name.as_deref(), Some("green text"));

            let fallback = ColorSwatchPicker::with_palette(vec![0x11_22_33], 1)
                .with_accessible_labels(vec!["wrong length".into(), "ignored".into()]);
            let fallback_tree = Widget::accessibility(&fallback).expect("picker semantics");
            assert_eq!(fallback_tree.children[0].name.as_deref(), Some("#112233"));
        }
    }
}
