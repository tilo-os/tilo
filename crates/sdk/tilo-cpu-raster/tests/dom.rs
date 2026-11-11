//! The scene as DOM operations: keyed marks come out around the draws they
//! hold, wherever an append moved them.

use tilo_cpu_raster::dom::DomEncoder;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};
use vello_encoding::{MarkKind, Transform};

fn square(scene: &mut Scene, x: f64) {
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(200, 0, 0),
        None,
        &Rect::new(x, 0.0, x + 10.0, 10.0),
    );
}

fn begin(scene: &mut Scene, key: u64, x: f32) {
    scene.encoding_mut().encode_mark(MarkKind::Begin {
        key,
        origin: Transform {
            matrix: [1.0, 0.0, 0.0, 1.0],
            translation: [x, 0.0],
        },
    });
}

#[test]
fn keys_come_out_around_their_draws() {
    let mut widget = Scene::new();
    begin(&mut widget, 0xab, 0.0);
    square(&mut widget, 0.0);
    widget.encoding_mut().encode_mark(MarkKind::End);
    let mut scene = Scene::new();
    square(&mut scene, 50.0);
    scene.append(&widget, Some(Affine::translate((20.0, 0.0))));
    square(&mut scene, 80.0);

    let frame = DomEncoder::new().encode(scene.encoding());
    let ops: Vec<serde_json::Value> = serde_json::from_str(&frame.ops).unwrap();
    let kinds: Vec<&str> = ops.iter().map(|op| op[0].as_str().unwrap()).collect();
    assert_eq!(kinds, ["r", "k", "r", "ke", "r"]);
    assert_eq!(ops[1][1], "00000000000000ab");
    assert_eq!(
        ops[1][2].as_f64(),
        Some(20.0),
        "the append moved the key with its draws"
    );
    assert_eq!(ops[2][1].as_f64(), Some(20.0));
}

/// An image goes to the page once, as straight RGBA under an id; every
/// frame after names it by that id, placed by its matrix.
#[test]
fn an_image_is_handed_over_once_and_named_by_its_id() {
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
    // Two pixels: opaque red, and half-transparent white premultiplied.
    let image = ImageData {
        data: Blob::new(Arc::new(vec![255, 0, 0, 255, 128, 128, 128, 128])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: 2,
        height: 1,
    };
    let mut scene = Scene::new();
    scene.draw_image(
        &ImageBrush::new(image),
        Affine::translate((10.0, 20.0)) * Affine::scale(4.0),
    );
    let mut encoder = DomEncoder::new();
    let first = encoder.encode(scene.encoding());
    let ops: Vec<serde_json::Value> = serde_json::from_str(&first.ops).unwrap();
    let image_op = ops.iter().find(|op| op[0] == "i").expect("an image op");
    assert_eq!(first.images.len(), 1);
    let given = &first.images[0];
    assert_eq!((given.width, given.height), (2, 1));
    assert_eq!(given.rgba, vec![255, 0, 0, 255, 255, 255, 255, 128]);
    assert_eq!(image_op[5].as_u64(), Some(u64::from(given.id)));
    // The image-to-device matrix: scaled by four, at 10, 20.
    let matrix: Vec<f64> = (6..12).map(|i| image_op[i].as_f64().unwrap()).collect();
    assert_eq!(matrix, vec![4.0, 0.0, 0.0, 4.0, 10.0, 20.0]);
    let again = encoder.encode(scene.encoding());
    assert!(again.images.is_empty(), "the page has it already");
    assert!(again.ops.contains(&format!(",{},", given.id)));
}

/// A picture, or a widget of many draws, is one image of what it drew; drawn
/// the same again, the page's image stands, and drawn anew it replaces it.
#[test]
fn a_picture_is_one_image_until_it_changes() {
    use std::collections::HashSet;
    use tilo_cpu_raster::dom::{ISLAND_DRAWS, Islands};
    let chart = |shift: f64| {
        let mut scene = Scene::new();
        square(&mut scene, 0.0);
        begin(&mut scene, 0xc4, 0.0);
        square(&mut scene, 20.0 + shift);
        square(&mut scene, 40.0);
        scene.encoding_mut().encode_mark(MarkKind::End);
        scene
    };
    let pictures: HashSet<u64> = [0xc4].into();
    let islands = Islands {
        pictures: Some(&pictures),
        keep: None,
        viewport: Rect::new(0.0, 0.0, 100.0, 100.0),
    };
    let mut encoder = DomEncoder::new();
    let kinds = |ops: &str| -> Vec<String> {
        let ops: Vec<serde_json::Value> = serde_json::from_str(ops).unwrap();
        ops.iter()
            .map(|op| op[0].as_str().unwrap().to_owned())
            .collect()
    };
    let first = encoder.encode_with(chart(0.0).encoding(), &islands);
    assert_eq!(kinds(&first.ops), ["r", "k", "i", "ke"]);
    assert_eq!(first.images.len(), 1);
    let image = &first.images[0];
    assert_eq!((image.width, image.height), (30, 10), "cut to what it drew");
    assert_eq!(&image.rgba[..4], [200, 0, 0, 255]);
    assert!(image.replaces.is_none());
    let again = encoder.encode_with(chart(0.0).encoding(), &islands);
    assert!(
        again.images.is_empty(),
        "drawn the same: the page's image stands"
    );
    let moved = encoder.encode_with(chart(5.0).encoding(), &islands);
    assert_eq!(moved.images.len(), 1);
    assert_eq!(moved.images[0].replaces, Some(image.id));

    // Without a picture's say-so, a widget of many draws of its own.
    let mut heavy = Scene::new();
    begin(&mut heavy, 0xd5, 0.0);
    let mut x = 0.0;
    for _ in 0..ISLAND_DRAWS {
        square(&mut heavy, x % 9.0);
        x += 1.0;
    }
    heavy.encoding_mut().encode_mark(MarkKind::End);
    let frame = DomEncoder::new().encode_with(
        heavy.encoding(),
        &Islands {
            viewport: Rect::new(0.0, 0.0, 100.0, 100.0),
            ..Islands::default()
        },
    );
    assert_eq!(kinds(&frame.ops), ["k", "i", "ke"]);
    // One that must stay elements (it scrolls natively) stays.
    let keep: HashSet<u64> = [0xd5].into();
    let frame = DomEncoder::new().encode_with(
        heavy.encoding(),
        &Islands {
            keep: Some(&keep),
            viewport: Rect::new(0.0, 0.0, 100.0, 100.0),
            ..Islands::default()
        },
    );
    assert_eq!(kinds(&frame.ops).len(), ISLAND_DRAWS + 2);
}
