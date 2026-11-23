//! Reconstruct a signed repository definition from the source's existing trust,
//! without fetching a replacement key or copying source deployment refs.
use super::Remote;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{io::Read, path::Path};

/// The installation root is the reviewed source directory, e.g.
/// `/var/lib/flatpak` or `~/.local/share/flatpak`. Returns `.flatpakrepo` bytes
/// carrying the exact existing remote key. Record and review this definition
/// before passing it to target-side restoration.
pub fn extract_remote_definition(
    remote: &Remote,
    installation_root: &Path,
) -> Result<Vec<u8>, String> {
    let url = reqwest::Url::parse(&remote.url).map_err(|_| "Invalid Flatpak repository URL")?;
    if remote.url.chars().any(char::is_control)
        || !super::token(&remote.name)
        || url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || remote
            .options
            .split(',')
            .any(|option| matches!(option.trim(), "no-gpg-verify" | "oci"))
    {
        return Err("This Flatpak repository needs a new connection on the target".into());
    }
    let repo =
        super::super::migration::PinnedDirectory::open_absolute(&installation_root.join("repo"))
            .map_err(|error| error.to_string())?;
    repo.validate().map_err(|error| error.to_string())?;
    let path = format!("{}.trustedkeys.gpg", remote.name);
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut key = cap_primitives::fs::open(&repo.file, Path::new(&path), &options)
        .map_err(|_| "Flatpak repository signing key is unavailable".to_owned())?;
    let before = key.metadata().map_err(|error| error.to_string())?;
    if !before.is_file() || before.len() == 0 || before.len() > 1024 * 1024 {
        return Err("Invalid Flatpak repository signing key".into());
    }
    let mut bytes = Vec::new();
    key.by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = key.metadata().map_err(|error| error.to_string())?;
    repo.validate().map_err(|error| error.to_string())?;
    if bytes.len() as u64 != before.len()
        || before.modified().ok() != after.modified().ok()
        || before.len() != after.len()
    {
        return Err("Flatpak signing key changed during preparation".into());
    }
    Ok(format!(
        "[Flatpak Repo]\nTitle={}\nUrl={}\nGPGKey={}\n",
        remote.name,
        url.as_str(),
        STANDARD.encode(bytes)
    )
    .into_bytes())
}
