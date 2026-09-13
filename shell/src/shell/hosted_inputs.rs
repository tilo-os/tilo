//! Narrow startup inputs for the existing embedded app constructors.
//! File paths are presentation data; they grant no filesystem authority.
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct HostedTileInputs {
    pub files: Option<file_explorer::StartupInputs>,
    pub picker_scope: Option<crate::portal::PickerScope>,
    fixture: Option<Arc<FixtureDirectory>>,
    file_services: Option<file_explorer::files_app::HostedFileServices>,
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
        Self::create_fixture(&std::env::temp_dir(), None)
    }

    /// Create an owned fixture child with thumbnails and document opening absent.
    /// The caller supplies an existing absolute, private temporary parent and
    /// retains it until every input/widget using the child has been dropped.
    /// # Errors
    /// Returns an error for a relative/missing parent or fixture filesystem errors.
    pub fn fixture_in(parent: &std::path::Path) -> std::io::Result<Self> {
        if !parent.is_absolute() || parent.parent().is_none() || !parent.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "fixture parent must be an existing absolute private directory",
            ));
        }
        Self::create_fixture(
            parent,
            Some(file_explorer::files_app::HostedFileServices {
                thumbnails: None,
                document_opener: None,
            }),
        )
    }

    fn create_fixture(
        parent: &std::path::Path,
        file_services: Option<file_explorer::files_app::HostedFileServices>,
    ) -> std::io::Result<Self> {
        let directory = parent.join(format!("qos-shell-fixture-{}", uuid::Uuid::new_v4()));
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
            file_services,
        })
    }

    pub(crate) fn file_services(&self) -> Option<&file_explorer::files_app::HostedFileServices> {
        self.file_services.as_ref()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn files_tile() -> crate::shell::state::TileContent {
        crate::shell::state::TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::FileExplorer,
            label: "Files".into(),
        }
    }

    #[test]
    fn fixture_directory_lives_through_last_input_clone() {
        let parent = tempfile::tempdir().unwrap();
        let inputs = HostedTileInputs::fixture_in(parent.path()).unwrap();
        let child = inputs.files.as_ref().unwrap().initial_dir.clone();
        assert_eq!(child.parent(), Some(parent.path()));
        assert_eq!(
            std::fs::read(child.join("Welcome.txt")).unwrap(),
            b"Shell capture fixture\n"
        );
        let last = inputs.clone();
        drop(inputs);
        assert!(child.is_dir());
        let tile = crate::shell::try_to_ui_tile(
            &files_tile(),
            terminal::HostedProcessServices::default(),
            &last,
        )
        .unwrap();
        assert!(matches!(tile, ui::widget::TileContent::App(_)));
        drop(tile);
        assert!(child.is_dir());
        drop(last);
        assert!(!child.exists());
        assert!(parent.path().is_dir());
    }

    #[test]
    fn fixture_missing_files_input_does_not_fall_back_on_linux() {
        let parent = tempfile::tempdir().unwrap();
        let mut inputs = HostedTileInputs::fixture_in(parent.path()).unwrap();
        inputs.files = None;
        let result = crate::shell::try_to_ui_tile(
            &files_tile(),
            terminal::HostedProcessServices::default(),
            &inputs,
        );
        assert!(matches!(
            result,
            Err(HostedTileError::RequiredServiceMissing("Files startup"))
        ));
    }
}
