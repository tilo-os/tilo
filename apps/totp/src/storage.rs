//! On-disk persistence for the vault.
//!
//! # Encryption-at-rest plan (TODO — currently plaintext)
//!
//! TOTP secrets are long-lived bearer credentials and MUST eventually be
//! encrypted at rest. The intended design, matching qOS's sandbox + key
//! model:
//!
//! 1. **Key custody.** A per-vault symmetric key (XChaCha20-Poly1305,
//!    256-bit) is held by the compositor's keyring service, unlocked at
//!    login. The app never sees the raw key on disk — it requests an
//!    AEAD seal/open over the cluster's Unix-socket capability channel
//!    (the same boundary used for collaboration + permission prompts).
//! 2. **File format.** `vault.json` becomes `vault.age`: an `age`
//!    (X25519 + ChaCha20-Poly1305) encrypted blob whose recipient is the
//!    keyring-held identity. `age` is chosen for its audited, simple
//!    format and the `age` Rust crate.
//! 3. **Memory hygiene.** Decoded secret bytes are zeroized
//!    (`zeroize::Zeroizing`) after each code derivation so they don't
//!    linger in the heap between rotations.
//! 4. **Sync.** The CRDT document that replicates to collaborators would
//!    carry per-field-encrypted secrets so an invited peer of the
//!    *cluster* can't read a vault they weren't explicitly shared.
//!
//! Until that lands, this module writes **plaintext JSON** under the
//! user data dir. It is intentionally best-effort: every IO error is
//! swallowed (logged via `tracing`) so a read-only or missing data dir
//! never crashes the app — the vault still works in-memory for the
//! session and for the `--shots` renderer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Account, Vault};

#[derive(Debug, Serialize, Deserialize, Default)]
struct VaultFile {
    version: u32,
    accounts: Vec<Account>,
}

const VAULT_VERSION: u32 = 1;

/// Resolve the vault file path: `$XDG_DATA_HOME/qos-authenticator/vault.json`
/// (falling back to `$HOME/.local/share/...`). Returns `None` when no
/// home directory is discoverable.
pub fn vault_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("qos-authenticator").join("vault.json"))
}

/// Load the vault from disk. Returns an empty vault when the file is
/// missing or unreadable — never an error to the caller.
pub fn load() -> Vault {
    let Some(path) = vault_path() else {
        return Vault::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<VaultFile>(&s) {
            Ok(file) => {
                let mut v = Vault::new();
                v.accounts = file.accounts;
                v.renumber();
                v
            }
            Err(e) => {
                tracing::warn!(error = %e, "authenticator: vault parse failed; starting empty");
                Vault::new()
            }
        },
        Err(_) => Vault::new(),
    }
}

/// Persist the vault to disk (plaintext for now — see module docs).
/// Best-effort: logs and returns on any IO error.
pub fn save(vault: &Vault) {
    let Some(path) = vault_path() else { return };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, "authenticator: could not create data dir");
            return;
        }
    }
    let file = VaultFile {
        version: VAULT_VERSION,
        accounts: vault.ordered(),
    };
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(e) = atomic_write(&path, json.as_bytes()) {
                tracing::warn!(error = %e, "authenticator: vault write failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, "authenticator: vault serialize failed"),
    }
}

/// Write via a temp file + rename so a crash mid-write can't truncate an
/// existing vault.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Demo-account fallback flag: true when nothing was persisted yet, so
/// the app seeds the demo vault on first run.
pub fn is_first_run() -> bool {
    vault_path().map(|p| !p.exists()).unwrap_or(true)
}

/// Test/inspection helper: deserialize an account list from a JSON blob.
#[doc(hidden)]
pub fn accounts_from_json(json: &str) -> Option<Vec<Account>> {
    serde_json::from_str::<VaultFile>(json).ok().map(|f| f.accounts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::demo_accounts;

    #[test]
    fn vault_file_round_trips_through_json() {
        let mut v = Vault::new();
        for a in demo_accounts() {
            v.upsert(a);
        }
        let file = VaultFile { version: VAULT_VERSION, accounts: v.ordered() };
        let json = serde_json::to_string(&file).unwrap();
        let back = accounts_from_json(&json).unwrap();
        assert_eq!(back.len(), v.len());
        assert_eq!(back[0].issuer, v.ordered()[0].issuer);
    }
}
