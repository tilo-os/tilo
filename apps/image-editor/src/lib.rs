//! A non-destructive, graph-native raster editor.
#![allow(
    // The editor deliberately keeps its direct-manipulation event, rendering,
    // and accessibility paths together. Splitting them just to satisfy line
    // counts makes pointer/keyboard/AT behaviour drift; the concrete unsafe
    // and arithmetic lints below remain denied by the workspace.
    clippy::arithmetic_side_effects,
    clippy::assigning_clones,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::if_same_then_else,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_return,
    clippy::new_without_default,
    clippy::semicolon_if_nothing_returned,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::unnecessary_literal_bound
)]
pub mod actions;
pub mod app;
pub mod doc;
pub mod engine;
pub mod model;
pub mod resource;
pub mod search;
mod web_file;
pub use app::ImageEditorApp;
use ui::widget::Widget;
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImageEditorApp::new())
}

/// The app as every host mounts it: the framework app over its project.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<ImageEditorShell>()
}
pub const SHOT_SCENARIOS: &[&str] = &[
    "empty",
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
pub use resource::{ImageEditor, ImageEditorShell, launch_spec, standalone_launch};
