//! A non-destructive, graph-native raster editor.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub mod actions;
pub mod app;
pub mod doc;
pub mod engine;
pub mod model;
pub mod search;
pub use app::ImageEditorApp;
use ui::widget::Widget;
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImageEditorApp::new())
}
pub const SHOT_SCENARIOS: &[&str] = &[
    "canvas-stack",
    "graph",
    "narrow",
    "narrow-more",
    "narrow-long-stack",
    "graph-narrow",
];
pub fn write_shot_image(path: &std::path::Path) -> Result<(), image::ImageError> {
    let mut image = image::RgbaImage::new(1200, 800);
    for (y, row) in image.rows_mut().enumerate() {
        for (x, p) in row.enumerate() {
            let fx = x as f32 / 1200.0;
            let fy = y as f32 / 800.0;
            let sun = ((fx - 0.28).powi(2) + (fy - 0.3).powi(2)).sqrt() < 0.09;
            *p = if sun {
                image::Rgba([255, 211, 112, 255])
            } else {
                image::Rgba([
                    (32.0 + 150.0 * fx) as u8,
                    (68.0 + 130.0 * (1.0 - fy)) as u8,
                    (150.0 + 80.0 * fy) as u8,
                    255,
                ])
            };
        }
    }
    image.save(path)
}
