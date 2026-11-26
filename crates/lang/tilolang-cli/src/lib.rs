//! The tilolang tool: what `tilolang build`, `tilolang run` and `tilolang dev` do. Every other
//! command is the compiler's ([`tilolang::cli`]).

pub mod describe;
pub mod dev;
pub mod fonts;
pub mod layout_check;
pub mod package;
pub mod profile;
pub mod repl;
pub mod run;
mod session;

use std::path::{Path, PathBuf};

use tilo_app_package::pack::SigningIdentity;

pub use tilolang_tilo::platform::{compile, compile_for};

/// Run in a checkout, the tool compiles against the checkout's standard library, as it does
/// against the packages beside the one it builds: an edit of it takes effect without rebuilding
/// the tool. Elsewhere it compiles against the one it was built with.
///
/// # Errors
/// The checkout's standard library does not load.
pub fn load_checkout_stdlib() -> Result<(), String> {
    let tree = std::env::current_dir()
        .ok()
        .and_then(|dir| tilolang::stdlib::in_tree(&dir));
    match tree.map(|dir| tilolang::stdlib::load_from(&dir)) {
        Some(Err(error)) => Err(error),
        _ => Ok(()),
    }
}

/// The package directories [`compile`] reads for `dir`: the ones to watch.
pub fn package_dirs(dir: &Path, search: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let published =
        tilolang_tilo::platform::published_packages(&tilolang::source::search_of(dir, search))?;
    tilolang::source::dependency_order_with(dir, search, &tilolang::source::apps_of(&published))
}

/// The signing identity in the publisher key file at `path` ([`registry_types::key_file`]).
pub fn load_identity(path: &Path) -> Result<SigningIdentity, String> {
    SigningIdentity::from_key_file(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The identity at `path`, made the first time it is asked
/// for: a development publisher key kept by its owner only.
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
    match tilo_files::native::atomic::create_private(path, text.as_bytes()) {
        Ok(()) => Ok(SigningIdentity::from_seed(&seed)),
        // Another command made the key first: it is whole, as it was published whole.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => load_identity(path),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The publisher identity a command signs with when it names no key file: this machine's
/// publisher key (`$TILO_PUBLISHER_KEY`, else the secret store), the one `tilolang keys` creates
/// and `tilolang publish` signs entries with, so a package and its registry entry name one
/// publisher. Only where there is no store, as on a build machine without a secret service, is it
/// the development key in [`state_dir`].
pub fn publisher_identity() -> Result<SigningIdentity, String> {
    use app_registry::publisher::key;
    match key::discover(&key::machine_store()).map_err(|error| format!("{error:#}"))? {
        Some(key) => Ok(SigningIdentity::from_seed(&key.to_bytes())),
        None => identity_at(&state_dir().join("publisher.json")),
    }
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
