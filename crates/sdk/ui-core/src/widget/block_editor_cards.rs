//! The cards a block editor draws for what its text projects: a page of its host's own, or a
//! record any app keeps, each shown as the thing's title and the start of its content, opening
//! what it names. One card for both, so a record reads as a page embed does.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Block;
use crate::widget::marks::{Mark, PageId};

/// What a block projecting something else shows as a card of: a page of the host's own, by its
/// id, or a record any app keeps, by the catalogue's reference to it
/// ([`tilo_reference::embed`]), the same address a `mention` run carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Card {
    Page(PageId),
    Record(String),
}

impl Card {
    /// What activating the card asks the accessibility tree for.
    pub(super) fn action_target(&self) -> String {
        match self {
            Self::Page(id) => format!("notes.transclusion:{id}"),
            Self::Record(reference) => format!("notes.reference:{reference}"),
        }
    }
}

/// What the whole of `block` projects, when it projects something: a paragraph that is one
/// live projection (an embed) and nothing else.
pub(super) fn transclusion_target(block: &Block) -> Option<Card> {
    let text_len = block.text().len();
    block.line.marks().iter().find_map(|span| match &span.mark {
        Mark::Transclusion(id)
            if !id.is_nil() && span.range.start == 0 && span.range.end == text_len =>
        {
            Some(Card::Page(*id))
        }
        Mark::Embed(reference)
            if !reference.is_empty() && span.range.start == 0 && span.range.end == text_len =>
        {
            Some(Card::Record(reference.clone()))
        }
        _ => None,
    })
}

/// What a card shows below its title: the content of what it projects, a line of runs, the
/// ones naming something drawn as a link.
pub(super) type CardBody<'a> = &'a [tilo_ui_graph::rich::Run];

/// What a card is painted with: the scene, where in it, and the renderer the theme sets its
/// text with.
pub(super) struct CardPaint<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text_renderer: &'a mut TextRenderer,
    pub theme: &'a Theme,
}

/// Paint the card a block projecting something else shows as: the thing's glyph and its title
/// on the first line with a trailing chevron (the whole card opens it), and the start of its
/// content under it. A run of that content naming something is a link, painted as one and
/// added to `links` where it was painted, so it opens what it names rather than the card.
pub(super) fn draw_embed_card(
    paint: &mut CardPaint<'_>,
    rect: Rect,
    title: &str,
    body: CardBody<'_>,
    links: &mut Vec<(Rect, String)>,
) {
    let transform = paint.transform;
    let tokens = &paint.theme.tokens;
    let card = vello::kurbo::RoundedRect::from_rect(
        rect.inflate(-1.0, -1.0),
        f64::from(tokens.radii.control),
    );
    paint.scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        tokens.colors.surfaces.raised,
        None,
        &card,
    );
    paint.scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        crate::surface::edge_on(tokens.colors.surfaces.raised),
        None,
        &card,
    );
    // An embedded note is an object you open: a note glyph and its title on
    // the first line with a trailing chevron (the whole card opens it), the
    // start of its content as the secondary line. No "Embedded note · Open" chain.
    let pad = f64::from(tokens.spacing.s4);
    let gap = f64::from(tokens.spacing.s2);
    let icon_side = f64::from(tokens.metrics.icon_sm * paint.theme.text_scale);
    let title_style = paint.theme.text(crate::tokens::TextRole::Subheading);
    let body_style = paint.theme.text(crate::tokens::TextRole::Body);
    let ink = tokens.colors.on_surface(tokens.colors.surfaces.raised);
    let (_, title_h) = paint.text_renderer.measure_style("Ag", None, &title_style);
    let title_h = f64::from(title_h);
    let title_y = rect.y0 + pad;
    let chevron_x = rect.x1 - pad - icon_side;
    let icon_rect = Rect::new(
        rect.x0 + pad,
        title_y + (title_h - icon_side) / 2.0,
        rect.x0 + pad + icon_side,
        title_y + (title_h + icon_side) / 2.0,
    );
    crate::icons::draw_icon(
        paint.scene,
        transform,
        icon_rect,
        crate::icons::Icon::FileText,
        ink.ink_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    crate::icons::draw_icon(
        paint.scene,
        transform,
        Rect::new(chevron_x, icon_rect.y0, chevron_x + icon_side, icon_rect.y1),
        crate::icons::Icon::ChevronRight,
        ink.ink_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    let title_x = icon_rect.x1 + gap;
    let title_w = (chevron_x - gap - title_x).max(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a card's width on screen, far inside f32"
    )]
    let title = paint
        .text_renderer
        .elide_to_width_style(title, title_w as f32, &title_style);
    paint.text_renderer.draw_style(
        paint.scene,
        &title,
        ink.ink,
        transform.pre_translate((title_x, title_y).into()),
        None,
        &title_style,
    );
    let body_y = title_y + title_h + f64::from(tokens.spacing.s1);
    let link_ink = tokens.colors.ink_on(
        tokens.colors.surfaces.raised,
        tokens.colors.intent.accent_ink,
        crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST,
    );
    draw_card_body(
        paint,
        (title_x, body_y, rect.x1 - pad),
        body,
        body_style,
        (ink.ink, link_ink),
        links,
    );
}

/// Paint one line of a card's content from `x` to `right` at `y`: each run in its ink, a run
/// naming something in the link's, and added to `links` where it was painted. A run that does
/// not fit gives way to the card's edge: what a card shows of what it projects is its start.
fn draw_card_body(
    paint: &mut CardPaint<'_>,
    (x, y, right): (f64, f64, f64),
    body: CardBody<'_>,
    style: crate::text_render::TextStyle,
    inks: (vello::peniko::Color, vello::peniko::Color),
    links: &mut Vec<(Rect, String)>,
) {
    let (plain, link) = inks;
    let (_, line_h) = paint.text_renderer.measure_style("Ag", None, &style);
    let line_h = f64::from(line_h);
    let mut at = x;
    for run in body {
        let room = right - at;
        if room <= 0.0 {
            break;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a card's width on screen, far inside f32"
        )]
        let shown = paint
            .text_renderer
            .elide_to_width_style(&run.text, room as f32, &style);
        let cut = shown != run.text;
        let width = f64::from(paint.text_renderer.measure_style(&shown, None, &style).0);
        paint.text_renderer.draw_style(
            paint.scene,
            &shown,
            if run.reference.is_some() { link } else { plain },
            paint.transform.pre_translate((at, y).into()),
            None,
            &style,
        );
        let rect = Rect::new(at, y, at + width, y + line_h);
        if let Some(reference) = &run.reference {
            links.push((rect, reference.clone()));
        }
        at = rect.x1;
        if cut {
            break;
        }
    }
}
