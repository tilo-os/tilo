//! Real toolkit-shaped scenes cross the wire unchanged and pass the
//! validator; hostile variants of them never panic the service's CPU side.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "test scenes: failures are the assertion"
)]

use std::collections::HashMap;
use std::sync::Arc;

use proptest::prelude::*;
use tilo_render_protocol::frame::{self, Backdrop, FrameWriter, LayerUpdate, Rect, ScrollCopy};
use tilo_render_protocol::scene::{self, ResourceIds, Resources};
use tilo_render_protocol::validate::validate;
use tilo_render_protocol::{FontId, ImageId, LayerId, Limits, SurfaceId, WireError};
use vello::Glyph;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect as KRect, RoundedRect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, ColorStop, Compose, Extend, Fill, FontData, Gradient, ImageAlphaType,
    ImageBrush, ImageData, ImageFormat, ImageQuality, Mix,
};
use vello_encoding::Resolver;

/// Both sides of a connection's registry: ids by blob, and back.
#[derive(Default)]
struct Registry {
    fonts: HashMap<u64, (FontId, FontData)>,
    images: HashMap<u64, (ImageId, ImageData)>,
}

impl ResourceIds for Registry {
    fn font(&mut self, font: &FontData) -> Result<FontId, WireError> {
        let next = FontId(u32::try_from(self.fonts.len()).unwrap());
        Ok(self
            .fonts
            .entry(font.data.id())
            .or_insert_with(|| (next, font.clone()))
            .0)
    }

    fn image(&mut self, image: &ImageData) -> Result<ImageId, WireError> {
        let next = ImageId(u32::try_from(self.images.len()).unwrap());
        Ok(self
            .images
            .entry(image.data.id())
            .or_insert_with(|| (next, image.clone()))
            .0)
    }
}

impl Resources for Registry {
    fn font(&self, id: FontId) -> Option<FontData> {
        self.fonts
            .values()
            .find(|(f, _)| *f == id)
            .map(|(_, data)| data.clone())
    }

    fn image(&self, id: ImageId) -> Option<ImageData> {
        self.images
            .values()
            .find(|(i, _)| *i == id)
            .map(|(_, data)| data.clone())
    }
}

fn font() -> FontData {
    let path = tilo_font_catalog::checkout_root().join("AdwaitaSans-Regular.ttf");
    FontData::new(Blob::new(Arc::new(std::fs::read(path).unwrap())), 0)
}

fn image(width: u32, height: u32) -> ImageData {
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|i| (i * 37 % 251) as u8)
        .collect();
    ImageData {
        data: Blob::new(Arc::new(pixels)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

fn gradient_stops() -> Vec<ColorStop> {
    vec![
        ColorStop::from((0.0, Color::from_rgba8(255, 0, 0, 255))),
        ColorStop::from((0.4, Color::from_rgba8(0, 128, 255, 200))),
        ColorStop::from((1.0, Color::from_rgba8(10, 200, 30, 255))),
    ]
}

/// What a toolkit frame draws: fills, strokes, layers and clips, every
/// gradient kind, images, blurred shadows and text.
fn toolkit_scene(font: &FontData, picture: &ImageData) -> Scene {
    let mut s = Scene::new();
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(240, 240, 240, 255),
        None,
        &KRect::new(0.0, 0.0, 800.0, 600.0),
    );
    s.draw_blurred_rounded_rect(
        Affine::translate((20.0, 20.0)),
        KRect::new(0.0, 0.0, 200.0, 120.0),
        Color::from_rgba8(0, 0, 0, 80),
        12.0,
        8.0,
    );
    s.fill(
        Fill::EvenOdd,
        Affine::scale(1.5),
        Color::from_rgba8(30, 60, 90, 255),
        None,
        &RoundedRect::new(10.0, 10.0, 150.0, 90.0, 9.0),
    );
    s.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        0.8,
        Affine::IDENTITY,
        &Circle::new((300.0, 300.0), 120.0),
    );
    s.push_clip_layer(
        Fill::NonZero,
        Affine::translate((5.0, 5.0)),
        &KRect::new(200.0, 200.0, 400.0, 400.0),
    );
    let linear =
        Gradient::new_linear((0.0, 0.0), (400.0, 0.0)).with_stops(gradient_stops().as_slice());
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &linear,
        None,
        &KRect::new(180.0, 180.0, 420.0, 420.0),
    );
    s.pop_layer();
    s.pop_layer();
    let radial = Gradient::new_two_point_radial((100.0, 400.0), 4.0, (120.0, 410.0), 80.0)
        .with_stops(gradient_stops().as_slice())
        .with_extend(Extend::Reflect);
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &radial,
        Some(Affine::rotate(0.3)),
        &Circle::new((110.0, 400.0), 90.0),
    );
    let sweep =
        Gradient::new_sweep((600.0, 100.0), 0.0, 4.0).with_stops(gradient_stops().as_slice());
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &sweep,
        None,
        &Circle::new((600.0, 100.0), 60.0),
    );
    let mut path = BezPath::new();
    path.move_to((10.0, 500.0));
    path.line_to((60.0, 450.0));
    path.quad_to((90.0, 420.0), (120.0, 470.0));
    path.curve_to((150.0, 520.0), (180.0, 430.0), (220.0, 480.0));
    s.stroke(
        &Stroke::new(3.0)
            .with_caps(Cap::Round)
            .with_join(Join::Miter)
            .with_dashes(1.0, [6.0, 3.0]),
        Affine::IDENTITY,
        Color::from_rgba8(200, 20, 20, 255),
        None,
        &path,
    );
    path.close_path();
    s.stroke(
        &Stroke::new(1.0),
        Affine::translate((0.0, 20.0)),
        Color::from_rgba8(0, 0, 0, 255),
        None,
        &path,
    );
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(0, 0, 0, 40),
        None,
        &path,
    );
    s.draw_image(
        &ImageBrush::new(picture.clone())
            .with_quality(ImageQuality::Low)
            .with_extend(Extend::Repeat)
            .with_alpha(0.7),
        Affine::translate((500.0, 300.0)).then_scale(2.0),
    );
    s.push_luminance_mask_layer(
        Fill::NonZero,
        1.0,
        Affine::IDENTITY,
        &KRect::new(0.0, 0.0, 50.0, 50.0),
    );
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::WHITE,
        None,
        &KRect::new(0.0, 0.0, 50.0, 50.0),
    );
    s.pop_layer();
    let glyphs = |y: f32| {
        (0..20_u32).map(move |i| Glyph {
            id: 30 + i,
            x: i as f32 * 9.0,
            y,
        })
    };
    s.draw_glyphs(font)
        .font_size(15.0)
        .hint(true)
        .transform(Affine::translate((40.0, 560.0)))
        .brush(Color::from_rgba8(20, 20, 20, 255))
        .draw(Fill::NonZero, glyphs(0.0));
    s.draw_glyphs(font)
        .font_size(22.0)
        .glyph_transform(Some(Affine::skew(0.2, 0.0)))
        .brush(&linear)
        .draw(&Stroke::new(0.5), glyphs(30.0));
    let mut inner = Scene::new();
    inner.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(1, 2, 3, 255),
        None,
        &KRect::new(0.0, 0.0, 10.0, 10.0),
    );
    inner
        .draw_glyphs(font)
        .font_size(12.0)
        .draw(Fill::NonZero, glyphs(5.0));
    s.append(&inner, Some(Affine::translate((700.0, 500.0))));
    s
}

fn roundtrip(scene: &Scene) -> (Registry, Vec<u8>) {
    let mut registry = Registry::default();
    let mut bytes = Vec::new();
    scene::encode(scene.encoding(), &mut registry, &mut bytes).unwrap();
    (registry, bytes)
}

#[test]
fn a_toolkit_scene_crosses_the_wire_unchanged_and_validates() {
    let scene = toolkit_scene(&font(), &image(4, 3));
    let (registry, bytes) = roundtrip(&scene);
    let decoded = scene::decode(&bytes, &registry).unwrap();
    let cost = validate(&decoded, &Limits::default()).unwrap();
    assert!(cost.paths > 10 && cost.glyphs == 60, "{cost:?}");
    let back = Scene::from(decoded);
    assert!(
        back.encoding_eq(&scene),
        "decoding gives back the encoding the app drew"
    );
    // vello's own resolver packs it without complaint.
    let mut packed = Vec::new();
    Resolver::new().resolve(back.encoding(), &mut packed);
}

#[test]
fn an_empty_scene_is_valid() {
    let (registry, bytes) = roundtrip(&Scene::new());
    let decoded = scene::decode(&bytes, &registry).unwrap();
    assert_eq!(validate(&decoded, &Limits::default()).unwrap().paths, 0);
}

#[test]
fn a_frame_record_carries_layers_damage_copies_and_backdrops() {
    let font = font();
    let picture = image(4, 3);
    let content = toolkit_scene(&font, &picture);
    let placeholder = image(8, 8);
    let mut registry = Registry::default();
    let mut out = vec![0xAA; 12]; // a record need not start the buffer
    let mut writer = FrameWriter::new(
        &mut out,
        (SurfaceId(3), 77),
        (800, 600),
        [1.0, 1.0, 1.0, 1.0],
        false,
    );
    writer
        .damage(&[Rect {
            x0: 1.0,
            y0: 2.0,
            x1: 30.0,
            y1: 40.0,
        }])
        .unwrap();
    writer
        .scroll_copies(&[ScrollCopy {
            x: 0,
            y: 100,
            width: 800,
            height: 400,
            dx: 0,
            dy: -40,
        }])
        .unwrap();
    writer
        .layer(
            &LayerUpdate::content(LayerId::ROOT, content.encoding().clone()),
            &mut registry,
        )
        .unwrap();
    // The placeholder is registered like any image the scene draws.
    let _ = ResourceIds::image(&mut registry, &placeholder).unwrap();
    writer
        .backdrop(
            &Backdrop {
                image: ResourceIds::image(&mut registry, &placeholder).unwrap(),
                rect: Rect {
                    x0: 10.0,
                    y0: 10.0,
                    x1: 110.0,
                    y1: 60.0,
                },
                blur_px: 12.0,
                ground: [0.9, 0.9, 0.9, 1.0],
                below: content.encoding().clone(),
            },
            &mut registry,
        )
        .unwrap();
    writer
        .raw_section(900, true, b"a later step's annotation")
        .unwrap();
    let len = writer.finish().unwrap();
    let record = &out[12..];
    assert_eq!(record.len(), len);
    let checked = frame::read(record, &registry, &Limits::default()).unwrap();
    let frame = checked.frame;
    assert_eq!(
        (frame.surface, frame.sequence, frame.width, frame.height),
        (SurfaceId(3), 77, 800, 600)
    );
    assert_eq!(frame.damage.len(), 1);
    assert_eq!(frame.scroll_copies[0].dy, -40);
    assert_eq!(frame.layers.len(), 1);
    assert_eq!(frame.backdrops.len(), 1);
    assert!(checked.cost.paths > 20);
}

#[test]
fn an_unknown_required_section_refuses_the_frame() {
    let mut out = Vec::new();
    let mut writer = FrameWriter::new(&mut out, (SurfaceId(0), 1), (10, 10), [0.0; 4], true);
    writer
        .raw_section(901, false, b"motion this service never announced")
        .unwrap();
    writer.finish().unwrap();
    assert!(matches!(
        frame::read(&out, &Registry::default(), &Limits::default()),
        Err(WireError::UnknownSection(901))
    ));
}

#[test]
fn a_frame_over_its_limits_is_refused() {
    let scene = toolkit_scene(&font(), &image(4, 3));
    let mut registry = Registry::default();
    let mut out = Vec::new();
    let mut writer = FrameWriter::new(&mut out, (SurfaceId(0), 1), (10, 10), [0.0; 4], true);
    writer
        .layer(
            &LayerUpdate::content(LayerId::ROOT, scene.encoding().clone()),
            &mut registry,
        )
        .unwrap();
    writer.finish().unwrap();
    let limits = Limits {
        paths: 3,
        ..Limits::default()
    };
    assert!(matches!(
        frame::read(&out, &registry, &limits),
        Err(WireError::OverLimit("paths"))
    ));
    let limits = Limits {
        surface_edge: 5,
        ..Limits::default()
    };
    assert!(matches!(
        frame::read(&out, &registry, &limits),
        Err(WireError::OverLimit("surface size"))
    ));
}

#[test]
fn an_image_draw_cannot_claim_more_pixels_than_its_image() {
    let picture = image(4, 3);
    let mut s = Scene::new();
    s.draw_image(&picture, Affine::IDENTITY);
    let mut encoding = s.encoding().clone();
    // width_height follows the atlas slot: claim 4096×4096 to read the atlas
    // around the image (another app's pixels).
    let Some(vello_encoding::Patch::Image {
        draw_data_offset, ..
    }) = encoding.resources.patches.first().cloned()
    else {
        panic!("an image patch");
    };
    encoding.draw_data[draw_data_offset + 1] = (4096 << 16) | 4096;
    assert!(matches!(
        validate(&encoding, &Limits::default()),
        Err(WireError::Invalid(_))
    ));
    // Dropping the patch would leave the slot to the app.
    let mut encoding = s.encoding().clone();
    encoding.resources.patches.clear();
    assert!(validate(&encoding, &Limits::default()).is_err());
}

#[test]
fn a_gradient_ramp_patch_must_sit_on_its_gradient() {
    let mut s = Scene::new();
    let linear =
        Gradient::new_linear((0.0, 0.0), (10.0, 0.0)).with_stops(gradient_stops().as_slice());
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::BLACK,
        None,
        &KRect::new(0.0, 0.0, 5.0, 5.0),
    );
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &linear,
        None,
        &KRect::new(0.0, 0.0, 10.0, 10.0),
    );
    let mut encoding = s.encoding().clone();
    if let Some(vello_encoding::Patch::Ramp {
        draw_data_offset, ..
    }) = encoding.resources.patches.first_mut()
    {
        *draw_data_offset = 0; // the colour fill's word
    }
    assert!(validate(&encoding, &Limits::default()).is_err());
}

#[test]
fn unbalanced_clips_and_non_finite_numbers_are_refused() {
    let mut s = Scene::new();
    s.push_clip_layer(
        Fill::NonZero,
        Affine::IDENTITY,
        &KRect::new(0.0, 0.0, 5.0, 5.0),
    );
    assert!(
        validate(s.encoding(), &Limits::default()).is_err(),
        "a layer left open"
    );
    let mut s = Scene::new();
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::BLACK,
        None,
        &KRect::new(0.0, 0.0, 5.0, 5.0),
    );
    let mut encoding = s.encoding().clone();
    encoding.path_data[1] = f32::NAN.to_bits();
    assert!(validate(&encoding, &Limits::default()).is_err());
    let mut encoding = s.encoding().clone();
    encoding.transforms[0].translation[0] = f32::INFINITY;
    assert!(validate(&encoding, &Limits::default()).is_err());
    let mut s = Scene::new();
    for _ in 0..70 {
        s.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &KRect::new(0.0, 0.0, 5.0, 5.0),
        );
    }
    for _ in 0..70 {
        s.pop_layer();
    }
    assert!(matches!(
        validate(s.encoding(), &Limits::default()),
        Err(WireError::OverLimit("clip depth"))
    ));
}

fn shape_strategy() -> impl Strategy<Value = Vec<(u8, [f64; 6], u32)>> {
    prop::collection::vec(
        (
            0_u8..6,
            prop::array::uniform6(-500.0_f64..1500.0),
            any::<u32>(),
        ),
        0..40,
    )
}

fn random_scene(shapes: &[(u8, [f64; 6], u32)], font: &FontData, picture: &ImageData) -> Scene {
    let mut s = Scene::new();
    let mut open = 0;
    for (kind, v, bits) in shapes {
        let color = Color::from_rgba8(
            (*bits >> 24) as u8,
            (*bits >> 16) as u8,
            (*bits >> 8) as u8,
            *bits as u8,
        );
        let rect = KRect::new(v[0], v[1], v[0] + v[2].abs(), v[1] + v[3].abs());
        match kind {
            0 => s.fill(
                Fill::NonZero,
                Affine::rotate(v[4] * 0.01),
                color,
                None,
                &rect,
            ),
            1 => s.stroke(
                &Stroke::new(v[5].abs() % 20.0),
                Affine::IDENTITY,
                color,
                None,
                &Circle::new(Point::new(v[0], v[1]), v[2].abs() % 300.0),
            ),
            2 => {
                s.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &rect);
                open += 1;
            }
            3 if open > 0 => {
                s.pop_layer();
                open -= 1;
            }
            4 => s.draw_image(picture, Affine::translate((v[0], v[1]))),
            _ => s
                .draw_glyphs(font)
                .font_size((v[5].abs() % 60.0 + 1.0) as f32)
                .brush(color)
                .draw(
                    Fill::NonZero,
                    (0..(*bits % 8)).map(|i| Glyph {
                        id: 20 + i,
                        x: i as f32 * 7.0,
                        y: 0.0,
                    }),
                ),
        }
    }
    for _ in 0..open {
        s.pop_layer();
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Whatever a toolkit draws passes, unchanged.
    #[test]
    fn every_scene_the_toolkit_can_draw_validates(shapes in shape_strategy()) {
        let scene = random_scene(&shapes, &font(), &image(3, 2));
        let (registry, bytes) = roundtrip(&scene);
        let decoded = scene::decode(&bytes, &registry).unwrap();
        prop_assert!(validate(&decoded, &Limits::default()).is_ok());
        prop_assert!(Scene::from(decoded).encoding_eq(&scene));
    }

    /// Bytes an app scribbles over a valid record never panic the service:
    /// decoding and validating refuse them, or what passes resolves.
    #[test]
    fn hostile_bytes_never_panic_the_service(
        shapes in shape_strategy(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..16),
        cut in any::<prop::sample::Index>(),
        truncate in any::<bool>(),
    ) {
        let scene = random_scene(&shapes, &font(), &image(3, 2));
        let (registry, mut bytes) = roundtrip(&scene);
        for (at, value) in &edits {
            if !bytes.is_empty() {
                let at = at.index(bytes.len());
                bytes[at] = *value;
            }
        }
        if truncate && !bytes.is_empty() {
            bytes.truncate(cut.index(bytes.len()));
        }
        if let Ok(decoded) = scene::decode(&bytes, &registry)
            && validate(&decoded, &Limits::default()).is_ok()
        {
            let mut packed = Vec::new();
            Resolver::new().resolve(&decoded, &mut packed);
        }
    }
}
