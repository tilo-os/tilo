//! The geometry of a kind of list row, apart from what it says: what a row on its way
//! reserves and what its skeleton draws.

use vello::kurbo::{Rect, RoundedRect};
use vello::peniko::Fill;

use super::{ListRow, ListRowFace, RowLeading, RowMetrics, RowValue};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::skeleton::{LAST_LINE_SHARE, TITLE_SHARE, paint_bar};
use crate::widget::{AvatarFace, MediaArt, MediaAspect};

/// What leads a kind of row.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RowLeadingShape {
    #[default]
    None,
    Mark,
    Avatar,
    Tile,
    Media(MediaAspect),
}

crate::component::choice!(RowLeadingShape, "RowLeadingShape", {
    None => "none",
    Mark => "mark",
    Avatar => "avatar",
    Tile => "tile",
});

/// The geometry of a kind of list row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowShape {
    pub leading: RowLeadingShape,
    pub title_lines: u8,
    pub subtitle: bool,
    pub secondary_lines: u8,
    pub title_value: bool,
}

impl Default for RowShape {
    fn default() -> Self {
        Self::TITLE
    }
}

impl RowShape {
    pub const TITLE: Self = Self {
        leading: RowLeadingShape::None,
        title_lines: 1,
        subtitle: false,
        secondary_lines: 0,
        title_value: false,
    };

    /// This kind with `lines` secondary lines under the title.
    #[must_use]
    pub const fn with_secondary(mut self, lines: u8) -> Self {
        self.secondary_lines = lines;
        self
    }

    /// This kind led by `leading`.
    #[must_use]
    pub const fn led_by(mut self, leading: RowLeadingShape) -> Self {
        self.leading = leading;
        self
    }

    /// This kind with a subtitle line.
    #[must_use]
    pub const fn with_subtitle(mut self) -> Self {
        self.subtitle = true;
        self
    }

    /// This kind with a two-line title.
    #[must_use]
    pub const fn two_line_title(mut self) -> Self {
        self.title_lines = 2;
        self
    }

    /// A face of this kind that says nothing: the geometry of the row.
    #[must_use]
    pub fn face(&self) -> ListRowFace<'static> {
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
            RowLeadingShape::Mark => face.leading(RowLeading::Icon(crate::icons::Icon::Circle)),
            RowLeadingShape::Avatar => face.leading(RowLeading::Avatar(AvatarFace::new("", ""))),
            RowLeadingShape::Tile => {
                face.leading(RowLeading::Avatar(AvatarFace::new("", "").square()))
            }
            RowLeadingShape::Media(aspect) => {
                face.leading(RowLeading::Media(MediaArt::Placeholder, aspect))
            }
        }
    }

    /// The height a row of this kind takes.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
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
                Some(RowLeading::Avatar(_) | RowLeading::AvatarSpace) => RowLeadingShape::Avatar,
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
    /// `caption` (where the row comes from) takes the last line's place,
    /// so naming the source moves nothing.
    pub fn paint_skeleton(&self, ctx: &mut PaintCtx<'_>, rect: Rect, caption: Option<&str>) {
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
        let last = lines.len().saturating_sub(1);
        for (i, (line, share)) in lines.into_iter().enumerate() {
            match caption {
                Some(caption) if i == last => {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "a laid-out width is far below f32::MAX"
                    )]
                    let room = width as f32;
                    let style = theme.text(TextRole::Caption);
                    let caption_line = f64::from(ctx.text.measure_style("Ag", None, &style).1);
                    ctx.text_role(
                        caption,
                        TextRole::Caption,
                        theme.tokens.colors.text_dim,
                        vello::kurbo::Point::new(geo.text_x, top + (line - caption_line) / 2.0),
                        Some(room),
                    );
                }
                _ => paint_bar(ctx, geo.text_x, top, width * share, line),
            }
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
