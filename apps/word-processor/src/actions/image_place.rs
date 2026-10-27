//! Where `place-image` puts a picture named by the text of the heading (or
//! paragraph) it goes after: an exact heading wins over a contents line or
//! a mention of the same words earlier, and a heading written loosely (other
//! case, punctuation, a typo, words running on past it) still finds its
//! place. Nothing like the text is refused with the headings to retry with.

use automerge::AutoCommit;
use ui::tilo_resource::Place;

/// The paragraph kinds an image is placed after.
const TEXT_BLOCKS: &[&str] = &["paragraph", "heading", "list_item"];
/// How many headings a refusal lists.
const LISTED_HEADINGS: usize = 20;
/// How alike an anchor and a paragraph must be for a loose match, 0 to 1.
const LOOSE_MATCH: f64 = 0.75;
/// The longest anchor compared character by character.
const MAX_COMPARED_CHARS: usize = 200;

/// A body paragraph an image may go after.
struct Paragraph {
    id: String,
    text: String,
    heading: bool,
}

/// The caret at the end of the paragraph `after` names in the body of
/// `doc`, where the image goes.
///
/// # Errors
/// Nothing in the body is like `after`: the refusal lists the headings.
pub(crate) fn after_paragraph(doc: &AutoCommit, after: &str) -> Result<Place, String> {
    let containers = crate::model::open(doc)?;
    let paragraphs: Vec<Paragraph> =
        tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
            .into_iter()
            .filter(|block| TEXT_BLOCKS.contains(&block.kind.as_str()))
            .map(|block| Paragraph {
                heading: block.kind == "heading",
                id: block.id,
                text: block.text,
            })
            .collect();
    let found = named(&paragraphs, after).ok_or_else(|| unmatched(&paragraphs, after))?;
    Ok(Place::Caret {
        item: found.id.clone(),
        at: found.text.chars().count(),
    })
}

/// The paragraph `after` names: a heading that is exactly it (ignoring
/// case, spacing and punctuation); else the first containing it as written;
/// else the first containing it loosely; else the longest one it begins
/// with; else the most alike above [`LOOSE_MATCH`].
fn named<'a>(paragraphs: &'a [Paragraph], after: &str) -> Option<&'a Paragraph> {
    let wanted = normalized(after);
    if wanted.is_empty() {
        return None;
    }
    if let Some(found) = paragraphs
        .iter()
        .find(|paragraph| paragraph.heading && normalized(&paragraph.text) == wanted)
    {
        return Some(found);
    }
    if let Some(found) = paragraphs
        .iter()
        .find(|paragraph| paragraph.text.contains(after.trim()))
    {
        return Some(found);
    }
    let texts: Vec<String> = paragraphs
        .iter()
        .map(|paragraph| normalized(&paragraph.text))
        .collect();
    let at = texts
        .iter()
        .position(|text| text.contains(&wanted))
        .or_else(|| {
            texts
                .iter()
                .enumerate()
                .filter(|(_, text)| text.chars().count() >= 4 && wanted.starts_with(text.as_str()))
                .max_by_key(|(_, text)| text.len())
                .map(|(at, _)| at)
        })
        .or_else(|| {
            let mut best: Option<(usize, f64)> = None;
            for (at, text) in texts.iter().enumerate() {
                let score = likeness(&wanted, text);
                if score >= LOOSE_MATCH && best.is_none_or(|(_, top)| score > top) {
                    best = Some((at, score));
                }
            }
            best.map(|(at, _)| at)
        })?;
    paragraphs.get(at)
}

/// Why nothing matched `after`, with the headings to retry with.
fn unmatched(paragraphs: &[Paragraph], after: &str) -> String {
    let headings: Vec<String> = paragraphs
        .iter()
        .filter(|paragraph| paragraph.heading && !paragraph.text.trim().is_empty())
        .take(LISTED_HEADINGS)
        .map(|paragraph| format!("`{}`", paragraph.text.trim()))
        .collect();
    if headings.is_empty() {
        format!(
            "no heading or paragraph matches `{after}`; the document has no headings: give `at` instead"
        )
    } else {
        format!(
            "no heading or paragraph matches `{after}`; the headings are {}",
            headings.join(", ")
        )
    }
}

/// `text` lowercased, each run of spaces and punctuation one space.
fn normalized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

/// How alike normalized `wanted` is to normalized `text`, 0 to 1: the
/// larger of the share of its words `text` has and how few edits make it
/// `text`'s opening.
fn likeness(wanted: &str, text: &str) -> f64 {
    let words: Vec<&str> = wanted.split(' ').collect();
    let present = words
        .iter()
        .filter(|word| text.split(' ').any(|candidate| candidate == **word))
        .count();
    let overlap = ratio(present, words.len());
    let wanted: Vec<char> = wanted.chars().take(MAX_COMPARED_CHARS).collect();
    let opening: Vec<char> = text.chars().take(wanted.len()).collect();
    let distance = edits(&wanted, &opening);
    let close = 1.0 - ratio(distance, wanted.len().max(1));
    overlap.max(close)
}

/// `part / whole` as a fraction.
fn ratio(part: usize, whole: usize) -> f64 {
    let part = u32::try_from(part).unwrap_or(u32::MAX);
    let whole = u32::try_from(whole.max(1)).unwrap_or(u32::MAX);
    f64::from(part) / f64::from(whole)
}

/// The Levenshtein distance between `a` and `b`.
fn edits(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i.saturating_add(1)];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous
                .get(j)
                .copied()
                .unwrap_or_default()
                .saturating_add(usize::from(ca != cb));
            let delete = previous
                .get(j.saturating_add(1))
                .copied()
                .unwrap_or_default()
                .saturating_add(1);
            let insert = current
                .get(j)
                .copied()
                .unwrap_or_default()
                .saturating_add(1);
            current.push(substitute.min(delete).min(insert));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(id: &str, text: &str, heading: bool) -> Paragraph {
        Paragraph {
            id: id.into(),
            text: text.into(),
            heading,
        }
    }

    fn found<'a>(paragraphs: &'a [Paragraph], after: &str) -> Option<&'a str> {
        named(paragraphs, after).map(|paragraph| paragraph.id.as_str())
    }

    #[test]
    fn the_heading_an_anchor_names_wins_over_contents_lines_and_mentions() {
        let body = [
            paragraph("toc", "Contents: Results, Methods", false),
            paragraph("intro", "See the results below.", false),
            paragraph("results", "Results", true),
            paragraph("methods", "Methods", true),
        ];
        assert_eq!(found(&body, "Results"), Some("results"));
        assert_eq!(
            found(&body, "results:"),
            Some("results"),
            "case and punctuation aside"
        );
    }

    #[test]
    fn a_loose_anchor_still_finds_its_heading() {
        let body = [
            paragraph("q3", "Q3 revenue by region", true),
            paragraph("outlook", "Outlook for 2027", true),
        ];
        assert_eq!(found(&body, "Q3 revenue by regoin"), Some("q3"), "a typo");
        assert_eq!(
            found(&body, "Outlook for 2027 and beyond"),
            Some("outlook"),
            "an anchor running on past the heading"
        );
        assert_eq!(found(&body, "Appendix B"), None);
    }

    #[test]
    fn an_unmatched_anchor_is_refused_with_the_headings() {
        let body = [
            paragraph("a", "Summary", true),
            paragraph("b", "Budget", true),
        ];
        assert_eq!(
            unmatched(&body, "Appendix B"),
            "no heading or paragraph matches `Appendix B`; the headings are `Summary`, `Budget`"
        );
    }
}
