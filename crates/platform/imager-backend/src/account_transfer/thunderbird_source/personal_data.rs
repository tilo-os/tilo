//! Hand selected Thunderbird contacts and calendars to the apps that own them.
//!
//! The original sidecar is still retained byte for byte. Beside it, this module
//! publishes the canonical vCard/iCalendar a Thunderbird address book or local
//! calendar actually holds, under the target app's own import inbox. Contacts
//! and Calendar read that inbox at open with their existing parsers and resource
//! actions, so the person gets real contacts and events rather than an unused
//! copy under `mail-import`.
//!
//! Thunderbird's modern local stores are `SQLite`. Their rows are read with
//! `rusqlite` in read-only mode from a private temporary copy of the exact
//! reviewed bytes; no source file is opened writable and no credential store is
//! touched.

use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sidecar {
    AddressBook,
    Calendar,
}

/// Publish `bytes` for the owning app when they carry canonical personal data.
/// Returns whether a pending import was written.
pub(super) fn stage(
    target_home: &Path,
    profile_id: &str,
    source_id: &str,
    sidecar: Sidecar,
    bytes: &[u8],
) -> io::Result<bool> {
    let Some(text) = canonical(sidecar, bytes)? else {
        return Ok(false);
    };
    let (app, extension, directory) = match sidecar {
        Sidecar::AddressBook => ("so.tilo.contacts", "vcf", "contacts"),
        Sidecar::Calendar => ("so.tilo.calendar", "ics", "calendars"),
    };
    let root = target_home
        .join(".local/share/tilo-rooms/personal")
        .join(app)
        .join(".local/share")
        .join(app)
        .join("personal-data-incoming")
        .join(directory);
    std::fs::create_dir_all(&root)?;
    let name = format!("{}-{}.{}", safe(profile_id), safe(source_id), extension);
    write_private(&root.join(name), text.as_bytes())?;
    Ok(true)
}

/// The canonical text a source sidecar holds, when this module understands it.
fn canonical(sidecar: Sidecar, bytes: &[u8]) -> io::Result<Option<String>> {
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

fn blocks(text: &str, begin: &str, end: &str) -> Vec<String> {
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

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
fn sqlite(_sidecar: Sidecar, _bytes: &[u8]) -> io::Result<Option<String>> {
    Ok(None)
}

fn safe(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(96));
    for byte in value.bytes().take(96) {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            out.push(char::from(byte));
        }
    }
    if out.is_empty() {
        out.push_str("selected");
    }
    out
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("data")
    ));
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&temporary)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Some(parent) = path.parent() {
        let directory = std::fs::File::open(parent)?;
        directory.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vcard_address_book_reaches_the_contacts_import_inbox() {
        let target = tempfile::tempdir().unwrap();
        let bytes = b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada@example.test\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nEND:VCARD\r\n";
        assert!(
            stage(
                target.path(),
                "profile",
                "book",
                Sidecar::AddressBook,
                bytes
            )
            .unwrap()
        );
        let staged = target.path().join(
            ".local/share/tilo-rooms/personal/so.tilo.contacts/.local/share/so.tilo.contacts/personal-data-incoming/contacts/profile-book.vcf",
        );
        let text = std::fs::read_to_string(staged).unwrap();
        assert!(text.contains("BEGIN:VCARD"));
        assert!(text.contains("FN:Ada Lovelace"));
    }

    #[test]
    fn an_ics_calendar_reaches_the_calendar_import_inbox() {
        let target = tempfile::tempdir().unwrap();
        let bytes = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event@example.test\r\nSUMMARY:Kept\r\nDTSTART:20260102T090000Z\r\nDTEND:20260102T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(
            stage(
                target.path(),
                "profile",
                "calendar",
                Sidecar::Calendar,
                bytes,
            )
            .unwrap()
        );
        let staged = target.path().join(
            ".local/share/tilo-rooms/personal/so.tilo.calendar/.local/share/so.tilo.calendar/personal-data-incoming/calendars/profile-calendar.ics",
        );
        let text = std::fs::read_to_string(staged).unwrap();
        assert!(text.contains("BEGIN:VEVENT"));
        assert!(text.contains("SUMMARY:Kept"));
    }

    #[test]
    fn an_unusable_sidecar_stages_nothing() {
        let target = tempfile::tempdir().unwrap();
        assert!(
            !stage(
                target.path(),
                "profile",
                "book",
                Sidecar::AddressBook,
                b"selected address book",
            )
            .unwrap()
        );
    }

    #[test]
    fn a_thunderbird_sqlite_contact_store_is_converted_to_vcard() {
        let target = tempfile::tempdir().unwrap();
        let database = target.path().join("abook.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE main(card TEXT);
                 INSERT INTO main VALUES('BEGIN:VCARD\r\nUID:person@example.test\r\nFN:Person\r\nEND:VCARD\r\n');",
            )
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&database).unwrap();
        assert!(
            stage(
                target.path(),
                "profile",
                "book",
                Sidecar::AddressBook,
                &bytes
            )
            .unwrap()
        );
        let staged = target.path().join(
            ".local/share/tilo-rooms/personal/so.tilo.contacts/.local/share/so.tilo.contacts/personal-data-incoming/contacts/profile-book.vcf",
        );
        let text = std::fs::read_to_string(staged).unwrap();
        assert!(text.contains("UID:person@example.test"));
        assert!(text.contains("FN:Person"));
    }

    #[test]
    fn a_thunderbird_sqlite_calendar_store_is_converted_to_ics() {
        let target = tempfile::tempdir().unwrap();
        let database = target.path().join("calendar-data.local.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cal_events(icalString TEXT);
                 INSERT INTO cal_events VALUES('BEGIN:VEVENT\r\nUID:kept@example.test\r\nSUMMARY:Kept\r\nDTSTART:20260102T090000Z\r\nDTEND:20260102T100000Z\r\nEND:VEVENT');",
            )
            .unwrap();
        drop(connection);
        let bytes = std::fs::read(&database).unwrap();
        assert!(
            stage(
                target.path(),
                "profile",
                "calendar",
                Sidecar::Calendar,
                &bytes
            )
            .unwrap()
        );
        let staged = target.path().join(
            ".local/share/tilo-rooms/personal/so.tilo.calendar/.local/share/so.tilo.calendar/personal-data-incoming/calendars/profile-calendar.ics",
        );
        let text = std::fs::read_to_string(staged).unwrap();
        assert!(text.contains("SUMMARY:Kept"));
    }
}
