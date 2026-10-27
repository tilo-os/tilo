//! A person's system-volume keyslot and the `tilo-signin` token that mirrors
//! their record into the LUKS2 header, where the boot can read it before the
//! root is open.
//!
//! Nothing here removes or rewrites a keyslot or token it did not create: the
//! recovery key, the old disk passphrase and tilo-rollback's slots stay.

use crate::record::{Factor, Record, SecretKind};
use crate::wrapper::{Entry, Kind, Wrapper};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::Path;
use tilo_rollback::argv;
use tilo_rollback::exec::{Runner, require};
use tilo_rollback::keyslots::{free_token_id, keyslots_in_use, verify_passphrase};
use tilo_rollback::policy::Workspace;

pub const TOKEN_TYPE: &str = "tilo-signin";

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

/// The header's JSON metadata, read in one call.
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
    let id = match existing {
        Some(id) => id,
        None => free_token_id(runner, device)?,
    };
    let json = ws.put(
        "signin-token.json",
        &serde_json::to_vec(token).context("encoding the sign-in token")?,
    )?;
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
    let result = require(runner, "cryptsetup", &args, "writing the sign-in token");
    let _ = std::fs::remove_file(&json);
    result?;
    Ok(id)
}

/// What authorises adding a keyslot.
pub enum Authority<'a> {
    /// A passphrase one of the existing keyslots accepts.
    Passphrase(&'a [u8]),
    /// The volume key itself.
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
        let undo = ws.put("signin-undo.key", passphrase)?;
        let slot_s = slot.to_string();
        let _ = runner.run(
            "cryptsetup",
            &argv![
                "luksKillSlot",
                "--batch-mode",
                "--key-file",
                undo.as_os_str(),
                device.as_os_str(),
                &slot_s,
            ],
            None,
        );
        let _ = std::fs::remove_file(&undo);
        bail!(
            "the new keyslot {slot} does not open {}; removed it again",
            device.display()
        );
    }
    Ok(slot)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_mirrors_the_record_in_the_contract_shape() {
        let mut record = Record::new(1000, "ada", "Ada Lovelace");
        record.set_secret_wrapper(Wrapper::Tpm {
            public: "cA==".into(),
            private: "cQ==".into(),
        });
        let token = SigninToken::mirror(&record, 4);
        assert_eq!(token.keyslot(), Some(4));
        assert_eq!(token.factors(), [Factor::Password]);
        assert_eq!(
            serde_json::to_value(&token).unwrap(),
            serde_json::json!({
                "type": "tilo-signin",
                "keyslots": ["4"],
                "uid": 1000,
                "name": "ada",
                "display_name": "Ada Lovelace",
                "wrappers": [{"kind": "tpm", "public": "cA==", "private": "cQ=="}],
            })
        );
    }
}
