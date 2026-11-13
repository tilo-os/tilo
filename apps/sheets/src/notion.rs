//! Import of a Notion export: each database becomes a sheet.

use crate::resource::CATALOG;
use std::collections::BTreeMap;

use serde_json::Value;
use tilo_doc_codecs::safety::{ZipLimits, read_zip_parts};

pub const MAX_IMPORTED_TABLES: usize = 64;
pub const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTable {
    pub name: String,
    pub csv: String,
}

pub fn tables(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_EXPORT_BYTES {
        return Err(CATALOG.t("sheets-notion-too-big"));
    }
    let tables = if looks_like_archive(bytes) {
        tables_from_archive(bytes)?
    } else {
        tables_from_listing(bytes)?
    };
    if tables.is_empty() {
        return Err(CATALOG.t("sheets-notion-no-tables"));
    }
    if tables.len() > MAX_IMPORTED_TABLES {
        return Err(crate::resource::CATALOG.t_args(
            "sheets-notion-too-many-tables",
            &[(
                "limit",
                ui::i18n::ArgValue::from(MAX_IMPORTED_TABLES.to_string()),
            )],
        ));
    }
    Ok(tables)
}

fn looks_like_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

fn tables_from_archive(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    let mut parts = read_zip_parts(bytes, &archive_limits())
        .map_err(|_| CATALOG.t("sheets-notion-archive-invalid"))?;
    parts.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut tables = Vec::new();
    for (name, part) in parts {
        if !has_extension(&name, "csv") {
            continue;
        }
        let Ok(text) = String::from_utf8(part) else {
            continue;
        };
        tables.push(ImportedTable {
            name: table_name(&name),
            csv: text,
        });
    }
    Ok(tables)
}

fn archive_limits() -> ZipLimits {
    ZipLimits {
        max_parts: MAX_IMPORTED_TABLES.saturating_mul(8),
        max_part_bytes: MAX_EXPORT_BYTES,
        max_total_bytes: MAX_EXPORT_BYTES,
    }
}

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
        CATALOG.t("sheets-notion-imported-table")
    } else {
        cleaned.to_owned()
    }
}

fn tables_from_listing(bytes: &[u8]) -> Result<Vec<ImportedTable>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| CATALOG.t("sheets-notion-unreadable"))?;
    let blocks: Vec<&Value> = match &value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => value
            .get("results")
            .and_then(Value::as_array)
            .map_or_else(|| vec![&value], |results| results.iter().collect()),
        _ => return Err(CATALOG.t("sheets-notion-unreadable")),
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

fn table_of(block: &Value) -> Option<ImportedTable> {
    let rows: Vec<&Value> = block.get("children")?.as_array()?.iter().collect();
    let mut csv = String::new();
    for row in rows {
        let cells = row.get("table_row")?.get("cells")?.as_array()?;
        let fields: Vec<String> = cells
            .iter()
            .map(|cell| csv_field_of(&rich_text(cell).unwrap_or_default()))
            .collect();
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    (!csv.is_empty()).then(|| ImportedTable {
        name: table_name_of(block).unwrap_or_else(|| CATALOG.t("sheets-notion-imported-table")),
        csv,
    })
}

fn database_of(block: &Value) -> Option<ImportedTable> {
    let body = block.get("child_database")?;
    let rows = body
        .get("rows")
        .or_else(|| block.get("children"))
        .and_then(Value::as_array)?;
    if rows.is_empty() {
        return None;
    }
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        for property in property_names(row) {
            if !columns.contains(&property) {
                columns.push(property);
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
                properties.get(column).map_or_else(String::new, |property| {
                    csv_field_of(&property_text(property))
                })
            })
            .collect();
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    Some(ImportedTable {
        name: table_name_of(block).unwrap_or_else(|| CATALOG.t("sheets-notion-imported-table")),
        csv,
    })
}

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

fn property_names(row: &Value) -> Vec<String> {
    row.get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

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
    let entries = value
        .as_array()
        .or_else(|| value.get("rich_text")?.as_array())?;
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
                    CATALOG.t("sheets-notion-yes")
                } else {
                    CATALOG.t("sheets-notion-no")
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
