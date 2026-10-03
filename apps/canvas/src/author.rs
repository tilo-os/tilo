//! What only the window that authors an action can find: colour as the
//! artwork shows it, for the magic wand and selecting by colour.
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

use ui::TextRenderer;
use ui::vello::kurbo::{Affine, Point};

use crate::model::{Artwork, path_data};
use crate::region::{Mask, Op, Region};
use crate::render::{Options, Renderer};
use crate::selection::Select;

/// The longest side of the picture colour is found in, in pixels.
const WAND_SIDE: f64 = 1600.0; // a sampling budget in pixels, not on-screen spacing
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
}

/// Turn `select`'s colour into the area it finds, in `d`.
///
/// # Errors
/// Returns why the colour cannot be found: no window shows the artwork, or
/// the query is malformed.
pub fn resolve(select: &mut Select) -> Result<(), String> {
    if !select.looks() {
        return Ok(());
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
        layers: None,
    };
    let found = with_sight(|renderer, text, art| colour_region(renderer, text, art, &query))?;
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
    Ok(())
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
        let art = Rc::clone(&sight.art);
        let text = sight.text.get_or_insert_with(TextRenderer::new);
        let mut renderer = renderer
            .try_borrow_mut()
            .map_err(|_| "the canvas is drawing; try again".to_owned())?;
        look(&mut renderer, text, &art)
    })
}

/// The area of `art` showing the colour `query` names, found in a picture
/// of it.
///
/// # Errors
/// Returns a point outside the artwork, or a render failure.
pub fn colour_region(
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    query: &ColourQuery,
) -> Result<Region, String> {
    let scale = (WAND_SIDE / art.width.max(art.height).max(1.0)).min(2.0);
    let options = Options {
        layers: query.layers.clone(),
        bare: query.layers.is_some(),
        ..Options::default()
    };
    let (rgba, width, height) =
        crate::view_ops::pixels(art.bounds(), scale, options.bare, |scene, to| {
            renderer.draw(art, scene, to, text, &options);
        })?;
    let (width, height) = (width as usize, height as usize);
    let pixel = |x: usize, y: usize| -> [u8; 4] {
        let start = (y * width + x) * 4;
        rgba.get(start..start + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .unwrap_or_default()
    };
    let seed = match query.at {
        Some(at) => {
            let cell = Affine::scale(scale) * at;
            if !(0.0..f64::from(u32::try_from(width).unwrap_or(u32::MAX))).contains(&cell.x)
                || !(0.0..f64::from(u32::try_from(height).unwrap_or(u32::MAX))).contains(&cell.y)
            {
                return Err(format!(
                    "at [{}, {}] is outside the artwork (0, 0, {}, {})",
                    at.x, at.y, art.width, art.height
                ));
            }
            Some((index(cell.x), index(cell.y)))
        }
        None => None,
    };
    let target = match (seed, query.colour) {
        (_, Some([r, g, b])) => [r, g, b, 255],
        (Some((x, y)), None) => pixel(x, y),
        (None, None) => return Err("give at or colour".into()),
    };
    let reach = (query.tolerance * 255.0).round();
    let like = |colour: [u8; 4]| {
        colour
            .iter()
            .zip(target)
            .all(|(a, b)| (f64::from(*a) - f64::from(b)).abs() <= reach)
    };
    let mut cells = vec![false; width * height];
    match seed.filter(|_| !query.everywhere) {
        Some((x, y)) => flood(&mut cells, (width, height), (x, y), |x, y| {
            like(pixel(x, y))
        }),
        None => {
            for (cell, colour) in cells.iter_mut().zip(rgba.chunks_exact(4)) {
                *cell = <[u8; 4]>::try_from(colour).is_ok_and(like);
            }
        }
    }
    let mask = Mask {
        width,
        height,
        cells,
    };
    Ok(mask.traced(Affine::scale(1.0 / scale)))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "checked to lie inside the picture before the cast"
)]
fn index(value: f64) -> usize {
    value.floor().max(0.0) as usize
}

/// Set every cell reachable from `start` through cells `inside` says are
/// in, one row's run at a time.
fn flood(
    cells: &mut [bool],
    (width, height): (usize, usize),
    start: (usize, usize),
    inside: impl Fn(usize, usize) -> bool,
) {
    let mut seen = vec![false; cells.len()];
    let mut stack = vec![start];
    while let Some((x, y)) = stack.pop() {
        let at = y * width + x;
        if seen.get(at).copied().unwrap_or(true) || !inside(x, y) {
            continue;
        }
        // The run of the row through (x, y).
        let mut left = x;
        while left > 0
            && !seen.get(at - (x - left) - 1).copied().unwrap_or(true)
            && inside(left - 1, y)
        {
            left -= 1;
        }
        let mut right = x;
        while right + 1 < width
            && !seen.get(at + (right - x) + 1).copied().unwrap_or(true)
            && inside(right + 1, y)
        {
            right += 1;
        }
        for column in left..=right {
            let cell = y * width + column;
            if let Some(slot) = seen.get_mut(cell) {
                *slot = true;
            }
            if let Some(slot) = cells.get_mut(cell) {
                *slot = true;
            }
            if y > 0 {
                stack.push((column, y - 1));
            }
            if y + 1 < height {
                stack.push((column, y + 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_stays_inside_its_colour() {
        // Two columns of in-cells joined at the bottom, and an island.
        let (width, height) = (7, 5);
        let inside = |x: usize, y: usize| x == 1 || x == 3 || y == 4 || (x == 6 && y == 0);
        let mut cells = vec![false; width * height];
        flood(&mut cells, (width, height), (1, 0), inside);
        assert!(cells[3], "the other column joins through the bottom row");
        assert!(cells[4 * width + 6], "the bottom row is reached");
        assert!(!cells[6], "the island is not");
        assert!(!cells[width], "nor a cell between the columns");
    }
}
