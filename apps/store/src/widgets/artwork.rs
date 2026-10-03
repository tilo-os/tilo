//! Catalogue artwork leaf with a useful monogram fallback.

use std::collections::HashMap;
use taffy::style_helpers::length;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill, ImageData};

/// A small, deliberately recognisable fallback mark. Remote metadata is
/// allowed to omit an icon (and the Store refuses unsafe image URLs), so a
/// catalogue full of anonymous initial tiles is not an acceptable fallback.
/// These marks keep the card geometry stable while giving common desktop
/// tools a useful visual identity even before artwork has downloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackMark {
    Terminal,
    Files,
    Media,
    Chess,
    Vector,
    Notes,
    Generic,
}

fn fallback_mark(label: &str) -> FallbackMark {
    match label.to_ascii_lowercase().as_str() {
        "terminal" => FallbackMark::Terminal,
        "files" | "file explorer" => FallbackMark::Files,
        "vlc" | "video player" | "music" => FallbackMark::Media,
        "chess" => FallbackMark::Chess,
        "inkscape" => FallbackMark::Vector,
        "notes" => FallbackMark::Notes,
        _ => FallbackMark::Generic,
    }
}

fn fallback_colors(mark: FallbackMark, theme: &ui::Theme) -> (Color, Color, Color) {
    // Shape carries the app identity; colour remains part of the shared theme.
    // Deriving the wash and both inks from tokens keeps these tiles coherent in
    // custom themes and preserves contrast without a name-based HC exception.
    let colors = &theme.tokens.colors;
    let tint = match mark {
        FallbackMark::Terminal => colors.semantic.success,
        FallbackMark::Files | FallbackMark::Vector => colors.semantic.info,
        FallbackMark::Media | FallbackMark::Notes => colors.semantic.warning,
        FallbackMark::Chess | FallbackMark::Generic => theme.accent_primary,
    };
    let chip = colors.tinted_chip_on(
        theme.bg_primary,
        tint,
        0.18,
        ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
    );
    let foreground = theme.on_surface(chip.fill).ink;
    (chip.fill, foreground, chip.ink)
}

pub struct Artwork {
    store: Option<ui::ImageStore>,
    url: String,
    image: Option<ImageData>,
    label: String,
    monogram: String,
    width: f32,
    height: f32,
    radius: f64,
    rect: Rect,
    dirty: bool,
}

impl Artwork {
    pub fn new(
        store: Option<ui::ImageStore>,
        url: Option<&str>,
        label: &str,
        width: f32,
        height: f32,
    ) -> Self {
        let monogram = if width >= 160.0 {
            "Preview unavailable".into()
        } else {
            label
                .chars()
                .find(|c| c.is_alphanumeric())
                .map_or_else(|| "•".into(), |c| c.to_uppercase().collect())
        };
        Self {
            store,
            url: url.unwrap_or_default().to_string(),
            image: None,
            label: label.to_string(),
            monogram,
            width,
            height,
            radius: if width <= 56.0 { 12.0 } else { 16.0 },
            rect: Rect::ZERO,
            dirty: true,
        }
    }
}

impl Widget for Artwork {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        if self.image.is_none()
            && !self.url.is_empty()
            && let Some(store) = &self.store
            && let Some(image) = store.get(&self.url)
        {
            self.image = Some(image);
            self.dirty = true;
        }
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: length(self.width),
                height: length(self.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Ok(layout) = taffy.layout(node) {
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x + layout.size.width),
                origin.y + f64::from(layout.location.y + layout.size.height),
            );
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the fallback artwork marks share one paint pass so every branch uses the same clipped geometry and high-contrast palette"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &ui::Theme,
        _presence: &PresenceCtx,
    ) {
        if let Some(image) = &self.image {
            ui::image_store::paint_image_cover(scene, transform, self.rect, image, self.radius);
            return;
        }
        let mark = fallback_mark(&self.label);
        let (background, foreground, accent) = fallback_colors(mark, theme);
        let tile = RoundedRect::from_rect(self.rect, self.radius);
        scene.fill(Fill::NonZero, transform, background, None, &tile);
        let inset = self.rect.width().min(self.rect.height()) * 0.22;
        let inner = self.rect.inset(-inset);
        match mark {
            FallbackMark::Terminal => {
                let chevron = BezPath::from_vec(vec![
                    vello::kurbo::PathEl::MoveTo(Point::new(
                        inner.x0 + inner.width() * 0.12,
                        inner.y0 + inner.height() * 0.2,
                    )),
                    vello::kurbo::PathEl::LineTo(Point::new(
                        inner.x0 + inner.width() * 0.55,
                        inner.center().y,
                    )),
                    vello::kurbo::PathEl::LineTo(Point::new(
                        inner.x0 + inner.width() * 0.12,
                        inner.y1 - inner.height() * 0.2,
                    )),
                ]);
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.7),
                    transform,
                    foreground,
                    None,
                    &chevron,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.7),
                    transform,
                    accent,
                    None,
                    &vello::kurbo::Line::new(
                        Point::new(
                            inner.x0 + inner.width() * 0.58,
                            inner.y1 - inner.height() * 0.23,
                        ),
                        Point::new(
                            inner.x1 - inner.width() * 0.08,
                            inner.y1 - inner.height() * 0.23,
                        ),
                    ),
                );
            }
            FallbackMark::Files => {
                let tab = Rect::new(
                    inner.x0,
                    inner.y0 + inner.height() * 0.12,
                    inner.x0 + inner.width() * 0.48,
                    inner.y0 + inner.height() * 0.36,
                );
                let folder = Rect::new(
                    inner.x0,
                    inner.y0 + inner.height() * 0.28,
                    inner.x1,
                    inner.y1 - inner.height() * 0.06,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &RoundedRect::from_rect(tab, 3.0),
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    foreground,
                    None,
                    &RoundedRect::from_rect(folder, 4.0),
                );
            }
            FallbackMark::Media => {
                let center = Point::new(inner.center().x, inner.y0 + inner.height() * 0.43);
                let cone = BezPath::from_vec(vec![
                    vello::kurbo::PathEl::MoveTo(Point::new(center.x, inner.y0)),
                    vello::kurbo::PathEl::LineTo(Point::new(
                        inner.x0 + inner.width() * 0.22,
                        inner.y1 - inner.height() * 0.23,
                    )),
                    vello::kurbo::PathEl::LineTo(Point::new(
                        inner.x1 - inner.width() * 0.22,
                        inner.y1 - inner.height() * 0.23,
                    )),
                    vello::kurbo::PathEl::ClosePath,
                ]);
                scene.fill(Fill::NonZero, transform, accent, None, &cone);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    foreground,
                    None,
                    &Rect::new(
                        inner.x0 + inner.width() * 0.12,
                        inner.y1 - inner.height() * 0.18,
                        inner.x1 - inner.width() * 0.12,
                        inner.y1,
                    ),
                );
            }
            FallbackMark::Chess => {
                let head = Circle::new(
                    Point::new(inner.center().x, inner.y0 + inner.height() * 0.28),
                    inner.width() * 0.18,
                );
                let body = RoundedRect::from_rect(
                    Rect::new(
                        inner.x0 + inner.width() * 0.28,
                        inner.y0 + inner.height() * 0.42,
                        inner.x1 - inner.width() * 0.28,
                        inner.y1 - inner.height() * 0.2,
                    ),
                    5.0,
                );
                let base = RoundedRect::from_rect(
                    Rect::new(
                        inner.x0 + inner.width() * 0.12,
                        inner.y1 - inner.height() * 0.18,
                        inner.x1 - inner.width() * 0.12,
                        inner.y1,
                    ),
                    3.0,
                );
                scene.fill(Fill::NonZero, transform, foreground, None, &head);
                scene.fill(Fill::NonZero, transform, foreground, None, &body);
                scene.fill(Fill::NonZero, transform, accent, None, &base);
            }
            FallbackMark::Vector => {
                let mountain = BezPath::from_vec(vec![
                    vello::kurbo::PathEl::MoveTo(Point::new(inner.x0, inner.y1)),
                    vello::kurbo::PathEl::LineTo(Point::new(
                        inner.center().x,
                        inner.y0 + inner.height() * 0.08,
                    )),
                    vello::kurbo::PathEl::LineTo(Point::new(inner.x1, inner.y1)),
                    vello::kurbo::PathEl::ClosePath,
                ]);
                scene.fill(Fill::NonZero, transform, foreground, None, &mountain);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &Circle::new(
                        Point::new(
                            inner.x1 - inner.width() * 0.18,
                            inner.y0 + inner.height() * 0.2,
                        ),
                        inner.width() * 0.1,
                    ),
                );
            }
            FallbackMark::Notes => {
                let paper = RoundedRect::from_rect(
                    Rect::new(
                        inner.x0 + inner.width() * 0.12,
                        inner.y0,
                        inner.x1 - inner.width() * 0.12,
                        inner.y1,
                    ),
                    4.0,
                );
                scene.fill(Fill::NonZero, transform, foreground, None, &paper);
                for ratio in [0.32, 0.52, 0.72] {
                    let y = inner.y0 + inner.height() * ratio;
                    scene.stroke(
                        &vello::kurbo::Stroke::new(1.5),
                        transform,
                        background,
                        None,
                        &vello::kurbo::Line::new(
                            Point::new(inner.x0 + inner.width() * 0.26, y),
                            Point::new(inner.x1 - inner.width() * 0.22, y),
                        ),
                    );
                }
            }
            FallbackMark::Generic => {
                let size = if self.width < 160.0 { 20.0 } else { 14.0 };
                let (tw, th) = text.measure(&self.monogram, size, None);
                text.draw_text(
                    scene,
                    &self.monogram,
                    size,
                    foreground,
                    transform.pre_translate(
                        (
                            self.rect.center().x - f64::from(tw) / 2.0,
                            self.rect.center().y - f64::from(th) / 2.0,
                        )
                            .into(),
                    ),
                    None,
                );
            }
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Image)
                .with_name(format!("{} artwork", self.label))
                .with_bounds(self.rect),
        )
    }
    fn name(&self) -> &'static str {
        "StoreArtwork"
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

#[cfg(test)]
mod tests {
    use super::{FallbackMark, fallback_mark};

    /// Known apps get distinct marks and unknown ones a legible generic mark.
    #[test]
    fn fallback_marks_are_distinct_and_legible() {
        // Case `known_desktop_apps_have_distinct_fallback_marks`.
        {
            assert_eq!(fallback_mark("Terminal"), FallbackMark::Terminal);
            assert_eq!(fallback_mark("Files"), FallbackMark::Files);
            assert_eq!(fallback_mark("VLC"), FallbackMark::Media);
            assert_eq!(fallback_mark("Notes"), FallbackMark::Notes);
        }

        // Case `unknown_catalogue_item_keeps_a_legible_generic_mark`.
        {
            assert_eq!(fallback_mark("Example app"), FallbackMark::Generic);
        }
    }
}
