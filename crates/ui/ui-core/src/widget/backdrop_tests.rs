//! The backdrop paints the shell's wallpaper under everything, and its gradient where there is
//! no picture.

use vello::kurbo::{Affine, Rect};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData, ImageFormat};

use super::Backdrop;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{part_paint::wallpaper, part_paint::wallpaper::Source};

/// A picture of one colour, `edge` square.
fn picture(edge: u32, rgba: [u8; 4]) -> ImageData {
    let data = rgba.repeat(usize::try_from(edge * edge).expect("a small picture"));
    ImageData {
        data: Blob::new(std::sync::Arc::new(data)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: edge,
        height: edge,
    }
}

/// A backdrop laid out over `rect` and drawn on the theme's ground, rendered.
fn rendered(theme: &Theme, rect: Rect) -> Vec<u8> {
    let mut backdrop = Backdrop::new();
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    for _ in 0..2 {
        tilo_ui_base::widget::layout::lay_out_detached(&mut backdrop, rect, theme, &mut text)
            .expect("the backdrop lays out");
    }
    let mut scene = vello::Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.base,
        None,
        &Rect::new(0.0, 0.0, 240.0, 240.0),
    );
    Widget::draw(
        &mut backdrop,
        &mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            PresenceCtx::empty().draw_env(),
        ),
    );
    tilo_ui_base::offscreen::OffscreenRenderer::new(240, 240)
        .render(&scene, vello::peniko::Color::BLACK)
}

#[test]
fn the_backdrop_paints_the_shells_wallpaper_under_everything() {
    let theme = Theme::dark();
    let rect = Rect::new(40.0, 40.0, 200.0, 160.0);
    let source = Source::new();
    source.set(Some(picture(4, [220, 40, 40, 255])));
    wallpaper::install(Some(source));
    let seen = rendered(&theme, rect);
    let (red, green, blue, _) =
        tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 120, 120);
    assert!(
        red > 180 && green < 90 && blue < 90,
        "the wallpaper shows through: saw {red},{green},{blue}"
    );
    // Its gradient stands where the shell has no picture.
    let source = Source::new();
    source.set_gradient((Color::from_rgb8(20, 40, 200), Color::from_rgb8(20, 40, 200)));
    wallpaper::install(Some(source));
    let seen = rendered(&theme, rect);
    let (red, green, blue, _) =
        tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 120, 120);
    assert!(
        blue > 150 && red < 90,
        "the gradient stands where no picture does: saw {red},{green},{blue}"
    );
    // And nothing stands where the shell was handed no wallpaper at all.
    wallpaper::install(None);
    let seen = rendered(&theme, rect);
    let (red, green, blue, _) =
        tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 120, 120);
    let ground = theme.tokens.colors.surfaces.base.to_rgba8();
    let near = |seen: u8, want: u8| (i32::from(seen) - i32::from(want)).abs() <= 12;
    assert!(
        near(red, ground.r) && near(green, ground.g) && near(blue, ground.b),
        "the ground stands where no wallpaper was handed: saw {red},{green},{blue}"
    );
}
