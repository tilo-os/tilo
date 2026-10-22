//! What only the window that authors an action can find: colour as the
//! artwork shows it, for the magic wand and selecting by colour, and the
//! picture's subject and edges (`crate::subject`).
//!
//! Like `set-reference` reading a file where the action is authored, such an
//! action is resolved to plain geometry before it is applied, so every
//! replica applies (and history records) the same area. The canvas tells
//! this module what it shows each time it is presented the artwork.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "pixel indices are bounded by the picture's size, and kurbo's float geometry does not overflow"
)]

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use ui::num::usize_of_f64;

use ui::TextRenderer;
use ui::vello::kurbo::{Affine, Point, Rect};

use crate::model::{Artwork, path_data};
use crate::region::{Coverage, Op, Region};
use crate::render::{Options, Renderer};
use crate::resource::FindArea;
use crate::selection::Select;

/// The longest side of the picture colour is found in, in pixels.
const WAND_SIDE: u32 = 1600; // a sampling budget in pixels, not on-screen spacing
/// How far a colour may differ when the caller names no tolerance.
pub const TOLERANCE: f64 = 0.15;

struct Sight {
    renderer: Weak<RefCell<Renderer>>,
    art: Rc<Artwork>,
    text: Option<TextRenderer>,
}

thread_local! {
    static SIGHT: RefCell<Option<Sight>> = const { RefCell::new(None) };
}

/// The canvas shows `art`, drawn by `renderer`.
pub fn see(renderer: &Rc<RefCell<Renderer>>, art: &Rc<Artwork>) {
    SIGHT.with_borrow_mut(|sight| match sight {
        Some(sight) => {
            sight.renderer = Rc::downgrade(renderer);
            sight.art = Rc::clone(art);
        }
        None => {
            *sight = Some(Sight {
                renderer: Rc::downgrade(renderer),
                art: Rc::clone(art),
                text: None,
            });
        }
    });
}

thread_local! {
    static LATEST: RefCell<Option<Rc<Artwork>>> = const { RefCell::new(None) };
}

/// The artwork as last read from its document. An action authored right
/// after another (an agent opening a photo, then selecting its subject)
/// must find what it names in that, not in what the window showed before.
pub fn read(art: &Artwork) {
    LATEST.with_borrow_mut(|latest| *latest = Some(Rc::new(art.clone())));
}

/// What to find by colour.
#[derive(Clone, Debug, Default)]
pub struct ColourQuery {
    /// Start at the colour here (the magic wand).
    pub at: Option<Point>,
    /// Or find this colour, `[r, g, b]`.
    pub colour: Option<[u8; 3]>,
    /// `0..=1`.
    pub tolerance: f64,
    /// Also like colour that does not touch the start.
    pub everywhere: bool,
    /// See only these layers.
    pub layers: Option<Vec<String>>,
    /// Find it only inside this area: the selection, when the wand takes
    /// from it or keeps only what is in it, or the bucket fills inside it.
    pub within: Option<Region>,
}

/// The colour `find` asks for (at, the magic wand, or colour everywhere)
/// in `art`; `None` when it asks for the subject or a refined edge.
pub fn colour_query(find: &FindArea, art: &Artwork) -> Result<Option<ColourQuery>, String> {
    if find.subject == Some(true) || find.refine.is_some() {
        if find.colour.is_some() || find.tolerance.is_some() {
            return Err(
                "subject and refine find the area themselves; leave colour and tolerance out"
                    .into(),
            );
        }
        return Ok(None);
    }
    if find.at.is_none() && find.colour.is_none() {
        return Err("give at (the magic wand), colour, subject: true or refine: units".into());
    }
    let colour = match &find.colour {
        Some(text) => Some(
            tilo_wet_media::colour::parse_hex(text)
                .map(|(rgb, _)| rgb.to_srgb8())
                .ok_or_else(|| format!("colour {text:?} must be #rrggbb"))?,
        ),
        None => None,
    };
    let tolerance = find.tolerance.unwrap_or(TOLERANCE);
    if !(0.0..=1.0).contains(&tolerance) {
        return Err("tolerance is 0 to 1".into());
    }
    if let Some(layer) = &find.layer
        && art.layer(layer).is_none_or(crate::model::Layer::is_mask)
    {
        return Err(format!(
            "there is no layer {layer:?} to find colour on; the layers are {}",
            crate::actions::layer_list(art)
        ));
    }
    // Taking from the selection, or keeping only part of it, the wand looks
    // only inside it: what it finds cannot reach other parts through
    // colour outside.
    let within_selection = matches!(find.mode.as_deref(), Some("subtract" | "intersect"));
    Ok(Some(ColourQuery {
        at: find.at.map(|[x, y]| Point::new(x, y)),
        colour,
        tolerance,
        everywhere: find.at.is_none(),
        layers: find.layer.clone().map(|layer| vec![layer]),
        within: art
            .selection
            .as_ref()
            .filter(|_| within_selection)
            .map(crate::model::Selection::region),
    }))
}

/// Run `look` with what the canvas shows.
pub fn with_sight<T>(
    look: impl FnOnce(&mut Renderer, &mut TextRenderer, &Artwork) -> Result<T, String>,
) -> Result<T, String> {
    SIGHT.with_borrow_mut(|sight| {
        let sight = sight
            .as_mut()
            .ok_or("colour is found in the artwork as a window shows it; none is open")?;
        let renderer = sight
            .renderer
            .upgrade()
            .ok_or("colour is found in the artwork as a window shows it; none is open")?;
        let art = LATEST
            .with_borrow(Clone::clone)
            .unwrap_or_else(|| Rc::clone(&sight.art));
        let text = sight.text.get_or_insert_with(TextRenderer::new);
        let mut renderer = renderer
            .try_borrow_mut()
            .map_err(|_| "the canvas is drawing; try again".to_owned())?;
        look(&mut renderer, text, &art)
    })
}

/// The area of `art` showing the colour `query` names, found in a picture
/// of it. The wand looks at the whole artwork first, then again closer at
/// the part it found, so its edge follows the paint finely on a big
/// artwork too.
pub fn colour_region(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    query: &ColourQuery,
) -> Result<Region, String> {
    let mut search = ColourSearch::start(renderer, text, art, query.clone());
    // Answered at once where the thread can wait for its pictures; a
    // caller that cannot wait is told pictures cannot be had here.
    search
        .poll(renderer, text, art)
        .unwrap_or_else(|| crate::view_ops::rgba_of(None).map(|_| Region::default()))
}

/// The wand's search for a colour, one or two pictures of the artwork long:
/// it looks at the whole artwork first, then again closer at the part it
/// found. In a browser page each picture comes a few frames after it is
/// asked for; [`poll`](Self::poll) it every frame until it answers.
pub struct ColourSearch {
    query: ColourQuery,
    looking: Looking,
}

/// A picture the search is waiting for.
struct Looking {
    pixels: ui::offscreen::pixels::PixelsRequest,
    sight: Glance,
}

/// What a look sees: `view` at `scale` pixels per unit.
struct Glance {
    view: Rect,
    scale: f64,
    /// Closer, it may only find what the first look found, and the pixels
    /// about its edge.
    near: Option<Region>,
    /// What the first look found, once this is the closer look.
    first: Option<Region>,
}

impl ColourSearch {
    /// Start looking for what `query` names in `art`.
    #[must_use]
    pub fn start(
        renderer: &mut Renderer,
        text: &mut TextRenderer,
        art: &Artwork,
        query: ColourQuery,
    ) -> Self {
        let scale = (f64::from(WAND_SIDE) / art.width.max(art.height).max(1.0)).min(2.0);
        let whole = Glance {
            view: art.bounds(),
            scale,
            near: None,
            first: None,
        };
        let looking = ask(renderer, text, art, &query, whole);
        Self { query, looking }
    }

    /// The area found, once its pictures came; `None` while one is still
    /// on its way.
    pub fn poll(
        &mut self,
        renderer: &mut Renderer,
        text: &mut TextRenderer,
        art: &Artwork,
    ) -> Option<Result<Region, String>> {
        loop {
            let answer = self.looking.pixels.take()?;
            let found = crate::view_ops::rgba_of(Some(answer)).and_then(|(rgba, width, height)| {
                let picture = crate::wand::Picture {
                    rgba: &rgba,
                    width: width as usize,
                    height: height as usize,
                };
                find_in(art, &self.query, &picture, &self.looking.sight)
            });
            let found = match found {
                Ok(found) => found,
                Err(why) => return Some(Err(why)),
            };
            let scale = self.looking.sight.scale;
            let found = if let Some(first) = self.looking.sight.first.take() {
                if found.is_empty() { first } else { found }
            } else if let Some((view, closer_scale)) = closer(art, &self.query, &found, scale) {
                let sight = Glance {
                    view,
                    scale: closer_scale,
                    near: Some(found.grown(3.0 / scale)),
                    first: Some(found),
                };
                self.looking = ask(renderer, text, art, &self.query, sight);
                continue;
            } else {
                found
            };
            // Found inside `within`, its edge runs a little past it, so
            // taking it from the selection or keeping only it there leaves
            // no sliver along the selection's own edge.
            return Some(Ok(
                found.combine(&Region::of_rect(art.bounds()), Op::Intersect)
            ));
        }
    }
}

/// Ask for the picture `sight` sees, showing what `query` looks at.
fn ask(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    query: &ColourQuery,
    sight: Glance,
) -> Looking {
    let options = Options {
        layers: query.layers.clone(),
        bare: query.layers.is_some(),
        ..Options::default()
    };
    // Colour is found in the pictures themselves, never in the placeholder
    // the canvas shows while a just-opened photo is still being decoded.
    let patient = std::mem::replace(&mut renderer.pictures.patient, true);
    let drawing = crate::view_ops::drawing(sight.view, sight.scale, options.bare, |scene, to| {
        renderer.draw(art, scene, to, text, &options);
    });
    renderer.pictures.patient = patient;
    Looking {
        pixels: drawing.request(),
        sight,
    }
}

/// Where to look again, closer, at what the first look found: the part of
/// the artwork around it in a picture of the same size, when that is
/// markedly closer.
fn closer(art: &Artwork, query: &ColourQuery, found: &Region, scale: f64) -> Option<(Rect, f64)> {
    if query.everywhere || query.at.is_none() {
        return None;
    }
    let bounds = found.bounds()?.inflate(3.0 / scale, 3.0 / scale);
    let (width, height) = (art.width * scale, art.height * scale);
    let closer = (width / bounds.width().max(1e-6))
        .min(height / bounds.height().max(1e-6))
        .min(MOST_PIXELS_PER_UNIT);
    if closer < scale * 1.25 {
        return None;
    }
    // The same size of picture as the first look, so its renderer is kept.
    let (half_width, half_height) = (width / closer / 2.0, height / closer / 2.0);
    let centre = bounds.center();
    Some((
        Rect::new(
            centre.x - half_width,
            centre.y - half_height,
            centre.x + half_width,
            centre.y + half_height,
        ),
        closer,
    ))
}

/// The closest the wand looks, in pixels per unit.
const MOST_PIXELS_PER_UNIT: f64 = 8.0; // a sampling density of the artwork, not on-screen spacing

/// The colour `query` names in `picture`, what `sight` sees, found only
/// inside its `near` when given.
fn find_in(
    art: &Artwork,
    query: &ColourQuery,
    picture: &crate::wand::Picture<'_>,
    sight: &Glance,
) -> Result<Region, String> {
    let (width, height) = (picture.width, picture.height);
    let (view, scale, near) = (sight.view, sight.scale, sight.near.as_ref());
    let to_cells = Affine::scale(scale) * Affine::translate((-view.x0, -view.y0));
    let seed = match query.at {
        Some(at) => {
            let cell = to_cells * at;
            let inside = |value: f64, side: usize| {
                (0.0..f64::from(u32::try_from(side).unwrap_or(u32::MAX))).contains(&value)
            };
            if !art.bounds().contains(at) || !inside(cell.x, width) || !inside(cell.y, height) {
                return Err(format!(
                    "at [{}, {}] is outside the artwork (0, 0, {}, {})",
                    at.x, at.y, art.width, art.height
                ));
            }
            Some((usize_of_f64(cell.x), usize_of_f64(cell.y)))
        }
        None => None,
    };
    if seed.is_none() && query.colour.is_none() {
        return Err("give at or colour".into());
    }
    // Only the artwork, only inside what it may find.
    let mut allowed = Region::of_rect(art.bounds());
    for limit in [query.within.as_ref(), near].into_iter().flatten() {
        allowed = allowed.combine(limit, Op::Intersect);
    }
    let allowed = allowed.cells(to_cells, width, height);
    let field = crate::wand::coverage(
        picture,
        &crate::wand::Wand {
            seed,
            colour: query.colour,
            tolerance: query.tolerance,
            everywhere: query.everywhere,
            allowed: Some(&allowed),
            pixels_per_unit: scale,
        },
    );
    Ok(Coverage {
        width,
        height,
        cells: &field,
    }
    .traced(to_cells.inverse()))
}

/// The area `find` found as the selection it makes, with its options.
pub fn found(find: &FindArea, found: &Region, matte: Option<crate::model::Matte>) -> Select {
    Select {
        d: Some(path_data(&found.to_path())),
        matte,
        mode: find.mode.clone(),
        feather: find.feather,
        ..Select::default()
    }
}
