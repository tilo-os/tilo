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
pub mod i18n;
pub mod model;
pub mod resource;
pub mod search;
mod web_file;
pub use app::ImageEditorApp;
use ui::widget::Widget;
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ImageEditorApp::new())
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.image-editor", app_root)
        .component_actions(actions::register)
        .as_definition("Image Editor")
}
pub use resource::{ImageEditor, ImageEditorShell, file_launch_spec, launch_spec, opens_a_file};
