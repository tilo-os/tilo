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
