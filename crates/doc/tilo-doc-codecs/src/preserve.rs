//! Keeping what the office codecs do not model: unmodeled package parts split into atoms for
//! qdoc's preservation layer, and unmodeled body content kept in place in the model.
//!
//! A part the codec does not model is carried through export verbatim. It is stored as ordered
//! atoms so two replicas changing different parts of it merge instead of one clobbering the
//! other; for XML that means one atom per element at the first level where the tree branches.

//!
//! Unmodeled content *inside* the regenerated body part is kept in place instead:
//! - a body-level element becomes a `fidelity` block whose `xml` attr holds the element verbatim
//!   and whose [`ATTR_FORMAT`] attr names the format it came from;
//! - an element inside a paragraph (a field, content control, formula, drawing that is not a
//!   plain image, …) becomes its visible text in the paragraph — a field's result, a content
//!   control's content — or one [`FIELD_PLACEHOLDER`] when it shows no text, covered by a mark
//!   ([`MARK_DOCX_XML`] / [`MARK_ODT_XML`]) whose value is the element verbatim.
//!
//! Both are re-emitted verbatim at the same position by the codec that captured them, and
//! skipped by the other one (the XML of one format means nothing in the other). An inline
//! element's text edits like any other; once it no longer matches the element (see
//! [`run_content`]), the edited text is written instead of the element. Deleting the block,
//! or all of an element's text, deletes the element.

use std::fmt::Write as _;
use tilo_content::ContentInputs;
use tilo_doc::model::DocMut;
use tilo_doc::schema::FIELD_PLACEHOLDER;
use tilo_doc::snapshot::BlockView;

pub use tilo_doc::schema::{MARK_DOCX_XML, MARK_ODT_XML};

/// Attr of a `fidelity` block naming the format its `xml` came from (`docx` / `odt`).
pub const ATTR_FORMAT: &str = "format";

/// Whether a block attr is one an office codec keeps for its own export — markup kept
/// verbatim, an original id — rather than part of the canonical model (`docx_…`, `odt_…`).
/// Other codecs ignore these; the one that wrote them uses them only while the modeled
/// attrs still agree with them.
#[must_use]
pub fn is_codec_private_attr(key: &str) -> bool {
    key.starts_with("docx_") || key.starts_with("odt_")
}

/// Whether two block sequences hold the same content: kinds, text, marks and attrs, whatever
/// their ids. Tells an unedited region from an edited one by comparing it with a fresh import
/// of its original, so the original can be written back with everything the model lacks.
#[must_use]
pub fn same_blocks(a: &[BlockView], b: &[BlockView]) -> bool {
    let key = |v: &BlockView| {
        let mut marks: Vec<String> = v
            .marks
            .iter()
            .map(|m| format!("{}:{}:{}:{:?}", m.name, m.start, m.end, m.value))
            .collect();
        marks.sort();
        (v.kind.clone(), v.text.clone(), marks, v.attrs.clone())
    };
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| key(x) == key(y))
}

/// The verbatim XML of a `fidelity` block if `format` may re-emit it: blocks captured by the
/// other office codec are skipped; blocks without a format tag predate it and are kept.
pub fn fidelity_xml<'a>(b: &'a BlockView, format: &str) -> Option<&'a str> {
    use tilo_doc::AttrVal;
    let Some(AttrVal::Str(xml)) = b.attrs.get("xml") else {
        return None;
    };
    match b.attrs.get(ATTR_FORMAT) {
        Some(AttrVal::Str(f)) if f != format => None,
        _ => Some(xml.as_str()),
    }
}

/// What an exporter writes for one run of text: the verbatim element to write first (its own
/// format's kept element, while the run still starts with the element's visible text), then
/// `text` as ordinary runs. Text of an element the user edited, or of an element kept by the
/// other office codec, is written as ordinary text — the edit wins over the kept XML, which is
/// dropped — and placeholders of elements without visible text are left out.
pub struct RunContent<'a> {
    pub kept: Option<&'a str>,
    pub text: std::borrow::Cow<'a, str>,
}

/// See [`RunContent`]; `own_mark` is the exporter's own format's mark.
#[must_use]
pub fn run_content<'a>(
    text: &'a str,
    marks: &[&'a tilo_doc::document::MarkSpan],
    own_mark: &str,
) -> RunContent<'a> {
    use std::borrow::Cow;
    let strip = |t: &'a str| -> Cow<'a, str> {
        if t.contains(FIELD_PLACEHOLDER) {
            Cow::Owned(t.chars().filter(|c| *c != FIELD_PLACEHOLDER).collect())
        } else {
            Cow::Borrowed(t)
        }
    };
    if let Some(xml) = marks
        .iter()
        .find(|m| m.name == own_mark)
        .and_then(|m| m.value.as_str())
    {
        let covered = covered_text(xml, own_mark);
        // Text typed right after the element joins its mark: it follows the element.
        if let Some(rest) = text.strip_prefix(covered.as_str()) {
            return RunContent {
                kept: Some(xml),
                text: strip(rest),
            };
        }
    }
    let kept_elsewhere = marks
        .iter()
        .any(|m| tilo_doc::schema::KEPT_INLINE_MARKS.contains(&m.name.as_str()));
    RunContent {
        kept: None,
        text: if kept_elsewhere {
            strip(text)
        } else {
            Cow::Borrowed(text)
        },
    }
}

/// The text an inline element kept verbatim covers in the model: its visible text (a field's
/// result, a content control's content), or one placeholder when it shows no text (a drawing,
/// a formula). Import and export both derive it from the XML, so export can tell whether the
/// user changed it.
#[must_use]
pub fn covered_text(xml: &str, mark: &str) -> String {
    let visible = if mark == MARK_ODT_XML {
        odt_visible_text(xml)
    } else {
        docx_visible_text(xml)
    };
    if visible.is_empty() {
        FIELD_PLACEHOLDER.to_string()
    } else {
        visible
    }
}

/// The visible text of a `WordprocessingML` fragment: its `w:t` text, tabs and line breaks,
/// leaving out field instructions, deleted text, and drawings, pictures, embedded objects and
/// formulas (which are not text).
fn docx_visible_text(xml: &str) -> String {
    const HIDDEN: &[&[u8]] = &[
        b"w:instrText",
        b"w:delInstrText",
        b"w:delText",
        b"w:drawing",
        b"w:pict",
        b"w:object",
        b"mc:AlternateContent",
        b"m:oMath",
        b"m:oMathPara",
        b"w:sdtPr",
        b"w:rPr",
    ];
    visible_text(
        xml,
        |name| HIDDEN.contains(&name),
        |e, out| match e.name().as_ref() {
            b"w:tab" => out.push('\t'),
            b"w:cr" => out.push('\n'),
            b"w:br" => {
                let wrapping = e
                    .attributes()
                    .flatten()
                    .all(|a| a.key.as_ref() != b"w:type" || a.value.as_ref() == b"textWrapping");
                if wrapping {
                    out.push('\n');
                }
            }
            _ => {}
        },
        |name| name == b"w:t",
    )
}

/// The visible text of an ODF paragraph-content element: its text, `text:tab`, `text:s` and
/// `text:line-break`, leaving out drawings and frames, notes and annotations.
fn odt_visible_text(xml: &str) -> String {
    let hidden = |name: &[u8]| {
        name.starts_with(b"draw:")
            || name.starts_with(b"svg:")
            || matches!(
                name,
                b"office:annotation" | b"text:note" | b"text:tracked-changes"
            )
    };
    visible_text(
        xml,
        hidden,
        |e, out| match e.name().as_ref() {
            b"text:tab" => out.push('\t'),
            b"text:line-break" => out.push('\n'),
            b"text:s" => {
                let n = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"text:c")
                    .and_then(|a| std::str::from_utf8(&a.value).ok()?.parse::<usize>().ok())
                    .unwrap_or(1)
                    .min(256);
                out.push_str(&" ".repeat(n));
            }
            _ => {}
        },
        |_| true,
    )
}

/// The text of `xml` outside `hidden` elements: character data inside elements `text_in`
/// accepts, plus what `empty` adds for empty elements. Layout whitespace between elements
/// is not text.
fn visible_text(
    xml: &str,
    hidden: impl Fn(&[u8]) -> bool,
    empty: impl Fn(&quick_xml::events::BytesStart, &mut String),
    text_in: impl Fn(&[u8]) -> bool,
) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut skip = 0usize;
    let mut open: Vec<Vec<u8>> = Vec::new();
    loop {
        let Ok(ev) = reader.read_event() else {
            return out;
        };
        match ev {
            Event::Start(e) => {
                if skip > 0 || hidden(e.name().as_ref()) {
                    skip = skip.saturating_add(1);
                }
                open.push(e.name().as_ref().to_vec());
            }
            Event::End(_) => {
                open.pop();
                skip = skip.saturating_sub(1);
            }
            Event::Empty(e) if skip == 0 && !hidden(e.name().as_ref()) => empty(&e, &mut out),
            Event::Text(t) if skip == 0 && open.last().is_some_and(|n| text_in(n)) => {
                if let Ok(txt) = t.xml10_content()
                    // Pretty-printer indentation between elements, not text.
                    && !(txt.chars().all(char::is_whitespace) && txt.contains(['\n', '\r']))
                {
                    out.push_str(&txt);
                }
            }
            Event::GeneralRef(r) if skip == 0 && open.last().is_some_and(|n| text_in(n)) => {
                out.extend(crate::resolve_general_ref(&r));
            }
            Event::Eof => return out,
            _ => {}
        }
    }
}

/// The start tag of an XML document's root element (`<w:document …>`), for re-emitting a
/// regenerated part under the original namespace declarations.
pub fn root_start_tag(xml: &[u8]) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let s = std::str::from_utf8(xml).ok()?;
    let mut reader = Reader::from_str(s);
    loop {
        let before = usize::try_from(reader.buffer_position()).ok()?;
        match reader.read_event().ok()? {
            Event::Start(_) => {
                let after = usize::try_from(reader.buffer_position()).ok()?;
                return s.get(before..after).map(str::to_string);
            }
            Event::Eof | Event::Empty(_) => return None,
            _ => {}
        }
    }
}

/// `original` (a root start tag, `<p:root a="…">`) with every `xmlns:prefix` of `required`
/// it does not declare yet appended, so both the carried-through fragments (which use the
/// original's prefixes) and the regenerated markup resolve. `None` when `original` is not a
/// start tag of the element `name`.
pub fn merge_root_namespaces(
    original: &str,
    name: &str,
    required: &[(&str, &str)],
) -> Option<String> {
    let body = original
        .trim()
        .strip_prefix('<')?
        .strip_suffix('>')?
        .strip_prefix(name)?;
    if !body.is_empty() && !body.starts_with(char::is_whitespace) {
        return None;
    }
    let mut out = format!("<{name}{body}");
    for (prefix, uri) in required {
        let decl = format!("xmlns:{prefix}=");
        if !body.contains(&decl) {
            let _ = write!(out, r#" xmlns:{prefix}="{uri}""#);
        }
    }
    out.push('>');
    Some(out)
}

/// Preserve `bytes` under `key`. A text part (XML, the ODF `mimetype`) is kept in the document, split
/// into mergeable atoms by [`split_atoms`]; a binary part (an image, a font, an embedded object) is a
/// binary input: it goes into `inputs` for the caller's content store and the document keeps only
/// its address.
pub fn set_preserved(doc: &mut DocMut<'_>, key: &str, bytes: &[u8], inputs: &mut ContentInputs) {
    if std::str::from_utf8(bytes).is_ok() {
        doc.set_preserved_atoms(key, &split_atoms(bytes));
    } else {
        let meta = tilo_content::sniff(bytes);
        let hash = inputs.add(bytes);
        doc.set_preserved_content(key, &hash, &meta);
    }
}

/// Whether a part starts like XML (an `<?xml` prologue or a leading tag, after whitespace).
fn looks_like_xml(bytes: &[u8]) -> bool {
    let head: &[u8] = bytes.get(..bytes.len().min(64)).unwrap_or(bytes);
    let trimmed = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .and_then(|p| head.get(p..))
        .unwrap_or(head);
    trimmed.starts_with(b"<")
}

/// Split a preserved part into independently-mergeable atoms whose concatenation is the original bytes verbatim.
/// For an XML part this is the sequence of top-level fragments — each top-level child element is its own atom,
/// with inter-element text riding on adjacent atoms — so replicas modifying different top-level elements touch
/// different atoms. A non-XML or unparseable part stays one whole-part atom. The split never changes the bytes.
pub fn split_atoms(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Only XML parts (`<?xml` prologue or a leading `<` tag) get split; everything
    // else (images, the mimetype literal, binary settings) is one atom.
    if !looks_like_xml(bytes) {
        return vec![bytes.to_vec()];
    }
    match split_xml_top_level(bytes) {
        Some(atoms) if atoms.len() > 1 => atoms,
        _ => vec![bytes.to_vec()],
    }
}

/// One parsed XML element span: its nesting `depth` (root = 0) and the byte offset `start` of its opening `<`.
struct ElemSpan {
    depth: usize,
    start: usize,
}

/// Parse the depth + start offset of every element in `bytes`. Returns `None` on any parse error or
/// imbalance (caller then keeps the part as a single atom — never corrupts it).
fn parse_elem_spans(bytes: &[u8]) -> Option<Vec<ElemSpan>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let s = std::str::from_utf8(bytes).ok()?;
    // quick-xml 0.36 defaults `expand_empty_elements` to false, so a self-closing
    // `<x/>` arrives as one `Event::Empty` rather than a Start+End pair.
    let mut reader = Reader::from_str(s);
    let mut depth: usize = 0;
    let mut spans: Vec<ElemSpan> = Vec::new();
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(_)) => {
                spans.push(ElemSpan {
                    depth,
                    start: before,
                });
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(_)) => {
                spans.push(ElemSpan {
                    depth,
                    start: before,
                });
            }
            Ok(Event::End(_)) => {
                depth = depth.checked_sub(1)?;
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    if depth != 0 {
        return None; // unbalanced
    }
    Some(spans)
}

/// Split XML bytes into per-element atoms, descending through single-child wrapper layers so the split lands
/// where the document actually *branches* (the first element with 2+ element children). E.g. for
/// `<doc><styles><a/><b/></styles></doc>` the atoms are the prologue+`<doc><styles>` open, `<a/>`, then
/// `<b/></styles></doc>`. Atoms always concatenate back to `bytes` byte-for-byte; `None` if no branch point exists or on parse error.
fn split_xml_top_level(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let spans = parse_elem_spans(bytes)?;
    if spans.is_empty() {
        return None;
    }
    // Find the shallowest depth that branches into 2+ sibling elements under a single shared parent — that's
    // where the independent unmodeled atoms live. We descend single-child wrapper layers transparently: a depth
    // is a valid split point only if its parent held exactly one element. Depth 0's "parent" is the implicit document root.
    let max_depth = spans.iter().map(|s| s.depth).max().unwrap_or(0);
    for d in 0..=max_depth {
        // Require a single shared parent: exactly one element at the parent depth.
        if d > 0
            && spans
                .iter()
                .filter(|s| s.depth == d.saturating_sub(1))
                .count()
                != 1
        {
            break; // the tree already branched above this depth — stop descending
        }
        // Elements at this depth, in source order.
        let mut at_d: Vec<&ElemSpan> = spans.iter().filter(|s| s.depth == d).collect();
        at_d.sort_by_key(|s| s.start);
        if at_d.len() < 2 {
            continue; // single wrapper (or none) at this depth — descend further
        }
        // Split at the boundaries between sibling elements: the leading atom is everything before the first sibling
        // (prologue + ancestor open tags); the final atom runs to EOF (last sibling + ancestor close tags + trailing text).
        let mut boundaries: Vec<usize> = Vec::new();
        for s in at_d.iter().skip(1) {
            boundaries.push(s.start);
        }
        if boundaries.is_empty() {
            continue;
        }
        let mut atoms: Vec<Vec<u8>> = Vec::with_capacity(boundaries.len().saturating_add(1));
        let mut prev = 0usize;
        for &b in &boundaries {
            atoms.push(bytes.get(prev..b)?.to_vec());
            prev = b;
        }
        atoms.push(bytes.get(prev..)?.to_vec());
        // Byte-fidelity guard: atoms MUST concatenate back to the source exactly.
        let total: usize = atoms.iter().map(std::vec::Vec::len).sum();
        if total != bytes.len() {
            return None;
        }
        return Some(atoms);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `styles.xml`-shaped part with two independent top-level style atoms — the
    /// canonical "two unmodeled regions in one part" shape that used to clobber.
    const TWO_STYLE_PART: &[u8] = br#"<?xml version="1.0"?>
<office:styles xmlns:office="o" xmlns:style="s">
<style:style style:name="A">alpha</style:style>
<style:style style:name="B">bravo</style:style>
</office:styles>"#;

    #[test]
    fn preserved_parts_split_into_atoms_and_concatenate_back() {
        // Phase `split_concatenates_back_verbatim`.
        {
            let atoms = split_atoms(TWO_STYLE_PART);
            // Two depth-1 siblings → two atoms: [prologue + root-open + style A], [style B + root-close]
            // (the first sibling rides with the leading material; each subsequent sibling opens a new atom).
            assert_eq!(atoms.len(), 2, "two style atoms: {atoms:?}");
            let joined: Vec<u8> = atoms.iter().flatten().copied().collect();
            assert_eq!(
                joined, TWO_STYLE_PART,
                "atoms must concatenate to the source byte-for-byte"
            );
            let has_a = atoms.iter().any(|a| a.windows(5).any(|w| w == b"alpha"));
            let has_b = atoms.iter().any(|a| a.windows(5).any(|w| w == b"bravo"));
            assert!(has_a && has_b, "both style atoms present in the split");
            let same_atom = atoms.iter().any(|a| {
                a.windows(5).any(|w| w == b"alpha") && a.windows(5).any(|w| w == b"bravo")
            });
            assert!(!same_atom, "the two styles must be in SEPARATE atoms");
        }
        // Phase `non_xml_part_stays_single_atom`.
        {
            let png = &[0x89u8, b'P', b'N', b'G', 0, 1, 2, 3][..];
            let atoms = split_atoms(png);
            assert_eq!(atoms, vec![png.to_vec()], "binary part is one opaque atom");
        }
    }
}
