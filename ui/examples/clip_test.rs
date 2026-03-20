use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::BlendMode;

fn main() {
    let mut scene = Scene::new();
    let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    scene.push_layer(BlendMode::default(), 1.0, Affine::IDENTITY, &rect);
    scene.pop_layer();
}
