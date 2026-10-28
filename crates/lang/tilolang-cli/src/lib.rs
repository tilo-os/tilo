//! The tilolang tool: what `tilolang build`, `tilolang run` and `tilolang dev` do. Every other
//! command is the compiler's ([`tilolang::cli`]).

pub mod dev;
pub mod package;
pub mod run;
mod session;

use std::path::{Path, PathBuf};

use tilo_app_package::pack::SigningIdentity;

/// A publisher identity file: `{ "secret_key_hex": "<64 hex>" }`, the shape `tilo-app-package`
/// and `tilo-runtime-bundle` read.
#[derive(serde::Serialize, serde::Deserialize)]
struct IdentityFile {
    secret_key_hex: String,
}

/// The signing identity in `path`.
///
/// # Errors
/// The file is missing or not an identity file.
pub fn load_identity(path: &Path) -> Result<SigningIdentity, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: IdentityFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw = hex::decode(file.secret_key_hex.trim())
        .map_err(|e| format!("{}: secret key: {e}", path.display()))?;
    let seed: [u8; 32] = raw
        .as_slice()
        .try_into()
        .map_err(|_| format!("{}: the secret key is not 32 bytes", path.display()))?;
    Ok(SigningIdentity::from_seed(&seed))
}

/// The identity at `path`, made the first time it is asked for: a development publisher key
/// kept by its owner only.
///
/// # Errors
/// The file cannot be read or written.
pub fn identity_at(path: &Path) -> Result<SigningIdentity, String> {
    if path.exists() {
        return load_identity(path);
    }
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness for a key: {e}"))?;
    let text = serde_json::to_vec(&IdentityFile {
        secret_key_hex: hex::encode(seed),
    })
    .map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    write_private(path, &text)?;
    Ok(SigningIdentity::from_seed(&seed))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Where the tool keeps its state: the development key and the nested sessions' state.
#[must_use]
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("tilolang")
}
