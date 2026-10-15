//! The PDF a record marks up as agents and scripts read it: its pages' text
//! in the reader's page order, a bounded run of pages at a time, with the
//! page count and what the file says about itself.
//!
//! Text is extracted with the portable engine in this process, never the
//! pdfium helper, so a read runs where an agent's confined worker runs: it
//! needs no other process, no file and no network, only the bytes the
//! room's content store holds.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use tilo_content::ContentStore;

use crate::doc::PagePlanEntry;
use crate::resource::PdfMarkup;

/// The most pages one read answers.
pub const MOST_PAGES: usize = 10;

/// The most characters of page text one read answers, so the answer fits
/// what an agent is shown of a read.
pub const MOST_CHARS: usize = 4_000;

/// Why a record with no document reads nothing.
const NO_DOCUMENT: &str = "no PDF is open here: open one with import-pdf-content";

/// Where a shown page's text comes from.
enum PageSource<'a> {
    /// A page (1-based) of the document the record marks up.
    Source(usize),
    /// A page (1-based) of a PDF imported into the record, by content hash.
    Imported(&'a str, usize),
    /// An imported picture: no text.
    Picture,
}

/// The bytes of the content `address` names (`tilo.content:<hash>`, or the
/// bare hash) in the window's content store, with their hash.
///
/// # Errors
/// Why the address names no content this window holds.
pub fn content_bytes(address: &str) -> Result<(String, Vec<u8>), String> {
    let address = address.trim();
    let hash = tilo_actions::content_hash(address)
        .unwrap_or(address)
        .to_ascii_lowercase();
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "`{address}` names no content: give `tilo.content:<hash>`"
        ));
    }
    let content = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to read the PDF from")?;
    let bytes = content
        .read(&hash)
        .ok_or_else(|| format!("content `{hash}` is not available on this machine"))?;
    Ok((hash, bytes))
}

/// Open `bytes` as a PDF with the in-process engine.
///
/// # Errors
/// Why the bytes are not a PDF this engine reads.
pub fn open(bytes: &[u8]) -> Result<tilo_pdf::Document, String> {
    tilo_pdf::Engine::portable()
        .open_bytes(bytes)
        .map_err(|error| format!("not a readable PDF: {error}"))
}

/// The text of the pages of `markup`'s document from page `page` (1-based,
/// the first when `None`) at character `offset` into it: up to `pages`
/// pages ([`MOST_PAGES`] at most) and [`MOST_CHARS`] characters, a page
/// longer than that cut where the budget ends. `next` names where the
/// following read starts, `null` at the end of the document.
///
/// # Errors
/// Why there is nothing to read: no document, its bytes not here, a page
/// it does not have.
pub fn read_text(
    markup: &PdfMarkup,
    content: Option<&ContentStore>,
    page: Option<usize>,
    pages: Option<usize>,
    offset: Option<usize>,
) -> Result<Value, String> {
    let source = markup.source.as_ref().ok_or(NO_DOCUMENT)?;
    let content = content.ok_or("this window keeps no content to read the PDF from")?;
    let bytes = content.read(&source.hash).ok_or_else(|| {
        format!(
            "the PDF's bytes (content `{}`) are not available on this machine",
            source.hash
        )
    })?;
    let document = open(&bytes)?;
    let shown = shown_pages(&markup.pages, document.page_count());
    let count = shown.len();
    let first = page.unwrap_or(1);
    if first == 0 || first > count {
        return Err(format!(
            "there is no page {first}: the document has {count} page{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    let most = pages.unwrap_or(MOST_PAGES).clamp(1, MOST_PAGES);
    let mut imported: HashMap<&str, Option<tilo_pdf::Document>> = HashMap::new();
    let mut answered = Vec::new();
    let mut budget = MOST_CHARS;
    let mut next = None;
    let mut skip = offset.unwrap_or(0);
    for (index, place) in shown.iter().enumerate().skip(first - 1).take(most) {
        let number = index + 1;
        let text = match place {
            PageSource::Source(at) => page_text(&document, *at),
            PageSource::Imported(hash, at) => imported
                .entry(hash)
                .or_insert_with(|| content.read(hash).and_then(|bytes| open(&bytes).ok()))
                .as_ref()
                .map(|other| page_text(other, *at))
                .unwrap_or_default(),
            PageSource::Picture => String::new(),
        };
        let length = text.chars().count();
        let from = skip.min(length);
        skip = 0;
        let left = length - from;
        if left > budget && !answered.is_empty() {
            next = Some((number, 0));
            break;
        }
        let taken: String = text.chars().skip(from).take(budget).collect();
        let took = taken.chars().count();
        let cut = left > budget;
        budget -= took.min(budget);
        let mut entry = Map::new();
        entry.insert("page".into(), number.into());
        if from > 0 {
            entry.insert("offset".into(), from.into());
        }
        entry.insert("text".into(), taken.into());
        answered.push(Value::Object(entry));
        if cut {
            next = Some((number, from + took));
            break;
        }
    }
    if next.is_none() {
        let after = first + answered.len();
        next = (after <= count).then_some((after, 0));
    }
    let mut answer = Map::new();
    if !source.name.is_empty() {
        answer.insert("name".into(), source.name.clone().into());
    }
    answer.insert("pageCount".into(), count.into());
    let facts = document.metadata();
    for (key, value) in [
        ("title", &facts.title),
        ("author", &facts.author),
        ("subject", &facts.subject),
    ] {
        if !value.trim().is_empty() {
            answer.insert(key.into(), value.trim().into());
        }
    }
    answer.insert("pages".into(), Value::Array(answered));
    answer.insert(
        "next".into(),
        next.map_or(
            Value::Null,
            |(page, offset)| json!({"page": page, "offset": offset}),
        ),
    );
    Ok(Value::Object(answer))
}

/// The pages the reader shows, in order: the record's page plan when it has
/// one, else every page of the document.
fn shown_pages(plan: &[PagePlanEntry], source_pages: usize) -> Vec<PageSource<'_>> {
    if plan.is_empty() {
        return (1..=source_pages).map(PageSource::Source).collect();
    }
    plan.iter()
        .map(|entry| match &entry.imported {
            None => PageSource::Source(entry.source_page),
            Some(page) if page.kind == "pdf" => PageSource::Imported(&page.hash, page.page),
            Some(_) => PageSource::Picture,
        })
        .collect()
}

/// The text of page `page` (1-based) of `document`, as copying the whole
/// page gives it; empty for a page with no text layer (a scan).
fn page_text(document: &tilo_pdf::Document, page: usize) -> String {
    let Some(index) = page.checked_sub(1) else {
        return String::new();
    };
    document
        .text(index)
        .map(|layer| {
            layer
                .text_for(&layer.select_range(0..layer.len()))
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
