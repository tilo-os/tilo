//! Bounded browser-CSV interchange for the Passwords portal flow.
//!
//! This module is deliberately path-free: the host portal owns file
//! selection; parsing consumes a reader and exporting produces bytes for the
//! already-selected destination.

use std::io::Read;

use accounts::SecretItem;
use tilo_seal::envelope::{self, Envelope, KeySource};
use tilo_seal::{Context, Label, PasswordParams};
use zeroize::Zeroizing;

pub const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ROWS: usize = 10_000;
pub const MAX_FIELDS: usize = 32;
pub const MAX_FIELD_BYTES: usize = 16 * 1024;
const MAX_BACKUP_BYTES: usize = 64 * 1024 * 1024;
const BACKUP_MAGIC: &[u8] = b"TILOPWBACKUP";
/// The magic, this byte, then a `tilo-seal` password envelope
/// that records its Argon2id parameters and salt.
const BACKUP_V2: u8 = 2;
const BACKUP_V2_PREFIX: &[u8] = b"TILOPWBACKUP\x02";
const BACKUP_READ_LIMIT: u64 = MAX_BACKUP_BYTES as u64 + 1;

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

pub fn backup_request() -> ui::portal::PickRequest {
    ui::portal::PickRequest::save(
        "Back up password vault",
        "Back up",
        "passwords-backup.tilopw",
    )
    .thread_name("passwords-portal-backup")
    .filter(
        ui::portal::FileFilter::new("Encrypted Passwords backup")
            .mimetype("application/x-tilo-passwords-backup")
            .glob("*.tilopw"),
    )
    .all_files()
}

pub fn restore_request() -> ui::portal::PickRequest {
    ui::portal::PickRequest::open("Restore password vault", "Open backup")
        .thread_name("passwords-portal-restore")
        .filter(
            ui::portal::FileFilter::new("Encrypted Passwords backup")
                .mimetype("application/x-tilo-passwords-backup")
                .glob("*.tilopw"),
        )
        .all_files()
}

pub fn read_encrypted_backup(mut reader: impl Read) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .by_ref()
        .take(BACKUP_READ_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the selected backup".to_string())?;
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err("The selected backup is larger than 64 MB".into());
    }
    Ok(bytes)
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

/// Serialize every vault field into a portable, passphrase-encrypted backup:
/// a `tilo-seal` Argon2id password envelope (XChaCha20-Poly1305) whose
/// header, parameters included, is authenticated.
pub fn export_encrypted_backup(
    items: &[SecretItem],
    passphrase: &str,
) -> Result<Zeroizing<Vec<u8>>, String> {
    validate_backup_passphrase(passphrase)?;
    if items.len() > MAX_ROWS {
        return Err("The vault contains more than 10,000 items; no backup was created".into());
    }
    let plaintext = Zeroizing::new(
        serde_json::to_vec(items).map_err(|_| "Could not prepare the vault backup".to_string())?,
    );
    if plaintext.len() > MAX_BACKUP_BYTES {
        return Err("The vault is too large to back up safely".into());
    }
    let sealed = envelope::seal_with_password(
        passphrase.as_bytes(),
        BACKUP_KDF_PARAMS,
        Context::new(Label::PasswordBackup, BACKUP_V2_PREFIX),
        &plaintext,
    )
    .map_err(|error| match error {
        tilo_seal::Error::Randomness => {
            "Secure randomness is unavailable; no backup was created".to_string()
        }
        _ => "Could not encrypt the vault backup".to_string(),
    })?;
    if BACKUP_V2_PREFIX.len().saturating_add(sealed.len()) > MAX_BACKUP_BYTES {
        return Err("The vault is too large to back up safely".into());
    }
    let mut out = Zeroizing::new(Vec::with_capacity(
        BACKUP_V2_PREFIX.len().saturating_add(sealed.len()),
    ));
    out.extend_from_slice(BACKUP_V2_PREFIX);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open a backup created by [`export_encrypted_backup`]. Authentication is verified before JSON is
/// parsed, so wrong passwords and modified files yield no partial vault
/// contents.
pub fn import_encrypted_backup(backup: &[u8], passphrase: &str) -> Result<Vec<SecretItem>, String> {
    validate_backup_passphrase(passphrase)?;
    if backup.len() > MAX_BACKUP_BYTES {
        return Err("This is not a valid Passwords backup".into());
    }
    let rest = backup
        .strip_prefix(BACKUP_MAGIC)
        .ok_or_else(|| "This is not a valid Passwords backup".to_string())?;
    let (version, body) = rest
        .split_first()
        .ok_or_else(|| "This is not a valid Passwords backup".to_string())?;
    let plaintext = match *version {
        BACKUP_V2 => open_v2_backup(body, passphrase)?,
        _ => return Err("This Passwords backup uses an unsupported version".into()),
    };
    let items = serde_json::from_slice::<Vec<SecretItem>>(&plaintext)
        .map_err(|_| "The decrypted backup is not a valid vault".to_string())?;
    if items.len() > MAX_ROWS {
        return Err("The backup contains more than 10,000 items".into());
    }
    Ok(items)
}

fn open_v2_backup(body: &[u8], passphrase: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    // Stored parameters are bounded before any derivation runs, so a crafted
    // file cannot demand an unbounded Argon2 workload.
    match Envelope::parse(body)
        .map_err(|_| "This is not a valid Passwords backup".to_string())?
        .source
    {
        KeySource::Password { params, .. }
            if params.m_cost_kib <= BACKUP_MAX_MEMORY_KIB
                && params.t_cost <= BACKUP_MAX_ITERATIONS
                && params.p_cost <= BACKUP_MAX_LANES => {}
        _ => return Err("This is not a valid Passwords backup".into()),
    }
    envelope::open_with_password(
        passphrase.as_bytes(),
        Context::new(Label::PasswordBackup, BACKUP_V2_PREFIX),
        body,
    )
    .map_err(|_| "The password is incorrect or the backup was changed".to_string())
}

fn validate_backup_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.chars().count() < 12 {
        return Err("Use at least 12 characters for the backup password".into());
    }
    if passphrase.len() > 1024 {
        return Err("The backup password is too long".into());
    }
    Ok(())
}

/// Argon2id v1.3 cost for backup keys. Files store them in the authenticated
/// header, so new backups can raise them without a format change. The OWASP
/// baseline profile keeps restores fast on low-end devices.
const BACKUP_KDF_PARAMS: PasswordParams = PasswordParams {
    m_cost_kib: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
};
/// The most work a backup file may ask a reader to do.
const BACKUP_MAX_MEMORY_KIB: u32 = 1024 * 1024;
const BACKUP_MAX_ITERATIONS: u32 = 16;
const BACKUP_MAX_LANES: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_envelope_format_is_versioned_and_cannot_be_downgraded() {
        eprintln!("phase new_backups_are_version_2_envelopes_that_cannot_be_downgraded");
        {
            let item = SecretItem::login("Mail", "ada", "strong secret");
            let backup = export_encrypted_backup(
                std::slice::from_ref(&item),
                "a sufficiently long password",
            )
            .unwrap();
            assert!(backup.starts_with(BACKUP_V2_PREFIX));
            assert!(envelope::is_envelope(&backup[BACKUP_V2_PREFIX.len()..]));
            assert_eq!(
                import_encrypted_backup(&backup, "a sufficiently long password").unwrap(),
                vec![item]
            );
            assert!(import_encrypted_backup(&backup, "another long password!").is_err());
            for index in [BACKUP_V2_PREFIX.len() + 8, backup.len() / 2] {
                let mut tampered = backup.to_vec();
                tampered[index] ^= 0x01;
                assert!(
                    import_encrypted_backup(&tampered, "a sufficiently long password").is_err(),
                    "byte {index}"
                );
            }
            let mut downgraded = backup.to_vec();
            downgraded[BACKUP_MAGIC.len()] = 1;
            assert!(import_encrypted_backup(&downgraded, "a sufficiently long password").is_err());
        }
    }

    #[test]
    fn browser_csv_parses_limits_and_round_trips_quoted_fields() {
        eprintln!("phase parses_chrome_firefox_and_quoted_fields");
        {
            let csv = "name,url,username,password,note\r\n\"Bank, personal\",https://bank.example,ada,\"p\"\"ass\",\"line 1\nline 2\"\r\n";
            let rows = parse_browser_csv(csv.as_bytes()).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title.as_str(), "Bank, personal");
            assert_eq!(rows[0].password.as_str(), "p\"ass");
            assert_eq!(rows[0].notes.as_str(), "line 1\nline 2");
        }

        eprintln!("phase rejects_oversized_input_and_fields");
        {
            let oversized = vec![b'x'; usize::try_from(MAX_IMPORT_BYTES).unwrap() + 1];
            assert!(parse_browser_csv(oversized.as_slice()).is_err());
            let field = "x".repeat(MAX_FIELD_BYTES + 1);
            assert!(parse_browser_csv(format!("name,password\na,{field}").as_bytes()).is_err());
        }

        eprintln!("phase export_round_trips_quotes_and_newlines");
        {
            let mut item = SecretItem::login("A, \"quoted\"", "ada", "line\nsecret");
            item.notes = "memo\nline".into();
            item.add_origin("https://example.com").unwrap();
            let csv = export_browser_csv(&[item]);
            let rows = parse_browser_csv(csv.as_bytes()).unwrap();
            assert_eq!(rows[0].title.as_str(), "A, \"quoted\"");
            assert_eq!(rows[0].password.as_str(), "line\nsecret");
            assert_eq!(rows[0].notes.as_str(), "memo\nline");
        }
    }

    #[test]
    fn portal_requests_keep_file_authority_at_the_process_boundary() {
        let open = import_request();
        assert_eq!(open.filter_names(), vec!["Password CSV files", "All files"]);
        let save = export_request();
        assert_eq!(save.suggested_name(), Some("passwords.csv"));
        assert_eq!(save.filter_names(), vec!["CSV file", "All files"]);
        let backup = backup_request();
        assert_eq!(backup.suggested_name(), Some("passwords-backup.tilopw"));
        assert_eq!(
            backup.filter_names(),
            vec!["Encrypted Passwords backup", "All files"]
        );
        assert_eq!(
            restore_request().filter_names(),
            vec!["Encrypted Passwords backup", "All files"]
        );
    }

    #[test]
    fn encrypted_backup_round_trips_and_rejects_invalid_input() {
        eprintln!("phase encrypted_backup_round_trips_every_vault_field_without_plaintext");
        {
            let mut item = SecretItem::login("Payroll", "ada@example.com", "UniqueSecret-1942");
            item.kind = accounts::SecretItemKind::ApiKey;
            item.notes = "Recovery belongs to Finance".into();
            item.add_origin("https://people.example/login").unwrap();
            item.add_origin("https://sso.example/payroll").unwrap();
            item.fields.insert("tenant".into(), "northstar".into());
            item.tags.extend(["work".into(), "favorite".into()]);
            let expected = item.clone();

            let backup = export_encrypted_backup(&[item], "correct horse battery staple").unwrap();
            assert!(backup.starts_with(BACKUP_MAGIC));
            assert!(
                !backup
                    .windows(b"UniqueSecret-1942".len())
                    .any(|w| w == b"UniqueSecret-1942")
            );
            assert!(!backup.windows(b"Payroll".len()).any(|w| w == b"Payroll"));
            let restored =
                import_encrypted_backup(&backup, "correct horse battery staple").unwrap();
            assert_eq!(restored, vec![expected]);
        }

        eprintln!("phase backup_export_refuses_item_counts_that_restore_cannot_accept");
        {
            let items = vec![SecretItem::login("Fixture", "user", "secret"); MAX_ROWS + 1];
            assert!(export_encrypted_backup(&items, "a sufficiently long password").is_err());
        }

        eprintln!("phase every_truncated_backup_header_is_rejected_without_decryption");
        {
            let backup = export_encrypted_backup(&[], "a sufficiently long password").unwrap();
            // Prefix plus the whole v2 envelope header (magic .. nonce).
            let v2_header_len = BACKUP_V2_PREFIX.len() + 8 + 12 + 1 + 16 + 24;
            for length in 0..=v2_header_len {
                assert!(
                    import_encrypted_backup(
                        backup.get(..length).unwrap(),
                        "a sufficiently long password"
                    )
                    .is_err()
                );
            }
            let mut unsupported = backup;
            *unsupported.get_mut(BACKUP_MAGIC.len()).unwrap() = 255;
            assert!(import_encrypted_backup(&unsupported, "a sufficiently long password").is_err());
        }

        eprintln!("phase encrypted_backup_rejects_wrong_password_tampering_and_weak_passwords");
        {
            let item = SecretItem::login("Mail", "ada", "strong secret");
            assert!(export_encrypted_backup(std::slice::from_ref(&item), "short").is_err());
            let mut backup =
                export_encrypted_backup(&[item], "a sufficiently long password").unwrap();
            assert!(import_encrypted_backup(&backup, "a different long password").is_err());
            let last = backup.len() - 1;
            backup[last] ^= 0x80;
            assert!(import_encrypted_backup(&backup, "a sufficiently long password").is_err());
            assert!(
                import_encrypted_backup(b"not a backup", "a sufficiently long password").is_err()
            );
        }
    }
}
