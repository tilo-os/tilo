//! Public ASAR fingerprint: exact pinned contents, only official desktopName patch.
use super::SourceError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
const FINGERPRINTS: [&str; 2] = [
    "9bd8d52778c1c002c744e5eebf0e9da8e7602efa1e6830d92986466481c77ee0",
    "2251c20d7a5c9ec342f27b16c0ada6e365d7cb895b59218e0f7cf08ccd7d38a1",
];
fn unsupported() -> SourceError {
    SourceError::UnsupportedEncryption
}
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        value => value,
    }
}
pub(super) fn verify(bytes: &[u8]) -> Result<Vec<(String, u64, String)>, SourceError> {
    if bytes.len() < 16 || bytes.len() > 16 * 1024 * 1024 {
        return Err(unsupported());
    }
    let header = usize::try_from(u32::from_le_bytes(
        bytes[4..8].try_into().map_err(|_| unsupported())?,
    ))
    .map_err(|_| unsupported())?;
    let json = usize::try_from(u32::from_le_bytes(
        bytes[12..16].try_into().map_err(|_| unsupported())?,
    ))
    .map_err(|_| unsupported())?;
    if header > 512 * 1024
        || json > header
        || json.checked_add(16).is_none_or(|end| end > bytes.len())
    {
        return Err(unsupported());
    }
    let payload = header
        .checked_add(8)
        .filter(|offset| *offset <= bytes.len())
        .ok_or_else(unsupported)?;
    let root: Value = serde_json::from_slice(&bytes[16..16 + json]).map_err(|_| unsupported())?;
    let mut rows = Vec::new();
    walk(&root, "", bytes, payload, &mut rows, 0)?;
    if rows.len() != 792 {
        return Err(unsupported());
    }
    let encoded = serde_json::to_vec(&rows).map_err(|_| unsupported())?;
    if !FINGERPRINTS.contains(&hex::encode(Sha256::digest(encoded)).as_str()) {
        return Err(unsupported());
    }
    rows.into_iter()
        .filter(|row| row[1] == true)
        .map(|row| {
            Ok((
                row[0].as_str().ok_or_else(unsupported)?.to_owned(),
                row[3].as_u64().ok_or_else(unsupported)?,
                row[4].as_str().ok_or_else(unsupported)?.to_owned(),
            ))
        })
        .collect()
}
fn walk(
    node: &Value,
    prefix: &str,
    bytes: &[u8],
    payload: usize,
    rows: &mut Vec<Value>,
    depth: usize,
) -> Result<(), SourceError> {
    if depth > 32 || rows.len() > 792 {
        return Err(unsupported());
    }
    let directory = node
        .as_object()
        .filter(|map| map.len() == 1)
        .and_then(|map| map.get("files"))
        .and_then(Value::as_object)
        .ok_or_else(unsupported)?;
    let ordered: BTreeMap<_, _> = directory.iter().collect();
    for (name, entry) in ordered {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            return Err(unsupported());
        }
        let path = format!("{prefix}{name}");
        if path.len() > 4096 {
            return Err(unsupported());
        }
        if entry.get("files").is_some() {
            walk(entry, &format!("{path}/"), bytes, payload, rows, depth + 1)?;
            continue;
        }
        let map = entry.as_object().ok_or_else(unsupported)?;
        if map.keys().any(|key| {
            !matches!(
                key.as_str(),
                "size" | "offset" | "unpacked" | "executable" | "integrity"
            )
        }) {
            return Err(unsupported());
        }
        let mut size = entry
            .get("size")
            .and_then(Value::as_u64)
            .filter(|n| *n <= 16 * 1024 * 1024)
            .ok_or_else(unsupported)?;
        let unpacked = entry
            .get("unpacked")
            .map_or(Ok(false), |v| v.as_bool().ok_or_else(unsupported))?;
        let _executable = entry
            .get("executable")
            .map_or(Ok(false), |v| v.as_bool().ok_or_else(unsupported))?;
        let hash = if unpacked {
            if entry.get("offset").is_some() {
                return Err(unsupported());
            }
            entry
                .get("integrity")
                .and_then(|v| v.get("hash"))
                .and_then(Value::as_str)
                .ok_or_else(unsupported)?
                .to_owned()
        } else {
            let offset = entry
                .get("offset")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(unsupported)?;
            let start = payload.checked_add(offset).ok_or_else(unsupported)?;
            let end = start
                .checked_add(usize::try_from(size).map_err(|_| unsupported())?)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(unsupported)?;
            let raw = bytes.get(start..end).ok_or_else(unsupported)?;
            if path == "package.json" {
                let mut package: Value = serde_json::from_slice(raw).map_err(|_| unsupported())?;
                let map = package.as_object_mut().ok_or_else(unsupported)?;
                if let Some(desktop) = map.remove("desktopName") {
                    if desktop.as_str() != Some("com.slack.Slack.desktop") {
                        return Err(unsupported());
                    }
                }
                let canonical = serde_json::to_vec(&sorted(package)).map_err(|_| unsupported())?;
                size = canonical.len() as u64;
                hex::encode(Sha256::digest(canonical))
            } else {
                hex::encode(Sha256::digest(raw))
            }
        };
        rows.push(serde_json::json!([path, unpacked, false, size, hash]));
        if rows.len() > 792 {
            return Err(unsupported());
        }
    }
    Ok(())
}
