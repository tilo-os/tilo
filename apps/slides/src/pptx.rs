//! Useful, deliberately bounded PowerPoint (`.pptx`) interchange.
//!
//! Slides, text boxes, rectangles, ellipses, lines and readable local pictures
//! map to native DrawingML objects. PowerPoint features which do not exist in
//! the qOS model (masters, charts, SmartArt, transitions and animation) are
//! intentionally not invented; the file UI tells users about that boundary.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::model::{Deck, Element, ElementKind, Layout, PaletteKind, Slide, TextAlign};

const EMU: f64 = 9_525.0;

pub fn write_pptx(deck: &Deck) -> std::io::Result<Vec<u8>> {
    let mut pictures = Vec::<Picture>::new();
    for slide in &deck.slides {
        for el in &slide.elements {
            if el.kind != ElementKind::Image || pictures.iter().any(|p| p.src == el.src) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&el.src) {
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
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, body: &str| -> std::io::Result<()> {
        zip.start_file(name, opt)?;
        zip.write_all(body.as_bytes())
    };
    put(
        "[Content_Types].xml",
        &content_types(deck.slides.len(), &pictures),
    )?;
    put("_rels/.rels", ROOT_RELS)?;
    put("docProps/core.xml", &core_xml(&deck.title))?;
    put("ppt/presentation.xml", &presentation_xml(deck))?;
    put(
        "ppt/_rels/presentation.xml.rels",
        &presentation_rels(deck.slides.len()),
    )?;
    put("ppt/slideMasters/slideMaster1.xml", MASTER)?;
    put("ppt/slideMasters/_rels/slideMaster1.xml.rels", MASTER_RELS)?;
    put("ppt/slideLayouts/slideLayout1.xml", LAYOUT)?;
    put("ppt/slideLayouts/_rels/slideLayout1.xml.rels", LAYOUT_RELS)?;
    put("ppt/theme/theme1.xml", THEME)?;
    for (idx, slide) in deck.slides.iter().enumerate() {
        let used: Vec<&Picture> = pictures
            .iter()
            .filter(|p| slide.elements.iter().any(|e| e.src == p.src))
            .collect();
        put(
            &format!("ppt/slides/slide{}.xml", idx + 1),
            &slide_xml(slide, &used),
        )?;
        put(
            &format!("ppt/slides/_rels/slide{}.xml.rels", idx + 1),
            &slide_rels(&used),
        )?;
    }
    drop(put);
    for picture in &pictures {
        zip.start_file(format!("ppt/media/{}", picture.name), opt)?;
        zip.write_all(&picture.bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

pub fn read_pptx(bytes: &[u8]) -> std::io::Result<Deck> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let title = read_zip_text(&mut zip, "docProps/core.xml")
        .and_then(|x| first_text(&x, b"dc:title"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported PowerPoint".into());
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|n| {
            n.starts_with("ppt/slides/slide") && n.ends_with(".xml") && !n.contains("/_rels/")
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
        slides.push(parse_slide(&xml, &rels, &mut zip));
    }
    if slides.is_empty() {
        slides.push(Slide::new(Layout::Blank));
    }
    Ok(Deck {
        title,
        palette: PaletteKind::Indigo,
        slides,
    })
}

struct Picture {
    src: String,
    name: String,
    bytes: Vec<u8>,
}

fn slide_xml(slide: &Slide, pictures: &[&Picture]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#,
    );
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
                let align = match el.align {
                    TextAlign::Center => "ctr",
                    TextAlign::Right => "r",
                    TextAlign::Justify => "just",
                    TextAlign::Left => "l",
                };
                let runs = el.text.split('\n').map(|line| format!(r#"<a:p><a:pPr algn="{align}"/><a:r><a:rPr lang="en-US" sz="{}" b="{}" i="{}"/><a:t>{}</a:t></a:r></a:p>"#, (el.font_size*100.0).round() as i64, u8::from(el.bold), u8::from(el.italic), xml(line))).collect::<String>();
                s.push_str(&format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Text {id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{runs}</p:txBody></p:sp>"#));
            }
            kind => {
                let prst = match kind {
                    ElementKind::Ellipse => "ellipse",
                    ElementKind::Line => "line",
                    _ => "rect",
                };
                s.push_str(&format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{w}" cy="{h}"/></a:xfrm><a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="5A60F0"/></a:solidFill></p:spPr></p:sp>"#));
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
        }
    }
}

fn parse_slide(
    xml: &str,
    rels: &HashMap<String, String>,
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> Slide {
    let mut slide = Slide::new(Layout::Blank);
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut cur: Option<Pending> = None;
    let mut in_text = false;
    let mut paragraph = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let n = local(name.as_ref());
                match n {
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
                        if paragraph {
                            if let Some(c) = cur.as_mut() {
                                c.text.push('\n')
                            }
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
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(t)) if in_text => {
                if let Some(c) = cur.as_mut() {
                    if let Ok(v) = t.xml10_content() {
                        c.text.push_str(&v)
                    }
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"t" => in_text = false,
                b"sp" | b"cxnSp" | b"pic" => {
                    if let Some(c) = cur.take() {
                        push_pending(&mut slide, c, rels, zip);
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
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
) {
    let w = if c.w > 0.0 { c.w } else { 1.0 };
    let h = if c.h > 0.0 { c.h } else { 1.0 };
    let mut el = if c.kind == ElementKind::Image {
        let Some(target) = rels.get(&c.embed) else {
            return;
        };
        let path = format!("ppt/{}", target.trim_start_matches("../"));
        let Ok(mut f) = zip.by_name(&path) else {
            return;
        };
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        };
        let Some(src) = crate::odp::cache_imported_picture(&bytes) else {
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
        e.bold = c.bold;
        e.italic = c.italic;
        e.align = c.align;
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
    el.z = slide.elements.len() as i64;
    slide.elements.push(el);
}

fn read_zip_text<R: Read + std::io::Seek>(
    z: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
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
fn first_text(xml: &str, wanted: &[u8]) -> Option<String> {
    let mut r = Reader::from_str(xml);
    let mut hit = false;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => hit = e.name().as_ref() == wanted,
            Ok(Event::Text(t)) if hit => return t.xml10_content().ok().map(|x| x.into_owned()),
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}
fn attr(e: &BytesStart<'_>, wanted: &[u8]) -> String {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == wanted)
        .map(|a| String::from_utf8_lossy(a.value.as_ref()).into_owned())
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
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn image_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else {
        None
    }
}

fn content_types(n: usize, p: &[Picture]) -> String {
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
    for i in 1..=n {
        s.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#))
    }
    s.push_str("</Types>");
    s
}
fn presentation_xml(d: &Deck) -> String {
    let ids = (1..=d.slides.len())
        .map(|i| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + i, i + 2))
        .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000" type="screen16x9"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    )
}
fn presentation_rels(n: usize) -> String {
    let mut s = String::from(RELS_HEAD);
    s.push_str(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>"#);
    for i in 1..=n {
        s.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{i}.xml"/>"#,i+2))
    }
    s.push_str("</Relationships>");
    s
}
fn slide_rels(p: &[&Picture]) -> String {
    let mut s = String::from(RELS_HEAD);
    s.push_str(r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>"#);
    for (i, x) in p.iter().enumerate() {
        s.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/{}"/>"#,i+2,x.name))
    }
    s.push_str("</Relationships>");
    s
}

const RELS_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#;
fn core_xml(title: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title></cp:coreProperties>"#,
        xml(title)
    )
}
const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#;
const MASTER: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMap accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" bg1="lt1" bg2="lt2" folHlink="folHlink" hlink="hlink" tx1="dk1" tx2="dk2"/><p:sldLayoutIdLst><p:sldLayoutId id="1" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#;
const MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;
const LAYOUT: &str = r#"<?xml version="1.0" encoding="UTF-8"?><p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;
const LAYOUT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;
const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="qOS"><a:themeElements><a:clrScheme name="qOS"><a:dk1><a:srgbClr val="1F2937"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="334155"/></a:dk2><a:lt2><a:srgbClr val="F8FAFC"/></a:lt2><a:accent1><a:srgbClr val="5A60F0"/></a:accent1><a:accent2><a:srgbClr val="10B981"/></a:accent2><a:accent3><a:srgbClr val="F59E0B"/></a:accent3><a:accent4><a:srgbClr val="F43F5E"/></a:accent4><a:accent5><a:srgbClr val="0EA5E9"/></a:accent5><a:accent6><a:srgbClr val="8B5CF6"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="qOS"><a:majorFont/><a:minorFont/></a:fontScheme><a:fmtScheme name="qOS"><a:fillStyleLst/><a:lnStyleLst/><a:effectStyleLst/><a:bgFillStyleLst/></a:fmtScheme></a:themeElements></a:theme>"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_core_content() {
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
            slides: vec![s],
        };
        let b = write_pptx(&d).unwrap();
        assert!(b.starts_with(b"PK"));
        let got = read_pptx(&b).unwrap();
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
    #[test]
    fn bundles_local_picture() {
        let p = std::env::temp_dir().join(format!("slides-pptx-{}.png", std::process::id()));
        std::fs::write(&p, b"\x89PNG\r\n\x1a\nbytes").unwrap();
        let mut s = Slide::new(Layout::Blank);
        s.add(Element::image(&p.to_string_lossy(), 1.0, 2.0, 30.0, 40.0));
        let d = Deck {
            title: "Pictures".into(),
            palette: PaletteKind::Indigo,
            slides: vec![s],
        };
        let b = write_pptx(&d).unwrap();
        let got = read_pptx(&b).unwrap();
        assert_eq!(got.slides[0].elements[0].kind, ElementKind::Image);
        assert!(
            std::fs::read(&got.slides[0].elements[0].src)
                .unwrap()
                .starts_with(b"\x89PNG")
        );
        let _ = std::fs::remove_file(p);
    }
}
