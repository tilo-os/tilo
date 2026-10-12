//! Browser ciphertext custody under the same protected staging transaction.
use super::{RecordStore, secure, valid_operation_id};
use crate::setup::{
    browser_transfer::Expectation,
    staging::browser::{self, BrowserArtifact},
};
use std::{io, path::PathBuf};
impl RecordStore {
    fn browser_path(&self, id: &str, expected: &Expectation) -> Result<PathBuf, String> {
        if !valid_operation_id(id) {
            return Err("Invalid browser staging operation".into());
        }
        expected.validate().map_err(|error| error.to_string())?;
        Ok(self
            .dir
            .join(format!("{id}.browser"))
            .join(&expected.filename))
    }
    pub(in crate::setup::staging) fn retain_browser(
        &self,
        id: &str,
        bytes: &[u8],
        expected: &Expectation,
    ) -> io::Result<()> {
        browser::verify(bytes, expected)?;
        secure::ensure_dir(&self.dir, false).map_err(io::Error::other)?;
        let path = self.browser_path(id, expected).map_err(io::Error::other)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Browser custody parent missing"))?;
        secure::ensure_dir(parent, false).map_err(io::Error::other)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return self.read_browser(id, expected).map(|_| ()),
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
        self.read_browser(id, expected).map(|_| ())
    }
    pub(in crate::setup::staging) fn read_browser(
        &self,
        id: &str,
        expected: &Expectation,
    ) -> io::Result<Vec<u8>> {
        let path = self.browser_path(id, expected).map_err(io::Error::other)?;
        let artifact = BrowserArtifact {
            source_path: path
                .to_str()
                .ok_or_else(|| io::Error::other("Browser custody path is not UTF-8"))?
                .into(),
            expectation: expected.clone(),
        };
        // SAFETY: observes the helper's effective identity without mutating it.
        let owner = unsafe { libc::geteuid() };
        artifact.read(owner)
    }
}
