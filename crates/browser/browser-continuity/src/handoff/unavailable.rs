//! Unsupported platforms never acquire sources or publish receipts.
use super::StatePath;
use crate::imports::ImportBundle;
use std::io;

pub struct SelectedCapture {
    pub bookmarks: Option<ImportBundle>,
    pub history: Option<ImportBundle>,
}
pub struct Captured {
    pub profiles: Vec<SelectedCapture>,
}
impl Captured {
    /// # Errors
    /// Always refuses: no native checkpoint authority exists on this platform.
    pub fn complete(&self, _state: &()) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native continuity handoff is unavailable",
        ))
    }
}
pub struct Prepared {
    pub capture: Captured,
    pub state: (),
    pub complete: bool,
}
#[derive(Clone)]
pub struct Source;
impl Source {
    pub fn sandbox(_state_path: StatePath) -> Self {
        Self
    }
    /// # Errors
    /// No sources are available; this never requests access or changes records.
    pub fn prepare(&self, _person: &[u8; 32]) -> io::Result<Option<Prepared>> {
        Ok(None)
    }
}
