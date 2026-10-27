//! A Notion export read into sheets.
//!
//! Notion exports a database as one CSV file per view beside the pages, and
//! hands its API's rows over as blocks. Both are read here into the tables
//! the workbook's import takes, so a Notion import lands as every other
//! spreadsheet does: one `import-sheets`, one undo, and no collaborator
//! seeing half of it.
//!
//! A block listing carries its table as a `table` block; its rows follow as
//! `table_row` children, and a `child_database` names the table it stands
//! for.

use std::collections::BTreeMap;

use bounded_zip::BoundedArchive;
use serde_json::Value;

/// The most pages, databases or views one export may bring in.
pub const MAX_IMPORTED_TABLES: usize = 64;
/// The largest export read, before anything is parsed.
pub const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

/// A table a Notion export holds: its name, and its CSV text as the export
/// wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTable {
    pub name: String,
    pub csv: String,
}

/// Every table `bytes` holds, named as the export named them.
///
/// `bytes` is a Notion export archive (its `.zip`) or a page listing from the
/// API (`results` blocks). Nothing is read from the network: a listing is
/// what the caller already fetched.
pub fn tables(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_EXPORT_BYTES {
        return Err(ui::i18n!("sheets-notion-too-big"));
    }
    let tables = if looks_like_archive(bytes) {
        tables_from_archive(bytes)?
    } else {
        tables_from_listing(bytes)?
    };
    if tables.is_empty() {
        return Err(ui::i18n!("sheets-notion-no-tables"));
    }
    if tables.len() > MAX_IMPORTED_TABLES {
        return Err(ui::i18n::I18nManager::format(
            "sheets-notion-too-many-tables",
            &[(
                "limit",
                ui::i18n::ArgValue::from(MAX_IMPORTED_TABLES.to_string()),
            )],
        ));
    }
    Ok(tables)
}

/// Whether `bytes` starts with a zip archive's signature.
fn looks_like_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// The CSV files of an export archive, in name order.
fn tables_from_archive(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    let mut archive = BoundedArchive::from_bytes(bytes, archive_limits())
        .map_err(|_| ui::i18n!("sheets-notion-archive-invalid"))?;
    let names: Vec<String> = archive
        .file_names()
        .map(str::to_owned)
        .filter(|name| has_extension(name, "csv"))
        .collect();
    let mut tables = Vec::new();
    for name in names {
        let cap = MAX_EXPORT_BYTES;
        let Ok(text) = archive.read_text(&name, cap) else {
            continue;
        };
        tables.push(ImportedTable {
            name: table_name(&name),
            csv: text,
        });
    }
    Ok(tables)
}

fn archive_limits() -> bounded_zip::ZipLimits {
    bounded_zip::ZipLimits {
        max_parts: MAX_IMPORTED_TABLES.saturating_mul(8),
        max_part_bytes: MAX_EXPORT_BYTES,
        max_total_bytes: MAX_EXPORT_BYTES,
    }
}

/// A JSON scalar as its cell text.
fn scalar_text(value: &Value) -> String {
    value.as_str().map_or_else(
        || match value {
            Value::Null => String::new(),
            other => other.to_string(),
        },
        str::to_owned,
    )
}

fn has_extension(name: &str, extension: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|held| held.eq_ignore_ascii_case(extension))
}

/// A CSV file's name as the table's: its file name, and, for a view Notion
/// marked as such, what is left once the marker is gone.
fn table_name(name: &str) -> String {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::trim)
        .unwrap_or_default();
    let cleaned = stem
        .trim_end_matches("_all")
        .rsplit_once(' ')
        .filter(|(_, hash)| {
            hash.len() >= 8 && hash.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map_or(stem, |(title, _)| title.trim());
    if cleaned.is_empty() {
        ui::i18n!("sheets-notion-imported-table")
    } else {
        cleaned.to_owned()
    }
}

/// The tables a page listing holds: each `table` block with its rows, and
/// each `child_database` with the rows a sibling hands it.
fn tables_from_listing(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| ui::i18n!("sheets-notion-unreadable"))?;
    let blocks: Vec<&Value> = match &value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => value
            .get("results")
            .and_then(Value::as_array)
            .map_or_else(|| vec![&value], |results| results.iter().collect()),
        _ => return Err(ui::i18n!("sheets-notion-unreadable")),
    };
    let mut tables = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("table") => {
                if let Some(table) = table_of(block) {
                    tables.push(table);
                }
            }
            Some("child_database") => {
                if let Some(table) = database_of(block) {
                    tables.push(table);
                }
            }
            _ => {}
        }
    }
    Ok(tables)
}

/// A `table` block, its `table_row` children becoming the rows.
fn table_of(block: &Value) -> Option<ImportedTable> {
    let rows: Vec<&Value> = block.get("children")?.as_array()?.iter().collect();
    let mut csv = String::new();
    for row in rows {
        let cells = row.get("table_row")?.get("cells")?.as_array()?;
        let fields: Vec<String> = cells.iter().map(|cell| csv_field(cell)).collect();
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    (!csv.is_empty()).then(|| ImportedTable {
        name: table_name_of(block).unwrap_or_else(|| ui::i18n!("sheets-notion-imported-table")),
        csv,
    })
}

/// A `child_database` block: its title, and the rows the listing gives it
/// under `rows` (what a fetch of the database returns).
fn database_of(block: &Value) -> Option<ImportedTable> {
    let body = block.get("child_database")?;
    let rows = body
        .get("rows")
        .or_else(|| block.get("children"))
        .and_then(Value::as_array)?;
    if rows.is_empty() {
        return None;
    }
    // The database's rows are records keyed by their properties, so the
    // columns are every property any row fills, in first-seen order.
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        for property in properties_of(row).keys() {
            if !columns.contains(property) {
                columns.push(property.clone());
            }
        }
    }
    if columns.is_empty() {
        return None;
    }
    let mut csv = String::new();
    csv.push_str(
        &columns
            .iter()
            .map(|each| csv_field_of(each))
            .collect::<Vec<_>>()
            .join(","),
    );
    csv.push('\n');
    for row in rows {
        let properties = properties_of(row);
        let fields: Vec<String> = columns
            .iter()
            .map(|column| {
                properties
                    .get(column)
                    .map_or_else(String::new, |value| csv_field(value))
            })
            .collect();
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    Some(ImportedTable {
        name: table_name_of(block).unwrap_or_else(|| ui::i18n!("sheets-notion-imported-table")),
        csv,
    })
}

/// The title a block or database carries, if the listing names it.
fn table_name_of(block: &Value) -> Option<String> {
    let body = block
        .get("child_database")
        .or_else(|| block.get("table"))
        .or_else(|| block.get("properties"));
    let title = body
        .and_then(|body| body.get("title"))
        .map_or_else(|| block.get("title"), Some)?;
    let text = title
        .as_str()
        .map(str::to_owned)
        .or_else(|| rich_text(title))?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A row's properties, keyed by name.
fn properties_of(row: &Value) -> BTreeMap<String, Value> {
    row.get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, property)| (name.clone(), property.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn rich_text(value: &Value) -> Option<String> {
    let entries = value.get("rich_text")?.as_array()?;
    let mut out = String::new();
    for entry in entries {
        out.push_str(
            entry
                .get("plain_text")
                .or_else(|| entry.get("text").and_then(|text| text.get("content")))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
    Some(out)
}

/// A property as the cell's text: rich text, a title, a number, a date, a
/// selection — what the person reading the sheet would see.
fn property_text(property: &Value) -> String {
    let kind = property
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = property.get(kind);
    match kind {
        "title" | "rich_text" => body.and_then(rich_text).unwrap_or_default(),
        // A number goes on as the export wrote it: the cell reads it as a
        // number on entry, and shows it in the person's own format.
        "number" => body.map(scalar_text).unwrap_or_default(),
        "checkbox" => body
            .and_then(Value::as_bool)
            .map(|checked| {
                if checked {
                    ui::i18n!("sheets-notion-yes")
                } else {
                    ui::i18n!("sheets-notion-no")
                }
            })
            .unwrap_or_default(),
        "date" => body
            .and_then(|date| date.get("start"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        "select" | "status" => body
            .and_then(|select| select.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        "multi_select" => body
            .and_then(Value::as_array)
            .map(|selected| {
                selected
                    .iter()
                    .filter_map(|each| each.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        "people" => body
            .and_then(Value::as_array)
            .map(|people| {
                people
                    .iter()
                    .filter_map(|each| each.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        "url" | "email" | "phone_number" => {
            body.and_then(Value::as_str).unwrap_or_default().to_owned()
        }
        "formula" => {
            let formula = body.unwrap_or(&Value::Null);
            ["string", "number", "boolean", "date"]
                .iter()
                .find_map(|kind| formula.get(*kind))
                .map_or_else(String::new, |value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| value.as_f64().map(|_| scalar_text(value)))
                        .unwrap_or_default()
                })
        }
        // A relation, a rollup, a file: the listing gives a list, which the
        // cell shows as its names.
        _ => body
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|each| {
                        each.get("name")
                            .or_else(|| each.get("title"))
                            .and_then(Value::as_str)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
    }
}

/// One rich-text cell as a CSV field.
fn csv_field(cell: &Value) -> String {
    csv_field_of(&rich_text(cell).unwrap_or_default())
}

/// `text` as a CSV field: quoted when it holds a comma, a quote or a line
/// break, its own quotes doubled.
fn csv_field_of(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
#[path = "notion_tests.rs"]
mod tests;
