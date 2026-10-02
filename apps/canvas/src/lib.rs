//! Canvas: vector drawing and painting in real media, as fluent for an agent
//! as for a person.
//!
//! The artwork is layers of marks, each a vector object with a stable id. A
//! plain layer draws its marks as crisp vector art; an oil or watercolour
//! layer paints them with a simulation of that medium, re-simulated from the
//! marks on every machine. Everything a person does with the pointer is one
//! of the artwork's actions, and `look` lets a caller with no screen see the
//! result.

/// The application id.
pub const APP_ID: &str = "so.tilo.canvas";
/// The window and launcher title.
pub const APP_TITLE: &str = "Canvas";

pub mod actions;
pub mod doc;
pub mod locales;
pub mod media;
pub mod model;
pub mod render;
pub mod resource;
pub mod view_ops;

pub use actions::CanvasAction;
pub use model::Artwork;
pub use resource::ArtworkResource;

use ui::prelude::*;

pub struct App;

impl ResourceApp for App {
    type Resource = ArtworkResource;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(_cx: &mut ViewCx<ArtworkResource>) -> Element {
        locales::install();
        Box::new(Text::new("canvas-title"))
    }

    fn label(art: &Artwork) -> String {
        if art.title.is_empty() {
            APP_TITLE.to_owned()
        } else {
            art.title.clone()
        }
    }
}

ui::dev_reload_entry!(App);
