use tilo_ui_base::surface::*;
use tilo_ui_base::theme::PresetDesign;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::Colors;
use tilo_ui_base::tokens::Shadow;
use vello::Scene;
use vello::kurbo::Affine;
use vello::kurbo::Rect;
use vello::kurbo::RoundedRect;
use vello::peniko::Color;
use vello::peniko::Fill;

tilo_test_env::scenario! {
    /// Edge fades, separators and derived edges.
    /// Each phase was a test of its own and builds its own fixture.
    fn edges_are_crisp_and_register_on_every_surface;

    phase an_edge_fade_is_one_gradient_inside_its_edge_band;
    {
        let ground = Theme::dark().tokens.colors.surfaces.content;
        for edge in [
            ContentEdge::Top,
            ContentEdge::Bottom,
            ContentEdge::Leading,
            ContentEdge::Trailing,
        ] {
            let mut scene = Scene::new();
            content_edge_fade(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 300.0, 200.0),
                edge,
                ground,
            );
            let tags = &scene.encoding().draw_tags;
            assert_eq!(tags.len(), 1, "{edge:?}");
            assert!(
                tags[0] == vello_encoding::DrawTag::LINEAR_GRADIENT,
                "{edge:?}"
            );
        }
        let mut scene = Scene::new();
        content_edge_fade(
            &mut scene,
            Affine::IDENTITY,
            Rect::ZERO,
            ContentEdge::Top,
            ground,
        );
        assert!(scene.encoding().draw_tags.is_empty());
    }

    phase a_vertical_separator_is_one_crisp_column;
    {
        let mut scene = Scene::new();
        let surface = Theme::light().tokens.colors.surfaces.content;
        vertical_separator(&mut scene, Affine::IDENTITY, 100.3, 0.0, 50.0, 4.0, surface);
        assert_eq!(scene.encoding().draw_tags.len(), 1);
        let mut empty = Scene::new();
        vertical_separator(&mut empty, Affine::IDENTITY, 10.0, 0.0, 6.0, 4.0, surface);
        assert!(
            empty.encoding().draw_tags.is_empty(),
            "inset past its length"
        );
        assert_eq!(
            tilo_ui_base::paint::snap_rect(Rect::new(0.4, 1.6, 10.5, 9.49)),
            Rect::new(0.0, 2.0, 11.0, 9.0)
        );
    }

    phase edges_and_chips_register_on_every_preset_surface;
    {
        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let s = &theme.tokens.colors.surfaces;
                for (name, ground) in [
                    ("base", s.base),
                    ("raised", s.raised),
                    ("overlay", s.overlay),
                    ("panel_ground", tilo_ui_base::chrome::shell::panel_ground(&theme)),
                ] {
                    let edge = edge_on(ground);
                    let moved = edge_delta(edge, ground);
                    assert!(
                        moved >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{name} edge on {:?} moved only {moved:.4} ({edge:?})",
                        theme.name
                    );
                }
            }
            // The declared border is what vanished on the dark preset: derived must
            // register there where declared does not.
            let dark = Theme::dark();
            let raised = dark.tokens.colors.surfaces.raised;
            let declared = edge_delta(dark.tokens.colors.border_standard, raised);
            let derived = edge_delta(edge_on(raised), raised);
            assert!(
                declared < tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                "dark border_standard on raised was the invisible edge ({declared:.4}); if this \
         changed, the ratchet's rationale needs revisiting"
            );
            assert!(
                derived > declared,
                "derived {derived:.4} vs declared {declared:.4}"
            );
        }

        {
            // A mid grey moves plenty at alpha 26: the ladder must not over-darken it.
            let grey = Color::from_rgb8(0x80, 0x80, 0x80);
            assert_eq!(
                edge_on(grey),
                Colors::hairline_on(grey, EDGE_ALPHA_STEPS[0])
            );
        }

        {
            // A 16 % accent wash over the light panel is still a light surface.
            for theme in [Theme::light(), Theme::dark()] {
                let ground = theme.tokens.colors.surfaces.raised;
                let wash = theme.tokens.colors.text_primary.with_alpha(0.16);
                let seen = tilo_ui_base::tokens::colors::composite(wash, ground);
                let derived = edge_on(seen);
                assert!(
                    edge_delta(derived, seen) >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                    "card_on edge must register on the seen surface"
                );
                let naive = edge_on(wash);
                assert!(
                    edge_delta(derived, seen) >= edge_delta(naive, seen),
                    "composited derivation must never register worse than the naive one"
                );
            }
        }

        {
            let whole = separator_rect(10.0, 200.0, 100.0, 12.0).expect("rule");
            assert_eq!((whole.x0, whole.x1), (22.0, 188.0));
            assert_eq!((whole.y0, whole.y1), (100.0, 101.0));
            let half = separator_rect(0.0, 50.0, 99.5, 0.0).expect("rule");
            assert_eq!((half.y0, half.y1), (99.0, 100.0));
            assert!(
                separator_rect(0.0, 20.0, 5.0, 12.0).is_none(),
                "insets that meet paint nothing"
            );
        }

        {
            // The memo must be transparent: the same surface asked twice (and many
            // distinct surfaces cycling through the slots) always yields exactly what the
            // ladder derives.
            let theme = Theme::dark();
            let surfaces = [
                theme.tokens.colors.surfaces.base,
                theme.tokens.colors.surfaces.raised,
                theme.tokens.colors.surfaces.overlay,
                theme.tokens.colors.lift(0.35),
                theme.tokens.colors.lift(0.5),
                Theme::light().tokens.colors.surfaces.base,
                Theme::light().tokens.colors.surfaces.raised,
                Theme::high_contrast().tokens.colors.surfaces.base,
                Theme::high_contrast().tokens.colors.surfaces.raised,
                Color::from_rgb8(120, 60, 200),
                Color::from_rgb8(255, 255, 255),
                Color::from_rgb8(0, 0, 0),
            ];
            for _ in 0..3 {
                for surface in surfaces {
                    assert_eq!(edge_on(surface), derive_edge_on(surface));
                }
            }
        }

        {
            for (name, theme) in [
                ("dark", Theme::dark()),
                ("light", Theme::light()),
                ("high-contrast", Theme::high_contrast()),
            ] {
                let ground = tilo_ui_base::chrome::panel_ground(&theme);
                let border = tilo_ui_base::chrome::panel_border(&theme);
                if theme.tokens.colors.palette.high_contrast {
                    assert!(
                        edge_delta(border, ground) >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{name}: the panel border must register on the panel ground"
                    );
                } else {
                    assert_eq!(border, theme.tokens.elevation.floating_edge, "{name}");
                    assert!(edge_delta(border, ground) > 0.005, "{name}: it registers");
                }
            }
            let mut scene = Scene::new();
            panel(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(10.0, 10.0, 10.0, 40.0),
                8.0,
                &Theme::dark(),
            );
            assert!(
                scene.encoding().is_empty(),
                "an empty panel encodes no draw ops"
            );
        }

        {
            let rect = Rect::new(10.0, 20.0, 110.0, 60.0);
            let outline = edge_outline(rect, 12.0);
            assert_eq!(outline.rect(), Rect::new(10.5, 20.5, 109.5, 59.5));
            assert!((outline.radii().top_left - 11.5).abs() < 1e-9);
            // A radius under half a pixel does not go negative.
            assert!(edge_outline(rect, 0.0).radii().top_left.abs() < 1e-9);
        }

        {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let quiet = crate::widget::ChipStyle::Neutral.colors(
                    &theme,
                    theme.tokens.colors.surfaces.content,
                    false,
                    false,
                );
                let edge = quiet.edge.expect("quiet chip has an edge");
                assert_eq!(
                    edge,
                    edge_on(quiet.fill),
                    "{}: edge derives from the fill",
                    theme.name
                );
                assert!(
                    edge_delta(edge, quiet.fill) >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: quiet chip edge moved only {:.4}",
                    theme.name,
                    edge_delta(edge, quiet.fill)
                );
            }
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test sample coordinates are always non-negative pixel offsets within the rendered canvas"
)]
fn render_token_shadow(
    w: u32,
    h: u32,
    rect: Rect,
    radius: f64,
    token: &Shadow,
) -> impl Fn(f64, f64) -> u8 {
    use tilo_ui_base::offscreen::OffscreenRenderer;
    let mut scene = Scene::new();
    shadow(&mut scene, Affine::IDENTITY, rect, radius, token);
    let mut r = OffscreenRenderer::new(w, h);
    let rgba = r.render(&scene, Color::BLACK);
    move |x: f64, y: f64| OffscreenRenderer::sample(&rgba, w, x.floor() as u32, y.floor() as u32).0
}

/// A control face painted in `finish` (and `border`, `depth`) over the dark window
/// ground: RGBA rows of 120 × 60.
fn control_in(
    finish: tilo_ui_base::tokens::Finish,
    border: tilo_ui_base::tokens::Border,
    depth: tilo_ui_base::tokens::Depth,
) -> (Theme, Vec<u8>) {
    use tilo_ui_base::offscreen::OffscreenRenderer;
    let mut document = tilo_ui_base::theme::Preset::Dark.document();
    document.material.control = finish;
    document.shape.border = border;
    document.depth = depth;
    let theme = Theme::from_document(&document).unwrap_or_else(|| {
        panic!(
            "the test's finish is a theme: {:?}",
            Theme::refusal(&document)
        )
    });
    let mut scene = Scene::new();
    let ground = theme.tokens.colors.surfaces.content;
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        ground,
        None,
        &Rect::new(0.0, 0.0, 120.0, 60.0),
    );
    let body = RoundedRect::from_rect(Rect::new(20.0, 10.0, 100.0, 50.0), 8.0);
    shadow(
        &mut scene,
        Affine::IDENTITY,
        body.rect(),
        8.0,
        &theme.tokens.elevation.window,
    );
    control_face(
        &mut scene,
        Affine::IDENTITY,
        &body,
        theme.tokens.colors.surfaces.raised,
        &theme,
    );
    let mut renderer = OffscreenRenderer::new(120, 60);
    let rgba = renderer.render(&scene, Color::BLACK);
    (theme, rgba)
}

fn at(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
    let (r, g, b, _) = tilo_ui_base::offscreen::OffscreenRenderer::sample(rgba, 120, x, y);
    [r, g, b]
}

fn rgb(color: Color) -> [u8; 3] {
    let [r, g, b, _] = color.to_rgba8().to_u8_array();
    [r, g, b]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 2)
}

#[test]
fn a_control_face_takes_every_finish_border_and_depth_of_its_theme() {
    use tilo_ui_base::{tokens::Border, tokens::BorderInk, tokens::Depth, tokens::Finish};
    // Flat and solid: the raised fill, exactly.
    for finish in [Finish::flat(), Finish::solid()] {
        let (theme, rgba) = control_in(finish, Border::STOCK, Depth::None);
        assert!(near(
            at(&rgba, 60, 30),
            rgb(theme.tokens.colors.surfaces.raised)
        ));
    }
    let (theme, rgba) = control_in(Finish::frosted(0.0, 0.5, 1.0), Border::STOCK, Depth::None);
    let half = tilo_ui_base::tokens::colors::over(
        theme.tokens.colors.surfaces.content,
        theme.tokens.colors.surfaces.raised,
        0.5,
    );
    assert!(
        near(at(&rgba, 60, 30), rgb(half)),
        "{:?}",
        at(&rgba, 60, 30)
    );
    let (_, rgba) = control_in(
        Finish::glass([0.0, 0.6, 1.0, 0.0, 0.5, 0.3]),
        Border::STOCK,
        Depth::None,
    );
    let sum = |p: [u8; 3]| p.iter().map(|c| u16::from(*c)).sum::<u16>();
    assert!(sum(at(&rgba, 30, 14)) > sum(at(&rgba, 90, 46)), "sheen");
    assert!(sum(at(&rgba, 60, 10)) > sum(at(&rgba, 60, 13)), "lit rim");
    let (theme, rgba) = control_in(
        Finish::flat(),
        Border {
            width: 2.0,
            ink: BorderInk::Strong,
        },
        Depth::None,
    );
    assert!(near(
        at(&rgba, 60, 10),
        rgb(theme.tokens.colors.text_tertiary)
    ));
    assert!(near(
        at(&rgba, 60, 11),
        rgb(theme.tokens.colors.text_tertiary)
    ));
    assert!(near(
        at(&rgba, 60, 54),
        rgb(theme.tokens.colors.surfaces.content)
    ));
    let (theme, rgba) = control_in(Finish::solid(), Border::STOCK, Depth::Outline);
    assert!(!near(
        at(&rgba, 60, 50),
        rgb(theme.tokens.colors.surfaces.content)
    ));
    assert!(near(
        at(&rgba, 60, 56),
        rgb(theme.tokens.colors.surfaces.content)
    ));
    let (theme, rgba) = control_in(Finish::solid(), Border::STOCK, Depth::STOCK);
    assert!(!near(
        at(&rgba, 60, 52),
        rgb(theme.tokens.colors.surfaces.content)
    ));
}

#[test]
fn token_shadow_is_even_and_follows_its_alpha_and_blur() {
    // Case `token_shadow_is_even_around_a_rounded_corner`: A shadow is as even around
    // a rounded corner as along a flat edge, since it's a blur of the surface's own
    // outline.
    {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let radius = 24.0;
        // offset_y 0 isolates shape from the deliberate downward shift.
        let token = Shadow {
            offset_y: 0.0,
            blur: 16.0,
            color: Color::WHITE,
            outline: None,
        };
        let at = render_token_shadow(400, 400, rect, radius, &token);

        let cc = 250.0 - radius;
        let k = std::f64::consts::FRAC_1_SQRT_2;
        for d in [2.0_f64, 5.0, 9.0, 14.0] {
            let edge = at(250.0 + d, 200.0);
            let corner = at(cc + (radius + d) * k, cc + (radius + d) * k);
            let (lo, hi) = (f64::from(edge.min(corner)), f64::from(edge.max(corner)));
            assert!(
                hi - lo <= 14.0,
                "at {d} px out the shadow must be as dense at the rounded \
             corner as along the flat edge: edge {edge}, corner \
             {corner}. A corner far lighter than the edge is a shadow \
             whose outline is not the surface's outline."
            );
        }
    }

    {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let radius = 24.0;
        let peak = |a: u8| {
            let token = Shadow {
                offset_y: 0.0,
                blur: 16.0,
                color: Color::from_rgba8(255, 255, 255, a),
                outline: None,
            };
            let at = render_token_shadow(400, 400, rect, radius, &token);
            at(251.0, 200.0)
        };
        let full = peak(255);
        let half = peak(128);
        assert!(
            full > 60,
            "a full-strength shadow must actually paint (got {full})"
        );
        let ratio = f64::from(half) / f64::from(full);
        assert!(
            (ratio - 0.5).abs() <= 0.08,
            "halving the token's alpha must halve the shadow: {full} -> \
         {half} (ratio {ratio:.3}). A ratio near 1.0 means the token's \
         alpha is being discarded."
        );
    }

    {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let token = Shadow {
            offset_y: 0.0,
            blur: 16.0,
            color: Color::WHITE,
            outline: None,
        };
        let at = render_token_shadow(400, 400, rect, 24.0, &token);
        let reach = std_dev_for(token.blur) * 2.5;
        let mut last = 0.0_f64;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "reach is 2.5 * std_dev_for(a non-negative blur), so reach * 2.0 is \
                  always non-negative and far below u32::MAX"
        )]
        let steps = (reach * 2.0) as u32;
        for d in 1..=steps {
            if at(250.0 + f64::from(d), 200.0) > 2 {
                last = f64::from(d);
            }
        }
        assert!(
            last <= reach + 2.0,
            "the shadow must die out within 2.5 standard deviations \
         ({reach:.1} px) of the edge; it was still painting at {last} px"
        );
        assert!(
            last >= reach * 0.5,
            "the shadow must actually reach most of its blur \
         ({reach:.1} px); it stopped at {last} px"
        );
    }
}

/// The red channel of the render *is* the composited alpha, so the sampler reads the
/// production output of [`soft_shadow`] in the same units [`shadow_profile`]
/// declares.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test sample coordinates are always non-negative pixel offsets within the rendered canvas"
)]
fn render_shadow(w: u32, h: u32, rect: Rect, spread: f64) -> impl Fn(f64, f64) -> u8 {
    use tilo_ui_base::offscreen::OffscreenRenderer;
    let mut scene = Scene::new();
    soft_shadow(
        &mut scene,
        Affine::IDENTITY,
        rect,
        Color::WHITE,
        spread,
        0.0,
        1.0,
    );
    let mut r = OffscreenRenderer::new(w, h);
    let rgba = r.render(&scene, Color::BLACK);
    move |x: f64, y: f64| OffscreenRenderer::sample(&rgba, w, x.floor() as u32, y.floor() as u32).0
}

#[test]
fn soft_shadow_falloff_is_seamless_circular_and_spread_sized() {
    // Case `soft_shadow_peak_matches_sprite_envelope`: The composited shadow has to
    // reach the strength the sprite's alpha envelope describes.
    {
        let rect = Rect::new(150.0, 150.0, 250.0, 250.0);
        let spread = 30.0;
        let at = render_shadow(400, 400, rect, spread);

        let (cx, cy) = (200.0, 200.0);
        let half = rect.width() / 2.0 + spread; // 80 px
        let (mut peak, mut peak_dx) = (0u8, 0.0f64);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "half is rect.width() / 2.0 + spread, always non-negative for any rect this toolkit draws"
        )]
        let steps = half as u32;
        for dx in 0..=steps {
            let v = at(cx + f64::from(dx), cy);
            if v > peak {
                peak = v;
                peak_dx = f64::from(dx);
            }
        }

        #[allow(
            clippy::cast_possible_truncation,
            reason = "255.0 * (1.0 - SHADOW_PEAK_RADIUS) is within 0.0..=255.0"
        )]
        let expected = (255.0 * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "255.0 * (1.0 - SHADOW_PEAK_RADIUS)^2 is within 0.0..=255.0"
        )]
        let double_premultiplied =
            (255.0 * (1.0 - SHADOW_PEAK_RADIUS) * (1.0 - SHADOW_PEAK_RADIUS)).round() as i32;
        assert!(
            (i32::from(peak) - expected).abs() <= 6,
            "full-intensity white shadow must peak at ~{expected}/255 \
         (the sprite's own alpha envelope); got {peak}. A peak near \
         {double_premultiplied} means the premultiplied sprite is being premultiplied a \
         second time.",
        );

        let expected_dx = rect.width() / 2.0 - (1.0 - 2.0 * SHADOW_PEAK_RADIUS) * spread;
        assert!(
            (peak_dx - expected_dx).abs() <= 4.0,
            "the ring must peak {expected_dx} px out from the centre — \
         {} px inside the rect's own edge, independent of the rect's \
         size; peaked at {peak_dx}",
            (1.0 - 2.0 * SHADOW_PEAK_RADIUS) * spread,
        );

        assert!(at(cx, cy) <= 6, "sprite centre must stay clear");
        assert_eq!(
            at(cx + half, cy + half),
            0,
            "the stretched sprite must not leak a square-edged halo"
        );
    }

    // Case `soft_shadow_falloff_width_is_the_spread_not_the_rects_size`: The falloff
    // outside an edge is a function of the distance to that edge and of `spread` —
    // never of the rect's own dimensions. Wide, tall and small-square must all match
    // [`shadow_profile`] pixel for pixel, on both axes.
    {
        let spread = 40.0;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "spread is a small positive px constant in these tests, always well within u32 range"
        )]
        let span = spread as u32 + 6;
        let wide = outward_profiles(Rect::new(150.0, 330.0, 1172.0, 565.0), spread, span);
        let tall = outward_profiles(Rect::new(560.0, 100.0, 800.0, 800.0), spread, span);
        let square = outward_profiles(Rect::new(600.0, 400.0, 700.0, 500.0), spread, span);

        for (label, profile) in [
            ("wide/right", &wide.0),
            ("wide/bottom", &wide.1),
            ("tall/right", &tall.0),
            ("tall/bottom", &tall.1),
            ("square/right", &square.0),
            ("square/bottom", &square.1),
        ] {
            for (k, got) in profile.iter().enumerate() {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "k is a small profile-vector index (at most a few hundred), \
                          far below where f64 loses integer precision"
                )]
                let u = (k as f64 + 0.5) / spread;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "255.0 * shadow_profile(u) is within 0.0..=255.0"
                )]
                let want = (255.0 * shadow_profile(u)).round() as i32;
                assert!(
                    (i32::from(*got) - want).abs() <= 8,
                    "{label}: {k} px out from the edge must be {want}/255 \
                 (shadow_profile({u:.3})); got {got}. Whole profile: \
                 {profile:?}"
                );
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "spread * 0.5 is a small positive px offset, always within the profile vector's length"
            )]
            let half_way = profile[(spread * 0.5) as usize];
            assert!(
                half_way >= 8,
                "{label}: half a spread ({} px) out the shadow must still \
             be painting; got {half_way}/255: {profile:?}",
                spread * 0.5,
            );
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "spread is a small positive px constant, always within the profile vector's length"
            )]
            let spread_floor = spread as usize;
            assert!(
                profile[spread_floor..].iter().all(|v| *v == 0),
                "{label}: nothing may paint past {spread} px — the spread \
             *is* the falloff width: {profile:?}"
            );
            #[allow(
                clippy::cast_possible_truncation,
                reason = "255.0 * shadow_profile(0.0) is within 0.0..=255.0"
            )]
            let edge_alpha = (255.0 * shadow_profile(0.0)) as i32;
            assert!(
                i32::from(profile[0]) >= edge_alpha - 12,
                "{label}: the shadow at the edge must read ~{}/255; got {}",
                (255.0 * shadow_profile(0.0)).round(),
                profile[0]
            );
        }

        for (label, a, b) in [
            ("wide vs square", &wide.0, &square.0),
            ("tall vs square", &tall.0, &square.0),
            ("wide: x vs y", &wide.0, &wide.1),
            ("tall: x vs y", &tall.0, &tall.1),
        ] {
            for (k, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert!(
                    (i32::from(*x) - i32::from(*y)).abs() <= 6,
                    "{label}: the profiles diverge {k} px out from the \
                 edge ({x} vs {y}): {a:?} vs {b:?}"
                );
            }
        }
    }

    {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        for r in [6.0_f64, 12.0, 20.0, 28.0, 36.0] {
            let straight = i32::from(at(rect.x1 + r, rect.center().y));
            for deg in [15.0_f64, 30.0, 45.0, 60.0, 75.0] {
                let (sin, cos) = deg.to_radians().sin_cos();
                let got = i32::from(at(rect.x1 + r * cos, rect.y1 + r * sin));
                assert!(
                    (got - straight).abs() <= 8,
                    "{r} px out from the bottom-right corner at {deg}° \
                 reads {got}/255, but {r} px straight out from the \
                 edge reads {straight}/255 — the corner is not a \
                 circle of radius {spread}"
                );
            }
        }
    }

    {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        // The corner slices own the first and last `spread` of the edge; between them
        // the field must not vary at all.
        for dy in [1.0_f64, 8.0, 20.0] {
            let y = rect.y1 + dy;
            let start = rect.x0 + spread;
            let end = rect.x1 - spread;
            let span = end - start;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "span is the sampled interior width in px (rect width minus \
                      2*spread); always non-negative here and far below u32::MAX \
                      for any rect this toolkit draws"
            )]
            let steps = if span >= 0.0 {
                (span as u32).saturating_add(1)
            } else {
                0
            };
            let samples: Vec<(f64, u8)> = (0..steps)
                .map(|i| {
                    let x = start + f64::from(i);
                    (x, at(x, y))
                })
                .collect();
            let lo = samples.iter().map(|(_, v)| *v).min().unwrap();
            let hi = samples.iter().map(|(_, v)| *v).max().unwrap();
            assert!(
                i32::from(hi) - i32::from(lo) <= 3,
                "{dy} px below the bottom edge the shadow varies from \
             {lo} to {hi} across the edge's length; the worst \
             columns were {:?}",
                samples
                    .iter()
                    .filter(|(_, v)| *v == lo || *v == hi)
                    .take(6)
                    .collect::<Vec<_>>()
            );
        }
    }

    // Case `soft_shadow_leaves_a_wide_rects_interior_clear`: A wide card's interior
    // stays completely clear — the centre slice is skipped because the envelope is
    // zero more than one band in.
    {
        let spread = 40.0;
        let rect = Rect::new(150.0, 330.0, 1172.0, 565.0);
        let at = render_shadow(1400, 900, rect, spread);
        for fx in [0.25_f64, 0.5, 0.75] {
            let x = rect.x0 + rect.width() * fx;
            let y = rect.center().y;
            assert_eq!(at(x, y), 0, "the interior at ({x}, {y}) must be clear");
        }
    }

    {
        let mut scene = Scene::new();
        // Zero-area rect, zero intensity, zero spread — must not panic and must add
        // nothing detectable; the test only asserts it returns.
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            Color::BLACK,
            10.0,
            0.0,
            0.5,
        );
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::BLACK,
            10.0,
            0.0,
            0.0,
        );
        soft_shadow(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Color::BLACK,
            0.0,
            0.0,
            0.5,
        );
    }

    // Case `a_pill_thinner_than_the_spread_keeps_the_full_falloff`: A pill shorter
    // than `2 · spread` still gets the full outward falloff: opposing slices share
    // the interior instead of squashing the profile.
    {
        let spread = 20.0;
        // 24 px tall — narrower than 2 · spread — and 120 px wide.
        let pill = Rect::new(400.0, 400.0, 520.0, 424.0);
        let at = render_shadow(1400, 900, pill, spread);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "spread is a small positive px constant in these tests, always well within u32 range"
        )]
        let steps = spread as u32 + 4;
        for k in 0..steps {
            let u = (f64::from(k) + 0.5) / spread;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "255.0 * shadow_profile(u) is within 0.0..=255.0"
            )]
            let want = (255.0 * shadow_profile(u)).round() as i32;
            let got = i32::from(at(pill.center().x, pill.y1 + f64::from(k)));
            assert!(
                (got - want).abs() <= 8,
                "{k} px below a {}-px-tall pill must read {want}/255 \
             (shadow_profile({u:.3})); got {got}",
                pill.height(),
            );
        }
    }
}

/// Sample the shadow outward from the middle of `rect`'s right and bottom edges, one
/// sample per pixel, starting half a pixel outside the edge.
fn outward_profiles(rect: Rect, spread: f64, span: u32) -> (Vec<u8>, Vec<u8>) {
    let at = render_shadow(1400, 900, rect, spread);
    let right = (0..span)
        .map(|k| at(rect.x1 + f64::from(k), rect.center().y))
        .collect();
    let bottom = (0..span)
        .map(|k| at(rect.center().x, rect.y1 + f64::from(k)))
        .collect();
    (right, bottom)
}
