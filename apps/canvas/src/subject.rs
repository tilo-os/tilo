//! Selecting a picture's subject, and refining a selection's edge to the
//! picture: found where the action is authored, in the artwork as the
//! window shows it, like the magic wand.
//!
//! The subject is found by the on-device segmentation model the person set
//! up (through the model gateway; nothing leaves the device). Its edge, or
//! any selection's, is then fitted to the picture's own edges
//! (`tilo_subject::refine_edge`), which keeps fur and hair soft. What is
//! found is plain data: the area as path data, and the soft edge as a
//! picture kept by its content, so every replica applies the same.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's float geometry and pixel sizes bounded by the render's"
)]

use std::cell::RefCell;
use ui::num::usize_of_f64;

use ui::TextRenderer;
use ui::vello::kurbo::{Affine, Point};

use crate::model::{Artwork, Matte};
use crate::region::{Coverage, Op, Region};
use crate::render::{Options, Renderer};
use crate::selection::{Select, UseSelection};

/// The closest a refined edge is looked at, in pixels per unit.
use crate::content_geometry::MOST_PIXELS_PER_UNIT;
/// The most a caller may ask an edge be fitted over, in units.
use crate::content_geometry::MOST_REFINE;
/// How far a subject's edge is fitted to the picture when the caller names
/// no reach, in pixels of the picture it is found in.
pub use crate::content_geometry::REFINE_PIXELS;
/// The longest side of the picture a subject is found and refined in, in
/// pixels.
use crate::content_geometry::SIDE;

/// What the model said about the last subject found, for the answer an
/// agent gets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub model: String,
    pub load_ms: u64,
    pub infer_ms: u64,
    pub refine_ms: u64,
}

thread_local! {
    static LAST: RefCell<Option<Found>> = const { RefCell::new(None) };
}

/// What the last subject found on this thread took, once.
pub fn take_found() -> Option<Found> {
    LAST.with_borrow_mut(Option::take)
}

/// Where to find the model when none is set up, said in a refusal.
pub const SET_UP: &str = "settings://models/u2net-subject";

/// The area `select` asks for by subject or refine, and its soft edge,
/// found here and now (an agent's action waits for it).
///
/// # Errors
/// Returns why: no model is set up (and where to set it up), no subject
/// was found, the reach is out of range, or the artwork cannot be seen.
pub fn find(
    select: &Select,
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
) -> Result<(Region, Option<Matte>), String> {
    let subject = select.subject == Some(true);
    let (area, matte, found) = prepare(select, renderer, text, art)?.finish()?;
    if subject {
        LAST.with_borrow_mut(|last| *last = Some(found));
    }
    Ok((area, matte))
}

/// What the artwork shows, ready for the model and the edge filter: the
/// part drawn on the window's thread; [`Prepared::finish`] does the slow
/// part anywhere, so a person's window keeps drawing meanwhile.
pub struct Prepared {
    content: tilo_content::ContentStore,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    view: ui::vello::kurbo::Rect,
    to_cells: Affine,
    /// The point `at` names, in the render's pixels.
    at: Option<(usize, usize)>,
    /// Where `at` was, in units, for a refusal.
    at_units: Option<[f64; 2]>,
    subject: bool,
    /// The selection to refine when there is no subject to find, as cells.
    current: Option<Vec<bool>>,
    reach: f64,
    bounds: ui::vello::kurbo::Rect,
}

/// Draw what `select` needs to find its area.
///
/// # Errors
/// Returns why the request is malformed or the artwork cannot be drawn.
pub fn prepare(
    select: &Select,
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
) -> Result<Prepared, String> {
    let content = ui::tilo_resource::content::current()
        .ok_or("the artwork's content store is unavailable")?;
    prepare_in(select, renderer, text, art, content)
}

/// Preparation with the originating window's captured store, including
/// when its snapshot is rendered on a worker thread.
pub(crate) fn prepare_in(
    select: &Select,
    renderer: &mut Renderer,
    text: &mut TextRenderer,
    art: &Artwork,
    content: tilo_content::ContentStore,
) -> Result<Prepared, String> {
    let subject = select.subject == Some(true);
    let refine = select.refine.unwrap_or(0.0);
    if !refine.is_finite() || !(0.0..=MOST_REFINE).contains(&refine) {
        return Err(format!("refine is 0 to {MOST_REFINE} units"));
    }
    if select.colour.is_some() {
        return Err("subject and refine find the area themselves; leave colour out".into());
    }
    if let Some([x, y]) = select.at
        && !art.bounds().contains(Point::new(x, y))
    {
        return Err(format!(
            "at [{x}, {y}] is outside the artwork (0, 0, {}, {})",
            art.width, art.height
        ));
    }
    let current = art.selection.as_ref().map(crate::model::Selection::region);
    let (view, scale) = view_of(art, subject, current.as_ref(), refine)?;
    let options = Options {
        layers: select.layer.clone().map(|layer| vec![layer]),
        bare: select.layer.is_some(),
        ..Options::default()
    };
    // The model must see the pictures themselves, not the placeholder the
    // canvas shows while a just-opened photo is still being decoded.
    let patient = std::mem::replace(&mut renderer.pictures.patient, true);
    let shown = crate::view_ops::pixels(view, scale, options.bare, |scene, to| {
        renderer.draw(art, scene, to, text, &options);
    });
    renderer.pictures.patient = patient;
    let (rgba, width, height) = shown?;
    // The render's own pixels per unit, after rounding its size.
    let to_cells = Affine::scale_non_uniform(
        f64::from(width) / view.width().max(1e-9),
        f64::from(height) / view.height().max(1e-9),
    ) * Affine::translate((-view.x0, -view.y0));
    let at = select.at.map(|[x, y]| {
        let cell = to_cells * Point::new(x, y);
        (usize_of_f64(cell.x), usize_of_f64(cell.y))
    });
    let current = if subject {
        None
    } else {
        let Some(current) = &current else {
            return Err("nothing is selected to refine; select an area first".into());
        };
        Some(current.cells(to_cells, width as usize, height as usize))
    };
    let reach = if select.refine.is_some() {
        refine * scale
    } else if subject {
        REFINE_PIXELS
    } else {
        0.0
    };
    Ok(Prepared {
        content,
        rgba,
        width,
        height,
        view,
        to_cells,
        at,
        at_units: select.at,
        subject,
        current,
        reach,
        bounds: art.bounds(),
    })
}

impl Prepared {
    /// Find the subject (asking the model) or take the selection, fit its
    /// edge to the picture, and keep the soft edge as content: the area,
    /// the soft edge, and what it took.
    ///
    /// # Errors
    /// Returns why: no model is set up, no subject or nothing was left.
    pub fn finish(self) -> Result<(Region, Option<Matte>, Found), String> {
        let (columns, rows) = (self.width as usize, self.height as usize);
        let mut found = Found::default();
        let matte = match &self.current {
            None => self.subject_matte(&mut found)?,
            Some(cells) => tilo_subject::Matte::new(
                columns,
                rows,
                cells
                    .iter()
                    .map(|&inside| f32::from(u8::from(inside)))
                    .collect(),
            )
            .ok_or("the selection is too small to refine")?,
        };
        let began = ui::time::now();
        let matte = if self.reach > 0.0 {
            let picture = tilo_subject::Picture {
                rgba: &self.rgba,
                width: columns,
                height: rows,
            };
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a reach of at most a few thousand pixels"
            )]
            tilo_subject::refine_edge(&picture, &matte, self.reach as f32)
                .map_err(|error| error.to_string())?
        } else {
            matte
        };
        found.refine_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        let area = Coverage {
            width: columns,
            height: rows,
            cells: &matte.values,
        }
        .traced(self.to_cells.inverse())
        .combine(&Region::of_rect(self.bounds), Op::Intersect);
        if area.is_empty() {
            return Err(if self.subject {
                "no subject stands out in the picture; select it by hand, or try at on it".into()
            } else {
                "refining left nothing selected".into()
            });
        }
        let soft = self.reach > 0.0
            && matte
                .values
                .iter()
                .any(|value| *value > 0.0 && *value < 1.0);
        let kept = if soft {
            Some(kept_matte(
                &self.content,
                &matte,
                self.view.origin(),
                self.to_cells,
            )?)
        } else {
            None
        };
        Ok((area, kept, found))
    }

    /// The subject the model finds in the render, or with `at` the one
    /// object there.
    fn subject_matte(&self, found: &mut Found) -> Result<tilo_subject::Matte, String> {
        let subject = tilo_subject::find_subject(&self.rgba, self.width, self.height)
            .map_err(|error| {
                if error.needs_model() {
                    format!(
                        "select subject needs the on-device subject model (U²-Net, 176 MB, Apache-2.0), which is not set up: set it up in Settings, Agent Models ({SET_UP}), then select again ({error})"
                    )
                } else {
                    format!("the subject could not be found: {error}")
                }
            })?;
        found.model = format!("{}@{}", subject.model_id, short(&subject.revision));
        found.load_ms = subject.load_ms;
        found.infer_ms = subject.infer_ms;
        let (Some((x, y)), Some([ux, uy])) = (self.at, self.at_units) else {
            return Ok(subject.matte);
        };
        tilo_subject::object_at(&subject.matte, x, y)
            .ok_or_else(|| format!("no object is at [{ux}, {uy}]"))
    }
}

/// The part of the artwork to look at, and how closely: all of it for a
/// subject, the selection and the reach around it for a refine.
fn view_of(
    art: &Artwork,
    subject: bool,
    current: Option<&Region>,
    refine: f64,
) -> Result<(ui::vello::kurbo::Rect, f64), String> {
    if subject {
        return Ok((
            art.bounds(),
            (SIDE / art.width.max(art.height).max(1.0)).min(2.0),
        ));
    }
    let Some(bounds) = current.and_then(Region::bounds) else {
        return Err("nothing is selected to refine; select an area first".into());
    };
    let around = bounds
        .inflate(refine * 2.0 + 1.0, refine * 2.0 + 1.0)
        .intersect(art.bounds());
    let scale = (SIDE / around.width().max(around.height()).max(1.0)).min(MOST_PIXELS_PER_UNIT);
    Ok((around, scale))
}

/// The soft edge kept as content, laid over the artwork where the render
/// it was found in lies.
fn kept_matte(
    content: &tilo_content::ContentStore,
    matte: &tilo_subject::Matte,
    origin: Point,
    to_cells: Affine,
) -> Result<Matte, String> {
    let [per_x, _, _, per_y, _, _] = to_cells.as_coeffs();
    #[allow(
        clippy::cast_precision_loss,
        reason = "a render a few thousand pixels a side"
    )]
    let (width, height) = (matte.width as f64, matte.height as f64);
    Ok(Matte {
        src: store_picture(content, &matte_picture(matte)?)?,
        rect: [origin.x, origin.y, width / per_x, height / per_y],
    })
}

/// The selection's soft edge as a picture: white, as opaque as each pixel
/// is selected.
fn matte_picture(matte: &tilo_subject::Matte) -> Result<Vec<u8>, String> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a coverage clamped to 0..=1 scaled to a byte"
    )]
    let rgba: Vec<u8> = matte
        .values
        .iter()
        .flat_map(|value| [255, 255, 255, (value.clamp(0.0, 1.0) * 255.0).round() as u8])
        .collect();
    let width = u32::try_from(matte.width).map_err(|_| "the matte is too wide")?;
    let height = u32::try_from(matte.height).map_err(|_| "the matte is too tall")?;
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or("the matte is malformed")?;
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|error| format!("the matte could not be kept: {error}"))?;
    Ok(out.into_inner())
}

/// The first characters of a revision, enough to tell it.
fn short(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

/// `using` as authored: masking (or clearing) by a refined edge, each
/// picture on the layer it masks has the old background's colour taken out
/// of its edge first, so the cut-out keeps no halo of it. The cleaned
/// pictures are kept as content and named in the action, so every replica
/// gives the marks the same pixels; undo brings the originals back.
///
/// # Errors
/// Returns why a picture could not be read or cleaned.
pub fn clean(using: &mut UseSelection) -> Result<(), String> {
    if !matches!(using.what.as_str(), "mask" | "clear") || using.defringe == Some(false) {
        return Ok(());
    }
    let pictures = crate::author::with_sight(|_, _, art| {
        let Some(matte) = art
            .selection
            .as_ref()
            .and_then(|selection| selection.matte.clone())
        else {
            return Ok(Vec::new());
        };
        let Ok(layer) = crate::actions::target_layer(art, using.layer.as_deref()) else {
            return Ok(Vec::new());
        };
        // A mask named stands for the layer it masks.
        let owner = art
            .layer(&layer)
            .filter(|layer| layer.is_mask())
            .map_or_else(|| layer.clone(), |mask| mask.mask_of.clone());
        let content = ui::tilo_resource::content::current()
            .ok_or("the artwork's content store is unavailable")?;
        let alpha = matte_alpha(&content, &matte)?;
        let mut cleaned = Vec::new();
        for mark in art.marks_of(&owner) {
            if let Some(src) = clean_picture(&content, mark, &matte, &alpha)? {
                cleaned.push([mark.id.clone(), src]);
            }
        }
        Ok(cleaned)
    })
    // Without a window there is nothing to clean with; the mask is still made.
    .unwrap_or_default();
    if !pictures.is_empty() {
        using.cleaned = Some(pictures);
    }
    Ok(())
}

/// A matte picture's coverage.
fn matte_alpha(
    content: &tilo_content::ContentStore,
    matte: &Matte,
) -> Result<tilo_subject::Matte, String> {
    let bytes = crate::picture::bytes_of(content, &matte.src)
        .ok_or("the selection's refined edge is not on this device; refine it again")?;
    let image = image::load_from_memory(&bytes)
        .map_err(|error| format!("the refined edge cannot be read: {error}"))?
        .to_rgba8();
    let (width, height) = (image.width() as usize, image.height() as usize);
    tilo_subject::Matte::new(
        width,
        height,
        image
            .pixels()
            .map(|pixel| f32::from(pixel.0[3]) / 255.0)
            .collect(),
    )
    .ok_or_else(|| "the refined edge is empty".to_owned())
}

/// `mark`'s picture with the background taken out of its colours along
/// the matte's edge, kept as content; `None` when it is no picture or the
/// edge does not cross it.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "pixel indices bounded by the picture's size; coordinates checked against it before the cast"
)]
fn clean_picture(
    content: &tilo_content::ContentStore,
    mark: &crate::model::Mark,
    matte: &Matte,
    alpha: &tilo_subject::Matte,
) -> Result<Option<String>, String> {
    let crate::model::Shape::Image { src, .. } = &mark.shape else {
        return Ok(None);
    };
    let Some(placement) = crate::picture::Placement::of(&mark.shape) else {
        return Ok(None);
    };
    let Some(bytes) = crate::picture::bytes_of(content, src) else {
        return Ok(None);
    };
    let image = image::load_from_memory(&bytes)
        .map_err(|error| format!("{:?}'s picture cannot be read: {error}", mark.id))?
        .to_rgba8();
    let (width, height) = (image.width() as usize, image.height() as usize);
    let [x, y, w, h] = matte.rect;
    let to_matte = Affine::scale_non_uniform(alpha.width as f64 / w, alpha.height as f64 / h)
        * Affine::translate((-x, -y))
        * mark.affine()
        * placement.picture_to_local((image.width(), image.height()));
    // Each picture pixel's coverage, read from the matte where it lands.
    let mut coverage = vec![0.0_f32; width * height];
    let mut partial = (usize::MAX, usize::MAX, 0, 0);
    for row in 0..height {
        for column in 0..width {
            let at = to_matte * Point::new(column as f64 + 0.5, row as f64 + 0.5);
            let value = sample(alpha, at.x - 0.5, at.y - 0.5);
            coverage[row * width + column] = value;
            if value > 0.004 && value < 0.996 {
                partial = (
                    partial.0.min(column),
                    partial.1.min(row),
                    partial.2.max(column),
                    partial.3.max(row),
                );
            }
        }
    }
    if partial.0 == usize::MAX {
        return Ok(None);
    }
    // Clean the part around the edge, with room for the blurs to see both
    // sides of it.
    let margin = (width.max(height) / 24).clamp(8, 90) + 4;
    let (x0, y0) = (
        partial.0.saturating_sub(margin),
        partial.1.saturating_sub(margin),
    );
    let (x1, y1) = (
        (partial.2 + margin + 1).min(width),
        (partial.3 + margin + 1).min(height),
    );
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut crop = Vec::with_capacity(cw * ch * 4);
    let mut crop_alpha = Vec::with_capacity(cw * ch);
    for row in y0..y1 {
        let start = (row * width + x0) * 4;
        crop.extend_from_slice(&image.as_raw()[start..start + cw * 4]);
        crop_alpha.extend_from_slice(&coverage[row * width + x0..row * width + x1]);
    }
    let crop_alpha = tilo_subject::Matte::new(cw, ch, crop_alpha).ok_or("the edge is empty")?;
    let clean = tilo_subject::defringe(
        &tilo_subject::Picture {
            rgba: &crop,
            width: cw,
            height: ch,
        },
        &crop_alpha,
    )
    .map_err(|error| error.to_string())?;
    let mut out = image.into_raw();
    for (index, row) in (y0..y1).enumerate() {
        let start = (row * width + x0) * 4;
        out[start..start + cw * 4].copy_from_slice(&clean[index * cw * 4..(index + 1) * cw * 4]);
    }
    let picture = image::RgbaImage::from_raw(width as u32, height as u32, out)
        .ok_or("the cleaned picture is malformed")?;
    store_picture(content, &encode_like(&picture, &bytes)?).map(Some)
}

fn store_picture(content: &tilo_content::ContentStore, bytes: &[u8]) -> Result<String, String> {
    let hash = content
        .put(bytes)
        .map_err(|error| format!("the content could not be stored: {error}"))?;
    ui::tilo_actions::content_resource(&hash).ok_or_else(|| "the content has no address".to_owned())
}

/// `picture` encoded as its source was: a photograph that came as a JPEG
/// stays one (at a quality that adds no visible loss), so the cleaned copy
/// costs about what the original did to keep and sync, rather than a PNG
/// several times its size; anything else is kept losslessly as PNG.
fn encode_like(picture: &image::RgbaImage, source: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    let mut out = std::io::Cursor::new(Vec::new());
    let kept = if image::guess_format(source).ok() == Some(image::ImageFormat::Jpeg) {
        let rgb = image::DynamicImage::ImageRgba8(picture.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, CLEANED_JPEG_QUALITY)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
    } else {
        picture.write_to(&mut out, image::ImageFormat::Png)
    };
    kept.map_err(|error| format!("the cleaned picture could not be kept: {error}"))?;
    Ok(out.into_inner())
}

/// The quality a cleaned JPEG photograph is kept at.
const CLEANED_JPEG_QUALITY: u8 = 95;

/// `matte` at a point between pixel centres, interpolated; 0 outside.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "checked to lie inside the matte before the cast"
)]
fn sample(matte: &tilo_subject::Matte, x: f64, y: f64) -> f32 {
    if !(x > -1.0 && y > -1.0 && x < matte.width as f64 && y < matte.height as f64) {
        return 0.0;
    }
    let (fx, fy) = (x.floor(), y.floor());
    let (dx, dy) = ((x - fx) as f32, (y - fy) as f32);
    let at = |cx: f64, cy: f64| {
        if cx < 0.0 || cy < 0.0 {
            0.0
        } else {
            matte.at(cx as usize, cy as usize)
        }
    };
    let top = at(fx, fy) * (1.0 - dx) + at(fx + 1.0, fy) * dx;
    let bottom = at(fx, fy + 1.0) * (1.0 - dx) + at(fx + 1.0, fy + 1.0) * dx;
    top * (1.0 - dy) + bottom * dy
}
