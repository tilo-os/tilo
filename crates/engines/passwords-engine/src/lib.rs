//! The Passwords app's own engine, as a package reaches it: `engine.call` answered for
//! `so.tilo.passwords` by the pieces this device already holds.
//!
//! The app runs sandboxed and keeps the workspace's vault as records. What belongs to this
//! device alone is here: the secret service's own vault and the permissions apps hold on it, the
//! browser CSV a person moves logins through, and the passphrase-encrypted backup. A request
//! carries JSON the app wrote; the answer is JSON the app reads back into its own records.
//!
//! Reading and keeping a file's bytes is the host's: [`Device`] is the platform's content store,
//! so the engine holds no path of its own.

use std::collections::BTreeMap;

use accounts::{
    AccessDecision, CredentialBroker, CredentialGrant, CredentialScope, GrantLifetime, SecretId,
    SecretItem, SecretItemKind, SecretStore,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tilo_seal::envelope::{self, Envelope, KeySource};
use tilo_seal::{Context, Label, PasswordParams};
use zeroize::Zeroizing;

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.passwords";

/// What the engine reads of this device: the content store a chosen file's bytes were kept in,
/// and where bytes it makes are kept.
pub trait Device {
    /// The bytes kept under `hash`, or none when this device does not hold them.
    fn read(&self, hash: &str) -> Option<Vec<u8>>;

    /// Keep `bytes` as the file `name` of `media`, and answer the hash they are kept under.
    ///
    /// # Errors
    ///
    /// Why the device cannot keep them.
    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<String, String>;
}

/// One `engine.call` for this device's app, or a refusal.
///
/// # Errors
///
/// The op is unknown, the input does not read, this device holds no secret service, or the work
/// fails.
pub fn call(device: &dyn Device, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "device-items" => Ok(device_items()),
        "device-remove" => device_remove(input),
        "access" => access(),
        "decide" => decide(input),
        "revoke" => revoke(input),
        "csv-import" => csv_import(device, input),
        "csv-export" => csv_export(device, input),
        "backup-export" => backup_export(device, input),
        "backup-import" => backup_import(device, input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

// ---------------------------------------------------------------- this device's vault

/// The entries this device's secret service holds that a workspace may share: the device's
/// authentication material and its Wi-Fi networks stay where they are.
fn device_items() -> String {
    let store = SecretStore::open();
    if let Some(error) = store.recovery_error() {
        return json!({"items": [], "error": error}).to_string();
    }
    let items: Vec<Json> = store
        .list()
        .iter()
        .filter(|item| travels_to_workspace(item))
        .map(item_json)
        .collect();
    json!({"items": items, "error": ""}).to_string()
}

/// Whether an item of the device vault belongs in the workspace: what a workspace may share,
/// which leaves the device's authentication material and the Wi-Fi networks the device joined.
fn travels_to_workspace(item: &SecretItem) -> bool {
    item.may_share_with_workspace() && item.kind != SecretItemKind::Wifi
}

/// One item as the app reads it.
fn item_json(item: &SecretItem) -> Json {
    json!({
        "id": item.id.0,
        "title": item.title,
        "kind": kind_word(item.kind),
        "username": item.username,
        "password": item.password,
        "notes": item.notes,
        "origins": item.origins.iter().collect::<Vec<_>>(),
        "tags": item.tags.iter().collect::<Vec<_>>(),
        "fields": item.fields,
    })
}

/// The word an item's kind is written as.
fn kind_word(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::Login => "login",
        SecretItemKind::ApiKey => "api-key",
        SecretItemKind::SecureNote => "note",
        SecretItemKind::Wifi => "wifi",
        SecretItemKind::Other => "other",
    }
}

/// The kind a word names.
fn kind_of(word: &str) -> SecretItemKind {
    match word {
        "api-key" | "api_key" => SecretItemKind::ApiKey,
        "note" => SecretItemKind::SecureNote,
        "wifi" => SecretItemKind::Wifi,
        "other" | "secret" => SecretItemKind::Other,
        _ => SecretItemKind::Login,
    }
}

/// Takes entries out of this device's vault, once the workspace has kept them.
fn device_remove(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Remove {
        ids: Vec<String>,
    }
    let ask: Remove = read(input, "the entries to take out")?;
    let mut store = SecretStore::open();
    if let Some(error) = store.recovery_error() {
        return Err(error.to_owned());
    }
    let removed = ask
        .ids
        .iter()
        .filter(|id| store.remove(&SecretId((*id).clone())).unwrap_or(false))
        .count();
    store
        .flush()
        .map_err(|error| format!("could not keep the device vault: {error}"))?;
    Ok(json!({ "removed": removed }).to_string())
}

// ---------------------------------------------------------------- app access

/// What apps may do with this device's vault, and what they ask.
fn access() -> Result<String, String> {
    let store = SecretStore::open();
    if let Some(error) = store.recovery_error() {
        return Err(error.to_owned());
    }
    let titles: BTreeMap<String, String> = store
        .list()
        .into_iter()
        .map(|item| (item.id.0.clone(), item.title))
        .collect();
    let broker = CredentialBroker::new(store);
    let grants: Vec<Json> = broker
        .current_grants()
        .iter()
        .map(|grant| grant_json(grant, &titles))
        .collect();
    let asks: Vec<Json> = tilo_messaging::service::proposals()
        .iter()
        .map(|proposal| {
            json!({
                "request_id": proposal.id,
                "app_name": app_name(&proposal.request.app_id),
                "item": titles.get(&proposal.request.item_id.0).cloned().unwrap_or_default(),
                "purpose": proposal.request.purpose,
                "origin": proposal.request.origin.clone().unwrap_or_default(),
                "service": proposal.scope.service,
                "account": proposal.scope.account,
                "generation": proposal.scope.generation,
                "person": proposal.scope.actor,
                "device": proposal.scope.device,
            })
        })
        .collect();
    Ok(json!({ "grants": grants, "asks": asks }).to_string())
}

/// One permission as the app reads it.
fn grant_json(grant: &CredentialGrant, titles: &BTreeMap<String, String>) -> Json {
    let scope: Option<&CredentialScope> = grant.scope.as_ref();
    json!({
        "grant": grant.id(),
        "app_name": app_name(&grant.app_id),
        "item": titles.get(&grant.item_id.0).cloned().unwrap_or_default(),
        "origin": grant.origin.clone().unwrap_or_default(),
        "lifetime": match grant.lifetime {
            GrantLifetime::Session => "session",
            GrantLifetime::Persistent => "persistent",
        },
        "service": scope.map(|scope| scope.service.clone()).unwrap_or_default(),
        "account": scope.map(|scope| scope.account.clone()).unwrap_or_default(),
        "generation": scope.map_or(0, |scope| scope.generation),
    })
}

/// "Browser" for `so.tilo.browser`, "Code editor" for `so.tilo.code-editor`.
fn app_name(app_id: &str) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id).replace('-', " ");
    let mut chars = last.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Answers an app's request: this session, until revoked, or no.
fn decide(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Decide {
        request: String,
        decision: String,
    }
    let ask: Decide = read(input, "the request to answer")?;
    let decision = match ask.decision.as_str() {
        "session" => AccessDecision::AllowForSession,
        "persistent" => AccessDecision::AllowAlways,
        "deny" => AccessDecision::Deny,
        other => return Err(format!("`{other}` is no answer to an app's request")),
    };
    tilo_messaging::service::decide(&ask.request, decision)
        .map_err(|error| format!("the request could not be answered: {error:?}"))?;
    Ok("{}".to_owned())
}

/// Takes one permission back.
fn revoke(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Revoke {
        grant: String,
    }
    let ask: Revoke = read(input, "the permission to take back")?;
    let store = SecretStore::open();
    if let Some(error) = store.recovery_error() {
        return Err(error.to_owned());
    }
    let mut broker = CredentialBroker::new(store);
    let held = broker
        .current_grants()
        .into_iter()
        .find(|grant| grant.id() == ask.grant)
        .ok_or_else(|| "that permission is no longer active".to_owned())?;
    let revoked = broker
        .revoke_grant(&held)
        .map_err(|error| format!("could not take the permission back: {error}"))?;
    Ok(json!({ "revoked": revoked }).to_string())
}

// ---------------------------------------------------------------- browser CSV files

/// The most bytes of one import.
pub const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
/// The most rows one import takes.
pub const MAX_ROWS: usize = 10_000;
/// The most fields one row holds.
pub const MAX_FIELDS: usize = 32;
/// The most bytes one field holds.
pub const MAX_FIELD_BYTES: usize = 16 * 1024;
/// The most bytes one backup holds.
pub const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;
const BACKUP_MAGIC: &[u8] = b"TILOPWBACKUP";
const BACKUP_V2: u8 = 2;
const BACKUP_V2_PREFIX: &[u8] = b"TILOPWBACKUP\x02";

/// One login read from a browser's CSV.
#[derive(Debug)]
pub struct ImportLogin {
    pub title: Zeroizing<String>,
    pub username: Zeroizing<String>,
    pub origin: Zeroizing<String>,
    pub password: Zeroizing<String>,
    pub notes: Zeroizing<String>,
}

/// The rows the browser CSV the app chose holds.
fn csv_import(device: &dyn Device, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
    }
    let ask: Ask = read(input, "the file to read")?;
    let bytes = read_file(device, &ask.url)?;
    let rows = parse_browser_csv(bytes.as_slice())?;
    let rows: Vec<Json> = rows
        .iter()
        .map(|row| {
            json!({
                "title": row.title.as_str(),
                "username": row.username.as_str(),
                "origin": row.origin.as_str(),
                "password": row.password.as_str(),
                "notes": row.notes.as_str(),
            })
        })
        .collect();
    Ok(json!({ "rows": rows }).to_string())
}

/// The vault as a browser CSV, kept for the app to save where the person chooses.
fn csv_export(device: &dyn Device, input: &str) -> Result<String, String> {
    let ask: ItemsAsk = read(input, "the items to export")?;
    let items = items_of(&ask.items);
    let csv = export_browser_csv(&items);
    let hash = device
        .keep("passwords.csv", "text/csv", csv.as_bytes())
        .map_err(|error| format!("could not keep the export: {error}"))?;
    Ok(json!({ "url": format!("tilo-blob://{hash}") }).to_string())
}

/// Reads a browser's CSV into the logins it holds.
///
/// # Errors
///
/// The file is too large, is not UTF-8, or holds no login.
pub fn parse_browser_csv(mut reader: impl std::io::Read) -> Result<Vec<ImportLogin>, String> {
    use std::io::Read as _;
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
    let records = parse_resources(text)?;
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
        let get =
            |index: Option<usize>| index.and_then(|i| record.get(i)).map_or("", String::as_str);
        let password_value = get(Some(password));
        if password_value.is_empty() {
            continue;
        }
        let origin_value = get(origin).trim();
        let title_value = get(title).trim();
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

fn parse_resources(text: &str) -> Result<Vec<Vec<String>>, String> {
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

/// The vault as a browser's CSV: a heading row, then one row an item.
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

// ---------------------------------------------------------------- encrypted backups

/// The vault as a passphrase-encrypted backup, kept for the app to save where the person chooses.
fn backup_export(device: &dyn Device, input: &str) -> Result<String, String> {
    let ask: BackupAsk = read(input, "the items to back up")?;
    let items = items_of(&ask.items);
    let backup = export_encrypted_backup(&items, &ask.passphrase)?;
    let hash = device
        .keep(
            "passwords-backup.tilopw",
            "application/x-tilo-passwords-backup",
            backup.as_slice(),
        )
        .map_err(|error| format!("could not keep the backup: {error}"))?;
    Ok(json!({ "url": format!("tilo-blob://{hash}") }).to_string())
}

/// The items a backup the app chose holds.
fn backup_import(device: &dyn Device, input: &str) -> Result<String, String> {
    let ask: RestoreAsk = read(input, "the backup to restore")?;
    let bytes = read_file(device, &ask.url)?;
    let items = import_encrypted_backup(bytes.as_slice(), &ask.passphrase)?;
    let items: Vec<Json> = items.iter().map(item_json).collect();
    Ok(json!({ "items": items, "error": "" }).to_string())
}

/// Serialize every vault field into a portable, passphrase-encrypted backup: a `tilo-seal`
/// Argon2id password envelope (XChaCha20-Poly1305) whose header, parameters included, is
/// authenticated.
///
/// # Errors
///
/// The password is too short or too long, the vault is larger than a backup takes, or the
/// envelope cannot be made.
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
    if plaintext.len() as u64 > MAX_BACKUP_BYTES {
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
    if (BACKUP_V2_PREFIX.len() as u64).saturating_add(sealed.len() as u64) > MAX_BACKUP_BYTES {
        return Err("The vault is too large to back up safely".into());
    }
    let mut out = Zeroizing::new(Vec::with_capacity(
        BACKUP_V2_PREFIX.len().saturating_add(sealed.len()),
    ));
    out.extend_from_slice(BACKUP_V2_PREFIX);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The items a backup holds.
///
/// # Errors
///
/// The file is no backup, uses another version, the password is wrong, or it was changed.
pub fn import_encrypted_backup(
    backup: &[u8],
    passphrase: &str,
) -> Result<Vec<SecretItem>, String> {
    validate_backup_passphrase(passphrase)?;
    if backup.len() as u64 > MAX_BACKUP_BYTES {
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
    let items = SecretItem::list_from_json(&plaintext)
        .map_err(|_| "The decrypted backup is not a valid vault".to_string())?;
    if items.len() > MAX_ROWS {
        return Err("The backup contains more than 10,000 items".into());
    }
    Ok(items)
}

fn open_v2_backup(body: &[u8], passphrase: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    // Stored parameters are bounded before any derivation runs, so a crafted file cannot demand
    // an unbounded Argon2 workload.
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

/// Argon2id v1.3 cost for backup keys.
const BACKUP_KDF_PARAMS: PasswordParams = PasswordParams {
    m_cost_kib: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
};
const BACKUP_MAX_MEMORY_KIB: u32 = 1024 * 1024;
const BACKUP_MAX_ITERATIONS: u32 = 16;
const BACKUP_MAX_LANES: u32 = 8;

// ---------------------------------------------------------------- what the app sends

/// One item as the app writes it.
#[derive(Deserialize)]
struct AskedItem {
    title: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    fields: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ItemsAsk {
    items: Vec<AskedItem>,
}

#[derive(Deserialize)]
struct BackupAsk {
    items: Vec<AskedItem>,
    passphrase: String,
}

#[derive(Deserialize)]
struct RestoreAsk {
    url: String,
    passphrase: String,
}

/// The items the app sent, as the vault's own.
fn items_of(asked: &[AskedItem]) -> Vec<SecretItem> {
    asked
        .iter()
        .map(|asked| {
            let mut item =
                SecretItem::login(asked.title.clone(), asked.username.clone(), asked.password.clone());
            item.kind = kind_of(&asked.kind);
            item.notes.clone_from(&asked.notes);
            for origin in &asked.origins {
                let _ = item.add_origin(origin);
            }
            item.tags.extend(asked.tags.iter().cloned());
            item.fields.extend(asked.fields.clone());
            item
        })
        .collect()
}

/// Reads the JSON an op takes, saying what it was for when it does not read.
fn read<T: for<'de> Deserialize<'de>>(input: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("{what} did not read: {error}"))
}

/// The bytes kept at the app's file address (`tilo-blob://<hash>`).
fn read_file(device: &dyn Device, url: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let hash = url
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| "the file is not one this device holds".to_owned())?;
    device
        .read(hash)
        .map(Zeroizing::new)
        .ok_or_else(|| "the file is no longer on this device".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::scenario;

    scenario! {
        fn transfers_keep_credentials_whole_and_refuse_what_is_out_of_bounds;

        phase browser_csv_parses_limits_and_round_trips_quoted_fields;
        {
            let csv = "name,url,username,password,note\r\n\"Bank, personal\",https://bank.example,ada,\"p\"\"ass\",\"line 1\nline 2\"\r\n";
            let rows = parse_browser_csv(csv.as_bytes()).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title.as_str(), "Bank, personal");
            assert_eq!(rows[0].password.as_str(), "p\"ass");
            assert_eq!(rows[0].notes.as_str(), "line 1\nline 2");

            let oversized = vec![b'x'; usize::try_from(MAX_IMPORT_BYTES).unwrap() + 1];
            assert!(parse_browser_csv(oversized.as_slice()).is_err());
            let field = "x".repeat(MAX_FIELD_BYTES + 1);
            assert!(parse_browser_csv(format!("name,password\na,{field}").as_bytes()).is_err());

            let mut item = SecretItem::login("A, \"quoted\"", "ada", "line\nsecret");
            item.notes = "memo\nline".into();
            item.add_origin("https://example.com").unwrap();
            let csv = export_browser_csv(&[item]);
            let rows = parse_browser_csv(csv.as_bytes()).unwrap();
            assert_eq!(rows[0].title.as_str(), "A, \"quoted\"");
            assert_eq!(rows[0].password.as_str(), "line\nsecret");
            assert_eq!(rows[0].notes.as_str(), "memo\nline");
        }

        phase encrypted_backup_round_trips_and_rejects_invalid_input;
        {
            let mut item = SecretItem::login("Payroll", "ada@example.com", "UniqueSecret-1942");
            item.kind = SecretItemKind::ApiKey;
            item.notes = "Recovery belongs to Finance".into();
            item.add_origin("https://people.example/login").unwrap();
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
            assert_eq!(
                import_encrypted_backup(&backup, "correct horse battery staple").unwrap(),
                vec![expected]
            );
            assert!(import_encrypted_backup(&backup, "another long password!").is_err());
            let last = backup.len() - 1;
            let mut tampered = backup.to_vec();
            tampered[last] ^= 0x80;
            assert!(import_encrypted_backup(&tampered, "correct horse battery staple").is_err());
            let items = vec![SecretItem::login("Fixture", "user", "secret"); MAX_ROWS + 1];
            assert!(export_encrypted_backup(&items, "a sufficiently long password").is_err());
        }
    }
}
