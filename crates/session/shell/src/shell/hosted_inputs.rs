//! What the shell hands the surfaces it draws itself: the feedback app and
//! the [`InProcessApps`](super::in_process::InProcessApps) that keeps its
//! records, and a fixture's own folder. Every packaged app runs as its own
//! process. File paths are presentation data; they grant no filesystem
//! authority.
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::state::AppKind;

/// An application the native shell can construct in-process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAppRegistration {
    pub package_id: &'static str,
    pub display_name: &'static str,
    pub kind: AppKind,
    pub opens_extensions: &'static [&'static str],
    pub opens_mime_types: &'static [&'static str],
}

#[derive(Clone, Default)]
pub struct HostedTileInputs {
    pub picker_scope: Option<crate::portal::PickerScope>,
    fixture: Option<Arc<FixtureDirectory>>,
    fixture_clock: Option<tilo_desktop::chrome::ClockSnapshot>,
    /// Whether this host stands in for a person's own files (a capture's
    /// or a test's folder): it shows the shipped wallpaper, never theirs.
    owns_files: bool,
    native_apps: Arc<[NativeAppRegistration]>,
    /// The framework apps this host mounts in its own process; without
    /// them a tab of such an app says it is unavailable.
    in_process: Option<super::in_process::SharedInProcessApps>,
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
    /// A host standing in for a person's own folder `directory`, drawing the
    /// shell's feedback through `apps`. It grants no filesystem access.
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
            owns_files: true,
            native_apps: Arc::from([NativeAppRegistration {
                package_id: tilo_desktop::feedback::PACKAGE,
                display_name: tilo_desktop::feedback::definition().title(),
                kind: AppKind::Other(tilo_desktop::feedback::KIND.into()),
                opens_extensions: &[],
                opens_mime_types: &[],
            }]),
            in_process: Some(Rc::new(RefCell::new(apps))),
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
        let apps = super::in_process::InProcessApps::at(
            &profile,
            &profile.join("person"),
            "fixture-person",
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        let owned = Arc::new(FixtureDirectory(vec![profile]));
        let mut inputs = Self::for_host_directory(directory, apps)?;
        inputs.private_profile = Some(owned);
        Ok(inputs)
    }

    /// Explicit deterministic inputs for scratch scenes, tests and captures.
    /// Files reads only this owned temporary directory, terminals open no
    /// session, and the apps keep their records in a profile of the
    /// fixture's own. This does not advertise a host provider.
    pub fn fixture() -> std::io::Result<Self> {
        Self::create_fixture(&std::env::temp_dir(), false)
    }

    /// Create an owned fixture child with thumbnails and document opening absent.
    /// The caller supplies an existing absolute, private temporary parent and
    /// retains it until every input/widget using the child has been dropped.
    pub fn fixture_in(parent: &std::path::Path) -> std::io::Result<Self> {
        if !parent.is_absolute() || parent.parent().is_none() || !parent.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "fixture parent must be an existing absolute private directory",
            ));
        }
        Self::create_fixture(parent, true)
    }

    fn create_fixture(parent: &std::path::Path, owns_files: bool) -> std::io::Result<Self> {
        let id = uuid::Uuid::new_v4();
        let directory = parent.join(format!("tilo-shell-fixture-{id}"));
        let profile = parent.join(format!("tilo-shell-fixture-apps-{id}"));
        std::fs::create_dir(&directory)?;
        let fixture = Arc::new(FixtureDirectory(vec![directory.clone(), profile.clone()]));
        // A fixture's apps keep their records in its own profile, as a
        // fixture person, and never reach the session's.
        let apps = super::in_process::InProcessApps::at(
            &profile,
            &profile.join("person"),
            "fixture-person",
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        std::fs::write(directory.join("Welcome.txt"), "Shell capture fixture\n")?;
        Ok(Self {
            picker_scope: Some(crate::portal::PickerScope {
                initial_dir: directory.clone(),
                roots: vec![directory.clone()],
                home: Some(directory.clone()),
            }),
            fixture: Some(fixture),
            fixture_clock: None,
            owns_files,
            native_apps: Arc::default(),
            in_process: Some(Rc::new(RefCell::new(apps))),
            private_profile: None,
        })
    }

    /// The Wayland session's inputs: its apps run as their own processes,
    /// and `services` keeps only what the session itself serves (feedback,
    /// the messaging accounts).
    pub fn run_apps_as_processes(&mut self, services: super::in_process::InProcessApps) {
        self.in_process = Some(Rc::new(RefCell::new(services)));
    }

    pub(crate) fn in_process(&self) -> Option<&super::in_process::SharedInProcessApps> {
        self.in_process.as_ref()
    }

    pub(crate) fn owns_files(&self) -> bool {
        self.owns_files
    }

    pub(crate) fn is_fixture(&self) -> bool {
        self.fixture.is_some()
    }

    /// An owned fixture whose apps reach the session's broker at
    /// `collab`, as a session's do: a capture of a room whose session
    /// service is gone points it at a socket nobody serves.
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
            .field("owns_files", &self.owns_files)
            .field("fixture", &self.is_fixture())
            .finish_non_exhaustive()
    }
}
