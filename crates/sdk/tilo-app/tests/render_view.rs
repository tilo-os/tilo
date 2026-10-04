#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! `render` answers with what the window shows, however much it draws: a
//! fresh renderer's first frame of a dense view is not handed back blank.

use std::rc::Rc;

use serde_json::json;
use tilo_app::prelude::*;
use tilo_app::vello::kurbo::{BezPath, Stroke};
use tilo_app::vello::peniko::Color;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Sketch {
    pub title: String,
}

#[resource(kind = "test.sketch", label = "Sketch")]
impl Sketch {
    /// Rename the sketch.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

struct SketchApp;

impl ResourceApp for SketchApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-sketch";
    const TITLE: &'static str = "Sketch";

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        cx.surface(Hatching::default())
    }
}

/// The ink the hatching is drawn in.
const INK: Color = Color::from_rgb8(0xd0, 0x30, 0x20);
/// The surface's side, in points.
const SIDE: f32 = 480.0;

/// A surface that draws a great many fine strokes: far more geometry than a
/// new renderer reserves room for in its first frame.
#[derive(Default)]
struct Hatching {
    rect: Rect,
}

impl Widget for Hatching {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use tilo_app::taffy::{Dimension, Size as Extent, Style};
        taffy
            .new_leaf(Style {
                size: Extent {
                    width: Dimension::length(SIDE),
                    height: Dimension::length(SIDE),
                },
                ..Style::default()
            })
            .unwrap()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::from_origin_size(
            origin,
            Size::new(f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _: &mut TextRenderer,
        _: &Theme,
        _: &PresenceCtx,
    ) {
        let pen = Stroke::new(0.6);
        for row in 0..400_u32 {
            let mut path = BezPath::new();
            let y = self.rect.y0 + f64::from(row) * f64::from(SIDE) / 400.0;
            path.move_to((self.rect.x0, y));
            for step in 1..=240_u32 {
                let x = self.rect.x0 + f64::from(step) * f64::from(SIDE) / 240.0;
                let wobble = if step % 2 == 0 { 0.8 } else { -0.8 };
                path.line_to((x, y + wobble));
            }
            scene.stroke(&pen, transform, INK, None, &path);
        }
    }

    fn name(&self) -> &'static str {
        "Hatching"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Sketch> for Hatching {
    fn present(&mut self, _: &Rc<Sketch>) {}
}

#[test]
fn render_answers_a_dense_view_with_its_pixels() {
    let mut app = TestApp::<SketchApp>::new().unwrap();
    let shown = app.call("render", json!({})).unwrap().unwrap();
    let address = shown["content"].as_str().expect("a stored picture");
    let hash = tilo_app::tilo_actions::content_hash(address).expect("a content address");
    let png = tilo_content::ContentStore::for_app()
        .read(hash)
        .expect("the picture is stored");
    let picture = tilo_app::png::decode_rgba8(&png).unwrap();
    let pixels = picture.rgba.chunks_exact(4);
    let total = pixels.len();
    let opaque = pixels.clone().filter(|pixel| pixel[3] == 255).count();
    assert_eq!(
        opaque, total,
        "the app's background covers the whole picture, not a blank first frame"
    );
    let inked = pixels
        .filter(|pixel| pixel[0] > 0x90 && pixel[1] < 0x60 && pixel[2] < 0x60)
        .count();
    assert!(
        inked > total / 10,
        "the hatching shows: {inked} inked pixels"
    );
}
