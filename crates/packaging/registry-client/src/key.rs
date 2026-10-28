//! Publisher keys. A machine keeps its key in the secret store, where
//! `tilo-app` and the App Publisher app both find it through [`stored`],
//! [`create`] and [`discover`]. Anywhere else — a file a command names, the
//! CI secret in [`KEY_ENV`], a backup — a key is the one key file format,
//! `{"secret_key_hex": "<64 hex digits>"}` ([`registry_types::key_file`]).

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
pub use ed25519_dalek::SigningKey;
use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;
use registry_types::key_file;
use tilo_secrets::{ItemSecret, Secret, SecretError, SecretStore};

/// The environment variable CI puts a publisher key in.
pub const KEY_ENV: &str = "TILO_PUBLISHER_KEY";

/// The app whose secrets hold the publisher key: the App Publisher's. Secret
/// stores are scoped per app, and the key belongs to the app that manages
/// it; `tilo-app` addresses the same app's secret so both see one key.
pub const SECRETS_APP: &str = "so.tilo.publisher";

/// The key's purpose and item in [`SECRETS_APP`]'s store: one key per
/// machine.
const PURPOSE: &str = "publisher-key";
const ITEM: &str = "signing";

/// [`SECRETS_APP`]'s secrets in this machine's secret store, for a caller
/// with no host to hand it the App Publisher's store.
#[must_use]
pub fn machine_store() -> SecretStore {
    SecretStore::for_app(SECRETS_APP)
}

fn secret(store: &SecretStore) -> ItemSecret {
    debug_assert_eq!(
        store.app_id(),
        SECRETS_APP,
        "the publisher key lives in the App Publisher's secrets"
    );
    store.item(PURPOSE, ITEM)
}

/// A key from a key file's contents ([`registry_types::key_file`]).
///
/// # Errors
///
/// When `text` is not a key file holding a 32-byte seed.
pub fn parse(text: &str) -> Result<SigningKey> {
    Ok(key_file::parse(text)?)
}

pub use key_file::encode;

/// The key in the file at `path`.
///
/// # Errors
///
/// When the file cannot be read or holds no key.
pub fn load(path: &Path) -> Result<SigningKey> {
    key_file::load(path).with_context(|| format!("publisher key {}", path.display()))
}

/// The key `store` holds, or `None` when it holds none.
///
/// # Errors
///
/// When the store cannot be read or holds something that is not a key.
pub fn stored(store: &SecretStore) -> Result<Option<SigningKey>> {
    decode(secret(store).get_blocking())
}

fn decode(read: Result<Option<Secret>, SecretError>) -> Result<Option<SigningKey>> {
    let Some(secret) = read.context("read the publisher key from the secret store")? else {
        return Ok(None);
    };
    let seed: [u8; 32] = secret
        .expose()
        .try_into()
        .map_err(|_| anyhow!("the secret store's publisher key is not 32 bytes"))?;
    Ok(Some(SigningKey::from_bytes(&seed)))
}

/// Create a new key in `store`. Never replaces a key already there.
///
/// # Errors
///
/// When `store` already holds a key, or cannot be read or written.
pub fn create(store: &SecretStore) -> Result<SigningKey> {
    if stored(store)?.is_some() {
        bail!("this machine already has a publisher key; a publisher key is never replaced");
    }
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    secret(store)
        .put_blocking(&key.to_bytes())
        .context("store the publisher key in the secret store")?;
    Ok(key)
}

/// The key named by `path`, else the one in [`KEY_ENV`], else the one in
/// `store`.
///
/// # Errors
///
/// When the chosen key cannot be read, or there is none.
pub fn resolve(path: Option<&Path>, store: &SecretStore) -> Result<SigningKey> {
    if let Some(path) = path {
        return load(path);
    }
    discover(store)?.ok_or_else(|| {
        anyhow!(
            "no publisher key: pass --key, set ${KEY_ENV}, or create one with `tilo-app keys new`"
        )
    })
}

/// The key in [`KEY_ENV`], else the one in `store`, or `None` when neither
/// has one. An empty [`KEY_ENV`] counts as unset, which is what a CI secret
/// missing from a pull request expands to; a machine with no secret store
/// (a CI runner) holds no key.
///
/// # Errors
///
/// When a key is there but cannot be read.
pub fn discover(store: &SecretStore) -> Result<Option<SigningKey>> {
    if let Ok(text) = std::env::var(KEY_ENV)
        && !text.trim().is_empty()
    {
        return parse(&text)
            .with_context(|| format!("${KEY_ENV}"))
            .map(Some);
    }
    match secret(store).get_blocking() {
        Err(SecretError::Unavailable(_) | SecretError::Unsupported(_)) => Ok(None),
        read => decode(read),
    }
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
        let store = SecretStore::memory(SECRETS_APP);
        assert!(stored(&store).unwrap().is_none());
        let key = create(&store).unwrap();
        assert_eq!(stored(&store).unwrap().unwrap().to_bytes(), key.to_bytes());
        assert!(create(&store).is_err());
        assert_eq!(stored(&store).unwrap().unwrap().to_bytes(), key.to_bytes());
        assert_eq!(parse(&encode(&key)).unwrap().to_bytes(), key.to_bytes());
        assert!(
            parse(&hex::encode(key.to_bytes())).is_err(),
            "bare hex is no key file"
        );
        assert!(parse("not a key").is_err());
    }

    #[test]
    fn a_named_key_file_wins_over_the_store() {
        let store = SecretStore::memory(SECRETS_APP);
        create(&store).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.json");
        std::fs::write(
            &path,
            format!(r#"{{"secret_key_hex": "{}"}}"#, "07".repeat(32)),
        )
        .unwrap();
        assert_eq!(
            resolve(Some(&path), &store).unwrap().to_bytes(),
            [7; 32],
            "--key names another key"
        );
    }
}
