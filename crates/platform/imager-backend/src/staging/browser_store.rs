//! Protected browser ciphertext custody for the staging journal.
use super::browser::{BrowserArtifact, Expectation, verify};
use super::protocol::valid_operation_id;
use super::record_io::secure;
use std::{
    io,
    path::{Path, PathBuf},
};

fn browser_path(dir: &Path, id: &str, expected: &impl Expectation) -> Result<PathBuf, String> {
    if !valid_operation_id(id) {
        return Err("Invalid browser staging operation".into());
    }
    expected.validate().map_err(|error| error.to_string())?;
    Ok(dir.join(format!("{id}.browser")).join(expected.filename()))
}
pub fn retain_browser(
    dir: &Path,
    id: &str,
    bytes: &[u8],
    expected: &impl Expectation,
) -> io::Result<()> {
    verify(bytes, expected)?;
    secure::ensure_dir(dir, false).map_err(io::Error::other)?;
    let path = browser_path(dir, id, expected).map_err(io::Error::other)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Browser custody parent missing"))?;
    secure::ensure_dir(parent, false).map_err(io::Error::other)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => return read_browser(dir, id, expected).map(|_| ()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    use io::Write;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    if let Err(error) = staged.persist_noclobber(&path)
        && error.error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error.error);
    }
    std::fs::File::open(parent)?.sync_all()?;
    read_browser(dir, id, expected).map(|_| ())
}
pub fn read_browser(dir: &Path, id: &str, expected: &impl Expectation) -> io::Result<Vec<u8>> {
    let path = browser_path(dir, id, expected).map_err(io::Error::other)?;
    let artifact = BrowserArtifact {
        source_path: path
            .to_str()
            .ok_or_else(|| io::Error::other("Browser custody path is not UTF-8"))?
            .into(),
        expectation: expected,
    };
    // SAFETY: observes the helper's effective identity without mutating it.
    let owner = rustix::process::geteuid().as_raw();
    artifact.read(owner)
}
