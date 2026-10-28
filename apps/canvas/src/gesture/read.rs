//! What a click reads off a picture of the artwork: the eyedropper's
//! colour, the area the magic wand or the fill bucket finds.
//!
//! The picture is asked for when the click lands. Where the thread can wait
//! for the GPU it is there at once; in a browser page it comes a few frames
//! later, so the canvas keeps the reading and [`polls`](Sampling::poll) it
//! every frame until it answers.

use ui::TextRenderer;
use ui::offscreen::pixels::PixelsRequest;
use ui::vello::peniko::Color;

use crate::author::ColourSearch;
use crate::model::Artwork;
use crate::region::Region;
use crate::render::Renderer;

/// A click's reading, on its way.
pub enum Sampling {
    /// The eyedropper's patch of the artwork.
    Colour(PixelsRequest),
    /// The wand's or the bucket's search.
    Area {
        search: Box<ColourSearch>,
        /// The bucket fills what it finds; the wand selects it.
        fill: bool,
        /// How the wand's area meets the selection.
        mode: Option<String>,
    },
}

/// What a click read.
pub enum Sampled {
    Colour(Color),
    Area {
        found: Region,
        fill: bool,
        mode: Option<String>,
    },
}

impl Sampling {
    /// What was read, once its pictures came; `None` while one is on its
    /// way.
    pub fn poll(
        &mut self,
        renderer: &mut Renderer,
        text: &mut TextRenderer,
        art: &Artwork,
    ) -> Option<Result<Sampled, String>> {
        match self {
            Self::Colour(pixels) => {
                let answer = pixels.take()?;
                Some(
                    crate::view_ops::rgba_of(Some(answer))
                        .map(|(rgba, _, _)| Sampled::Colour(crate::view_ops::mean_colour(&rgba))),
                )
            }
            Self::Area { search, fill, mode } => {
                let found = search.poll(renderer, text, art)?;
                Some(found.map(|found| Sampled::Area {
                    found,
                    fill: *fill,
                    mode: mode.take(),
                }))
            }
        }
    }
}
