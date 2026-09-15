//! Narrow startup inputs for the existing embedded app constructors.
//! File paths are presentation data; they grant no filesystem authority.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::state::{AppKind, TabId};

/// An application the native shell can construct in-process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAppRegistration {
    pub package_id: &'static str,
    pub display_name: &'static str,
    pub kind: AppKind,
    pub opens_extensions: &'static [&'static str],
    pub opens_mime_types: &'static [&'static str],
}

/// Device-local launch data for one open hosted document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedDocument {
    pub provider_id: &'static str,
    pub path: PathBuf,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedDocumentState {
    Ready(HostedDocument),
    Unavailable {
        provider_id: &'static str,
        path: PathBuf,
        reason: String,
    },
}

#[derive(Clone, Default)]
pub struct HostedTileInputs {
    pub files: Option<file_explorer::StartupInputs>,
    pub picker_scope: Option<crate::portal::PickerScope>,
    fixture: Option<Arc<FixtureDirectory>>,
    file_services: Option<file_explorer::files_app::HostedFileServices>,
    native_apps: Arc<[NativeAppRegistration]>,
    documents: Rc<RefCell<HashMap<TabId, HostedDocumentState>>>,
}

struct FixtureDirectory(PathBuf);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl HostedTileInputs {
    /// Native host startup data for the real Files widget. This chooses its
    /// initial directory; it does not claim to sandbox filesystem access.
    /// # Errors
    /// Returns an error if the directory cannot be resolved or is not a directory.
    pub fn for_host_directory(directory: &std::path::Path) -> std::io::Result<Self> {
        let directory = directory.canonicalize()?;
        if !directory.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "Files needs a directory",
            ));
        }
        Ok(Self {
            files: Some(file_explorer::StartupInputs {
                initial_dir: directory,
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            }),
            native_apps: Arc::from([NativeAppRegistration {
                package_id: "org.nirios.Editor",
                display_name: "Editor",
                kind: AppKind::Other("Editor".into()),
                opens_extensions: &["txt"],
                opens_mime_types: &["text/plain"],
            }]),
            ..Self::default()
        })
    }

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
            native_apps: Arc::default(),
            documents: Rc::default(),
        })
    }

    pub(crate) fn file_services(&self) -> Option<&file_explorer::files_app::HostedFileServices> {
        self.file_services.as_ref()
    }

    pub(crate) fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    /// The real hosted application catalogue for this native session.
    pub fn native_apps(&self) -> &[NativeAppRegistration] {
        &self.native_apps
    }

    pub(crate) fn native_app_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<NativeAppRegistration> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        let mime = mime_guess::from_path(path).first_raw();
        self.native_apps
            .iter()
            .find(|app| {
                app.opens_extensions.iter().any(|claim| *claim == extension)
                    || mime.is_some_and(|mime| app.opens_mime_types.contains(&mime))
            })
            .cloned()
    }

    pub(crate) fn bind_document(&self, tab: TabId, document: HostedDocument) {
        self.documents
            .borrow_mut()
            .insert(tab, HostedDocumentState::Ready(document));
    }

    pub(crate) fn restore_documents(
        &self,
        references: &HashMap<TabId, super::state::NativeDocumentReference>,
    ) {
        for (&tab, reference) in references {
            let Some(app) = self
                .native_apps
                .iter()
                .find(|app| app.package_id == reference.provider_id)
            else {
                continue;
            };
            let restored = reference
                .path
                .canonicalize()
                .and_then(|path| std::fs::read_to_string(&path).map(|text| (path, text)));
            let state = match restored {
                Ok((path, text)) => HostedDocumentState::Ready(HostedDocument {
                    provider_id: app.package_id,
                    path,
                    text,
                }),
                Err(error) => HostedDocumentState::Unavailable {
                    provider_id: app.package_id,
                    path: reference.path.clone(),
                    reason: format!("{} is unavailable: {error}", reference.path.display()),
                },
            };
            self.documents.borrow_mut().insert(tab, state);
        }
    }

    pub(crate) fn document(&self, tab: TabId) -> Option<HostedDocumentState> {
        self.documents.borrow().get(&tab).cloned()
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
