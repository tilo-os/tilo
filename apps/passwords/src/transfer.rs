//! Bounded browser-CSV interchange for the Passwords portal flow.
//!
//! This module is deliberately path-free: the host portal owns file
//! selection; parsing consumes a reader and exporting produces bytes for the
//! already-selected destination.

use std::io::Read;

use accounts::SecretItem;
use zeroize::Zeroizing;

pub const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ROWS: usize = 10_000;
pub const MAX_FIELDS: usize = 32;
pub const MAX_FIELD_BYTES: usize = 16 * 1024;

pub fn import_request() -> ui::portal::PickRequest {
    ui::portal::PickRequest::open("Import passwords", "Choose CSV")
        .thread_name("passwords-portal-import")
        .filter(
            ui::portal::FileFilter::new("Password CSV files")
                .mimetype("text/csv")
                .glob("*.csv"),
        )
        .all_files()
}

pub fn export_request() -> ui::portal::PickRequest {
    ui::portal::PickRequest::save("Export passwords as CSV", "Export", "passwords.csv")
        .thread_name("passwords-portal-export")
        .filter(
            ui::portal::FileFilter::new("CSV file")
                .mimetype("text/csv")
                .glob("*.csv"),
        )
        .all_files()
}

#[derive(Debug)]
pub struct ImportLogin {
    pub title: Zeroizing<String>,
    pub username: Zeroizing<String>,
    pub origin: Zeroizing<String>,
    pub password: Zeroizing<String>,
    pub notes: Zeroizing<String>,
}

pub fn parse_browser_csv(mut reader: impl Read) -> Result<Vec<ImportLogin>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the selected file".to_string())?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err("The selected file is larger than 8 MB".into());
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "The selected CSV is not UTF-8".to_string())?;
    let records = parse_records(text)?;
    let Some(headers) = records.first() else {
        return Err("The selected CSV is empty".into());
    };
    let normalized: Vec<String> = headers
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    let column = |names: &[&str]| normalized.iter().position(|h| names.contains(&h.as_str()));
    let title = column(&["name", "title"]);
    let username = column(&["username", "login_username", "user"]);
    let password = column(&["password", "login_password"])
        .ok_or_else(|| "The CSV has no password column".to_string())?;
    let origin = column(&["url", "origin", "login_uri", "website"]);
    let notes = column(&["note", "notes", "extra"]);
    let mut out = Vec::new();
    for record in records.into_iter().skip(1) {
        if out.len() >= MAX_ROWS {
            return Err("The CSV contains more than 10,000 rows".into());
        }
        let get = |index: Option<usize>| {
            index
                .and_then(|i| record.get(i))
                .map_or("", String::as_str)
                .trim()
        };
        let password_value = get(Some(password));
        if password_value.is_empty() {
            continue;
        }
        let origin_value = get(origin);
        let title_value = get(title);
        let fallback = origin_value
            .strip_prefix("https://")
            .or_else(|| origin_value.strip_prefix("http://"))
            .and_then(|s| s.split('/').next())
            .unwrap_or("Imported login");
        out.push(ImportLogin {
            title: Zeroizing::new(if title_value.is_empty() {
                fallback.into()
            } else {
                title_value.into()
            }),
            username: Zeroizing::new(get(username).into()),
            origin: Zeroizing::new(origin_value.into()),
            password: Zeroizing::new(password_value.into()),
            notes: Zeroizing::new(get(notes).into()),
        });
    }
    if out.is_empty() {
        return Err("No logins with passwords were found".into());
    }
    Ok(out)
}

fn parse_records(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                push_field(&mut row, &mut field)?;
            }
            '\n' if !quoted => {
                push_field(&mut row, &mut field)?;
                if !row.iter().all(String::is_empty) {
                    rows.push(std::mem::take(&mut row));
                }
            }
            '\r' if !quoted && chars.peek() == Some(&'\n') => {}
            _ => {
                field.push(ch);
                if field.len() > MAX_FIELD_BYTES {
                    return Err("A CSV field is larger than 16 KB".into());
                }
            }
        }
    }
    if quoted {
        return Err("The CSV ends inside a quoted field".into());
    }
    if !field.is_empty() || !row.is_empty() {
        push_field(&mut row, &mut field)?;
        rows.push(row);
    }
    Ok(rows)
}

fn push_field(row: &mut Vec<String>, field: &mut String) -> Result<(), String> {
    if row.len() >= MAX_FIELDS {
        return Err("A CSV row contains more than 32 fields".into());
    }
    row.push(std::mem::take(field));
    Ok(())
}

pub fn export_browser_csv(items: &[SecretItem]) -> Zeroizing<String> {
    let mut csv = Zeroizing::new("name,url,username,password,note\r\n".to_string());
    for item in items {
        let origin = item.origins.iter().next().map_or("", String::as_str);
        for (index, value) in [
            &item.title,
            origin,
            &item.username,
            &item.password,
            &item.notes,
        ]
        .into_iter()
        .enumerate()
        {
            csv.push('"');
            csv.push_str(&value.replace('"', "\"\""));
            csv.push('"');
            if index < 4 {
                csv.push(',');
            }
        }
        csv.push_str("\r\n");
    }
    csv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chrome_firefox_and_quoted_fields() {
        let csv = "name,url,username,password,note\r\n\"Bank, personal\",https://bank.example,ada,\"p\"\"ass\",\"line 1\nline 2\"\r\n";
        let rows = parse_browser_csv(csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_str(), "Bank, personal");
        assert_eq!(rows[0].password.as_str(), "p\"ass");
        assert_eq!(rows[0].notes.as_str(), "line 1\nline 2");
    }

    #[test]
    fn rejects_oversized_input_and_fields() {
        let oversized = vec![b'x'; usize::try_from(MAX_IMPORT_BYTES).unwrap() + 1];
        assert!(parse_browser_csv(oversized.as_slice()).is_err());
        let field = "x".repeat(MAX_FIELD_BYTES + 1);
        assert!(parse_browser_csv(format!("name,password\na,{field}").as_bytes()).is_err());
    }

    #[test]
    fn export_round_trips_quotes_and_newlines() {
        let mut item = SecretItem::login("A, \"quoted\"", "ada", "line\nsecret");
        item.notes = "memo\nline".into();
        item.add_origin("https://example.com").unwrap();
        let csv = export_browser_csv(&[item]);
        let rows = parse_browser_csv(csv.as_bytes()).unwrap();
        assert_eq!(rows[0].title.as_str(), "A, \"quoted\"");
        assert_eq!(rows[0].password.as_str(), "line\nsecret");
        assert_eq!(rows[0].notes.as_str(), "memo\nline");
    }

    #[test]
    fn portal_requests_keep_file_authority_at_the_process_boundary() {
        let open = import_request();
        assert_eq!(open.filter_names(), vec!["Password CSV files", "All files"]);
        let save = export_request();
        assert_eq!(save.suggested_name(), Some("passwords.csv"));
        assert_eq!(save.filter_names(), vec!["CSV file", "All files"]);
    }
}
