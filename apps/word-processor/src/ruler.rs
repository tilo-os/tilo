//! Horizontal document ruler with owned layout, painting, and drag state.

use ui::paint::with_alpha;
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect};
use vello::peniko::{Color, Fill};

const PX_PER_INCH: f64 = 820.0 / 8.5;
const GRAB_RADIUS: f64 = 8.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RulerColors {
    pub bar: Color,
    pub field: Color,
    pub paper: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub accent_faint: Color,
}

#[derive(Clone, Debug)]
pub(crate) struct RulerModel {
    pub band: Rect,
    pub page_x: f64,
    pub page_w: f64,
    pub margin_left: f64,
    pub margin_right: f64,
    pub indent_left: f64,
    pub indent_first: f64,
    pub tab_stops: Vec<f64>,
    pub zoom: f64,
    pub anchor_x: f64,
}

impl RulerModel {
    fn to_screen(&self, x: f64) -> f64 {
        self.anchor_x + (x - self.anchor_x) * self.zoom
    }
    fn to_content(&self, x: f64) -> f64 {
        self.anchor_x + (x - self.anchor_x) / self.zoom
    }
    fn left_text_x(&self) -> f64 {
        self.page_x + self.margin_left
    }
    fn right_text_x(&self) -> f64 {
        self.page_x + self.page_w - self.margin_right
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RulerMarker {
    MarginLeft,
    MarginRight,
    IndentLeft,
    IndentFirst,
    Tab(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RulerEffect {
    AddTab(f64),
    PreviewMarginLeft(f64),
    PreviewMarginRight(f64),
    PreviewIndentLeft(f64),
    PreviewIndentFirst(f64),
    PreviewTab { index: usize, value: f64 },
    CommitMargins,
    CommitIndentLeft,
    CommitIndentFirst,
    CommitTabs { remove: Option<usize> },
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    marker: RulerMarker,
    drop_off: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Ruler {
    rect: Rect,
    drag: Option<Drag>,
}

impl Ruler {
    pub fn rect(&self) -> Rect {
        self.rect
    }
    pub fn clear(&mut self) {
        self.rect = Rect::ZERO;
        self.drag = None;
    }
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }
    #[cfg(test)]
    pub fn drag_marker(&self) -> Option<RulerMarker> {
        self.drag.map(|d| d.marker)
    }
    #[cfg(test)]
    pub fn dropping_tab(&self) -> bool {
        self.drag.is_some_and(|d| d.drop_off)
    }

    pub fn markers(model: &RulerModel) -> Vec<(RulerMarker, f64)> {
        let left = model.left_text_x();
        let indent = left + model.indent_left;
        let mut markers = vec![
            (RulerMarker::MarginLeft, model.to_screen(left)),
            (
                RulerMarker::MarginRight,
                model.to_screen(model.right_text_x()),
            ),
            (RulerMarker::IndentLeft, model.to_screen(indent)),
            (
                RulerMarker::IndentFirst,
                model.to_screen(indent + model.indent_first),
            ),
        ];
        markers.extend(
            model
                .tab_stops
                .iter()
                .enumerate()
                .map(|(i, stop)| (RulerMarker::Tab(i), model.to_screen(left + stop))),
        );
        markers
    }

    pub fn press(&mut self, point: Point, model: &RulerModel) -> Option<RulerEffect> {
        if model.page_w <= 0.0 || !self.rect.contains(point) {
            return None;
        }
        let third = self.rect.height() / 3.0;
        let top = point.y < self.rect.y0 + third;
        let bottom = point.y > self.rect.y1 - third;
        let priority = |marker| match marker {
            RulerMarker::Tab(_) => 4,
            RulerMarker::IndentFirst if top => 5,
            RulerMarker::IndentLeft if bottom => 5,
            RulerMarker::MarginLeft | RulerMarker::MarginRight if !top && !bottom => 3,
            _ => 1,
        };
        let mut best: Option<(RulerMarker, f64, i32)> = None;
        for (marker, x) in Self::markers(model) {
            let distance = (point.x - x).abs();
            if distance > GRAB_RADIUS {
                continue;
            }
            let rank = priority(marker);
            if best.is_none_or(|(_, old_distance, old_rank)| {
                distance < old_distance - 0.5
                    || ((distance - old_distance).abs() <= 0.5 && rank > old_rank)
            }) {
                best = Some((marker, distance, rank));
            }
        }
        if let Some((marker, _, _)) = best {
            self.drag = Some(Drag {
                marker,
                drop_off: false,
            });
            return None;
        }
        let left = model.to_screen(model.left_text_x());
        let right = model.to_screen(model.right_text_x());
        (point.x >= left && point.x <= right).then(|| {
            RulerEffect::AddTab((model.to_content(point.x) - model.left_text_x()).max(0.0))
        })
    }

    pub fn drag_to(&mut self, point: Point, model: &RulerModel) -> Option<RulerEffect> {
        let drag = self.drag.as_mut()?;
        if model.page_w <= 0.0 {
            return None;
        }
        let x = model.to_content(point.x);
        let left = model.left_text_x();
        match drag.marker {
            RulerMarker::MarginLeft => Some(RulerEffect::PreviewMarginLeft(
                (x - model.page_x).clamp(4.0, model.page_w / 2.0),
            )),
            RulerMarker::MarginRight => Some(RulerEffect::PreviewMarginRight(
                (model.page_x + model.page_w - x).clamp(4.0, model.page_w / 2.0),
            )),
            RulerMarker::IndentLeft => Some(RulerEffect::PreviewIndentLeft((x - left).max(0.0))),
            RulerMarker::IndentFirst => Some(RulerEffect::PreviewIndentFirst(
                x - (left + model.indent_left),
            )),
            RulerMarker::Tab(index) => {
                drag.drop_off = point.y > self.rect.y1;
                (!drag.drop_off).then_some(RulerEffect::PreviewTab {
                    index,
                    value: (x - left).max(0.0),
                })
            }
        }
    }

    pub fn release(&mut self) -> Option<RulerEffect> {
        let drag = self.drag.take()?;
        Some(match drag.marker {
            RulerMarker::MarginLeft | RulerMarker::MarginRight => RulerEffect::CommitMargins,
            RulerMarker::IndentLeft => RulerEffect::CommitIndentLeft,
            RulerMarker::IndentFirst => RulerEffect::CommitIndentFirst,
            RulerMarker::Tab(index) => RulerEffect::CommitTabs {
                remove: drag.drop_off.then_some(index),
            },
        })
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        colors: RulerColors,
        model: &RulerModel,
    ) {
        self.rect = model.band;
        let band = self.rect;
        scene.fill(Fill::NonZero, transform, colors.bar, None, &band);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.border,
            None,
            &Rect::new(band.x0, band.y1 - 1.0, band.x1, band.y1),
        );
        let clip = |x: f64| x.clamp(band.x0, band.x1);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.field,
            None,
            &Rect::new(
                clip(model.to_screen(model.page_x)),
                band.y0 + 4.0,
                clip(model.to_screen(model.page_x + model.page_w)),
                band.y1 - 4.0,
            ),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.paper,
            None,
            &Rect::new(
                clip(model.to_screen(model.left_text_x())),
                band.y0 + 4.0,
                clip(model.to_screen(model.right_text_x())),
                band.y1 - 4.0,
            ),
        );
        self.draw_ticks(scene, transform, text, colors, model);
        text.draw_text(
            scene,
            "in",
            9.0,
            colors.text_dim,
            transform * Affine::translate((band.x0 + 4.0, f64::midpoint(band.y0, band.y1) - 6.0)),
            None,
        );
        self.draw_markers(scene, transform, colors, model);
    }

    fn draw_ticks(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        colors: RulerColors,
        model: &RulerModel,
    ) {
        let band = self.rect;
        let mid = f64::midpoint(band.y0, band.y1);
        let left = model.left_text_x();
        let right = model.right_text_x();
        let inches = ((right - left) / PX_PER_INCH).ceil() as i32;
        for inch in 0..=inches {
            let x = left + f64::from(inch) * PX_PER_INCH;
            let sx = model.to_screen(x);
            if sx < band.x0 - 2.0 || sx > band.x1 + 2.0 {
                continue;
            }
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_dim,
                None,
                &Rect::new(sx - 0.5, mid - 5.0, sx + 0.5, mid + 5.0),
            );
            if inch > 0 {
                text.draw_text(
                    scene,
                    &inch.to_string(),
                    9.0,
                    colors.text_dim,
                    transform * Affine::translate((sx + 2.0, band.y0 + 1.0)),
                    None,
                );
            }
            for quarter in 1..4 {
                let qx = x + f64::from(quarter) * PX_PER_INCH / 4.0;
                if qx > right + 1.0 {
                    break;
                }
                let qsx = model.to_screen(qx);
                let h = if quarter == 2 { 3.5 } else { 2.0 };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    with_alpha(colors.text_dim, 0xaa),
                    None,
                    &Rect::new(qsx - 0.5, mid - h, qsx + 0.5, mid + h),
                );
            }
        }
    }

    fn draw_markers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        colors: RulerColors,
        model: &RulerModel,
    ) {
        let band = self.rect;
        let mid = f64::midpoint(band.y0, band.y1);
        let active = self.drag.map(|d| d.marker);
        for (marker, sx) in Self::markers(model) {
            if sx < band.x0 - 6.0 || sx > band.x1 + 6.0 {
                continue;
            }
            let selected = active == Some(marker);
            let color = if selected { colors.accent } else { colors.text };
            match marker {
                RulerMarker::MarginLeft | RulerMarker::MarginRight => {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        if selected {
                            colors.accent
                        } else {
                            colors.accent_faint
                        },
                        None,
                        &Rect::new(sx - 1.0, band.y0 + 2.0, sx + 1.0, band.y1 - 2.0),
                    );
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &vello::kurbo::Circle::new((sx, mid), 3.0),
                    );
                }
                RulerMarker::IndentFirst => fill_triangle(
                    scene,
                    transform,
                    color,
                    (sx - 4.0, band.y0 + 2.0),
                    (sx + 4.0, band.y0 + 2.0),
                    (sx, band.y0 + 9.0),
                ),
                RulerMarker::IndentLeft => fill_triangle(
                    scene,
                    transform,
                    color,
                    (sx - 4.0, band.y1 - 2.0),
                    (sx + 4.0, band.y1 - 2.0),
                    (sx, band.y1 - 9.0),
                ),
                RulerMarker::Tab(_) => {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &Rect::new(sx - 0.5, mid, sx + 0.5, band.y1 - 3.0),
                    );
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &Rect::new(sx - 0.5, band.y1 - 3.5, sx + 4.0, band.y1 - 3.0),
                    );
                }
            }
        }
    }
}

fn fill_triangle(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
) {
    let mut path = BezPath::new();
    path.move_to(a);
    path.line_to(b);
    path.line_to(c);
    path.close_path();
    scene.fill(Fill::NonZero, transform, color, None, &path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> RulerModel {
        RulerModel {
            band: Rect::new(0.0, 0.0, 1000.0, 24.0),
            page_x: 100.0,
            page_w: 820.0,
            margin_left: 80.0,
            margin_right: 80.0,
            indent_left: 20.0,
            indent_first: 10.0,
            tab_stops: vec![150.0],
            zoom: 1.0,
            anchor_x: 0.0,
        }
    }

    /// A press prefers the marker drawn in the pointer zone, dragging a tab below the band becomes a remove
    /// commit, and empty space inside the text span adds a tab.
    #[test]
    fn ruler_presses_resolve_to_markers_tabs_or_removals() {
        // Phase: press_prefers_the_marker_drawn_in_the_pointer_zone.
        {
            let model = model();
            let mut ruler = Ruler {
                rect: model.band,
                ..Ruler::default()
            };
            let x = Ruler::markers(&model)
                .into_iter()
                .find(|(m, _)| *m == RulerMarker::IndentLeft)
                .unwrap()
                .1;
            ruler.press(Point::new(x, model.band.y1 - 1.0), &model);
            assert_eq!(ruler.drag_marker(), Some(RulerMarker::IndentLeft));
        }

        // Phase: dragging_a_tab_below_the_band_becomes_a_remove_commit.
        {
            let model = model();
            let mut ruler = Ruler {
                rect: model.band,
                ..Ruler::default()
            };
            let x = Ruler::markers(&model)
                .into_iter()
                .find(|(m, _)| *m == RulerMarker::Tab(0))
                .unwrap()
                .1;
            ruler.press(Point::new(x, model.band.center().y), &model);
            assert_eq!(
                ruler.drag_to(Point::new(x, model.band.y1 + 20.0), &model),
                None
            );
            assert!(ruler.dropping_tab());
            assert_eq!(
                ruler.release(),
                Some(RulerEffect::CommitTabs { remove: Some(0) })
            );
        }

        // Phase: empty_space_inside_the_text_span_adds_a_tab.
        {
            let model = model();
            let mut ruler = Ruler {
                rect: model.band,
                ..Ruler::default()
            };
            let point = Point::new(model.left_text_x() + 300.0, model.band.center().y);
            assert_eq!(ruler.press(point, &model), Some(RulerEffect::AddTab(300.0)));
            assert!(!ruler.is_dragging());
        }
    }
}
