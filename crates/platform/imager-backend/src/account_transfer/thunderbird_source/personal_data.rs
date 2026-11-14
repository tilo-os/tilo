//! Hand selected Thunderbird contacts and calendars to the apps that own them.
//!
//! The original sidecar is still retained byte for byte. Beside it, this module
//! publishes the canonical vCard/iCalendar a Thunderbird address book or local
//! calendar actually holds, under the target app's own import inbox. Contacts
//! and Calendar read that inbox at open with their existing parsers and resource
//! actions, so the person gets real contacts and events rather than an unused
//! copy under `mail-import`.
//!
//! What the file holds is read by `migration_sources::thunderbird::personal_data`,
//! which the laptop command reads the same way.

use std::io;
use std::path::Path;

pub(super) use migration_sources::thunderbird::personal_data::{Sidecar, canonical};

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
        .join(".local/share/tilo-workspaces/personal")
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

    tilo_test_env::scenario! {
        fn thunderbird_personal_data_reaches_the_import_inboxes;

        phase a_vcard_address_book_reaches_the_contacts_import_inbox;
        {
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
                ".local/share/tilo-workspaces/personal/so.tilo.contacts/.local/share/so.tilo.contacts/personal-data-incoming/contacts/profile-book.vcf",
            );
            let text = std::fs::read_to_string(staged).unwrap();
            assert!(text.contains("BEGIN:VCARD"));
            assert!(text.contains("FN:Ada Lovelace"));
        }

        phase an_ics_calendar_reaches_the_calendar_import_inbox;
        {
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
                ".local/share/tilo-workspaces/personal/so.tilo.calendar/.local/share/so.tilo.calendar/personal-data-incoming/calendars/profile-calendar.ics",
            );
            let text = std::fs::read_to_string(staged).unwrap();
            assert!(text.contains("BEGIN:VEVENT"));
            assert!(text.contains("SUMMARY:Kept"));
        }

        phase an_unusable_sidecar_stages_nothing;
        {
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
    }
}
