//! The cache directory the live client keeps its answers in, and the bytes a person keeps for
//! playing with no network, through `tilo-files` and the device's content store.

use std::io;
use std::path::Path;

use tilo_files::{FsError, blocking};

fn at(path: &Path) -> io::Result<(tilo_files::Dir, tilo_files::VPath)> {
    tilo_files::file_at(path).map_err(io::Error::other)
}

fn io(error: FsError) -> io::Error {
    match error {
        FsError::NotFound => io::ErrorKind::NotFound.into(),
        FsError::AlreadyExists => io::ErrorKind::AlreadyExists.into(),
        other => io::Error::other(other),
    }
}

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    let (dir, name) = at(path)?;
    blocking::create_dir_all(&dir, &name).map_err(io)
}

/// Keep the bytes at `url` on this device as the song called `name`, answering the content
/// address a window reads them at (`tilo-blob://…`): what keeping a song offline is. The bytes
/// are the device's content, not a cache of this connector's, so every window of the workspace
/// reads the same kept song by the same address.
///
/// # Errors
///
/// Why the song was not kept: the network did not answer, this process holds no content store,
/// or the store refused the bytes.
pub fn pin(url: &str, name: &str) -> Result<String, String> {
    let client = tilo_net::http::blocking_client()
        .build()
        .map_err(|error| format!("this process has no network: {error}"))?;
    let fetch: ui::content::Fetch = std::sync::Arc::new(move |url: &str| {
        client
            .get(url)
            .send()
            .ok()?
            .bytes()
            .ok()
            .map(|bytes| bytes.to_vec())
    });
    let store = ui::content::device()
        .ok_or_else(|| format!("this computer cannot keep `{name}`: it holds no content store"))?;
    ui::content::keep_fetched(&store, &fetch, url)
        .ok_or_else(|| format!("`{name}` could not be fetched or kept"))
}
