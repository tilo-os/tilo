//! Layout and paint helpers shared by the app's panes and its history
//! preview.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::highlight;
use crate::model::Snippet;

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

/// Paint a snippet's body, syntax-highlighted with line numbers, in a code
/// well at `card`: the read-only view History shows of a past version.
pub fn paint_code(
    scene: &mut Scene,
    t: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    card: Rect,
    snippet: &Snippet,
) {
    let spacing = &theme.tokens.spacing;
    let ground = theme.tokens.colors.surfaces.raised;
    ui::surface::card(
        scene,
        t,
        card,
        f64::from(theme.tokens.radii.control),
        ground,
    );
    let ink = theme.on_ground(ground);
    let code = theme.text(TextRole::Code);
    let line_h = f64::from(code.line_height_px());
    let pad = f64::from(spacing.s4);
    let lines = snippet.body.lines().count().max(1);
    let number_w = f64::from(
        text.measure_style(&lines.to_string(), None, &code.clone().numeric())
            .0,
    );
    let code_x = card.x0 + pad + number_w + f64::from(spacing.s4);
    let mut y = card.y0 + pad;
    let mut ctx = PaintCtx::new(scene, t, text, &ink);
    for (i, spans) in highlight::highlight_lines(&snippet.body, snippet.language).enumerate() {
        if y + line_h > card.y1 - pad {
            break;
        }
        let number = (i + 1).to_string();
        let width = f64::from(
            ctx.text
                .measure_style(&number, None, &code.clone().numeric())
                .0,
        );
        ctx.text(
            &number,
            &code.clone().numeric(),
            ink.text_tertiary,
            Point::new(card.x0 + pad + number_w - width, y),
            None,
        );
        let mut x = code_x;
        for span in spans {
            let style = if span.kind == ui::widget::code_editor::TokenKind::Keyword {
                code.clone().strong()
            } else {
                code.clone()
            };
            ctx.text(
                &span.text,
                &style,
                theme.tokens.syntax.color(span.kind),
                Point::new(x, y),
                None,
            );
            x += f64::from(ctx.text.measure_style(&span.text, None, &style).0);
        }
        y += line_h;
    }
}

/// The height of a code well that shows all of `snippet`'s lines.
pub fn code_height(theme: &Theme, snippet: &Snippet) -> f64 {
    let line_h = f64::from(theme.text(TextRole::Code).line_height_px());
    let lines = snippet.body.lines().count().max(1);
    line_h * lines as f64 + f64::from(theme.tokens.spacing.s4) * 2.0
}
