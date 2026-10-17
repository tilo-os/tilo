//! Browser source custody is unavailable on this host; metadata is not authority.
use crate::account_sources::Candidate;
use browser_continuity::Profile;
use std::{io, path::Path, sync::atomic::AtomicBool};

pub struct ReviewedProfile {
    pub profile: Profile,
    pub label: String,
    _private: (),
}
impl ReviewedProfile {
    pub fn validate(&self) -> io::Result<()> {
        Err(crate::journey_media::Unsupported.into())
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_files(
        &self,
        selection: &Profile,
    ) -> io::Result<crate::browser_selection::Snapshot> {
        self.capture_files_cancelled(selection, &AtomicBool::new(false))
    }
    pub fn capture_files_cancelled(
        &self,
        _selection: &Profile,
        _cancel: &AtomicBool,
    ) -> io::Result<crate::browser_selection::Snapshot> {
        Err(crate::journey_media::Unsupported.into())
    }
}
pub fn discover(_home: &Path, _candidates: &[Candidate]) -> io::Result<Vec<ReviewedProfile>> {
    Err(crate::journey_media::Unsupported.into())
}
