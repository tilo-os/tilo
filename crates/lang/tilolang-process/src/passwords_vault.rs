//! What Passwords' process does with secrets on the app's behalf, so none of them ever becomes a
//! value of its code, a field of its records, an argument or history.
//!
//! Passwords keeps its secrets in the workspace's vault, which this device's daemon publishes to
//! Passwords alone as `vault`. Its code names an entry and never holds a secret: a secret arrives
//! as what the person typed on the system's card for the call they approved
//! ([`tilolang_tilo::page::with_typed`]), or from a file the person chose (a browser's CSV, a
//! backup, this device's own older vault), and this host hands it straight to the vault. What
//! leaves the vault for a file is read here from the published vault and written into the file.
//!
//! Each op is an `engine.call` of Passwords' (`vault-…`), answered with JSON that holds no
//! secret.

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The resource the vault is published as, and its verbs.
const VAULT: &str = "vault";
const KEEP_SECRET: &str = "keep-secret";
const CARRY: &str = "carry";

/// What the person is told when the call has no typed secret to keep.
const NOTHING_TYPED: &str = "Nothing was typed on the card, so nothing was kept";

/// Answer `op` for Passwords, or `None` when it is not one of the vault's.
pub(crate) fn call(
    device: &dyn passwords_engine::Device,
    op: &str,
    input: &str,
) -> Option<Result<String, String>> {
    Some(match op {
        "vault-keep-typed" => keep_typed(input),
        "vault-forget-field" => forget_field(input),
        "vault-absorb-device" => absorb_device(device),
        "vault-preview-csv" => preview_csv(device, input),
        "vault-import-csv" => import_csv(device, input),
        "vault-export-csv" => export_csv(device),
        "vault-export-backup" => export_backup(device),
        "vault-restore-backup" => restore_backup(device, input),
        _ => return None,
    })
}

/// Ask this device's vault to run `action` with `arguments`.
fn invoke(action: &str, arguments: &Map<String, Value>) -> Result<(), String> {
    collaboration::SessionResource::invoke_without_watching(VAULT, action, arguments)
}

/// The JSON `input` reads as, saying what it was for when it does not.
fn read<T: for<'de> Deserialize<'de>>(input: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|_| format!("{what} did not read"))
}

/// A fresh entry id: 32 lowercase hexadecimal digits.
fn fresh_id() -> String {
    passwords_engine::SecretId::new().0
}

/// Keep what the person typed as one secret of an entry: `{"id", "part"}`, the first take of the
/// call they approved.
fn keep_typed(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        id: String,
        part: String,
    }
    let ask: Ask = read(input, "the entry to keep it in")?;
    tilolang_tilo::page::with_typed(|typed| {
        let secret = typed.first().ok_or(NOTHING_TYPED)?;
        let mut arguments = Map::new();
        arguments.insert("id".into(), ask.id.clone().into());
        arguments.insert("part".into(), ask.part.clone().into());
        arguments.insert("secret".into(), (*secret).into());
        invoke(KEEP_SECRET, &arguments)
    })?;
    Ok("{}".to_owned())
}

/// Take a field off an entry, `{"id", "part": "field:<label>"}`: an empty secret takes it away.
fn forget_field(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        id: String,
        part: String,
    }
    let ask: Ask = read(input, "the field to take away")?;
    if !ask.part.starts_with("field:") {
        return Err("only a field can be taken away".to_owned());
    }
    let mut arguments = Map::new();
    arguments.insert("id".into(), ask.id.into());
    arguments.insert("part".into(), ask.part.into());
    arguments.insert("secret".into(), "".into());
    invoke(KEEP_SECRET, &arguments)?;
    Ok("{}".to_owned())
}

/// Carry one item an engine read (its id, kind, title, name, password, notes, places, tags and
/// fields, as `item_json` writes it) into the vault under `id`.
fn carry(item: &Value, id: &str) -> Result<(), String> {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
    let list = |key: &str| item.get(key).cloned().unwrap_or_else(|| json!([]));
    let fields: Vec<Value> = match item.get("fields") {
        Some(Value::Object(map)) => map
            .iter()
            .map(|(label, value)| json!({"label": label, "value": value}))
            .collect(),
        Some(Value::Array(fields)) => fields.clone(),
        _ => Vec::new(),
    };
    let mut arguments = Map::new();
    arguments.insert("id".into(), id.into());
    arguments.insert("kind".into(), text("kind").replace('-', "_").into());
    arguments.insert("title".into(), text("title").into());
    arguments.insert("username".into(), text("username").into());
    arguments.insert("password".into(), text("password").into());
    arguments.insert("notes".into(), text("notes").into());
    arguments.insert("origins".into(), list("origins"));
    arguments.insert("tags".into(), list("tags"));
    arguments.insert("fields".into(), Value::Array(fields));
    invoke(CARRY, &arguments)
}

/// Carry every entry this device's own older vault holds into the workspace's, and take each
/// out of the device's once the workspace keeps it.
fn absorb_device(device: &dyn passwords_engine::Device) -> Result<String, String> {
    let answer = passwords_engine::call(device, passwords_engine::APP, "device-items", "")?;
    let answer: Value =
        serde_json::from_str(&answer).map_err(|_| "the device vault did not read")?;
    let problem = answer
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !problem.is_empty() {
        return Ok(json!({"carried": 0, "error": problem}).to_string());
    }
    let mut carried = Vec::new();
    for item in answer
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let entry = id.replace('-', "");
        if !passwords_engine::is_entry_id(&entry) {
            continue;
        }
        carry(item, &entry)?;
        carried.push(id.to_owned());
    }
    if !carried.is_empty() {
        passwords_engine::call(
            device,
            passwords_engine::APP,
            "device-remove",
            &json!({"ids": carried}).to_string(),
        )?;
    }
    Ok(json!({"carried": carried.len(), "error": ""}).to_string())
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

/// Bring the rows of a browser's CSV into the vault, each a login, but those the window skips:
/// `{"url", "skip": [index]}`. Answers how many came in.
fn import_csv(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        url: String,
        #[serde(default)]
        skip: Vec<usize>,
    }
    let ask: Ask = read(input, "the rows to import")?;
    let rows = csv_rows(device, &json!({"url": ask.url}).to_string())?;
    let mut imported = 0;
    for (index, row) in rows.iter().enumerate() {
        if ask.skip.contains(&index) {
            continue;
        }
        let origin = row.get("origin").cloned().unwrap_or_default();
        let origins = if origin.as_str().is_some_and(|origin| !origin.is_empty()) {
            json!([origin])
        } else {
            json!([])
        };
        let item = json!({
            "kind": "login",
            "title": row.get("title").cloned().unwrap_or_default(),
            "username": row.get("username").cloned().unwrap_or_default(),
            "password": row.get("password").cloned().unwrap_or_default(),
            "notes": row.get("notes").cloned().unwrap_or_default(),
            "origins": origins,
        });
        carry(&item, &fresh_id())?;
        imported += 1;
    }
    Ok(json!({"imported": imported}).to_string())
}

/// Every entry of the vault as this device publishes it to Passwords, as the engine's items.
fn vault_items() -> Result<Vec<Value>, String> {
    let vault = collaboration::SessionResource::open(VAULT)?;
    let reading = vault
        .latest()
        .ok_or("the vault did not answer")?
        .map_err(|why| why.clone())?;
    let reading: Value = serde_json::from_str(&reading).map_err(|_| "the vault did not read")?;
    Ok(reading
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            let mut item = entry.clone();
            let fields: Map<String, Value> = entry
                .get("fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|field| {
                    Some((
                        field.get("label")?.as_str()?.to_owned(),
                        field.get("value")?.clone(),
                    ))
                })
                .collect();
            item["fields"] = Value::Object(fields);
            item
        })
        .collect())
}

/// The vault as a browser CSV, kept for the window to save where the person chooses.
fn export_csv(device: &dyn passwords_engine::Device) -> Result<String, String> {
    let items = vault_items()?;
    passwords_engine::call(
        device,
        passwords_engine::APP,
        "csv-export",
        &json!({"items": items}).to_string(),
    )
}

/// The vault as a backup under the passphrase the person typed on the card.
fn export_backup(device: &dyn passwords_engine::Device) -> Result<String, String> {
    let items = vault_items()?;
    tilolang_tilo::page::with_typed(|typed| {
        let passphrase = typed.first().ok_or(NOTHING_TYPED)?;
        passwords_engine::call(
            device,
            passwords_engine::APP,
            "backup-export",
            &json!({"items": items, "passphrase": passphrase}).to_string(),
        )
    })
}

/// Bring a backup's entries into the vault with the passphrase the person typed on the card,
/// `{"url"}`: an entry the vault already keeps under its id is left as it is, so restoring twice
/// adds nothing. Answers how many came in.
fn restore_backup(device: &dyn passwords_engine::Device, input: &str) -> Result<String, String> {
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
    let mut restored = 0;
    for item in answer
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .map(|id| id.replace('-', ""))
            .filter(|id| passwords_engine::is_entry_id(id))
            .unwrap_or_else(fresh_id);
        carry(item, &id)?;
        restored += 1;
    }
    Ok(json!({"restored": restored}).to_string())
}
