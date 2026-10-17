//! What the shell hands the apps it mounts in its own process: where Files
//! starts, the services they borrow from the shell, and the
//! [`InProcessApps`](super::in_process::InProcessApps) that keeps their
//! records. File paths are presentation data; they grant no filesystem
//! authority.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::state::{AppKind, TabId};

/// The Files widget is the Files package; it registers under that id.
pub const FILES_PACKAGE_ID: &str = "so.tilo.file-explorer";

/// An application the native shell can construct in-process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAppRegistration {
    pub package_id: &'static str,
    pub display_name: &'static str,
    pub kind: AppKind,
    pub opens_extensions: &'static [&'static str],
    pub opens_mime_types: &'static [&'static str],
}

/// Device-local launch data for one open hosted document: which app opens
/// which file. The app reads the file itself, so the shell never does: on
/// the workspace mount the first read hydrates it from a peer, which the
/// compositor loop must not wait for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedDocument {
    pub provider_id: &'static str,
    pub path: PathBuf,
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
    fixture_clock: Option<tilo_desktop::chrome::ClockSnapshot>,
    file_services: Option<file_explorer::files_app::HostedFileServices>,
    native_apps: Arc<[NativeAppRegistration]>,
    documents: Rc<RefCell<HashMap<TabId, HostedDocumentState>>>,
    file_directories: Rc<RefCell<HashMap<TabId, PathBuf>>>,
    /// The framework apps this host mounts in its own process; without
    /// them a tab of such an app says it is unavailable.
    in_process: Option<super::in_process::SharedInProcessApps>,
    /// Whether the terminals this host mounts open sessions on the
    /// session's process supervisor: only a real session's do.
    opens_sessions: bool,
    /// A private profile for the apps of a host that keeps nobody's records,
    /// removed with the last input that uses it.
    private_profile: Option<Arc<FixtureDirectory>>,
}

/// A fixture's folder for Files and the profile its apps keep records in,
/// removed with the last input that uses them.
struct FixtureDirectory(Vec<PathBuf>);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        for directory in &self.0 {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

impl HostedTileInputs {
    /// Native host startup data for the Files widget. This chooses its
    /// initial directory; it does not claim to sandbox filesystem access.
    /// # Errors
    /// Returns an error if the directory cannot be resolved or is not a directory.
    pub fn for_host_directory(
        directory: &std::path::Path,
        apps: super::in_process::InProcessApps,
    ) -> std::io::Result<Self> {
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
            file_services: Some(
                file_explorer::files_app::HostedFileServices::deferred_document_open(),
            ),
            native_apps: Arc::from([
                NativeAppRegistration {
                    package_id: tilo_desktop::feedback::PACKAGE,
                    display_name: tilo_desktop::feedback::definition().title,
                    kind: AppKind::Other(tilo_desktop::feedback::KIND.into()),
                    opens_extensions: &[],
                    opens_mime_types: &[],
                },
                NativeAppRegistration {
                    package_id: FILES_PACKAGE_ID,
                    display_name: "Files",
                    kind: AppKind::FileExplorer,
                    opens_extensions: &[],
                    opens_mime_types: &[],
                },
                NativeAppRegistration {
                    package_id: "org.nirios.Editor",
                    display_name: "Editor",
                    kind: AppKind::Other("Editor".into()),
                    opens_extensions: &["txt"],
                    opens_mime_types: &["text/plain"],
                },
            ]),
            in_process: Some(Rc::new(RefCell::new(apps))),
            opens_sessions: true,
            ..Self::default()
        })
    }

    /// [`Self::for_host_directory`] for a test or a capture: the apps keep
    /// their records in a private profile removed with these inputs, as a
    /// fixture person, and terminals open no session.
    /// # Errors
    /// As [`Self::for_host_directory`], and the profile cannot be created.
    pub fn for_host_directory_privately(directory: &std::path::Path) -> std::io::Result<Self> {
        let profile =
            std::env::temp_dir().join(format!("tilo-shell-apps-{}", uuid::Uuid::new_v4()));
        let apps = super::in_process::InProcessApps::at(&profile, "fixture-person")
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let owned = Arc::new(FixtureDirectory(vec![profile]));
        let mut inputs = Self::for_host_directory(directory, apps)?;
        inputs.opens_sessions = false;
        inputs.private_profile = Some(owned);
        Ok(inputs)
    }

    /// Explicit deterministic inputs for scratch scenes, tests and captures.
    /// Files reads only this owned temporary directory, terminals open no
    /// session, and the apps keep their records in a profile of the
    /// fixture's own. This does not advertise a host provider.
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
                document_opener: file_explorer::files_app::DocumentOpenRoute::Unavailable,
                open_with: None,
                local_clipboard: false,
            }),
        )
    }

    fn create_fixture(
        parent: &std::path::Path,
        file_services: Option<file_explorer::files_app::HostedFileServices>,
    ) -> std::io::Result<Self> {
        let id = uuid::Uuid::new_v4();
        let directory = parent.join(format!("tilo-shell-fixture-{id}"));
        let profile = parent.join(format!("tilo-shell-fixture-apps-{id}"));
        std::fs::create_dir(&directory)?;
        let fixture = Arc::new(FixtureDirectory(vec![directory.clone(), profile.clone()]));
        // A fixture's apps keep their records in its own profile, as a
        // fixture person, and never reach the session's.
        let apps = super::in_process::InProcessApps::at(&profile, "fixture-person")
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        std::fs::write(directory.join("Welcome.txt"), "Shell capture fixture\n")?;
        Ok(Self {
            picker_scope: Some(crate::portal::PickerScope {
                initial_dir: directory.clone(),
                roots: vec![directory.clone()],
                home: Some(directory.clone()),
            }),
            files: Some(file_explorer::StartupInputs {
                initial_dir: directory,
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            }),
            fixture: Some(fixture),
            fixture_clock: None,
            file_services,
            native_apps: Arc::default(),
            documents: Rc::default(),
            file_directories: Rc::default(),
            in_process: Some(Rc::new(RefCell::new(apps))),
            opens_sessions: false,
            private_profile: None,
        })
    }

    /// Mount framework apps (the terminal, Files, the browser) in this
    /// process through `apps`, as a real session does: its terminals open
    /// sessions on the session's process supervisor.
    pub fn mount_apps_in_process(&mut self, apps: super::in_process::InProcessApps) {
        self.in_process = Some(Rc::new(RefCell::new(apps)));
        self.opens_sessions = true;
    }

    /// Whether the terminals this host mounts open sessions.
    pub(crate) fn opens_sessions(&self) -> bool {
        self.opens_sessions
    }

    pub(crate) fn in_process(&self) -> Option<&super::in_process::SharedInProcessApps> {
        self.in_process.as_ref()
    }

    pub(crate) fn file_services(&self) -> Option<&file_explorer::files_app::HostedFileServices> {
        self.file_services.as_ref()
    }

    /// Accept only the opaque deferred-open capability installed with this
    /// host's real Files services. Other mounted apps can construct the public
    /// intent shape, but cannot authorize a native document read/publication.
    pub(crate) fn accepts_document_open_capability(
        &self,
        capability: &ui::widget::DocumentOpenCapability,
    ) -> bool {
        self.file_services
            .as_ref()
            .is_some_and(|services| services.accepts_document_open_capability(capability))
    }

    pub(crate) fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    /// An owned fixture whose apps reach the session's broker at
    /// `collab`, as a session's do: a capture of a room whose session
    /// service is gone points it at a socket nobody serves.
    /// # Errors
    /// Returns an error if these inputs belong to a live host.
    pub fn with_session_broker_at(
        self,
        collab: std::path::PathBuf,
        identity: std::path::PathBuf,
    ) -> std::io::Result<Self> {
        if !self.is_fixture() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "A fixture broker requires owned fixture inputs",
            ));
        }
        if let Some(apps) = &self.in_process {
            apps.borrow_mut()
                .reach_broker(std::rc::Rc::new(move |app, room| {
                    super::in_process::session_connector(
                        collab.clone(),
                        identity.clone(),
                        app,
                        room,
                    )
                }));
        }
        Ok(self)
    }

    /// Give an owned fixture a stable clock, without changing the live source.
    /// # Errors
    /// Returns an error if these inputs belong to a live host.
    pub fn with_fixture_clock(
        mut self,
        clock: tilo_desktop::chrome::ClockSnapshot,
    ) -> std::io::Result<Self> {
        if !self.is_fixture() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "A fixture clock requires owned fixture inputs",
            ));
        }
        self.fixture_clock = Some(clock);
        Ok(self)
    }

    pub(crate) fn fixture_clock(&self) -> Option<tilo_desktop::chrome::ClockSnapshot> {
        self.is_fixture()
            .then(|| self.fixture_clock.clone().unwrap_or_default())
    }

    /// The hosted application catalogue for this native session.
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

    /// Choose the directory a newly-created hosted Files tab presents. The
    /// binding is device-local launch data, just like [`Self::bind_document`];
    /// filesystem authority is granted separately by chrome.
    pub(crate) fn bind_files_directory(&self, tab: TabId, directory: PathBuf) {
        self.file_directories.borrow_mut().insert(tab, directory);
    }

    pub(crate) fn files_for_tab(&self, tab: TabId) -> Option<file_explorer::StartupInputs> {
        let directory = self.file_directories.borrow().get(&tab).cloned()?;
        let mut inputs = self
            .files
            .clone()
            .unwrap_or_else(|| file_explorer::StartupInputs {
                initial_dir: directory.clone(),
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            });
        inputs.initial_dir = directory;
        Some(inputs)
    }

    /// Release launch data once its tab leaves any workspace, without rereading files.
    /// Returns whether persisted references changed.
    pub(crate) fn prune_documents(&self, tree: &mut super::state::WorkspaceTree) -> bool {
        if tree.native_document_tabs.is_empty() && self.documents.borrow().is_empty() {
            return false;
        }
        let live: std::collections::HashSet<_> = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        let before = tree.native_document_tabs.len();
        tree.native_document_tabs
            .retain(|tab, _| live.contains(tab));
        self.documents
            .borrow_mut()
            .retain(|tab, _| live.contains(tab));
        before != tree.native_document_tabs.len()
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn restore_documents(&self, tree: &mut super::state::WorkspaceTree) {
        // Old profiles can contain already closed tabs. Filter before filesystem I/O.
        self.prune_documents(tree);
        let references = &tree.native_document_tabs;
        for (&tab, reference) in references {
            let provider_id = reference.provider_id.as_str();
            let Some(app) = self
                .native_apps
                .iter()
                .find(|app| app.package_id == provider_id)
            else {
                continue;
            };
            // Restored at startup, before any frame: whether the file is
            // still readable text decides between the editor and a notice.
            let restored = reference
                .path
                .canonicalize()
                .and_then(|path| std::fs::read_to_string(&path).map(|_| path));
            let state = match restored {
                Ok(path) => HostedDocumentState::Ready(HostedDocument {
                    provider_id: app.package_id,
                    path,
                }),
                Err(error) => HostedDocumentState::Unavailable {
                    provider_id: app.package_id,
                    path: reference.path.clone(),
                    reason: crate::shell::i18n::t_args(
                        "hosted-document-unavailable",
                        &[
                            ("path", reference.path.display().to_string().into()),
                            ("error", error.to_string().into()),
                        ],
                    ),
                },
            };
            self.documents.borrow_mut().insert(tab, state);
        }
    }

    pub(crate) fn document(&self, tab: TabId) -> Option<HostedDocumentState> {
        self.documents.borrow().get(&tab).cloned()
    }

    /// Notes for tab `tab` of `room`, on a notebook of its own holding
    /// `markdown` as the note `name`, as Notes opens a Markdown file it is
    /// handed. `None` for a host that mounts no apps in-process.
    pub fn mount_note(
        &self,
        room: &str,
        tab: TabId,
        name: &str,
        markdown: &str,
    ) -> Option<Result<ui::application::AppComponent, ui::launch::LaunchError>> {
        let apps = self.in_process.as_ref()?;
        let file = match ui::application::OpenFile::new(
            name,
            "text/markdown",
            markdown.as_bytes().to_vec(),
        ) {
            Ok((file, _receipt)) => file,
            Err(error) => return Some(Err(ui::launch::LaunchError::Operation(error))),
        };
        Some(apps.borrow_mut().mount(
            notes::definition(),
            room,
            tab,
            ui::record::RecordId::minted(),
            |context| context.provide_open_file(file),
        ))
    }
}

#[derive(Debug)]
pub enum HostedTileError {
    RequiredServiceMissing(&'static str),
    Fixture(String),
}

impl std::fmt::Display for HostedTileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiredServiceMissing(service) => {
                write!(formatter, "Required service unavailable: {service}")
            }
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

    /// The Files tab a host's inputs mount, as the shell's tile resolver
    /// mounts it.
    fn mounted_files(
        inputs: &HostedTileInputs,
    ) -> Option<Result<ui::application::AppComponent, ui::launch::LaunchError>> {
        let tab = crate::shell::state::Tab {
            id: 7,
            title: "Files".into(),
            tile: files_tile(),
        };
        let request = tilo_desktop::resolver::TileRequest {
            room: crate::shell::state::RoomId::new(),
            tab: &tab,
        };
        crate::shell::mount_in_process(&request, inputs).map(|resolution| match resolution {
            tilo_desktop::resolver::TileResolution::App(component, _) => Ok(component),
            tilo_desktop::resolver::TileResolution::Failed(error) => Err(error),
            _ => Err(ui::launch::LaunchError::Operation("not a component".into())),
        })
    }

    #[test]
    fn hosted_file_inputs_live_through_clones_and_never_fall_back() {
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
            let component = mounted_files(&last)
                .expect("a fixture mounts Files in-process")
                .unwrap();
            let mut harness = ui::headless::FrameHarness::from_component(
                component,
                (1000, 700),
                ui::application::ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
            harness.relayout();
            assert!(
                harness.inspection_json().unwrap().contains("Welcome.txt"),
                "Files lists the fixture's own folder"
            );
            harness.close_components();
            drop(harness);
            assert!(child.is_dir());
            drop(last);
            assert!(!child.exists());
            assert!(parent.path().is_dir());
        }

        fn fixture_missing_files_input_does_not_fall_back_on_linux() {
            let parent = tempfile::tempdir().unwrap();
            let mut inputs = HostedTileInputs::fixture_in(parent.path()).unwrap();
            inputs.files = None;
            assert!(mounted_files(&inputs).is_none(), "nothing is mounted");
            let result = crate::shell::try_to_ui_tile(&files_tile(), &inputs);
            assert!(matches!(
                result,
                Err(HostedTileError::RequiredServiceMissing("Files startup"))
            ));
        }

        fn a_files_tab_can_start_in_its_dropped_directory() {
            let parent = tempfile::tempdir().unwrap();
            let inputs = HostedTileInputs::fixture_in(parent.path()).unwrap();
            let dropped = parent.path().join("dropped");
            std::fs::create_dir(&dropped).unwrap();
            inputs.bind_files_directory(42, dropped.clone());
            assert_eq!(inputs.files_for_tab(42).unwrap().initial_dir, dropped);
            assert!(inputs.files_for_tab(43).is_none());
        }

        fixture_directory_lives_through_last_input_clone();
        fixture_missing_files_input_does_not_fall_back_on_linux();
        a_files_tab_can_start_in_its_dropped_directory();
    }
}

#[cfg(test)]
mod desktop_catalog_tests {
    use super::*;

    #[test]
    fn desktop_native_catalog_includes_constructible_files() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = HostedTileInputs::for_host_directory_privately(dir.path()).unwrap();
        assert!(
            inputs
                .native_apps()
                .iter()
                .any(|app| app.display_name == "Files" && app.kind == AppKind::FileExplorer)
        );
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod document_lifecycle_tests {
    use super::*;
    use crate::compositor::macos::{Options, prepare};
    use crate::shell::{state::NativeDocumentReference, window_actions};

    #[test]
    #[serial_test::serial]
    fn closed_hosted_documents_are_pruned_and_restart_retains_live_documents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.txt");
        std::fs::write(&path, "live document\n").unwrap();
        let options = Options {
            state_dir: directory.path().join("state"),
            files_dir: directory.path().to_path_buf(),
            shots: None,
        };
        let tabs = {
            let session = prepare(&options).unwrap();
            let inputs = session.chrome.borrow().hosted_inputs.clone();
            let reference = NativeDocumentReference {
                provider_id: "org.nirios.Editor".into(),
                path: path.canonicalize().unwrap(),
            };
            let mut tabs = Vec::new();
            for _ in 0..2 {
                let tab = window_actions::open_hosted(
                    &mut session.chrome.borrow_mut(),
                    AppKind::Other("Editor".into()),
                    "note.txt",
                    window_actions::Destination::Here,
                );
                session
                    .tree
                    .write()
                    .native_document_tabs
                    .insert(tab, reference.clone());
                inputs.bind_document(
                    tab,
                    HostedDocument {
                        provider_id: "org.nirios.Editor",
                        path: reference.path.clone(),
                    },
                );
                tabs.push(tab);
            }
            let mut driver = ui::headless::FrameHarness::new(
                ui::State::new(),
                Box::new(session.host),
                (1280, 800),
            );
            driver.set_modifiers(&["meta".into()]);
            driver.key("w");
            driver.set_modifiers(&[]);
            assert!(
                !session
                    .tree
                    .read()
                    .native_document_tabs
                    .contains_key(&tabs[1]),
                "closing a tab must remove its persisted document reference"
            );
            assert!(
                inputs.document(tabs[1]).is_none(),
                "closing must release cached text"
            );
            assert!(matches!(
                inputs.document(tabs[0]),
                Some(HostedDocumentState::Ready(_))
            ));
            // Simulate an older profile that retained the already closed tab.
            session
                .tree
                .write()
                .native_document_tabs
                .insert(tabs[1], reference);
            session.tree.read().queue_save();
            drop(driver);
            drop(session.chrome);
            drop(session.records);
            drop(session.tree);
            tabs
        };
        let reopened = prepare(&options).unwrap();
        assert!(
            !reopened
                .tree
                .read()
                .native_document_tabs
                .contains_key(&tabs[1])
        );
        let restored = reopened.chrome.borrow().hosted_inputs.clone();
        assert!(
            restored.document(tabs[1]).is_none(),
            "stale references must be filtered before restoration"
        );
        assert!(
            matches!(restored.document(tabs[0]), Some(HostedDocumentState::Ready(document)) if document.provider_id == "org.nirios.Editor" && document.path == path.canonicalize().unwrap())
        );
        let mut driver =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(reopened.host), (1280, 800));
        // Tree reconciliation can remove tabs without using the close gesture.
        assert!(crate::shell::tab_binding::remove_tab(
            &mut reopened.tree.write(),
            tabs[0]
        ));
        driver.profile_frame();
        assert!(reopened.tree.read().native_document_tabs.is_empty());
        assert!(restored.document(tabs[0]).is_none());
    }
}
