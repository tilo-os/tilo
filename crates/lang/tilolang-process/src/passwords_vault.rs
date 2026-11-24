//! What Passwords' process does with secrets on the app's behalf, so none of them ever becomes a
//! value of its code, a field of its records, an argument or history.
//!
//! Passwords keeps its secrets in the workspace's vault and calls the vault's own actions by
//! their address (`tilo.vault/vault#add`). A secret its code hands the vault is named by where
//! it is, never what it is ([`tilolang_tilo::page::fill_secrets`]): what the person typed on the
//! system's card for the call they approved, or one this host read from a file the person chose
//! (a browser's CSV, a backup, this device's own older vault) and holds under a token
//! ([`tilolang_tilo::page::hold`]). A secret leaving the vault for a file is answered into this
//! host the same way, and this host writes it into the file.
//!
//! Each op is an `engine.call` of Passwords' (`vault-…`), answered with JSON that holds no
//! secret: only tokens the host resolves.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// What the person is told when the call has no typed secret to keep.
const NOTHING_TYPED: &str = "Nothing was typed on the card, so nothing was kept";
/// What the person is told when a token names nothing this host holds.
const NOT_HELD: &str = "What was read for it is no longer here: try again";

/// Answer `op` for Passwords, or `None` when it is not one of the vault's.
pub(crate) fn call(
    device: &dyn passwords_engine::Device,
    op: &str,
    input: &str,
) -> Option<Result<String, String>> {
    Some(match op {
        "vault-hold-typed" => hold_typed(input),
        "vault-device-items" => device_items(device),
        "vault-device-remove" => {
            passwords_engine::call(device, passwords_engine::APP, "device-remove", input)
        }
        "vault-preview-csv" => preview_csv(device, input),
        "vault-read-csv" => read_csv(device, input),
        "vault-read-backup" => read_backup(device, input),
        "vault-export-csv" => export_csv(device, input),
        "vault-export-backup" => export_backup(device, input),
        _ => return None,
    })
}

/// The JSON `input` reads as, saying what it was for when it does not.
fn read<T: for<'de> Deserialize<'de>>(input: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|_| format!("{what} did not read"))
}

/// `secret` held for the app's code: its token, or nothing for an empty one.
fn held(secret: &str) -> Result<Value, String> {
    if secret.is_empty() {
        return Ok(Value::String(String::new()));
    }
    tilolang_tilo::page::hold(secret.to_owned()).map(Value::String)
}

/// What a token names, taken: empty for no token.
fn resolved(token: &str) -> Result<String, String> {
    if token.is_empty() {
        return Ok(String::new());
    }
    tilolang_tilo::page::take_held(token).ok_or_else(|| NOT_HELD.to_owned())
}

/// Hold what the person typed in place `place` on the card for the call being run, for a later
/// step of the same action: `{"place": n}`, answered `{"token"}`.
fn hold_typed(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        #[serde(default)]
        place: usize,
    }
    let ask: Ask = read(input, "the place of what was typed")?;
    let typed = tilolang_tilo::page::with_typed(|typed| {
        typed.get(ask.place).map(|typed| (*typed).to_owned())
    })
    .ok_or(NOTHING_TYPED)?;
    Ok(json!({"token": held(&typed)?}).to_string())
}

/// One item an engine read (its id, kind, title, name, password, notes, places, tags and fields),
/// with its password, its notes and its fields' values held: what the app's code adds to the
/// vault by naming them.
fn held_item(item: &Value) -> Result<Value, String> {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    let list = |key: &str| item.get(key).cloned().unwrap_or_else(|| json!([]));
    let mut fields = Vec::new();
    match item.get("fields") {
        Some(Value::Object(map)) => {
            for (label, value) in map {
                fields.push(json!({
                    "label": label,
                    "value": held(value.as_str().unwrap_or_default())?,
                }));
            }
        }
        Some(Value::Array(listed)) => {
            for field in listed {
                fields.push(json!({
                    "label": field.get("label").and_then(Value::as_str).unwrap_or_default(),
                    "value": held(field.get("value").and_then(Value::as_str).unwrap_or_default())?,
                }));
            }
        }
        _ => {}
    }
    Ok(json!({
        "id": text("id"),
        "kind": text("kind"),
        "title": text("title"),
        "username": text("username"),
        "origins": list("origins"),
        "tags": list("tags"),
        "password": held(text("password"))?,
        "notes": held(text("notes"))?,
        "fields": fields,
    }))
}

/// Every entry this device's own older vault holds, with its secrets held: `{"items", "error"}`.
fn device_items(device: &dyn passwords_engine::Device) -> Result<String, String> {
    let answer = passwords_engine::call(device, passwords_engine::APP, "device-items", "")?;
    let answer: Value =
        serde_json::from_str(&answer).map_err(|_| "the device vault did not read")?;
    let problem = answer
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !problem.is_empty() {
        return Ok(json!({"items": [], "error": problem}).to_string());
    }
    let mut items = Vec::new();
    for item in answer
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        items.push(held_item(item)?);
    }
    Ok(json!({"items": items, "error": ""}).to_string())
}

/// The rows of a browser's CSV the person chose, without their passwords or notes: what the
/// window shows while the person decides. `{"url"}`.
fn preview_csv(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    let rows = csv_rows(device, input)?;
    let shown: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "title": row.get("title").cloned().unwrap_or_default(),
                "username": row.get("username").cloned().unwrap_or_default(),
                "origin": row.get("origin").cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(json!({"rows": shown}).to_string())
}

/// The rows a browser's CSV holds, secrets included, read here alone.
fn csv_rows(device: &dyn passwords_engine::Device, input: &str) -> Result<Vec<Value>, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
    }
    let ask: Ask = read(input, "the file to read")?;
    let answer = passwords_engine::call(
        device,
        passwords_engine::APP,
        "csv-import",
        &json!({"url": ask.url}).to_string(),
    )?;
    let answer: Value = serde_json::from_str(&answer).map_err(|_| "the file did not read")?;
    Ok(answer
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// The rows of a browser's CSV to bring into the vault, but those the window skips, each a login
/// with its password and notes held: `{"url", "skip": [index]}`, answered `{"items"}`.
fn read_csv(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        #[serde(default)]
        skip: Vec<usize>,
    }
    let ask: Ask = read(input, "the rows to import")?;
    let rows = csv_rows(device, &json!({"url": ask.url}).to_string())?;
    let mut read = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if ask.skip.contains(&index) {
            continue;
        }
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
        let origin = text("origin");
        read.push(json!({
            "kind": "login",
            "title": text("title"),
            "username": text("username"),
            "origins": if origin.is_empty() { json!([]) } else { json!([origin]) },
            "tags": [],
            "password": held(text("password"))?,
            "notes": held(text("notes"))?,
            "fields": [],
        }));
    }
    Ok(json!({"items": read}).to_string())
}

/// The entries a backup holds, opened with the passphrase the person typed on the card for the
/// call, each with its secrets held: `{"url"}`, answered `{"items"}`.
fn read_backup(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
    }
    let ask: Ask = read(input, "the backup to restore")?;
    let answer = tilolang_tilo::page::with_typed(|typed| {
        let passphrase = typed.first().ok_or(NOTHING_TYPED)?;
        passwords_engine::call(
            device,
            passwords_engine::APP,
            "backup-import",
            &json!({"url": ask.url, "passphrase": passphrase}).to_string(),
        )
    })?;
    let answer: Value = serde_json::from_str(&answer).map_err(|_| "the backup did not read")?;
    let mut items = Vec::new();
    for item in answer
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        items.push(held_item(item)?);
    }
    Ok(json!({"items": items}).to_string())
}

/// The items the app's code names for a file, their secrets resolved here: each its kind, title,
/// tags and places, its username, password and notes by token, and its fields' values by token.
fn resolved_items(items: &[Value]) -> Result<Vec<Value>, String> {
    let mut read = Vec::with_capacity(items.len());
    for item in items {
        let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
        let mut fields = Map::new();
        for field in item
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let label = field
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let value = resolved(
                field
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?;
            fields.insert(label.to_owned(), Value::String(value));
        }
        read.push(json!({
            "kind": text("kind"),
            "title": text("title"),
            "username": resolved(text("username"))?,
            "password": resolved(text("password"))?,
            "notes": resolved(text("notes"))?,
            "origins": item.get("origins").cloned().unwrap_or_else(|| json!([])),
            "tags": item.get("tags").cloned().unwrap_or_else(|| json!([])),
            "fields": fields,
        }));
    }
    Ok(read)
}

/// What a file is made of: the items, and for a backup the token of its passphrase.
#[derive(Deserialize)]
struct ToFile {
    items: Vec<Value>,
    #[serde(default)]
    passphrase: String,
}

/// The vault as a browser CSV, kept for the window to save where the person chooses:
/// `{"items"}`, answered `{"url"}`.
fn export_csv(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    let asked: ToFile = read(input, "the items to export")?;
    let items = resolved_items(&asked.items)?;
    passwords_engine::call(
        device,
        passwords_engine::APP,
        "csv-export",
        &json!({"items": items}).to_string(),
    )
}

/// The vault as a backup under the passphrase the person typed on the card, held for this step:
/// `{"items", "passphrase"}`, answered `{"url"}`.
fn export_backup(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    let asked: ToFile = read(input, "the items to back up")?;
    let passphrase = resolved(&asked.passphrase)?;
    if passphrase.is_empty() {
        return Err(NOTHING_TYPED.to_owned());
    }
    let items = resolved_items(&asked.items)?;
    passwords_engine::call(
        device,
        passwords_engine::APP,
        "backup-export",
        &json!({"items": items, "passphrase": passphrase}).to_string(),
    )
}
