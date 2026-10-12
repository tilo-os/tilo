//! The LUKS2 volume: reading the token, checking and adding the sibling slot.

use crate::exec;
use crate::token::{self, TOKEN_TYPE, Token};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::Path;

/// The volume's LUKS2 metadata as JSON.
///
/// # Errors
/// When the device is not a LUKS2 volume cryptsetup can read.
pub fn metadata(device: &Path) -> Result<String> {
    let out = exec::run(
        "cryptsetup",
        [
            "luksDump".as_ref(),
            "--dump-json-metadata".as_ref(),
            device.as_os_str(),
        ],
        None,
    )?;
    String::from_utf8(out.to_vec()).context("cryptsetup printed metadata that is not UTF-8")
}

/// The volume's sibling-unlock token, if it has one.
///
/// # Errors
/// When the metadata cannot be read or the token is malformed.
pub fn token(device: &Path) -> Result<Option<Token>> {
    token::from_metadata(&metadata(device)?)
}

/// The volume's UUID.
///
/// # Errors
/// When cryptsetup cannot read it.
pub fn uuid(device: &Path) -> Result<String> {
    let out = exec::run(
        "cryptsetup",
        ["luksUUID".as_ref(), device.as_os_str()],
        None,
    )?;
    let uuid = String::from_utf8_lossy(&out).trim().to_owned();
    if uuid.is_empty() {
        bail!("{} has no LUKS UUID", device.display());
    }
    Ok(uuid)
}

/// The volume's UUID as its 16 bytes.
///
/// # Errors
/// When cryptsetup cannot read it or it is not a UUID.
pub fn uuid_bytes(device: &Path) -> Result<[u8; 16]> {
    let text = uuid(device)?;
    let digits: String = text.chars().filter(|c| *c != '-').collect();
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(&digits, &mut bytes).with_context(|| format!("{text:?} is not a UUID"))?;
    Ok(bytes)
}

/// Whether `passphrase` opens `keyslot` of the volume. Checked before
/// answering the prompt, so a stale half never costs the person a try.
pub fn opens(device: &Path, keyslot: u32, passphrase: &str) -> bool {
    exec::check(
        "cryptsetup",
        [
            "open".as_ref(),
            "--test-passphrase".as_ref(),
            "--key-slot".as_ref(),
            keyslot.to_string().as_ref(),
            "--key-file=-".as_ref(),
            device.as_os_str(),
        ],
        Some(passphrase.as_bytes()),
    )
    .is_ok()
}

fn slots(metadata: &str) -> Result<BTreeSet<u32>> {
    let value: serde_json::Value = serde_json::from_str(metadata).context("LUKS2 metadata")?;
    Ok(value
        .get("keyslots")
        .and_then(serde_json::Value::as_object)
        .map(|slots| slots.keys().filter_map(|slot| slot.parse().ok()).collect())
        .unwrap_or_default())
}

/// Adds a key slot opened by `passphrase`, authorised by the existing secret in
/// `authorise` (a key file, or `None` to let cryptsetup ask on the terminal),
/// and returns its number.
///
/// The passphrase is 256 bits of derived key, so the slot uses the cheapest
/// PBKDF: stretching adds nothing to a key that is not guessable, and early
/// boot tries it in milliseconds.
///
/// # Errors
/// When cryptsetup refuses, or the new slot cannot be identified.
pub fn add_slot(device: &Path, authorise: Option<&Path>, passphrase: &str) -> Result<u32> {
    let before = slots(&metadata(device)?)?;
    let dir = tempfile::Builder::new()
        .prefix("tilo-sibling-unlock-")
        .tempdir_in("/run")
        .or_else(|_| tempfile::tempdir())
        .context("no private scratch directory")?;
    let new_key = dir.path().join("new.key");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&new_key)?;
        file.write_all(passphrase.as_bytes())?;
    }
    let mut args: Vec<std::ffi::OsString> = vec![
        "luksAddKey".into(),
        "--batch-mode".into(),
        "--pbkdf".into(),
        "pbkdf2".into(),
        "--pbkdf-force-iterations".into(),
        "1000".into(),
        "--hash".into(),
        "sha512".into(),
    ];
    if let Some(key) = authorise {
        args.push("--key-file".into());
        args.push(key.into());
    }
    args.push(device.into());
    args.push(new_key.clone().into());
    let added = exec::check("cryptsetup", &args, None);
    let _ = std::fs::write(&new_key, vec![0_u8; passphrase.len()]);
    added.context("cryptsetup could not add the sibling key slot")?;
    let after = slots(&metadata(device)?)?;
    let mut new: Vec<u32> = after.difference(&before).copied().collect();
    match (new.pop(), new.is_empty()) {
        (Some(slot), true) => Ok(slot),
        _ => bail!("could not tell which key slot cryptsetup added"),
    }
}

/// Removes `keyslot` (and with it any token bound only to it).
///
/// # Errors
/// When cryptsetup refuses.
pub fn kill_slot(device: &Path, keyslot: u32, authorise: Option<&Path>) -> Result<()> {
    let mut args: Vec<std::ffi::OsString> = vec!["luksKillSlot".into(), "--batch-mode".into()];
    if let Some(key) = authorise {
        args.push("--key-file".into());
        args.push(key.into());
    }
    args.push(device.into());
    args.push(keyslot.to_string().into());
    exec::check("cryptsetup", &args, None)
}

/// Removes the sibling-unlock token, if there is one.
///
/// # Errors
/// When cryptsetup refuses.
pub fn remove_token(device: &Path) -> Result<()> {
    if let Some(existing) = token_id(&metadata(device)?)? {
        exec::check(
            "cryptsetup",
            [
                "token".as_ref(),
                "remove".as_ref(),
                "--token-id".as_ref(),
                existing.as_ref(),
                device.as_os_str(),
            ],
            None,
        )?;
    }
    Ok(())
}

/// Writes `token` into the header, replacing an earlier sibling-unlock token.
///
/// # Errors
/// When cryptsetup refuses the token.
pub fn import_token(device: &Path, token: &Token) -> Result<()> {
    let json = zeroize::Zeroizing::new(serde_json::to_vec(token)?);
    // An earlier token is replaced in one header write, so a failure leaves
    // the old one in place rather than none.
    let existing = token_id(&metadata(device)?)?;
    let mut args: Vec<&std::ffi::OsStr> = vec!["token".as_ref(), "import".as_ref()];
    if let Some(id) = &existing {
        args.push("--token-replace".as_ref());
        args.push("--token-id".as_ref());
        args.push(id.as_ref());
    }
    args.push(device.as_os_str());
    exec::check("cryptsetup", &args, Some(&json))
        .context("cryptsetup could not store the sibling-unlock token")
}

fn token_id(metadata: &str) -> Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(metadata).context("LUKS2 metadata")?;
    Ok(value
        .get("tokens")
        .and_then(serde_json::Value::as_object)
        .and_then(|tokens| {
            tokens.iter().find_map(|(id, token)| {
                (token.get("type").and_then(serde_json::Value::as_str) == Some(TOKEN_TYPE))
                    .then(|| id.clone())
            })
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_slots_and_the_token_id() {
        let metadata = serde_json::json!({
            "keyslots": { "0": {}, "1": {}, "3": {} },
            "tokens": { "0": { "type": "systemd-recovery" }, "2": { "type": TOKEN_TYPE } },
        })
        .to_string();
        assert_eq!(slots(&metadata).unwrap(), BTreeSet::from([0, 1, 3]));
        assert_eq!(token_id(&metadata).unwrap().as_deref(), Some("2"));
        assert_eq!(token_id(r#"{"tokens":{}}"#).unwrap(), None);
    }
}
