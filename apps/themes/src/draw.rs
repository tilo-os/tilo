//! Content drawing: a miniature desktop in the theme being previewed.
//!
//! What it paints is the subject of the app — what a theme document does to
//! a window: ground, regions, ink tiers, the selection lift, a primary
//! action, an on switch, your focus ring, presence, status and a floating
//! panel. Every value comes from the *previewed* theme's tokens, never from
//! the theme the studio itself is drawn in.

use ui::prelude::*;
use ui::tokens::PresenceHue;
use ui::vello::kurbo::{Circle, RoundedRect, Shape, Stroke};
use ui::vello::peniko::{Color, Fill};

/// Logical size the miniature is composed at, before it is scaled to fit.
pub const CANVAS: (f64, f64) = (480.0, 300.0);

struct Painter<'a> {
    scene: &'a mut Scene,
    text: &'a mut TextRenderer,
    transform: Affine,
    theme: &'a Theme,
}

impl Painter<'_> {
    fn fill(&mut self, shape: &impl Shape, colour: Color) {
        self.scene
            .fill(Fill::NonZero, self.transform, colour, None, shape);
    }

    fn outline(&mut self, shape: &impl Shape, width: f64, colour: Color) {
        self.scene
            .stroke(&Stroke::new(width), self.transform, colour, None, shape);
    }

    fn label(&mut self, words: &str, role: TextRole, colour: Color, at: Point) {
        let style = self.theme.tokens.typography.style(role);
        self.text.draw_style(
            self.scene,
            words,
            colour,
            self.transform * Affine::translate(at.to_vec2()),
            None,
            &style,
        );
    }
}

fn rounded(rect: Rect, radius: f32) -> RoundedRect {
    RoundedRect::from_rect(rect, f64::from(radius))
}

/// Paint the miniature of `theme` at the origin of `transform`, in
/// [`CANVAS`] coordinates.
#[allow(
    clippy::too_many_lines,
    reason = "one composition, read top to bottom like the window it draws"
)]
pub fn miniature(scene: &mut Scene, transform: Affine, text: &mut TextRenderer, theme: &Theme) {
    let colors = &theme.tokens.colors;
    let radii = &theme.tokens.radii;
    let space = &theme.tokens.spacing;
    let mut paint = Painter {
        scene,
        text,
        transform,
        theme,
    };
    let (width, height) = CANVAS;
    let gutter = f64::from(space.s4);
    let inset = f64::from(space.s2);

    // The desktop ground and one window tile on it.
    paint.fill(&Rect::new(0.0, 0.0, width, height), colors.surfaces.base);
    let tile = Rect::new(gutter, gutter, width - gutter, height - gutter);
    paint.fill(&rounded(tile, radii.tile), colors.surfaces.content);
    paint.outline(&rounded(tile, radii.tile), 1.0, theme.window_edge());

    // The navigation region, with one selected row lifted.
    let sidebar = Rect::new(tile.x0, tile.y0, tile.x0 + 132.0, tile.y1);
    let corner = f64::from(radii.tile);
    paint.fill(
        &RoundedRect::from_rect(sidebar, (corner, 0.0, 0.0, corner)),
        colors.surfaces.sidebar,
    );
    let row_height = 26.0;
    for (i, name) in ["Inbox", "Drafts", "Shared"].into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "three rows")]
        let top = sidebar.y0 + gutter + i as f64 * (row_height + 2.0);
        let row = Rect::new(
            sidebar.x0 + inset,
            top,
            sidebar.x1 - inset,
            top + row_height,
        );
        let ink = if i == 0 {
            paint.fill(
                &rounded(row, radii.item),
                colors.lift_on(colors.surfaces.sidebar, 1.0),
            );
            colors.text_primary
        } else {
            colors.text_dim
        };
        paint.label(
            name,
            TextRole::Label,
            ink,
            Point::new(row.x0 + inset, row.y0 + 5.0),
        );
    }
    // Who else is here, in their colours.
    for (i, hue) in [PresenceHue::Orange, PresenceHue::Teal, PresenceHue::Pink]
        .into_iter()
        .enumerate()
    {
        #[allow(clippy::cast_precision_loss, reason = "three people")]
        let left = sidebar.x0 + gutter + 9.0 + i as f64 * 14.0;
        paint.fill(
            &Circle::new((left, sidebar.y1 - gutter - 9.0), 8.0),
            colors.presence.hue(hue),
        );
    }

    // The content region: a title, a line in each quieter ink tier, a field
    // with your focus ring and a collaborator's caret, an on switch, a
    // primary and a secondary action.
    let left = sidebar.x1 + f64::from(space.s6);
    let mut top = tile.y0 + gutter;
    paint.label(
        "Quarterly review",
        TextRole::Title,
        colors.text_primary,
        Point::new(left, top),
    );
    top += 30.0;
    paint.label(
        "Shared with Ada and Bo",
        TextRole::Body,
        colors.text_dim,
        Point::new(left, top),
    );
    top += 20.0;
    paint.label(
        "Edited 2 minutes ago",
        TextRole::Caption,
        colors.text_tertiary,
        Point::new(left, top),
    );
    top += 26.0;
    let field = Rect::new(left, top, left + 180.0, top + 28.0);
    paint.fill(&rounded(field, radii.control), colors.surfaces.raised);
    paint.outline(
        &rounded(field.inflate(2.0, 2.0), radii.control + 2.0),
        2.0,
        colors.intent.you,
    );
    paint.label(
        "Budget notes",
        TextRole::Label,
        colors.text_primary,
        Point::new(field.x0 + inset, field.y0 + 6.0),
    );
    paint.fill(
        &Rect::new(
            field.x0 + 112.0,
            field.y0 + 6.0,
            field.x0 + 114.0,
            field.y1 - 6.0,
        ),
        colors.presence.hue(PresenceHue::Orange),
    );
    let track = Rect::new(
        field.x1 + 16.0,
        field.y0 + 5.0,
        field.x1 + 50.0,
        field.y1 - 5.0,
    );
    paint.fill(
        &RoundedRect::from_rect(track, track.height() / 2.0),
        colors.intent.inverted,
    );
    paint.fill(
        &Circle::new((track.x1 - 9.0, track.center().y), 7.0),
        colors.intent.on_inverted,
    );

    top = field.y1 + gutter;
    let primary = Rect::new(left, top, left + 96.0, top + 30.0);
    paint.fill(&rounded(primary, radii.control), colors.intent.inverted);
    paint.label(
        "Share",
        TextRole::Label,
        colors.intent.on_inverted,
        Point::new(primary.x0 + 28.0, primary.y0 + 7.0),
    );
    let secondary = Rect::new(primary.x1 + inset, top, primary.x1 + 96.0, top + 30.0);
    paint.fill(&rounded(secondary, radii.control), colors.surfaces.raised);
    paint.label(
        "Cancel",
        TextRole::Label,
        colors.text_primary,
        Point::new(secondary.x0 + 22.0, secondary.y0 + 7.0),
    );

    // Status, muted.
    let status_y = primary.y1 + gutter + 6.0;
    for (i, colour) in [
        colors.semantic.success,
        colors.semantic.warning,
        colors.semantic.danger,
    ]
    .into_iter()
    .enumerate()
    {
        #[allow(clippy::cast_precision_loss, reason = "three dots")]
        let cx = left + 5.0 + i as f64 * 16.0;
        paint.fill(&Circle::new((cx, status_y), 5.0), colour);
    }

    // A floating panel: the overlay surface, its shadow and edge, and the
    // armed row lifted inside it.
    let panel = Rect::new(
        tile.x1 - 150.0,
        tile.y1 - 104.0,
        tile.x1 - 18.0,
        tile.y1 - 18.0,
    );
    ui::surface::shadow(
        paint.scene,
        transform,
        panel,
        f64::from(radii.panel),
        &theme.tokens.elevation.floating,
    );
    paint.fill(&rounded(panel, radii.panel), colors.surfaces.overlay);
    paint.outline(
        &rounded(panel, radii.panel),
        1.0,
        ui::chrome::panel_border(theme),
    );
    let armed = Rect::new(
        panel.x0 + inset,
        panel.y0 + 10.0,
        panel.x1 - inset,
        panel.y0 + 34.0,
    );
    paint.fill(
        &rounded(armed, radii.concentric(radii.panel, space.s2)),
        colors.lift_on(colors.surfaces.overlay, 1.0),
    );
    for (i, item) in ["Copy link", "Rename", "Move"].into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "three items")]
        let item_y = panel.y0 + 14.0 + i as f64 * 24.0;
        paint.label(
            item,
            TextRole::Label,
            colors.text_primary,
            Point::new(panel.x0 + 18.0, item_y),
        );
    }
}

/// The twelve presence colours of `theme`, as a strip of dots `height` tall.
pub fn presence_strip(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let presence = &theme.tokens.colors.presence;
    let radius = (rect.height() / 2.0).min(rect.width() / 24.0);
    for (i, hue) in PresenceHue::ALL.into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "twelve hues")]
        let x = rect.x0 + radius + i as f64 * radius * 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            presence.hue(hue),
            None,
            &Circle::new((x, rect.center().y), radius * 0.8),
        );
    }
}
