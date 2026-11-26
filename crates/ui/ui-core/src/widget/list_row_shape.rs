//! What a row of a kind shows and how its skeleton draws it, from the geometry [`RowShape`]
//! holds: what a row on its way reserves is that geometry's, not what the row says.

use vello::kurbo::{Rect, RoundedRect};
use vello::peniko::Fill;

use super::{ListRow, ListRowFace, RowLeading, RowMetrics, RowValue};
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use crate::widget::skeleton::{LAST_LINE_SHARE, TITLE_SHARE, paint_bar};
use crate::widget::MediaArt;
use tilo_ui_base::widget::AvatarFace;
use tilo_ui_base::{widget::RowLeadingShape, widget::RowShape};

/// What a row of a kind shows, as a list row's face.
pub trait RowShapeFace {
    /// A face of this kind that says nothing: the geometry of the row.
    fn face(&self) -> ListRowFace<'static>;

    /// The height a row of this kind takes.
    fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64;
}

impl RowShapeFace for RowShape {
    fn face(&self) -> ListRowFace<'static> {
        let mut face = ListRowFace::new(" ");
        if self.title_lines > 1 {
            face = face.two_line_title();
        }
        if self.subtitle {
            face = face.subtitle(" ");
        }
        if self.secondary_lines > 0 {
            face = face.secondary(" ").secondary_lines(self.secondary_lines);
        }
        if self.title_value {
            face = face.title_value(" ");
        }
        match self.leading {
            RowLeadingShape::None => face,
            RowLeadingShape::Mark => face.leading(RowLeading::Icon(tilo_ui_base::icons::Icon::Circle)),
            RowLeadingShape::Avatar => face.leading(RowLeading::Avatar(AvatarFace::new("", ""))),
            RowLeadingShape::Tile => {
                face.leading(RowLeading::Avatar(AvatarFace::new("", "").square()))
            }
            RowLeadingShape::Media(aspect) => {
                face.leading(RowLeading::Media(MediaArt::Placeholder, aspect))
            }
        }
    }

    fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        self.face().height(text, theme)
    }
}

impl ListRowFace<'_> {
    /// The kind of row this is: what a row of it on its way reserves.
    #[must_use]
    pub fn shape(&self) -> RowShape {
        RowShape {
            leading: match self.leading {
                None => RowLeadingShape::None,
                Some(
                    RowLeading::Icon(_)
                    | RowLeading::Face(_)
                    | RowLeading::Badge(_)
                    | RowLeading::Checkbox { .. }
                    | RowLeading::Swatch(_)
                    | RowLeading::Signal(_),
                ) => RowLeadingShape::Mark,
                Some(RowLeading::Avatar(face)) if face.is_square() => RowLeadingShape::Tile,
                Some(RowLeading::Avatar(_)) => RowLeadingShape::Avatar,
                Some(RowLeading::Thumbnail(_) | RowLeading::Paint(_)) => RowLeadingShape::Tile,
                Some(RowLeading::Media(_, aspect)) => RowLeadingShape::Media(aspect),
            },
            title_lines: self.title_lines,
            subtitle: self.subtitle.is_some(),
            secondary_lines: if self.secondary.is_some() {
                self.secondary_lines
            } else {
                0
            },
            title_value: matches!(self.value, Some((_, RowValue::Title))),
        }
    }

    /// Paint this row's skeleton in `rect`: its leading mark and a bar
    /// where each of its lines stands, as [`Self::paint`] places them.
    pub fn paint_skeleton(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = RowMetrics::new(theme);
        let tone = crate::widget::skeleton_tone(theme.tokens.colors.surfaces.content);
        let geo = self.geometry(rect, &m, ctx.text, theme);
        let radius = f64::from(theme.tokens.radii.detail);
        if self.leading.is_some() {
            let slot = geo.leading;
            let (mark, round) = match self.shape().leading {
                RowLeadingShape::Avatar => {
                    let side = slot.height();
                    (
                        Rect::from_center_size(slot.center(), (side, side)),
                        side / 2.0,
                    )
                }
                RowLeadingShape::Tile => {
                    let side = slot.height();
                    (Rect::from_center_size(slot.center(), (side, side)), radius)
                }
                RowLeadingShape::Media(aspect) => {
                    let h = slot.height();
                    let w = (h * aspect.ratio()).min(slot.width());
                    (Rect::from_center_size(slot.center(), (w, h)), radius)
                }
                RowLeadingShape::Mark | RowLeadingShape::None => (
                    Rect::from_center_size(slot.center(), (m.icon, m.icon)),
                    radius,
                ),
            };
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                tone,
                None,
                &RoundedRect::from_rect(mark, round),
            );
        }
        let title = RowMetrics::line(ctx.text, &self.title_style(&m));
        let mut lines: Vec<(f64, f64)> = Vec::new();
        for _ in 0..self.title_lines {
            lines.push((title, TITLE_SHARE));
        }
        if self.subtitle.is_some() {
            let line = RowMetrics::line(ctx.text, &m.subtitle);
            lines.push((line, TITLE_SHARE));
        }
        if self.secondary.is_some() {
            let line = RowMetrics::line(ctx.text, &m.secondary);
            for _ in 0..self.secondary_lines {
                lines.push((line, LAST_LINE_SHARE));
            }
        }
        let block: f64 = lines.iter().map(|(line, _)| line).sum();
        let mut top = rect.center().y - block / 2.0;
        let width = (geo.text_right - geo.text_x).max(0.0);
        for (line, share) in lines {
            paint_bar(ctx, geo.text_x, top, width * share, line);
            top += line;
        }
    }
}

impl ListRow {
    /// The kind of row this is: what a row of it on its way reserves.
    #[must_use]
    pub fn shape(&self) -> RowShape {
        self.with_face(|face| face.shape())
    }
}
