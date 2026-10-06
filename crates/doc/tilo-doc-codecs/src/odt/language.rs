//! Document default language ⇄ ODF default paragraph style, carried as
//! `<style:default-style style:family="paragraph"><style:text-properties fo:language="xx"
//! fo:country="YY"/></style:default-style>` inside `styles.xml`'s `<office:styles>`. This
//! maps it onto the document-wide language setting, splitting/reassembling the BCP-47 tag
//! (`de-DE` ⇄ `de`+`DE`) for spell-check locale and accessibility/screen-reader use.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::{DocMut, DocRef, language as model_language};

use super::styles::reassemble_bcp47;
use crate::xml_escape::escape_attr;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// Import the document default language from the first paragraph-family
/// `style:default-style` in `styles.xml`. No-op when none carries `fo:language`.
pub(super) fn import_default_language(styles_xml: &[u8], doc: &mut DocMut) {
    if let Some(code) = parse_default_language(styles_xml) {
        doc.set_language(&code);
    }
}

/// Scan `styles.xml` for the paragraph-family `<style:default-style>`'s language attributes
/// and reassemble the BCP-47 tag. Only the *paragraph* default-style is inspected.
fn parse_default_language(styles_xml: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(styles_xml).ok()?;
    let mut reader = Reader::from_str(s);
    let mut in_para_default = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"style:default-style" => {
                in_para_default = attr(&e, b"style:family").as_deref() == Some("paragraph");
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"style:default-style" => {
                in_para_default = false;
            }
            Ok(Event::Start(e) | Event::Empty(e))
                if in_para_default && e.name().as_ref() == b"style:text-properties" =>
            {
                let lang = attr(&e, b"fo:language");
                let country = attr(&e, b"fo:country");
                if let Some(code) = reassemble_bcp47(lang.as_deref(), country.as_deref()) {
                    return Some(code);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

/// Read a quick-xml element attribute by key as an owned, unescaped `String`.
fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| crate::unescape_attr(&a))
}

/// Splice the document default language into `styles_xml`'s paragraph `style:default-style`,
/// or `None` when the document declares none (original left untouched); replaces an
/// existing default-style's language or injects a fresh one into `<office:styles>`.
pub(super) fn export_default_language(doc: &DocRef, styles_xml: &[u8]) -> Option<Vec<u8>> {
    let code = model_language(doc.doc, doc.c)?;
    let (lang, country) = split_bcp47(&code);
    Some(splice_default_language(
        styles_xml,
        &lang,
        country.as_deref(),
    ))
}

/// Split a BCP-47 tag into `(language, optional country)` (`de-DE` → `("de", Some("DE"))`,
/// `fr` → `("fr", None)`).
fn split_bcp47(code: &str) -> (String, Option<String>) {
    match code.split_once('-') {
        Some((l, c)) if !c.is_empty() => (l.to_string(), Some(c.to_string())),
        _ => (code.to_string(), None),
    }
}

/// The `<style:text-properties …/>` attribute fragment for a language/country.
fn lang_attrs(lang: &str, country: Option<&str>) -> String {
    let mut s = format!(r#"fo:language="{}""#, escape_attr(lang));
    if let Some(c) = country {
        let _ = write!(s, r#" fo:country="{}""#, escape_attr(c));
    }
    s
}

/// The full paragraph-family `style:default-style` element for a language.
fn default_style_xml(lang: &str, country: Option<&str>) -> String {
    format!(
        r#"<style:default-style style:family="paragraph"><style:text-properties {}/></style:default-style>"#,
        lang_attrs(lang, country)
    )
}

/// Splice the default language into `styles.xml`, reusing an existing paragraph
/// `style:default-style` or injecting a fresh one into `<office:styles>`.
fn splice_default_language(styles_xml: &[u8], lang: &str, country: Option<&str>) -> Vec<u8> {
    let s = String::from_utf8_lossy(styles_xml).into_owned();

    // Case 1: a paragraph default-style exists — rewrite it wholesale; other text-properties on a default style are uncommon, so re-synthesising keeps the splice simple.
    if let Some((start, end)) = find_para_default_style(&s) {
        let mut out = String::with_capacity(s.len());
        out.push_str(s.get(..start).unwrap_or_default());
        out.push_str(&default_style_xml(lang, country));
        out.push_str(s.get(end..).unwrap_or_default());
        return out.into_bytes();
    }

    let elem = default_style_xml(lang, country);

    // Case 2: an `<office:styles>` section exists — inject right after its open tag (default styles conventionally lead the section).
    if let Some(open_end) = open_tag_end(&s, "<office:styles") {
        // A capacity hint only; bounded by the styles.xml + injected-element
        // sizes, nowhere near `usize::MAX`.
        let mut out = String::with_capacity(s.len().saturating_add(elem.len()));
        out.push_str(s.get(..open_end).unwrap_or_default());
        out.push_str(&elem);
        out.push_str(s.get(open_end..).unwrap_or_default());
        return out.into_bytes();
    }
    // Case 2b: a self-closing `<office:styles/>` — expand it to a paired element.
    if let Some((sc_start, sc_end)) = self_closing_span(&s, "<office:styles") {
        // A capacity hint only; bounded by the styles.xml + injected-element
        // sizes, nowhere near `usize::MAX`.
        let mut out = String::with_capacity(s.len().saturating_add(elem.len()));
        out.push_str(s.get(..sc_start).unwrap_or_default());
        out.push_str("<office:styles>");
        out.push_str(&elem);
        out.push_str("</office:styles>");
        out.push_str(s.get(sc_end..).unwrap_or_default());
        return out.into_bytes();
    }

    // Case 3: no `<office:styles>` at all — synthesise one. ODF orders `office:styles` before `office:automatic-styles`/`office:master-styles`.
    let section = format!("<office:styles>{elem}</office:styles>");
    let at = s
        .find("<office:automatic-styles")
        .or_else(|| s.find("<office:master-styles"))
        .or_else(|| s.rfind("</office:document-styles>"));
    if let Some(pos) = at {
        // A capacity hint only; bounded by the styles.xml + injected-section
        // sizes, nowhere near `usize::MAX`.
        let mut out = String::with_capacity(s.len().saturating_add(section.len()));
        out.push_str(s.get(..pos).unwrap_or_default());
        out.push_str(&section);
        out.push_str(s.get(pos..).unwrap_or_default());
        out.into_bytes()
    } else {
        let mut out = s;
        out.push_str(&section);
        out.into_bytes()
    }
}

/// Locate the first paragraph-family `<style:default-style>` (or self-closing form), by `(start, end)` byte span.
fn find_para_default_style(s: &str) -> Option<(usize, usize)> {
    let needle = "<style:default-style";
    let mut from = 0;
    while let Some(rel) = s.get(from..)?.find(needle) {
        let start = from.checked_add(rel)?;
        let after_name = start.checked_add(needle.len())?;
        // The char after the element name must be a delimiter, so `<style:default-style-x` doesn't match.
        let is_delim = matches!(
            s.as_bytes().get(after_name),
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        );
        let gt = start.checked_add(s.get(start..)?.find('>')?)?;
        let open_tag = s.get(start..=gt)?;
        if is_delim && open_tag.contains(r#"style:family="paragraph""#) {
            if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
                return Some((start, gt.checked_add(1)?));
            }
            let close = "</style:default-style>";
            let end = gt
                .checked_add(s.get(gt..)?.find(close)?)?
                .checked_add(close.len())?;
            return Some((start, end));
        }
        from = gt.checked_add(1)?;
    }
    None
}

/// Byte index just past the `>` of the first `{open_prefix}` open tag, or `None` if self-closing/absent.
fn open_tag_end(s: &str, open_prefix: &str) -> Option<usize> {
    let open = s.find(open_prefix)?;
    let gt = open.checked_add(s.get(open..)?.find('>')?)?;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None; // self-closing — handled separately
    }
    gt.checked_add(1)
}

/// The `(start, end)` byte span of a self-closing `{open_prefix} …/>` tag, or `None`.
fn self_closing_span(s: &str, open_prefix: &str) -> Option<(usize, usize)> {
    let open = s.find(open_prefix)?;
    let gt = open.checked_add(s.get(open..)?.find('>')?)?;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        Some((open, gt.checked_add(1)?))
    } else {
        None
    }
}
