//! A live level meter: how loud a device is right now, as a bar that rises
//! with the sound and falls back smoothly, with the recent peak marked.
//!
//! The level comes from the model through a shared cell, read between
//! frames, so a moving meter repaints itself alone and never rebuilds the
//! page around it.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::Fill;

/// The level a model publishes for a meter: `0..=1`, and whether it is
/// being measured at all.
#[derive(Clone, Default)]
pub struct Level {
    value: Rc<Cell<f32>>,
    live: Rc<Cell<bool>>,
}

impl Level {
    pub fn set(&self, value: f32) {
        self.value.set(value.clamp(0.0, 1.0));
    }

    pub fn set_live(&self, live: bool) {
        self.live.set(live);
        if !live {
            self.value.set(0.0);
        }
    }

    pub fn is_live(&self) -> bool {
        self.live.get()
    }
}

/// The bar's height at the default text size.
const HEIGHT: f32 = 6.0;
/// How much of the way to a lower level the bar falls each frame, and how
/// fast the peak mark sinks: a meter that rises at once and settles.
const FALL: f32 = 0.18;
const PEAK_FALL: f32 = 0.01;

pub struct LevelMeter {
    level: Level,
    shown: f32,
    peak: f32,
    name: String,
    rect: Rect,
}

impl LevelMeter {
    pub fn new(level: &Level, name: impl Into<String>) -> Self {
        let now = level.value.get();
        Self {
            level: level.clone(),
            shown: now,
            peak: now,
            name: name.into(),
            rect: Rect::ZERO,
        }
    }

    /// Follow the published level one frame: up at once, down smoothly.
    fn step(&mut self) -> bool {
        let target = self.level.value.get();
        let (shown, peak) = (self.shown, self.peak);
        self.shown = if target >= self.shown {
            target
        } else {
            self.shown + (target - self.shown) * FALL
        };
        if self.shown < 0.002 {
            self.shown = 0.0;
        }
        self.peak = self.shown.max(self.peak - PEAK_FALL).max(0.0);
        (self.shown - shown).abs() > f32::EPSILON || (self.peak - peak).abs() > f32::EPSILON
    }
}

impl Widget for LevelMeter {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = (HEIGHT * theme.tokens.metrics.min_pointer_target
            / ui::tokens::Metrics::BASE.min_pointer_target)
            .round();
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(height),
            },
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.step()
    }

    fn needs_animation_frame(&self) -> bool {
        self.level.is_live() || self.shown > 0.0 || self.peak > 0.0
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius = self.rect.height() / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(self.rect, radius),
        );
        let width = self.rect.width();
        if self.shown > 0.0 {
            let fill = Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + (width * f64::from(self.shown)).max(self.rect.height()),
                self.rect.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.multiply_alpha(0.9),
                None,
                &RoundedRect::from_rect(fill, radius),
            );
        }
        if self.peak > 0.02 {
            let x = (self.rect.x0 + width * f64::from(self.peak))
                .clamp(self.rect.x0 + 1.0, self.rect.x1 - 1.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary,
                None,
                &Rect::new(x - 1.0, self.rect.y0, x + 1.0, self.rect.y1),
            );
        }
    }

    fn name(&self) -> &'static str {
        "LevelMeter"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::ProgressBar)
                .with_name(self.name.clone())
                .with_value(ui::format::percent(f64::from(self.shown))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar rises at once, falls back over frames, and the peak stays a
    /// moment above it.
    #[test]
    fn the_meter_rises_at_once_and_settles() {
        let level = Level::default();
        level.set_live(true);
        let mut meter = LevelMeter::new(&level, "Microphone");
        level.set(0.8);
        assert!(meter.step());
        assert!((meter.shown - 0.8).abs() < 1e-6);
        level.set(0.0);
        meter.step();
        assert!(meter.shown > 0.5 && meter.shown < 0.8, "{}", meter.shown);
        assert!(meter.peak > meter.shown);
        for _ in 0..200 {
            meter.step();
        }
        assert!(meter.shown.abs() < f32::EPSILON);
        level.set_live(false);
        assert!(!meter.needs_animation_frame());
    }
}
