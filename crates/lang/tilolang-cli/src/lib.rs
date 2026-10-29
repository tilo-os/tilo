//! The tilolang tool: what `tilolang build`, `tilolang run` and `tilolang dev` do. Every other
//! command is the compiler's ([`tilolang::cli`]).

pub mod describe;
pub mod dev;
pub mod layout_check;
pub mod package;
pub mod repl;
pub mod run;
mod session;

use std::path::{Path, PathBuf};

use tilo_app_package::pack::SigningIdentity;

/// The signing identity in the publisher key file at `path` ([`registry_types::key_file`]).
///
/// # Errors
/// The file is missing or not a key file.
pub fn load_identity(path: &Path) -> Result<SigningIdentity, String> {
    registry_types::key_file::load(path)
        .map(SigningIdentity::from_signing_key)
        .map_err(|e| format!("{}: {e}", path.display()))
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
    let text = registry_types::key_file::encode(&ed25519_dalek::SigningKey::from_bytes(&seed));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    write_private(path, text.as_bytes())?;
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
