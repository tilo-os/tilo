//! The system volume's LUKS2 header, and the one place that reads and writes
//! it with cryptsetup: a person's keyslot and the `tilo-signin` token that
//! mirrors their record, where the boot can read it before the root is open;
//! the device's `tilo-comforts` token, which carries the login screens'
//! comforts to the boot (docs/security/sign-in.md §3.4); and the
//! `tilo-sibling-unlock` token with its keyslot (docs/device-reach.md).

use crate::record::{Factor, Record, SecretKind};
use crate::sibling::token::{self as sibling_token, Token as SiblingToken};
use crate::wrapper::{Entry, Kind, Wrapper};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use settings_store::sign_in_comforts::SignInComforts;
use std::ffi::OsStr;
use std::path::Path;
use tilo_rollback::argv;
use tilo_rollback::exec::{Runner, require};
use tilo_rollback::keyslots::{free_token_id, keyslots_in_use, verify_passphrase};
use tilo_rollback::policy::Workspace;

pub const TOKEN_TYPE: &str = "tilo-signin";
pub const COMFORTS_TOKEN_TYPE: &str = "tilo-comforts";

/// The cost of a keyslot whose passphrase is 256 random bits: none to speak
/// of, as for systemd-cryptenroll's TPM and FIDO2 slots. A memory-hard KDF
/// protects guessable passphrases; here it would only add seconds to every
/// boot, twice (the check before writing the key file, then the open).
const SLOT_PBKDF: [&str; 4] = ["--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "1000"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SigninToken {
    #[serde(rename = "type")]
    pub token_type: String,
    pub keyslots: Vec<String>,
    pub uid: u32,
    pub name: String,
    pub display_name: String,
    pub wrappers: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretKind>,
}

impl SigninToken {
    pub fn mirror(record: &Record, keyslot: u32) -> Self {
        Self {
            token_type: TOKEN_TYPE.to_owned(),
            keyslots: vec![keyslot.to_string()],
            uid: record.uid,
            name: record.name.clone(),
            display_name: record.display_name.clone(),
            wrappers: record.wrappers.clone(),
            secret: record.secret,
        }
    }

    pub fn keyslot(&self) -> Option<u32> {
        match self.keyslots.as_slice() {
            [one] => one.parse().ok(),
            _ => None,
        }
    }

    pub fn known(&self) -> impl Iterator<Item = &Wrapper> {
        self.wrappers.iter().filter_map(Entry::known)
    }

    pub fn factors(&self) -> Vec<Factor> {
        let mut factors = Vec::new();
        if self
            .known()
            .any(|w| matches!(w.kind(), Kind::Tpm | Kind::Passphrase))
        {
            factors.push(Factor::Password);
        }
        if self.known().any(|w| w.kind() == Kind::Recovery) {
            factors.push(Factor::Recovery);
        }
        factors
    }
}

fn metadata(runner: &dyn Runner, device: &Path) -> Result<serde_json::Value> {
    let out = require(
        runner,
        "cryptsetup",
        &argv!["luksDump", "--dump-json-metadata", device.as_os_str()],
        "reading the LUKS2 header",
    )?;
    serde_json::from_slice(&out.stdout).context("parsing the LUKS2 header metadata")
}

/// Every `tilo-signin` token on `device`, by token id. A malformed one is
/// reported and skipped so it cannot stop anyone else from signing in.
pub fn tokens(runner: &dyn Runner, device: &Path) -> Result<Vec<(u32, SigninToken)>> {
    let meta = metadata(runner, device)?;
    let mut found = Vec::new();
    let Some(tokens) = meta.get("tokens").and_then(serde_json::Value::as_object) else {
        return Ok(found);
    };
    for (id, token) in tokens {
        if token.get("type").and_then(serde_json::Value::as_str) != Some(TOKEN_TYPE) {
            continue;
        }
        let Ok(id) = id.parse::<u32>() else { continue };
        match serde_json::from_value::<SigninToken>(token.clone()) {
            Ok(t) => found.push((id, t)),
            Err(e) => eprintln!(
                "tilo-auth: ignoring sign-in token {id} on {}: {e}",
                device.display()
            ),
        }
    }
    found.sort_by_key(|(_, t)| t.uid);
    Ok(found)
}

/// The volume's sibling-unlock token; `None` when it has no sibling factor.
///
/// # Errors
///
/// The header cannot be read, or carries a malformed or more than one
/// sibling-unlock token.
pub fn sibling(runner: &dyn Runner, device: &Path) -> Result<Option<SiblingToken>> {
    Ok(sibling_token::find(&metadata(runner, device)?)?.map(|(_, token)| token))
}

/// Write the sibling-unlock token, replacing the one there is in one header
/// write, so a failure leaves the old one in place rather than none.
pub fn write_sibling(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    token: &SiblingToken,
) -> Result<u32> {
    let existing = sibling_token::find(&metadata(runner, device)?)?.map(|(id, _)| id);
    let json = zeroize::Zeroizing::new(
        serde_json::to_vec(token).context("encoding the sibling-unlock token")?,
    );
    import(
        runner,
        ws,
        device,
        existing,
        &json,
        "writing the sibling-unlock token",
    )
}

/// Remove the sibling-unlock token, if there is one.
pub fn remove_sibling(runner: &dyn Runner, device: &Path) -> Result<()> {
    let Some((id, _)) = sibling_token::find(&metadata(runner, device)?)? else {
        return Ok(());
    };
    let id = id.to_string();
    require(
        runner,
        "cryptsetup",
        &argv!["token", "remove", "--token-id", &id, device.as_os_str()],
        "removing the sibling-unlock token",
    )?;
    Ok(())
}

/// The volume's UUID as its 16 bytes.
pub fn uuid(runner: &dyn Runner, device: &Path) -> Result<[u8; 16]> {
    let out = require(
        runner,
        "cryptsetup",
        &argv!["luksUUID", device.as_os_str()],
        "reading the volume's UUID",
    )?;
    let text = out.stdout_utf8();
    let digits: String = text.trim().chars().filter(|c| *c != '-').collect();
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(&digits, &mut bytes)
        .with_context(|| format!("{:?} is not a UUID", text.trim()))?;
    Ok(bytes)
}

/// Write `token`, replacing the person's existing `tilo-signin` token if
/// there is one. Returns the token id.
pub fn write_token(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    token: &SigninToken,
) -> Result<u32> {
    let existing = tokens(runner, device)?
        .into_iter()
        .find(|(_, t)| t.uid == token.uid)
        .map(|(id, _)| id);
    import(
        runner,
        ws,
        device,
        existing,
        &serde_json::to_vec(token).context("encoding the sign-in token")?,
        "writing the sign-in token",
    )
}

/// The device's sign-in comforts from its `tilo-comforts` token; `None` when
/// it has none. Anyone holding the disk can write the header, so the token
/// reads field by field with every value clamped
/// ([`SignInComforts::from_map`]); of several, the lowest id counts.
pub fn comforts(runner: &dyn Runner, device: &Path) -> Result<Option<SignInComforts>> {
    Ok(comforts_token(runner, device)?.map(|(_, comforts)| comforts))
}

fn comforts_token(runner: &dyn Runner, device: &Path) -> Result<Option<(u32, SignInComforts)>> {
    let meta = metadata(runner, device)?;
    let Some(tokens) = meta.get("tokens").and_then(serde_json::Value::as_object) else {
        return Ok(None);
    };
    Ok(tokens
        .iter()
        .filter_map(|(id, token)| {
            let token = token.as_object()?;
            if token.get("type").and_then(serde_json::Value::as_str) != Some(COMFORTS_TOKEN_TYPE) {
                return None;
            }
            Some((id.parse::<u32>().ok()?, SignInComforts::from_map(token)))
        })
        .min_by_key(|(id, _)| *id))
}

/// Write the device's `tilo-comforts` token, replacing the one there is.
/// Returns the token id.
pub fn write_comforts(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    comforts: &SignInComforts,
) -> Result<u32> {
    let existing = comforts_token(runner, device)?.map(|(id, _)| id);
    let serde_json::Value::Object(mut token) =
        serde_json::to_value(comforts).context("encoding the comforts token")?
    else {
        bail!("the sign-in comforts did not encode as an object");
    };
    token.insert("type".into(), COMFORTS_TOKEN_TYPE.into());
    token.insert("keyslots".into(), serde_json::Value::Array(Vec::new()));
    import(
        runner,
        ws,
        device,
        existing,
        &serde_json::to_vec(&token).context("encoding the comforts token")?,
        "writing the comforts token",
    )
}

fn import(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    existing: Option<u32>,
    json: &[u8],
    what: &str,
) -> Result<u32> {
    let id = match existing {
        Some(id) => id,
        None => free_token_id(runner, device)?,
    };
    let json = ws.put("signin-token.json", json)?;
    let id_s = id.to_string();
    let mut args = argv![
        "token",
        "import",
        "--token-id",
        &id_s,
        "--json-file",
        json.as_os_str(),
    ];
    if existing.is_some() {
        args.push(OsStr::new("--token-replace"));
    }
    args.push(device.as_os_str());
    let result = require(runner, "cryptsetup", &args, what);
    let _ = std::fs::remove_file(&json);
    result?;
    Ok(id)
}

pub enum Authority<'a> {
    Passphrase(&'a [u8]),
    VolumeKey(&'a [u8]),
}

/// Add a keyslot for `passphrase` and prove it opens the volume before
/// returning; a slot that does not is removed again.
pub fn add_slot(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    authority: &Authority<'_>,
    passphrase: &[u8],
) -> Result<u32> {
    let before = keyslots_in_use(runner, device)?;
    let new_file = ws.put("signin-slot.key", passphrase)?;
    let (flag, auth_file) = match authority {
        Authority::Passphrase(p) => ("--key-file", ws.put("signin-auth.key", p)?),
        Authority::VolumeKey(k) => ("--volume-key-file", ws.put("signin-auth.vk", k)?),
    };
    let key_bits = match authority {
        Authority::VolumeKey(k) => Some(k.len().saturating_mul(8).to_string()),
        Authority::Passphrase(_) => None,
    };
    let mut args = argv!["luksAddKey", "--batch-mode", flag, auth_file.as_os_str()];
    if let Some(bits) = &key_bits {
        args.push(OsStr::new("--key-size"));
        args.push(OsStr::new(bits));
    }
    args.extend(SLOT_PBKDF.iter().map(OsStr::new));
    args.push(device.as_os_str());
    args.push(new_file.as_os_str());
    let result = require(runner, "cryptsetup", &args, "adding the sign-in keyslot");
    let _ = std::fs::remove_file(&auth_file);
    let _ = std::fs::remove_file(&new_file);
    result?;

    let added: Vec<u32> = keyslots_in_use(runner, device)?
        .into_iter()
        .filter(|s| !before.contains(s))
        .collect();
    let [slot] = added.as_slice() else {
        bail!(
            "expected exactly one new keyslot on {}, found {added:?}; not guessing which is the \
             sign-in slot",
            device.display()
        );
    };
    let slot = *slot;
    if !verify_passphrase(runner, ws, device, passphrase, Some(slot))? {
        let _ = kill_slot(runner, ws, device, slot, Some(passphrase));
        bail!(
            "the new keyslot {slot} does not open {}; removed it again",
            device.display()
        );
    }
    Ok(slot)
}

/// Remove `slot`, with a passphrase of the volume where the caller has one.
pub fn kill_slot(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    slot: u32,
    passphrase: Option<&[u8]>,
) -> Result<()> {
    let auth_file = passphrase.map(|p| ws.put("kill-auth.key", p)).transpose()?;
    let slot_s = slot.to_string();
    let mut args = argv!["luksKillSlot", "--batch-mode"];
    if let Some(file) = &auth_file {
        args.push(OsStr::new("--key-file"));
        args.push(file.as_os_str());
    }
    args.push(device.as_os_str());
    args.push(OsStr::new(&slot_s));
    let result = require(runner, "cryptsetup", &args, "removing a keyslot");
    if let Some(file) = &auth_file {
        let _ = std::fs::remove_file(file);
    }
    result.map(drop)
}

pub fn opens(
    runner: &dyn Runner,
    ws: &Workspace,
    device: &Path,
    passphrase: &[u8],
    slot: u32,
) -> Result<bool> {
    verify_passphrase(runner, ws, device, passphrase, Some(slot))
}
