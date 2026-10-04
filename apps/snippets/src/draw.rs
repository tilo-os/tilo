//! Layout and paint helpers shared by the app's panes, the fill-in dialog's
//! preview and the history preview.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::highlight::{self, Span};
use crate::model::{Language, Snippet};

/// A leaf node that fills what its parent gives it, registered for `widget`.
#[allow(
    clippy::expect_used,
    reason = "TaffyTree::new_leaf only fails on allocation, which aborts anyway"
)]
pub(crate) fn fill_leaf<W: Widget + 'static>(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    widget: &mut W,
) -> NodeId {
    let node = taffy
        .new_leaf(Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        })
        .expect("a leaf node");
    node_map.insert(node, std::ptr::from_mut::<dyn Widget>(widget));
    node
}

/// The rect taffy placed `node` at, in window coordinates.
pub(crate) fn leaf_rect(taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Rect {
    let layout = taffy.layout(node).copied().unwrap_or_default();
    let x = origin.x + f64::from(layout.location.x);
    let y = origin.y + f64::from(layout.location.y);
    Rect::new(
        x,
        y,
        x + f64::from(layout.size.width),
        y + f64::from(layout.size.height),
    )
}

/// One painted row of wrapped code: the source line it starts (counted from
/// 1; `None` when it continues the line above) and its highlighted runs.
pub(crate) struct CodeRow {
    pub(crate) number: Option<usize>,
    pub(crate) runs: Vec<Span>,
}

fn width_of(text: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(text.measure_style(s, None, style).0)
}

/// The longest leading part of `s` that fits in `room`, ending after a
/// space or `/` when one fits. Without `force` (the row already holds
/// code) a part that would split a token is empty: the token moves to the
/// next row; with `force` the part is never empty.
fn fitting_prefix<'a>(
    text: &mut TextRenderer,
    s: &'a str,
    style: &TextStyle,
    room: f64,
    force: bool,
) -> &'a str {
    let bounds: Vec<usize> = s.char_indices().map(|(i, c)| i + c.len_utf8()).collect();
    // Binary search the last char boundary whose prefix fits.
    let (mut lo, mut hi) = (0usize, bounds.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let end = bounds.get(mid - 1).copied().unwrap_or(0);
        if width_of(text, s.get(..end).unwrap_or(""), style) <= room {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let end = if lo == 0 {
        if force {
            bounds.first().copied().unwrap_or(0)
        } else {
            0
        }
    } else {
        bounds.get(lo - 1).copied().unwrap_or(0)
    };
    let fits = s.get(..end).unwrap_or("");
    if end >= s.len() {
        return fits;
    }
    // Break after a space or a path separator when one fits, so words and
    // path segments stay whole; a token with neither breaks where it must.
    match fits.rfind([' ', '/']) {
        Some(at) if at > 0 => s.get(..=at).unwrap_or(fits),
        _ if !force => "",
        _ => fits,
    }
}

/// Highlight `body` and wrap it to `width` in `style`: a line that does not
/// fit continues on the next row, so nothing is cut off.
pub(crate) fn wrap_code(
    text: &mut TextRenderer,
    style: &TextStyle,
    body: &str,
    language: Language,
    width: f64,
) -> Vec<CodeRow> {
    let width = width.max(1.0);
    let mut rows = Vec::new();
    for (index, spans) in highlight::highlight_lines(body, language).enumerate() {
        let mut row = CodeRow {
            number: Some(index + 1),
            runs: Vec::new(),
        };
        let mut x = 0.0;
        for span in spans {
            let mut rest = span.text.as_str();
            while !rest.is_empty() {
                let whole = width_of(text, rest, style);
                if x + whole <= width {
                    row.runs.push(Span {
                        text: rest.to_string(),
                        kind: span.kind,
                    });
                    x += whole;
                    break;
                }
                let part = fitting_prefix(text, rest, style, width - x, x == 0.0);
                if !part.is_empty() {
                    row.runs.push(Span {
                        text: part.to_string(),
                        kind: span.kind,
                    });
                }
                rows.push(std::mem::replace(
                    &mut row,
                    CodeRow {
                        number: None,
                        runs: Vec::new(),
                    },
                ));
                x = 0.0;
                rest = rest.get(part.len()..).unwrap_or("");
            }
        }
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(CodeRow {
            number: Some(1),
            runs: Vec::new(),
        });
    }
    rows
}

/// Paint `rows` from `origin`, one code line box apart, each run in its
/// syntax colour. Line numbers, when `numbers` gives their column width,
/// sit right-aligned before the code in tertiary ink.
pub(crate) fn paint_code_rows(
    ctx: &mut PaintCtx<'_>,
    rows: &[CodeRow],
    origin: Point,
    numbers: Option<f64>,
) {
    let theme = ctx.theme;
    let code = theme.text(TextRole::Code);
    let numeric = code.clone().numeric();
    let line = f64::from(code.line_height_px());
    let gap = f64::from(theme.tokens.spacing.s4);
    let code_x = numbers.map_or(origin.x, |w| origin.x + w + gap);
    let mut y = origin.y;
    for row in rows {
        if let (Some(column), Some(number)) = (numbers, row.number) {
            let label = number.to_string();
            let width = width_of(ctx.text, &label, &numeric);
            ctx.text(
                &label,
                &numeric,
                theme.text_tertiary,
                Point::new(origin.x + column - width, y),
                None,
            );
        }
        let mut x = code_x;
        for run in &row.runs {
            ctx.text(
                &run.text,
                &code,
                theme.tokens.syntax.color(run.kind),
                Point::new(x, y),
                None,
            );
            x += width_of(ctx.text, &run.text, &code);
        }
        y += line;
    }
}

/// The width of the line-number column for `body`.
fn number_column(text: &mut TextRenderer, theme: &Theme, body: &str) -> f64 {
    let lines = body.lines().count().max(1);
    width_of(
        text,
        &lines.to_string(),
        &theme.text(TextRole::Code).numeric(),
    )
}

/// The code a well at `width` shows for `snippet`, wrapped to fit.
fn snippet_rows(
    text: &mut TextRenderer,
    theme: &Theme,
    width: f64,
    snippet: &Snippet,
) -> (Vec<CodeRow>, f64) {
    let pad = f64::from(theme.tokens.spacing.s4);
    let numbers = number_column(text, theme, &snippet.body);
    let code_w = width - pad * 2.0 - numbers - f64::from(theme.tokens.spacing.s4);
    let rows = wrap_code(
        text,
        &theme.text(TextRole::Code),
        &snippet.body,
        snippet.language,
        code_w,
    );
    (rows, numbers)
}

/// Paint a snippet's body, syntax-highlighted with line numbers, in a code
/// well at `card`: the read-only view History shows of a past version. Long
/// lines wrap inside the well; [`code_height`] sizes the well to hold them.
pub fn paint_code(
    scene: &mut Scene,
    t: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    snippet: &Snippet,
) {
    let ground = theme.tokens.colors.surfaces.raised;
    ui::surface::card(
        scene,
        t,
        card,
        f64::from(theme.tokens.radii.control),
        ground,
    );
    let ink = theme.on_ground(ground);
    let pad = f64::from(theme.tokens.spacing.s4);
    let (rows, numbers) = snippet_rows(text, theme, card.width(), snippet);
    let mut ctx = PaintCtx::new(scene, t, text, &ink);
    paint_code_rows(
        &mut ctx,
        &rows,
        Point::new(card.x0 + pad, card.y0 + pad),
        Some(numbers),
    );
}

/// The height of a code well `width` wide that shows all of `snippet`,
/// long lines wrapped.
pub fn code_height(text: &mut TextRenderer, theme: &Theme, width: f64, snippet: &Snippet) -> f64 {
    let line_h = f64::from(theme.text(TextRole::Code).line_height_px());
    let (rows, _) = snippet_rows(text, theme, width, snippet);
    line_h * rows.len() as f64 + f64::from(theme.tokens.spacing.s4) * 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapped code keeps every character, in order, and every row fits.
    #[test]
    fn wrapped_code_fits_and_keeps_every_character() {
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let style = theme.text(TextRole::Code);
        let body =
            "COPY --from=builder /app/target/release/${binary} /usr/local/bin/${binary}\nRUN x";
        let width = 220.0;
        let rows = wrap_code(&mut text, &style, body, Language::Dockerfile, width);
        assert!(rows.len() > 2, "the long line wraps");
        assert_eq!(
            rows.iter().filter(|row| row.number.is_some()).count(),
            2,
            "a continuation row carries no line number"
        );
        for row in &rows {
            let line: String = row.runs.iter().map(|run| run.text.as_str()).collect();
            assert!(width_of(&mut text, &line, &style) <= width, "{line:?} fits");
        }
        let joined: String = rows
            .iter()
            .flat_map(|row| row.runs.iter().map(|run| run.text.as_str()))
            .collect();
        assert_eq!(joined, body.replace('\n', ""));
    }
}
