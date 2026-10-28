//! Useful, deliberately bounded PowerPoint (`.pptx`) interchange.
//!
//! Slides, text boxes, rectangles, ellipses, lines and readable local pictures
//! map to native DrawingML objects. Model-backed fills, outlines, stroke widths,
//! text colour and emphasis are emitted as native DrawingML too. PowerPoint
//! features which do not exist in
//! the Tilo model (masters, charts, SmartArt, transitions and animation) are
//! intentionally not invented; the file UI tells users about that boundary.

#![allow(
    clippy::doc_markdown,
    clippy::collapsible_if,
    clippy::drop_non_drop,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::many_single_char_names,
    clippy::obfuscated_if_else,
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines,
    clippy::unnested_or_patterns,
    reason = "the compact DrawingML writer deliberately mirrors its XML grammar; expanding each relationship fragment obscures format fidelity without changing bounded behavior"
)]

use std::collections::HashMap;
use std::io::{Cursor, Write};

use crate::odp::Package;
use tilo_content::ContentStore;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::model::{
    Background, Deck, DeckLook, Element, ElementKind, Layout, Palette, PaletteKind, Slide,
    TextAlign,
};
use vello::peniko::Color;

const EMU: f64 = 9_525.0;

/// Serialise `deck` to the bytes of a `.pptx` package, bundling the pictures
/// `content` holds for its image elements.
pub fn write_pptx(deck: &Deck, content: &ContentStore) -> std::io::Result<Vec<u8>> {
    let shown = crate::render::shown_text_heights(deck);
    let mut pictures = Vec::<Picture>::new();
    for slide in &deck.slides {
        for el in &slide.elements {
            if el.kind != ElementKind::Image || pictures.iter().any(|p| p.src == el.src) {
                continue;
            }
            if let Some(bytes) = crate::assets::read_asset(content, &el.src) {
                if let Some(ext) = image_ext(&bytes) {
                    pictures.push(Picture {
                        src: el.src.clone(),
                        name: format!("image{}.{}", pictures.len() + 1, ext),
                        bytes,
                    });
                }
            }
        }
    }
    let has_notes = deck
        .slides
        .iter()
        .any(|slide| !slide.notes.trim().is_empty());
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, body: &str| -> std::io::Result<()> {
        zip.start_file(name, opt)?;
        zip.write_all(body.as_bytes())
    };
    put(
        "[Content_Types].xml",
        &content_types(deck, &pictures, has_notes),
    )?;
    put("_rels/.rels", ROOT_RELS)?;
    put("docProps/core.xml", &core_xml(&deck.title))?;
    put("ppt/presentation.xml", &presentation_xml(deck, has_notes))?;
    put(
        "ppt/_rels/presentation.xml.rels",
        &presentation_rels(deck.slides.len(), has_notes),
    )?;
    put("ppt/slideMasters/slideMaster1.xml", MASTER)?;
    put("ppt/slideMasters/_rels/slideMaster1.xml.rels", MASTER_RELS)?;
    put("ppt/slideLayouts/slideLayout1.xml", LAYOUT)?;
    put("ppt/slideLayouts/_rels/slideLayout1.xml.rels", LAYOUT_RELS)?;
    let palette = Palette::resolve(deck.palette, deck.look);
    put("ppt/theme/theme1.xml", &theme_xml(&palette))?;
    if has_notes {
        put("ppt/notesMasters/notesMaster1.xml", NOTES_MASTER)?;
        put(
            "ppt/notesMasters/_rels/notesMaster1.xml.rels",
            NOTES_MASTER_RELS,
        )?;
    }
    for (idx, slide) in deck.slides.iter().enumerate() {
        let used: Vec<&Picture> = pictures
            .iter()
            .filter(|p| slide.elements.iter().any(|e| e.src == p.src))
            .collect();
        put(
            &format!("ppt/slides/slide{}.xml", idx + 1),
            &slide_xml(slide, &palette, &used, &shown),
        )?;
        put(
            &format!("ppt/slides/_rels/slide{}.xml.rels", idx + 1),
            &slide_rels(&used, (!slide.notes.trim().is_empty()).then_some(idx + 1)),
        )?;
        if !slide.notes.trim().is_empty() {
            put(
                &format!("ppt/notesSlides/notesSlide{}.xml", idx + 1),
                &notes_slide_xml(&slide.notes),
            )?;
            put(
                &format!("ppt/notesSlides/_rels/notesSlide{}.xml.rels", idx + 1),
                &notes_slide_rels(idx + 1),
            )?;
        }
    }
    drop(put);
    for picture in &pictures {
        zip.start_file(format!("ppt/media/{}", picture.name), opt)?;
        zip.write_all(&picture.bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

/// Parse the bytes of a `.pptx` package into a [`Deck`], storing its
/// pictures in `content`.
pub fn read_pptx(bytes: &[u8], content: &ContentStore) -> std::io::Result<Deck> {
    let mut zip = crate::odp::open_package(bytes)?;
    let title = read_zip_text(&mut zip, "docProps/core.xml")
        .and_then(|x| first_text(&x, b"dc:title"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported PowerPoint".into());
    let mut names: Vec<String> = zip
        .file_names()
        .map(std::borrow::Cow::into_owned)
        .filter(|n| {
            n.starts_with("ppt/slides/slide")
                && std::path::Path::new(n)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
                && !n.contains("/_rels/")
        })
        .collect();
    names.sort_by_key(|n| slide_number(n));
    let mut slides = Vec::new();
    for name in names {
        let Some(xml) = read_zip_text(&mut zip, &name) else {
            continue;
        };
        let rel_name = name.replace("ppt/slides/", "ppt/slides/_rels/") + ".rels";
        let rels = read_zip_text(&mut zip, &rel_name)
            .map(|x| parse_rels(&x))
            .unwrap_or_default();
        let mut slide = parse_slide(&xml, &rels, &mut zip, content);
        if let Some(target) = rels.values().find(|target| target.contains("notesSlides/")) {
            let path = format!("ppt/{}", target.trim_start_matches("../"));
            if let Some(notes_xml) = read_zip_text(&mut zip, &path) {
                slide.notes = parse_notes(&notes_xml);
            }
        }
        slides.push(slide);
    }
    if slides.is_empty() {
        slides.push(Slide::new(Layout::Blank));
    }
    let (palette, look) = read_zip_text(&mut zip, "ppt/theme/theme1.xml").map_or_else(
        || (PaletteKind::Indigo, DeckLook::default()),
        |theme| deck_look_of(&theme),
    );
    let deck = Deck {
        title,
        palette,
        look,
        slides,
    };
    crate::odp::validate_imported_deck(&deck)?;
    Ok(deck)
}

struct Picture {
    src: String,
    name: String,
    bytes: Vec<u8>,
}

/// Write a Tilo palette role as DrawingML. Theme colours stay live when the
/// presentation is opened in Office; literal picker colours stay literal,
/// and so do the roles no scheme slot holds (white, the ink on the accent).
fn drawing_color(role: &str, palette: &Palette) -> String {
    if let Some(hex) = role.strip_prefix('#') {
        if (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let rgb = std::str::from_utf8(hex.as_bytes().get(..6).unwrap_or_default())
                .unwrap_or_default();
            let alpha = if hex.len() == 8 {
                hex.get(6..)
                    .and_then(|alpha| u8::from_str_radix(alpha, 16).ok())
            } else {
                None
            };
            let alpha_xml = alpha
                .map(|a| format!("<a:alpha val=\"{}\"/>", u32::from(a) * 100_000 / 255))
                .unwrap_or_default();
            return format!(r#"<a:srgbClr val="{rgb}">{alpha_xml}</a:srgbClr>"#);
        }
    }
    let literal = |color: Color| format!(r#"<a:srgbClr val="{}"/>"#, rgb(color));
    let scheme = match role {
        // The deck names pure white itself: document content, not a theme colour.
        "white" => return r#"<a:srgbClr val="FFFFFF"/>"#.to_owned(),
        "on-accent" => return literal(palette.on_accent),
        "surface" => "lt1",
        "surface-dim" => "lt2",
        "text" => "dk1",
        "text-dim" => "dk2",
        "deep" => "accent6",
        "success" => "accent2",
        "warning" => "accent3",
        "danger" => "accent4",
        "info" => "accent5",
        // Accent-soft/deep deliberately remain tied to accent1. Transparency
        // and tonal transforms are not modelled independently.
        _ => "accent1",
    };
    format!(r#"<a:schemeClr val="{scheme}"/>"#)
}

fn drawing_fill(role: &str, palette: &Palette) -> String {
    if role == "none" {
        "<a:noFill/>".into()
    } else {
        format!(
            "<a:solidFill>{}</a:solidFill>",
            drawing_color(role, palette)
        )
    }
}

/// Emit the model's three slide treatments as a native DrawingML background.
/// The value deliberately stays theme-relative: an Accent or Deep slide still
/// tracks the presentation theme when opened in PowerPoint instead of being
/// flattened to a Tilo-specific RGB colour.
fn drawing_slide_background(background: Background, palette: &Palette) -> String {
    let role = match background {
        Background::Surface => "surface",
        Background::Accent => "accent",
        Background::Deep => "deep",
    };
    format!(
        "<p:bg><p:bgPr>{}<a:effectLst/></p:bgPr></p:bg>",
        drawing_fill(role, palette)
    )
}

/// Map the only theme-relative slide backgrounds our model represents. A
/// literal RGB, gradient, picture, or inherited master background is left at
/// the neutral model default rather than pretending it survived unchanged.
fn slide_background_from_color(e: &BytesStart<'_>) -> Option<Background> {
    (local(e.name().as_ref()) == b"schemeClr").then(|| match attr(e, b"val").as_str() {
        "lt1" => Some(Background::Surface),
        "accent1" => Some(Background::Accent),
        // `dk1` is where files written before the deep tone had its own
        // slot kept it.
        "accent6" | "dk1" => Some(Background::Deep),
        _ => None,
    })?
}

fn drawing_line(role: &str, width: f64, palette: &Palette) -> String {
    let width = (width.max(0.0) * EMU).round() as i64;
    if role == "none" || width == 0 {
        "<a:ln><a:noFill/></a:ln>".into()
    } else {
        format!(
            r#"<a:ln w="{width}"><a:solidFill>{}</a:solidFill></a:ln>"#,
            drawing_color(role, palette)
        )
    }
}

#[derive(Clone, Copy)]
enum ColorTarget {
    Fill,
    Stroke,
    Text,
}

fn color_target(text_depth: usize, line_depth: usize) -> ColorTarget {
    if text_depth > 0 {
        ColorTarget::Text
    } else if line_depth > 0 {
        ColorTarget::Stroke
    } else {
        ColorTarget::Fill
    }
}

fn set_pending_no_fill(c: &mut Pending, target: ColorTarget) {
    match target {
        ColorTarget::Fill | ColorTarget::Text => c.fill = "none".into(),
        ColorTarget::Stroke => c.stroke = "none".into(),
    }
}

fn set_pending_color(c: &mut Pending, target: ColorTarget, e: &BytesStart<'_>) {
    let role = match local(e.name().as_ref()) {
        b"schemeClr" => match attr(e, b"val").as_str() {
            "lt1" => "surface".into(),
            "lt2" => "surface-dim".into(),
            "accent6" => "deep".into(),
            "dk1" => "text".into(),
            "dk2" => "text-dim".into(),
            "accent2" => "success".into(),
            "accent3" => "warning".into(),
            "accent4" => "danger".into(),
            "accent5" => "info".into(),
            // Accent6 and any unknown scheme keep a visual accent instead of
            // claiming to retain a colour the model cannot represent.
            _ => "accent".into(),
        },
        b"srgbClr" => {
            let rgb = attr(e, b"val");
            if rgb.len() == 6 && rgb.bytes().all(|b| b.is_ascii_hexdigit()) {
                format!("#{}", rgb.to_ascii_uppercase())
            } else {
                "accent".into()
            }
        }
        _ => return,
    };
    match target {
        ColorTarget::Fill | ColorTarget::Text => c.fill = role,
        ColorTarget::Stroke => c.stroke = role,
    }
}

/// One slide's part. `shown` holds each text box's height as Slides shows
/// it ([`crate::render::shown_text_heights`]): its frame here, with the box
/// marked to fit its text, so a box its words outgrew is not cut short.
fn slide_xml(
    slide: &Slide,
    palette: &Palette,
    pictures: &[&Picture],
    shown: &HashMap<String, f64>,
) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld>"#,
    );
    s.push_str(&drawing_slide_background(slide.background, palette));
    let on = palette.on(slide.background);
    s.push_str(r#"<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#);
    for (i, el) in slide.z_sorted().into_iter().enumerate() {
        let id = i + 2;
        let (x, y, w, h) = (emu(el.x), emu(el.y), emu(el.w), emu(el.h));
        match el.kind {
            ElementKind::Image => {
                if let Some(pos) = pictures.iter().position(|p| p.src == el.src) {
                    let rid = pos + 2;
                    s.push_str(&format!(r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#));
                }
            }
            ElementKind::Text => {
                let h = emu(shown.get(&el.id).copied().unwrap_or(el.h));
                let align = match el.align {
                    TextAlign::Center => "ctr",
                    TextAlign::Right => "r",
                    TextAlign::Justify => "just",
                    TextAlign::Left => "l",
                };
                let size = (el.font_size * 100.0).round() as i64;
                // The ink Slides draws the words in: the theme's text slot
                // where that is it, else the ink itself, chosen against what
                // the words stand on.
                let ink = crate::render::text_ink(&on, slide, el);
                let color = match el.fill.as_str() {
                    "text" if ink == palette.text => drawing_color("text", palette),
                    "text-dim" if ink == palette.text_dim => drawing_color("text-dim", palette),
                    "text" | "text-dim" => format!(r#"<a:srgbClr val="{}"/>"#, rgb(ink)),
                    role => drawing_color(role, palette),
                };
                let run = |text: &str, bold: bool, italic: bool| {
                    format!(
                        r#"<a:r><a:rPr lang="en-US" sz="{size}" b="{}" i="{}">{color}</a:rPr><a:t>{}</a:t></a:r>"#,
                        u8::from(bold),
                        u8::from(italic),
                        quick_xml::escape::partial_escape(text)
                    )
                };
                // A paragraph per block, a run per stretch of one weight and slant.
                let runs = el
                    .rich_blocks()
                    .iter()
                    .map(|block| {
                        let runs = crate::model::runs_of(block);
                        let body = if runs.is_empty() {
                            run("", false, false)
                        } else {
                            runs.into_iter()
                                .map(|(text, bold, italic)| run(text, bold, italic))
                                .collect()
                        };
                        // A list item carries its bullet or number, hung
                        // in the margin as PowerPoint sets one.
                        let marker = match block.kind {
                            ui::widget::BlockKind::Bullet { .. } => Some(r#"<a:buChar char="•"/>"#),
                            ui::widget::BlockKind::Numbered { .. } => {
                                Some(r#"<a:buAutoNum type="arabicPeriod"/>"#)
                            }
                            _ => None,
                        };
                        match marker {
                            Some(marker) => format!(
                                r#"<a:p><a:pPr algn="{align}" marL="457200" indent="-342900">{marker}</a:pPr>{body}</a:p>"#
                            ),
                            None => format!(r#"<a:p><a:pPr algn="{align}"/>{body}</a:p>"#),
                        }
                    })
                    .collect::<String>();
                s.push_str(&format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Text {id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr><a:spAutoFit/></a:bodyPr><a:lstStyle/>{runs}</p:txBody></p:sp>"#));
            }
            kind => {
                let prst = match kind {
                    ElementKind::Ellipse => "ellipse",
                    ElementKind::Line => "line",
                    _ => "rect",
                };
                s.push_str(&format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>{}{}</p:spPr></p:sp>"#, drawing_fill(&el.fill, palette), drawing_line(&el.stroke, el.stroke_width, palette)));
            }
        }
    }
    s.push_str("</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>");
    s
}

struct Pending {
    kind: ElementKind,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    prst: String,
    text: String,
    embed: String,
    size: f64,
    bold: bool,
    italic: bool,
    align: TextAlign,
    fill: String,
    stroke: String,
    stroke_width: f64,
    /// The text as written: a paragraph per `a:p`, a mark per bold or
    /// italic run.
    blocks: Vec<ui::widget::BlockContent>,
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            kind: ElementKind::Rect,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            prst: String::new(),
            text: String::new(),
            embed: String::new(),
            size: 0.0,
            bold: false,
            italic: false,
            align: TextAlign::Left,
            fill: "accent".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            blocks: Vec::new(),
        }
    }
}

fn parse_slide(
    xml: &str,
    rels: &HashMap<String, String>,
    zip: &mut Package<'_>,
    content: &ContentStore,
) -> Slide {
    let mut slide = Slide::new(Layout::Blank);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut cur: Option<Pending> = None;
    let mut in_text = false;
    let mut paragraph = false;
    // The same DrawingML colour element occurs in fills, outlines and text
    // runs. Keep a tiny amount of ancestor context instead of building a DOM.
    let mut in_line = 0usize;
    let mut in_text_style = 0usize;
    // `<p:bg>` belongs to the slide rather than a shape. Keep it separate
    // from fill parsing so a text/shape colour can never overwrite it.
    let mut in_slide_background = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let n = local(name.as_ref());
                match n {
                    b"bg" => in_slide_background += 1,
                    b"sp" | b"cxnSp" => {
                        cur = Some(Pending {
                            kind: ElementKind::Rect,
                            ..Default::default()
                        })
                    }
                    b"pic" => {
                        cur = Some(Pending {
                            kind: ElementKind::Image,
                            ..Default::default()
                        })
                    }
                    b"off" => {
                        if let Some(c) = cur.as_mut() {
                            c.x = attr_num(&e, b"x") / EMU;
                            c.y = attr_num(&e, b"y") / EMU
                        }
                    }
                    b"ext" => {
                        if let Some(c) = cur.as_mut() {
                            c.w = attr_num(&e, b"cx") / EMU;
                            c.h = attr_num(&e, b"cy") / EMU
                        }
                    }
                    b"prstGeom" => {
                        if let Some(c) = cur.as_mut() {
                            c.prst = attr(&e, b"prst")
                        }
                    }
                    b"blip" => {
                        if let Some(c) = cur.as_mut() {
                            c.embed = attr(&e, b"embed")
                        }
                    }
                    b"rPr" | b"defRPr" => {
                        if let Some(c) = cur.as_mut() {
                            c.size = attr_num(&e, b"sz") / 100.0;
                            c.bold = attr(&e, b"b") == "1";
                            c.italic = attr(&e, b"i") == "1"
                        }
                        in_text_style += 1;
                    }
                    b"ln" => {
                        if let Some(c) = cur.as_mut() {
                            c.stroke_width = attr_num(&e, b"w") / EMU;
                        }
                        in_line += 1;
                    }
                    b"srgbClr" | b"schemeClr" => {
                        if in_slide_background > 0 {
                            if let Some(background) = slide_background_from_color(&e) {
                                slide.background = background;
                            }
                        } else if let Some(c) = cur.as_mut() {
                            set_pending_color(c, color_target(in_text_style, in_line), &e);
                        }
                    }
                    b"noFill" => {
                        if let Some(c) = cur.as_mut() {
                            set_pending_no_fill(c, color_target(in_text_style, in_line));
                        }
                    }
                    b"pPr" => {
                        if let Some(c) = cur.as_mut() {
                            c.align = match attr(&e, b"algn").as_str() {
                                "ctr" => TextAlign::Center,
                                "r" => TextAlign::Right,
                                "just" => TextAlign::Justify,
                                _ => TextAlign::Left,
                            }
                        }
                    }
                    b"p" => {
                        if let Some(c) = cur.as_mut() {
                            if paragraph {
                                c.text.push('\n');
                            }
                            c.blocks.push(ui::widget::BlockContent::paragraph("", ""));
                        }
                        paragraph = true
                    }
                    b"t" => in_text = true,
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                let n = local(name.as_ref());
                if in_slide_background > 0
                    && matches!(n, b"srgbClr" | b"schemeClr")
                    && let Some(background) = slide_background_from_color(&e)
                {
                    slide.background = background;
                }
                if let Some(c) = cur.as_mut() {
                    match n {
                        b"off" => {
                            c.x = attr_num(&e, b"x") / EMU;
                            c.y = attr_num(&e, b"y") / EMU
                        }
                        b"ext" => {
                            c.w = attr_num(&e, b"cx") / EMU;
                            c.h = attr_num(&e, b"cy") / EMU
                        }
                        b"prstGeom" => c.prst = attr(&e, b"prst"),
                        b"blip" => c.embed = attr(&e, b"embed"),
                        b"rPr" | b"defRPr" => {
                            c.size = attr_num(&e, b"sz") / 100.0;
                            c.bold = attr(&e, b"b") == "1";
                            c.italic = attr(&e, b"i") == "1"
                        }
                        b"srgbClr" | b"schemeClr" => {
                            set_pending_color(c, color_target(in_text_style, in_line), &e);
                        }
                        // A paragraph's bullet or number makes it a list item.
                        b"buChar" | b"buAutoNum" => {
                            if let Some(block) = c.blocks.last_mut() {
                                block.kind = if n == b"buChar" {
                                    ui::widget::BlockKind::Bullet { indent: 0 }
                                } else {
                                    ui::widget::BlockKind::Numbered { indent: 0 }
                                };
                            }
                        }
                        b"noFill" => {
                            set_pending_no_fill(c, color_target(in_text_style, in_line));
                        }
                        // Empty run properties cannot have a child colour.
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(t)) if in_text => {
                if let Some(c) = cur.as_mut() {
                    if let Ok(v) = t.xml10_content() {
                        c.text.push_str(&v);
                        let (bold, italic) = (c.bold, c.italic);
                        if let Some(block) = c.blocks.last_mut() {
                            crate::model::push_run(block, &v, bold, italic);
                        }
                    }
                }
            }
            Ok(Event::CData(data)) if in_text => {
                if let Some(c) = cur.as_mut()
                    && let Ok(text) = data.xml10_content()
                {
                    c.text.push_str(&text);
                    if let Some(block) = c.blocks.last_mut() {
                        crate::model::push_run(block, &text, c.bold, c.italic);
                    }
                }
            }
            // quick-xml reports `&amp;`/`&#x2019;` as events of their own between
            // the text around them.
            Ok(Event::GeneralRef(r)) if in_text => {
                if let Some(c) = cur.as_mut()
                    && let Some(ch) = tilo_doc_codecs::resolve_general_ref(&r)
                {
                    c.text.push(ch);
                    let (bold, italic) = (c.bold, c.italic);
                    if let Some(block) = c.blocks.last_mut() {
                        crate::model::push_run(block, ch.encode_utf8(&mut [0; 4]), bold, italic);
                    }
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_text = false,
                b"rPr" | b"defRPr" => in_text_style = in_text_style.saturating_sub(1),
                b"ln" => in_line = in_line.saturating_sub(1),
                b"bg" => in_slide_background = in_slide_background.saturating_sub(1),
                b"sp" | b"cxnSp" | b"pic" => {
                    if let Some(c) = cur.take() {
                        push_pending(&mut slide, c, rels, zip, content);
                    }
                    paragraph = false
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    slide
}

fn push_pending(
    slide: &mut Slide,
    c: Pending,
    rels: &HashMap<String, String>,
    zip: &mut Package<'_>,
    content: &ContentStore,
) {
    let w = if c.w > 0.0 { c.w } else { 1.0 };
    let h = if c.h > 0.0 { c.h } else { 1.0 };
    let mut el = if c.kind == ElementKind::Image {
        let Some(target) = rels.get(&c.embed) else {
            return;
        };
        let path = format!("ppt/{}", target.trim_start_matches("../"));
        let Some(bytes) = crate::odp::read_package_picture(zip, &path) else {
            return;
        };
        let Some(src) = crate::odp::cache_imported_picture(content, &bytes) else {
            return;
        };
        Element::image(&src, c.x, c.y, w, h)
    } else if !c.text.is_empty() {
        let mut e = Element::text(
            &c.text,
            c.x,
            c.y,
            w,
            h,
            if c.size > 0.0 { c.size } else { 24.0 },
        );
        e.align = c.align;
        if crate::model::plain_text_of(&c.blocks) == c.text {
            e.set_blocks(c.blocks);
        } else {
            e.bold = c.bold;
            e.italic = c.italic;
        }
        e
    } else {
        Element::shape(
            match c.prst.as_str() {
                "ellipse" => ElementKind::Ellipse,
                "line" => ElementKind::Line,
                _ => ElementKind::Rect,
            },
            c.x,
            c.y,
            w,
            h,
        )
    };
    // Preserve the native DrawingML styling the deck model can represent.
    // (Text uses `fill` as its ink colour; shapes use it as their fill.)
    el.fill = c.fill;
    el.stroke = c.stroke;
    el.stroke_width = c.stroke_width;
    el.z = slide.elements.len() as i64;
    slide.elements.push(el);
}

/// A note slide contains several placeholders (date, footer, thumbnail) in a
/// normal PowerPoint deck.  Export only the body placeholder, which is the
/// model's speaker-notes text and the one PowerPoint presents to the speaker.
fn notes_slide_xml(notes: &str) -> String {
    let paragraphs = notes
        .split('\n')
        .map(|line| {
            format!(
                r#"<a:p><a:r><a:rPr lang="en-US"/><a:t>{}</a:t></a:r></a:p>"#,
                quick_xml::escape::partial_escape(line)
            )
        })
        .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:notes xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name=""><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id="2" name="Notes Placeholder 1"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"#
    )
}

/// Read only the note-body placeholder. This avoids accidentally importing
/// page numbers, dates, footers, or the slide thumbnail from a foreign notes
/// page as speaker text.
fn parse_notes(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut in_shape = 0usize;
    let mut body_placeholder = false;
    let mut in_text = false;
    let mut paragraph = false;
    let mut notes = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local(e.name().as_ref()) {
                b"sp" => {
                    in_shape += 1;
                    body_placeholder = false;
                }
                b"ph" if in_shape > 0 && attr(&e, b"type") == "body" => {
                    body_placeholder = true;
                }
                b"p" if body_placeholder => {
                    if paragraph {
                        notes.push('\n');
                    }
                    paragraph = true;
                }
                b"t" if body_placeholder => in_text = true,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == b"ph" && in_shape > 0 && attr(&e, b"type") == "body"
                {
                    body_placeholder = true;
                }
            }
            Ok(Event::Text(text)) if in_text => {
                if let Ok(text) = text.xml10_content() {
                    notes.push_str(&text);
                }
            }
            Ok(Event::CData(data)) if in_text => {
                if let Ok(text) = data.xml10_content() {
                    notes.push_str(&text);
                }
            }
            Ok(Event::GeneralRef(r)) if in_text => {
                notes.extend(tilo_doc_codecs::resolve_general_ref(&r));
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_text = false,
                b"sp" => {
                    in_shape = in_shape.saturating_sub(1);
                    if in_shape == 0 {
                        body_placeholder = false;
                        in_text = false;
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    notes
}

fn read_zip_text(z: &mut Package<'_>, name: &str) -> Option<String> {
    crate::odp::read_package_xml(z, name).ok()
}
fn parse_rels(xml: &str) -> HashMap<String, String> {
    let mut r = Reader::from_str(xml);
    let mut m = HashMap::new();
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e))
                if local(e.name().as_ref()) == b"Relationship" =>
            {
                m.insert(attr(&e, b"Id"), attr(&e, b"Target"));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    m
}
/// The text content of the first `wanted` element, or `None` if there is none.
fn first_text(xml: &str, wanted: &[u8]) -> Option<String> {
    let mut r = Reader::from_str(xml);
    let mut text: Option<String> = None;
    loop {
        match (r.read_event(), text.as_mut()) {
            (Ok(Event::Start(e)), None) if e.name().as_ref() == wanted => {
                text = Some(String::new());
            }
            (Ok(Event::Text(t)), Some(text)) => text.push_str(&t.xml10_content().ok()?),
            (Ok(Event::CData(data)), Some(text)) => text.push_str(&data.xml10_content().ok()?),
            (Ok(Event::GeneralRef(g)), Some(text)) => {
                text.extend(tilo_doc_codecs::resolve_general_ref(&g));
            }
            (Ok(Event::End(_)), Some(_)) => return text,
            (Ok(Event::Eof) | Err(_), _) => return None,
            _ => {}
        }
    }
}
fn attr(e: &BytesStart<'_>, wanted: &[u8]) -> String {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == wanted)
        .and_then(|a| tilo_doc_codecs::unescape_attr(&a))
        .unwrap_or_default()
}
fn attr_num(e: &BytesStart<'_>, wanted: &[u8]) -> f64 {
    attr(e, wanted).parse().unwrap_or(0.0)
}
fn local(n: &[u8]) -> &[u8] {
    n.rsplit(|b| *b == b':').next().unwrap_or(n)
}
fn slide_number(n: &str) -> usize {
    n.trim_end_matches(".xml")
        .rsplit("slide")
        .next()
        .and_then(|x| x.parse().ok())
        .unwrap_or(0)
}
fn emu(v: f64) -> i64 {
    (v * EMU).round() as i64
}
fn image_ext(b: &[u8]) -> Option<&'static str> {
    tilo_content::image_format(b)
        .map(|format| format.extension)
        .filter(|ext| matches!(*ext, "png" | "jpg" | "gif"))
}

fn content_types(deck: &Deck, p: &[Picture], has_notes: bool) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#,
    );
    for ext in ["png", "jpg", "gif"] {
        if p.iter().any(|x| x.name.ends_with(ext)) {
            s.push_str(&format!(
                r#"<Default Extension="{ext}" ContentType="image/{}"/>"#,
                if ext == "jpg" { "jpeg" } else { ext }
            ))
        }
    }
    s.push_str(r#"<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#);
    for i in 1..=deck.slides.len() {
        s.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#))
    }
    if has_notes {
        s.push_str(r#"<Override PartName="/ppt/notesMasters/notesMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml"/>"#);
        for (i, slide) in deck.slides.iter().enumerate() {
            if !slide.notes.trim().is_empty() {
                s.push_str(&format!(r#"<Override PartName="/ppt/notesSlides/notesSlide{}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/>"#, i + 1));
            }
        }
    }
    s.push_str("</Types>");
    s
}
fn presentation_xml(d: &Deck, has_notes: bool) -> String {
    let ids = (1..=d.slides.len())
        .map(|i| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + i, i + 2))
        .collect::<String>();
    let notes_master = has_notes
        .then_some(r#"<p:notesMasterIdLst><p:notesMasterId r:id="rId2"/></p:notesMasterIdLst>"#)
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>{notes_master}<p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000" type="screen16x9"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    )
}
fn presentation_rels(n: usize, has_notes: bool) -> String {
    let mut s = String::from(RELS_HEAD);
    s.push_str(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>"#);
    if has_notes {
        s.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster" Target="notesMasters/notesMaster1.xml"/>"#);
    }
    for i in 1..=n {
        s.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{i}.xml"/>"#,i+2))
    }
    s.push_str("</Relationships>");
    s
}
fn slide_rels(p: &[&Picture], notes_slide: Option<usize>) -> String {
    let mut s = String::from(RELS_HEAD);
    s.push_str(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>"#);
    for (i, x) in p.iter().enumerate() {
        s.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/{}"/>"#,i+2,x.name))
    }
    if let Some(idx) = notes_slide {
        s.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide{idx}.xml"/>"#, p.len() + 2));
    }
    s.push_str("</Relationships>");
    s
}

fn notes_slide_rels(slide: usize) -> String {
    format!(
        r#"{RELS_HEAD}<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="../slides/slide{slide}.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesMaster" Target="../notesMasters/notesMaster1.xml"/></Relationships>"#
    )
}

const RELS_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;
fn core_xml(title: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title></cp:coreProperties>"#,
        quick_xml::escape::partial_escape(title)
    )
}
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
const MASTER: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMap accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" bg1="lt1" bg2="lt2" folHlink="folHlink" hlink="hlink" tx1="dk1" tx2="dk2"/><p:sldLayoutIdLst><p:sldLayoutId id="1" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#;
const MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
const LAYOUT: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;
const LAYOUT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;
const NOTES_MASTER: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:notesMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld name=""><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMap accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" bg1="lt1" bg2="lt2" folHlink="folHlink" hlink="hlink" tx1="dk1" tx2="dk2"/></p:notesMaster>"#;
const NOTES_MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
/// The presentation theme: the deck's own colours in its look, so the file
/// shows the deck as every collaborator sees it. Slots: `lt1`/`lt2` the
/// surfaces, `dk1`/`dk2` the inks on them, `accent1` the accent,
/// `accent2`..`accent5` the status hues and `accent6` the deep tone.
fn theme_xml(palette: &Palette) -> String {
    let slot = |name: &str, color: Color| {
        format!(r#"<a:{name}><a:srgbClr val="{}"/></a:{name}>"#, rgb(color))
    };
    let scheme = [
        slot("dk1", palette.text),
        slot("lt1", palette.surface),
        slot("dk2", palette.text_dim),
        slot("lt2", palette.surface_dim),
        slot("accent1", palette.accent),
        slot("accent2", palette.success),
        slot("accent3", palette.warning),
        slot("accent4", palette.danger),
        slot("accent5", palette.info),
        slot("accent6", palette.deep),
    ]
    .concat();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Tilo"><a:themeElements><a:clrScheme name="Tilo">{scheme}<a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Tilo"><a:majorFont/><a:minorFont/></a:fontScheme><a:fmtScheme name="Tilo"><a:fillStyleLst/><a:lnStyleLst/><a:effectStyleLst/><a:bgFillStyleLst/></a:fmtScheme></a:themeElements></a:theme>"#
    )
}

/// `color` as DrawingML's `RRGGBB`.
fn rgb(color: Color) -> String {
    ui::color::to_hex(color).trim_start_matches('#').to_owned()
}

/// The colour a theme names in scheme slot `slot` (`lt1`, `accent1`, …).
fn scheme_color(theme: &str, slot: &str) -> Option<Color> {
    let open = format!("<a:{slot}>");
    let start = theme.find(&open)? + open.len();
    let end = start + theme.get(start..)?.find(&format!("</a:{slot}>"))?;
    let body = theme.get(start..end)?;
    let value = |key: &str| {
        let at = body.find(key)? + key.len();
        body.get(at..at + 6)
    };
    ui::color::parse_hex(&format!(
        "#{}",
        value("srgbClr val=\"").or_else(|| value("lastClr=\""))?
    ))
}

/// The palette and look a presentation's theme shows: a dark look where its
/// light slot is the darker, and the Slides palette whose accent it carries.
fn deck_look_of(theme: &str) -> (PaletteKind, DeckLook) {
    let luminance = |color: Color| {
        let c = color.to_rgba8();
        u32::from(c.r) * 299 + u32::from(c.g) * 587 + u32::from(c.b) * 114
    };
    let look = match (scheme_color(theme, "lt1"), scheme_color(theme, "dk1")) {
        (Some(light), Some(dark)) if luminance(light) < luminance(dark) => DeckLook::Dark,
        _ => DeckLook::Light,
    };
    let palette = scheme_color(theme, "accent1")
        .and_then(|accent| {
            PaletteKind::ALL
                .into_iter()
                .find(|kind| rgb(Palette::resolve(*kind, look).accent) == rgb(accent))
        })
        .unwrap_or(PaletteKind::Indigo);
    (palette, look)
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
        let bytes = write_pptx(&deck, &content).expect("write");
        let back = read_pptx(&bytes, &content).expect("read");
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

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn pptx_round_trips_content_styles_backgrounds_and_pictures() {
        {
            let content = ContentStore::memory();
            let mut s = Slide::new(Layout::Blank);
            s.add(
                Element::text("Hello\nPowerPoint", 10.0, 20.0, 500.0, 100.0, 36.0)
                    .with(|e| e.bold = true),
            );
            s.add(Element::shape(
                ElementKind::Ellipse,
                100.0,
                200.0,
                220.0,
                90.0,
            ));
            let d = Deck {
                title: "Talk".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![s],
            };
            let b = write_pptx(&d, &content).unwrap();
            assert!(b.starts_with(b"PK"));
            let got = read_pptx(&b, &content).unwrap();
            assert_eq!(got.title, "Talk");
            assert_eq!(got.slides.len(), 1);
            assert_eq!(got.slides[0].elements[0].text, "Hello\nPowerPoint");
            assert!(
                got.slides[0]
                    .elements
                    .iter()
                    .any(|e| e.kind == ElementKind::Ellipse)
            );
        }

        {
            let content = ContentStore::memory();
            let mut slide = Slide::new(Layout::Blank);
            let mut shape = Element::shape(ElementKind::Rect, 10.0, 20.0, 100.0, 80.0);
            shape.fill = "#12A4EF".into();
            shape.stroke = "danger".into();
            shape.stroke_width = 3.5;
            slide.add(shape);
            let mut text = Element::text("Ink", 20.0, 120.0, 200.0, 40.0, 18.0);
            text.fill = "success".into();
            slide.add(text);
            let deck = Deck {
                title: "Style fidelity".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![slide],
            };

            let bytes = write_pptx(&deck, &content).expect("pptx export");
            let got = read_pptx(&bytes, &content).expect("pptx import");
            let shape = &got.slides[0].elements[0];
            assert_eq!(shape.fill, "#12A4EF");
            assert_eq!(shape.stroke, "danger");
            assert!((shape.stroke_width - 3.5).abs() < 0.01);
            assert_eq!(got.slides[0].elements[1].fill, "success");
        }

        {
            let content = ContentStore::memory();
            let mut accent = Slide::new(Layout::Blank);
            accent.background = Background::Accent;
            let mut deep = Slide::new(Layout::Blank);
            deep.background = Background::Deep;
            let deck = Deck {
                title: "Background fidelity".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![accent, deep],
            };

            let bytes = write_pptx(&deck, &content).expect("pptx export");
            let mut archive = crate::odp::open_package(&bytes).expect("pptx zip");
            let accent_xml =
                read_zip_text(&mut archive, "ppt/slides/slide1.xml").expect("slide xml");
            let deep_xml = read_zip_text(&mut archive, "ppt/slides/slide2.xml").expect("slide xml");
            assert!(
                accent_xml.contains("<p:bg><p:bgPr><a:solidFill><a:schemeClr val=\"accent1\""),
                "accent is a native theme background"
            );
            assert!(
                deep_xml.contains("<p:bg><p:bgPr><a:solidFill><a:schemeClr val=\"accent6\""),
                "deep is a native theme background"
            );

            let got = read_pptx(&bytes, &content).expect("pptx import");
            assert_eq!(got.slides[0].background, Background::Accent);
            assert_eq!(got.slides[1].background, Background::Deep);
        }

        {
            let content = ContentStore::memory();
            let p = std::env::temp_dir().join(format!("slides-pptx-{}.png", std::process::id()));
            std::fs::write(&p, b"\x89PNG\r\n\x1a\nbytes").unwrap();
            let asset = crate::assets::import_asset_file(&content, &p).unwrap();
            let mut s = Slide::new(Layout::Blank);
            s.add(Element::image(&asset, 1.0, 2.0, 30.0, 40.0));
            let d = Deck {
                title: "Pictures".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![s],
            };
            let b = write_pptx(&d, &content).unwrap();
            let got = read_pptx(&b, &content).unwrap();
            assert_eq!(got.slides[0].elements[0].kind, ElementKind::Image);
            assert!(
                crate::assets::read_asset(&content, &got.slides[0].elements[0].src)
                    .unwrap()
                    .starts_with(b"\x89PNG")
            );
            let _ = std::fs::remove_file(p);
        }
    }

    /// Speaker notes, placeholder filtering, reserved characters and entity references.
    #[test]
    fn pptx_notes_and_text_escaping_round_trip() {
        {
            let content = ContentStore::memory();
            let mut with_notes = Slide::new(Layout::Blank);
            with_notes.notes = "Open with the decision.\nPause for questions.".into();
            let without_notes = Slide::new(Layout::Blank);
            let deck = Deck {
                title: "Notes fidelity".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![with_notes, without_notes],
            };

            let bytes = write_pptx(&deck, &content).expect("pptx export");
            let mut archive = crate::odp::open_package(&bytes).expect("pptx zip");
            let types = read_zip_text(&mut archive, "[Content_Types].xml").expect("content types");
            let notes = read_zip_text(&mut archive, "ppt/notesSlides/notesSlide1.xml")
                .expect("notes slide");
            assert!(types.contains("presentationml.notesSlide+xml"));
            assert!(notes.contains(r#"<p:ph type="body" idx="1"/>"#));
            assert!(notes.contains("Open with the decision."));
            assert!(
                !archive
                    .file_names()
                    .any(|name| name == "ppt/notesSlides/notesSlide2.xml"),
                "slides without notes do not gain empty note pages"
            );
            drop(archive);

            let got = read_pptx(&bytes, &content).expect("pptx import");
            assert_eq!(
                got.slides[0].notes,
                "Open with the decision.\nPause for questions."
            );
            assert!(got.slides[1].notes.is_empty());
        }

        {
            let notes = parse_notes(
                r#"<p:notes xmlns:a="a" xmlns:p="p"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="dt"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>2026-01-01</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Keep this</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#,
            );
            assert_eq!(notes, "Keep this");
        }

        {
            let content = ContentStore::memory();
            let mut slide = Slide::new(Layout::Blank);
            slide.add(Element::text(
                "R&D <b> & \u{1F600}",
                10.0,
                20.0,
                500.0,
                100.0,
                36.0,
            ));
            slide.notes = "a < b && c > d".into();
            let deck = Deck {
                title: "Q&A <live>".into(),
                palette: PaletteKind::Indigo,
                look: DeckLook::default(),
                slides: vec![slide],
            };
            let got = read_pptx(&write_pptx(&deck, &content).unwrap(), &content).unwrap();
            assert_eq!(got.title, "Q&A <live>");
            assert_eq!(got.slides[0].elements[0].text, "R&D <b> & \u{1F600}");
            assert_eq!(got.slides[0].notes, "a < b && c > d");
        }

        {
            let notes = parse_notes(&format!(
                r#"<p:notes xmlns:a="a" xmlns:p="p"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{RAW_REFS}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
            ));
            assert_eq!(notes, REFS_TEXT);
            let title = first_text(
                &format!("<cp:coreProperties><dc:title>{RAW_REFS}</dc:title></cp:coreProperties>"),
                b"dc:title",
            );
            assert_eq!(title.as_deref(), Some(REFS_TEXT));
            let rels = parse_rels(
                r#"<Relationships><Relationship Id="rId1" Target="../media/R&amp;D&#x20;chart.png"/></Relationships>"#,
            );
            assert_eq!(
                rels.get("rId1").map(String::as_str),
                Some("../media/R&D chart.png")
            );
        }
    }

    /// Every predefined entity and decimal/hex character references (one
    /// outside the BMP), next to each other and to text.
    const RAW_REFS: &str =
        "R&amp;D &lt;b&gt; &quot;q&quot; &apos;s&apos; &#8217;&#x2019;&#x1F600;&#65;&amp;&amp;";
    const REFS_TEXT: &str = "R&D <b> \"q\" 's' \u{2019}\u{2019}\u{1F600}A&&";
}
