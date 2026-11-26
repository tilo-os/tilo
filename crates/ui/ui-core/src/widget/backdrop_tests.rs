//! The backdrop paints the shell's wallpaper under everything, and its gradient where there is
//! no picture.

use vello::kurbo::{Affine, Rect};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData, ImageFormat};

use super::{Backdrop, Ground};
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
    rendered_over(
        theme,
        rect,
        Ground::Wallpaper,
        theme.tokens.colors.surfaces.base,
    )
}

/// A backdrop of `ground` laid out over `rect` and drawn over `beneath`, rendered.
fn rendered_over(theme: &Theme, rect: Rect, ground: Ground, beneath: Color) -> Vec<u8> {
    let mut backdrop = Backdrop::new().with_ground(ground);
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    for _ in 0..2 {
        tilo_ui_base::widget::layout::lay_out_detached(&mut backdrop, rect, theme, &mut text)
            .expect("the backdrop lays out");
    }
    let mut scene = vello::Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        beneath,
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
    // And the theme's own ground stands where the shell was handed no wallpaper at all: light
    // paper on a light theme, the dark ground on a dark one.
    wallpaper::install(None);
    let luma = |(red, green, blue, _): (u8, u8, u8, u8)| {
        0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue)
    };
    let stands: [(Theme, &str, fn(f64) -> bool); 2] = [
        (Theme::light(), "light paper", |value| value > 170.0),
        (Theme::dark(), "the dark ground", |value| value < 80.0),
    ];
    for (theme, what, stands) in stands {
        let seen = rendered(&theme, rect);
        let ground = tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 120, 120);
        assert!(
            stands(luma(ground)),
            "the ground stands where no wallpaper was handed, as {what}: saw {ground:?}"
        );
    }
}

/// A light theme's ground stays light from corner to corner, and the ramp is dithered: a whole
/// screen's gradient steps by a level every few rows, and without the grain those steps band.
#[test]
fn a_light_ground_stays_light_and_its_ramp_is_dithered() {
    let theme = Theme::light();
    wallpaper::install(None);
    let seen = rendered(&theme, Rect::new(0.0, 0.0, 240.0, 240.0));
    let luma = |(red, green, blue, _): (u8, u8, u8, u8)| {
        0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue)
    };
    // The far corner is the darkest point of the ramp, and it still reads as light paper.
    let lit = tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 48, 48);
    let far = tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, 196, 156);
    assert!(
        luma(far) > 170.0,
        "the light ground's far corner stays light: saw {far:?}"
    );
    assert!(
        luma(far) < luma(lit),
        "the ground falls away toward the far corner: {far:?} against {lit:?}"
    );
    // Neighbouring pixels along one row of the ramp differ by a level here and there: the grain
    // is what makes them, where the bare ramp would step once every few columns.
    let varied = (80..160).any(|x| {
        let a = tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, x, 100);
        let b = tilo_ui_base::offscreen::OffscreenRenderer::sample(&seen, 240, x + 1, 100);
        a != b
    });
    assert!(varied, "the ground's ramp is dithered, not bare steps");
    // A level's grain, no more: the specks break the ramp's steps without reading as dots.
    let patch: Vec<f64> = (90..130)
        .flat_map(|y| (60..100).map(move |x| (x, y)))
        .map(|(x, y)| {
            luma(tilo_ui_base::offscreen::OffscreenRenderer::sample(
                &seen, 240, x, y,
            ))
        })
        .collect();
    let count = f64::from(u32::try_from(patch.len()).unwrap_or(1));
    let mean = patch.iter().sum::<f64>() / count;
    let spread = (patch.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / count).sqrt();
    assert!(
        (0.4..=4.0).contains(&spread),
        "the grain is a level's, never dots: spread {spread:.2}"
    );
}

/// Setup stands on the system's own ground, opaque over whatever an app drew; its tour veils the
/// desktop it covers and leaves it legible, and nothing outside its box.
#[test]
fn setup_stands_on_its_own_ground_and_its_tour_veils_only_what_it_covers() {
    let rect = Rect::new(40.0, 40.0, 200.0, 160.0);
    let red = Color::from_rgb8(220, 40, 40);
    // A light theme and a dark one: the trusted ground stays opaque over what an app drew, and
    // the tour veils the desktop it covers either way, never lightening it.
    for theme in [Theme::light(), Theme::dark()] {
        let seen = rendered_over(&theme, rect, Ground::Trusted, red);
        let (r, g, b, _) = sample(&seen, 120, 120);
        assert!(
            r.abs_diff(g) < 16 && g.abs_diff(b) < 16,
            "in {} what lies beneath never shows through: saw {r},{g},{b}",
            theme.name
        );

        let seen = rendered_over(&theme, rect, Ground::Scrim, red);
        let (r, g, b, _) = sample(&seen, 120, 120);
        let (outside, outside_g, outside_b, _) = sample(&seen, 10, 10);
        assert!(
            u32::from(r) + u32::from(g) + u32::from(b)
                < u32::from(outside) + u32::from(outside_g) + u32::from(outside_b),
            "in {} the tour darkens the desktop it covers: saw {r},{g},{b} against {outside},{outside_g},{outside_b}",
            theme.name
        );
        assert!(
            r > g + 40,
            "in {} the veiled desktop is still legible: saw {r},{g},{b}",
            theme.name
        );
        assert!(
            outside > 200 && outside_g < 60 && outside_b < 60,
            "in {} outside its box the desktop is untouched: saw {outside},{outside_g},{outside_b}",
            theme.name
        );
    }
}

/// The pixel at `(x, y)` of a 240-pixel-wide render.
fn sample(seen: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    tilo_ui_base::offscreen::OffscreenRenderer::sample(seen, 240, x, y)
}
