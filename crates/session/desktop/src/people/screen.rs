//! Someone's screen as a small picture: a window card holding their windows where they stand, each
//! named under its app's mark where there is room, the one they work in ringed in their colour.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;

use ui::TextRole;
use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use super::RosterRow;
use crate::here::{Screen, ScreenWindow};

/// One of their windows inside their screen `screen`: its ground, its app's mark, what it is
/// called here under it where there is room, and the ring of the one they work in.
fn paint_window(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    screen_rect: Rect,
    window: &ScreenWindow,
    colour: Color,
) {
    let tile = Rect::new(
        screen_rect.x0 + window.rect.x0 * screen_rect.width(),
        screen_rect.y0 + window.rect.y0 * screen_rect.height(),
        screen_rect.x0 + window.rect.x1 * screen_rect.width(),
        screen_rect.y0 + window.rect.y1 * screen_rect.height(),
    )
    .inset(-1.0);
    if tile.width() < 4.0 || tile.height() < 4.0 {
        return;
    }
    // The tile is a well in the screen's card, and its rest edge is the well's own.
    let on = ui::widget::paint_surface(
        scene,
        transform,
        tile,
        ui::widget::SurfaceLevel::Raised,
        theme,
    );
    let ink = on.tokens.colors.text_dim;
    let style = on
        .text(TextRole::Caption)
        .with_weight(ui::text_render::TextWeight::Medium);
    let line_h = f64::from(theme.text(TextRole::Caption).line_height_px());
    let icon = (tile.height() * 0.3).clamp(10.0, 20.0);
    let name = window
        .name
        .as_deref()
        .filter(|_| tile.height() >= icon + line_h + 10.0 && tile.width() >= 44.0);
    let block = if name.is_some() {
        icon + 4.0 + line_h
    } else {
        icon
    };
    let top = tile.center().y - block / 2.0;
    let mark = Rect::new(
        tile.center().x - icon / 2.0,
        top,
        tile.center().x + icon / 2.0,
        top + icon,
    );
    let glyph = match ui::icons::app_glyph(&window.app, None) {
        Icon::AppWindow => Icon::Square,
        glyph => glyph,
    };
    glyph.draw_with_stroke(scene, transform, mark, ink, 1.5);
    if let Some(name) = name {
        let lines = Rect::new(
            tile.x0 + 4.0,
            mark.y1 + 4.0,
            tile.x1 - 4.0,
            mark.y1 + 4.0 + line_h,
        );
        ui::widget::TextLineFace::styled(name, &style, TextRole::Caption).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, &on),
            lines,
            ink,
        );
    }
    // The one they work in wears their colour: the surface's own edge stands everywhere else.
    if window.focused {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            colour,
            None,
            &ui::surface::edge_outline(tile, ui::widget::SurfaceLevel::Raised.radius(theme)),
        );
    }
}

/// Someone's screen as a small picture: a window card holding their windows where they stand,
/// each named under its app's mark where there is room, the one they work in ringed in their
/// colour.
pub(super) fn picture(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    row: &RosterRow,
    screen: &Screen,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let aspect = screen.size.0 / screen.size.1.max(1.0);
    // Their screen keeps its shape inside the room the card gives it.
    let (width, height) = if rect.width() / aspect <= rect.height() {
        (rect.width(), rect.width() / aspect)
    } else {
        (rect.height() * aspect, rect.height())
    };
    let screen_rect = Rect::from_center_size(rect.center(), (width, height));
    ui::widget::paint_window_card(scene, transform, screen_rect, theme, |scene, on| {
        for window in &screen.windows {
            paint_window(scene, transform, text, on, screen_rect, window, row.color);
        }
    });
}
