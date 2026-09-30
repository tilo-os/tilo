//! A second incremental update over pdfium's export, for what pdfium's API
//! cannot write: appearance streams with embedded fonts (free text,
//! stamps) and reply links between annotations (`/IRT`, `/RT`).
//!
//! pdfium appends every annotation the export creates to its page's
//! `/Annots`, after the removals, in creation order; this pass finds them in
//! pdfium's output by that position (checking each one's `/NM`), writes new
//! versions of those that need it — with `/AP`, `/IRT` and `/RT` — together
//! with the appearance streams and fonts, and appends them as a standard
//! incremental update (objects, a classic cross-reference section and a
//! trailer chained to pdfium's with `/Prev`). Everything is numbered and
//! ordered deterministically, so the same export still produces the same
//! bytes.

use std::collections::BTreeMap;
use std::sync::Arc;

use hayro_syntax::object::{Array, Dict, MaybeRef, Object, ObjectIdentifier};
use pdf_writer::{Chunk, Name, Ref};

use crate::appearance::Appearance;
use crate::font::TextSetter;
use crate::model::ReplyType;

/// What to add to one annotation the export created.
pub(crate) struct AnnotPatch {
    /// Its index in [`crate::edits::Edits::annotations`].
    pub key: usize,
    /// Output page it is on.
    pub page: usize,
    /// Its position among the annotations created on that page.
    pub ordinal: usize,
    /// The `/NM` it was given, to check it was found.
    pub name: String,
    pub appearance: Option<Appearance>,
    /// The key of the annotation it replies to.
    pub reply_to: Option<(usize, ReplyType)>,
}

/// Everything the pass needs: the patches, how many annotations were
/// created on each output page, and the text they set.
#[derive(Default)]
pub(crate) struct PatchPlan {
    pub patches: Vec<AnnotPatch>,
    pub created: BTreeMap<usize, usize>,
    pub setter: TextSetter,
}

/// Why the pass could not run; the caller keeps pdfium's output.
#[derive(Debug)]
pub(crate) struct Declined(pub &'static str);

/// Values copied from the last trailer.
struct Trailer {
    size: i32,
    root: String,
    info: Option<String>,
    id: Option<String>,
}

struct Found {
    id: ObjectIdentifier,
    dict: Vec<u8>,
}

/// Apply `plan` to `out` (pdfium's saved bytes).
pub(crate) fn apply(out: &[u8], plan: PatchPlan) -> Result<Vec<u8>, Declined> {
    let PatchPlan {
        patches,
        created,
        setter,
    } = plan;
    if patches.is_empty() {
        return Ok(out.to_vec());
    }
    let prev = last_startxref(out).ok_or(Declined("no cross-reference section"))?;
    let trailer = read_trailer(out, prev).ok_or(Declined("unreadable trailer"))?;
    let found = find_created(out, &patches, &created)?;

    // Every object number is fixed before anything is written: appearance
    // streams first, then the fonts they use.
    let mut next = trailer.size.max(1);
    let mut appearance_refs: BTreeMap<usize, Ref> = BTreeMap::new();
    for (i, patch) in patches.iter().enumerate() {
        if patch.appearance.is_some() {
            appearance_refs.insert(i, Ref::new(next));
            next = next.saturating_add(1);
        }
    }
    let mut font_chunks: Vec<Chunk> = Vec::new();
    let font_refs: BTreeMap<usize, Ref> = setter
        .embed(&mut font_chunks, &mut next)
        .into_iter()
        .map(|font| (font.index, font.font))
        .collect();

    let mut objects: Vec<(i32, i32, Vec<u8>)> = Vec::new();
    for (i, patch) in patches.iter().enumerate() {
        if let (Some(appearance), Some(r)) = (&patch.appearance, appearance_refs.get(&i)) {
            objects.push((r.get(), 0, appearance_object(*r, appearance, &font_refs)));
        }
    }
    for chunk in font_chunks {
        if let Some(r) = chunk.refs().next() {
            objects.push((r.get(), 0, chunk.as_bytes().to_vec()));
        }
    }
    for (i, patch) in patches.iter().enumerate() {
        let annot = found
            .get(&patch.key)
            .ok_or(Declined("a created annotation is missing from the output"))?;
        let parent = match &patch.reply_to {
            Some((key, kind)) => Some((
                found
                    .get(key)
                    .ok_or(Declined("a reply's parent is missing from the output"))?
                    .id,
                *kind,
            )),
            None => None,
        };
        let body = annotation_object(annot, appearance_refs.get(&i).copied(), parent)
            .ok_or(Declined("an annotation could not be rewritten"))?;
        objects.push((annot.id.obj_number, annot.id.gen_number, body));
    }
    Ok(append_update(
        out,
        objects,
        &trailer,
        next.max(trailer.size),
        prev,
    ))
}

/// The form `XObject` of `appearance`, with its fonts as resources.
fn appearance_object(r: Ref, appearance: &Appearance, fonts: &BTreeMap<usize, Ref>) -> Vec<u8> {
    let mut chunk = Chunk::new();
    {
        let mut form = chunk.form_xobject(r, &appearance.content);
        form.bbox(pdf_writer::Rect::new(
            appearance.bbox.x0,
            appearance.bbox.y0,
            appearance.bbox.x1,
            appearance.bbox.y1,
        ));
        let mut resources = form.resources();
        let mut entries = resources.fonts();
        for index in &appearance.fonts {
            if let Some(font) = fonts.get(index) {
                entries.pair(Name(TextSetter::resource_name(*index).as_bytes()), *font);
            }
        }
    }
    chunk.as_bytes().to_vec()
}

/// A new version of the annotation `annot`: with appearance `appearance`
/// and replying to `parent`, when given.
fn annotation_object(
    annot: &Found,
    appearance: Option<Ref>,
    parent: Option<(ObjectIdentifier, ReplyType)>,
) -> Option<Vec<u8>> {
    use std::fmt::Write as _;
    let mut extra = String::new();
    let mut drop_keys: Vec<&str> = Vec::new();
    if let Some(r) = appearance {
        let _ = write!(extra, "/AP<</N {} 0 R>>", r.get());
        drop_keys.extend(["AP", "AS"]);
    }
    if let Some((id, kind)) = parent {
        let kind = match kind {
            ReplyType::Reply => "R",
            ReplyType::Group => "Group",
        };
        let _ = write!(
            extra,
            "/IRT {} {} R/RT/{kind}",
            id.obj_number, id.gen_number
        );
        drop_keys.extend(["IRT", "RT"]);
    }
    let dict = rewrite_dict(&annot.dict, &drop_keys, &extra)?;
    let mut body = format!("{} {} obj\n", annot.id.obj_number, annot.id.gen_number).into_bytes();
    body.extend_from_slice(&dict);
    body.extend_from_slice(b"\nendobj\n");
    Some(body)
}

/// `out` followed by an incremental update: `objects` (number, generation,
/// serialised object), a classic cross-reference section and a trailer
/// chained to the section at `prev`.
fn append_update(
    out: &[u8],
    objects: Vec<(i32, i32, Vec<u8>)>,
    trailer: &Trailer,
    size: i32,
    prev: usize,
) -> Vec<u8> {
    use std::io::Write as _;
    let mut result = out.to_vec();
    if !result.ends_with(b"\n") {
        result.push(b'\n');
    }
    let mut offsets: BTreeMap<i32, (i32, usize)> = BTreeMap::new();
    for (number, generation, bytes) in objects {
        offsets.insert(number, (generation, result.len()));
        result.extend_from_slice(&bytes);
        if !result.ends_with(b"\n") {
            result.push(b'\n');
        }
    }
    let xref_at = result.len();
    result.extend_from_slice(b"xref\n");
    // One subsection per run of consecutive object numbers.
    let mut runs: Vec<Vec<(i32, i32, usize)>> = Vec::new();
    for (number, (generation, offset)) in &offsets {
        match runs.last_mut() {
            Some(run)
                if run
                    .last()
                    .is_some_and(|(last, _, _)| last.checked_add(1) == Some(*number)) =>
            {
                run.push((*number, *generation, *offset));
            }
            _ => runs.push(vec![(*number, *generation, *offset)]),
        }
    }
    for run in runs {
        let Some((first, _, _)) = run.first() else {
            continue;
        };
        let _ = writeln!(result, "{first} {}", run.len());
        for (_, generation, offset) in &run {
            let _ = write!(result, "{offset:010} {generation:05} n\r\n");
        }
    }
    let _ = write!(
        result,
        "trailer\n<</Size {size}/Prev {prev}/Root {}",
        trailer.root
    );
    if let Some(info) = &trailer.info {
        let _ = write!(result, "/Info {info}");
    }
    if let Some(id) = &trailer.id {
        let _ = write!(result, "/ID {id}");
    }
    let _ = write!(result, ">>\nstartxref\n{xref_at}\n%%EOF\n");
    result
}

/// The offset in the last `startxref`.
fn last_startxref(bytes: &[u8]) -> Option<usize> {
    let tail_from = bytes.len().saturating_sub(2048);
    let tail = bytes.get(tail_from..)?;
    let at = tail.windows(9).rposition(|w| w == b"startxref")?;
    let rest = tail.get(at.saturating_add(9)..)?;
    let digits: String = rest
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit())
        .map(|b| char::from(*b))
        .collect();
    digits.parse().ok().filter(|&o: &usize| o < bytes.len())
}

/// The trailer (or cross-reference stream) dictionary the section at
/// `offset` ends with. Encrypted documents are declined: this pass writes
/// plain objects.
fn read_trailer(bytes: &[u8], offset: usize) -> Option<Trailer> {
    let section = bytes.get(offset..)?;
    let dict_start = if section.starts_with(b"xref") {
        let at = section.windows(7).position(|w| w == b"trailer")?;
        at.saturating_add(7)
    } else {
        // A cross-reference stream: `N G obj <<…>> stream`.
        let at = section.windows(3).position(|w| w == b"obj")?;
        at.saturating_add(3)
    };
    let rest = section.get(dict_start..)?;
    let skip = rest.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let dict = <Dict<'_> as hayro_syntax::object::FromBytes>::from_bytes(rest.get(skip..)?)?;
    if dict.contains_key(b"Encrypt") {
        return None;
    }
    let size = dict.get::<i32>(b"Size")?;
    let root = dict.get_ref(b"Root")?;
    let info = dict.get_ref(b"Info");
    let id = dict
        .get::<Array<'_>>(b"ID")
        .map(|a| format!("[{}]", String::from_utf8_lossy(a.data())));
    Some(Trailer {
        size,
        root: format!("{} {} R", root.obj_number, root.gen_number),
        info: info.map(|r| format!("{} {} R", r.obj_number, r.gen_number)),
        id,
    })
}

/// The annotations `patches` name (and those they reply to), found by
/// their position among the ones created on their page, by key.
fn find_created(
    out: &[u8],
    patches: &[AnnotPatch],
    created: &BTreeMap<usize, usize>,
) -> Result<BTreeMap<usize, Found>, Declined> {
    let pdf = hayro_syntax::Pdf::new(Arc::new(out.to_vec()))
        .map_err(|_| Declined("pdfium's output could not be read back"))?;
    let pages = pdf.pages();
    let mut found = BTreeMap::new();
    for patch in patches {
        let page = pages
            .get(patch.page)
            .ok_or(Declined("a patched page is missing"))?;
        let annots: Vec<MaybeRef<Object<'_>>> = page
            .raw()
            .get::<Array<'_>>(b"Annots")
            .map(|a| a.raw_iter().collect())
            .unwrap_or_default();
        let count = created.get(&patch.page).copied().unwrap_or(0);
        let first = annots
            .len()
            .checked_sub(count)
            .ok_or(Declined("fewer annotations on a page than were created"))?;
        let at = first
            .checked_add(patch.ordinal)
            .ok_or(Declined("annotation position out of range"))?;
        let Some(MaybeRef::Ref(r)) = annots.get(at) else {
            return Err(Declined("a created annotation is not an indirect object"));
        };
        let id: ObjectIdentifier = (*r).into();
        let dict = pdf
            .xref()
            .get::<Dict<'_>>(id)
            .ok_or(Declined("a created annotation could not be read"))?;
        let name = dict
            .get::<hayro_syntax::object::String<'_>>(b"NM")
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        if !patch.name.is_empty() && !pdf_text_eq(&name, &patch.name) {
            return Err(Declined(
                "a created annotation is not where it was expected",
            ));
        }
        found.insert(
            patch.key,
            Found {
                id,
                dict: dict.data().to_vec(),
            },
        );
    }
    // Replies name their parent by key; every parent has a patch of its own.
    if patches
        .iter()
        .filter_map(|p| p.reply_to.as_ref())
        .any(|(parent, _)| !found.contains_key(parent))
    {
        return Err(Declined("a reply's parent was not found"));
    }
    Ok(found)
}

/// Whether the bytes of a PDF text string are `expected`, in either of the
/// encodings a writer uses: one byte per character (`PDFDocEncoding`, which
/// agrees with Latin-1 for the characters that matter here) or UTF-16BE
/// with a byte order mark.
fn pdf_text_eq(read: &[u8], expected: &str) -> bool {
    let single: Option<Vec<u8>> = expected.chars().map(|c| u8::try_from(c).ok()).collect();
    if single.as_deref() == Some(read) {
        return true;
    }
    let mut utf16 = vec![0xFE_u8, 0xFF];
    for unit in expected.encode_utf16() {
        utf16.extend_from_slice(&unit.to_be_bytes());
    }
    utf16 == read
}

/// `dict` re-serialised without `drop` keys and with `extra` entries
/// appended.
fn rewrite_dict(dict: &[u8], drop: &[&str], extra: &str) -> Option<Vec<u8>> {
    let parsed = <Dict<'_> as hayro_syntax::object::FromBytes>::from_bytes(dict)?;
    let mut out = b"<<".to_vec();
    for (key, value) in parsed.entries() {
        if drop.iter().any(|d| d.as_bytes() == key.as_ref()) {
            continue;
        }
        write_name(&mut out, key.as_ref());
        out.push(b' ');
        match value {
            MaybeRef::Ref(r) => {
                out.extend_from_slice(format!("{} {} R", r.obj_number, r.gen_number).as_bytes());
            }
            MaybeRef::NotRef(object) => write_object(&mut out, &object)?,
        }
    }
    out.extend_from_slice(extra.as_bytes());
    out.extend_from_slice(b">>");
    Some(out)
}

fn write_name(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    for &b in name {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'*' | b':') {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn write_object(out: &mut Vec<u8>, object: &Object<'_>) -> Option<()> {
    match object {
        Object::Null(_) => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Number(n) => {
            let v = n.as_f64();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::float_cmp,
                reason = "an integral value within i64's range is written as an integer"
            )]
            if v.fract() == 0.0 && v.abs() < 1e15 {
                out.extend_from_slice(format!("{}", v as i64).as_bytes());
            } else {
                let text = format!("{v:.6}");
                out.extend_from_slice(text.trim_end_matches('0').trim_end_matches('.').as_bytes());
            }
        }
        Object::String(s) => {
            out.push(b'<');
            for b in s.as_bytes() {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Name(n) => write_name(out, n.as_ref()),
        // Nested containers are copied verbatim: their references and
        // strings are already valid PDF syntax.
        Object::Dict(d) => out.extend_from_slice(d.data()),
        Object::Array(a) => {
            out.push(b'[');
            out.extend_from_slice(a.data());
            out.push(b']');
        }
        // An annotation dictionary never holds a direct stream.
        Object::Stream(_) => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_updates_rewrite_entries_after_the_last_startxref() {
        // Phase `rewrites_keep_entries_drop_replaced_keys_and_append`.
        {
            let dict =
                b"<</Type/Annot/Subtype/Text/Rect[1 2 3 4]/AP 9 0 R/T(Ada \\(A\\))/C[1 0 0]/P 5 0 R>>";
            let out = rewrite_dict(dict, &["AP"], "/IRT 7 0 R/RT/R").unwrap();
            let text = String::from_utf8(out.clone()).unwrap();
            assert!(!text.contains("/AP"), "{text}");
            assert!(text.contains("/Rect [1 2 3 4]"), "{text}");
            assert!(text.contains("/P 5 0 R"), "{text}");
            assert!(text.ends_with("/IRT 7 0 R/RT/R>>"), "{text}");
            let reparsed = <Dict<'_> as hayro_syntax::object::FromBytes>::from_bytes(&out).unwrap();
            let author = reparsed
                .get::<hayro_syntax::object::String<'_>>(b"T")
                .unwrap();
            assert_eq!(author.as_bytes(), b"Ada (A)");
        }
        // Phase `finds_the_last_startxref`.
        {
            let bytes = b"%PDF-1.7\nstartxref\n5\n%%EOF\nmore\nstartxref\n12\n%%EOF\n";
            assert_eq!(last_startxref(bytes), Some(12));
        }
    }
}
