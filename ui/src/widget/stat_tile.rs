//! `StatTile` — a compact "big number + caption" metric card. Dashboards and
//! monitors (`monitor`, `firewall`, `containers`, `disks`, `vpn`, …) each
//! hand-roll a `dim caption + bold value` tile, several with a trend delta or a
//! semantic value colour. `StatTile` folds that into one theme-driven card:
//!
//! - a **caption** (the dim label, e.g. "CPU"),
//! - a big **value** (e.g. "42%"), optionally tinted with a semantic colour,
//! - an optional **delta** line with an up/down/flat [`Trend`] arrow (e.g.
//!   "▲ 12%") coloured green/red/dim,
//! - an optional leading **icon** in the top-right corner.
//!
//! Like the other promoted visuals it ships in two forms: the retained
//! [`Widget`] (drop it into a `Row` for an even split) and the immediate-mode
//! [`StatTile::paint`] entry point for the many apps that paint to a `Scene`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

const PAD: f64 = 14.0;
const ICON_D: f64 = 18.0;

/// Direction of a [`StatTile`] delta — selects the arrow glyph and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    /// Value rose — up chevron, success colour.
    Up,
    /// Value fell — down chevron, danger colour.
    Down,
    /// No meaningful change — no arrow, dim colour.
    Flat,
}

impl Trend {
    fn color(self, theme: &Theme) -> Color {
        match self {
            Trend::Up => theme.tokens.colors.semantic.success,
            Trend::Down => theme.tokens.colors.semantic.danger,
            Trend::Flat => theme.text_dim,
        }
    }

    fn arrow(self) -> Option<Icon> {
        match self {
            Trend::Up => Some(Icon::ChevronUp),
            Trend::Down => Some(Icon::ChevronDown),
            Trend::Flat => None,
        }
    }
}

/// The tile height with a delta line vs. without.
fn tile_height(has_delta: bool, theme: &Theme) -> f64 {
    let caption = f64::from(theme.tokens.typography.scale.sm);
    let value = f64::from(theme.tokens.typography.scale.x2l);
    let delta = if has_delta {
        6.0 + f64::from(theme.tokens.typography.scale.sm)
    } else {
        0.0
    };
    PAD * 2.0 + caption + 6.0 + value + delta
}

/// Shared paint of a stat tile filling `rect`. Used by both the retained
/// [`StatTile`] widget and the immediate-mode [`StatTile::paint`] entry point.
#[allow(clippy::too_many_arguments)]
fn paint_stat_tile(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    caption: &str,
    value: &str,
    value_color: Color,
    icon: Option<Icon>,
    delta: Option<(&str, Trend)>,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let card = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.lg));
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &card,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.border_standard,
        None,
        &card,
    );

    let caption_size = theme.tokens.typography.scale.sm;
    let value_size = theme.tokens.typography.scale.x2l;
    let delta_size = theme.tokens.typography.scale.sm;

    let x = rect.x0 + PAD;
    let mut y = rect.y0 + PAD;

    text_renderer.draw_text(
        scene,
        caption,
        caption_size,
        theme.text_dim,
        transform * Affine::translate((x, y)),
        None,
    );

    if let Some(icon) = icon {
        let ir = Rect::from_center_size(
            Point::new(rect.x1 - PAD - ICON_D / 2.0, rect.y0 + PAD + ICON_D / 2.0),
            (ICON_D, ICON_D),
        );
        icon.draw_with_stroke(scene, transform, ir, theme.text_dim, 1.6);
    }

    y += f64::from(caption_size) + 6.0;

    text_renderer.draw_text(
        scene,
        value,
        value_size,
        value_color,
        transform * Affine::translate((x, y)),
        None,
    );
    y += f64::from(value_size) + 6.0;

    if let Some((delta_text, trend)) = delta {
        let col = trend.color(theme);
        let mut dx = x;
        if let Some(arrow) = trend.arrow() {
            let ar = Rect::new(dx, y, dx + f64::from(delta_size), y + f64::from(delta_size));
            arrow.draw_with_stroke(scene, transform, ar, col, 1.8);
            dx += f64::from(delta_size) + 2.0;
        }
        text_renderer.draw_text(
            scene,
            delta_text,
            delta_size,
            col,
            transform * Affine::translate((dx, y)),
            None,
        );
    }
}

/// A metric card: caption + big value + optional trend delta + optional icon.
pub struct StatTile {
    caption: String,
    value: String,
    icon: Option<Icon>,
    value_color: Option<Color>,
    delta: Option<(String, Trend)>,
    rect: Rect,
}

impl StatTile {
    pub fn new(caption: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            caption: caption.into(),
            value: value.into(),
            icon: None,
            value_color: None,
            delta: None,
            rect: Rect::ZERO,
        }
    }

    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Defaults to `theme.text_primary` when unset.
    pub fn with_value_color(mut self, color: Color) -> Self {
        self.value_color = Some(color);
        self
    }

    /// E.g. `("12%", Trend::Up)`.
    pub fn with_delta(mut self, text: impl Into<String>, trend: Trend) -> Self {
        self.delta = Some((text.into(), trend));
        self
    }

    pub fn value_color(&self, theme: &Theme) -> Color {
        self.value_color.unwrap_or(theme.text_primary)
    }

    /// For apps that render straight to a `Scene`.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        caption: &str,
        value: &str,
        value_color: Color,
        icon: Option<Icon>,
        delta: Option<(&str, Trend)>,
    ) {
        paint_stat_tile(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            caption,
            value,
            value_color,
            icon,
            delta,
        );
    }
}

impl Widget for StatTile {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let h = tile_height(self.delta.is_some(), theme) as f32;
        // Grows to share a row evenly, but keeps the fixed content height.
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            flex_basis: length(0.0_f32),
            min_size: taffy::Size {
                width: length(120.0_f32),
                height: length(h),
            },
            size: taffy::Size {
                width: percent(1.0_f32),
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
        let theme = Theme::dark();
        taffy::Size {
            width: 160.0,
            height: tile_height(self.delta.is_some(), &theme) as f32,
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
        let value_color = self.value_color(theme);
        paint_stat_tile(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            &self.caption,
            &self.value,
            value_color,
            self.icon,
            self.delta.as_ref().map(|(t, tr)| (t.as_str(), *tr)),
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Stat tiles are informational — no input handling.
    }

    fn name(&self) -> &'static str {
        "StatTile"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // Delta, if present, joins the value so a reader announces the
        // trend too.
        let value = match &self.delta {
            Some((d, _)) => format!("{} ({})", self.value, d),
            None => self.value.clone(),
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name(self.caption.clone())
                .with_value(value),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn lay_out_row(tiles: usize) -> Vec<Rect> {
        // Lay N tiles inside a Row-like flex container 600px wide.
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut widgets: Vec<StatTile> = (0..tiles)
            .map(|i| StatTile::new(format!("m{i}"), "1"))
            .collect();
        let mut nodes = Vec::new();
        for w in widgets.iter_mut() {
            nodes.push(w.build_layout(&mut taffy, &theme, &mut node_map));
        }
        let root = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                gap: taffy::Size {
                    width: length(12.0_f32),
                    height: length(0.0_f32),
                },
                size: taffy::Size {
                    width: length(600.0_f32),
                    height: length(120.0_f32),
                },
                ..Default::default()
            },
            &nodes,
        );
        taffy
            .compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(600.0),
                    height: taffy::AvailableSpace::Definite(120.0),
                },
            )
            .unwrap();
        for (w, n) in widgets.iter_mut().zip(nodes.iter()) {
            w.apply_layout(&taffy, *n, Point::ZERO);
        }
        widgets.iter().map(|w| w.get_rect()).collect()
    }

    #[test]
    fn tiles_split_a_row_evenly() {
        let rects = lay_out_row(3);
        assert_eq!(rects.len(), 3);
        let w0 = rects[0].width();
        for r in &rects {
            assert!(
                (r.width() - w0).abs() < 1.0,
                "all tiles share width, got {} vs {w0}",
                r.width()
            );
        }
        // Three tiles + 2×12px gaps must fit in 600px.
        assert!((w0 * 3.0 + 24.0 - 600.0).abs() < 2.0, "widths + gaps ≈ 600");
    }

    #[test]
    fn delta_makes_tile_taller() {
        let theme = Theme::dark();
        assert!(
            tile_height(true, &theme) > tile_height(false, &theme),
            "a delta line adds height"
        );
    }

    #[test]
    fn trend_colors_are_semantic() {
        let theme = Theme::dark();
        assert_eq!(
            Trend::Up.color(&theme),
            theme.tokens.colors.semantic.success
        );
        assert_eq!(
            Trend::Down.color(&theme),
            theme.tokens.colors.semantic.danger
        );
        assert_eq!(Trend::Flat.color(&theme), theme.text_dim);
        assert_eq!(Trend::Up.arrow(), Some(Icon::ChevronUp));
        assert_eq!(Trend::Flat.arrow(), None, "a flat trend draws no arrow");
    }

    #[test]
    fn value_color_defaults_to_primary_and_overrides() {
        let theme = Theme::dark();
        let plain = StatTile::new("CPU", "42%");
        assert_eq!(plain.value_color(&theme), theme.text_primary);
        let tinted =
            StatTile::new("CPU", "99%").with_value_color(theme.tokens.colors.semantic.danger);
        assert_eq!(
            tinted.value_color(&theme),
            theme.tokens.colors.semantic.danger
        );
    }

    #[test]
    fn draws_card_and_content() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        let mut t = StatTile::new("Requests", "1,284")
            .with_icon(Icon::BarChart)
            .with_delta("12%", Trend::Up);
        t.rect = Rect::new(0.0, 0.0, 160.0, 100.0);
        t.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|x| **x == DrawTag::COLOR)
            .count();
        assert!(
            fills > 4,
            "card + caption + value + icon + delta ⇒ many fills, got {fills}"
        );
    }

    #[test]
    fn accessibility_pairs_caption_and_value() {
        use crate::a11y::Role;
        let t = StatTile::new("CPU", "42%").with_delta("3%", Trend::Down);
        let node = Widget::accessibility(&t).expect("stat tile exposes a11y");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("CPU"));
        assert_eq!(
            node.value.as_deref(),
            Some("42% (3%)"),
            "delta joins the announced value"
        );
    }
}
