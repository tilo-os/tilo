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
