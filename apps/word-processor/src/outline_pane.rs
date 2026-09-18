use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::i18n::t;
use crate::num;
use crate::paint_context::PaintContext;
use crate::word_processor::Heading;

#[derive(Clone, Copy)]
pub(super) struct OutlinePalette {
    pub background: Color,
    pub border: Color,
    pub chip: Color,
    pub text: Color,
    pub text_dim: Color,
}

/// The document-outline surface and its frame-local interaction geometry.
///
/// Heading derivation and scrolling remain editor concerns. This component owns
/// whether the surface is visible, how entries are presented, and which heading
/// a pointer press selects.
#[derive(Default)]
pub(super) struct OutlinePane {
    open: bool,
    rect: Rect,
    hits: Vec<(Rect, String)>,
}

impl OutlinePane {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.clear_frame();
        }
    }

    pub fn contains(&self, point: Point) -> bool {
        self.open && self.rect.contains(point)
    }

    pub fn heading_at(&self, point: Point) -> Option<&str> {
        self.hits
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| id.as_str())
    }

    pub fn clear_frame(&mut self) {
        self.rect = Rect::ZERO;
        self.hits.clear();
    }

    pub fn draw(
        &mut self,
        context: &mut PaintContext<'_>,
        panel: Rect,
        entries: &[Heading],
        active_id: Option<&str>,
        palette: OutlinePalette,
    ) {
        self.rect = panel;
        self.hits.clear();
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            palette.background,
            None,
            &panel,
        );
        context.scene.fill(
            Fill::NonZero,
            context.transform,
            palette.border,
            None,
            &Rect::new(panel.x1 - 1.0, panel.y0, panel.x1, panel.y1),
        );
        context.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            context.transform,
            &panel,
        );

        let pad = 14.0;
        let x0 = panel.x0 + pad;
        let right = panel.x1 - pad;
        let inner_w = (right - x0).max(40.0);
        let mut y = panel.y0 + 14.0;

        context.text.draw_text(
            context.scene,
            &t("outline-header"),
            10.5,
            palette.text_dim,
            context.transform * Affine::translate((x0, y)),
            None,
        );
        y += 18.0;

        if entries.is_empty() {
            context.text.draw_text(
                context.scene,
                &t("outline-empty"),
                12.5,
                palette.text_dim,
                context.transform * Affine::translate((x0, y)),
                Some(num::f32_of_f64(inner_w)),
            );
            context.scene.pop_layer();
            return;
        }

        let row_h = 24.0;
        for heading in entries {
            if y > panel.y1 - 4.0 {
                break;
            }
            let indent = ((heading.level.max(1) - 1) as f64) * 14.0;
            let rect = Rect::new(x0, y, right, y + row_h);
            let active = active_id == Some(heading.id.as_str());
            if active {
                // The current heading is a lifted row: the chip face with
                // its derived edge, never an accent wash (HIG §9.3).
                ui::surface::card(context.scene, context.transform, rect, 5.0, palette.chip);
            }
            let (size, ink) = match heading.level {
                1 => (13.5, palette.text),
                2 => (12.5, palette.text),
                _ => (
                    11.5,
                    if active {
                        palette.text
                    } else {
                        palette.text_dim
                    },
                ),
            };
            let label = if heading.text.trim().is_empty() {
                t("outline-untitled")
            } else {
                heading.text.trim().to_string()
            };
            context.text.draw_text(
                context.scene,
                &label,
                size,
                ink,
                context.transform * Affine::translate((x0 + indent, y + 4.0)),
                Some(num::f32_of_f64((inner_w - indent).max(20.0))),
            );
            self.hits.push((rect, heading.id.clone()));
            y += row_h;
        }
        context.scene.pop_layer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_clears_stale_frame_hits() {
        let mut pane = OutlinePane {
            open: true,
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            hits: vec![(Rect::new(5.0, 5.0, 50.0, 25.0), "heading".into())],
        };
        assert_eq!(pane.heading_at(Point::new(10.0, 10.0)), Some("heading"));

        pane.toggle();

        assert!(!pane.is_open());
        assert!(!pane.contains(Point::new(10.0, 10.0)));
        assert_eq!(pane.heading_at(Point::new(10.0, 10.0)), None);
    }
}
