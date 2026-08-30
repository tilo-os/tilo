//! Floating comments panel for a highlighted passage.
//!
//! The reader supplies annotations and the current composition id; this
//! component owns the overlay's open/anchor state, paints the panel, and keeps
//! its hit regions private so clicks cannot fall through to the page beneath.

use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::model::{AnnoKind, Annotation, HiColor};
use crate::paint;
use crate::reader_theme::ReaderPalette;

const TOP_BAR: f64 = 54.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommentAction {
    Close,
    Swallow,
    DeleteHighlight(Uuid),
    Add(Uuid),
    Edit(Uuid),
    Delete(Uuid),
}

#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: CommentAction,
}

#[derive(Default)]
pub(crate) struct CommentOverlay {
    group: Option<Uuid>,
    anchor: Option<(f64, f64, f64, f64)>,
    hits: Vec<Hit>,
}

impl CommentOverlay {
    pub(crate) fn open(&mut self, group: Uuid) {
        self.group = Some(group);
    }

    pub(crate) fn close(&mut self) {
        self.group = None;
        self.anchor = None;
        self.hits.clear();
    }

    pub(crate) fn group(&self) -> Option<Uuid> {
        self.group
    }

    pub(crate) fn is_open(&self) -> bool {
        self.group.is_some()
    }

    pub(crate) fn set_anchor(&mut self, anchor: Option<(f64, f64, f64, f64)>) {
        self.anchor = anchor;
    }

    pub(crate) fn begin_frame(&mut self) {
        self.hits.clear();
    }

    /// Return the topmost overlay action at `point`. The full-window dismiss
    /// hit is recorded first and the card/actions afterward, mirroring paint
    /// order and making the panel modal without leaking its geometry outward.
    pub(crate) fn hit_test(&self, point: Point) -> Option<CommentAction> {
        self.hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(point))
            .map(|hit| hit.action)
    }

    fn push_hit(&mut self, rect: Rect, action: CommentAction) {
        self.hits.push(Hit { rect, action });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        cursor: Point,
        annotations: &[Annotation],
        default_color: HiColor,
        composing: Option<Uuid>,
    ) {
        self.begin_frame();
        let Some(group) = self.group else { return };

        // Tap-away closes. It is deliberately behind the card and its controls.
        self.push_hit(viewport, CommentAction::Close);

        let (color, excerpt) = {
            let mut rows: Vec<&Annotation> = annotations
                .iter()
                .filter(|a| a.kind == AnnoKind::Highlight && a.group == group)
                .collect();
            rows.sort_by_key(|a| (a.block, a.start));
            let color = rows.first().map_or(default_color, |a| a.color);
            let excerpt = rows
                .iter()
                .map(|a| a.excerpt.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (color, excerpt)
        };
        let excerpt = if excerpt.is_empty() {
            "Highlighted passage".to_string()
        } else {
            format!("\u{201C}{excerpt}\u{201D}")
        };
        let mut comments: Vec<Annotation> = annotations
            .iter()
            .filter(|a| a.kind == AnnoKind::Note && a.group == group)
            .cloned()
            .collect();
        comments.sort_by_key(|a| a.created);

        let colors = &theme.tokens.colors;
        let cw = 360.0_f64.min(viewport.width() - 24.0);
        let pad = 16.0;
        let inner_w = (cw - pad * 2.0) as f32;
        let text_w = inner_w - 52.0;
        let (_, excerpt_h) = tr.measure(&excerpt, 12.0, Some(inner_w));
        let display = |a: &Annotation| {
            if composing == Some(a.id) {
                format!("{}\u{2502}", a.note)
            } else if a.note.trim().is_empty() {
                "Empty comment".to_string()
            } else {
                a.note.clone()
            }
        };
        let row_heights: Vec<f64> = comments
            .iter()
            .map(|a| f64::from(tr.measure(&display(a), 13.0, Some(text_w)).1).max(18.0) + 16.0)
            .collect();

        let header_h = 28.0;
        let gap = 8.0;
        let add_h = 34.0;
        let comments_h = row_heights.iter().sum::<f64>() + gap * comments.len().max(1) as f64;
        let ch = pad + header_h + 8.0 + f64::from(excerpt_h) + 12.0 + comments_h + add_h + pad;
        let (ax, atop, abot) = self.anchor.map_or_else(
            || {
                let c = viewport.center();
                (c.x, c.y, c.y)
            },
            |(x0, y0, x1, y1)| (f64::midpoint(x0, x1), y0, y1),
        );
        let x0 = (ax - cw / 2.0).clamp(
            viewport.x0 + 12.0,
            (viewport.x1 - 12.0 - cw).max(viewport.x0 + 12.0),
        );
        let top = viewport.y0 + TOP_BAR + 8.0;
        let bottom = viewport.y1 - 16.0;
        let y0 = if abot + 12.0 + ch <= bottom {
            abot + 12.0
        } else if atop - 12.0 - ch >= top {
            atop - 12.0 - ch
        } else {
            (bottom - ch).max(top)
        };
        let card = Rect::new(x0, y0, x0 + cw, y0 + ch);
        let shadow = Rect::new(card.x0 - 1.0, card.y0 + 4.0, card.x1 + 1.0, card.y1 + 7.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 80));
        paint::fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            colors.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            theme.border_focus,
            1.4,
        );
        self.push_hit(card, CommentAction::Swallow);

        let ix0 = x0 + pad;
        let ix1 = x0 + cw - pad;
        let mut y = y0 + pad;
        paint::circle(
            scene,
            t,
            Point::new(ix0 + 6.0, y + 8.0),
            6.0,
            ReaderPalette::highlight_swatch(color),
        );
        tr.draw_text_weighted(
            scene,
            "Highlight",
            13.5,
            colors.text_primary,
            t * Affine::translate((ix0 + 20.0, y + 1.0)),
            None,
            TextWeight::Semibold,
        );
        let trash = Rect::new(ix1 - 24.0, y - 2.0, ix1, y + 22.0);
        let hovered = trash.contains(cursor);
        if hovered {
            paint::fill_round(scene, t, trash, 6.0, theme.bg_hover);
        }
        paint::trash(
            scene,
            t,
            trash,
            if hovered {
                colors.semantic.danger
            } else {
                colors.text_dim
            },
            1.5,
        );
        self.push_hit(trash, CommentAction::DeleteHighlight(group));
        y += header_h;

        tr.draw_text(
            scene,
            &excerpt,
            12.0,
            colors.text_dim,
            t * Affine::translate((ix0, y)),
            Some(inner_w),
        );
        y += f64::from(excerpt_h) + 12.0;

        for (annotation, height) in comments.iter().zip(row_heights) {
            let row = Rect::new(ix0, y, ix1, y + height);
            paint::fill_round(scene, t, row, 8.0, colors.surfaces.overlay);
            if composing == Some(annotation.id) {
                paint::stroke_round(scene, t, row, 8.0, theme.border_focus, 1.4);
            }
            let text = display(annotation);
            tr.draw_text(
                scene,
                &text,
                13.0,
                if annotation.note.trim().is_empty() && composing != Some(annotation.id) {
                    colors.text_dim
                } else {
                    colors.text_primary
                },
                t * Affine::translate((ix0 + 10.0, y + 8.0)),
                Some(text_w),
            );
            let delete = Rect::new(ix1 - 24.0, y + 4.0, ix1 - 2.0, y + 26.0);
            let delete_hover = delete.contains(cursor);
            paint::close(
                scene,
                t,
                delete,
                if delete_hover {
                    colors.semantic.danger
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            self.push_hit(delete, CommentAction::Delete(annotation.id));
            let edit = Rect::new(ix1 - 48.0, y + 4.0, ix1 - 26.0, y + 26.0);
            paint::pencil(
                scene,
                t,
                edit,
                if edit.contains(cursor) {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.7)
                },
                1.4,
            );
            if composing != Some(annotation.id) {
                self.push_hit(edit, CommentAction::Edit(annotation.id));
            }
            y += height + gap;
        }

        let add = Rect::new(ix0, y, ix1, y + add_h - 4.0);
        let add_hover = add.contains(cursor);
        paint::fill_round(
            scene,
            t,
            add,
            8.0,
            if add_hover {
                theme.accent_primary
            } else {
                theme.accent_primary.with_alpha_factor(0.14)
            },
        );
        tr.draw_text_weighted(
            scene,
            "＋  Add comment",
            12.5,
            if add_hover {
                Color::from_rgb8(248, 248, 252)
            } else {
                theme.accent_primary
            },
            t * Affine::translate((ix0 + 12.0, y + 8.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(add, CommentAction::Add(group));
    }

    #[cfg(test)]
    pub(crate) fn action_center(&self, pred: impl Fn(CommentAction) -> bool) -> Option<Point> {
        self.hits
            .iter()
            .rev()
            .find(|h| pred(h.action))
            .map(|h| h.rect.center())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(kind: AnnoKind, group: Uuid, note: &str) -> Annotation {
        let mut a = Annotation::new("book", kind, 0, 0).with_group(group);
        a.excerpt = "a highlighted sentence".into();
        a.note = note.into();
        a
    }

    #[test]
    fn card_controls_win_over_the_full_window_dismiss_hit() {
        let group = Uuid::new_v4();
        let comment = annotation(AnnoKind::Note, group, "Useful thought");
        let annotations = [annotation(AnnoKind::Highlight, group, ""), comment.clone()];
        let mut overlay = CommentOverlay::default();
        overlay.open(group);
        overlay.set_anchor(Some((200.0, 180.0, 300.0, 200.0)));
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        overlay.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            Rect::new(0.0, 0.0, 800.0, 700.0),
            Point::ZERO,
            &annotations,
            HiColor::Yellow,
            None,
        );

        let edit = overlay
            .action_center(|a| a == CommentAction::Edit(comment.id))
            .unwrap();
        assert_eq!(
            overlay.hit_test(edit),
            Some(CommentAction::Edit(comment.id))
        );
        assert_eq!(
            overlay.hit_test(Point::new(4.0, 690.0)),
            Some(CommentAction::Close)
        );
    }

    #[test]
    fn close_clears_geometry_and_prevents_stale_hits() {
        let group = Uuid::new_v4();
        let annotations = [annotation(AnnoKind::Highlight, group, "")];
        let mut overlay = CommentOverlay::default();
        overlay.open(group);
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        overlay.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            Rect::new(0.0, 0.0, 600.0, 500.0),
            Point::ZERO,
            &annotations,
            HiColor::Yellow,
            None,
        );
        assert!(overlay.hit_test(Point::ZERO).is_some());
        overlay.begin_frame();
        assert!(
            overlay.hit_test(Point::ZERO).is_none(),
            "a frame that does not draw the overlay cannot retain modal hits"
        );
        overlay.close();
        assert!(!overlay.is_open());
        assert!(overlay.hit_test(Point::ZERO).is_none());
    }
}
