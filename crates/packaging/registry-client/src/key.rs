//! Publisher keys on disk: `{"secret_key_hex": "<64 hex digits>"}`, the file
//! `tilo-app-package keygen` and the App Publisher app write. CI passes the
//! same contents, or just the hex, in [`KEY_ENV`].

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use ed25519_dalek::SigningKey;
use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use serde::{Deserialize, Serialize};

/// The environment variable CI puts a publisher key in.
pub const KEY_ENV: &str = "TILO_PUBLISHER_KEY";

#[derive(Serialize, Deserialize)]
struct KeyFile {
    secret_key_hex: String,
}

/// Where a publisher key lives when none is named:
/// `$XDG_CONFIG_HOME/tilo/publisher-key.json`.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    xdg_paths::config_dir().map(|dir| dir.join("tilo").join("publisher-key.json"))
}

/// A key from a key file's contents or from its bare hex seed.
///
/// # Errors
///
/// When `text` is neither.
pub fn parse(text: &str) -> Result<SigningKey> {
    let text = text.trim();
    let hex_seed = if text.starts_with('{') {
        serde_json::from_str::<KeyFile>(text)
            .context("parse publisher key JSON")?
            .secret_key_hex
    } else {
        text.to_owned()
    };
    let seed: [u8; 32] = hex::decode(hex_seed.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| anyhow!("a publisher key is 32 bytes of hex"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The key in `path`.
///
/// # Errors
///
/// When the file cannot be read or holds no key.
pub fn load(path: &Path) -> Result<SigningKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read publisher key {}", path.display()))?;
    parse(&text).with_context(|| format!("publisher key {}", path.display()))
}

/// The key named by `path`, else the one in [`KEY_ENV`], else the one at
/// [`default_path`].
///
/// # Errors
///
/// When the chosen key cannot be read, or there is none.
pub fn resolve(path: Option<&Path>) -> Result<SigningKey> {
    if let Some(path) = path {
        return load(path);
    }
    discover()?.ok_or_else(|| {
        anyhow!(
            "no publisher key: pass --key, set ${KEY_ENV}, or create one with `tilo-app keys new`"
        )
    })
}

/// The key in [`KEY_ENV`], else the one at [`default_path`], or `None` when
/// neither is there. An empty [`KEY_ENV`] counts as unset, which is what a
/// CI secret missing from a pull request expands to.
///
/// # Errors
///
/// When a key is there but cannot be read.
pub fn discover() -> Result<Option<SigningKey>> {
    if let Ok(text) = std::env::var(KEY_ENV)
        && !text.trim().is_empty()
    {
        return parse(&text)
            .with_context(|| format!("${KEY_ENV}"))
            .map(Some);
    }
    match default_path() {
        Some(path) if path.exists() => load(&path).map(Some),
        _ => Ok(None),
    }
}

/// Write a new key to `path`, readable only by its owner. Never replaces a
/// key already there.
///
/// # Errors
///
/// When `path` exists or cannot be written.
pub fn create(path: &Path) -> Result<SigningKey> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let body = serde_json::to_vec_pretty(&KeyFile {
        secret_key_hex: hex::encode(key.to_bytes()),
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).with_context(|| {
        format!(
            "create {}; a publisher key is never replaced",
            path.display()
        )
    })?;
    file.write_all(&body)?;
    file.sync_all()?;
    Ok(key)
}

/// The key's public half, as the registry names publishers.
#[must_use]
pub fn public_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_are_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/key.json");
        let key = create(&path).unwrap();
        assert_eq!(load(&path).unwrap().to_bytes(), key.to_bytes());
        assert_eq!(
            parse(&hex::encode(key.to_bytes())).unwrap().to_bytes(),
            key.to_bytes()
        );
        assert!(create(&path).is_err());
        assert_eq!(load(&path).unwrap().to_bytes(), key.to_bytes());
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
        assert!(parse("not a key").is_err());
    }
}
