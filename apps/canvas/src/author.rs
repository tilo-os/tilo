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

/// Turn `select`'s colour into the area it finds, in `d`.
///
/// # Errors
/// Returns why the colour cannot be found: no window shows the artwork, or
/// the query is malformed.
pub fn resolve(mut select: Select) -> Result<crate::actions::CanvasAction, String> {
    use crate::actions::CanvasAction;
    if !select.looks() {
        if select.layer.is_some() {
            return Err("layer names where at, colour or subject is found; give one".into());
        }
        return Ok(CanvasAction::Select(Box::new(select)));
    }
    if select.subject == Some(true) || select.refine.is_some() {
        return Err("subject selection and edge refinement run as the find-area job; invoke find-area with these arguments".into());
    }
    let colour = match &select.colour {
        Some(text) => Some(
            tilo_wet_media::colour::parse_hex(text)
                .map(|(rgb, _)| rgb.to_srgb8())
                .ok_or_else(|| format!("colour {text:?} must be #rrggbb"))?,
        ),
        None => None,
    };
    let tolerance = select.tolerance.unwrap_or(TOLERANCE);
    if !(0.0..=1.0).contains(&tolerance) {
        return Err("tolerance is 0 to 1".into());
    }
    let query = ColourQuery {
        at: select.at.map(|[x, y]| Point::new(x, y)),
        colour,
        tolerance,
        everywhere: select.at.is_none(),
        layers: select.layer.clone().map(|layer| vec![layer]),
        within: None,
    };
    // Taking from the selection, or keeping only part of it, the wand looks
    // only inside it: what it finds cannot reach other parts through
    // colour outside.
    let within_selection = matches!(select.mode.as_deref(), Some("subtract" | "intersect"));
    let found = with_sight(|renderer, text, art| {
        if let Some(layer) = &select.layer
            && art.layer(layer).is_none_or(crate::model::Layer::is_mask)
        {
            return Err(format!(
                "there is no layer {layer:?} to find colour on; the layers are {}",
                crate::actions::layer_list(art)
            ));
        }
        let query = ColourQuery {
            within: art
                .selection
                .as_ref()
                .filter(|_| within_selection)
                .map(crate::model::Selection::region),
            ..query
        };
        colour_region(renderer, text, art, &query)
    })?;
    let found = match &select.d {
        Some(d) => Region::of_path(&ui::vello::kurbo::BezPath::from_svg(d).unwrap_or_default())
            .combine(&found, Op::Union),
        None => found,
    };
    if found.is_empty() {
        return Err("no part of the artwork shows that colour".into());
    }
    select.d = Some(path_data(&found.to_path()));
    select.at = None;
    select.colour = None;
    select.tolerance = None;
    select.layer = None;
    Ok(CanvasAction::Select(Box::new(select)))
}

/// Run `look` with what the canvas shows.
///
/// # Errors
/// Returns that no canvas shows the artwork, or `look`'s own error.
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
///
/// # Errors
/// Returns a point outside the artwork, or a render failure.
pub fn colour_region(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    query: &ColourQuery,
) -> Result<Region, String> {
    // Colour is found in the pictures themselves, never in the placeholder
    // the canvas shows while a just-opened photo is still being decoded.
    let patient = std::mem::replace(&mut renderer.pictures.patient, true);
    let found = look_for_colour(renderer, text, art, query);
    renderer.pictures.patient = patient;
    found
}

/// [`colour_region`], its pictures waited for.
fn look_for_colour(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    query: &ColourQuery,
) -> Result<Region, String> {
    let whole = art.bounds();
    let scale = (f64::from(WAND_SIDE) / art.width.max(art.height).max(1.0)).min(2.0);
    let mut eye = Eye {
        renderer,
        text,
        art,
        query,
    };
    let first = eye.look(whole, scale, None)?;
    let found = match closer(art, query, &first, scale) {
        Some((view, closer_scale)) => {
            // Closer, it may only find what it found, and the pixels about
            // its edge.
            let near = first.grown(3.0 / scale);
            let again = eye.look(view, closer_scale, Some(&near))?;
            if again.is_empty() { first } else { again }
        }
        None => first,
    };
    // Found inside `within`, its edge runs a little past it, so taking it
    // from the selection or keeping only it there leaves no sliver along
    // the selection's own edge.
    Ok(found.combine(&Region::of_rect(whole), Op::Intersect))
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

/// What the wand looks with.
struct Eye<'a> {
    renderer: &'a mut Renderer,
    text: &'a mut TextRenderer,
    art: &'a Artwork,
    query: &'a ColourQuery,
}

impl Eye<'_> {
    /// The colour the query names in a picture of `view` at `scale` pixels
    /// per unit, found only inside `near` when given.
    fn look(&mut self, view: Rect, scale: f64, near: Option<&Region>) -> Result<Region, String> {
        let (art, query) = (self.art, self.query);
        let options = Options {
            layers: query.layers.clone(),
            bare: query.layers.is_some(),
            ..Options::default()
        };
        let (renderer, text) = (&mut *self.renderer, &mut *self.text);
        let (rgba, width, height) =
            crate::view_ops::pixels(view, scale, options.bare, |scene, to| {
                renderer.draw(art, scene, to, text, &options);
            })?;
        let (width, height) = (width as usize, height as usize);
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
            &crate::wand::Picture {
                rgba: &rgba,
                width,
                height,
            },
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
}

/// The slow query's resolved geometry, retaining its selection options.
pub fn found(
    mut select: Select,
    found: Region,
    matte: Option<crate::model::Matte>,
) -> crate::actions::CanvasAction {
    let found = match &select.d {
        Some(d) => Region::of_path(&ui::vello::kurbo::BezPath::from_svg(d).unwrap_or_default())
            .combine(&found, Op::Union),
        None => found,
    };
    select.d = Some(path_data(&found.to_path()));
    select.at = None;
    select.subject = None;
    select.refine = None;
    select.layer = None;
    select.tolerance = None;
    select.matte = matte;
    crate::actions::CanvasAction::Select(Box::new(select))
}
