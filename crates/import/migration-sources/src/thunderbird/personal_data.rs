//! What a Thunderbird address book or local calendar file really holds, as the
//! canonical vCard/iCalendar text the app that owns it can read.
//!
//! Thunderbird's modern local stores are `SQLite`. Their rows are read with
//! `rusqlite` in read-only mode from a private temporary copy of the exact
//! reviewed bytes; no source file is opened writable and no credential store is
//! touched. A file that is already text is read directly.

use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sidecar {
    AddressBook,
    Calendar,
}

/// The canonical text `bytes` hold, when this module understands the file.
///
/// # Errors
/// The file could not be copied, opened or read within its bounds.
pub fn canonical(sidecar: Sidecar, bytes: &[u8]) -> io::Result<Option<String>> {
    if let Some(text) = utf8(bytes)
        && let Some(canonical) = direct(sidecar, &text)
    {
        return Ok(Some(canonical));
    }
    if !is_sqlite(bytes) {
        return Ok(None);
    }
    sqlite(sidecar, bytes)
}

fn utf8(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control) || text.contains('\n')).then(|| text.to_owned())
}

fn direct(sidecar: Sidecar, text: &str) -> Option<String> {
    match sidecar {
        Sidecar::AddressBook => {
            let cards = blocks(text, "BEGIN:VCARD", "END:VCARD");
            (!cards.is_empty()).then(|| cards.join("\r\n"))
        }
        Sidecar::Calendar => {
            let components = calendar_components(text);
            (!components.is_empty()).then(|| wrap_calendar(&components))
        }
    }
}

/// Every `begin`…`end` block of `text`, CRLF-normalized and without repeats.
pub fn blocks(text: &str, begin: &str, end: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(begin) {
        let Some(after) = rest.get(start..) else {
            break;
        };
        let Some(finish) = after.find(end) else {
            break;
        };
        let Some(block) = after.get(..finish.saturating_add(end.len())) else {
            break;
        };
        if !found.iter().any(|existing| existing == block) {
            found.push(block.replace("\r\n", "\n").replace('\n', "\r\n"));
        }
        rest = after
            .get(finish.saturating_add(end.len())..)
            .unwrap_or_default();
    }
    found
}

fn calendar_components(text: &str) -> Vec<String> {
    let mut components = blocks(text, "BEGIN:VTIMEZONE", "END:VTIMEZONE");
    components.extend(blocks(text, "BEGIN:VEVENT", "END:VEVENT"));
    components.extend(blocks(text, "BEGIN:VTODO", "END:VTODO"));
    components
}

fn wrap_calendar(components: &[String]) -> String {
    let mut text = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Thunderbird import//EN\r\n",
    );
    for component in components {
        text.push_str(component.trim_end_matches(['\r', '\n']));
        text.push_str("\r\n");
    }
    text.push_str("END:VCALENDAR\r\n");
    text
}

fn is_sqlite(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

fn sqlite(sidecar: Sidecar, bytes: &[u8]) -> io::Result<Option<String>> {
    use std::io::Write as _;
    let mut temporary = tempfile::Builder::new()
        .prefix("tilo-thunderbird-personal-data-")
        .tempfile()?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    let connection = rusqlite::Connection::open_with_flags(
        temporary.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    let tables: Vec<String> = {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .map_err(io::Error::other)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(io::Error::other)?;
        rows.filter_map(Result::ok).take(128).collect()
    };
    let mut components = Vec::new();
    for table in tables {
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        let Ok(mut statement) = connection.prepare(&format!("SELECT * FROM {quoted} LIMIT 10000"))
        else {
            continue;
        };
        let Ok(mut rows) = statement.query([]) else {
            continue;
        };
        loop {
            let row = rows.next().map_err(io::Error::other)?;
            let Some(row) = row else { break };
            for column in 0..row.as_ref().column_count() {
                let Ok(value) = row.get_ref(column) else {
                    continue;
                };
                let (rusqlite::types::ValueRef::Text(bytes)
                | rusqlite::types::ValueRef::Blob(bytes)) = value
                else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(bytes) else {
                    continue;
                };
                match sidecar {
                    Sidecar::AddressBook => {
                        for card in blocks(text, "BEGIN:VCARD", "END:VCARD") {
                            if !components.contains(&card) {
                                components.push(card);
                            }
                        }
                    }
                    Sidecar::Calendar => {
                        for component in calendar_components(text) {
                            if !components.contains(&component) {
                                components.push(component);
                            }
                        }
                    }
                }
                if components.len() > 100_000 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Thunderbird personal data exceeds its bound",
                    ));
                }
            }
        }
    }
    Ok(match sidecar {
        Sidecar::AddressBook => (!components.is_empty()).then(|| components.join("\r\n")),
        Sidecar::Calendar => (!components.is_empty()).then(|| wrap_calendar(&components)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vcard_file_is_read_as_cards() {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:a\r\nFN:Ada\r\nEND:VCARD\r\n\r\nBEGIN:VCARD\r\nUID:b\r\nFN:Grace\r\nEND:VCARD\r\n";
        let canonical = canonical(Sidecar::AddressBook, text.as_bytes())
            .unwrap()
            .expect("cards");
        assert_eq!(canonical.matches("BEGIN:VCARD").count(), 2);
        assert!(canonical.contains("FN:Grace"));
    }

    #[test]
    fn an_ics_file_is_read_as_a_calendar() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e\r\nSUMMARY:Kept\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let canonical = canonical(Sidecar::Calendar, text.as_bytes())
            .unwrap()
            .expect("calendar");
        assert!(canonical.starts_with("BEGIN:VCALENDAR"));
        assert_eq!(canonical.matches("BEGIN:VEVENT").count(), 1);
    }

    #[test]
    fn a_thunderbird_address_book_sqlite_is_converted_to_vcard() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("abook.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cards(uid TEXT, card TEXT);
                 INSERT INTO cards VALUES('one', 'BEGIN:VCARD\r\nUID:person@example.test\r\nFN:Person\r\nEND:VCARD\r\n');",
            )
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&database).unwrap();
        let canonical = canonical(Sidecar::AddressBook, &bytes)
            .unwrap()
            .expect("cards");
        assert!(canonical.contains("UID:person@example.test"));
        assert!(canonical.contains("FN:Person"));
    }

    #[test]
    fn a_thunderbird_calendar_sqlite_is_converted_to_ics() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("local.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cal_events(icalString TEXT);
                 INSERT INTO cal_events VALUES('BEGIN:VEVENT\r\nUID:kept@example.test\r\nSUMMARY:Kept\r\nEND:VEVENT');",
            )
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&database).unwrap();
        let canonical = canonical(Sidecar::Calendar, &bytes)
            .unwrap()
            .expect("calendar");
        assert!(canonical.contains("SUMMARY:Kept"));
    }

    #[test]
    fn an_unusable_sidecar_is_not_understood() {
        assert!(
            canonical(Sidecar::AddressBook, b"selected address book")
                .unwrap()
                .is_none()
        );
    }
}
