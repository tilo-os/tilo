//! Document default language ⇄ ODF default paragraph style. ODF carries a document
//! default language on the paragraph-family default style, as
//! `<style:default-style style:family="paragraph"><style:text-properties
//! fo:language="xx" fo:country="YY"/></style:default-style>` inside `styles.xml`'s
//! `<office:styles>` section. This maps that `fo:language`/`fo:country` onto the
//! document-wide language setting (both directions), splitting / reassembling the
//! BCP-47 tag (`de-DE` ⇄ `de`+`DE`). Supports spell-check locale selection and
//! accessibility/screen-reader language.

use crate::model::{language as model_language, DocMut, DocRef};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::styles::reassemble_bcp47;

/// Import the document default language from the first paragraph-family
/// `style:default-style` in `styles.xml` into the document-wide language setting.
/// No-op when no such default-style carries an `fo:language` (the document then
/// declares no default language).
pub(super) fn import_default_language(styles_xml: &[u8], doc: &mut DocMut) {
    if let Some(code) = parse_default_language(styles_xml) {
        doc.set_language(&code);
    }
}

/// Scan `styles.xml` for the paragraph-family `<style:default-style>`'s
/// `<style:text-properties fo:language=… fo:country=…>` and reassemble the BCP-47
/// tag. Returns `None` when absent. Only the *paragraph* default-style is inspected
/// (other families' defaults carry their own, unrelated, language attributes).
fn parse_default_language(styles_xml: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(styles_xml).ok()?;
    let mut reader = Reader::from_str(s);
    // Whether we're inside `<style:default-style style:family="paragraph">`.
    let mut in_para_default = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"style:default-style" => {
                in_para_default = attr(&e, b"style:family").as_deref() == Some("paragraph");
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"style:default-style" => {
                in_para_default = false;
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
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
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Splice the document default language into `styles_xml`'s paragraph-family
/// `style:default-style`, returning the updated bytes — or `None` when the document
/// declares no default language (so the original `styles.xml` is left untouched).
///
/// If a paragraph `style:default-style` already exists, its `style:text-properties`
/// gets the `fo:language`/`fo:country` (replacing any prior pair); otherwise a fresh
/// default-style is injected into the `<office:styles>` section (created if absent).
pub(super) fn export_default_language(doc: &DocRef, styles_xml: &[u8]) -> Option<Vec<u8>> {
    let code = model_language(doc.doc, doc.c)?;
    let (lang, country) = split_bcp47(&code);
    Some(splice_default_language(styles_xml, &lang, country.as_deref()))
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
        s.push_str(&format!(r#" fo:country="{}""#, escape_attr(c)));
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

/// Splice the default language into `styles.xml`. Reuses the existing paragraph
/// `style:default-style` when present (replacing its language attributes); otherwise
/// injects a fresh one into (or creates) the `<office:styles>` section.
fn splice_default_language(styles_xml: &[u8], lang: &str, country: Option<&str>) -> Vec<u8> {
    let s = String::from_utf8_lossy(styles_xml).into_owned();

    // Case 1: a paragraph `style:default-style` already exists — rewrite it wholesale
    // (its only modeled content is the language; other text-properties on a default
    // style are uncommon and re-synthesising keeps the splice simple and correct).
    if let Some((start, end)) = find_para_default_style(&s) {
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..start]);
        out.push_str(&default_style_xml(lang, country));
        out.push_str(&s[end..]);
        return out.into_bytes();
    }

    let elem = default_style_xml(lang, country);

    // Case 2: an `<office:styles>` section exists — inject the default-style right
    // after its open tag (default styles conventionally lead the section).
    if let Some(open_end) = open_tag_end(&s, "<office:styles") {
        let mut out = String::with_capacity(s.len() + elem.len());
        out.push_str(&s[..open_end]);
        out.push_str(&elem);
        out.push_str(&s[open_end..]);
        return out.into_bytes();
    }
    // Case 2b: a self-closing `<office:styles/>` — expand it to a paired element.
    if let Some((sc_start, sc_end)) = self_closing_span(&s, "<office:styles") {
        let mut out = String::with_capacity(s.len() + elem.len());
        out.push_str(&s[..sc_start]);
        out.push_str("<office:styles>");
        out.push_str(&elem);
        out.push_str("</office:styles>");
        out.push_str(&s[sc_end..]);
        return out.into_bytes();
    }

    // Case 3: no `<office:styles>` at all — synthesise one holding the default-style.
    // ODF orders `office:styles` before `office:automatic-styles`/`office:master-styles`.
    let section = format!("<office:styles>{elem}</office:styles>");
    let at = s
        .find("<office:automatic-styles")
        .or_else(|| s.find("<office:master-styles"))
        .or_else(|| s.rfind("</office:document-styles>"));
    match at {
        Some(pos) => {
            let mut out = String::with_capacity(s.len() + section.len());
            out.push_str(&s[..pos]);
            out.push_str(&section);
            out.push_str(&s[pos..]);
            out.into_bytes()
        }
        None => {
            let mut out = s;
            out.push_str(&section);
            out.into_bytes()
        }
    }
}

/// Locate the first paragraph-family `<style:default-style …>…</style:default-style>`
/// (or self-closing form), returning its `(start, end)` byte span. Matches only the
/// `style:family="paragraph"` default-style (other families are left untouched).
fn find_para_default_style(s: &str) -> Option<(usize, usize)> {
    let needle = "<style:default-style";
    let mut from = 0;
    while let Some(rel) = s[from..].find(needle) {
        let start = from + rel;
        let after_name = start + needle.len();
        // The char after the element name must be a delimiter (so `<style:default-style-x`
        // doesn't match).
        let is_delim = matches!(s.as_bytes().get(after_name), Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/'));
        let gt = match s[start..].find('>') {
            Some(g) => start + g,
            None => return None,
        };
        let open_tag = &s[start..=gt];
        if is_delim && open_tag.contains(r#"style:family="paragraph""#) {
            if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
                // Self-closing.
                return Some((start, gt + 1));
            }
            let close = "</style:default-style>";
            let end = s[gt..].find(close)? + gt + close.len();
            return Some((start, end));
        }
        from = gt + 1;
    }
    None
}

/// Byte index just past the `>` of the first `{open_prefix}` open tag, or `None` if
/// it's self-closing / absent.
fn open_tag_end(s: &str, open_prefix: &str) -> Option<usize> {
    let open = s.find(open_prefix)?;
    let gt = s[open..].find('>')? + open;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None; // self-closing — handled separately
    }
    Some(gt + 1)
}

/// The `(start, end)` byte span of a self-closing `{open_prefix} …/>` tag, or `None`.
fn self_closing_span(s: &str, open_prefix: &str) -> Option<(usize, usize)> {
    let open = s.find(open_prefix)?;
    let gt = s[open..].find('>')? + open;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        Some((open, gt + 1))
    } else {
        None
    }
}

/// Escape a string for use inside a double-quoted XML attribute value.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}
