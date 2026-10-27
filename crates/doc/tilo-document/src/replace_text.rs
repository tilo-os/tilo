//! `replace-text`: find a phrase in the body and replace it where it is,
//! the first occurrence or every one. Resolved against the document as it
//! applies, so a caller names text it read rather than block ids and
//! offsets; other text, marks, images and blocks stay as they are.

use automerge::AutoCommit;
use tilo_doc::DocMut;
use tilo_doc::model::Containers;

use crate::action::{MAX_ACTION_TEXT_CHARS, Written};

/// One occurrence: its block and its characters `start..end`.
struct Found {
    block: String,
    start: usize,
    end: usize,
}

/// Where `find` occurs in the text of the body's text blocks (tables and
/// images left out), in document order: the first only unless `all`.
fn occurrences(doc: &AutoCommit, c: &Containers, find: &str, all: bool) -> Vec<Found> {
    let mut found = Vec::new();
    if find.is_empty() {
        return found;
    }
    let width = find.chars().count();
    let order = tilo_doc::model::blocks_in_order(doc, c);
    for view in tilo_doc::model::materialize_blocks(doc, c, &order) {
        if !view.children.is_empty() || view.kind == "image" || view.kind == "table" {
            continue;
        }
        for (byte, _) in view.text.match_indices(find) {
            let start = view.text.get(..byte).map_or(0, |head| head.chars().count());
            found.push(Found {
                block: view.id.clone(),
                start,
                end: start.saturating_add(width),
            });
            if !all {
                return found;
            }
        }
    }
    found
}

/// `text` quoted for a refusal, shortened when long.
fn quoted(text: &str) -> String {
    let mut short: String = text.chars().take(80).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    format!("`{short}`")
}

/// Why replacing `find` by `replace` does not apply to the document as it
/// is now, in words its caller can act on.
pub(crate) fn check(
    doc: &AutoCommit,
    c: &Containers,
    find: &str,
    replace: &str,
) -> Result<(), String> {
    if find.is_empty() {
        return Err("`find` is empty: name the text to replace".to_owned());
    }
    if find.contains('\n') || replace.contains('\n') {
        return Err(
            "`find` and `replace` are text within one paragraph; replace-body rewrites several"
                .to_owned(),
        );
    }
    if find.chars().count() > MAX_ACTION_TEXT_CHARS
        || replace.chars().count() > MAX_ACTION_TEXT_CHARS
    {
        return Err(format!(
            "text is longer than {MAX_ACTION_TEXT_CHARS} characters"
        ));
    }
    if occurrences(doc, c, find, false).is_empty() {
        return Err(format!(
            "the text {} is not in the document: read it and use its exact text",
            quoted(find)
        ));
    }
    Ok(())
}

/// Replace `find` by `replace` where it occurs (every occurrence with
/// `all`), and what was typed in.
pub(crate) fn apply(dm: &mut DocMut<'_>, find: &str, replace: &str, all: bool) -> Vec<Written> {
    let mut found = occurrences(dm.doc, dm.c, find, all);
    // Back to front, so a block's earlier offsets still hold.
    found.reverse();
    let mut written = Vec::new();
    for at in &found {
        let del = isize::try_from(at.end.saturating_sub(at.start)).unwrap_or(isize::MAX);
        dm.splice_text(&at.block, at.start, del, replace);
        if !replace.is_empty() {
            written.push(Written::body(at.block.clone(), at.start, replace));
        }
    }
    written
}
