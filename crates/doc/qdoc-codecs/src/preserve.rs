//! Splitting unmodeled package parts into atoms for qdoc's preservation layer.
//!
//! A part the codec does not model is carried through export verbatim. It is stored as ordered
//! atoms so two replicas changing different parts of it merge instead of one clobbering the
//! other; for XML that means one atom per element at the first level where the tree branches.

use qdoc::model::DocMut;

/// Preserve `bytes` under `key`, split into mergeable atoms by [`split_atoms`].
pub fn set_preserved(doc: &mut DocMut<'_>, key: &str, bytes: &[u8]) {
    doc.set_preserved_atoms(key, &split_atoms(bytes));
}

/// Split a preserved part into independently-mergeable atoms whose concatenation is the original bytes verbatim.
/// For an XML part this is the sequence of top-level fragments — each top-level child element is its own atom,
/// with inter-element text riding on adjacent atoms — so replicas modifying different top-level elements touch
/// different atoms. A non-XML or unparseable part stays one whole-part atom. The split never changes the bytes.
pub fn split_atoms(bytes: &[u8]) -> Vec<Vec<u8>> {
    // Only XML parts (`<?xml` prologue or a leading `<` tag) get split; everything
    // else (images, the mimetype literal, binary settings) is one atom.
    let looks_xml = {
        let head: &[u8] = bytes.get(..bytes.len().min(64)).unwrap_or(bytes);
        let trimmed = head
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .and_then(|p| head.get(p..))
            .unwrap_or(head);
        trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<")
    };
    if !looks_xml {
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
    fn split_concatenates_back_verbatim() {
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
        let same_atom = atoms
            .iter()
            .any(|a| a.windows(5).any(|w| w == b"alpha") && a.windows(5).any(|w| w == b"bravo"));
        assert!(!same_atom, "the two styles must be in SEPARATE atoms");
    }

    #[test]
    fn non_xml_part_stays_single_atom() {
        let png = &[0x89u8, b'P', b'N', b'G', 0, 1, 2, 3][..];
        let atoms = split_atoms(png);
        assert_eq!(atoms, vec![png.to_vec()], "binary part is one opaque atom");
    }
}
