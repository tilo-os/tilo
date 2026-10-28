//! A person's system-volume keyslot and the `tilo-signin` token that mirrors
//! their record into the LUKS2 header, where the boot can read it before the
//! root is open; and the device's `tilo-comforts` token, which carries the
//! login screens' comforts to the boot (docs/security/sign-in.md §3.4).
//!
//! Nothing here removes or rewrites a keyslot or token it did not create: the
//! recovery key, the old disk passphrase and tilo-rollback's slots stay.

use crate::record::{Factor, Record, SecretKind};
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
    // A LUKS2 token names the keyslots it opens; this one opens none.
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

/// Import `json` over token `existing`, or into a free token id.
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use tilo_rollback::exec::Output;

    /// A stand-in for cryptsetup over a LUKS2 header's token area.
    struct Header {
        tokens: RefCell<BTreeMap<u32, serde_json::Value>>,
    }

    impl Header {
        fn with(tokens: &[(u32, serde_json::Value)]) -> Self {
            Self {
                tokens: RefCell::new(tokens.iter().cloned().collect()),
            }
        }

        fn token(&self, id: u32) -> Option<serde_json::Value> {
            self.tokens.borrow().get(&id).cloned()
        }
    }

    impl Runner for Header {
        fn run(&self, program: &str, args: &[&OsStr], _stdin: Option<&[u8]>) -> Result<Output> {
            if program != "cryptsetup" {
                bail!("no {program} here");
            }
            let args: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
            let flag = |name: &str| {
                args.iter()
                    .position(|a| *a == name)
                    .and_then(|i| args.get(i + 1).copied())
            };
            let token_id = || flag("--token-id").and_then(|id| id.parse::<u32>().ok());
            let (code, stdout) = match args.as_slice() {
                ["luksDump", "--dump-json-metadata", _] => {
                    let tokens: serde_json::Map<String, serde_json::Value> = self
                        .tokens
                        .borrow()
                        .iter()
                        .map(|(id, token)| (id.to_string(), token.clone()))
                        .collect();
                    (0, serde_json::json!({ "tokens": tokens }).to_string())
                }
                ["token", "export", ..] => {
                    let found = token_id().and_then(|id| self.token(id));
                    found.map_or((1, String::new()), |t| (0, t.to_string()))
                }
                ["token", "import", ..] => {
                    let id = token_id().context("an import names its token id")?;
                    let replace = args.contains(&"--token-replace");
                    if self.tokens.borrow().contains_key(&id) && !replace {
                        (1, String::new())
                    } else {
                        let json = std::fs::read(flag("--json-file").context("a json file")?)?;
                        self.tokens
                            .borrow_mut()
                            .insert(id, serde_json::from_slice(&json)?);
                        (0, String::new())
                    }
                }
                other => bail!("no {other:?} here"),
            };
            Ok(Output {
                code,
                stdout: stdout.into_bytes(),
                stderr: String::new(),
            })
        }
    }

    fn scratch() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        (dir, ws)
    }

    #[test]
    fn the_comforts_token_is_written_beside_others_and_rewritten_in_place() {
        let tpm2 = serde_json::json!({"type": "systemd-tpm2", "keyslots": ["1"]});
        let signin = serde_json::json!({"type": "tilo-signin", "keyslots": ["2"], "uid": 1000});
        let header = Header::with(&[(0, tpm2.clone()), (1, signin.clone())]);
        let (_dir, ws) = scratch();
        let device = Path::new("/dev/vda3");
        assert_eq!(comforts(&header, device).unwrap(), None);

        let on = SignInComforts {
            screen_reader: true,
            screen_reader_rate: 150,
            ..SignInComforts::default()
        };
        assert_eq!(write_comforts(&header, &ws, device, &on).unwrap(), 2);
        assert_eq!(
            header.token(2).unwrap(),
            serde_json::json!({
                "type": "tilo-comforts",
                "keyslots": [],
                "screen_reader": true,
                "screen_reader_rate": 150,
                "screen_reader_voice": "system",
                "text_scale": 1.0,
                "high_contrast": false,
            })
        );
        assert_eq!(comforts(&header, device).unwrap(), Some(on));

        let larger = SignInComforts {
            text_scale: 1.5,
            ..SignInComforts::default()
        };
        assert_eq!(write_comforts(&header, &ws, device, &larger).unwrap(), 2);
        assert_eq!(comforts(&header, device).unwrap(), Some(larger));
        assert_eq!(header.token(0), Some(tpm2));
        assert_eq!(header.token(1), Some(signin));
        assert_eq!(header.tokens.borrow().len(), 3);
    }

    #[test]
    fn a_comforts_token_someone_else_wrote_is_read_clamped() {
        let header = Header::with(&[
            (
                3,
                serde_json::json!({
                    "type": "tilo-comforts",
                    "keyslots": [],
                    "screen_reader": true,
                    "screen_reader_rate": 900,
                    "text_scale": "huge",
                    "language": "de-DE\u{202e}",
                    "xkb_layout": "us,ru",
                }),
            ),
            (
                7,
                serde_json::json!({"type": "tilo-comforts", "keyslots": [], "high_contrast": true}),
            ),
        ]);
        assert_eq!(
            comforts(&header, Path::new("/dev/vda3")).unwrap(),
            Some(SignInComforts {
                screen_reader: true,
                screen_reader_rate: 200,
                ..SignInComforts::default()
            })
        );
    }

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
