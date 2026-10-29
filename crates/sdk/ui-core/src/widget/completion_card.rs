//! The card of a completion drawer's selected row, beside the drawer on a wide window: what
//! the candidate is before it is picked (a person's avatar, name and handle; a channel's topic;
//! a command's description and parameters).

use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

use crate::icons::{LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::num::f32_of_f64;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::completion::Candidate;

/// Paint `candidate`'s card beside `drawer`, inside `viewport`: on the trailing side where it
/// fits, otherwise the leading side; nothing where neither has room.
pub(crate) fn paint(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    candidate: &Candidate,
    drawer: Rect,
    viewport: Rect,
) {
    let tokens = &theme.tokens;
    let width = f64::from(tokens.metrics.panel_width_utility);
    let gap = f64::from(tokens.spacing.s2);
    let pad = f64::from(tokens.spacing.s4);
    let x0 = if drawer.x1 + gap + width <= viewport.x1 {
        drawer.x1 + gap
    } else if drawer.x0 - gap - width >= viewport.x0 {
        drawer.x0 - gap - width
    } else {
        return;
    };
    let inner = width - 2.0 * pad;
    let title_style = theme.text(TextRole::Subheading);
    let body_style = theme.text(TextRole::Body);
    let caption_style = theme.text(TextRole::Caption);
    let mark = f64::from(tokens.metrics.icon_lg) * 1.5;
    let mut lines: Vec<(String, &crate::text_render::TextStyle, bool)> = Vec::new();
    if let Some(handle) = &candidate.handle {
        lines.push((handle.clone(), &caption_style, true));
    }
    if let Some(aside) = &candidate.aside {
        lines.push((aside.clone(), &body_style, false));
    }
    if let Some(command) = &candidate.command {
        for param in &command.params {
            lines.push((param.label.clone(), &caption_style, true));
        }
    }
    if let Some(label) = &candidate.label {
        lines.push((label.clone(), &caption_style, true));
    }
    let title_h = f64::from(
        text.measure_style(&candidate.name, Some(f32_of_f64(inner)), &title_style)
            .1,
    );
    let mut height = pad + mark.max(title_h) + pad;
    let measured: Vec<f64> = lines
        .iter()
        .map(|(line, style, _)| {
            f64::from(text.measure_style(line, Some(f32_of_f64(inner)), style).1)
        })
        .collect();
    for h in &measured {
        height += h + f64::from(tokens.spacing.s1);
    }
    let y0 = drawer.y0.min(viewport.y1 - height).max(viewport.y0);
    let card = Rect::new(x0, y0, x0 + width, y0 + height);
    let radius = f64::from(tokens.radii.panel);
    crate::surface::shadow(scene, transform, card, radius, &tokens.elevation.floating);
    crate::surface::panel(scene, transform, card, radius, theme);
    let ink = theme.text_primary;
    let dim = tokens.colors.ink_on_any(
        &[crate::chrome::panel_ground(theme)],
        theme.text_dim,
        crate::tokens::colors::ON_FILL_MIN_CONTRAST,
    );
    let mut x = card.x0 + pad;
    let mut y = card.y0 + pad;
    let head = mark.max(title_h);
    let mark_rect = Rect::new(
        x,
        y + (head - mark) / 2.0,
        x + mark,
        y + (head + mark) / 2.0,
    );
    if let Some(person) = &candidate.person {
        let colour = tokens.colors.presence.of_rgb(person.color);
        crate::widget::AvatarFace::new(&candidate.name, &person.identity)
            .presence(colour)
            .paint(&mut PaintCtx::new(scene, transform, text, theme), mark_rect);
        x += mark + f64::from(tokens.spacing.s3);
    } else if let Some(icon) = candidate.icon {
        let side = f64::from(tokens.metrics.icon_md);
        let c = mark_rect.center();
        draw_icon(
            scene,
            transform,
            Rect::new(
                c.x - side / 2.0,
                c.y - side / 2.0,
                c.x + side / 2.0,
                c.y + side / 2.0,
            ),
            icon,
            dim,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        x += mark + f64::from(tokens.spacing.s3);
    }
    let room = (card.x1 - pad - x).max(0.0);
    text.draw_style(
        scene,
        &candidate.name,
        ink,
        transform.pre_translate(Vec2::new(x, y + (head - title_h) / 2.0)),
        Some(f32_of_f64(room)),
        &title_style,
    );
    y += head + f64::from(tokens.spacing.s2);
    for ((line, style, quiet), h) in lines.iter().zip(&measured) {
        text.draw_style(
            scene,
            line,
            if *quiet { dim } else { ink },
            transform.pre_translate(Vec2::new(card.x0 + pad, y)),
            Some(f32_of_f64(inner)),
            style,
        );
        y += h + f64::from(tokens.spacing.s1);
    }
}
