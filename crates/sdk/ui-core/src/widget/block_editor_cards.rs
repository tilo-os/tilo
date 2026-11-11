//! The cards a block editor draws for what its text projects: a page of its host's own, or a
//! record any app keeps, each shown as the thing's title and the start of its content, opening
//! what it names. One card for both, so a record reads as a page embed does.

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::Block;
use crate::widget::marks::{Mark, PageId};
use crate::widget::{BlockEditor, TransclusionPreview};

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

/// The most lines of what a card projects it shows under its title: a readable excerpt, then
/// the last line gives way to the card's edge.
pub(super) const CARD_BODY_LINES: usize = 3;

/// One line of a card's content: the words it is made of, each with what it names when it names
/// something. A word is never cut in two by a wrap, so a link inside the excerpt reads whole.
type CardLine = Vec<(String, Option<String>)>;

/// Whether a card this wide shows anything below its title, and how tall it stands: the card's
/// own padding, the title's line, the gap under it, and the lines of content it shows wrapped to
/// the width its content has across.
pub(super) fn card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    width: f64,
    body: CardBody<'_>,
) -> f64 {
    let tokens = &theme.tokens;
    let pad = f64::from(tokens.spacing.s4);
    let (title_style, body_style) = card_styles(theme);
    let (_, title_h) = text_renderer.measure_style("Ag", None, &title_style);
    let mut height = pad + f64::from(title_h);
    let lines = card_lines(
        text_renderer,
        &body_style,
        body,
        card_body_width(theme, width),
    );
    if !lines.is_empty() {
        let (_, line_h) = text_renderer.measure_style("Ag", None, &body_style);
        height += f64::from(tokens.spacing.s1) + f64::from(line_h) * lines.len() as f64;
    }
    height + pad
}

/// The width a card's content has across in a card `width` wide: its padding on either side, the
/// glyph its title follows and the gap after it.
pub(super) fn card_body_width(theme: &Theme, width: f64) -> f64 {
    let tokens = &theme.tokens;
    let pad = f64::from(tokens.spacing.s4);
    let gap = f64::from(tokens.spacing.s2);
    let icon_side = f64::from(tokens.metrics.icon_sm * theme.text_scale);
    (width - 2.0 * pad - icon_side - gap).max(1.0)
}

/// What a card sets its title and its content in.
fn card_styles(theme: &Theme) -> (TextStyle, TextStyle) {
    (
        theme.text(crate::tokens::TextRole::Subheading),
        theme.text(crate::tokens::TextRole::Body),
    )
}

/// What `body` says, laid out as the lines a card shows at `width`: a line per paragraph it
/// breaks, each wrapped to the width, at most [`CARD_BODY_LINES`] of them, the last one ending in
/// an ellipsis where what it projects reads on.
pub(super) fn card_lines(
    text_renderer: &mut TextRenderer,
    style: &TextStyle,
    body: CardBody<'_>,
    width: f64,
) -> Vec<CardLine> {
    let space = f64::from(text_renderer.measure_style(" ", None, style).0);
    let mut lines: Vec<CardLine> = Vec::new();
    let mut line: CardLine = Vec::new();
    let mut used = 0.0_f64;
    let mut more = false;
    // Each paragraph of what it projects starts a line of its own, and is wrapped within it.
    for paragraph in paragraphs(body) {
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0.0;
        }
        if lines.len() >= CARD_BODY_LINES {
            more = true;
            break;
        }
        for (text, reference) in paragraph {
            for word in text.split_whitespace() {
                let word_w = f64::from(text_renderer.measure_style(word, None, style).0);
                if !line.is_empty() && used + space + word_w > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                    if lines.len() >= CARD_BODY_LINES {
                        more = true;
                        break;
                    }
                }
                used = if line.is_empty() {
                    word_w
                } else {
                    used + space + word_w
                };
                line.push((word.to_owned(), reference.clone()));
            }
        }
        if more {
            break;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let kept = lines.len().min(CARD_BODY_LINES);
    more |= lines.len() > CARD_BODY_LINES;
    lines.truncate(kept);
    if more && let Some(last) = lines.last_mut() {
        last.push(("…".to_owned(), None));
    }
    lines
}

/// What `body` says, a paragraph per line it breaks: each paragraph the words of it, with what
/// the run they came from names.
fn paragraphs(body: CardBody<'_>) -> Vec<Vec<(String, Option<String>)>> {
    let mut out: Vec<Vec<(String, Option<String>)>> = vec![Vec::new()];
    for run in body {
        for (at, part) in run.text.split('\n').enumerate() {
            if at > 0 {
                out.push(Vec::new());
            }
            if let Some(paragraph) = out.last_mut()
                && !part.trim().is_empty()
            {
                paragraph.push((part.to_owned(), run.reference.clone()));
            }
        }
    }
    out.retain(|paragraph| !paragraph.is_empty());
    out
}

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
/// content under it, a few lines of it. A word of that content naming something is a link,
/// painted as one and added to `links` where it was painted, so it opens what it names rather
/// than the card.
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
    // the first line with a trailing chevron (the whole card opens it), then
    // the start of its content as a few lines under it. No "Embedded note ·
    // Open" chain.
    let pad = f64::from(tokens.spacing.s4);
    let gap = f64::from(tokens.spacing.s2);
    let icon_side = f64::from(tokens.metrics.icon_sm * paint.theme.text_scale);
    let (title_style, body_style) = card_styles(paint.theme);
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
    let link_ink = tokens.colors.ink_on(
        tokens.colors.surfaces.raised,
        tokens.colors.intent.accent_ink,
        crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST,
    );
    let right = rect.x1 - pad;
    let lines = card_lines(
        paint.text_renderer,
        &body_style,
        body,
        card_body_width(paint.theme, rect.width()),
    );
    if lines.is_empty() {
        return;
    }
    let space = f64::from(paint.text_renderer.measure_style(" ", None, &body_style).0);
    let (_, line_h) = paint.text_renderer.measure_style("Ag", None, &body_style);
    let line_h = f64::from(line_h);
    let mut y = title_y + title_h + f64::from(tokens.spacing.s1);
    for line in &lines {
        let mut at = title_x;
        for (word, reference) in line {
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
                .elide_to_width_style(word, room as f32, &body_style);
            let width = f64::from(
                paint
                    .text_renderer
                    .measure_style(&shown, None, &body_style)
                    .0,
            );
            paint.text_renderer.draw_style(
                paint.scene,
                &shown,
                if reference.is_some() {
                    link_ink
                } else {
                    ink.ink
                },
                transform.pre_translate((at, y).into()),
                None,
                &body_style,
            );
            let word_rect = Rect::new(at, y, at + width, y + line_h);
            if let Some(reference) = reference {
                links.push((word_rect, reference.clone()));
            }
            at = word_rect.x1 + space;
        }
        y += line_h;
    }
}

impl BlockEditor {
    /// Show what each page of the host's own shows of itself where the text projects it: what a
    /// card of the page says under its title.
    pub fn set_transclusion_previews(&mut self, previews: HashMap<PageId, TransclusionPreview>) {
        if self.transclusion_previews != previews {
            self.transclusion_previews = previews;
            self.dirty = true;
            self.structure_dirty = true;
            // A card shows what its page says now: it takes the room that says.
            self.stack.width = None;
        }
    }

    /// The page whose card a person activated since the last frame, when one did.
    pub fn take_transclusion_open(&mut self) -> Option<PageId> {
        self.pending_transclusion_opens.pop()
    }

    /// Measure how tall each embed's card stands at the width the blocks are stacked at: its
    /// title, and the lines of what it projects a card shows, as the draw reads them, so the
    /// stack gives the card the room it needs as what it projects says more or less.
    pub(super) fn measure_card_heights(&mut self, width: f32, text_renderer: &mut TextRenderer) {
        let theme = std::sync::Arc::clone(&self.layout_theme);
        let card_width = f64::from(width) - 2.0 * self.text_pad;
        let mut heights: Vec<(String, f32)> = Vec::new();
        for block in &self.blocks {
            let Some(card) = transclusion_target(block) else {
                continue;
            };
            let body: Vec<tilo_ui_graph::rich::Run> = match &card {
                Card::Page(id) => self.transclusion_previews.get(id).map_or_else(
                    || {
                        vec![tilo_ui_graph::rich::Run {
                            text: crate::i18n!("block-embed-missing"),
                            reference: None,
                        }]
                    },
                    |preview| {
                        vec![tilo_ui_graph::rich::Run {
                            text: preview.excerpt.clone(),
                            reference: None,
                        }]
                    },
                ),
                Card::Record(reference) => self
                    .reference_preview(reference)
                    .map_or_else(Vec::new, <[tilo_ui_graph::rich::Run]>::to_vec),
            };
            let height = card_height(text_renderer, &theme, card_width, &body);
            heights.push((block.id.clone(), crate::num::f32_of_f64(height)));
        }
        self.card_heights = heights.into_iter().collect();
        for (block, stack_box) in self.blocks.iter().zip(&mut self.stack_boxes) {
            if let Some(height) = self.card_heights.get(&block.id) {
                stack_box.min = *height;
            }
        }
    }

    /// Activate the card a click landed on: a link inside it opens what it names, one painted
    /// under it in front of the thing the card itself projects. `true` when it did.
    pub(super) fn activate_card(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let point = self.last_mouse_pos;
        // A link inside a card stands in front of the card it is painted on.
        if let Some(reference) = self
            .card_links
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, reference)| reference.clone())
        {
            self.open_reference(&reference);
            return true;
        }
        let Some(card) = self
            .transclusion_rects
            .iter()
            .flatten()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, card)| card.clone())
        else {
            return false;
        };
        match card {
            Card::Record(reference) => {
                self.open_reference(&reference);
                true
            }
            Card::Page(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_graph::rich::Run;

    fn run(text: &str) -> Run {
        Run {
            text: text.to_owned(),
            reference: None,
        }
    }

    fn body(lines: &[&str]) -> Vec<Run> {
        let mut body = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            if at > 0 {
                body.push(run("\n"));
            }
            body.push(run(line));
        }
        body
    }

    #[test]
    fn a_card_shows_a_few_lines_of_what_it_projects_and_says_where_it_reads_on() {
        let theme = Theme::light();
        let style = theme.text(crate::tokens::TextRole::Body);
        let mut renderer = TextRenderer::new();
        let long = "Oat milk, eggs, two lemons, bread and cheese for lunch, coffee beans for \
                    the week, apples for the crumble, yoghurt the plain one, something green";
        // Six lines of a note, each wider than the card: the card shows its start, a few lines
        // of it, and says that it reads on.
        let six = body(&[long, long, long, long, long, long]);
        let lines = card_lines(&mut renderer, &style, &six, 240.0);
        assert_eq!(
            lines.len(),
            CARD_BODY_LINES,
            "the lines a card has room for"
        );
        assert!(
            lines.len() > 1,
            "a card shows more than the one line of what it projects"
        );
        let first = lines
            .first()
            .expect("a line")
            .iter()
            .map(|(word, _)| word.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(first.starts_with("Oat milk, eggs"), "{first:?}");
        let last = lines.last().expect("a line");
        assert_eq!(
            last.last().map(|(word, _)| word.as_str()),
            Some("…"),
            "the card says what it shows is the start: {last:?}"
        );

        // A note of two short lines shows the two, and no ellipsis: nothing is left out.
        let two = body(&["Oat milk, eggs.", "Bread and cheese."]);
        let lines = card_lines(&mut renderer, &style, &two, 400.0);
        assert_eq!(lines.len(), 2, "a line per paragraph: {lines:?}");
        assert!(
            !lines.iter().flatten().any(|(word, _)| word == "…"),
            "nothing reads on: {lines:?}"
        );

        // A word naming something stays whole under the card, so it is one link where it is
        // painted, and the card stands taller for the lines it shows than for one.
        let theme = Theme::light();
        let with_link = vec![
            run("Oat milk from "),
            Run {
                text: "Groceries".to_owned(),
                reference: Some("tl:groceries".to_owned()),
            },
            run(" and bread from the bakery on the corner by the river, with cheese and apples"),
        ];
        let lines = card_lines(&mut renderer, &style, &with_link, 240.0);
        assert!(
            lines
                .iter()
                .flatten()
                .any(|(_, reference)| reference.as_deref() == Some("tl:groceries")),
            "the link it names is still there: {lines:?}"
        );
        let one = card_height(&mut renderer, &theme, 400.0, &two);
        let several = card_height(&mut renderer, &theme, 400.0, &six);
        assert!(
            several > one,
            "a card of six lines stands taller than one of two: {several} vs {one}"
        );
    }
}
