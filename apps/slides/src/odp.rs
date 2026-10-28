//! `LibreOffice` **`OpenDocument` Presentation** (`.odp`) read + write for the
//! slides deck model.
//!
//! An ODP file is an `OpenDocument` package: a ZIP archive whose first entry is
//! an uncompressed `mimetype`, alongside `content.xml` (the slides), plus the
//! usual `styles.xml`, `meta.xml` and `META-INF/manifest.xml`. This module
//! mirrors how `crates/doc/tilo-doc` handles `.odt` (ZIP + `content.xml`), but maps
//! the presentation vocabulary — `draw:page` slides holding `draw:frame`
//! text-boxes and `draw:rect` / `draw:ellipse` / `draw:line` shapes — onto the
//! [`Deck`]/[`Slide`]/[`Element`](crate::model) model and back.
//!
//! Fidelity: every element is emitted as its native ODF drawing object (so the
//! file opens and renders in `LibreOffice` Impress) **and** carries the exact
//! model fields in a private `qslides:` namespace, so the app's own importer
//! reconstructs the deck losslessly (a full `model → .odp → model` round-trip
//! is identity — see the test at the bottom). A foreign `.odp` that lacks the
//! `qslides:` attributes still imports structurally via the native geometry.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "ODF geometry is conventionally x/y/w/h and the streaming parser keeps all XML state in one auditable bounded state machine"
)]

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::Path;

use bounded_zip::{BoundedArchive, ZipError, ZipLimits};
use tilo_content::ContentStore;
use ui::widget::BlockContent;
use vello::peniko::Color;

use crate::model::{
    Background, Deck, DeckLook, Element, ElementKind, Layout, Palette, PaletteKind, SLIDE_H,
    SLIDE_W, Slide, TextAlign,
};

/// Physical page size we map the 1280×720 slide space onto (16:9, 25.4 cm
/// wide — `LibreOffice`'s default widescreen page). Only the ODF-native `svg:*`
/// geometry uses centimetres; the model's own units travel verbatim in the
/// `qslides:` attributes, so the round-trip never depends on this scale.
const PAGE_W_CM: f64 = 25.4;
const PAGE_H_CM: f64 = PAGE_W_CM * SLIDE_H / SLIDE_W;

const MIME: &str = "application/vnd.oasis.opendocument.presentation";
/// Every imported office package is checked before XML parsing or media
/// extraction.  These limits bound both deliberately hostile archives and a
/// mistaken attempt to open an unrelated disk image as a presentation.
pub(crate) const ARCHIVE_LIMITS: ZipLimits = ZipLimits {
    max_parts: 4_096,
    max_part_bytes: 32 * 1024 * 1024,
    max_total_bytes: 128 * 1024 * 1024,
};
pub(crate) const MAX_XML_MEMBER_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_IMPORTED_PICTURE_BYTES: usize = 24 * 1024 * 1024;
const MAX_IMPORTED_TEXT_CHARS: usize = 100_000;

// ===========================================================================
// Write
// ===========================================================================

/// Serialise `deck` to the bytes of a `.odp` package, bundling the pictures
/// `content` holds for its image elements.
///
/// # Errors
///
/// Returns an error if the ZIP writer fails (an I/O error on the underlying
/// cursor, which does not happen in practice since it writes to memory).
pub fn write_odp(deck: &Deck, content: &ContentStore) -> std::io::Result<Vec<u8>> {
    let pictures = collect_pictures(deck, content);
    let picture_hrefs: HashMap<&str, &str> = pictures
        .iter()
        .map(|picture| (picture.source.as_str(), picture.href.as_str()))
        .collect();
    let shown = crate::render::shown_text_heights(deck);
    let content = content_xml(deck, &picture_hrefs, &shown);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // The `mimetype` entry must be first and stored uncompressed per the ODF
    // packaging spec, so a reader can sniff the type from the archive header.
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("mimetype", stored)?;
    zip.write_all(MIME.as_bytes())?;
    zip.start_file("content.xml", deflated)?;
    zip.write_all(content.as_bytes())?;
    zip.start_file("styles.xml", deflated)?;
    zip.write_all(styles_xml().as_bytes())?;
    zip.start_file("meta.xml", deflated)?;
    zip.write_all(meta_xml(deck).as_bytes())?;
    for picture in &pictures {
        zip.start_file(&picture.href, deflated)?;
        zip.write_all(&picture.bytes)?;
    }
    zip.start_file("META-INF/manifest.xml", deflated)?;
    zip.write_all(manifest_xml(&pictures).as_bytes())?;
    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

fn cm(units: f64, per_unit: f64) -> String {
    format!("{:.4}cm", units * per_unit)
}

/// The document's content. `shown` holds each text box's height as Slides
/// shows it ([`crate::render::shown_text_heights`]): its frame here, its own
/// height the text box's least, so a box its words outgrew is not cut short.
fn content_xml(
    deck: &Deck,
    picture_hrefs: &HashMap<&str, &str>,
    shown: &HashMap<String, f64>,
) -> String {
    let sx = PAGE_W_CM / SLIDE_W;
    let sy = PAGE_H_CM / SLIDE_H;
    // The deck's colours as Slides draws them, in its own look: the file
    // shows the deck as every collaborator sees it.
    let palette = Palette::resolve(deck.palette, deck.look);
    let mut styles = AutoStyles::default();
    let mut body = String::new();
    body.push_str("<office:body><office:presentation");
    push_attr(&mut body, "qslides:title", &deck.title);
    push_attr(&mut body, "qslides:palette", deck.palette.as_str());
    push_attr(&mut body, "qslides:look", deck.look.as_str());
    body.push('>');
    for slide in &deck.slides {
        let on = palette.on(slide.background);
        let page = styles.name(
            "drawing-page",
            &format!(
                r#"<style:drawing-page-properties draw:fill="solid" draw:fill-color="{}"/>"#,
                ui::color::to_hex(on.slide_bg(slide.background))
            ),
        );
        body.push_str("<draw:page draw:name=\"");
        body.push_str(&quick_xml::escape::escape(&slide.id));
        body.push_str("\" draw:master-page-name=\"Default\"");
        push_attr(&mut body, "draw:style-name", &page);
        push_attr(&mut body, "qslides:layout", slide.layout.as_str());
        push_attr(&mut body, "qslides:background", slide.background.as_str());
        push_attr(&mut body, "qslides:notes", &slide.notes);
        body.push('>');
        for el in slide.z_sorted() {
            let look = Look {
                palette: &on,
                slide,
            };
            push_element(
                &mut body,
                el,
                (sx, sy),
                (picture_hrefs, shown),
                (&mut styles, look),
            );
        }
        body.push_str("</draw:page>");
    }
    body.push_str("</office:presentation></office:body></office:document-content>");
    let mut s = String::new();
    s.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    s.push_str(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:qslides="urn:tilo:slides:1.0" office:version="1.3">"#,
    );
    // The pages', shapes' and runs' colours, then the lists' markers.
    s.push_str("<office:automatic-styles>");
    s.push_str(&styles.xml);
    s.push_str(concat!(
        r#"<text:list-style style:name="qsL"><text:list-level-style-bullet text:level="1" text:bullet-char="•"/></text:list-style>"#,
        r#"<text:list-style style:name="qsN"><text:list-level-style-number text:level="1" style:num-format="1" style:num-suffix="."/></text:list-style>"#,
        "</office:automatic-styles>",
    ));
    s.push_str(&body);
    s
}

/// The automatic styles a document's pages, shapes and runs name, one per
/// distinct look.
#[derive(Default)]
struct AutoStyles {
    xml: String,
    names: HashMap<String, String>,
}

impl AutoStyles {
    /// The name of the `family` style holding `properties`, written once.
    fn name(&mut self, family: &str, properties: &str) -> String {
        let key = format!("{family}\u{0}{properties}");
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let name = format!("qs{}", self.names.len() + 1);
        self.xml.push_str("<style:style");
        push_attr(&mut self.xml, "style:name", &name);
        push_attr(&mut self.xml, "style:family", family);
        self.xml.push('>');
        self.xml.push_str(properties);
        self.xml.push_str("</style:style>");
        self.names.insert(key, name.clone());
        name
    }

    /// A text run's style: its ink, weight and slant.
    fn run(&mut self, ink: Color, bold: bool, italic: bool) -> String {
        let mut properties = String::from("<style:text-properties");
        push_attr(&mut properties, "fo:color", &ui::color::to_hex(ink));
        if bold {
            push_attr(&mut properties, "fo:font-weight", "bold");
        }
        if italic {
            push_attr(&mut properties, "fo:font-style", "italic");
        }
        properties.push_str("/>");
        self.name("text", &properties)
    }

    /// A drawing object's style: its fill and outline, as Slides draws them.
    fn graphic(&mut self, fill: Option<Color>, stroke: Option<(Color, f64)>) -> String {
        let mut properties = String::from("<style:graphic-properties");
        match fill {
            Some(fill) => {
                push_attr(&mut properties, "draw:fill", "solid");
                push_attr(&mut properties, "draw:fill-color", &ui::color::to_hex(fill));
                let alpha = fill.to_rgba8().a;
                if alpha < u8::MAX {
                    let percent = f64::from(alpha) * 100.0 / 255.0;
                    push_attr(&mut properties, "draw:opacity", &format!("{percent:.0}%"));
                }
            }
            None => push_attr(&mut properties, "draw:fill", "none"),
        }
        match stroke {
            Some((color, width)) => {
                push_attr(&mut properties, "draw:stroke", "solid");
                push_attr(
                    &mut properties,
                    "svg:stroke-color",
                    &ui::color::to_hex(color),
                );
                push_attr(
                    &mut properties,
                    "svg:stroke-width",
                    &cm(width, PAGE_W_CM / SLIDE_W),
                );
            }
            None => push_attr(&mut properties, "draw:stroke", "none"),
        }
        properties.push_str("/>");
        self.name("graphic", &properties)
    }
}

/// What an element's colours resolve against: the deck's palette on its
/// slide's background, and the slide (a text box's ink follows what it
/// stands on).
#[derive(Clone, Copy)]
struct Look<'a> {
    palette: &'a Palette,
    slide: &'a Slide,
}

/// The `qslides:*` attributes that carry the exact model fields on every draw
/// object — the source of truth the importer reads back.
fn push_model_attrs(s: &mut String, el: &Element, exported_src: Option<&str>) {
    push_attr(s, "qslides:id", &el.id);
    push_attr(s, "qslides:kind", el.kind.as_str());
    push_attr(s, "qslides:x", &fmt_f64(el.x));
    push_attr(s, "qslides:y", &fmt_f64(el.y));
    push_attr(s, "qslides:w", &fmt_f64(el.w));
    push_attr(s, "qslides:h", &fmt_f64(el.h));
    push_attr(s, "qslides:fill", &el.fill);
    push_attr(s, "qslides:stroke", &el.stroke);
    push_attr(s, "qslides:stroke-width", &fmt_f64(el.stroke_width));
    push_attr(s, "qslides:font-size", &fmt_f64(el.font_size));
    push_attr(s, "qslides:align", el.align.as_str());
    push_attr(s, "qslides:bold", if el.bold { "true" } else { "false" });
    push_attr(
        s,
        "qslides:italic",
        if el.italic { "true" } else { "false" },
    );
    push_attr(s, "qslides:radius", &fmt_f64(el.radius));
    push_attr(s, "qslides:z", &el.z.to_string());
    push_attr(s, "qslides:src", exported_src.unwrap_or(&el.src));
}

fn push_element(
    s: &mut String,
    el: &Element,
    (sx, sy): (f64, f64),
    (picture_hrefs, shown): (&HashMap<&str, &str>, &HashMap<String, f64>),
    (styles, look): (&mut AutoStyles, Look<'_>),
) {
    let palette = look.palette;
    let outline = || {
        palette
            .resolve_role(&el.stroke)
            .map(|color| (color, el.stroke_width))
    };
    let gx = cm(el.x, sx);
    let gy = cm(el.y, sy);
    let gw = cm(el.w, sx);
    let gh = cm(shown.get(&el.id).copied().unwrap_or(el.h), sy);
    match el.kind {
        ElementKind::Text | ElementKind::Image => {
            s.push_str("<draw:frame");
            push_geo(s, &gx, &gy, &gw, &gh);
            if el.kind == ElementKind::Text {
                push_attr(s, "draw:style-name", &styles.graphic(None, None));
            }
            let exported_src = if el.kind == ElementKind::Image {
                picture_hrefs.get(el.src.as_str()).copied()
            } else {
                None
            };
            push_model_attrs(s, el, exported_src);
            s.push('>');
            if el.kind == ElementKind::Image {
                s.push_str("<draw:image xlink:href=\"");
                s.push_str(&quick_xml::escape::escape(exported_src.unwrap_or(&el.src)));
                s.push_str("\" xlink:type=\"simple\"/>");
            } else {
                s.push_str("<draw:text-box fo:min-height=\"");
                s.push_str(&cm(el.h, sy));
                s.push_str("\">");
                // One `text:p` per explicit line so multi-line bodies survive
                // (attribute-value normalisation would fold newlines to spaces,
                // so text can't ride in a `qslides:` attribute).
                // A run of list items is one `text:list`, an item each.
                let mut list: Option<&str> = None;
                for block in el.rich_blocks() {
                    let style = match block.kind {
                        ui::widget::BlockKind::Bullet { .. } => Some("qsL"),
                        ui::widget::BlockKind::Numbered { .. } => Some("qsN"),
                        _ => None,
                    };
                    if list.is_some() && list != style {
                        s.push_str("</text:list>");
                    }
                    if let Some(style) = style.filter(|_| list != style) {
                        s.push_str("<text:list text:style-name=\"");
                        s.push_str(style);
                        s.push_str("\">");
                    }
                    list = style;
                    if list.is_some() {
                        s.push_str("<text:list-item>");
                    }
                    s.push_str("<text:p>");
                    // Every run in the ink Slides draws the box's words in.
                    let ink = crate::render::text_ink(palette, look.slide, el);
                    for (text, bold, italic) in crate::model::runs_of(&block) {
                        let text = quick_xml::escape::partial_escape(text);
                        s.push_str("<text:span");
                        push_attr(s, "text:style-name", &styles.run(ink, bold, italic));
                        s.push('>');
                        s.push_str(&text);
                        s.push_str("</text:span>");
                    }
                    s.push_str("</text:p>");
                    if list.is_some() {
                        s.push_str("</text:list-item>");
                    }
                }
                if list.is_some() {
                    s.push_str("</text:list>");
                }
                s.push_str("</draw:text-box>");
            }
            s.push_str("</draw:frame>");
        }
        ElementKind::Rect => {
            s.push_str("<draw:rect");
            push_geo(s, &gx, &gy, &gw, &gh);
            let style = styles.graphic(palette.resolve_role(&el.fill), outline());
            push_attr(s, "draw:style-name", &style);
            push_model_attrs(s, el, None);
            s.push_str("/>");
        }
        ElementKind::Ellipse => {
            s.push_str("<draw:ellipse");
            push_geo(s, &gx, &gy, &gw, &gh);
            let style = styles.graphic(palette.resolve_role(&el.fill), outline());
            push_attr(s, "draw:style-name", &style);
            push_model_attrs(s, el, None);
            s.push_str("/>");
        }
        ElementKind::Line => {
            s.push_str("<draw:line svg:x1=\"");
            s.push_str(&gx);
            s.push_str("\" svg:y1=\"");
            s.push_str(&gy);
            s.push_str("\" svg:x2=\"");
            s.push_str(&cm(el.x + el.w, sx));
            s.push_str("\" svg:y2=\"");
            s.push_str(&cm(el.y + el.h, sy));
            s.push('"');
            // As the canvas strokes a line: its outline, else its fill.
            let color = palette
                .resolve_role(&el.stroke)
                .or_else(|| palette.resolve_role(&el.fill))
                .unwrap_or(palette.accent);
            let style = styles.graphic(None, Some((color, el.stroke_width.max(2.0))));
            push_attr(s, "draw:style-name", &style);
            push_model_attrs(s, el, None);
            s.push_str("/>");
        }
    }
}

fn push_geo(s: &mut String, gx: &str, gy: &str, gw: &str, gh: &str) {
    s.push_str(" svg:x=\"");
    s.push_str(gx);
    s.push_str("\" svg:y=\"");
    s.push_str(gy);
    s.push_str("\" svg:width=\"");
    s.push_str(gw);
    s.push_str("\" svg:height=\"");
    s.push_str(gh);
    s.push('"');
}

fn push_attr(s: &mut String, key: &str, value: &str) {
    s.push(' ');
    s.push_str(key);
    s.push_str("=\"");
    s.push_str(&quick_xml::escape::escape(value));
    s.push('"');
}

/// Format an `f64` so it round-trips exactly through `parse::<f64>()`. Rust's
/// default `Display` for `f64` is already shortest-round-trippable.
fn fmt_f64(v: f64) -> String {
    let mut out = format!("{v}");
    if !out.contains(['.', 'e', 'E', 'n', 'i']) {
        out.push_str(".0");
    }
    out
}

fn styles_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" office:version="1.3"><office:styles/><office:master-styles><draw:layer-set/></office:master-styles></office:document-styles>"#.to_string()
}

fn meta_xml(deck: &Deck) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" office:version="1.3"><office:meta><dc:title>{}</dc:title><meta:generator xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">Tilo Slides</meta:generator></office:meta></office:document-meta>"#,
        quick_xml::escape::partial_escape(&deck.title)
    )
}

fn manifest_xml(pictures: &[BundledPicture]) -> String {
    let mut entries = String::new();
    for picture in pictures {
        entries.push_str("<manifest:file-entry manifest:full-path=\"");
        entries.push_str(&quick_xml::escape::escape(&picture.href));
        entries.push_str("\" manifest:media-type=\"");
        entries.push_str(picture.mime);
        entries.push_str("\"/>");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:media-type="{MIME}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/>{entries}</manifest:manifest>"#
    )
}

struct BundledPicture {
    source: String,
    href: String,
    mime: &'static str,
    bytes: Vec<u8>,
}

/// Collect verified content-addressed pictures exactly once. Missing blobs are
/// intentionally omitted from interchange rather than dereferencing a path
/// supplied by another collaborator.
fn collect_pictures(deck: &Deck, content: &ContentStore) -> Vec<BundledPicture> {
    let mut pictures = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for element in deck.slides.iter().flat_map(|slide| &slide.elements) {
        if element.kind != ElementKind::Image || !seen.insert(element.src.clone()) {
            continue;
        }
        let Some(bytes) = crate::assets::read_asset(content, &element.src) else {
            continue;
        };
        let Some(tilo_content::ImageFormat {
            extension: ext,
            mime,
        }) = tilo_content::image_format(&bytes)
        else {
            continue;
        };
        let href = format!("Pictures/picture-{}.{}", pictures.len() + 1, ext);
        pictures.push(BundledPicture {
            source: element.src.clone(),
            href,
            mime,
            bytes,
        });
    }
    pictures
}

// ===========================================================================
// Read
// ===========================================================================

/// Parse the bytes of a `.odp` package back into a [`Deck`], storing its
/// pictures in `content`.
pub fn read_odp(bytes: &[u8], content: &ContentStore) -> std::io::Result<Deck> {
    let mut zip = open_package(bytes)?;
    let content_xml = read_package_xml(&mut zip, "content.xml")?;
    let mut deck = parse_content(&content_xml).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "no slides in content.xml")
    })?;
    validate_imported_deck(&deck)?;
    let mut extracted = HashMap::new();
    for element in deck
        .slides
        .iter_mut()
        .flat_map(|slide| &mut slide.elements)
        .filter(|element| element.kind == ElementKind::Image)
    {
        if let Some(path) = extracted.get(&element.src) {
            element.src.clone_from(path);
            continue;
        }
        if let Some(path) = extract_picture(&mut zip, &element.src, content) {
            extracted.insert(element.src.clone(), path.clone());
            element.src = path;
        }
    }
    Ok(deck)
}

/// An imported office package (`.odp`/`.pptx`), read under [`ARCHIVE_LIMITS`].
pub(crate) type Package<'a> = BoundedArchive<Cursor<&'a [u8]>>;

/// Opens an untrusted presentation package and refuses it before any XML
/// parsing or media extraction if its entry count or declared sizes breach
/// [`ARCHIVE_LIMITS`].
pub(crate) fn open_package(bytes: &[u8]) -> std::io::Result<Package<'_>> {
    let mut zip = BoundedArchive::from_bytes(bytes, ARCHIVE_LIMITS).map_err(package_error)?;
    zip.check_declared_sizes().map_err(package_error)?;
    Ok(zip)
}

/// Reads an XML part of a presentation package: bounded to
/// [`MAX_XML_MEMBER_BYTES`] and rejected if it declares a DTD.
pub(crate) fn read_package_xml(zip: &mut Package<'_>, name: &str) -> std::io::Result<String> {
    zip.read_xml(name, MAX_XML_MEMBER_BYTES as u64)
        .map_err(package_error)
}

/// Reads an embedded picture, bounded to [`MAX_IMPORTED_PICTURE_BYTES`].
/// A missing or oversized picture is skipped, not an import failure.
pub(crate) fn read_package_picture(zip: &mut Package<'_>, name: &str) -> Option<Vec<u8>> {
    zip.read(name, MAX_IMPORTED_PICTURE_BYTES as u64).ok()
}

fn package_error(error: ZipError) -> std::io::Error {
    let message = match &error {
        ZipError::TooManyEntries { .. } => "presentation archive has too many entries",
        ZipError::PartTooLarge { .. } => "presentation archive member is too large",
        ZipError::PartInflatesTooLarge { .. } => {
            "presentation archive member expands beyond the allowed limit"
        }
        ZipError::TotalTooLarge { .. } => "presentation archive expands beyond the allowed limit",
        _ => return error.into(),
    };
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

pub(crate) fn validate_imported_deck(deck: &Deck) -> std::io::Result<()> {
    if !crate::actions::deck_within_aggregate_limits(deck)
        || deck.title.chars().count() > MAX_IMPORTED_TEXT_CHARS
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "presentation is too large",
        ));
    }
    if deck.slides.iter().any(|slide| {
        slide.notes.chars().count() > MAX_IMPORTED_TEXT_CHARS
            || slide.elements.iter().any(|element| {
                element.text.chars().count() > MAX_IMPORTED_TEXT_CHARS || element.src.len() > 4_096
            })
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "presentation has too much content",
        ));
    }
    Ok(())
}

fn extract_picture(zip: &mut Package<'_>, href: &str, content: &ContentStore) -> Option<String> {
    if !href.starts_with("Pictures/")
        || Path::new(href).file_name()?.to_str()? != href.get("Pictures/".len()..)?
    {
        return None;
    }
    let bytes = read_package_picture(zip, href)?;
    crate::assets::store_asset(content, &bytes)
}

/// Persist picture bytes from an imported office package in `content`, the
/// store ODP imports into. PPTX uses this too, so reopening either format
/// gives the renderer content-addressed bytes without leaking the author's
/// original path.
pub(crate) fn cache_imported_picture(content: &ContentStore, bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_IMPORTED_PICTURE_BYTES {
        return None;
    }
    tilo_content::image_format(bytes)?;
    crate::assets::store_asset(content, bytes)
}

use quick_xml::Reader;
use quick_xml::events::Event;

/// A draw object whose `qslides:kind` attribute is present carries the whole
/// model element in its attributes; text bodies additionally come from nested
/// `text:p` runs.
fn parse_content(xml: &str) -> Option<Deck> {
    let mut reader = Reader::from_str(xml);

    let mut title = "Untitled deck".to_string();
    let mut palette = PaletteKind::Indigo;
    let mut look = DeckLook::default();
    let mut slides: Vec<Slide> = Vec::new();

    // In-progress text-frame accumulation (a `<draw:frame>` container whose
    // lines arrive as nested `text:p` runs before the frame closes).
    let mut cur_text_el: Option<Element> = None;
    let mut cur_lines: Vec<String> = Vec::new();
    let mut in_text_p = false;
    let mut p_buf = String::new();
    // The frame's paragraphs as written, with their bold and italic runs:
    // automatic text styles by name, and the spans open around the text.
    let mut cur_blocks: Vec<BlockContent> = Vec::new();
    let mut p_block = BlockContent::paragraph("", "");
    let mut text_styles: HashMap<String, (bool, bool)> = HashMap::new();
    let mut style_name: Option<String> = None;
    let mut spans: Vec<(bool, bool)> = Vec::new();
    // The lists open around the paragraph being read, numbered or not, and
    // the list styles that number.
    let mut lists: Vec<bool> = Vec::new();
    let mut list_style: Option<String> = None;
    let mut numbered_styles: std::collections::HashSet<String> = ["qsN".to_owned()].into();

    loop {
        // A `Start` opens a container (text frames); an `Empty` is a self-closed
        // shape. Both may carry the model attributes — the `is_container` flag
        // is what routes a text frame into line accumulation vs. immediate push.
        let (e, is_container) = match reader.read_event() {
            Err(_) => return None,
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => (e, true),
            Ok(Event::Empty(e)) => (e, false),
            Ok(Event::Text(t)) => {
                if in_text_p {
                    let text = t.xml10_content().unwrap_or_default();
                    p_buf.push_str(&text);
                    let (bold, italic) = spans.last().copied().unwrap_or_default();
                    crate::model::push_run(&mut p_block, &text, bold, italic);
                }
                continue;
            }
            Ok(Event::CData(data)) => {
                if in_text_p {
                    let text = data.xml10_content().ok()?;
                    p_buf.push_str(&text);
                    let (bold, italic) = spans.last().copied().unwrap_or_default();
                    crate::model::push_run(&mut p_block, &text, bold, italic);
                }
                continue;
            }
            Ok(Event::GeneralRef(r)) => {
                // quick_xml surfaces an entity reference (`&amp;`, `&#38;`, …) as
                // its own event, splitting the surrounding text. Fold it back into
                // the active run so a literal `&`, `<` or `>` survives the round
                // trip instead of vanishing at the split.
                if in_text_p && let Some(ch) = tilo_doc_codecs::resolve_general_ref(&r) {
                    p_buf.push(ch);
                    let (bold, italic) = spans.last().copied().unwrap_or_default();
                    crate::model::push_run(&mut p_block, ch.encode_utf8(&mut [0; 4]), bold, italic);
                }
                continue;
            }
            Ok(Event::End(end)) => {
                match end.name().as_ref() {
                    b"text:p" if in_text_p => {
                        in_text_p = false;
                        spans.clear();
                        if let Some(&numbered) = lists.last() {
                            let indent = u8::try_from(lists.len() - 1).unwrap_or(u8::MAX);
                            p_block.kind = if numbered {
                                ui::widget::BlockKind::Numbered { indent }
                            } else {
                                ui::widget::BlockKind::Bullet { indent }
                            };
                        }
                        cur_lines.push(std::mem::take(&mut p_buf));
                        cur_blocks.push(std::mem::replace(
                            &mut p_block,
                            BlockContent::paragraph("", ""),
                        ));
                    }
                    b"text:span" => {
                        spans.pop();
                    }
                    b"text:list" => {
                        lists.pop();
                    }
                    b"text:list-style" => list_style = None,
                    b"style:style" => style_name = None,
                    b"draw:frame" => {
                        if let Some(mut el) = cur_text_el.take() {
                            el.text = cur_lines.join("\n");
                            // Runs of their own weight or slant keep them;
                            // a box written before runs keeps its own flags.
                            if cur_blocks.iter().any(|block| {
                                !block.marks.is_empty()
                                    || block.kind != ui::widget::BlockKind::Paragraph
                            }) {
                                el.set_blocks(std::mem::take(&mut cur_blocks));
                            }
                            if let Some(slide) = slides.last_mut() {
                                slide.elements.push(el);
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            Ok(_) => continue,
        };
        match e.name().as_ref() {
            b"office:presentation" => {
                if let Some(v) = attr(&e, b"qslides:title") {
                    title = v;
                }
                if let Some(v) = attr(&e, b"qslides:palette") {
                    palette = PaletteKind::from_str(&v);
                }
                if let Some(v) = attr(&e, b"qslides:look") {
                    look = DeckLook::from_str(&v);
                }
            }
            b"draw:page" => {
                let id = attr(&e, b"draw:name").unwrap_or_else(crate::model::new_id);
                let layout = Layout::from_str(
                    &attr(&e, b"qslides:layout").unwrap_or_else(|| "title-content".into()),
                );
                let background = Background::from_str(
                    &attr(&e, b"qslides:background").unwrap_or_else(|| "surface".into()),
                );
                let notes = attr(&e, b"qslides:notes").unwrap_or_default();
                slides.push(Slide {
                    id,
                    layout,
                    background,
                    notes,
                    elements: Vec::new(),
                });
            }
            b"text:p" if cur_text_el.is_some() => {
                in_text_p = true;
                p_buf.clear();
                p_block = BlockContent::paragraph("", "");
            }
            b"text:list" if is_container => {
                let numbered = attr(&e, b"text:style-name")
                    .is_some_and(|name| numbered_styles.contains(&name));
                lists.push(numbered);
            }
            b"text:list-style" => list_style = attr(&e, b"style:name"),
            b"text:list-level-style-number" => {
                if let Some(name) = &list_style {
                    numbered_styles.insert(name.clone());
                }
            }
            b"text:span" if in_text_p && is_container => {
                let inherited = spans.last().copied().unwrap_or_default();
                let style = attr(&e, b"text:style-name")
                    .and_then(|name| text_styles.get(&name).copied())
                    .unwrap_or(inherited);
                spans.push(style);
            }
            b"style:style" => {
                style_name = (attr(&e, b"style:family").as_deref() == Some("text"))
                    .then(|| attr(&e, b"style:name"))
                    .flatten();
            }
            b"style:text-properties" => {
                if let Some(name) = &style_name {
                    let bold = attr(&e, b"fo:font-weight").as_deref() == Some("bold");
                    let italic = attr(&e, b"fo:font-style").as_deref() == Some("italic");
                    text_styles.insert(name.clone(), (bold, italic));
                }
            }
            _ => {
                if let Some(kind) = attr(&e, b"qslides:kind") {
                    let el = element_from_attrs(&e, &kind);
                    if el.kind == ElementKind::Text && is_container {
                        cur_text_el = Some(el);
                        cur_lines.clear();
                        cur_blocks.clear();
                    } else if let Some(slide) = slides.last_mut() {
                        slide.elements.push(el);
                    }
                } else {
                    match e.name().as_ref() {
                        // Foreign ODPs do not have qslides:* fidelity attrs.
                        // Recover their native frame geometry and nested text or
                        // image reference instead of opening an empty deck.
                        b"draw:frame" if is_container => {
                            cur_text_el = Some(native_frame(&e));
                            cur_lines.clear();
                            cur_blocks.clear();
                        }
                        b"draw:image" => {
                            if let Some(frame) = cur_text_el.as_mut() {
                                frame.kind = ElementKind::Image;
                                frame.src = attr(&e, b"xlink:href").unwrap_or_default();
                                frame.fill = "surface".into();
                            }
                        }
                        b"draw:rect" | b"draw:ellipse" | b"draw:line" => {
                            if let Some(slide) = slides.last_mut() {
                                slide.elements.push(native_shape(&e));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if slides.is_empty() {
        return None;
    }
    Some(Deck {
        title,
        palette,
        look,
        slides,
    })
}

fn native_frame(e: &quick_xml::events::BytesStart) -> Element {
    let (x, y, w, h) = native_geometry(e);
    let mut element = Element::text("", x, y, w, h, 32.0);
    element.id = attr(e, b"draw:name").unwrap_or_else(crate::model::new_id);
    element
}

fn native_shape(e: &quick_xml::events::BytesStart) -> Element {
    let kind = match e.name().as_ref() {
        b"draw:ellipse" => ElementKind::Ellipse,
        b"draw:line" => ElementKind::Line,
        _ => ElementKind::Rect,
    };
    let (x, y, w, h) = if kind == ElementKind::Line {
        let x1 = native_length(attr(e, b"svg:x1").as_deref(), true);
        let y1 = native_length(attr(e, b"svg:y1").as_deref(), false);
        let x2 = native_length(attr(e, b"svg:x2").as_deref(), true);
        let y2 = native_length(attr(e, b"svg:y2").as_deref(), false);
        (x1, y1, x2 - x1, y2 - y1)
    } else {
        native_geometry(e)
    };
    let mut element = Element::shape(kind, x, y, w, h);
    element.id = attr(e, b"draw:name").unwrap_or_else(crate::model::new_id);
    element
}

fn native_geometry(e: &quick_xml::events::BytesStart) -> (f64, f64, f64, f64) {
    (
        native_length(attr(e, b"svg:x").as_deref(), true),
        native_length(attr(e, b"svg:y").as_deref(), false),
        native_length(attr(e, b"svg:width").as_deref(), true),
        native_length(attr(e, b"svg:height").as_deref(), false),
    )
}

fn native_length(value: Option<&str>, horizontal: bool) -> f64 {
    let Some(value) = value else { return 0.0 };
    let (number, centimetres_per_unit) = if let Some(number) = value.strip_suffix("cm") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("mm") {
        (number, 0.1)
    } else if let Some(number) = value.strip_suffix("in") {
        (number, 2.54)
    } else if let Some(number) = value.strip_suffix("pt") {
        (number, 2.54 / 72.0)
    } else {
        (value, 1.0)
    };
    let cm = number.trim().parse::<f64>().unwrap_or(0.0) * centimetres_per_unit;
    if horizontal {
        cm * SLIDE_W / PAGE_W_CM
    } else {
        cm * SLIDE_H / PAGE_H_CM
    }
}

fn element_from_attrs(e: &quick_xml::events::BytesStart, kind: &str) -> Element {
    let f = |k: &[u8]| attr(e, k);
    let num = |k: &[u8], d: f64| f(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
    let boolean = |k: &[u8]| f(k).is_some_and(|v| v == "true");
    Element {
        id: f(b"qslides:id").unwrap_or_else(crate::model::new_id),
        kind: ElementKind::from_str(kind),
        x: num(b"qslides:x", 0.0),
        y: num(b"qslides:y", 0.0),
        w: num(b"qslides:w", 200.0),
        h: num(b"qslides:h", 80.0),
        text: String::new(),
        blocks: Vec::new(),
        src: f(b"qslides:src").unwrap_or_default(),
        fill: f(b"qslides:fill").unwrap_or_else(|| "text".into()),
        stroke: f(b"qslides:stroke").unwrap_or_else(|| "none".into()),
        stroke_width: num(b"qslides:stroke-width", 0.0),
        font_size: num(b"qslides:font-size", 32.0),
        align: TextAlign::from_str(&f(b"qslides:align").unwrap_or_else(|| "left".into())),
        bold: boolean(b"qslides:bold"),
        italic: boolean(b"qslides:italic"),
        radius: num(b"qslides:radius", 0.0),
        z: f(b"qslides:z")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
    }
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| tilo_doc_codecs::unescape_attr(&a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bold_word_in_a_text_box_survives_the_round_trip() {
        use ui::widget::marks::{Mark, MarkSpan};
        let content = ContentStore::memory();
        let mut deck = crate::model::seed_deck();
        let mut first = ui::widget::BlockContent::paragraph("", "plain bold");
        first.marks = vec![MarkSpan::new(6..10, Mark::Bold)];
        let mut second = ui::widget::BlockContent::paragraph("", "slanted");
        second.marks = vec![MarkSpan::new(0..7, Mark::Italic)];
        let element = deck.slides[0]
            .elements
            .iter_mut()
            .find(|element| element.kind == crate::model::ElementKind::Text)
            .unwrap();
        element.set_blocks(vec![first, second]);
        let id = element.id.clone();
        let written = element.clone();
        let bytes = write_odp(&deck, &content).expect("write");
        let back = read_odp(&bytes, &content).expect("read");
        let read = back.slides[0]
            .elements
            .iter()
            .find(|element| element.id == id || element.text == written.text)
            .expect("the text box comes back");
        assert_eq!(read.text, "plain bold\nslanted");
        assert_eq!(
            read.rich_blocks(),
            written.rich_blocks(),
            "runs keep their marks"
        );
    }
    use crate::model::{ElementKind, seed_deck};
    use std::io::Read;

    #[test]
    fn odp_round_trips_model_text_and_fills() {
        // Case `odp_round_trips_the_model_exactly`.
        // The core requirement: a deck written to `.odp` and read back is
        // byte-for-byte the same model (geometry, text, colours, z-order, every
        // per-element flag, and the deck title + palette).
        {
            let content = ContentStore::memory();
            let deck = seed_deck();
            let bytes = write_odp(&deck, &content).expect("write");
            let back = read_odp(&bytes, &content).expect("read");
            assert_eq!(deck, back, "model → .odp → model must be identity");
        }

        {
            let content = ContentStore::memory();
            let deck = seed_deck();
            let bytes = write_odp(&deck, &content).unwrap();
            // The ODF package's first entry is an uncompressed `mimetype` whose
            // bytes appear verbatim right after the local file header.
            let head = String::from_utf8_lossy(&bytes[..120]);
            assert!(head.contains(MIME), "package must declare the ODP mimetype");
        }

        {
            let content = ContentStore::memory();
            let mut deck = seed_deck();
            // A body with blank lines + an explicit alignment is the tricky case:
            // newlines can't ride in an attribute, so they must survive as text:p.
            let s = deck.slides.get_mut(1).unwrap();
            s.elements[0].text = "Line one\n\nLine three".into();
            s.elements[0].align = TextAlign::Justify;
            let bytes = write_odp(&deck, &content).unwrap();
            let back = read_odp(&bytes, &content).unwrap();
            let el = &back.slides[1].elements[0];
            assert_eq!(el.text, "Line one\n\nLine three");
            assert_eq!(el.align, TextAlign::Justify);
        }

        {
            let content = ContentStore::memory();
            let mut deck = seed_deck();
            deck.slides[0].elements[0].fill = "#1A2B3C".into();
            let bytes = write_odp(&deck, &content).unwrap();
            let back = read_odp(&bytes, &content).unwrap();
            assert_eq!(back.slides[0].elements[0].fill, "#1A2B3C");
        }
    }

    #[test]
    fn odp_preserves_foreign_content_kinds_and_pictures() {
        {
            let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"><office:body><office:presentation><draw:page draw:name="slide-1"><draw:frame draw:name="title" svg:x="2.54cm" svg:y="1.4288cm" svg:width="20.32cm" svg:height="2.8575cm"><draw:text-box><text:p>Hello from LibreOffice</text:p><text:p>Second line</text:p></draw:text-box></draw:frame><draw:rect draw:name="box" svg:x="5.08cm" svg:y="7.1438cm" svg:width="10.16cm" svg:height="4.2863cm"/></draw:page></office:presentation></office:body></office:document-content>"#;
            let deck = parse_content(xml).expect("foreign ODP content parses");
            assert_eq!(deck.slides.len(), 1);
            assert_eq!(deck.slides[0].elements.len(), 2);
            let text = &deck.slides[0].elements[0];
            assert_eq!(text.kind, ElementKind::Text);
            assert_eq!(text.text, "Hello from LibreOffice\nSecond line");
            assert!((text.x - 128.0).abs() < 0.1);
            assert!((text.w - 1024.0).abs() < 0.1);
            assert_eq!(deck.slides[0].elements[1].kind, ElementKind::Rect);
        }

        {
            let content = ContentStore::memory();
            let deck = seed_deck();
            let bytes = write_odp(&deck, &content).unwrap();
            let back = read_odp(&bytes, &content).unwrap();
            for (a, b) in deck.slides.iter().zip(&back.slides) {
                let ka: Vec<ElementKind> = a.elements.iter().map(|e| e.kind).collect();
                let kb: Vec<ElementKind> = b.elements.iter().map(|e| e.kind).collect();
                assert_eq!(ka, kb, "each slide keeps its element kinds + order");
            }
        }

        {
            let content = ContentStore::memory();
            let source = std::env::temp_dir()
                .join(format!("tilo-slides-picture-{}.png", std::process::id()));
            // Packaging only needs the real image signature; rendering/decode is
            // separately covered by ImageStore tests.
            let picture = b"\x89PNG\r\n\x1a\nportable-picture";
            std::fs::write(&source, picture).unwrap();
            let asset = crate::assets::import_asset_file(&content, &source).unwrap();
            let mut deck = seed_deck();
            deck.slides[0]
                .elements
                .push(Element::image(&asset, 40.0, 60.0, 320.0, 180.0));

            let bytes = write_odp(&deck, &content).unwrap();
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
            let mut bundled = Vec::new();
            zip.by_name("Pictures/picture-1.png")
                .unwrap()
                .read_to_end(&mut bundled)
                .unwrap();
            assert_eq!(bundled, picture);
            let mut manifest = String::new();
            zip.by_name("META-INF/manifest.xml")
                .unwrap()
                .read_to_string(&mut manifest)
                .unwrap();
            assert!(manifest.contains("Pictures/picture-1.png"));
            drop(zip);

            let reopened = read_odp(&bytes, &content).unwrap();
            let image = reopened.slides[0]
                .elements
                .iter()
                .find(|element| element.kind == ElementKind::Image)
                .unwrap();
            assert!(crate::assets::is_asset_id(&image.src));
            assert_eq!(
                crate::assets::read_asset(&content, &image.src).as_deref(),
                Some(&picture[..])
            );

            let _ = std::fs::remove_file(source);
        }
    }
}
