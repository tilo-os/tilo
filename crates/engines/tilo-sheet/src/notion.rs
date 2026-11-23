//! Import of a Notion export: each database becomes a sheet.

use std::collections::BTreeMap;

use serde_json::Value;
use tilo_doc_codecs::safety::{ZipLimits, read_zip_parts};

pub const MAX_IMPORTED_TABLES: usize = 64;
pub const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

/// The words an import answers in: the app's own, so a person reads why an export could not be
/// imported in their language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Words {
    pub too_big: String,
    pub too_many: String,
    pub archive_invalid: String,
    pub unreadable: String,
    pub no_tables: String,
    pub imported_table: String,
    pub yes: String,
    pub no: String,
}

impl Default for Words {
    fn default() -> Self {
        Self {
            too_big: "The export is too large to import".into(),
            too_many: "The export holds more tables than one import brings".into(),
            archive_invalid: "The export archive cannot be read".into(),
            unreadable: "This file is not a Notion export".into(),
            no_tables: "No table in this export to import".into(),
            imported_table: "Imported table".into(),
            yes: "Yes".into(),
            no: "No".into(),
        }
    }
}

impl Words {
    fn too_many(&self) -> String {
        self.too_many.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedTable {
    pub name: String,
    pub csv: String,
}

pub fn tables(bytes: &[u8], words: &Words) -> Result<Vec<ImportedTable>, String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_EXPORT_BYTES {
        return Err(words.too_big.clone());
    }
    let tables = if looks_like_archive(bytes) {
        tables_from_archive(bytes, words)?
    } else {
        tables_from_listing(bytes, words)?
    };
    if tables.is_empty() {
        return Err(words.no_tables.clone());
    }
    if tables.len() > MAX_IMPORTED_TABLES {
        return Err(words.too_many());
    }
    Ok(tables)
}

fn looks_like_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

fn tables_from_archive(bytes: &[u8], words: &Words) -> Result<Vec<ImportedTable>, String> {
    let mut parts =
        read_zip_parts(bytes, &archive_limits()).map_err(|_| words.archive_invalid.clone())?;
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
            name: table_name(&name, words),
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

fn table_name(name: &str, words: &Words) -> String {
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
        words.imported_table.clone()
    } else {
        cleaned.to_owned()
    }
}

fn tables_from_listing(bytes: &[u8], words: &Words) -> Result<Vec<ImportedTable>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| words.unreadable.clone())?;
    let blocks: Vec<&Value> = match &value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => value
            .get("results")
            .and_then(Value::as_array)
            .map_or_else(|| vec![&value], |results| results.iter().collect()),
        _ => return Err(words.unreadable.clone()),
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
                if let Some(table) = database_of(block, words) {
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
        name: table_name_of(block, words).unwrap_or_else(|| words.imported_table.clone()),
        csv,
    })
}

fn database_of(block: &Value, words: &Words) -> Option<ImportedTable> {
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
                    csv_field_of(&property_text(property, words))
                })
            })
            .collect();
        csv.push_str(&fields.join(","));
        csv.push('\n');
    }
    Some(ImportedTable {
        name: table_name_of(block, words).unwrap_or_else(|| words.imported_table.clone()),
        csv,
    })
}

fn table_name_of(block: &Value, words: &Words) -> Option<String> {
    let _ = words;
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

fn property_text(property: &Value, words: &Words) -> String {
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
                    words.yes.clone()
                } else {
                    words.no.clone()
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
mod tests {
    use super::*;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut package = tilo_doc_codecs::package::ZipPackage { parts: Vec::new() };
        for (name, bytes) in files {
            package.set(name, bytes.to_vec());
        }
        package.write_opc().unwrap()
    }

    #[test]
    fn an_export_archive_gives_one_table_per_csv() {
        let bytes = archive(&[
            ("Tasks 1a2b3c4d5e_all.csv", b"Name,Done\nShip,yes\n"),
            ("Notes ef60718293.md", b"# Notes\n"),
        ]);
        let tables = tables(&bytes, &Words::default()).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "Tasks");
        assert_eq!(tables[0].csv, "Name,Done\nShip,yes\n");
    }

    #[test]
    fn a_listing_reads_its_tables_and_their_properties() {
        let listing = serde_json::json!([
            {
                "type": "child_database",
                "child_database": {"title": "Tasks"},
                "results": [
                    {
                        "properties": {
                            "Name": {"type": "title", "title": [{"plain_text": "Ship"}]},
                            "Done": {"type": "checkbox", "checkbox": true}
                        }
                    }
                ]
            }
        ]);
        let bytes = serde_json::to_vec(&listing).unwrap();
        let tables = tables(&bytes, &Words::default()).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "Tasks");
        assert_eq!(tables[0].csv, "Name,Done\nShip,Yes\n");
    }

    #[test]
    fn an_export_with_no_table_is_refused() {
        let bytes = archive(&[("Page 123.md", b"# Page\n")]);
        assert_eq!(
            tables(&bytes, &Words::default()).unwrap_err(),
            "No table in this export to import"
        );
    }
}
