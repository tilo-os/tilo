//! The workspace's vault as Passwords' own window reads and changes it, on the person's device.
//!
//! Passwords is the vault's app: its entries are the vault's entries, never fields of its own
//! records, which hold only the entry each item stands for. Its window reads the opened entries
//! as one published reading ([`RESOURCE`]), answered to Passwords' process alone and to no other
//! app, and kept nowhere: the reading is shown, never stored. What the window changes is a call
//! of this device's vault that records no secret anywhere:
//!
//! - [`KEEP`] keeps what is safe to say of an entry (its kind, title, name, places and tags),
//!   making the entry where the vault has none by that id.
//! - [`KEEP_SECRET`] keeps one secret of an entry: what the person typed on the system's card,
//!   which the host hands here from the approval it came with, never from the app's code.
//! - [`GENERATE`] draws a strong password for an entry here, so nobody types or sees it.
//! - [`FORGET`] forgets an entry.
//! - [`CARRY`] carries one entry an older Passwords kept in its own records into the vault, once:
//!   an entry the vault already keeps under that id is left as it is.

use accounts::vault_items::VaultChange;
use accounts::{GeneratedPasswordPolicy, SecretId, SecretItem, SecretItemKind};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

use super::Workspaces;

/// The app whose window reads and changes the vault this way: the vault's own.
pub const PASSWORDS_APP: &str = tilo_app_id::apps::PASSWORDS;

/// The resource the vault is published as to Passwords' window, and its verbs are asked of.
pub const RESOURCE: &str = "vault";

/// Keep what is safe to say of an entry ([`Facts`]).
pub const KEEP: &str = "keep";
/// Keep one secret of an entry ([`SecretPart`]).
pub const KEEP_SECRET: &str = "keep-secret";
/// Draw a strong password for an entry ([`Generate`]).
pub const GENERATE: &str = "generate";
/// Forget an entry (`{"id": …}`).
pub const FORGET: &str = "forget";
/// Carry an entry kept in an older Passwords' own records into the vault ([`Carried`]).
pub const CARRY: &str = "carry";

/// What the person is told when an app other than Passwords asks for the vault this way.
pub const ONLY_PASSWORDS: &str = "Only Passwords reads and changes the workspace's vault \
                                  itself. Another app asks for one item, and its person allows \
                                  it once.";

/// What an entry is told by when its id is not one.
const NOT_AN_ENTRY: &str = "That is no entry of the vault";

/// The words for a part of an entry that is no secret of it.
const NOT_A_PART: &str =
    "An entry keeps its password, its notes and the fields a person adds, and nothing else";

/// What is safe to say of an entry: everything but its secrets.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Facts {
    id: String,
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// One secret of an entry, as a host hands it from what the person typed.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretPart {
    id: String,
    /// `password`, `notes`, or `field:<label>`.
    part: String,
    secret: Zeroizing<String>,
}

/// A password to draw for an entry, by the shared policy: its length, and the kinds of
/// character it takes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Generate {
    id: String,
    #[serde(default = "twenty")]
    length: i64,
    #[serde(default = "yes")]
    capitals: bool,
    #[serde(default = "yes")]
    digits: bool,
    #[serde(default = "yes")]
    symbols: bool,
}

const fn twenty() -> i64 {
    20
}

const fn yes() -> bool {
    true
}

/// An entry an older Passwords kept as fields of its own records.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Carried {
    #[serde(flatten)]
    facts: Facts,
    #[serde(default)]
    password: Zeroizing<String>,
    #[serde(default)]
    notes: Zeroizing<String>,
    #[serde(default)]
    fields: Vec<Field>,
}

/// A field a person added to an entry, by its label.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Field {
    label: String,
    value: Zeroizing<String>,
}

/// The kind an entry is, by the word Passwords' window names it with.
fn kind_of(word: &str) -> Result<SecretItemKind, String> {
    Ok(match word {
        "login" => SecretItemKind::Login,
        "api_key" | "api-key" => SecretItemKind::ApiKey,
        "note" | "secure-note" => SecretItemKind::SecureNote,
        "wifi" => SecretItemKind::Wifi,
        "secret" | "other" => SecretItemKind::Other,
        _ => return Err(format!("`{word}` is no kind of entry")),
    })
}

/// The word Passwords' window names `kind` with.
fn kind_word(kind: SecretItemKind) -> &'static str {
    match kind {
        SecretItemKind::Login => "login",
        SecretItemKind::ApiKey => "api_key",
        SecretItemKind::SecureNote => "note",
        SecretItemKind::Wifi => "wifi",
        SecretItemKind::Other => "secret",
    }
}

/// `facts` written onto `entry`, which keeps its secrets.
fn with_facts(mut entry: SecretItem, facts: Facts) -> Result<SecretItem, String> {
    entry.kind = kind_of(&facts.kind)?;
    entry.title = facts.title.trim().to_owned();
    entry.username = facts.username.trim().to_owned();
    entry.origins = facts
        .origins
        .into_iter()
        .map(|origin| origin.trim().to_owned())
        .filter(|origin| !origin.is_empty())
        .collect();
    entry.tags = facts
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect();
    Ok(entry)
}

/// A new entry under `id`, holding nothing yet.
fn new_entry(id: &str) -> SecretItem {
    let now = tilo_clock::unix_now();
    SecretItem {
        id: SecretId(id.to_owned()),
        kind: SecretItemKind::Login,
        title: String::new(),
        username: String::new(),
        password: String::new(),
        origins: BTreeSet::new(),
        notes: String::new(),
        fields: BTreeMap::new(),
        tags: BTreeSet::new(),
        created_unix: now,
        updated_unix: now,
    }
}

/// `secret` kept as `part` of `entry`. An empty field's secret takes the field away.
fn with_secret(mut entry: SecretItem, part: &str, secret: &str) -> Result<SecretItem, String> {
    match part {
        "password" => secret.clone_into(&mut entry.password),
        "notes" => secret.clone_into(&mut entry.notes),
        _ => {
            let label = part
                .strip_prefix("field:")
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .ok_or(NOT_A_PART)?;
            if secret.is_empty() {
                entry.fields.remove(label);
            } else {
                entry.fields.insert(label.to_owned(), secret.to_owned());
            }
        }
    }
    entry.updated_unix = tilo_clock::unix_now();
    Ok(entry)
}

/// What Passwords' window reads of one entry: everything it is, its secrets included, which the
/// window shows only when the person asks and keeps nowhere.
fn entry_reading(entry: &SecretItem) -> Value {
    serde_json::json!({
        "id": entry.id.0,
        "kind": kind_word(entry.kind),
        "title": entry.title,
        "username": entry.username,
        "password": entry.password,
        "notes": entry.notes,
        "origins": entry.origins.iter().collect::<Vec<_>>(),
        "tags": entry.tags.iter().collect::<Vec<_>>(),
        "fields": entry
            .fields
            .iter()
            .map(|(label, value)| serde_json::json!({"label": label, "value": value}))
            .collect::<Vec<_>>(),
        "created": entry.created_unix,
        "updated": entry.updated_unix,
        "changed": iso_utc(entry.updated_unix),
        "stale": tilo_clock::unix_now().saturating_sub(entry.updated_unix) > YEAR,
    })
}

/// A year in seconds: a password last changed longer ago than this is old.
const YEAR: u64 = 365 * 24 * 60 * 60;

/// `unix` seconds as an ISO 8601 time in UTC, `2026-11-24T09:30:00Z`, which tl reads as a `Time`.
fn iso_utc(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX / 2);
    let of_day = unix % 86_400;
    // Days since 1970-01-01 to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// The arguments `arguments`, read as `T`, or why they are none: never in their own words, so a
/// secret meant as one is never echoed.
fn read<T: serde::de::DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, String> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|_| "The vault was asked with arguments it does not take".to_owned())
}

/// `id` as an entry's id, or the refusal.
fn entry_id(id: &str) -> Result<&str, String> {
    accounts::is_entry_id(id)
        .then_some(id)
        .ok_or_else(|| NOT_AN_ENTRY.to_owned())
}

impl Workspaces {
    /// Fires whenever an entry of a vault this device holds changed here.
    #[must_use]
    pub fn vault_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.vault_moved.subscribe()
    }

    /// Say the vault changed, so Passwords' window reads it again.
    pub(crate) fn vault_changed(&self) {
        self.inner
            .vault_moved
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// `workspace`'s vault as `app`'s window reads it: every entry, secrets included, for
    /// Passwords alone.
    ///
    /// # Errors
    /// `app` is not Passwords, or the vault does not open on this device.
    pub async fn passwords_vault_reading(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<Value, String> {
        if app != PASSWORDS_APP {
            return Err(ONLY_PASSWORDS.to_owned());
        }
        let Some(opened) = self.read_made_vault(workspace).await? else {
            return Ok(serde_json::json!({"entries": []}));
        };
        let mut entries: Vec<&SecretItem> =
            opened.entries().iter().map(|(entry, _)| entry).collect();
        entries.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(serde_json::json!({
            "entries": entries.into_iter().map(entry_reading).collect::<Vec<_>>(),
        }))
    }

    /// Run `action` of the vault for `app`'s window in `workspace`, as this device's person.
    ///
    /// # Errors
    /// `app` is not Passwords, the action or its arguments are none of the vault's, or the
    /// vault does not open or take the write on this device.
    pub async fn passwords_vault_invoke(
        &self,
        workspace: &str,
        app: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<(), String> {
        if app != PASSWORDS_APP {
            return Err(ONLY_PASSWORDS.to_owned());
        }
        let opened = self.open_vault(workspace).await?;
        let kept = |id: &str| opened.entry(id).cloned().unwrap_or_else(|| new_entry(id));
        let change = match action {
            KEEP => {
                let facts: Facts = read(arguments)?;
                let entry = kept(entry_id(&facts.id)?);
                let mut entry = with_facts(entry, facts)?;
                entry.updated_unix = tilo_clock::unix_now();
                VaultChange::Keep(entry)
            }
            KEEP_SECRET => {
                let part: SecretPart = read(arguments)?;
                let entry = kept(entry_id(&part.id)?);
                VaultChange::Keep(with_secret(entry, &part.part, &part.secret)?)
            }
            GENERATE => {
                let asked: Generate = read(arguments)?;
                let entry = kept(entry_id(&asked.id)?);
                let policy = GeneratedPasswordPolicy {
                    length: usize::try_from(asked.length.clamp(12, 128)).unwrap_or(20),
                    lowercase: true,
                    uppercase: asked.capitals,
                    digits: asked.digits,
                    symbols: asked.symbols,
                };
                let drawn = policy.generate().map_err(|error| error.to_string())?;
                VaultChange::Keep(with_secret(entry, "password", &drawn)?)
            }
            FORGET => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Forget {
                    id: String,
                }
                let forget: Forget = read(arguments)?;
                if opened.entry(entry_id(&forget.id)?).is_none() {
                    return Ok(());
                }
                VaultChange::Forget(SecretId(forget.id))
            }
            CARRY => {
                let carried: Carried = read(arguments)?;
                let id = entry_id(&carried.facts.id)?.to_owned();
                // Carried once: what the vault keeps under the id already is the newer.
                if opened.entry(&id).is_some() {
                    return Ok(());
                }
                let mut entry = with_facts(new_entry(&id), carried.facts)?;
                carried.password.as_str().clone_into(&mut entry.password);
                carried.notes.as_str().clone_into(&mut entry.notes);
                for field in carried.fields {
                    let label = field.label.trim();
                    if !label.is_empty() && !field.value.is_empty() {
                        entry
                            .fields
                            .insert(label.to_owned(), field.value.as_str().to_owned());
                    }
                }
                VaultChange::Keep(entry)
            }
            other => return Err(format!("`{other}` is no action of the vault")),
        };
        self.write_vault(&opened, vec![change], None, None).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test an unwrap is the assertion")]
mod tests {
    use super::*;

    #[test]
    fn a_secret_part_lands_where_it_names_and_nowhere_else() {
        let entry = new_entry("0123456789abcdef0123456789abcdef");
        let entry = with_secret(entry, "password", "hunter2").unwrap();
        let entry = with_secret(entry, "field:PIN", "1234").unwrap();
        let entry = with_secret(entry, "notes", "kept").unwrap();
        assert_eq!(entry.password, "hunter2");
        assert_eq!(entry.fields.get("PIN").map(String::as_str), Some("1234"));
        assert_eq!(entry.notes, "kept");
        let entry = with_secret(entry, "field:PIN", "").unwrap();
        assert!(
            entry.fields.is_empty(),
            "an empty field's secret takes it away"
        );
        assert!(with_secret(entry, "username", "x").is_err());
    }

    #[test]
    fn a_change_time_reads_as_a_utc_day() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_795_512_645), "2026-11-24T09:30:45Z");
    }

    #[test]
    fn a_refusal_never_quotes_what_was_sent() {
        let mut arguments = Map::new();
        arguments.insert("id".into(), "x".into());
        arguments.insert("part".into(), "password".into());
        arguments.insert("secret".into(), "hunter2".into());
        arguments.insert("extra".into(), "hunter2".into());
        let refusal = read::<SecretPart>(arguments).err().unwrap();
        assert!(!refusal.contains("hunter2"));
    }
}
