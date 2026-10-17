//! Browser receipt bridge to protected native staging custody.
use super::RecordStore;
use crate::setup::browser_transfer::Expectation;
use std::io;

impl RecordStore {
    pub(in crate::setup::staging) fn retain_browser(
        &self,
        id: &str,
        bytes: &[u8],
        expected: &Expectation,
    ) -> io::Result<()> {
        imager_backend::staging::browser_store::retain_browser(&self.dir, id, bytes, expected)
    }

    pub(in crate::setup::staging) fn read_browser(
        &self,
        id: &str,
        expected: &Expectation,
    ) -> io::Result<Vec<u8>> {
        imager_backend::staging::browser_store::read_browser(&self.dir, id, expected)
    }
}
