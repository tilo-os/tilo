//! A [`SearchRow::Command`](crate::search::SearchRow::Command) row: what
//! leads it (artwork, an avatar or an icon), its title over a description
//! with the query's words set off, and what trails it (an outcome button,
//! faces, a status pill, keycaps or the selected row's alternative).

use tilo_sort::CompactSort as _;
use vello::kurbo::{Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Color;

use crate::color_compat::ColorCompat;
use crate::icons;
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::search::{ActivationModifier, CommandRow, SearchResult};
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST;

/// What the view knows about a row beyond its data.
pub(super) struct RowState<'a> {
    pub selected: bool,
    /// The query whose words are set off where the row matches them.
    pub highlight: &'a str,
    pub held: Option<ActivationModifier>,
}

/// The ground a row's ink is measured on: the armed lift when selected,
/// else the panel.
fn row_ground(theme: &Theme, selected: bool) -> Color {
    if selected {
        crate::widget::floating_surface::armed_row(theme).0
    } else {
        crate::chrome::panel_ground(theme)
    }
}

/// Where the leading glyph sits.
fn icon_rect(row: Rect) -> Rect {
    let size = 16.0;
    let cy = f64::midpoint(row.y0, row.y1);
    Rect::new(
        row.x0 + 12.0,
        cy - size / 2.0,
        row.x0 + 12.0 + size,
        cy + size / 2.0,
    )
}

/// Where a person's avatar sits: the icon's place, a little larger, so its
/// initials read.
fn avatar_rect(row: Rect) -> Rect {
    let icon = icon_rect(row);
    let grow = 2.0;
    Rect::new(
        icon.x0,
        icon.y0 - grow,
        icon.x1 + 2.0 * grow,
        icon.y1 + grow,
    )
}

pub(super) fn draw(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    result: &SearchResult,
    row: &CommandRow,
    state: &RowState<'_>,
) {
    let PaintCtx {
        scene,
        transform,
        text: text_renderer,
        theme,
    } = ctx.reborrow();
    let colors = &theme.tokens.colors;
    let label_style = theme.text(TextRole::Body);
    let desc_style = theme.text(TextRole::Caption);
    let selected = state.selected;
    // Needs contrast against the armed lift: take the nearest legible step
    // from the theme's dim tone rather than promoting it to the title's
    // weight to survive.
    let subtitle_ink = colors.ink_on(
        row_ground(theme, selected),
        colors.text_dim,
        SMALL_TEXT_MIN_CONTRAST,
    );
    let label_ink = if selected {
        crate::widget::floating_surface::armed_row(theme).1
    } else {
        colors.text_primary
    };

    let mut text_x = rect.x0 + 12.0;
    let image_x = row.image.as_ref().and_then(|image| {
        let at = icon_rect(rect);
        icons::draw_app_image(scene, transform, at, image).then_some(at.x1 + 10.0)
    });
    if let Some(x) = image_x {
        text_x = x;
    } else if let Some((name, key)) = &row.avatar {
        let face = avatar_rect(rect);
        crate::widget::AvatarFace::new(name, key).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            face,
        );
        text_x = face.x1 + 10.0;
    } else if let Some(icon) = result.icon {
        let at = icon_rect(rect);
        let ink = if selected { label_ink } else { colors.text_dim };
        icons::draw_icon(scene, transform, at, icon, ink, 1.5);
        text_x = at.x1 + 10.0;
    }

    let text_right = draw_trailing(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        rect,
        row,
        state,
    );
    let avail = (text_right - text_x).max(0.0);

    let label = text_renderer.elide_to_width_style(&result.title, f32_of_f64(avail), &label_style);
    let label_marks = match_ranges(state.highlight, &label, tilo_search::FieldWeight::TITLE);
    let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
    let Some(desc) = result.subtitle.as_deref() else {
        let (_, height) = ctx.text.measure_style(&label, None, &label_style);
        let ly = rect.y0 + (rect.height() - f64::from(height)) / 2.0;
        draw_marked(
            &mut ctx,
            &label,
            &label_marks,
            Point::new(text_x, ly),
            label_ink,
            &label_style,
        );
        return;
    };
    // A path keeps both ends; prose keeps its beginning, where the meaning
    // is.
    let sub = if row.description_is_path {
        elide_middle(ctx.text, desc, &desc_style, avail)
    } else {
        ctx.text
            .elide_to_width_style(desc, f32_of_f64(avail), &desc_style)
    };
    let (_, label_height) = ctx.text.measure_style(&label, None, &label_style);
    let (_, desc_height) = ctx.text.measure_style(&sub, None, &desc_style);
    let gap = f64::from(theme.tokens.spacing.s1);
    let block = f64::from(label_height + desc_height) + gap;
    let ly = rect.y0 + (rect.height() - block) / 2.0;
    draw_marked(
        &mut ctx,
        &label,
        &label_marks,
        Point::new(text_x, ly),
        label_ink,
        &label_style,
    );
    // Only one line is set off: the title when it shows the query, else the
    // description, so a row never shouts twice.
    let marks = |text: &str| {
        if label_marks.is_empty() {
            match_ranges(state.highlight, text, tilo_search::FieldWeight::SUBTITLE)
        } else {
            Vec::new()
        }
    };
    let sub_y = ly + f64::from(label_height) + gap;
    if row.facts.len() > 1 {
        // Facts are runs of their own with space between them, the last
        // losing its end when the row runs out.
        let fact_gap = f64::from(theme.tokens.spacing.s3);
        let mut x = text_x;
        let right = text_x + avail;
        for fact in &row.facts {
            let room = right - x;
            if room <= fact_gap {
                break;
            }
            let shown = ctx
                .text
                .elide_to_width_style(fact, f32_of_f64(room), &desc_style);
            let (w, _) = ctx.text.measure_style(&shown, None, &desc_style);
            draw_marked(
                &mut ctx,
                &shown,
                &marks(&shown),
                Point::new(x, sub_y),
                subtitle_ink,
                &desc_style,
            );
            x += f64::from(w) + fact_gap;
        }
    } else {
        draw_marked(
            &mut ctx,
            &sub,
            &marks(&sub),
            Point::new(text_x, sub_y),
            subtitle_ink,
            &desc_style,
        );
    }
}

/// Paints what trails the row and returns the x its text must stay clear
/// of: an outcome button, then faces; else a status pill, a keycap chip, or
/// on the selected row its alternative activation.
fn draw_trailing(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    row: &CommandRow,
    state: &RowState<'_>,
) -> f64 {
    let PaintCtx {
        scene,
        transform,
        text: text_renderer,
        theme,
    } = ctx.reborrow();
    let colors = &theme.tokens.colors;
    let selected = state.selected;
    let cy = f64::midpoint(rect.y0, rect.y1);
    // What Enter does, when the row says: its outcome, as a button at the
    // trailing edge ("Join", "Answer"). The row is the control; the button
    // names what choosing it does.
    let mut right = rect.x1 - 8.0;
    if let Some(outcome) = row.outcome.as_deref() {
        use crate::widget::button::{Button, ButtonFace, ButtonRole};
        use crate::widget::control_size::ControlSize;
        let probe = ButtonFace::new(Rect::ZERO, outcome).control_size(ControlSize::Small);
        let width = probe.intrinsic_width(text_renderer, theme);
        let height = ButtonFace::height(ControlSize::Small, theme);
        let at = Rect::new(right - width, cy - height / 2.0, right, cy + height / 2.0);
        let face = ButtonFace::new(at, outcome)
            .role(if selected {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            })
            .control_size(ControlSize::Small);
        Button::paint(scene, transform, text_renderer, theme, &face);
        right = at.x0 - 8.0;
    }
    // Faces first: who the row is about outranks a hint.
    if !row.faces.is_empty() {
        let stack = crate::presence::FaceStack::WINDOW;
        let (shown, more) = stack.split(row.faces.len());
        let faces: Vec<crate::presence::Face<'_>> = row
            .faces
            .iter()
            .take(shown)
            .map(|face| crate::presence::Face {
                is_agent: face.is_agent,
                dimmed: face.dimmed,
                ..crate::presence::Face::new(&face.name, &face.identity, face.colour)
            })
            .collect();
        stack.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            (right, cy),
            (&faces, more),
            row_ground(theme, selected),
        );
        return right - stack.width_of(faces.len(), more) - 8.0;
    }
    if row.outcome.is_some() {
        return right;
    }
    // Reserved before measuring the text, so a long title is ellipsised
    // rather than painted under the chip.
    if let Some(badge) = &row.badge {
        let badge_style = theme.text(TextRole::Caption);
        let (text_w, text_h) = text_renderer.measure_style(badge, None, &badge_style);
        let pill_w = f64::from(text_w) + 16.0;
        let pill_h = f64::from(text_h) + 8.0;
        let pill = Rect::new(
            rect.x1 - pill_w - 8.0,
            cy - pill_h / 2.0,
            rect.x1 - 8.0,
            cy + pill_h / 2.0,
        );
        let shape = RoundedRect::from_rect(pill, pill_h / 2.0);
        let fill = colors
            .text_primary
            .with_alpha_factor(if selected { 0.28 } else { 0.16 });
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.text_primary.with_alpha_factor(0.55),
            None,
            &shape,
        );
        let ink = colors.ink_on(
            row_ground(theme, selected),
            colors.text_primary,
            SMALL_TEXT_MIN_CONTRAST,
        );
        text_renderer.draw_style(
            scene,
            badge,
            ink,
            transform.pre_translate(Vec2::new(pill.x0 + 8.0, cy - f64::from(text_h) / 2.0)),
            None,
            &badge_style,
        );
        pill.x0 - 8.0
    } else if let Some(labels) = &row.keycaps {
        let chip_w = crate::widget::InputHint::measure_keycap_labels(labels, text_renderer, theme);
        let chip_h = 26.0;
        let chip = Rect::new(
            rect.x1 - chip_w - 8.0,
            cy - chip_h / 2.0,
            rect.x1 - 8.0,
            cy + chip_h / 2.0,
        );
        crate::widget::InputHint::paint_keycap_labels_at(
            scene,
            transform,
            Point::new(chip.x0, chip.y0),
            labels,
            text_renderer,
            theme,
        );
        chip.x0 - 8.0
    } else if selected && let Some(alternate) = row.shown_alternate(state.held) {
        // The selected row says what its other activation does: while the
        // modifier is held, in full ink as a preview of where it lands;
        // otherwise dimmed, with its keys, as the one hint.
        let held = state.held == Some(alternate.modifier);
        let style = theme.text(TextRole::Caption);
        let ink = if held {
            colors.text_primary
        } else {
            colors.ink_on(
                row_ground(theme, selected),
                colors.text_dim,
                SMALL_TEXT_MIN_CONTRAST,
            )
        };
        let keycaps = crate::keycaps::active();
        let labels = vec![keycaps.key(alternate.modifier.key()), keycaps.key("Enter")];
        let caps_w = crate::widget::InputHint::measure_keycap_labels(&labels, text_renderer, theme);
        let gap = f64::from(theme.tokens.spacing.s1);
        let (label_w, label_h) = text_renderer.measure_style(&alternate.label, None, &style);
        let label_x = rect.x1 - 12.0 - f64::from(label_w);
        text_renderer.draw_style(
            scene,
            &alternate.label,
            ink,
            transform.pre_translate(Vec2::new(label_x, cy - f64::from(label_h) / 2.0)),
            None,
            &style,
        );
        let caps_x = label_x - gap - caps_w;
        let cap_h = crate::widget::InputHint::keycap_height(theme);
        crate::widget::InputHint::paint_keycap_labels_at(
            scene,
            transform,
            Point::new(caps_x, cy - cap_h / 2.0),
            &labels,
            text_renderer,
            theme,
        );
        caps_x - 8.0
    } else {
        rect.x1 - 12.0
    }
}

/// Where `text`, a row's line searched as a field of `weight`, shows the
/// query's words as search matched them, merged and in order: the part of a
/// row a person reads to see why it matched.
pub(super) fn match_ranges(
    query: &str,
    text: &str,
    weight: tilo_search::FieldWeight,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = query
        .split_whitespace()
        .filter_map(|word| tilo_search::locate(word, text, weight))
        .collect();
    ranges.compact_sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// `text` at `origin`, the parts in `ranges` in a heavier weight.
fn draw_marked(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    ranges: &[std::ops::Range<usize>],
    origin: Point,
    ink: Color,
    style: &TextStyle,
) {
    let marked = style
        .clone()
        .with_weight(crate::text_render::TextWeight::Semibold);
    let mut x = origin.x;
    let mut at = 0usize;
    let mut pieces: Vec<(&str, bool)> = Vec::new();
    for range in ranges {
        if let Some(before) = text.get(at..range.start) {
            pieces.push((before, false));
        }
        if let Some(hit) = text.get(range.start..range.end) {
            pieces.push((hit, true));
        }
        at = range.end;
    }
    if let Some(rest) = text.get(at..) {
        pieces.push((rest, false));
    }
    for (piece, strong) in pieces {
        if piece.is_empty() {
            continue;
        }
        let piece_style = if strong { &marked } else { style };
        ctx.text.draw_style(
            ctx.scene,
            piece,
            ink,
            ctx.transform.pre_translate(Vec2::new(x, origin.y)),
            None,
            piece_style,
        );
        x += f64::from(ctx.text.measure_style(piece, None, piece_style).0);
    }
}

/// Shorten `text` by eating its middle, keeping both ends readable — for
/// paths, where the head (root) and tail (own folder) both carry more
/// information than the middle. Trailing-ellipsis case is
/// [`TextRenderer::elide_to_width`].
pub(super) fn elide_middle(
    tr: &mut TextRenderer,
    text: &str,
    style: &TextStyle,
    max_w: f64,
) -> String {
    if max_w <= 0.0 || f64::from(tr.measure_style(text, None, style).0) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    for drop in 1..n {
        let keep = n.saturating_sub(drop);
        let tail = keep.saturating_mul(2) / 3;
        let head = keep.saturating_sub(tail);
        let Some(head_chars) = chars.get(..head) else {
            continue;
        };
        let Some(tail_chars) = chars.get(n.saturating_sub(tail)..) else {
            continue;
        };
        let candidate: String = head_chars.iter().collect::<String>()
            + "\u{2026}"
            + &tail_chars.iter().collect::<String>();
        if f64::from(tr.measure_style(&candidate, None, style).0) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_words_are_located_in_the_row_and_prose_is_cut_at_its_end() {
        let title = tilo_search::FieldWeight::TITLE;
        assert_eq!(
            match_ranges("dark the", "Use the dark theme", title),
            vec![4..7, 8..12],
            "each query word, merged and in order"
        );
        assert!(match_ranges("dark the", "Brightness", title).is_empty());
        assert!(
            match_ranges(
                "pi",
                "Insert it where you're typing",
                tilo_search::FieldWeight::SUBTITLE
            )
            .is_empty(),
            "two letters inside a word of a description are no match"
        );
        assert_eq!(match_ranges("pi", "Pixel Editor", title), vec![0..2]);

        let theme = Theme::dark();
        let style = theme.text(TextRole::Caption);
        let mut tr = TextRenderer::new();
        let prose = "Request one-session microphone access for the focused field, or finish the current recording.";
        let width = f64::from(tr.measure_style(prose, None, &style).0) / 2.0;
        let cut = tr.elide_to_width_style(prose, f32_of_f64(width), &style);
        assert!(
            cut.starts_with("Request one-session"),
            "prose keeps its start: {cut}"
        );
        assert!(cut.ends_with('\u{2026}'));
        let path = "~/src/tilo/crates/sdk/ui-core/src/action_palette/host.rs";
        let width = f64::from(tr.measure_style(path, None, &style).0) / 2.0;
        let cut = elide_middle(&mut tr, path, &style, width);
        assert!(
            cut.starts_with("~/") && cut.ends_with("host.rs"),
            "a path keeps both ends: {cut}"
        );
    }
}
