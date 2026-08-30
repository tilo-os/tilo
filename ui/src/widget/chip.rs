//! `Chip` — a compact interactive pill for tags, filters, and tokens. Distinct
//! from [`super::Badge`] (a tiny non-interactive count/dot): a chip carries a
//! label and can be **selectable** (a filter that toggles on/off, tinting to
//! the accent when active) and/or **removable** (a trailing "×" that removes a
//! token, e.g. a recipient or an applied filter).
//!
//! Both interactions surface through the toolkit's drain channel —
//! [`Chip::take_toggled`] and [`Chip::take_removed`] — rather than callbacks,
//! so a CRDT-native host reads them each frame and mutates its own model.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::AccentChip;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

const HEIGHT: f64 = 28.0;
const PAD_X: f64 = 12.0;
const GAP: f64 = 6.0;
const DOT_D: f64 = 8.0;
const X_D: f64 = 14.0;

pub struct Chip {
    label: String,
    selectable: bool,
    selected: bool,
    removable: bool,
    dot: Option<Color>,
    toggled: bool,
    removed: bool,
    rect: Rect,
    x_rect: Rect,
    last_cursor: Point,
    label_w: f64,
}

impl Chip {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            selectable: false,
            selected: false,
            removable: false,
            dot: None,
            toggled: false,
            removed: false,
            rect: Rect::ZERO,
            x_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            label_w: 0.0,
        }
    }

    /// Make the chip a toggle (a filter). Clicking the body flips selection.
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// Set the initial selected state (implies [`selectable`](Self::selectable)).
    pub fn selected(mut self, on: bool) -> Self {
        self.selectable = true;
        self.selected = on;
        self
    }

    /// Show a trailing "×" that removes the chip when clicked.
    pub fn removable(mut self) -> Self {
        self.removable = true;
        self
    }

    /// Add a small leading colour dot (e.g. a calendar/category colour).
    pub fn with_dot(mut self, color: Color) -> Self {
        self.dot = Some(color);
        self
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Programmatically set selection (host syncing from its model).
    pub fn set_selected(&mut self, on: bool) {
        self.selected = on;
    }

    /// Drain the "selection toggled" signal (fires once per click on the body
    /// of a selectable chip). Read `is_selected()` for the new value.
    pub fn take_toggled(&mut self) -> bool {
        std::mem::take(&mut self.toggled)
    }

    /// Drain the "× clicked / remove me" signal (fires once).
    pub fn take_removed(&mut self) -> bool {
        std::mem::take(&mut self.removed)
    }

    /// The "×" hit rect (empty until laid out + drawn). Exposed for tests.
    pub fn remove_rect(&self) -> Rect {
        self.x_rect
    }

    fn intrinsic_width(&self, text_renderer: &mut TextRenderer, theme: &Theme) -> f64 {
        let (w, _) = text_renderer.measure(&self.label, theme.tokens.typography.scale.sm, None);
        let mut total = PAD_X * 2.0 + f64::from(w);
        if self.dot.is_some() {
            total += DOT_D + GAP;
        }
        if self.removable {
            total += GAP + X_D;
        }
        total
    }
}

/// What sits in a chip's leading slot when it is painted immediately by
/// [`Chip::draw_at`].
#[derive(Clone, Copy, Debug, Default)]
pub enum ChipLead {
    /// Label only.
    #[default]
    None,
    /// A colour-coded dot — the same affordance [`Chip::with_dot`] paints.
    Dot(Color),
    /// A stroked glyph, for chips that name an action rather than a value.
    Glyph(Icon),
}

/// Paint-time variant of an immediate-mode chip. Mirrors the states the
/// [`Chip`] widget itself has — quiet by default, tinted when it is the
/// selected one of a set — plus a `ghost` slot-shape for "there could be
/// another one here" affordances.
#[derive(Clone, Copy, Debug)]
pub struct ChipStyle {
    /// Accent for this chip. `Some` paints the selected treatment (tinted
    /// fill, tinted border, tinted label); `None` the quiet one.
    pub tint: Option<Color>,
    /// Dashed border and dimmed label — an empty slot, not a value.
    pub ghost: bool,
    /// Drop the pill entirely and paint only the lead and the label — for
    /// chips that already sit inside something doing the containing (a lit
    /// disc sector, a row that is itself the selection), where a pill reads
    /// as a box inside a box.
    pub bare: bool,
    /// Multiplies every painted alpha. Lets a caller fade a whole chip
    /// without recolouring it stop by stop.
    pub alpha: f32,
    /// Overrides the derivation from [`Self::tint`] — `accent × 0.18` is a
    /// different colour on a white panel than on a dark menu, and only the
    /// composite can be measured, so a caller that knows its ground resolves
    /// the three through
    /// [`Colors::accent_chip_on`](crate::tokens::colors::Colors::accent_chip_on).
    pub resolved: Option<AccentChip>,
}

impl Default for ChipStyle {
    fn default() -> Self {
        Self {
            tint: None,
            ghost: false,
            bare: false,
            alpha: 1.0,
            resolved: None,
        }
    }
}

impl ChipStyle {
    /// Quiet chip — theme surface, standard border, primary label.
    pub fn quiet() -> Self {
        Self::default()
    }

    /// Selected chip — `tint` carries the fill, border and label colour.
    pub fn tinted(tint: Color) -> Self {
        Self {
            tint: Some(tint),
            ..Self::default()
        }
    }

    /// Empty-slot chip — dashed border, dimmed contents.
    pub fn ghost() -> Self {
        Self {
            ghost: true,
            ..Self::default()
        }
    }

    /// `tint` still colours the lead and the label, so a bare chip has
    /// the same quiet/selected pair the pill form does. See
    /// [`ChipStyle::bare`] for when this is the right shape.
    pub fn bare(tint: Option<Color>) -> Self {
        Self {
            tint,
            bare: true,
            ..Self::default()
        }
    }

    /// Chip painted from colours the caller has already measured against the
    /// ground it lands on. See [`ChipStyle::resolved`].
    pub fn measured(chip: AccentChip) -> Self {
        Self {
            resolved: Some(chip),
            ..Self::default()
        }
    }

    /// Fade the whole chip by `alpha` (clamped to `0.0..=1.0`).
    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
}

impl Chip {
    /// Text size a chip of `height` px paints its label at. The widget's
    /// own [`HEIGHT`] maps to `typography.scale.sm`, and anything taller or
    /// shorter scales with it, so a caller that wants a bigger pill gets
    /// proportionally bigger type instead of a lonely small label floating
    /// in a tall capsule.
    pub fn label_size_for_height(height: f64, theme: &Theme) -> f32 {
        theme.tokens.typography.scale.sm * (height / HEIGHT).clamp(0.75, 1.6) as f32
    }

    /// Intrinsic width of an immediate-mode chip — what [`Chip::draw_at`]
    /// needs to paint `label` and `lead` at `height` without truncating.
    pub fn natural_width_at(
        label: &str,
        height: f64,
        lead: ChipLead,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) -> f64 {
        let size = Self::label_size_for_height(height, theme);
        let (w, _) = text_renderer.measure(label, size, None);
        let lead_w = match lead {
            ChipLead::None => 0.0,
            ChipLead::Dot(_) => DOT_D + GAP,
            ChipLead::Glyph(_) => height * 0.44 + GAP,
        };
        PAD_X * 2.0 + lead_w + f64::from(w)
    }

    /// No layout node, caching, or widget instance — for a list that lays
    /// out its own rows (the sibling of
    /// [`Avatar::draw_at`](super::Avatar::draw_at)): on a curve, in a rail,
    /// or at any position taffy won't hand back.
    ///
    /// The label is **truncated with an ellipsis** to whatever width `rect`
    /// allows — use [`Chip::natural_width_at`] to size `rect` when the space is free.
    // Standard immediate-mode paint-helper signature for this vello
    // renderer (scene + transform + text_renderer + theme, plus this
    // draw's own content); every parameter is independently load-bearing.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        label: &str,
        lead: ChipLead,
        style: ChipStyle,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let a = style.alpha.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let fade = |c: Color, f: f32| c.with_alpha_factor(f * a);

        let colors = &theme.tokens.colors;
        let pill = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.full));

        // Surface + border. A tinted chip carries its accent at the same
        // 0.18 fill / full-strength border the selected widget state uses,
        // so a rail of these reads identically to a row of real Chips.
        if !style.bare {
            let (bg, border) = match (style.resolved, style.tint, style.ghost) {
                (Some(c), _, false) => (c.fill, c.border),
                (_, _, true) => (colors.surface_white_5, colors.border_standard),
                (_, Some(t), _) => (t.with_alpha_factor(0.18), t),
                (_, None, _) => (colors.surface_white_5, colors.border_standard),
            };
            scene.fill(Fill::NonZero, transform, fade(bg, 1.0), None, &pill);
            let stroke = if style.ghost {
                Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0])
            } else {
                Stroke::new(1.0)
            };
            scene.stroke(&stroke, transform, fade(border, 1.0), None, &pill);
        }

        let cy = rect.center().y;
        // An icon-only chip centres its lead: with no label to sit beside,
        // a glyph pinned to the left padding reads as a pill someone
        // forgot to finish rather than as a compact action.
        let lead_w = match lead {
            ChipLead::None => 0.0,
            ChipLead::Dot(_) => DOT_D,
            ChipLead::Glyph(_) => rect.height() * 0.44,
        };
        let mut x = if label.is_empty() && lead_w > 0.0 {
            rect.center().x - lead_w / 2.0
        } else {
            rect.x0 + PAD_X
        };
        // A bare chip has no fill to sit the lead against, so a `text_dim`
        // glyph beside a `text_primary` label reads as two unrelated marks
        // rather than as one token. Pull the lead up to the label there.
        let lead_col = match (style.resolved, style.tint) {
            (Some(c), _) if !style.bare => c.ink,
            (_, Some(t)) => t,
            (_, None) if style.bare => colors.text_primary,
            (_, None) => colors.text_dim,
        };
        match lead {
            ChipLead::None => {}
            ChipLead::Dot(c) => {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    fade(c, 1.0),
                    None,
                    &Circle::new(Point::new(x + DOT_D / 2.0, cy), DOT_D / 2.0),
                );
                x += DOT_D + GAP;
            }
            ChipLead::Glyph(icon) => {
                let d = rect.height() * 0.44;
                let gr = Rect::from_center_size(Point::new(x + d / 2.0, cy), (d, d));
                icon.draw_with_stroke(scene, transform, gr, fade(lead_col, 1.0), 1.6);
                x += d + GAP;
            }
        }

        // Label, truncated to the space actually left inside the pill.
        let size = Self::label_size_for_height(rect.height(), theme);
        let avail = rect.x1 - PAD_X - x;
        if avail <= 1.0 {
            return;
        }
        let shown = truncate_to_width(label, size, avail, text_renderer);
        if shown.is_empty() {
            return;
        }
        let (_, lh) = text_renderer.measure(&shown, size, None);
        let label_col = match (style.resolved, style.tint, style.ghost) {
            (Some(c), _, false) if !style.bare => c.ink,
            (_, _, true) => colors.text_dim,
            (_, Some(t), _) => t,
            (_, None, _) => colors.text_primary,
        };
        text_renderer.draw_text(
            scene,
            &shown,
            size,
            fade(label_col, 1.0),
            transform * Affine::translate((x, cy - f64::from(lh) / 2.0)),
            None,
        );
    }
}

/// Longest prefix of `label` that fits `avail` px at `size`, with an
/// ellipsis appended when anything was dropped. Returns an empty string
/// when not even one character plus the ellipsis fits — callers treat that
/// as "no room for a label here" rather than painting a lone `…`.
fn truncate_to_width(
    label: &str,
    size: f32,
    avail: f64,
    text_renderer: &mut TextRenderer,
) -> String {
    let (full, _) = text_renderer.measure(label, size, None);
    if f64::from(full) <= avail {
        return label.to_string();
    }
    let chars: Vec<char> = label.chars().collect();
    // Walk down from the full string; the first prefix that fits wins.
    // Labels here are workspace names — short enough that a linear scan
    // costs less than the branchy binary search it would replace.
    for take in (1..chars.len()).rev() {
        let mut cand: String = chars[..take].iter().collect();
        cand.push('…');
        let (w, _) = text_renderer.measure(&cand, size, None);
        if f64::from(w) <= avail {
            return cand;
        }
    }
    String::new()
}

impl Widget for Chip {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Approximate width from a fixed advance; measure() refines it.
        let approx = PAD_X * 2.0
            + self.label.chars().count() as f64
                * (f64::from(theme.tokens.typography.scale.sm) * 0.6)
            + if self.dot.is_some() { DOT_D + GAP } else { 0.0 }
            + if self.removable { GAP + X_D } else { 0.0 };
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: length(approx as f32),
                height: length(HEIGHT as f32),
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
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size {
            width: self.intrinsic_width(text_renderer, &Theme::dark()) as f32,
            height: HEIGHT as f32,
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
        let accent = theme.accent_primary;
        let selected = self.selectable && self.selected;

        let pill = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.full));
        let bg = if selected {
            accent.with_alpha_factor(0.18)
        } else {
            theme.tokens.colors.surface_white_5
        };
        let border = if selected {
            accent
        } else {
            theme.border_standard
        };
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        scene.stroke(&Stroke::new(1.0), transform, border, None, &pill);

        let mut x = self.rect.x0 + PAD_X;
        let cy = self.rect.center().y;

        if let Some(dot) = self.dot {
            scene.fill(
                Fill::NonZero,
                transform,
                dot,
                None,
                &Circle::new(Point::new(x + DOT_D / 2.0, cy), DOT_D / 2.0),
            );
            x += DOT_D + GAP;
        }

        let size = theme.tokens.typography.scale.sm;
        let (lw, lh) = text_renderer.measure(&self.label, size, None);
        self.label_w = f64::from(lw);
        let label_col = if selected { accent } else { theme.text_primary };
        text_renderer.draw_text(
            scene,
            &self.label,
            size,
            label_col,
            transform * Affine::translate((x, cy - f64::from(lh) / 2.0)),
            None,
        );
        x += f64::from(lw);

        if self.removable {
            x += GAP;
            let xr = Rect::from_center_size(Point::new(x + X_D / 2.0, cy), (X_D, X_D));
            self.x_rect = xr;
            let hovered = xr.contains(self.last_cursor);
            let col = if hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
            Icon::X.draw_with_stroke(scene, transform, xr, col, 1.6);
        } else {
            self.x_rect = Rect::ZERO;
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
                if self.removable && self.x_rect.contains(self.last_cursor) {
                    self.removed = true;
                    *handled = true;
                } else if self.selectable && self.rect.contains(self.last_cursor) {
                    self.selected = !self.selected;
                    self.toggled = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.selectable || self.removable) && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        "Chip"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if self.selectable {
            Some(
                AccessNode::new(Role::ToggleButton)
                    .with_name(self.label.clone())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.selected)
                            .checked(self.selected),
                    ),
            )
        } else {
            Some(AccessNode::new(Role::StaticText).with_value(self.label.clone()))
        }
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
                    height: taffy::AvailableSpace::Definite(60.0),
                },
                |k, a, id, _, _| {
                    node_map
                        .get(&id)
                        .map(|&p| unsafe { (*p).measure(k, a, &mut tr) })
                        .unwrap_or(taffy::Size::ZERO)
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn draw(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn synth() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth(),
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
    fn selectable_chip_toggles_on_body_click() {
        let mut c = Chip::new("Rust").selectable();
        lay_out(&mut c);
        draw(&mut c);
        assert!(!c.is_selected());
        let center = c.get_rect().center();
        dispatch(&mut c, &cursor(center));
        assert!(
            dispatch(&mut c, &press()),
            "body click on a selectable chip is consumed"
        );
        assert!(c.is_selected(), "click selects");
        assert!(c.take_toggled(), "toggle signal fires");
        assert!(!c.take_toggled(), "toggle drains after one read");
        // Click again → deselect.
        dispatch(&mut c, &press());
        assert!(!c.is_selected(), "second click deselects");
    }

    #[test]
    fn plain_chip_does_not_toggle() {
        let mut c = Chip::new("Label");
        lay_out(&mut c);
        draw(&mut c);
        let center = c.get_rect().center();
        dispatch(&mut c, &cursor(center));
        assert!(!dispatch(&mut c, &press()), "a plain chip ignores clicks");
        assert!(!c.take_toggled());
    }

    #[test]
    fn removable_chip_fires_remove_on_x_click() {
        let mut c = Chip::new("alice@example.com").removable();
        lay_out(&mut c);
        draw(&mut c);
        let x = c.remove_rect().center();
        assert!(x.x > 0.0, "× rect resolves after draw");
        dispatch(&mut c, &cursor(x));
        assert!(dispatch(&mut c, &press()), "× click is consumed");
        assert!(c.take_removed(), "remove signal fires");
        assert!(!c.take_removed(), "remove drains after one read");
    }

    #[test]
    fn removable_and_selectable_x_wins_over_toggle() {
        let mut c = Chip::new("tag").selectable().removable();
        lay_out(&mut c);
        draw(&mut c);
        let x = c.remove_rect().center();
        dispatch(&mut c, &cursor(x));
        dispatch(&mut c, &press());
        assert!(c.take_removed(), "× removes");
        assert!(!c.take_toggled(), "× does not also toggle selection");
        assert!(!c.is_selected());
    }

    #[test]
    fn accessibility_toggle_vs_static() {
        use crate::a11y::Role;
        let sel = Chip::new("Filter").selected(true);
        let node = Widget::accessibility(&sel).unwrap();
        assert_eq!(node.role, Role::ToggleButton);
        assert_eq!(
            node.state.checked,
            Some(true),
            "a selected chip reports checked"
        );
        assert!(node.state.focusable);

        let plain = Chip::new("tag");
        assert_eq!(
            Widget::accessibility(&plain).unwrap().role,
            Role::StaticText
        );
    }
}
