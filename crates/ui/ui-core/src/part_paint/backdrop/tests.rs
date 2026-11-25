use super::*;
use crate::offscreen::OffscreenRenderer;
use crate::part_paint::wallpaper;
use vello::kurbo::{Point, RoundedRect, Size, Vec2};

const PANEL: Rect = Rect::new(64.0, 48.0, 192.0, 144.0);

fn panel() -> RoundedRect {
    RoundedRect::from_rect(PANEL, 12.0)
}

/// Eight-pixel black and white columns: the busiest backdrop there is, and one whose blur has a known
/// value.
fn stripes(width: u32, height: u32) -> Scene {
    let mut scene = Scene::new();
    for column in 0..width / 8 {
        let colour = if column % 2 == 0 {
            Color::BLACK
        } else {
            Color::WHITE
        };
        let x = f64::from(column * 8);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colour,
            None,
            &Rect::new(x, 0.0, x + 8.0, f64::from(height)),
        );
    }
    scene
}

fn floating(scene: &mut Scene, transform: Affine, theme: &Theme) {
    fill(scene, transform, &panel(), Material::Floating, theme);
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let (r, g, b, a) = OffscreenRenderer::sample(rgba, width, x, y);
    [r, g, b, a]
}

fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= tolerance)
}

tilo_test_env::scenario! {
    fn a_backdrop_recording_registers_floating_surfaces;

    phase outside_a_recording_the_material_is_the_opaque_panel_ground;
    {
        let mut scene = Scene::new();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &Theme::dark());
        assert!(
            scene.encoding().resources.patches.is_empty(),
            "no recording, so no placeholder image is drawn"
        );
    }

    phase a_recording_registers_each_floating_surface_with_what_is_below_it;
    {
        let theme = Theme::dark();
        let mut scene = stripes(256, 192);
        let below_len = scene.encoding().draw_tags.len();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 2);
        floating(&mut scene, Affine::translate((10.0, 5.0)), &theme);
        let mut scratch = Scene::new();
        floating(&mut scratch, Affine::IDENTITY, &theme);
        fill(
            &mut scene,
            Affine::IDENTITY,
            &panel(),
            Material::Opaque,
            &theme,
        );
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 1, "only the floating material records");
        let surface = &surfaces[0];
        assert_eq!(surface.layer, 2);
        assert_eq!(surface.bounds, PANEL + Vec2::new(10.0, 5.0));
        assert_eq!(surface.below.encoding().draw_tags.len(), below_len);
        assert_eq!((surface.image.width, surface.image.height), (64, 48));
        assert!(
            (surface.blur_px - f64::from(theme.tokens.colors.surfaces.material_blur_px)).abs()
                < f64::EPSILON
        );
        assert!(
            scratch.encoding().resources.patches.is_empty(),
            "an unnamed scene paints the opaque material"
        );
        let mut after = Scene::new();
        floating(&mut after, Affine::IDENTITY, &theme);
        assert!(after.encoding().resources.patches.is_empty());
    }

    phase reduced_transparency_and_forced_colours_keep_floating_chrome_opaque;
    {
        for theme in [
            Theme::dark().with_reduce_transparency(true),
            Theme::high_contrast(),
        ] {
            let mut scene = Scene::new();
            let recording = BackdropRecording::begin();
            layer_scene(&scene, 0);
            floating(&mut scene, Affine::IDENTITY, &theme);
            assert!(recording.finish().is_empty(), "{}", theme.name);
        }
    }

    phase a_frame_blurs_at_most_its_region_budget;
    {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        for _ in 0..MAX_BACKDROP_REGIONS + 2 {
            floating(&mut scene, Affine::IDENTITY, &theme);
        }
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), MAX_BACKDROP_REGIONS);
        let ids: std::collections::HashSet<u64> = surfaces
            .iter()
            .map(|surface| surface.image.data.id())
            .collect();
        assert_eq!(
            ids.len(),
            MAX_BACKDROP_REGIONS,
            "each region has its own placeholder"
        );
    }

    phase a_continued_recording_keeps_the_frames_regions_distinct;
    {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let first = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let main_pass = first.finish();
        let second = BackdropRecording::continuing(main_pass);
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let surfaces = second.finish();
        assert_eq!(surfaces.len(), 2);
        assert_ne!(
            surfaces[0].image.data.id(),
            surfaces[1].image.data.id(),
            "the second pass takes the next region slot"
        );
    }

    // A subtree cache (a `Column`'s) is drawn on its and appended later: a surface inside it takes
    // what the frame held before the cache, plus the cache's content under it, at the cache's place in the
    // frame.
    phase a_surface_in_a_nested_cache_lands_where_the_cache_is_appended;
    {
        let theme = Theme::dark();
        let mut frame = stripes(256, 192);
        let frame_len = frame.encoding().draw_tags.len();
        let recording = BackdropRecording::begin();
        layer_scene(&frame, 1);
        let mut outer = Scene::new();
        nest_scene(&outer);
        let mut inner = Scene::new();
        nest_scene(&inner);
        inner.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, 4.0, 4.0),
        );
        let inner_len = inner.encoding().draw_tags.len();
        floating(&mut inner, Affine::IDENTITY, &theme);
        assert_eq!(
            append_nested(&mut outer, &inner, Affine::translate((4.0, 2.0))),
            1
        );
        assert_eq!(
            append_nested(&mut frame, &outer, Affine::translate((6.0, 3.0))),
            1,
            "a cache inside a cache carries its child's surface up"
        );
        let mut lost = Scene::new();
        nest_scene(&lost);
        floating(&mut lost, Affine::IDENTITY, &theme);
        let mut scratch = Scene::new();
        assert_eq!(append_nested(&mut scratch, &lost, Affine::IDENTITY), 1);
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 1);
        let surface = &surfaces[0];
        assert_eq!(surface.layer, 1);
        assert_eq!(surface.bounds, PANEL + Vec2::new(10.0, 5.0));
        assert_eq!(
            surface.below.encoding().draw_tags.len(),
            frame_len + inner_len,
            "below is the frame so far plus the cache's own content"
        );
        let mut plain = Scene::new();
        assert_eq!(append_nested(&mut plain, &inner, Affine::IDENTITY), 0);
    }

    // The render service draws a backdrop's scene by itself and refuses a frame
    // whose backdrop leaves a clip open ("invalid scene: a clip is left open"),
    // which froze every window showing a menu, toast or notice drawn inside a
    // clip or a fade. What lies below a surface must pass the service
    // validator, wherever the surface was drawn.
    phase what_lies_below_a_surface_drawn_inside_open_layers_is_a_balanced_scene;
    {
        use tilo_render_protocol::Limits;
        use tilo_render_protocol::validate::validate;
        use vello::peniko::BlendMode;

        let theme = Theme::dark();
        let mut frame = stripes(256, 192);
        let recording = BackdropRecording::begin();
        layer_scene(&frame, 0);
        frame.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 256.0, 192.0),
        );
        frame.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            0.5,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 256.0, 192.0),
        );
        floating(&mut frame, Affine::IDENTITY, &theme);
        frame.pop_layer();
        let mut cache = Scene::new();
        nest_scene(&cache);
        cache.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 128.0, 96.0),
        );
        floating(&mut cache, Affine::IDENTITY, &theme);
        cache.pop_layer();
        append_nested(&mut frame, &cache, Affine::translate((8.0, 8.0)));
        frame.pop_layer();
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 2);
        validate(frame.encoding(), &Limits::default()).expect("the frame itself is balanced");
        for surface in &surfaces {
            assert_eq!(surface.below.encoding().n_open_clips, 0);
            validate(surface.below.encoding(), &Limits::default())
                .expect("the render service accepts what lies below a surface");
        }
    }

    phase a_nested_recording_never_shares_a_placeholder_with_its_host;
    {
        let theme = Theme::dark();
        let mut outer_scene = Scene::new();
        let mut inner_scene = Scene::new();
        let outer = BackdropRecording::begin();
        layer_scene(&outer_scene, 0);
        floating(&mut outer_scene, Affine::IDENTITY, &theme);
        let inner = BackdropRecording::begin();
        layer_scene(&inner_scene, 0);
        floating(&mut inner_scene, Affine::IDENTITY, &theme);
        let inner_surfaces = inner.finish();
        let outer_surfaces = outer.finish();
        assert_eq!((outer_surfaces.len(), inner_surfaces.len()), (1, 1));
        assert_ne!(
            outer_surfaces[0].image.data.id(),
            inner_surfaces[0].image.data.id(),
            "the host's blur must not be substituted into the inner surface"
        );
    }

    phase an_unchanged_surface_encodes_identically_frame_to_frame;
    {
        let theme = Theme::dark();
        let frame = || {
            let mut scene = Scene::new();
            let recording = BackdropRecording::begin();
            layer_scene(&scene, 0);
            floating(&mut scene, Affine::IDENTITY, &theme);
            drop(recording.finish());
            scene
        };
        assert!(frame().encoding_eq(&frame()));
    }
}

#[test]
fn the_blur_pass_shows_the_tinted_average_of_the_backdrop() {
    let theme = Theme::dark();
    let (width, height) = (256, 192);
    let mut scene = stripes(width, height);
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    floating(&mut scene, Affine::IDENTITY, &theme);
    let frame = BackdropFrame::from_single_scene(recording.finish());
    assert_eq!(frame.regions.len(), 1);

    let mut renderer = OffscreenRenderer::new(width, height);
    let opaque = renderer.render(&scene, Color::BLACK);
    renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
    let blurred = renderer.render(&scene, Color::BLACK);

    let ground = crate::chrome::panel_ground(&theme).to_rgba8().to_u8_array();
    let centre = (128, 96);
    assert!(
        close(pixel(&opaque, width, centre.0, centre.1), ground, 1),
        "without the pass the placeholder is the opaque ground"
    );
    let tint = f64::from(theme.tokens.colors.surfaces.material_opacity);
    let (dark_end, light_end) = material_band(&theme, crate::chrome::panel_ground(&theme));
    let (dark_end, light_end) = (
        dark_end.to_rgba8().to_u8_array(),
        light_end.to_rgba8().to_u8_array(),
    );
    let shown = Legible::of(&theme, crate::chrome::panel_ground(&theme)).tone([0.5; 3]);
    let mut channel_index = 0;
    let expected = ground.map(|channel| {
        let held = (shown.get(channel_index).copied().unwrap_or(1.0) * 255.0)
            .min(f64::from(light_end[channel_index]))
            .max(f64::from(dark_end[channel_index]));
        channel_index += 1;
        let mixed = f64::from(channel).mul_add(tint, held * (1.0 - tint));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a mix of two bytes is a byte"
        )]
        let mixed = mixed.round() as u8;
        mixed
    });
    let seen = pixel(&blurred, width, centre.0, centre.1);
    assert!(
        close(
            [seen[0], seen[1], seen[2], 255],
            [expected[0], expected[1], expected[2], 255],
            8
        ),
        "blurred centre {seen:?}, expected about {expected:?}"
    );
    // No stripe survives: across two stripe periods the panel varies by a
    // few levels at most.
    let row: Vec<u8> = (112..=144)
        .map(|x| pixel(&blurred, width, x, centre.1)[0])
        .collect();
    let spread = row.iter().max().unwrap() - row.iter().min().unwrap();
    assert!(
        spread <= 6,
        "stripes still visible through the material: {row:?}"
    );
    for (x, y) in [(8, 8), (20, 180), (250, 100)] {
        assert!(close(
            pixel(&opaque, width, x, y),
            pixel(&blurred, width, x, y),
            1
        ));
    }
}

tilo_test_env::scenario! {
    fn published_and_provided_backdrops_stand_in_for_the_placeholder;

    phase the_shells_ground_is_its_picture_blurred_as_one_image_fill;
    {
        let theme = Theme::dark();
        let ground = crate::chrome::panel_ground(&theme);
        let mut wallpaper = Vec::new();
        for y in 0..64_u32 {
            for x in 0..64_u32 {
                let v = if (x / 8 + y / 8) % 2 == 0 { 0 } else { 255 };
                wallpaper.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let wallpaper = crate::raster::rgba8(wallpaper, 64, 64);
        let source = wallpaper::Source::new();
        let draw = |theme: &Theme| {
            let mut scene = Scene::new();
            wallpaper::shell(&mut scene, &source, Affine::IDENTITY, &panel(), theme, ground);
            scene
        };
        assert!(
            draw(&theme).encoding().resources.patches.is_empty(),
            "no picture on the shell: opaque"
        );
        let display = panel().bounding_box();
        let blurred = wallpaper::blurred_cover(&wallpaper, display, 24.0).expect("pixels in memory");
        let again = wallpaper::blurred_cover(&wallpaper, display, 24.0).expect("cached");
        assert_eq!(
            blurred.0.data.id(),
            again.0.data.id(),
            "made once per wallpaper and size"
        );
        // Blurred by 24 px, 8 px checks average out to grey.
        let pixels = blurred.0.data.data();
        let mid = (blurred.0.height as usize / 2 * blurred.0.width as usize
            + blurred.0.width as usize / 2)
            * 4;
        let grey = pixels[mid];
        assert!((100..=155).contains(&grey), "the checks still show: {grey}");
        source.set(Some(wallpaper));
        let frosted = draw(&theme);
        assert_eq!(
            frosted.encoding().resources.patches.len(),
            1,
            "one image fill"
        );
        assert!(
            frosted.encoding_eq(&draw(&theme)),
            "the same shell encodes the same"
        );
        assert!(
            draw(&theme.clone().with_reduce_transparency(true))
                .encoding()
                .resources
                .patches
                .is_empty(),
            "reduced transparency is opaque"
        );
    }

    phase the_shells_ground_is_its_gradient_where_it_shows_no_picture;
    {
        let theme = Theme::dark();
        let ground = crate::chrome::panel_ground(&theme);
        let source = wallpaper::Source::new();
        let draw = |theme: &Theme| {
            let mut scene = Scene::new();
            wallpaper::shell(&mut scene, &source, Affine::IDENTITY, &panel(), theme, ground);
            scene
        };
        let ramps = |scene: &Scene| {
            scene
                .encoding()
                .resources
                .patches
                .iter()
                .filter(|patch| matches!(patch, crate::vello_encoding::Patch::Ramp { .. }))
                .count()
        };
        source.set_gradient((Color::from_rgb8(30, 40, 60), Color::from_rgb8(10, 12, 20)));
        assert_eq!(
            ramps(&draw(&theme)),
            1,
            "the shell's gradient shows through, not a flat ground"
        );
        assert_eq!(
            ramps(&draw(&theme.clone().with_reduce_transparency(true))),
            0,
            "reduced transparency is opaque"
        );
        source.set(None);
        assert_eq!(ramps(&draw(&theme)), 0, "a shell showing nothing: opaque");
    }

    phase a_provided_backdrop_of_the_right_size_is_drawn_in_place_of_the_placeholder;
    {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let (width, height) = recording.finish()[0].texels();
        assert_eq!((width, height), (64, 48));
        let blurred = crate::raster::rgba8_premultiplied(
            [200_u8, 10, 10, 255].repeat((width * height) as usize),
            width,
            height,
        );
        provide_backdrops(vec![(0, blurred.clone())]);
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 1);
        assert_eq!(
            surfaces[0].image.data.id(),
            blurred.data.id(),
            "the provided image, not the opaque placeholder"
        );

        provide_backdrops(vec![(
            0,
            crate::raster::rgba8_premultiplied(vec![0; 4], 1, 1),
        )]);
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let surfaces = recording.finish();
        assert_ne!(surfaces[0].image.data.id(), blurred.data.id());
        assert_eq!(
            (surfaces[0].image.width, surfaces[0].image.height),
            (64, 48)
        );
        provide_backdrops(Vec::new());
    }

    // A cell-sampled surface (Pixelated) takes one texel per cell, and the
    // size a host is told to provide is that size: a host that hands back an
    // image at any other size leaves the surface showing its opaque
    // placeholder, so its cells never appear — which is what the live shell
    // did while the request was sized for a smooth sample.
    phase a_blocky_surface_asks_for_one_texel_per_cell;
    {
        let theme = crate::theme_document::finish_theme("pixelated");
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 1, "the panel samples");
        let surface = &surfaces[0];
        assert_eq!(
            surface.sampling,
            Sampling::Blocky,
            "Pixelated's floating surface is shown as cells"
        );
        let cell = surface.cell_px;
        assert!(cell >= 2.0, "a cell of {cell} px shows no pixels");
        let requested = surface.texels();
        assert_eq!(
            requested,
            (
                cells(surface.bounds.width(), cell),
                cells(surface.bounds.height(), cell)
            ),
            "the texels the host is asked for are one per cell"
        );
        let smooth = (texels(surface.bounds.width()), texels(surface.bounds.height()));
        assert!(
            requested.0 * 2 <= smooth.0,
            "one texel per {cell} px cell ({requested:?}) is coarser than a smooth \
             sample ({smooth:?})"
        );
        let image = crate::raster::rgba8_premultiplied(
            [90_u8, 140, 200, 255].repeat((requested.0 * requested.1) as usize),
            requested.0,
            requested.1,
        );
        provide_backdrops(vec![(0, image.clone())]);
        let mut scene = Scene::new();
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        floating(&mut scene, Affine::IDENTITY, &theme);
        let surfaces = recording.finish();
        assert_eq!(
            surfaces[0].image.data.id(),
            image.data.id(),
            "the provided cells are drawn, not the opaque placeholder"
        );
        provide_backdrops(Vec::new());
    }
}
fn render_floating_over_stripes(theme: &Theme) -> Vec<u8> {
    let (width, height) = (256, 192);
    let mut scene = stripes(width, height);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::BLACK,
        None,
        &Rect::new(0.0, 0.0, 80.0, f64::from(height)),
    );
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    crate::surface::panel(&mut scene, Affine::IDENTITY, PANEL, 12.0, theme);
    let frame = BackdropFrame::from_single_scene(recording.finish());
    let mut renderer = OffscreenRenderer::new(width, height);
    renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
    renderer.render(&scene, Color::BLACK)
}

fn glass_theme(refraction: f32) -> Theme {
    let mut document = crate::theme_document::finish_document("glass");
    document.material.floating =
        crate::tokens::Finish::glass([4.0, 0.2, 1.0, refraction, 0.5, 0.0]);
    Theme::from_document(&document).unwrap()
}

fn brightness(p: [u8; 4]) -> u16 {
    u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])
}

/// A glass panel bends what is under it at its rim and leaves its middle as the blur shows it, lights
/// its top edge, and has every pixel of its body inside the band ink is measured against.
#[test]
fn a_glass_panel_bends_and_lights_its_rim_and_stays_legible() {
    let width = 256;
    let flat = render_floating_over_stripes(&glass_theme(0.0));
    let bent = render_floating_over_stripes(&glass_theme(16.0));
    let centre = (128, 96);
    assert!(close(
        pixel(&flat, width, centre.0, centre.1),
        pixel(&bent, width, centre.0, centre.1),
        2
    ));
    let moved = (66..90).any(|x| {
        !close(
            pixel(&flat, width, x, centre.1),
            pixel(&bent, width, x, centre.1),
            6,
        )
    });
    assert!(moved, "the rim shows the backdrop where it was");

    let theme = glass_theme(16.0);
    let top = pixel(&bent, width, 128, 48);
    let below = pixel(&bent, width, 128, 51);
    assert!(
        brightness(top) > brightness(below),
        "lit rim {top:?} over body {below:?}"
    );

    let ink = theme.tokens.colors.text_dim;
    for y in (54..140).step_by(4) {
        for x in (70..186).step_by(3) {
            let [r, g, b, _] = pixel(&bent, width, x, y);
            let seen = Color::from_rgba8(r, g, b, 255);
            let ratio = crate::color::contrast_ratio(ink, seen);
            assert!(
                ratio >= crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST - 0.1,
                "text_dim on the glass at ({x}, {y}) is {ratio:.2}:1"
            );
        }
    }
}

/// The Liquid Glass preset with its floating finish bending `refraction`
/// across a 22 px band, unblurred and with no rim, so only the bend shows.
fn liquid_theme(refraction: f32, dispersion: f32) -> Theme {
    let mut document = crate::theme_document::finish_document("liquid-glass");
    document.material.floating = crate::tokens::Finish::new(
        crate::part_paint::builtin::liquid_set().clone(),
        [
            ("tint", 0.16),
            ("refraction", refraction),
            ("band", 22.0),
            ("dispersion", dispersion),
            ("edge_highlight", 0.0),
        ],
    )
    .unwrap();
    Theme::from_document(&document).unwrap()
}

tilo_test_env::scenario! {
    fn a_surface_samples_only_what_its_finish_asks_for;

    // A lens moves what a pixel shows inward along the outline's normal: by nearly its whole strength at
    // the rim, easing to nothing at the band's inner side, and not at all in the flat middle.
    phase a_lens_displaces_at_the_rim_and_leaves_the_middle_straight;
    {
        let lens = Refraction {
            strength: 10.0,
            band: 20.0,
            radius: 12.0,
            dispersion: 0.0,
        };
        let size = Size::new(200.0, 100.0);
        let at = |x: f64, y: f64| lens.displacement(Point::new(x, y), size);
        let near = |a: Vec2, b: Vec2| (a - b).hypot() < 1e-9;
        // Half a pixel in from the left rim: 0.975² of the strength, inward.
        assert!(near(at(0.5, 50.0), Vec2::new(10.0 * 0.975 * 0.975, 0.0)));
        assert!(near(at(199.5, 50.0), Vec2::new(-10.0 * 0.975 * 0.975, 0.0)));
        assert!(near(at(100.0, 0.5), Vec2::new(0.0, 10.0 * 0.975 * 0.975)));
        assert!(near(at(10.0, 50.0), Vec2::new(2.5, 0.0)));
        assert!(near(at(25.0, 50.0), Vec2::ZERO));
        assert!(near(at(100.0, 50.0), Vec2::ZERO));
        let corner = at(3.0, 3.0);
        assert!(
            corner.x > 0.0 && (corner.x - corner.y).abs() < 1e-9,
            "{corner:?}"
        );
        // No strength, no bend; a lens with no band eases across 2.5 times it.
        assert!(near(
            Refraction::NONE.displacement(Point::new(0.5, 50.0), size),
            Vec2::ZERO
        ));
        let lens = Lens {
            refraction: 8.0,
            ..Lens::default()
        };
        assert!((Refraction::of(lens, 4.0).band - 20.0).abs() < 1e-9);
        let doubled = Refraction {
            dispersion: 0.2,
            ..Refraction::of(lens, 4.0)
        }
        .scaled(2.0);
        assert!((doubled.strength - 16.0).abs() < 1e-9 && (doubled.radius - 8.0).abs() < 1e-9);
        assert!((doubled.dispersion - 0.2).abs() < 1e-9);
    }

    // A Liquid panel records its lens with the backdrop it samples, in the recording's pixels, and a
    // frame's region has it to the pass.
    phase a_liquid_panel_records_its_lens_with_its_backdrop;
    {
        let theme = liquid_theme(14.0, 0.1);
        let mut scene = stripes(256, 192);
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        crate::surface::panel(&mut scene, Affine::IDENTITY, PANEL, 12.0, &theme);
        let surfaces = recording.finish();
        assert_eq!(surfaces.len(), 1);
        let refraction = surfaces[0].refraction;
        assert!(refraction.bends());
        assert!((refraction.strength - 14.0).abs() < 1e-6, "{refraction:?}");
        assert!((refraction.band - 22.0).abs() < 1e-6);
        assert!((refraction.radius - 12.0).abs() < 1e-6);
        assert!((refraction.dispersion - 0.1).abs() < 1e-6);
        let region = surfaces
            .into_iter()
            .next()
            .unwrap()
            .into_region(PANEL, 2.0, |below| below);
        assert!((region.refraction.strength - 28.0).abs() < 1e-6);
        assert!((region.refraction.band - 44.0).abs() < 1e-6);
        let mut scene = stripes(256, 192);
        let recording = BackdropRecording::begin();
        layer_scene(&scene, 0);
        crate::surface::panel(
            &mut scene,
            Affine::IDENTITY,
            PANEL,
            12.0,
            &liquid_theme(0.0, 0.0),
        );
        let surfaces = recording.finish();
        assert_eq!(
            surfaces.len(),
            1,
            "an unblurred lens of nothing still samples"
        );
        assert!(!surfaces[0].refraction.bends());
    }

    // Glass and Pixelated keep a window's ground their fill: only their floating chrome and bars sample
    // what lies below.
    phase a_glass_or_pixelated_window_ground_samples_nothing;
    {
        for finish in ["glass", "pixelated"] {
            let theme = crate::theme_document::finish_theme(finish);
            let mut scene = stripes(256, 192);
            let recording = BackdropRecording::begin();
            layer_scene(&scene, 0);
            let window = RoundedRect::from_rect(Rect::new(8.0, 30.0, 120.0, 186.0), 12.0);
            let ground = theme.tokens.colors.surfaces.content;
            fill_role(
                &mut scene,
                Affine::IDENTITY,
                &window,
                SurfaceRole::Window,
                &theme,
                ground,
            );
            assert!(recording.finish().is_empty(), "{finish}'s window sampled");
        }
    }

    phase the_legible_range_compresses_luminance_and_keeps_hue;
    {
        let theme = crate::theme_document::finish_theme("liquid-glass");
        let legible = Legible::of(&theme, crate::chrome::panel_ground(&theme));
        assert!(legible.compresses());
        let luminance = |[r, g, b]: [f64; 3]| {
            crate::color::relative_luminance(Color::new([r as f32, g as f32, b as f32, 1.0]))
        };
        let close = |a: f64, b: f64| (a - b).abs() < 2e-3;
        assert!(close(luminance(legible.tone([1.0; 3])), legible.light));
        assert!(close(luminance(legible.tone([0.0; 3])), legible.dark));
        assert!(close(legible.aim(legible.ground), legible.ground));
        let dark = Theme::dark();
        let menu = Legible::of(&dark, crate::chrome::panel_ground(&dark));
        let page = dark.tokens.colors.surfaces.content;
        let [r, g, b, _] = page.components;
        let shown = luminance(menu.tone([r, g, b].map(f64::from)));
        assert!(
            close(shown, crate::color::relative_luminance(page)),
            "a dark window under a dark menu shows at {shown}, not as itself"
        );
        let greys: Vec<f64> = (0..=10)
            .map(|step| luminance(legible.tone([f64::from(step) / 10.0; 3])))
            .collect();
        assert!(greys.windows(2).all(|pair| pair[0] < pair[1]), "{greys:?}");
        let [r, g, b] = legible.tone([0.9, 0.1, 0.1]);
        assert!(r > 2.0 * g && r > 2.0 * b, "red became {r} {g} {b}");
        let [r, g, b] = legible.tone([0.1, 0.8, 0.2]);
        assert!(g > 2.0 * r && g > 2.0 * b, "green became {r} {g} {b}");
        assert_eq!(Legible::ANY.tone([0.9, 0.1, 0.3]), [0.9, 0.1, 0.3]);
        let light = crate::theme_document::finish_theme("liquid-glass-light");
        let lifted = Legible::of(&light, crate::chrome::panel_ground(&light));
        let [r, g, b] = lifted.tone([14.0 / 255.0, 16.0 / 255.0, 22.0 / 255.0]);
        assert!(b - r < 0.03 && b - g < 0.03, "the page turned {r} {g} {b}");
        let [r, g, b] = lifted.tone([0.9, 0.1, 0.1]);
        assert!(r > g + 0.1 && r > b + 0.1, "red became {r} {g} {b}");
    }
}

#[test]
fn the_shipped_liquid_glass_shows_what_lies_below() {
    let width = 256;
    let theme = crate::theme_document::finish_theme("liquid-glass");
    let seen = render_floating_over_stripes(&theme);
    let row: Vec<u16> = (100..160)
        .map(|x| brightness(pixel(&seen, width, x, 96)))
        .collect();
    let spread = row.iter().max().unwrap() - row.iter().min().unwrap();
    assert!(spread >= 20, "the panel shows nothing below: {row:?}");
}

/// The bar, a window and a panel over it, under Liquid Glass in one frame, all show the stripes below
/// them: every region of the frame is substituted, not only the first, and the window's ground is glass
/// too.
#[test]
fn a_liquid_bar_window_and_panel_in_one_frame_all_show_what_lies_below() {
    let (width, height) = (256, 192);
    let theme = crate::theme_document::finish_theme("liquid-glass");
    let ground = theme.tokens.colors.surfaces.base;
    let mut scene = stripes(width, height);
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    let bar = RoundedRect::from_rect(Rect::new(0.0, 0.0, 256.0, 24.0), 0.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &bar,
        SurfaceRole::Bar,
        &theme,
        ground,
    );
    let window = RoundedRect::from_rect(Rect::new(8.0, 30.0, 120.0, 186.0), 12.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &window,
        SurfaceRole::Window,
        &theme,
        ground,
    );
    crate::surface::panel(&mut scene, Affine::IDENTITY, PANEL, 12.0, &theme);
    let surfaces = recording.finish();
    assert_eq!(
        surfaces.len(),
        3,
        "the bar, the window and the panel sample"
    );
    let frame = BackdropFrame::from_single_scene(surfaces);
    let mut renderer = OffscreenRenderer::new(width, height);
    renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
    let seen = renderer.render(&scene, Color::BLACK);
    let spread = |xs: std::ops::Range<u32>, y: u32| {
        let row: Vec<u16> = xs.map(|x| brightness(pixel(&seen, width, x, y))).collect();
        row.iter().max().unwrap() - row.iter().min().unwrap()
    };
    assert!(spread(40..120, 12) >= 12, "the bar shows nothing below");
    assert!(spread(20..60, 120) >= 12, "the window shows nothing below");
    assert!(spread(130..180, 100) >= 12, "the panel shows nothing below");
}

fn colour_stripes(width: u32, height: u32) -> Scene {
    let colours = [
        Color::from_rgb8(230, 40, 40),
        Color::from_rgb8(40, 200, 60),
        Color::from_rgb8(40, 80, 230),
        Color::from_rgb8(250, 220, 40),
        Color::WHITE,
    ];
    let mut scene = Scene::new();
    for column in 0..width.div_ceil(16) {
        let x = f64::from(column * 16);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            colours[column as usize % colours.len()],
            None,
            &Rect::new(x, 0.0, x + 16.0, f64::from(height)),
        );
    }
    scene
}

fn white_page(width: u32, height: u32) -> Scene {
    let mut scene = Scene::new();
    let page = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    scene.fill(Fill::NonZero, Affine::IDENTITY, Color::WHITE, None, &page);
    for line in 0..height / 12 {
        let y = f64::from(line * 12 + 4);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(20, 20, 24),
            None,
            &Rect::new(6.0, y, f64::from(width) - 30.0, y + 4.0),
        );
    }
    scene
}

fn render_over(theme: &Theme, mut scene: Scene) -> Vec<u8> {
    let (width, height) = (256, 192);
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    let window = RoundedRect::from_rect(Rect::new(4.0, 4.0, 60.0, 188.0), 12.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &window,
        SurfaceRole::Window,
        theme,
        theme.tokens.colors.surfaces.content,
    );
    crate::surface::panel(&mut scene, Affine::IDENTITY, PANEL, 12.0, theme);
    let frame = BackdropFrame::from_single_scene(recording.finish());
    let mut renderer = OffscreenRenderer::new(width, height);
    renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
    renderer.render(&scene, Color::BLACK)
}

/// Over a white page and over bright colour, text on every translucent finish's panel (and, under
/// Liquid Glass, on its window) keeps the theme's contrast floor at every pixel, while what lies below
/// still shows: compressed, not flattened.
#[test]
fn text_over_a_white_page_and_bright_colour_stays_legible_on_every_translucent_finish() {
    let width = 256;
    let floor = crate::tokens::contrast::BODY_TEXT_MIN_CONTRAST - 0.1;
    for finish in ["liquid-glass", "liquid-glass-light", "glass", "pixelated"] {
        let theme = crate::theme_document::finish_theme(finish);
        let ink = theme.tokens.colors.text_dim;
        for (name, below) in [
            ("white page", white_page(256, 192)),
            ("colour", colour_stripes(256, 192)),
        ] {
            let seen = render_over(&theme, below);
            let mut areas = vec![("panel", (70..186, 54..140))];
            if finish.starts_with("liquid") {
                areas.push(("window", (10..54, 20..170)));
            }
            for (area, (xs, ys)) in areas {
                for y in ys.step_by(3) {
                    for x in xs.clone().step_by(3) {
                        let [r, g, b, _] = pixel(&seen, width, x, y);
                        let ratio =
                            crate::color::contrast_ratio(ink, Color::from_rgba8(r, g, b, 255));
                        assert!(
                            ratio >= floor,
                            "{finish} {area} over the {name} at ({x}, {y}): {ratio:.2}:1"
                        );
                    }
                }
            }
        }
        let seen = render_over(&theme, colour_stripes(256, 192));
        let row: Vec<u16> = (100..160)
            .map(|x| brightness(pixel(&seen, width, x, 96)))
            .collect();
        let spread = row.iter().max().unwrap() - row.iter().min().unwrap();
        assert!(spread >= 6, "{finish} flattens the colour: {row:?}");
    }
}

#[test]
fn liquid_glass_keeps_the_colour_of_what_lies_below() {
    let width = 256;
    let theme = crate::theme_document::finish_theme("liquid-glass");
    let seen = render_over(&theme, colour_stripes(256, 192));
    // Columns are 16 px: red at 80..96, green at 96..112 (column 5 and 6
    // of five colours repeating: 5 % 5 = red, 6 % 5 = green).
    let [r, g, _, _] = pixel(&seen, width, 88, 96);
    assert!(r > g + 12, "red shows as {r} {g}");
    let [r, g, _, _] = pixel(&seen, width, 104, 96);
    assert!(g > r + 12, "green shows as {r} {g}");
}

/// Over the stripes, a Liquid panel's rim shows what lies further in (the pixel at its right rim shows
/// a stripe from about a lens's strength in), its middle shows the stripes exactly as an unbent panel
/// does, sharp, and dispersion sets red and blue apart where the stripes' edges bend.
#[test]
fn a_liquid_panel_bends_its_rim_and_leaves_its_middle_sharp() {
    let width = 256;
    let flat = render_floating_over_stripes(&liquid_theme(0.0, 0.0));
    let bent = render_floating_over_stripes(&liquid_theme(14.0, 0.0));
    let row = 96;
    for x in [120, 124, 128, 132] {
        assert!(
            close(pixel(&flat, width, x, row), pixel(&bent, width, x, row), 2),
            "the lens reached the middle at {x}"
        );
    }
    let (white, black) = (pixel(&bent, width, 124, row), pixel(&bent, width, 132, row));
    assert!(
        brightness(white) > brightness(black) + 60,
        "the stripes are blurred away: {white:?} {black:?}"
    );
    assert!(close(pixel(&bent, width, 121, row), white, 2));
    assert!(close(pixel(&bent, width, 134, row), black, 2));
    // At the right rim (x = 192) the stripes are bent: the pixel just
    // inside shows what the unbent panel shows about twelve pixels in.
    let rim = pixel(&bent, width, 190, row);
    assert!(
        !close(rim, pixel(&flat, width, 190, row), 6),
        "the rim shows the backdrop where it was: {rim:?}"
    );
    assert!(
        (176..=181).any(|x| close(rim, pixel(&flat, width, x, row), 8)),
        "the rim shows {rim:?}, nothing from a lens's strength in"
    );
    let split = render_floating_over_stripes(&liquid_theme(14.0, 0.4));
    let warmth = |seen: &[u8], x: u32| {
        let [r, _, b, _] = pixel(seen, width, x, row);
        i16::from(r) - i16::from(b)
    };
    let fringe = (166..192).any(|x| (warmth(&split, x) - warmth(&bent, x)).abs() > 12);
    assert!(fringe, "no colour fringe at the rim");
    for x in [120, 124, 128, 132] {
        assert!(
            close(pixel(&split, width, x, row), pixel(&bent, width, x, row), 2),
            "the middle is fringed at {x}"
        );
    }
}

#[test]
fn a_block_panel_is_flat_and_bordered() {
    let width = 256;
    let theme = crate::theme_document::finish_theme("block");
    let seen = render_floating_over_stripes(&theme);
    let ground = crate::chrome::panel_ground(&theme).to_rgba8().to_u8_array();
    assert!(close(pixel(&seen, width, 128, 96), ground, 1));
    assert!(close(pixel(&seen, width, 100, 60), ground, 1));
    let border = theme.tokens.colors.text_tertiary.to_rgba8().to_u8_array();
    assert!(
        close(pixel(&seen, width, 128, 48), border, 2),
        "the top row is the border: {:?} vs {border:?}",
        pixel(&seen, width, 128, 48)
    );
    let stripes_only = {
        let mut renderer = OffscreenRenderer::new(width, 192);
        renderer.render(&stripes(width, 192), Color::BLACK)
    };
    for (x, y) in [(128, 150), (128, 160), (200, 96)] {
        assert!(close(
            pixel(&seen, width, x, y),
            pixel(&stripes_only, width, x, y),
            1
        ));
    }
}

/// Pixelated's floating panel shows what lies below it as cells: one colour across each cell, and the
/// stripes still there from cell to cell, not blurred away and not the opaque placeholder.
#[test]
fn a_pixelated_panel_shows_what_lies_below_as_cells() {
    let width = 256;
    let theme = crate::theme_document::finish_theme("pixelated");
    let seen = render_floating_over_stripes(&theme);
    let cell = theme
        .finish(crate::tokens::SurfaceRole::Floating)
        .params()
        .iter()
        .find(|param| param.name == "cell")
        .map(|param| param.value)
        .expect("the pixelated floating finish states its cell");
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the shipped cell is a few whole pixels"
    )]
    let cell = cell as u32;
    assert!(cell >= 2, "a cell of {cell} px shows no pixels");
    // The panel's cells start at its top left (64, 48). Inside one cell,
    // away from the rim, every pixel is the same colour.
    let (x0, y0) = (64 + 6 * cell, 48 + 4 * cell);
    for (dx, dy) in [(1, 0), (0, 1), (cell - 1, cell - 1)] {
        assert!(
            close(
                pixel(&seen, width, x0, y0),
                pixel(&seen, width, x0 + dx, y0 + dy),
                2
            ),
            "one cell, two colours: {:?} vs {:?}",
            pixel(&seen, width, x0, y0),
            pixel(&seen, width, x0 + dx, y0 + dy)
        );
    }
    let row: Vec<u16> = (0..12)
        .map(|i| brightness(pixel(&seen, width, 96 + i * cell + 1, y0)))
        .collect();
    let spread = row.iter().max().unwrap() - row.iter().min().unwrap();
    assert!(
        spread >= 6,
        "the cells show nothing of the stripes: {row:?}"
    );
    let ground = crate::chrome::panel_ground(&theme).to_rgba8().to_u8_array();
    assert!(
        row.iter()
            .any(|value| value.abs_diff(brightness(ground)) > 6),
        "the cells are the placeholder's flat ground: {row:?}"
    );
}

fn liquid_backdrop(window_x: f64, mark: Option<Rect>) -> (Scene, BackdropFrame) {
    let theme = crate::theme_document::finish_theme("liquid-glass");
    let ground = theme.tokens.colors.surfaces.base;
    let mut scene = stripes(256, 192);
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    let bar = RoundedRect::from_rect(Rect::new(0.0, 0.0, 256.0, 24.0), 0.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &bar,
        SurfaceRole::Bar,
        &theme,
        ground,
    );
    let window = RoundedRect::from_rect(Rect::new(window_x, 30.0, window_x + 112.0, 186.0), 12.0);
    fill_role(
        &mut scene,
        Affine::IDENTITY,
        &window,
        SurfaceRole::Window,
        &theme,
        ground,
    );
    if let Some(mark) = mark {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(220, 30, 30),
            None,
            &mark,
        );
    }
    crate::surface::panel(&mut scene, Affine::IDENTITY, PANEL, 12.0, &theme);
    (scene, BackdropFrame::from_single_scene(recording.finish()))
}

/// A frame whose backdrops have not changed shows every region from the sample it kept, with no pass; a
/// region whose backdrop changed (a window moved, something drawn under the panel) is taken anew, the
/// others kept, and the frame matches one rendered from scratch.
#[test]
fn an_unchanged_backdrop_is_shown_from_the_sample_it_kept() {
    let (width, height) = (256, 192);
    let mut renderer = OffscreenRenderer::new(width, height);
    let show = |renderer: &mut OffscreenRenderer, (scene, frame): (Scene, BackdropFrame)| {
        renderer.set_backdrop(Some(std::sync::Arc::new(frame)));
        let pixels = renderer.render(&scene, Color::BLACK);
        (pixels, renderer.backdrop_stats())
    };
    let fresh = |backdrop: (Scene, BackdropFrame)| {
        let mut renderer = OffscreenRenderer::new(width, height);
        renderer.set_backdrop(Some(std::sync::Arc::new(backdrop.1)));
        renderer.render(&backdrop.0, Color::BLACK)
    };
    let (first, stats) = show(&mut renderer, liquid_backdrop(8.0, None));
    assert_eq!((stats.substituted, stats.reused), (3, 0));
    let (again, stats) = show(&mut renderer, liquid_backdrop(8.0, None));
    assert_eq!(
        (stats.substituted, stats.reused),
        (3, 3),
        "an idle frame ran a pass"
    );
    assert_eq!(first, again);

    let mark = Some(Rect::new(120.0, 60.0, 180.0, 120.0));
    let (marked, stats) = show(&mut renderer, liquid_backdrop(8.0, mark));
    assert_eq!((stats.substituted, stats.reused), (3, 2));
    assert_eq!(marked, fresh(liquid_backdrop(8.0, mark)));

    let (moved, stats) = show(&mut renderer, liquid_backdrop(20.0, mark));
    assert_eq!((stats.substituted, stats.reused), (3, 1));
    assert_eq!(moved, fresh(liquid_backdrop(20.0, mark)));
}

#[test]
fn light_liquid_glass_is_clear_and_bright() {
    let width = 256;
    let theme = crate::theme_document::finish_theme("liquid-glass-light");
    let seen = render_over(&theme, white_page(256, 192));
    // Rows 4..8 of each 12 carry a line; row 10 is paper.
    let paper = pixel(&seen, width, 128, 94);
    assert!(
        paper[..3].iter().all(|&c| c >= 220),
        "the panel over paper is {paper:?}"
    );
    let line = pixel(&seen, width, 128, 90);
    assert!(
        brightness(paper) > brightness(line) + 60,
        "the page's lines are gone: {line:?} on {paper:?}"
    );
    let seen = render_over(&theme, colour_stripes(256, 192));
    let [r, g, b, _] = pixel(&seen, width, 88, 96);
    assert!(r > g + 12 && r > b + 12, "red shows as {r} {g} {b}");
}

/// A menu over a window of the dark theme stands off the window by its own tint: the window
/// lies near the panel's ground, so the pass shows it as it is, and the panel comes out the
/// mix of its ground and the window, never darker than the ground over a page just below it.
#[test]
fn a_dark_panel_over_its_window_shows_its_tint_over_the_window() {
    let theme = Theme::dark();
    let (width, height) = (256, 192);
    let page = theme.tokens.colors.surfaces.content;
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        page,
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 0);
    floating(&mut scene, Affine::IDENTITY, &theme);
    let mut renderer = OffscreenRenderer::new(width, height);
    renderer.set_backdrop(Some(std::sync::Arc::new(BackdropFrame::from_single_scene(
        recording.finish(),
    ))));
    let seen = pixel(&renderer.render(&scene, Color::BLACK), width, 128, 96);
    let tint = f64::from(theme.tokens.colors.surfaces.material_opacity);
    let ground = crate::chrome::panel_ground(&theme).to_rgba8().to_u8_array();
    let page = page.to_rgba8().to_u8_array();
    let mut channel = 0;
    let expected = ground.map(|g| {
        let mixed = f64::from(g).mul_add(tint, f64::from(page[channel]) * (1.0 - tint));
        channel += 1;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a mix of two bytes is a byte"
        )]
        let mixed = mixed.round() as u8;
        mixed
    });
    assert!(
        close(
            [seen[0], seen[1], seen[2], 255],
            [expected[0], expected[1], expected[2], 255],
            2
        ),
        "the panel over the window is {seen:?}, its tint over the window {expected:?}"
    );
}

/// White below a translucent surface whose ground sits just under white shows white: the band's
/// light end is reached exactly, and the pass never carries it past white into a flipped colour.
#[test]
fn white_below_a_near_white_window_shows_white_through_the_pass() {
    let theme = crate::theme_document::finish_theme("liquid-glass-light");
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::WHITE,
        None,
        &Rect::new(0.0, 0.0, 256.0, 192.0),
    );
    let seen = render_over(&theme, scene);
    for (x, y) in [(32, 96), (10, 20)] {
        let [r, g, b, _] = pixel(&seen, 256, x, y);
        assert!(
            r.min(g).min(b) >= 240,
            "the window over white at ({x}, {y}) is {r} {g} {b}"
        );
    }
}
