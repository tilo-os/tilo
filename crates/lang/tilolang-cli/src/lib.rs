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
/// kept by its owner only. Several commands asking at once (four `tilolang dev` started
/// together) all get the one key: it is written whole beside `path` and linked into place, so
/// the key file is never seen half written, and a command that finds another's key there takes
/// that one.
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
    let parent = path
        .parent()
        .ok_or_else(|| format!("{}: a key file has a directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let made = tempfile::Builder::new()
        .prefix(".publisher-")
        .tempfile_in(parent)
        .map_err(|e| format!("{}: {e}", parent.display()))?;
    write_private(made.path(), text.as_bytes())?;
    match std::fs::hard_link(made.path(), path) {
        Ok(()) => Ok(SigningIdentity::from_seed(&seed)),
        // Another command made the key first: it is whole, as it was linked whole.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => load_identity(path),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_asking_for_the_key_at_once_all_get_the_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/publisher.json");
        let keys: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || identity_at(&path).map(|key| key.public_key_bytes()))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|asked| asked.join().unwrap().unwrap())
            .collect();
        let first = keys.first().copied().unwrap();
        assert!(keys.iter().all(|key| *key == first), "one key for all");
        assert_eq!(load_identity(&path).unwrap().public_key_bytes(), first);
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["publisher.json"], "nothing half made is left");
    }
}
