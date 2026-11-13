//! `DocumentOffer`s — the documents an app's workspace already holds, painted as the short
//! list a person opens one from, under the heading that says where they are: what an app
//! whose windows each show a document of their own (a reader, a video player) puts in its
//! empty state, so a document is one click away rather than a file chooser away.

use crate::i18n::widget_text;
use vello::kurbo::{Point, Rect};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Interaction, ListRowFace, RowLeading};

/// One document a person can open from the list: what it is called, what it is, and the
/// target a press on it answers.
#[derive(Clone, Copy, Debug)]
pub struct DocumentOffer<'a> {
    pub target: &'a str,
    pub name: &'a str,
    pub caption: Option<&'a str>,
}

/// What [`paint`] drew: one row per offer, in the order the offers came.
#[derive(Clone, Debug, Default)]
pub struct PaintedOffers {
    rows: Vec<Rect>,
}

impl PaintedOffers {
    /// Where the offer at `index` stands, for hit-testing its press.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<Rect> {
        self.rows.get(index).copied()
    }

    /// The offer a press at `point` lands on.
    #[must_use]
    pub fn at(&self, point: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(point))
    }

    /// Every row as drawn, in the offers' order.
    #[must_use]
    pub fn rows(&self) -> Vec<Rect> {
        self.rows.clone()
    }
}

/// The height the heading and one row per offer take at `width` — the list's own column,
/// narrower than the surface it stands in.
#[must_use]
pub fn height(
    text: &mut TextRenderer,
    theme: &Theme,
    offers: &[DocumentOffer<'_>],
    width: f64,
) -> f64 {
    if offers.is_empty() || width <= 0.0 {
        return 0.0;
    }
    let (_, heading) = text.measure_style("Aa", Some(to_f32(width)), &heading_style(theme));
    let rows: f64 = offers
        .iter()
        .map(|offer| row_face(offer).height(text, theme))
        .sum();
    f64::from(heading) + gap(theme) + rows
}

/// Paint the heading and one row per offer from the top of `rect`, `hover` the pointer for
/// the rows' hover state.
#[must_use]
pub fn paint(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    offers: &[DocumentOffer<'_>],
    hover: Point,
) -> PaintedOffers {
    let mut painted = PaintedOffers::default();
    if offers.is_empty() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return painted;
    }
    let theme = ctx.theme;
    let heading = widget_text("widget-documents-in-workspace");
    let (_, heading_h) =
        ctx.text
            .measure_style(&heading, Some(to_f32(rect.width())), &heading_style(theme));
    ctx.text(
        &heading,
        &heading_style(theme),
        theme.text_primary,
        Point::new(rect.x0, rect.y0),
        Some(to_f32(rect.width())),
    );
    let mut y = rect.y0 + f64::from(heading_h) + gap(theme);
    for offer in offers {
        let h = row_face(offer).height(ctx.text, theme);
        let row = Rect::new(rect.x0, y, rect.x1, y + h);
        y += h;
        if row.y1 > rect.y1 {
            break;
        }
        let face = row_face(offer).interaction(Interaction {
            hovered: row.contains(hover),
            ..Default::default()
        });
        face.paint(ctx, row);
        painted.rows.push(row);
    }
    painted
}

fn row_face<'a>(offer: &'a DocumentOffer<'a>) -> ListRowFace<'a> {
    let face = ListRowFace::new(offer.name).leading(RowLeading::Icon(Icon::FileText));
    match offer.caption {
        Some(caption) => face.secondary(caption),
        None => face,
    }
}

fn heading_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Label)
}

fn gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.tight)
}

fn to_f32(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "list widths are logical pixels well within f32 range"
    )]
    {
        value as f32
    }
}
