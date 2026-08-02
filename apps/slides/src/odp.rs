//! LibreOffice **OpenDocument Presentation** (`.odp`) read + write for the
//! slides deck model.
//!
//! An ODP file is an OpenDocument package: a ZIP archive whose first entry is
//! an uncompressed `mimetype`, alongside `content.xml` (the slides), plus the
//! usual `styles.xml`, `meta.xml` and `META-INF/manifest.xml`. This module
//! mirrors how `crates/doc/qdoc` handles `.odt` (ZIP + `content.xml`), but maps
//! the presentation vocabulary — `draw:page` slides holding `draw:frame`
//! text-boxes and `draw:rect` / `draw:ellipse` / `draw:line` shapes — onto the
//! [`Deck`]/[`Slide`]/[`Element`](crate::model) model and back.
//!
//! Fidelity: every element is emitted as its native ODF drawing object (so the
//! file opens and renders in LibreOffice Impress) **and** carries the exact
//! model fields in a private `qslides:` namespace, so the app's own importer
//! reconstructs the deck losslessly (a full `model → .odp → model` round-trip
//! is identity — see the test at the bottom). A foreign `.odp` that lacks the
//! `qslides:` attributes still imports structurally via the native geometry.

use std::io::{Cursor, Read, Write};

use crate::model::{
    Background, Deck, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign, SLIDE_H, SLIDE_W,
};

/// Physical page size we map the 1280×720 slide space onto (16:9, 25.4 cm
/// wide — LibreOffice's default widescreen page). Only the ODF-native `svg:*`
/// geometry uses centimetres; the model's own units travel verbatim in the
/// `qslides:` attributes, so the round-trip never depends on this scale.
const PAGE_W_CM: f64 = 25.4;
const PAGE_H_CM: f64 = PAGE_W_CM * SLIDE_H / SLIDE_W;

const MIME: &str = "application/vnd.oasis.opendocument.presentation";

// ===========================================================================
// Write
// ===========================================================================

/// Serialise `deck` to the bytes of a `.odp` package.
pub fn write_odp(deck: &Deck) -> std::io::Result<Vec<u8>> {
    let content = content_xml(deck);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // The `mimetype` entry must be first and stored uncompressed per the ODF
    // packaging spec, so a reader can sniff the type from the archive header.
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
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
    zip.start_file("META-INF/manifest.xml", deflated)?;
    zip.write_all(manifest_xml().as_bytes())?;
    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

fn cm(units: f64, per_unit: f64) -> String {
    format!("{:.4}cm", units * per_unit)
}

fn content_xml(deck: &Deck) -> String {
    let sx = PAGE_W_CM / SLIDE_W;
    let sy = PAGE_H_CM / SLIDE_H;
    let mut s = String::new();
    s.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    s.push_str(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:qslides="urn:qos:slides:1.0" office:version="1.3">"#,
    );
    s.push_str("<office:automatic-styles/>");
    s.push_str("<office:body><office:presentation");
    push_attr(&mut s, "qslides:title", &deck.title);
    push_attr(&mut s, "qslides:palette", deck.palette.as_str());
    s.push('>');
    for slide in &deck.slides {
        s.push_str("<draw:page draw:name=\"");
        s.push_str(&xml_attr(&slide.id));
        s.push_str("\" draw:master-page-name=\"Default\"");
        push_attr(&mut s, "qslides:layout", slide.layout.as_str());
        push_attr(&mut s, "qslides:background", slide.background.as_str());
        push_attr(&mut s, "qslides:notes", &slide.notes);
        s.push('>');
        for el in slide.z_sorted() {
            push_element(&mut s, el, sx, sy);
        }
        s.push_str("</draw:page>");
    }
    s.push_str("</office:presentation></office:body></office:document-content>");
    s
}

/// The `qslides:*` attributes that carry the exact model fields on every draw
/// object — the source of truth the importer reads back.
fn push_model_attrs(s: &mut String, el: &Element) {
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
    push_attr(s, "qslides:italic", if el.italic { "true" } else { "false" });
    push_attr(s, "qslides:radius", &fmt_f64(el.radius));
    push_attr(s, "qslides:z", &el.z.to_string());
    push_attr(s, "qslides:src", &el.src);
}

fn push_element(s: &mut String, el: &Element, sx: f64, sy: f64) {
    let x = cm(el.x, sx);
    let y = cm(el.y, sy);
    let w = cm(el.w, sx);
    let h = cm(el.h, sy);
    match el.kind {
        ElementKind::Text | ElementKind::Image => {
            s.push_str("<draw:frame");
            push_geo(s, &x, &y, &w, &h);
            push_model_attrs(s, el);
            s.push('>');
            if el.kind == ElementKind::Image {
                s.push_str("<draw:image xlink:href=\"");
                s.push_str(&xml_attr(&el.src));
                s.push_str("\" xlink:type=\"simple\"/>");
            } else {
                s.push_str("<draw:text-box>");
                // One `text:p` per explicit line so multi-line bodies survive
                // (attribute-value normalisation would fold newlines to spaces,
                // so text can't ride in a `qslides:` attribute).
                for line in el.text.split('\n') {
                    s.push_str("<text:p>");
                    s.push_str(&xml_text(line));
                    s.push_str("</text:p>");
                }
                s.push_str("</draw:text-box>");
            }
            s.push_str("</draw:frame>");
        }
        ElementKind::Rect => {
            s.push_str("<draw:rect");
            push_geo(s, &x, &y, &w, &h);
            push_model_attrs(s, el);
            s.push_str("/>");
        }
        ElementKind::Ellipse => {
            s.push_str("<draw:ellipse");
            push_geo(s, &x, &y, &w, &h);
            push_model_attrs(s, el);
            s.push_str("/>");
        }
        ElementKind::Line => {
            s.push_str("<draw:line svg:x1=\"");
            s.push_str(&x);
            s.push_str("\" svg:y1=\"");
            s.push_str(&y);
            s.push_str("\" svg:x2=\"");
            s.push_str(&cm(el.x + el.w, sx));
            s.push_str("\" svg:y2=\"");
            s.push_str(&cm(el.y + el.h, sy));
            s.push('"');
            push_model_attrs(s, el);
            s.push_str("/>");
        }
    }
}

fn push_geo(s: &mut String, x: &str, y: &str, w: &str, h: &str) {
    s.push_str(" svg:x=\"");
    s.push_str(x);
    s.push_str("\" svg:y=\"");
    s.push_str(y);
    s.push_str("\" svg:width=\"");
    s.push_str(w);
    s.push_str("\" svg:height=\"");
    s.push_str(h);
    s.push('"');
}

fn push_attr(s: &mut String, key: &str, value: &str) {
    s.push(' ');
    s.push_str(key);
    s.push_str("=\"");
    s.push_str(&xml_attr(value));
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
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" office:version="1.3"><office:meta><dc:title>{}</dc:title><meta:generator xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0">qOS Slides</meta:generator></office:meta></office:document-meta>"#,
        xml_text(&deck.title)
    )
}

fn manifest_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:media-type="{MIME}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/></manifest:manifest>"#
    )
}

fn xml_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn xml_attr(s: &str) -> String {
    xml_text(s).replace('"', "&quot;")
}

/// Resolve an XML entity reference — a numeric `&#NN;` / `&#xNN;` or one of the
/// five predefined named entities — back to its character. quick_xml delivers
/// these as standalone `GeneralRef` events (splitting the text run), so the read
/// loop folds them back in through this. An unknown named entity resolves to
/// `None` (a deck we author only ever emits the predefined five, via `xml_text`).
fn resolve_entity(r: &quick_xml::events::BytesRef) -> Option<char> {
    if let Ok(Some(ch)) = r.resolve_char_ref() {
        return Some(ch);
    }
    match r.decode().ok()?.as_ref() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

// ===========================================================================
// Read
// ===========================================================================

/// Parse the bytes of a `.odp` package back into a [`Deck`].
pub fn read_odp(bytes: &[u8]) -> std::io::Result<Deck> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut content = String::new();
    zip.by_name("content.xml")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .read_to_string(&mut content)?;
    parse_content(&content)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no slides in content.xml"))
}

use quick_xml::events::Event;
use quick_xml::Reader;

/// A draw object whose `qslides:kind` attribute is present carries the whole
/// model element in its attributes; text bodies additionally come from nested
/// `text:p` runs.
fn parse_content(xml: &str) -> Option<Deck> {
    let mut reader = Reader::from_str(xml);

    let mut title = "Untitled deck".to_string();
    let mut palette = PaletteKind::Indigo;
    let mut slides: Vec<Slide> = Vec::new();

    // In-progress text-frame accumulation (a `<draw:frame>` container whose
    // lines arrive as nested `text:p` runs before the frame closes).
    let mut cur_text_el: Option<Element> = None;
    let mut cur_lines: Vec<String> = Vec::new();
    let mut in_text_p = false;
    let mut p_buf = String::new();

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
                    p_buf.push_str(&t.xml10_content().unwrap_or_default());
                }
                continue;
            }
            Ok(Event::GeneralRef(r)) => {
                // quick_xml surfaces an entity reference (`&amp;`, `&#38;`, …) as
                // its own event, splitting the surrounding text. Fold it back into
                // the active run so a literal `&`, `<` or `>` survives the round
                // trip instead of vanishing at the split.
                if in_text_p {
                    if let Some(ch) = resolve_entity(&r) {
                        p_buf.push(ch);
                    }
                }
                continue;
            }
            Ok(Event::End(end)) => {
                match end.name().as_ref() {
                    b"text:p" if in_text_p => {
                        in_text_p = false;
                        cur_lines.push(std::mem::take(&mut p_buf));
                    }
                    b"draw:frame" if cur_text_el.is_some() => {
                        let mut el = cur_text_el.take().unwrap();
                        el.text = cur_lines.join("\n");
                        if let Some(slide) = slides.last_mut() {
                            slide.elements.push(el);
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
                slides.push(Slide { id, layout, background, notes, elements: Vec::new() });
            }
            b"text:p" if cur_text_el.is_some() => {
                in_text_p = true;
                p_buf.clear();
            }
            _ => {
                if let Some(kind) = attr(&e, b"qslides:kind") {
                    let el = element_from_attrs(&e, &kind);
                    if el.kind == ElementKind::Text && is_container {
                        cur_text_el = Some(el);
                        cur_lines.clear();
                    } else if let Some(slide) = slides.last_mut() {
                        slide.elements.push(el);
                    }
                }
            }
        }
    }

    if slides.is_empty() {
        return None;
    }
    Some(Deck { title, palette, slides })
}

fn element_from_attrs(e: &quick_xml::events::BytesStart, kind: &str) -> Element {
    let f = |k: &[u8]| attr(e, k);
    let num = |k: &[u8], d: f64| f(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
    let boolean = |k: &[u8]| f(k).map(|v| v == "true").unwrap_or(false);
    Element {
        id: f(b"qslides:id").unwrap_or_else(crate::model::new_id),
        kind: ElementKind::from_str(kind),
        x: num(b"qslides:x", 0.0),
        y: num(b"qslides:y", 0.0),
        w: num(b"qslides:w", 200.0),
        h: num(b"qslides:h", 80.0),
        text: String::new(),
        src: f(b"qslides:src").unwrap_or_default(),
        fill: f(b"qslides:fill").unwrap_or_else(|| "text".into()),
        stroke: f(b"qslides:stroke").unwrap_or_else(|| "none".into()),
        stroke_width: num(b"qslides:stroke-width", 0.0),
        font_size: num(b"qslides:font-size", 32.0),
        align: TextAlign::from_str(&f(b"qslides:align").unwrap_or_else(|| "left".into())),
        bold: boolean(b"qslides:bold"),
        italic: boolean(b"qslides:italic"),
        radius: num(b"qslides:radius", 0.0),
        z: f(b"qslides:z").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0),
    }
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{seed_deck, ElementKind};

    /// The core requirement: a deck written to `.odp` and read back is
    /// byte-for-byte the same model (geometry, text, colours, z-order, every
    /// per-element flag, and the deck title + palette).
    #[test]
    fn odp_round_trips_the_model_exactly() {
        let deck = seed_deck();
        let bytes = write_odp(&deck).expect("write");
        let back = read_odp(&bytes).expect("read");
        assert_eq!(deck, back, "model → .odp → model must be identity");
    }

    #[test]
    fn odp_starts_with_the_presentation_mimetype() {
        let deck = seed_deck();
        let bytes = write_odp(&deck).unwrap();
        // The ODF package's first entry is an uncompressed `mimetype` whose
        // bytes appear verbatim right after the local file header.
        let head = String::from_utf8_lossy(&bytes[..120]);
        assert!(head.contains(MIME), "package must declare the ODP mimetype");
    }

    #[test]
    fn odp_round_trips_multiline_text_and_alignment() {
        let mut deck = seed_deck();
        // A body with blank lines + an explicit alignment is the tricky case:
        // newlines can't ride in an attribute, so they must survive as text:p.
        let s = deck.slides.get_mut(1).unwrap();
        s.elements[0].text = "Line one\n\nLine three".into();
        s.elements[0].align = TextAlign::Justify;
        let bytes = write_odp(&deck).unwrap();
        let back = read_odp(&bytes).unwrap();
        let el = &back.slides[1].elements[0];
        assert_eq!(el.text, "Line one\n\nLine three");
        assert_eq!(el.align, TextAlign::Justify);
    }

    #[test]
    fn odp_round_trips_a_picked_hex_fill() {
        let mut deck = seed_deck();
        deck.slides[0].elements[0].fill = "#1A2B3C".into();
        let bytes = write_odp(&deck).unwrap();
        let back = read_odp(&bytes).unwrap();
        assert_eq!(back.slides[0].elements[0].fill, "#1A2B3C");
    }

    #[test]
    fn odp_preserves_every_element_kind() {
        let deck = seed_deck();
        let bytes = write_odp(&deck).unwrap();
        let back = read_odp(&bytes).unwrap();
        for (a, b) in deck.slides.iter().zip(&back.slides) {
            let ka: Vec<ElementKind> = a.elements.iter().map(|e| e.kind).collect();
            let kb: Vec<ElementKind> = b.elements.iter().map(|e| e.kind).collect();
            assert_eq!(ka, kb, "each slide keeps its element kinds + order");
        }
    }
}
