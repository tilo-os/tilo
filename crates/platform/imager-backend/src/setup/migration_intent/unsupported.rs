//! No source permission can be minted without the native source custodian.
use crate::setup::{
    flatpak_transfer::InstalledRef,
    migration::PinnedDirectory,
    migration_handoff::{HomePolicy, Plan},
};
use std::{io, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Unsupported {}

/// A reviewed native migration permission. Its representation and constructors
/// remain sealed even on hosts that cannot capture a native source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationIntent {
    unsupported: Unsupported,
}

fn unavailable<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Native migration permissions require Linux",
    ))
}

impl MigrationIntent {
    pub fn load(_directory: &Path) -> io::Result<Self> {
        unavailable()
    }

    pub fn capture(
        _source: &Path,
        _directory: &Path,
        _categories: [bool; 3],
        _selected: Vec<InstalledRef>,
    ) -> io::Result<Self> {
        unavailable()
    }

    pub fn capture_with_policy(
        _source: &Path,
        _directory: &Path,
        _categories: [bool; 3],
        _selected: Vec<InstalledRef>,
        _home_policy: HomePolicy,
    ) -> io::Result<Self> {
        unavailable()
    }

    pub fn capture_reviewed_with_policy(
        _source: &Path,
        _directory: &Path,
        _categories: [bool; 3],
        _selected: Vec<InstalledRef>,
        _home_policy: HomePolicy,
    ) -> io::Result<Self> {
        unavailable()
    }

    pub fn capture_computer_applications(
        _source: &Path,
        _directory: &Path,
        _categories: [bool; 3],
        _selected: Vec<InstalledRef>,
    ) -> io::Result<Self> {
        unavailable()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_using(
        _source: &Path,
        _directory: &Path,
        _categories: [bool; 3],
        _selected: Vec<InstalledRef>,
        _home_policy: HomePolicy,
        _capture: super::Capture,
    ) -> io::Result<Self> {
        unavailable()
    }

    pub fn directory(&self) -> &Path {
        match self.unsupported {}
    }

    pub fn application_minimum(&self) -> u64 {
        match self.unsupported {}
    }

    pub fn source_plan(&self) -> Plan {
        match self.unsupported {}
    }

    pub fn attach_deferred_handoff(
        &self,
        _handoff: super::super::migration_handoff::DeferredHandoff,
    ) -> io::Result<Self> {
        match self.unsupported {}
    }

    pub fn for_media(&self) -> io::Result<Plan> {
        match self.unsupported {}
    }

    pub fn for_pending_offline(&self) -> io::Result<Plan> {
        match self.unsupported {}
    }

    pub fn for_pending_applications(&self) -> io::Result<Plan> {
        match self.unsupported {}
    }

    pub fn has_computer_applications(&self) -> io::Result<bool> {
        match self.unsupported {}
    }

    pub fn computer_applications(&self) -> io::Result<Option<&Self>> {
        match self.unsupported {}
    }

    pub fn validate_computer_applications_selection(
        &self,
        _selected: &[InstalledRef],
    ) -> io::Result<()> {
        match self.unsupported {}
    }

    pub fn validate_artifact_destination(&self, _destination: &PinnedDirectory) -> io::Result<()> {
        match self.unsupported {}
    }

    pub fn validate_staging_copy(&self, _declared: &Plan) -> io::Result<()> {
        match self.unsupported {}
    }

    pub fn bind(
        &self,
        _operation: &str,
        _installation: &crate::plan::Plan,
    ) -> io::Result<serde_json::Value> {
        match self.unsupported {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_refuse_native_permissions() {
        let path = Path::new("unavailable-source");
        assert_eq!(
            MigrationIntent::load(path).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            MigrationIntent::capture(path, path, [true, false, false], Vec::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
