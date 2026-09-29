//! The portable engine: hayro, a memory-safe PDF rasteriser, in this process.
//!
//! It answers the same requests as the pdfium backend from the same types:
//! page geometry, rendering, the characters of a page for the text layer,
//! links, annotations and the outline. What it does not do yet:
//!
//! - annotation appearances and form widgets are not drawn (page content
//!   only), and [`Engine::form_fields`] is empty;
//! - export, images wrapped as pages and encrypted documents answer
//!   [`Error::Unavailable`];
//! - a region render crops a full-page render at the same scale (cached for
//!   the tiles that follow) instead of rasterising only the region.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, interpret_page,
};
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, ObjectIdentifier};
use hayro::hayro_syntax::page::{Page, Rotation as PdfRotation};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::kurbo::{self, Affine, BezPath, Shape};
use hayro::{RenderCache, RenderSettings};

use crate::Bitmap;
use crate::error::{Error, Result};
use crate::geom::{PageGeometry, Point, Rect, Rotation};
use crate::model::{
    AnnotationInfo, AnnotationKind, Destination, DocumentInfo, FormField, Link, MAX_RENDER_PIXELS,
    Metadata, OutlineEntry, RenderOptions, ReplyType, Rgba, parse_default_appearance,
};
use crate::text::{MAX_PAGE_CHARS, RawChar};

/// Outline entries read before the rest is ignored: a cyclic or hostile
/// outline cannot make this loop forever.
const MAX_OUTLINE_ENTRIES: usize = 10_000;

struct Doc {
    pdf: Pdf,
    pages: Vec<PageGeometry>,
    /// Page object → index, for destinations that name a page by reference.
    page_ids: HashMap<ObjectIdentifier, usize>,
    /// The last full-page render, which the tiles of one view crop from.
    last_render: Option<FullRender>,
}

struct FullRender {
    page: usize,
    scale: u32,
    rotation: Rotation,
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
}

/// Documents open in the portable engine.
#[derive(Default)]
pub(crate) struct Engine {
    docs: HashMap<u64, Doc>,
}

fn geometry_of(page: &Page<'_>) -> PageGeometry {
    let crop = page.intersected_crop_box();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "PDF coordinates are points; f32 holds every page size exactly enough"
    )]
    let user_box = Rect::from_corners(
        Point::new(crop.x0 as f32, crop.y0 as f32),
        Point::new(crop.x1 as f32, crop.y1 as f32),
    );
    let rotation = match page.rotation() {
        PdfRotation::None => Rotation::None,
        PdfRotation::Horizontal => Rotation::Cw90,
        PdfRotation::Flipped => Rotation::Cw180,
        PdfRotation::FlippedHorizontal => Rotation::Cw270,
    };
    PageGeometry { user_box, rotation }
}

/// A PDF text string: UTF-16 with a byte-order mark, UTF-8 with one, or
/// `PDFDocEncoding` (read as Latin-1, which agrees on every printable code).
fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| {
                u16::from_be_bytes([
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| {
                u16::from_le_bytes([
                    c.first().copied().unwrap_or(0),
                    c.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn dict_text(dict: &Dict<'_>, key: &[u8]) -> String {
    dict.get::<hayro::hayro_syntax::object::String<'_>>(key)
        .map(|s| text_string(s.as_bytes()))
        .unwrap_or_default()
}

fn dict_name(dict: &Dict<'_>, key: &[u8]) -> Option<String> {
    dict.get::<Name<'_>>(key).map(|n| n.as_str().to_owned())
}

fn metadata_text(value: Option<&Vec<u8>>) -> String {
    value.map(|v| text_string(v)).unwrap_or_default()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "PDF coordinates are points; f32 is ample"
)]
fn user_rect(r: hayro::hayro_syntax::object::Rect) -> Rect {
    Rect::from_corners(
        Point::new(r.x0 as f32, r.y0 as f32),
        Point::new(r.x1 as f32, r.y1 as f32),
    )
}

fn pixels_for(value: f32) -> Result<u16> {
    let rounded = value.round();
    if !(rounded.is_finite() && rounded >= 1.0 && rounded <= f32::from(u16::MAX)) {
        return Err(Error::Limit("the render exceeds the pixel budget".into()));
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked to lie in 1..=u16::MAX"
    )]
    Ok(rounded as u16)
}

/// Rotate a tightly packed RGBA image clockwise by `turns` quarter turns.
fn rotate(pixels: &[u8], width: u32, height: u32, turns: u8) -> (Vec<u8>, u32, u32) {
    let turns = turns % 4;
    if turns == 0 {
        return (pixels.to_vec(), width, height);
    }
    let (w, h) = (width as usize, height as usize);
    let (out_w, out_h) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![255u8; out_w.saturating_mul(out_h).saturating_mul(4)];
    let last_x = w.saturating_sub(1);
    let last_y = h.saturating_sub(1);
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turns {
                1 => (last_y.saturating_sub(y), x),
                2 => (last_x.saturating_sub(x), last_y.saturating_sub(y)),
                _ => (y, last_x.saturating_sub(x)),
            };
            let src = y.saturating_mul(w).saturating_add(x).saturating_mul(4);
            let dst = ny
                .saturating_mul(out_w)
                .saturating_add(nx)
                .saturating_mul(4);
            if let (Some(from), Some(to)) = (
                pixels.get(src..src.saturating_add(4)),
                out.get_mut(dst..dst.saturating_add(4)),
            ) {
                to.copy_from_slice(from);
            }
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the dimensions are the input's, swapped"
    )]
    (out, out_w as u32, out_h as u32)
}

impl Engine {
    fn doc(&self, id: u64) -> Result<&Doc> {
        self.docs
            .get(&id)
            .ok_or_else(|| Error::Invalid("the document is closed".into()))
    }

    pub(crate) fn open(
        &mut self,
        id: u64,
        bytes: Vec<u8>,
        password: Option<&str>,
    ) -> Result<DocumentInfo> {
        let pdf = match Pdf::new_with_password(bytes, password.unwrap_or("")) {
            Ok(pdf) => pdf,
            Err(LoadPdfError::Decryption(_)) => return Err(Error::Password),
            Err(LoadPdfError::Invalid) => {
                return Err(Error::Format("the file is not a readable PDF".into()));
            }
        };
        let mut pages = Vec::new();
        let mut page_ids = HashMap::new();
        for (index, page) in pdf.pages().iter().enumerate() {
            pages.push(geometry_of(page));
            if let Some(id) = page.raw().obj_id() {
                page_ids.insert(id, index);
            }
        }
        if pages.is_empty() {
            return Err(Error::Format("the document has no pages".into()));
        }
        let meta = pdf.metadata();
        let metadata = Metadata {
            title: metadata_text(meta.title.as_ref()),
            author: metadata_text(meta.author.as_ref()),
            subject: metadata_text(meta.subject.as_ref()),
            keywords: metadata_text(meta.keywords.as_ref()),
            creator: metadata_text(meta.creator.as_ref()),
            producer: metadata_text(meta.producer.as_ref()),
        };
        let has_form = catalog(&pdf).is_some_and(|c| c.contains_key(b"AcroForm"));
        let info = DocumentInfo {
            pages: pages.clone(),
            metadata,
            has_form,
        };
        self.docs.insert(
            id,
            Doc {
                pdf,
                pages,
                page_ids,
                last_render: None,
            },
        );
        Ok(info)
    }

    pub(crate) fn close(&mut self, id: u64) {
        self.docs.remove(&id);
    }

    pub(crate) fn render(
        &mut self,
        id: u64,
        page: usize,
        options: &RenderOptions,
    ) -> Result<Bitmap> {
        let doc = self
            .docs
            .get_mut(&id)
            .ok_or_else(|| Error::Invalid("the document is closed".into()))?;
        let geometry = *doc
            .pages
            .get(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        let (full_w, full_h) = options.page_pixels(geometry.size());
        if u64::from(full_w).saturating_mul(u64::from(full_h)) > MAX_RENDER_PIXELS {
            return Err(Error::Limit("the render exceeds the pixel budget".into()));
        }
        let scale_key = options.scale.to_bits();
        let cached = doc
            .last_render
            .as_ref()
            .filter(|r| r.page == page && r.scale == scale_key && r.rotation == options.rotation);
        let full = if let Some(full) = cached {
            (Arc::clone(&full.pixels), full.width, full.height)
        } else {
            let pdf_page = doc
                .pdf
                .pages()
                .get(page)
                .ok_or_else(|| Error::Invalid("page out of range".into()))?;
            let size = geometry.size();
            let (base_w, base_h) = (
                pixels_for(size.width * options.scale)?,
                pixels_for(size.height * options.scale)?,
            );
            let pixmap = hayro::render(
                pdf_page,
                &RenderCache::new(),
                &InterpreterSettings::default(),
                &RenderSettings {
                    x_scale: options.scale,
                    y_scale: options.scale,
                    width: Some(base_w),
                    height: Some(base_h),
                    bg_color: WHITE,
                },
            );
            let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
            let (pixels, w, h) = rotate(
                pixmap.data_as_u8_slice(),
                w,
                h,
                options.rotation.quarter_turns(),
            );
            let pixels = Arc::new(pixels);
            doc.last_render = Some(FullRender {
                page,
                scale: scale_key,
                rotation: options.rotation,
                width: w,
                height: h,
                pixels: Arc::clone(&pixels),
            });
            (pixels, w, h)
        };
        let (pixels, w, h) = full;
        let Some(region) = options.region else {
            return Ok(Bitmap::from_parts(w, h, pixels));
        };
        let mut out = vec![
            255u8;
            (region.width as usize)
                .saturating_mul(region.height as usize)
                .saturating_mul(4)
        ];
        let row = (region.width as usize).saturating_mul(4);
        for dy in 0..region.height {
            let sy = region.y.saturating_add(dy);
            if sy >= h || region.x >= w {
                continue;
            }
            let copy = (w.saturating_sub(region.x).min(region.width) as usize).saturating_mul(4);
            let src = (sy as usize)
                .saturating_mul(w as usize)
                .saturating_add(region.x as usize)
                .saturating_mul(4);
            let dst = (dy as usize).saturating_mul(row);
            if let (Some(from), Some(to)) = (
                pixels.get(src..src.saturating_add(copy)),
                out.get_mut(dst..dst.saturating_add(copy)),
            ) {
                to.copy_from_slice(from);
            }
        }
        Ok(Bitmap::from_vec(region.width, region.height, out))
    }

    pub(crate) fn text(&self, id: u64, page: usize) -> Result<Vec<RawChar>> {
        let doc = self.doc(id)?;
        let pdf_page = doc
            .pdf
            .pages()
            .get(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        let crop = pdf_page.intersected_crop_box();
        // Characters are reported before the page's own rotation, as the
        // pdfium backend reports them: y down from the crop box's top.
        let transform = Affine::new([1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1]);
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            transform,
            kurbo::Rect::new(0.0, 0.0, crop.width(), crop.height()),
            &cache,
            pdf_page.xref(),
            InterpreterSettings::default(),
        );
        let mut device = TextDevice::default();
        interpret_page(pdf_page, &mut context, &mut device);
        if device.overflow {
            return Err(Error::Limit("the page has too much text".into()));
        }
        Ok(device.chars)
    }

    pub(crate) fn links(&self, id: u64, page: usize) -> Result<Vec<Link>> {
        let doc = self.doc(id)?;
        let (pdf_page, geometry) = doc.page(page)?;
        let mut out = Vec::new();
        for annot in pdf_page
            .raw()
            .get::<Array<'_>>(b"Annots")
            .unwrap_or_default()
            .iter::<Dict<'_>>()
        {
            if dict_name(&annot, b"Subtype").as_deref() != Some("Link") {
                continue;
            }
            let Some(rect) = annot.get::<hayro::hayro_syntax::object::Rect>(b"Rect") else {
                continue;
            };
            let destination = annot
                .get::<Dict<'_>>(b"A")
                .and_then(|action| doc.action(&action))
                .or_else(|| {
                    annot
                        .get::<Object<'_>>(b"Dest")
                        .and_then(|d| doc.destination(d))
                });
            if let Some(destination) = destination {
                out.push(Link {
                    rect: geometry.rect_to_display(user_rect(rect)),
                    destination,
                });
            }
        }
        Ok(out)
    }

    pub(crate) fn annotations(&self, id: u64, page: usize) -> Result<Vec<AnnotationInfo>> {
        let doc = self.doc(id)?;
        let (pdf_page, geometry) = doc.page(page)?;
        let annots = pdf_page
            .raw()
            .get::<Array<'_>>(b"Annots")
            .unwrap_or_default();
        let ids: Vec<Option<ObjectIdentifier>> = annots
            .raw_iter()
            .map(|item| item.as_obj_ref().map(ObjectIdentifier::from))
            .collect();
        let index_of = |dict: &Dict<'_>, key: &[u8]| {
            let target = ObjectIdentifier::from(dict.get_ref(key)?);
            ids.iter().position(|id| *id == Some(target))
        };
        let mut out = Vec::new();
        for (index, annot) in annots.iter::<Dict<'_>>().enumerate() {
            let Some(rect) = annot.get::<hayro::hayro_syntax::object::Rect>(b"Rect") else {
                continue;
            };
            let kind = annotation_kind(dict_name(&annot, b"Subtype").as_deref().unwrap_or(""));
            let color = annot.get::<Vec<f32>>(b"C").and_then(|c| rgb(&c));
            let quads = annot
                .get::<Vec<f32>>(b"QuadPoints")
                .unwrap_or_default()
                .chunks_exact(8)
                .map(|q| {
                    let points: Vec<Point> = q
                        .chunks_exact(2)
                        .map(|p| {
                            Point::new(
                                p.first().copied().unwrap_or(0.0),
                                p.get(1).copied().unwrap_or(0.0),
                            )
                        })
                        .collect();
                    let first = points.first().copied().unwrap_or_default();
                    let bounds = points
                        .iter()
                        .fold(Rect::from_corners(first, first), |r, p| {
                            r.union(&Rect::from_corners(*p, *p))
                        });
                    geometry.rect_to_display(bounds)
                })
                .collect();
            let ink = annot
                .get::<Array<'_>>(b"InkList")
                .map(|strokes| {
                    strokes
                        .iter::<Vec<f32>>()
                        .map(|stroke| {
                            stroke
                                .chunks_exact(2)
                                .map(|p| {
                                    geometry.to_display(Point::new(
                                        p.first().copied().unwrap_or(0.0),
                                        p.get(1).copied().unwrap_or(0.0),
                                    ))
                                })
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let border_width = annot
                .get::<Dict<'_>>(b"BS")
                .and_then(|bs| bs.get::<f32>(b"W"))
                .or_else(|| {
                    annot
                        .get::<Vec<f32>>(b"Border")
                        .and_then(|b| b.get(2).copied())
                });
            let (font_size, text_color) = parse_default_appearance(&dict_text(&annot, b"DA"));
            out.push(AnnotationInfo {
                index,
                kind,
                rect: geometry.rect_to_display(user_rect(rect)),
                color,
                contents: dict_text(&annot, b"Contents"),
                author: dict_text(&annot, b"T"),
                name: dict_text(&annot, b"NM"),
                modified: dict_text(&annot, b"M"),
                created: dict_text(&annot, b"CreationDate"),
                flags: annot.get::<u32>(b"F").unwrap_or(0),
                quads,
                ink,
                border_width,
                in_reply_to: index_of(&annot, b"IRT"),
                reply_type: if dict_name(&annot, b"RT").as_deref() == Some("Group") {
                    ReplyType::Group
                } else {
                    ReplyType::Reply
                },
                popup: index_of(&annot, b"Popup"),
                font_size,
                text_color,
            });
        }
        Ok(out)
    }

    pub(crate) fn form_fields(&self, id: u64) -> Result<Vec<FormField>> {
        self.doc(id).map(|_| Vec::new())
    }

    pub(crate) fn outline(&self, id: u64) -> Result<Vec<OutlineEntry>> {
        let doc = self.doc(id)?;
        let Some(first) = catalog(&doc.pdf)
            .and_then(|c| c.get::<Dict<'_>>(b"Outlines"))
            .and_then(|o| o.get::<Dict<'_>>(b"First"))
        else {
            return Ok(Vec::new());
        };
        let mut budget = MAX_OUTLINE_ENTRIES;
        let mut seen = HashSet::new();
        Ok(doc.outline_level(first, &mut budget, &mut seen, 0))
    }
}

fn catalog(pdf: &Pdf) -> Option<Dict<'_>> {
    pdf.xref().get::<Dict<'_>>(pdf.xref().root_id())
}

fn rgb(c: &[f32]) -> Option<Rgba> {
    let channel = |v: f32| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 first"
        )]
        let v = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        v
    };
    match c {
        [g] => Some(Rgba::new(channel(*g), channel(*g), channel(*g), 255)),
        [r, g, b] => Some(Rgba::new(channel(*r), channel(*g), channel(*b), 255)),
        [c, m, y, k] => {
            let to = |v: f32| channel((1.0 - v) * (1.0 - k));
            Some(Rgba::new(to(*c), to(*m), to(*y), 255))
        }
        _ => None,
    }
}

fn annotation_kind(subtype: &str) -> AnnotationKind {
    match subtype {
        "Text" => AnnotationKind::Text,
        "Link" => AnnotationKind::Link,
        "FreeText" => AnnotationKind::FreeText,
        "Line" => AnnotationKind::Line,
        "Square" => AnnotationKind::Square,
        "Circle" => AnnotationKind::Circle,
        "Polygon" => AnnotationKind::Polygon,
        "PolyLine" => AnnotationKind::Polyline,
        "Highlight" => AnnotationKind::Highlight,
        "Underline" => AnnotationKind::Underline,
        "Squiggly" => AnnotationKind::Squiggly,
        "StrikeOut" => AnnotationKind::StrikeOut,
        "Stamp" => AnnotationKind::Stamp,
        "Caret" => AnnotationKind::Caret,
        "Ink" => AnnotationKind::Ink,
        "Popup" => AnnotationKind::Popup,
        "FileAttachment" => AnnotationKind::FileAttachment,
        "Widget" => AnnotationKind::Widget,
        "Redact" => AnnotationKind::Redact,
        other => AnnotationKind::Other(other.to_owned()),
    }
}

impl Doc {
    fn page(&self, page: usize) -> Result<(&Page<'_>, PageGeometry)> {
        let geometry = *self
            .pages
            .get(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        let pdf_page = self
            .pdf
            .pages()
            .get(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        Ok((pdf_page, geometry))
    }

    fn outline_level(
        &self,
        first: Dict<'_>,
        budget: &mut usize,
        seen: &mut HashSet<ObjectIdentifier>,
        depth: usize,
    ) -> Vec<OutlineEntry> {
        let mut out = Vec::new();
        let mut next = Some(first);
        while let Some(item) = next.take() {
            if *budget == 0 || depth > 64 {
                break;
            }
            *budget = budget.saturating_sub(1);
            if let Some(id) = item.obj_id()
                && !seen.insert(id)
            {
                break;
            }
            let destination = item
                .get::<Dict<'_>>(b"A")
                .and_then(|action| self.action(&action))
                .or_else(|| {
                    item.get::<Object<'_>>(b"Dest")
                        .and_then(|d| self.destination(d))
                });
            let children = item
                .get::<Dict<'_>>(b"First")
                .map(|child| self.outline_level(child, budget, seen, depth.saturating_add(1)))
                .unwrap_or_default();
            out.push(OutlineEntry {
                title: dict_text(&item, b"Title"),
                destination,
                children,
            });
            next = item.get::<Dict<'_>>(b"Next");
        }
        out
    }

    fn action(&self, action: &Dict<'_>) -> Option<Destination> {
        match dict_name(action, b"S")?.as_str() {
            "URI" => action
                .get::<hayro::hayro_syntax::object::String<'_>>(b"URI")
                .map(|uri| Destination::Uri(String::from_utf8_lossy(uri.as_bytes()).into_owned())),
            "GoTo" => action
                .get::<Object<'_>>(b"D")
                .and_then(|d| self.destination(d)),
            "GoToR" | "Launch" => {
                let file = action.get::<Dict<'_>>(b"F").map_or_else(
                    || dict_text(action, b"F"),
                    |spec| {
                        let unicode = dict_text(&spec, b"UF");
                        if unicode.is_empty() {
                            dict_text(&spec, b"F")
                        } else {
                            unicode
                        }
                    },
                );
                (!file.is_empty()).then_some(Destination::File(file))
            }
            _ => None,
        }
    }

    fn destination(&self, dest: Object<'_>) -> Option<Destination> {
        match dest {
            Object::Array(array) => self.explicit_destination(&array),
            Object::Dict(dict) => dict
                .get::<Object<'_>>(b"D")
                .and_then(|d| self.destination(d)),
            Object::Name(name) => self.named_destination(name.as_ref()),
            Object::String(name) => self.named_destination(name.as_bytes()),
            _ => None,
        }
    }

    fn explicit_destination(&self, array: &Array<'_>) -> Option<Destination> {
        let mut items = array.raw_iter();
        let target = items.next()?;
        let page = match target.as_obj_ref() {
            Some(reference) => *self.page_ids.get(&ObjectIdentifier::from(reference))?,
            None => match array.iter::<f32>().next() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a remote page number; checked non-negative"
                )]
                Some(n) if n >= 0.0 => n as usize,
                _ => return None,
            },
        };
        let geometry = self.pages.get(page)?;
        let kind = array.flex_iter();
        let mut kind = kind;
        let _: Option<Object<'_>> = kind.next();
        let at = match kind.next::<Name<'_>>().as_ref().map(Name::as_str) {
            Some("XYZ") => {
                let left = kind.next::<f32>();
                let top = kind.next::<f32>();
                match (left, top) {
                    (Some(x), Some(y)) => Some(geometry.to_display(Point::new(x, y))),
                    (None, Some(y)) => {
                        Some(geometry.to_display(Point::new(geometry.user_box.x0, y)))
                    }
                    _ => None,
                }
            }
            Some("FitH" | "FitBH") => kind
                .next::<f32>()
                .map(|y| geometry.to_display(Point::new(geometry.user_box.x0, y))),
            _ => None,
        };
        Some(Destination::Page { page, at })
    }

    fn named_destination(&self, name: &[u8]) -> Option<Destination> {
        let catalog = catalog(&self.pdf)?;
        if let Some(found) = catalog
            .get::<Dict<'_>>(b"Names")
            .and_then(|names| names.get::<Dict<'_>>(b"Dests"))
            .and_then(|tree| name_tree_lookup(&tree, name, 0))
        {
            return self.destination(found);
        }
        let old = catalog.get::<Dict<'_>>(b"Dests")?;
        let key = std::str::from_utf8(name).ok()?;
        self.destination(old.get::<Object<'_>>(key.as_bytes())?)
    }
}

fn name_tree_lookup<'a>(node: &Dict<'a>, name: &[u8], depth: usize) -> Option<Object<'a>> {
    if depth > 32 {
        return None;
    }
    if let Some(names) = node.get::<Array<'a>>(b"Names") {
        let mut items = names.flex_iter();
        loop {
            let key = items.next::<hayro::hayro_syntax::object::String<'a>>()?;
            let value = items.next::<Object<'a>>()?;
            if key.as_bytes() == name {
                return Some(value);
            }
        }
    }
    for kid in node.get::<Array<'a>>(b"Kids")?.iter::<Dict<'a>>() {
        if let Some(limits) = kid.get::<Array<'a>>(b"Limits") {
            let mut bounds = limits.iter::<hayro::hayro_syntax::object::String<'a>>();
            if let (Some(low), Some(high)) = (bounds.next(), bounds.next())
                && (name < low.as_bytes() || name > high.as_bytes())
            {
                continue;
            }
        }
        if let Some(found) = name_tree_lookup(&kid, name, depth.saturating_add(1)) {
            return Some(found);
        }
    }
    None
}

/// Collects the page's glyphs in content order as [`RawChar`]s, inserting
/// the word spaces the content stream leaves implicit in glyph positions.
#[derive(Default)]
struct TextDevice {
    chars: Vec<RawChar>,
    overflow: bool,
}

impl TextDevice {
    fn push(&mut self, unicode: u32, bbox: Rect, loose: Rect, font_size: f32) {
        if self.chars.len() >= MAX_PAGE_CHARS {
            self.overflow = true;
            return;
        }
        if let Some(prev) = self.chars.last().copied() {
            let same_line = (prev.loose.y1 - loose.y1).abs() < font_size.max(prev.font_size) * 0.5;
            let gap = loose.x0 - prev.loose.x1;
            let spaces = |c: u32| char::from_u32(c).is_some_and(char::is_whitespace);
            if same_line && gap > font_size * 0.15 && !spaces(prev.unicode) && !spaces(unicode) {
                self.chars.push(RawChar {
                    unicode: u32::from(' '),
                    index: u32::try_from(self.chars.len()).unwrap_or(u32::MAX),
                    bbox: Rect::default(),
                    loose: Rect::from_corners(
                        Point::new(prev.loose.x1, loose.y0),
                        Point::new(loose.x0, loose.y1),
                    ),
                    font_size,
                    flags: crate::text::raw_flags::GENERATED,
                });
            }
        }
        self.chars.push(RawChar {
            unicode,
            index: u32::try_from(self.chars.len()).unwrap_or(u32::MAX),
            bbox,
            loose,
            font_size,
            flags: if unicode == 0 {
                crate::text::raw_flags::MAP_ERROR
            } else {
                0
            },
        });
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "page coordinates in points; f32 is ample"
)]
fn to_rect(r: kurbo::Rect) -> Rect {
    Rect::from_corners(
        Point::new(r.x0 as f32, r.y0 as f32),
        Point::new(r.x1 as f32, r.y1 as f32),
    )
}

impl<'a> Device<'a> for TextDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'a>,
        _: &GlyphDrawMode,
    ) {
        let full = std::ops::Mul::mul(transform, glyph_transform);
        #[allow(clippy::cast_possible_truncation, reason = "a font size in points")]
        let font_size = (full.determinant().abs().sqrt() * 1000.0) as f32;
        // Glyph space is 1000 units per em for every glyph hayro reports.
        let outline = match glyph {
            Glyph::Outline(outline) => outline.outline().bounding_box(),
            Glyph::Type3(_) => kurbo::Rect::ZERO,
        };
        let (x0, x1) = if outline.width() > 0.0 {
            (outline.x0.min(0.0), outline.x1)
        } else {
            (0.0, 500.0)
        };
        let loose = to_rect(full.transform_rect_bbox(kurbo::Rect::new(x0, -216.0, x1, 800.0)));
        let bbox = if outline.width() > 0.0 && outline.height() > 0.0 {
            to_rect(full.transform_rect_bbox(outline))
        } else {
            Rect::default()
        };
        let text = glyph.as_unicode().map(|s| match s {
            hayro::hayro_interpret::hayro_cmap::BfString::Char(c) => c.to_string(),
            hayro::hayro_interpret::hayro_cmap::BfString::String(s) => s,
        });
        match text {
            Some(text) if !text.is_empty() => {
                let count = text.chars().count().max(1);
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a ligature expands to a handful of characters"
                )]
                let step = loose.width() / count as f32;
                for (i, ch) in text.chars().enumerate() {
                    #[allow(clippy::cast_precision_loss, reason = "as above")]
                    let x = loose.x0 + step * i as f32;
                    let part =
                        Rect::from_corners(Point::new(x, loose.y0), Point::new(x + step, loose.y1));
                    let tight = if count == 1 { bbox } else { part };
                    self.push(u32::from(ch), tight, part, font_size);
                }
            }
            _ => self.push(0, bbox, loose, font_size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings_decode_every_encoding() {
        assert_eq!(text_string(b"Plain"), "Plain");
        assert_eq!(text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]), "Hi");
        assert_eq!(text_string(&[0xEF, 0xBB, 0xBF, b'o', b'k']), "ok");
    }

    #[test]
    fn quarter_turns_rotate_clockwise() {
        // 2×1: red, blue.
        let px = [255, 0, 0, 255, 0, 0, 255, 255];
        let (out, w, h) = rotate(&px, 2, 1, 1);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, [255, 0, 0, 255, 0, 0, 255, 255]);
        let (out, w, h) = rotate(&px, 2, 1, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, [0, 0, 255, 255, 255, 0, 0, 255]);
    }
}
