//! A single, non-human identity silhouette for agents across Tilo.
//!
//! People keep the shared circular [`crate::widget::Avatar`]. Agents use this
//! rounded processor tile with a circuit-shaped centre. The distinction is
//! geometric rather than textual, so it remains visible when a label is
//! elided, in monochrome, and at live-presence sizes.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::{ColorCompat, theme::Theme};

/// Whether the principal is present or only represented by an unaccepted
/// invitation. An invited identity stays hollow instead of pretending to be
/// a live collaborator.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AgentIdentityStyle {
    #[default]
    Solid,
    Outline {
        alpha: f32,
    },
}

/// Paint Tilo's shared agent identity mark into `rect`.
///
/// `color` is the principal's stable identity colour. Interior ink is chosen
/// through the active theme rather than fixed black or white, preserving the
/// same contrast behaviour as other themed controls. The geometry scales down
/// to the 9px live-presence badge without relying on a font or a UTF symbol.
pub fn draw_agent_identity_mark(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    color: Color,
    theme: &Theme,
    style: AgentIdentityStyle,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }

    let size = rect.width().min(rect.height());
    let radius = (size * 0.22).min(f64::from(theme.tokens.radii.detail));
    let tile = RoundedRect::from_rect(rect, radius);
    let ink = match style {
        AgentIdentityStyle::Solid => {
            scene.fill(Fill::NonZero, transform, color, None, &tile);
            theme.on_surface(color).ink
        }
        AgentIdentityStyle::Outline { alpha } => {
            let stroke_color = color.with_alpha_factor(alpha.clamp(0.0, 1.0));
            scene.stroke(
                &Stroke::new((size * 0.055).clamp(1.0, 1.5)).with_dashes(0.0, [3.0, 2.0]),
                transform,
                stroke_color,
                None,
                &tile,
            );
            theme.tokens.colors.text_primary.with_alpha_factor(alpha)
        }
    };

    // A circuit diamond and its two traces read as a synthetic identity at
    // roster scale, while the strong horizontal centre still survives the
    // antialiasing footprint of a compact presence badge.
    let center = rect.center();
    let diamond_radius = size * 0.21;
    let inset = size * 0.14;
    let mut circuit = BezPath::new();
    circuit.move_to((center.x, center.y - diamond_radius));
    circuit.line_to((center.x + diamond_radius, center.y));
    circuit.line_to((center.x, center.y + diamond_radius));
    circuit.line_to((center.x - diamond_radius, center.y));
    circuit.close_path();
    circuit.move_to((rect.x0 + inset, center.y));
    circuit.line_to((center.x - diamond_radius, center.y));
    circuit.move_to((center.x + diamond_radius, center.y));
    circuit.line_to((rect.x1 - inset, center.y));
    scene.stroke(
        &Stroke::new((size * 0.065).clamp(1.0, 1.5)),
        transform,
        ink,
        None,
        &circuit,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::OffscreenRenderer;

    #[test]
    fn shared_agent_mark_renders_at_presence_size_in_every_system_palette() {
        let color = Color::from_rgb8(0x63, 0x66, 0xf1);
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut scene = Scene::new();
            draw_agent_identity_mark(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(8.0, 8.0, 17.0, 17.0),
                color,
                &theme,
                AgentIdentityStyle::Solid,
            );
            let ground = theme.tokens.colors.surfaces.content;
            let mut renderer = OffscreenRenderer::new(25, 25);
            let pixels = renderer.render(&scene, ground);
            let background = ground.to_rgba8();
            let changed = (7..=18).any(|y| {
                (7..=18).any(|x| {
                    let pixel = OffscreenRenderer::sample(&pixels, 25, x, y);
                    pixel.0 != background.r || pixel.1 != background.g || pixel.2 != background.b
                })
            });
            assert!(changed, "agent mark disappeared in {}", theme.name);
        }
    }

    #[test]
    fn outline_style_is_not_filled_like_an_active_agent() {
        let theme = Theme::dark();
        let color = Color::from_rgb8(0x22, 0xc5, 0x5e);
        let render = |style| {
            let mut scene = Scene::new();
            draw_agent_identity_mark(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(8.0, 8.0, 30.0, 30.0),
                color,
                &theme,
                style,
            );
            let mut renderer = OffscreenRenderer::new(40, 40);
            renderer.render(&scene, theme.tokens.colors.surfaces.content)
        };
        let solid = render(AgentIdentityStyle::Solid);
        let outline = render(AgentIdentityStyle::Outline { alpha: 0.55 });
        let solid_corner = OffscreenRenderer::sample(&solid, 40, 12, 12);
        let outline_corner = OffscreenRenderer::sample(&outline, 40, 12, 12);
        assert_ne!(solid_corner, outline_corner);
    }
}
