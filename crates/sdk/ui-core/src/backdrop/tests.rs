use super::*;
use crate::offscreen::OffscreenRenderer;
use vello::kurbo::{RoundedRect, Vec2};

const PANEL: Rect = Rect::new(64.0, 48.0, 192.0, 144.0);

fn panel() -> RoundedRect {
    RoundedRect::from_rect(PANEL, 12.0)
}

/// Eight-pixel black and white columns: the busiest backdrop there is, and
/// one whose blur has a known value.
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

#[test]
fn outside_a_recording_the_material_is_the_opaque_panel_ground() {
    let mut scene = Scene::new();
    layer_scene(&scene, 0);
    floating(&mut scene, Affine::IDENTITY, &Theme::dark());
    assert!(
        scene.encoding().resources.patches.is_empty(),
        "no recording, so no placeholder image is drawn"
    );
}

#[test]
fn a_recording_registers_each_floating_surface_with_what_is_below_it() {
    let theme = Theme::dark();
    let mut scene = stripes(256, 192);
    let below_len = scene.encoding().draw_tags.len();
    let recording = BackdropRecording::begin();
    layer_scene(&scene, 2);
    floating(&mut scene, Affine::translate((10.0, 5.0)), &theme);
    // A scratch scene nobody named has no known backdrop.
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
    // Once the recording ends, surfaces paint opaque again.
    let mut after = Scene::new();
    floating(&mut after, Affine::IDENTITY, &theme);
    assert!(after.encoding().resources.patches.is_empty());
}

#[test]
fn reduced_transparency_and_forced_colours_keep_floating_chrome_opaque() {
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

#[test]
fn a_frame_blurs_at_most_its_region_budget() {
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

#[test]
fn an_unchanged_surface_encodes_identically_frame_to_frame() {
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

/// The floating material over high-contrast stripes: the stripes are gone,
/// and what is left is the panel ground at the tint's opacity over the
/// stripes' average. Without the blur pass the same scene is the opaque
/// panel ground.
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
    renderer.set_backdrop(Some(Arc::new(frame)));
    let blurred = renderer.render(&scene, Color::BLACK);

    let ground = crate::chrome::panel_ground(&theme).to_rgba8().to_u8_array();
    let centre = (128, 96);
    assert!(
        close(pixel(&opaque, width, centre.0, centre.1), ground, 1),
        "without the pass the placeholder is the opaque ground"
    );
    let tint = f64::from(theme.tokens.colors.surfaces.material_opacity);
    let expected = ground.map(|channel| {
        let mixed = f64::from(channel).mul_add(tint, 127.5 * (1.0 - tint));
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
    // Outside the panel nothing changes.
    for (x, y) in [(8, 8), (20, 180), (250, 100)] {
        assert!(close(
            pixel(&opaque, width, x, y),
            pixel(&blurred, width, x, y),
            1
        ));
    }
}

/// Median wall time, GPU completion included, of `run` over `samples`
/// iterations after a warm-up.
fn median_us(device: &wgpu::Device, samples: usize, mut run: impl FnMut()) -> u128 {
    for _ in 0..5 {
        run();
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }
    let mut times: Vec<u128> = (0..samples)
        .map(|_| {
            let started = crate::time::now();
            run();
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            started.elapsed().as_micros()
        })
        .collect();
    times.sort_unstable();
    times[times.len() / 2]
}

/// What one blurred surface costs on this machine's adapter at 1080p: a
/// quick-settings-sized panel over a busy frame, at the stock blur radius.
/// Reports the backdrop render and the blur passes separately.
///
/// `cargo test -p tilo-ui --lib backdrop::tests::cost_at_1080p -- --ignored --nocapture`
#[test]
#[ignore = "a timing report for the machine it runs on, not a check"]
fn cost_at_1080p() {
    let gpu = crate::offscreen::shared_gpu().expect("a wgpu adapter");
    let mut renderer = Renderer::new(
        &gpu.device,
        vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::all(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello renderer");
    let theme = Theme::dark();
    let target = (1920, 1080);
    let rect = Rect::new(1480.0, 56.0, 1900.0, 776.0);
    let ground = crate::chrome::panel_ground(&theme);
    let region = BackdropRegion {
        image: placeholder(0, texels(rect.width()), texels(rect.height()), ground),
        rect,
        blur_px: f64::from(theme.tokens.colors.surfaces.material_blur_px),
        below: stripes(target.0, target.1),
        ground,
    };
    let frame = BackdropFrame {
        regions: vec![region.clone()],
    };
    let target_rect = Rect::new(0.0, 0.0, f64::from(target.0), f64::from(target.1));
    let plan = plan(&region, target_rect, 8192).expect("a plan");
    let mut blur = BackdropBlur::new(&gpu.device);
    let mut encode_us = Vec::new();
    let whole = median_us(&gpu.device, 60, || {
        let stats = blur.prepare(
            &gpu.device,
            &gpu.queue,
            &mut renderer,
            &frame,
            target,
            Color::BLACK,
        );
        encode_us.push(stats.encode_us);
        blur.release(&mut renderer);
    });
    let ground_only = {
        let size = plan.levels[0];
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        median_us(&gpu.device, 60, || {
            let mut below = Scene::new();
            below.append(
                &region.below,
                Some(Affine::scale(0.5) * Affine::translate((-plan.source.x0, -plan.source.y0))),
            );
            renderer
                .render_to_texture(
                    &gpu.device,
                    &gpu.queue,
                    &below,
                    &view,
                    &RenderParams {
                        base_color: Color::BLACK,
                        width: size.0,
                        height: size.1,
                        antialiasing_method: AaConfig::Area,
                    },
                )
                .expect("backdrop render");
        })
    };
    encode_us.sort_unstable();
    let info = gpu.device.adapter_info();
    println!(
        "backdrop at 1920x1080, {}x{} panel, sigma {} px, levels {:?} on {} ({:?}): \
         whole {whole} us, backdrop render {ground_only} us, blur passes {} us, \
         CPU encode {} us (medians)",
        rect.width(),
        rect.height(),
        region.blur_px,
        plan.levels,
        info.name,
        info.device_type,
        whole.saturating_sub(ground_only),
        encode_us[encode_us.len() / 2],
    );
}
