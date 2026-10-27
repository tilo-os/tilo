//! A Notion export as notes.
//!
//! Notion leaves in two shapes, and both are read here into the one thing
//! this app knows how to add: Markdown with the pictures a note shows.
//!
//! - **An export archive.** "Export → Markdown & CSV" is a zip holding one
//!   `.md` per page, named after the page's title, with its pictures beside
//!   it and a `.csv` per database. A database's CSV becomes a Markdown table
//!   appended to the page it was exported from. A picture the export kept is
//!   read out of the archive and given to the notebook, so the note shows it
//!   without the export staying on disk.
//! - **An API page listing.** The JSON a page's `children` endpoint answers
//!   is turned into the same Markdown: headings, paragraphs, lists,
//!   checklists, quotes, callouts, code, dividers and tables. A picture is
//!   kept as a link to its URL, since the listing carries no bytes.
//!
//! Every page becomes a top-level note of its own; the caller adds them all
//! as one action group, so a single undo takes the whole import back and a
//! collaborator sees either all of it or none. A page nested in another is
//! still a note of its own, and the link the export writes where that page
//! used to sit is left exactly as the export wrote it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;
use ui::tilo_resource::Invoke;

use crate::notebook::Notebook;

/// The most pages one import brings.
pub const MAX_IMPORTED_PAGES: usize = 256;

/// The largest export archive one import reads.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// One page of an import: its title, its body as Markdown and the pictures
/// its body shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedPage {
    pub title: String,
    pub markdown: String,
    pub images: Vec<Vec<u8>>,
}

/// The pages `bytes` holds: the zip of a Notion export, or the JSON of an
/// API page listing.
///
/// # Errors
/// Returns why the export could not be read: it is neither shape, it holds
/// no page or more than [`MAX_IMPORTED_PAGES`], or a page is too large for
/// one note.
pub fn pages(bytes: &[u8]) -> Result<Vec<ImportedPage>, String> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ui::i18n!("notes-notion-too-big"));
    }
    let pages = if looks_like_archive(bytes) {
        pages_from_archive(bytes)?
    } else {
        pages_from_api_json(bytes)?
    };
    if pages.is_empty() {
        return Err(ui::i18n!("notes-notion-empty"));
    }
    if pages.len() > MAX_IMPORTED_PAGES {
        return Err(ui::i18n::I18nManager::format(
            "notes-notion-too-many-pages",
            &[(
                "limit",
                ui::i18n::ArgValue::from(MAX_IMPORTED_PAGES.to_string()),
            )],
        ));
    }
    for page in &pages {
        crate::notebook::check_markdown_import(&page.markdown)?;
    }
    Ok(pages)
}

/// Every page of `pages`, added to the notebook as one action: one undo
/// takes the whole import back, and a collaborator sees all of it or none.
///
/// Each note's id is minted by the group from its seed and the member's
/// position, so every replica adds the same notes under the same ids.
#[must_use]
pub fn import_action(pages: &[ImportedPage]) -> Invoke<Notebook> {
    Invoke::group(pages.iter().filter_map(import_of))
}

/// The import of `page` as one invocation; `None` only if the action cannot
/// be encoded, which plain owned data never causes.
fn import_of(page: &ImportedPage) -> Option<Invoke<Notebook>> {
    Invoke::apply(&crate::actions::import_markdown_action(
        None,
        &page.title,
        &page.markdown,
        page.images.clone(),
    ))
    .ok()
}

/// Whether `bytes` starts with a zip archive's signature: a Notion export is
/// one, and an API listing is text.
fn looks_like_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")
}

/// The pages a Notion zip export holds, in name order so the same archive
/// always becomes the same notes.
fn pages_from_archive(bytes: &[u8]) -> Result<Vec<ImportedPage>, String> {
    let limits = bounded_zip::ZipLimits::default();
    let mut archive = bounded_zip::BoundedArchive::from_bytes(bytes, limits)
        .map_err(|_| ui::i18n!("notes-notion-archive-invalid"))?;
    archive
        .check_declared_sizes()
        .map_err(|_| ui::i18n!("notes-notion-archive-invalid"))?;
    let mut names: Vec<String> = archive
        .file_names()
        .filter(|name| !name.starts_with("__MACOSX/"))
        .map(std::borrow::Cow::into_owned)
        .collect();
    names.sort();
    // Every picture the export kept, by the path a page names it by.
    let mut pictures: HashMap<String, Vec<u8>> = HashMap::new();
    for name in &names {
        if !is_picture(name) {
            continue;
        }
        let cap = u64::try_from(crate::notebook::MAX_ASSET_BYTES).unwrap_or(u64::MAX);
        if let Ok(bytes) = archive.read(name, cap) {
            pictures.insert(name.clone(), bytes);
        }
    }
    // A database exported beside its page is a CSV named after it, a space
    // apart when one page holds two.
    let mut pages = Vec::new();
    for name in &names {
        if !name.to_lowercase().ends_with(".md") {
            continue;
        }
        let cap = u64::try_from(crate::notebook::MAX_MARKDOWN_IMPORT_BYTES).unwrap_or(u64::MAX);
        let Ok(text) = archive.read_text(name, cap) else {
            continue;
        };
        let title = page_title(name, &text);
        let mut import = Pictures::default();
        let body = keep_pictures(&text, name, &pictures, &mut import);
        let mut markdown = body;
        for database in databases_of(name, &names) {
            let cap = u64::try_from(crate::notebook::MAX_MARKDOWN_IMPORT_BYTES).unwrap_or(u64::MAX);
            let Ok(csv) = archive.read_text(&database, cap) else {
                continue;
            };
            match csv_table(&csv) {
                Some(table) if !table.trim().is_empty() => {
                    if !markdown.is_empty() && !markdown.ends_with('\n') {
                        markdown.push('\n');
                    }
                    markdown.push('\n');
                    markdown.push_str(&table);
                    markdown.push('\n');
                }
                _ => {}
            }
        }
        pages.push(ImportedPage {
            title,
            markdown,
            images: import.bytes,
        });
    }
    Ok(pages)
}

/// The pictures a page shows, and the ones already given to the notebook, so
/// one picture shown twice is kept once.
#[derive(Default)]
struct Pictures {
    bytes: Vec<Vec<u8>>,
    kept: HashMap<String, String>,
}

/// The title of the page stored at `name`: its file name, and, for a page an
/// export named "Untitled", the `# Heading` Notion writes as its first line.
fn page_title(name: &str, text: &str) -> String {
    let from_file = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty() && *stem != "Untitled");
    let from_body = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|heading| !heading.is_empty());
    from_file
        .or(from_body)
        .map_or_else(|| ui::i18n!("notes-imported-untitled"), str::to_owned)
}

/// The Markdown of the page stored at `name`, with each picture it shows
/// given to the notebook and shown from there. A picture the export does not
/// hold (a link to a remote one) stays the link it was.
fn keep_pictures(
    text: &str,
    name: &str,
    pictures: &HashMap<String, Vec<u8>>,
    kept: &mut Pictures,
) -> String {
    let mut output = String::with_capacity(text.len());
    let directory = Path::new(name).parent().map(Path::to_path_buf);
    for segment in text.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let Some((indent, alt, destination)) = image_parts(line) else {
            output.push_str(segment);
            continue;
        };
        let Some(bytes) = resolve_picture(directory.as_deref(), destination, pictures) else {
            output.push_str(segment);
            continue;
        };
        if crate::app::check_image(bytes).is_err() {
            output.push_str(segment);
            continue;
        }
        let digest = blake3::hash(bytes).to_hex().to_string();
        let source = format!("asset:{digest}");
        if let std::collections::hash_map::Entry::Vacant(entry) = kept.kept.entry(source.clone()) {
            kept.bytes.push(bytes.clone());
            entry.insert(source.clone());
        }
        let _ = write!(output, "{indent}![{alt}]({source}){newline}");
    }
    output
}

/// The picture `destination` names, as the archive holds it: beside the page,
/// or by the path the export wrote.
fn resolve_picture<'a>(
    directory: Option<&Path>,
    destination: &str,
    pictures: &'a HashMap<String, Vec<u8>>,
) -> Option<&'a Vec<u8>> {
    if destination.contains("://") || destination.starts_with("data:") {
        return None;
    }
    let destination = destination.trim_start_matches("./");
    if let Some(bytes) = pictures.get(destination) {
        return Some(bytes);
    }
    let joined = directory?.join(destination);
    pictures.get(joined.to_str()?)
}

/// `line` as an indented `![alt](destination)` picture, when it is one.
fn image_parts(line: &str) -> Option<(&str, &str, &str)> {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix("![")?;
    let (alt, destination) = body.split_once("](")?;
    let destination = destination.strip_suffix(')')?.trim();
    (!destination.is_empty()).then_some((
        line.get(..line.len().saturating_sub(line.trim_start().len()))
            .unwrap_or(""),
        alt,
        destination,
    ))
}

/// The CSVs the export gave the page at `name`: named after it, beside it.
fn databases_of(name: &str, names: &[String]) -> Vec<String> {
    let Some(stem) = Path::new(name).file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    let directory = Path::new(name).parent();
    let mut databases: Vec<String> = names
        .iter()
        .filter(|candidate| {
            let path = Path::new(candidate.as_str());
            if !candidate.to_lowercase().ends_with(".csv") || path.parent() != directory {
                return false;
            }
            let Some(candidate_stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                return false;
            };
            candidate_stem == stem
                || candidate_stem
                    .strip_prefix(stem)
                    .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('_'))
        })
        .cloned()
        .collect();
    databases.sort();
    databases
}

/// `csv` as a Markdown table: its first row the header, one line per record.
/// A quoted field keeps its embedded newlines and commas; `None` when the
/// file holds no field at all.
#[must_use]
pub fn csv_table(csv: &str) -> Option<String> {
    let records = csv_records(csv);
    let mut records = records.into_iter();
    let header = records.next()?;
    if header.is_empty() {
        return None;
    }
    let width = header.len();
    let mut table = String::new();
    table.push('|');
    for field in &header {
        let _ = write!(table, " {} |", escape_cell(field));
    }
    table.push('\n');
    table.push('|');
    for _ in 0..width {
        table.push_str(" --- |");
    }
    table.push('\n');
    for record in records {
        if record.iter().all(String::is_empty) {
            continue;
        }
        table.push('|');
        for at in 0..width {
            let cell = record.get(at).map_or("", String::as_str);
            let _ = write!(table, " {} |", escape_cell(cell));
        }
        table.push('\n');
    }
    Some(table)
}

/// A Markdown table cell: a pipe or a line break would end it.
fn escape_cell(field: &str) -> String {
    field
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_owned()
}

/// The records of `csv`, each a row of fields: comma apart, quotes kept as
/// RFC 4180 keeps them (`""` inside a quoted field is one quote, a quoted
/// field may hold commas and line breaks).
fn csv_records(csv: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut ended_with_separator = false;
    let mut chars = csv.chars().peekable();
    while let Some(ch) = chars.next() {
        ended_with_separator = false;
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    let _ = chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.trim().is_empty() => {
                field.clear();
                quoted = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                ended_with_separator = true;
            }
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            other => field.push(other),
        }
    }
    if !field.is_empty() || !record.is_empty() || ended_with_separator {
        record.push(field);
        records.push(record);
    }
    records
}

fn is_picture(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    )
}

/// The pages a Notion API `children` listing describes, as Markdown.
fn pages_from_api_json(bytes: &[u8]) -> Result<Vec<ImportedPage>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| ui::i18n!("notes-notion-unreadable"))?;
    // A listing is one page's blocks; an array of listings is several pages.
    let listings: Vec<&Value> = match &value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => vec![&value],
        _ => return Err(ui::i18n!("notes-notion-unreadable")),
    };
    let mut pages = Vec::new();
    for listing in listings {
        let blocks = listing
            .get("results")
            .and_then(Value::as_array)
            .map_or_else(|| std::slice::from_ref(listing), Vec::as_slice);
        if blocks.is_empty() {
            continue;
        }
        let title = api_title(listing, blocks);
        let mut markdown = String::new();
        for block in blocks {
            write_block(&mut markdown, block);
        }
        pages.push(ImportedPage {
            title,
            markdown,
            images: Vec::new(),
        });
    }
    Ok(pages)
}

/// The title a listing gives its page: the page's `title` property, else the
/// title of its first heading, else the neutral name an import uses.
fn api_title(listing: &Value, blocks: &[Value]) -> String {
    let from_property = listing.get("title").and_then(rich_text).or_else(|| {
        listing.get("properties").and_then(|properties| {
            properties
                .as_object()?
                .values()
                .find(|property| property.get("type").and_then(Value::as_str) == Some("title"))
                .and_then(|property| property.get("title"))
                .and_then(rich_text)
        })
    });
    let from_heading = blocks
        .iter()
        .find(|block| block_kind(block) == Some("heading_1"))
        .and_then(|block| block.get("heading_1"))
        .and_then(rich_text);
    from_property
        .or(from_heading)
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| ui::i18n!("notes-imported-untitled"))
}

/// Write one API block as Markdown; its children follow it, indented for a
/// list item and plain for anything else.
fn write_block(out: &mut String, block: &Value) {
    let kind = block_kind(block);
    let text = block.get(kind.unwrap_or_default()).map_or_else(
        || plain_text(block.get("paragraph")),
        |body| plain_text(Some(body)),
    );
    write_block_body(out, block, kind, &text);
    if kind != Some("child_page")
        && let Some(children) = block.get("children").and_then(Value::as_array)
    {
        let indented = matches!(
            kind,
            Some("bulleted_list_item" | "numbered_list_item" | "to_do")
        );
        for child in children {
            let mut line = String::new();
            write_block(&mut line, child);
            for child_line in line.lines().filter(|line| !line.is_empty()) {
                if indented {
                    let _ = writeln!(out, "    {child_line}");
                } else {
                    let _ = writeln!(out, "{child_line}");
                }
            }
        }
        if indented {
            out.push('\n');
        }
    }
}

/// The words of one block, in the shape of its kind.
fn write_block_body(out: &mut String, block: &Value, kind: Option<&str>, text: &str) {
    match kind {
        Some("heading_1") => {
            let _ = writeln!(out, "# {text}");
            out.push('\n');
        }
        Some("heading_2") => {
            let _ = writeln!(out, "## {text}");
            out.push('\n');
        }
        Some("heading_3") => {
            let _ = writeln!(out, "### {text}");
            out.push('\n');
        }
        Some("bulleted_list_item") => {
            let _ = writeln!(out, "- {}", one_line(&text));
        }
        Some("numbered_list_item") => {
            let _ = writeln!(out, "1. {}", one_line(&text));
        }
        Some("to_do") => {
            let checked = block
                .get("to_do")
                .and_then(|body| body.get("checked"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let _ = writeln!(
                out,
                "- [{}] {}",
                if checked { 'x' } else { ' ' },
                one_line(&text)
            );
        }
        Some("quote") => {
            let _ = writeln!(out, "> {}", one_line(&text));
            out.push('\n');
        }
        Some("code") => {
            let language = block
                .get("code")
                .and_then(|body| body.get("language"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let fence = if language.is_empty() {
                "```".to_owned()
            } else {
                format!("```{language}")
            };
            let _ = writeln!(out, "{fence}\n{text}\n```");
            out.push('\n');
        }
        Some("divider") => out.push_str("---\n\n"),
        Some("callout") => {
            // Notes has no callout block; a quote carries the same words.
            let _ = writeln!(out, "> {}", one_line(&text));
            out.push('\n');
        }
        Some("child_page") => {
            let title = block
                .get("child_page")
                .and_then(|body| body.get("title"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let _ = writeln!(out, "[[{}]]", one_line(title));
            out.push('\n');
        }
        Some("paragraph") | None => {
            if !text.trim().is_empty() {
                let _ = writeln!(out, "{text}");
            }
            out.push('\n');
        }
        // A block kind Notes does not keep (`synced_block`, `template`,
        // `table_of_contents`) still carries words: they are not dropped.
        Some(_) => {
            if !text.trim().is_empty() {
                let _ = writeln!(out, "{text}");
                out.push('\n');
            }
        }
    }
}

/// A block's kind: its `type` (`heading_1`, `paragraph`), else the key of
/// its body object.
fn block_kind(block: &Value) -> Option<&str> {
    let object = block.as_object()?;
    object.get("type").and_then(Value::as_str).or_else(|| {
        object
            .iter()
            .find(|(key, value)| {
                *key != "object"
                    && *key != "id"
                    && *key != "created_time"
                    && *key != "last_edited_time"
                    && *key != "has_children"
                    && *key != "archived"
                    && value.is_object()
            })
            .map(|(key, _)| key.as_str())
    })
}

/// The words of a rich-text array, marks and all: bold, italic, code, strike
/// and links become the Markdown that spells them.
fn rich_text(body: &Value) -> Option<String> {
    let entries = body.get("rich_text")?.as_array()?;
    Some(rich_text_of(entries))
}

fn plain_text(body: Option<&Value>) -> String {
    body.and_then(rich_text)
        .or_else(|| body.and_then(|body| body.get("title")).and_then(rich_text))
        .unwrap_or_default()
}

/// A rich-text array as Markdown, each entry spelled with the marks it
/// carries.
fn rich_text_of(entries: &[Value]) -> String {
    let mut out = String::new();
    for entry in entries {
        let text = entry
            .get("plain_text")
            .or_else(|| entry.get("text").and_then(|text| text.get("content")))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let annotations = entry.get("annotations");
        let on = |name: &str| {
            annotations
                .and_then(|annotations| annotations.get(name))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        let href = entry
            .get("href")
            .and_then(Value::as_str)
            .filter(|href| !href.is_empty());
        let mut markdown = text.to_owned();
        if on("code") {
            markdown = format!("`{markdown}`");
        }
        if on("bold") {
            markdown = format!("**{markdown}**");
        }
        if on("italic") {
            markdown = format!("*{markdown}*");
        }
        if on("strikethrough") {
            markdown = format!("~~{markdown}~~");
        }
        if on("underline") {
            markdown = format!("__{markdown}__");
        }
        if let Some(href) = href {
            markdown = format!("[{markdown}]({href})");
        }
        out.push_str(&markdown);
    }
    out
}

/// `text` on one line: a list item's words do not break its marker.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_owned()
}

#[cfg(test)]
#[path = "notion_tests.rs"]
mod tests;
