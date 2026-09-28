//! Intrinsically measured metadata for a video card at accessibility text sizes.
//!
//! A normal card uses the shared `Text` widgets.  At 2× those independent
//! inline boxes can be measured before their final lane width is known, while
//! the renderer correctly paints the scaled glyphs.  This leaf owns the three
//! lanes together, so its height is the measured title + channel + stat line
//! height at the *actual* card width.  The following card therefore always
//! starts after the last rendered baseline, not after a guessed spacer.

use std::collections::HashMap;

use taffy::style_helpers::percent;
use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use super::{layout_rect, leaf_node};

const TITLE_SIZE: f32 = 14.5;
const META_SIZE: f32 = 12.5;
const TITLE_TO_CHANNEL_GAP: f32 = 12.0;
const CHANNEL_TO_STATS_GAP: f32 = 8.0;
const CARD_BOTTOM_GAP: f32 = 18.0;

#[derive(Clone, Copy, Debug, Default)]
struct LaneLayout {
    channel_y: f32,
    stats_y: f32,
    height: f32,
}

/// Three independent text lanes with a single authoritative measure/draw
/// path.  It is deliberately presentation-only: the enclosing video card
/// owns the accessible button and canonical open action.
pub struct LargeVideoMeta {
    title: String,
    channel: String,
    stats: String,
    rect: Rect,
    lanes: LaneLayout,
    target_text_scale: f32,
    dirty: bool,
}

impl LargeVideoMeta {
    pub fn new(title: &str, channel: &str, stats: &str) -> Self {
        Self {
            title: title.to_string(),
            channel: channel.to_string(),
            stats: stats.to_string(),
            rect: Rect::ZERO,
            lanes: LaneLayout::default(),
            target_text_scale: 1.0,
            dirty: true,
        }
    }

    fn layout_for(&self, renderer: &mut TextRenderer, width: f32) -> LaneLayout {
        let width = width.max(1.0);
        // Taffy's measure callback can run before the host has applied the
        // renderer's text scale for the frame. Scale the styles relative to
        // the renderer's current value so the callback and eventual paint use
        // identical effective glyph metrics in either ordering.
        let metric_scale = self.target_text_scale / renderer.text_scale();
        let title_style =
            TextStyle::new(TITLE_SIZE * metric_scale).with_weight(TextWeight::Regular);
        let meta_style = TextStyle::new(META_SIZE * metric_scale);
        let (_, title_height) = renderer.measure_style(&self.title, Some(width), &title_style);
        let (_, channel_height) = renderer.measure_style(&self.channel, Some(width), &meta_style);
        let (_, stats_height) = renderer.measure_style(&self.stats, Some(width), &meta_style);
        // These are typographic gaps, not fixed box offsets: scale them by
        // the renderer that produced the measured line heights so a 2× screen
        // gets the same breathing room as 1×.
        let scale = self.target_text_scale;
        let channel_y = title_height + TITLE_TO_CHANNEL_GAP * scale;
        let stats_y = channel_y + channel_height + CHANNEL_TO_STATS_GAP * scale;
        LaneLayout {
            channel_y,
            stats_y,
            height: stats_y + stats_height + CARD_BOTTOM_GAP * scale,
        }
    }
}

impl Widget for LargeVideoMeta {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.target_text_scale = theme.text_scale;
        let node = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<AvailableSpace>,
        renderer: &mut TextRenderer,
    ) -> Size<f32> {
        let width = known.width.or(match available.width {
            AvailableSpace::Definite(width) => Some(width),
            AvailableSpace::MinContent => Some(1.0),
            AvailableSpace::MaxContent => None,
        });
        // A card is fill-width in every large-text feed. Max-content is only
        // queried by Taffy during its exploratory pass; use a generous lane so
        // that pass stays finite, then the definite pass records the real one.
        let width = width.unwrap_or(900.0).max(1.0);
        self.lanes = self.layout_for(renderer, width);
        Size {
            width,
            height: self.lanes.height,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let width = self.rect.width() as f32;
        let title_style = TextStyle::new(TITLE_SIZE).with_weight(TextWeight::Regular);
        let meta_style = TextStyle::new(META_SIZE);
        renderer.draw_style(
            scene,
            &self.title,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0, self.rect.y0)),
            Some(width),
            &title_style,
        );
        renderer.draw_style(
            scene,
            &self.channel,
            theme.text_dim,
            transform
                * Affine::translate((self.rect.x0, self.rect.y0 + f64::from(self.lanes.channel_y))),
            Some(width),
            &meta_style,
        );
        renderer.draw_style(
            scene,
            &self.stats,
            theme.text_dim,
            transform
                * Affine::translate((self.rect.x0, self.rect.y0 + f64::from(self.lanes.stats_y))),
            Some(width),
            &meta_style,
        );
    }

    fn name(&self) -> &'static str {
        "LargeVideoMeta"
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_wrapped_title_reserves_every_metadata_lane() {
        let mut renderer = TextRenderer::new();
        let short = LargeVideoMeta::new("Short title", "Creator", "8K views • today");
        let long = LargeVideoMeta::new(
            "A long title that has to wrap into several lines at this card width",
            "Creator",
            "8K views • today",
        );
        let mut short = short;
        let mut long = long;
        short.target_text_scale = 2.0;
        long.target_text_scale = 2.0;
        let short = short.layout_for(&mut renderer, 380.0);
        let long = long.layout_for(&mut renderer, 380.0);
        assert!(long.channel_y > short.channel_y);
        assert!(long.stats_y > long.channel_y);
        assert!(long.height > long.stats_y);
    }
}
