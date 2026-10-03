use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use ui::chrome::{self, NavRowFace, NavSectionFace, SelectionState};
use ui::surface::Region;
use ui::theme::Theme;
use ui::tokens::TextRole;

use crate::i18n::t;
use crate::num;
use crate::paint_context::PaintContext;
use crate::word_processor::Heading;

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
    pointer: Option<Point>,
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

    /// Track the pointer for the rows' hover lift; returns whether the
    /// hovered row changed.
    pub fn pointer_moved(&mut self, point: Point) -> bool {
        let before = self
            .pointer
            .and_then(|p| self.heading_at(p).map(str::to_owned));
        self.pointer = Some(point);
        before.as_deref() != self.heading_at(point)
    }

    /// Where heading `id`'s row was drawn this frame.
    #[cfg(test)]
    pub fn heading_rect(&self, id: &str) -> Option<Rect> {
        self.hits
            .iter()
            .find(|(_, heading)| heading == id)
            .map(|(rect, _)| *rect)
    }

    pub fn clear_frame(&mut self) {
        self.rect = Rect::ZERO;
        self.hits.clear();
    }

    /// Paint the outline as the window's navigation column: the sidebar
    /// region, an "Outline" section header and one navigation row per
    /// heading, indented by its level, with the heading the caret is in
    /// lifted.
    pub fn draw(
        &mut self,
        context: &mut PaintContext<'_>,
        theme: &Theme,
        panel: Rect,
        entries: &[Heading],
        active_id: Option<&str>,
    ) {
        self.rect = panel;
        self.hits.clear();
        ui::surface::region(
            context.scene,
            context.transform,
            panel,
            Region::Sidebar,
            theme,
        );
        let local = theme.in_region(Region::Sidebar);
        context.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            context.transform,
            &panel,
        );
        let inset = f64::from(local.tokens.spacing.s2);
        let x0 = panel.x0 + inset;
        let x1 = panel.x1 - inset;
        let mut y = panel.y0 + inset;
        let header_h = chrome::nav_section_height(&local, true);
        let header = t("outline-header");
        chrome::paint_nav_section(
            context.scene,
            context.transform,
            context.text,
            &local,
            &NavSectionFace::new(Rect::new(x0, y, x1, y + header_h), &header),
        );
        y += header_h;

        if entries.is_empty() {
            let style = local.text(TextRole::Body);
            let pad = f64::from(local.tokens.spacing.s3);
            context.text.draw_style(
                context.scene,
                &t("outline-empty"),
                local.tokens.colors.text_dim,
                context.transform * Affine::translate((x0 + pad, y)),
                Some(num::f32_of_f64((x1 - x0 - 2.0 * pad).max(1.0))),
                &style,
            );
            context.scene.pop_layer();
            return;
        }

        let row_h = chrome::nav_row_height(&local);
        for heading in entries {
            if y > panel.y1 {
                break;
            }
            let rect = Rect::new(x0, y, x1, y + row_h);
            let active = active_id == Some(heading.id.as_str());
            let hovered = self.pointer.is_some_and(|p| rect.contains(p));
            let label = if heading.text.trim().is_empty() {
                t("outline-untitled")
            } else {
                heading.text.trim().to_string()
            };
            chrome::paint_nav_row(
                context.scene,
                context.transform,
                context.text,
                &local,
                &NavRowFace::new(rect, &label)
                    .depth(usize::try_from(heading.level.max(1) - 1).unwrap_or(0))
                    .state(SelectionState::new(active, hovered, false)),
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
            pointer: None,
        };
        assert_eq!(pane.heading_at(Point::new(10.0, 10.0)), Some("heading"));

        pane.toggle();

        assert!(!pane.is_open());
        assert!(!pane.contains(Point::new(10.0, 10.0)));
        assert_eq!(pane.heading_at(Point::new(10.0, 10.0)), None);
    }
}
