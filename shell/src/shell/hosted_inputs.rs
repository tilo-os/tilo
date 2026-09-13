//! Narrow startup inputs for the existing embedded app constructors.
//! File paths are presentation data; they grant no filesystem authority.
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct HostedTileInputs {
    pub files: Option<file_explorer::StartupInputs>,
    pub picker_scope: Option<crate::portal::PickerScope>,
    fixture: Option<Arc<FixtureDirectory>>,
}

struct FixtureDirectory(PathBuf);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl HostedTileInputs {
    /// Explicit deterministic inputs for scratch scenes, tests and captures.
    /// The real Files widget reads only this owned temporary directory; Terminal
    /// uses its existing null backend. This does not advertise a host provider.
    /// # Errors
    /// Returns filesystem errors while creating the fixture.
    pub fn fixture() -> std::io::Result<Self> {
        let directory =
            std::env::temp_dir().join(format!("qos-shell-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let fixture = Arc::new(FixtureDirectory(directory.clone()));
        std::fs::write(directory.join("Welcome.txt"), "Shell capture fixture\n")?;
        Ok(Self {
            picker_scope: Some(crate::portal::PickerScope {
                initial_dir: directory.clone(),
                roots: vec![directory.clone()],
            }),
            files: Some(file_explorer::StartupInputs {
                initial_dir: directory,
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            }),
            fixture: Some(fixture),
        })
    }

    pub(crate) fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }
}

#[derive(Debug)]
pub enum HostedTileError {
    RequiredServiceMissing(&'static str),
    Terminal(terminal::HostedTerminalError),
    Fixture(String),
}

impl std::fmt::Display for HostedTileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiredServiceMissing(service) => {
                write!(formatter, "Required service unavailable: {service}")
            }
            Self::Terminal(error) => error.fmt(formatter),
            Self::Fixture(error) => write!(formatter, "Fixture unavailable: {error}"),
        }
    }
}

impl std::error::Error for HostedTileError {}

impl std::fmt::Debug for HostedTileInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostedTileInputs")
            .field("has_files", &self.files.is_some())
            .field("fixture", &self.is_fixture())
            .finish_non_exhaustive()
    }
}
