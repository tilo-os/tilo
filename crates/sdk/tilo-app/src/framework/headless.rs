//! One framework app mounted headlessly: the real component runtime,
//! storage, permissions, undo and history, without a window. Tests drive it
//! as [`TestApp`](super::testing::TestApp); the app binary's own modes drive
//! it too, so it ships in every release app: `--surface` reports the
//! surface it publishes, `--bench` captures its fixtures, and `--data-dir`
//! and `--shots` open it on a profile of their own.

use std::path::PathBuf;
use std::rc::Rc;

use tilo_resource::{Effect, Invoke, Resource};
use ui_core::application::{
    ApplicationHost, Capture, ComponentRunnerInputs, HostContext, OpenFile, PlaybackHandoff,
    PreparedComponentInputs,
};
use ui_core::bound_dispatch::BoundActionAddress;
use ui_core::headless::FrameHarness;
use ui_core::launch::LaunchError;
use ui_core::permissions::{PermissionContext, Role, SharedPermissions, UserId};

pub(super) const AUTHOR: &str = "test-author";
use ui_core::record::{RecordCheckout, RecordId, RecordStore};

use super::app::{
    DATA_DIR_INSTANCE, ResourceApp, data_dir_host, definition, hand_launch, prepare_on,
    role_permissions, settle_launch,
};
use super::launch::LaunchInput;
use super::root::ResourceRoot;

pub(super) const INSTANCE: u64 = 7;
/// The workspace a test opens the app in unless it names one.
pub(super) const TEST_WORKSPACE: &str = "test-workspace";
/// The device a test app runs on unless it names another.
const TEST_DEVICE: &str = "test-device";

/// A fresh directory for a test's or a capture's private profile, removed
/// with it. It is in memory where the system has one
/// ([`tilo_paths::memory_temp_dir`], `/dev/shm` on Linux):
/// the store still syncs every write as it does for a person, but a sync of
/// a profile that is deleted moments later then costs nothing, where on a
/// shared disk it waits for every other writer's data to reach it, and a
/// capture syncs on each staged edit. Elsewhere it is the system's
/// temporary directory.
pub(crate) fn profile_directory() -> std::io::Result<tempfile::TempDir> {
    if let Some(memory) = tilo_paths::memory_temp_dir()
        && let Ok(directory) = tempfile::Builder::new()
            .prefix("tilo-profile-")
            .tempdir_in(memory)
    {
        return Ok(directory);
    }
    tempfile::tempdir()
}

enum Profile {
    /// A private temporary profile, removed with the app, the workspace the app
    /// opens in and the device it runs on.
    Temporary(Rc<tempfile::TempDir>, String, String),
    /// An explicit `--data-dir` profile the app itself would open.
    DataDir(PathBuf),
    /// Another window a launcher opens on a temporary profile, in a workspace on
    /// a device:
    /// an instance of its own, whose fallback record is that window's own
    /// unless the app keeps one per workspace, as the native host decides.
    Window(Rc<tempfile::TempDir>, String, String, u64),
}

/// A component prepared on a private or explicit profile at `root` keeps
/// its settings and its files in that profile too, never in the person's
/// session document or file areas.
pub(crate) fn in_profile(
    mut prepared: PreparedComponentInputs,
    root: &std::path::Path,
    app_id: &str,
) -> PreparedComponentInputs {
    prepared
        .context
        .provide_settings(ui_core::storage::SettingsDocument::Profile(
            root.to_path_buf(),
        ));
    prepared
        .context
        .provide_files(ui_core::storage::AppFiles::at(
            ui_core::storage::AppPaths::under(root.join("files"), app_id),
        ));
    prepared
}

pub(crate) fn data_dir_root(directory: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf())
}

pub(crate) fn data_dir_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    super::app::profile_content(
        &std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf()),
    )
}

/// The content secret of `workspace`'s members, which every window of it in
/// this process holds.
fn workspace_secret(workspace: &str) -> Option<tilo_seal::SymmetricKey> {
    static SECRETS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, tilo_seal::SymmetricKey>>,
    > = std::sync::OnceLock::new();
    let generated = tilo_seal::SymmetricKey::generate().ok()?;
    Some(
        SECRETS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(workspace.to_owned())
            .or_insert(generated)
            .clone(),
    )
}

impl Profile {
    /// The workspace the app opens in, on a temporary profile.
    fn workspace(&self) -> Option<&str> {
        match self {
            Self::Temporary(_, workspace, _) | Self::Window(_, workspace, ..) => Some(workspace),
            Self::DataDir(_) => None,
        }
    }

    /// The profile's settings, which the app's typed settings use instead of
    /// the person's.
    fn settings(&self) -> ui_core::storage::SettingsDocument {
        ui_core::storage::SettingsDocument::Profile(self.root())
    }

    /// The directory the profile keeps everything in.
    fn root(&self) -> PathBuf {
        match self {
            Self::Temporary(directory, ..) | Self::Window(directory, ..) => {
                directory.path().to_path_buf()
            }
            Self::DataDir(directory) => data_dir_root(directory),
        }
    }

    /// The profile's content store, which the app keeps handed files and
    /// other bytes its record names in, instead of the person's. It stands in
    /// for the device store of the one workspace the app is mounted in, so a
    /// content reference resolves in it as on a device.
    fn content(&self) -> tilo_content::ContentStore {
        match self {
            Self::Temporary(directory, ..) | Self::Window(directory, ..) => {
                window_content(directory.path())
            }
            Self::DataDir(directory) => data_dir_content(directory).for_one_workspace(),
        }
    }
}

/// The content store of the temporary profile `directory`, as a window of it
/// holds it: one workspace's, named by plain address.
pub(super) fn window_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    super::app::profile_content(directory).for_one_workspace()
}

/// One framework app mounted headlessly: a test, a fixture seeder or an
/// inspection tool drives it through the real runtime.
pub struct HeadlessApp<A: ResourceApp> {
    harness: FrameHarness,
    /// Whether the app is closed: its record written and its components closed, once.
    closed: bool,
    records: RecordStore,
    checkout: RecordCheckout,
    instance: u64,
    profile: Profile,
    role: Role,
    /// Who the app is open for: the author of its edits and the person its
    /// person records belong to.
    author: String,
    /// The last effect [`Self::effect`] invoked.
    last_effect: Option<tilo_actions::InvocationId>,
    /// What a host replicating the record to a workspace published of it
    /// ([`Self::shared_in_workspace`]); `None` while nobody replicates it.
    workspace: Option<ui_core::state::DocumentFrontier>,
    /// Shared with the host: changing it is a role change the running app
    /// sees, as when the record's owner changes someone's access.
    permissions: SharedPermissions,
    /// The window is a capture: every motion in it reads arrived while this
    /// is held, so what it photographs is the window at rest.
    _settled: Option<ui_core::motion::testing::Settled>,
    marker: std::marker::PhantomData<A>,
}

/// Stand in for the hosts keeping the records `A` declares kept by them
/// ([`Records::kept_by`](super::Records::kept_by)), as the session does:
/// each provider's settings sections are held over the profile's settings
/// document `settings`, and what the window's controls invoke there is
/// changed as `author`'s.
fn serve_kept<A: ResourceApp>(
    harness: &mut FrameHarness,
    settings: &ui_core::storage::SettingsDocument,
    author: &str,
) {
    let mut declared = super::Records::declaring();
    A::records(&mut declared);
    let mut providers: Vec<(&'static str, Vec<super::Declare>)> = Vec::new();
    for (provider, declare) in declared.into_kept() {
        match providers.iter_mut().find(|(known, _)| *known == provider) {
            Some((_, kept)) => kept.push(declare),
            None => providers.push((provider, vec![declare])),
        }
    }
    for (provider, kept) in providers {
        let records = super::SettingsRecords::in_document(
            super::app::hooks::<A>().id(),
            &kept,
            settings.clone(),
        );
        harness.serve_provider(provider, records.served_for(author));
    }
}

pub(super) fn operation(error: impl std::fmt::Display) -> LaunchError {
    LaunchError::Operation(error.to_string())
}

impl<A: ResourceApp> HeadlessApp<A> {
    /// The content store the app's profile keeps the bytes its record names
    /// in: what `cx.content()` reads and writes on this device.
    #[must_use]
    pub fn content(&self) -> tilo_content::ContentStore {
        self.profile.content()
    }

    /// The app as its owner opens it on a fresh private profile.
    pub fn new() -> Result<Self, LaunchError> {
        Self::with_role(Role::Owner)
    }

    /// The app as its owner opens it on a fresh private profile, in `workspace`:
    /// one of several windows of the app in one process, each in a workspace of
    /// its own.
    pub fn in_workspace(workspace: &str) -> Result<Self, LaunchError> {
        Self::fresh(workspace, Role::Owner, AUTHOR)
    }

    /// The app as its owner opens it on a fresh profile on the device
    /// `device`: one of two devices sharing a record, each carrying out what
    /// is queued for it.
    pub fn on_device(device: &str) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), device.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app as a participant with `role` opens it on a fresh profile.
    pub fn with_role(role: Role) -> Result<Self, LaunchError> {
        Self::fresh(TEST_WORKSPACE, role, AUTHOR)
    }

    /// The app as `person` with `role` opens it on a fresh private profile,
    /// in `workspace`.
    fn fresh(workspace: &str, role: Role, person: &str) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, workspace.to_owned(), TEST_DEVICE.to_owned()),
            role,
            person,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app as `person` (the hex of a root public key) opens it on a
    /// fresh profile: one of that person's devices, whose shared records
    /// ([`RecordEnv::shared_person`](super::RecordEnv::shared_person)) are
    /// theirs.
    pub fn for_person(person: &str) -> Result<Self, LaunchError> {
        Self::fresh(TEST_WORKSPACE, Role::Owner, person)
    }

    /// [`Self::for_person`], with the app open in the workspace `workspace`
    /// (a workspace id), whose records sync on their routes.
    pub fn for_person_in(person: &str, workspace: &str) -> Result<Self, LaunchError> {
        Self::fresh(workspace, Role::Owner, person)
    }

    /// Open a fresh person profile with a service lent by its mounting host.
    /// The loan follows production `HostContext::lend` and is not retained on
    /// reopen: the next host must authorize and lend its own service again.
    pub fn for_person_with_lent<S: 'static>(person: &str, service: S) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            person,
            &[],
            None,
            None,
            None,
            move |context| context.lend(service),
        )
    }

    /// The app as `person` opens it in a host that keeps the person's
    /// shared records itself, in `held`, and lends them to the apps it
    /// shows in its process (a session showing Settings).
    pub fn for_person_shown_by(
        person: &str,
        held: &ui_core::record::person::PersonRecords,
    ) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        let held = held.clone();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            person,
            &[],
            None,
            None,
            None,
            move |context| {
                if let Some(own) = context.person_records().cloned() {
                    context.provide_person_records(own.sharing(&held));
                }
            },
        )
    }

    /// How many of the person's records the window holds open, the records
    /// a settings section keeps among them: a section's opens only when it
    /// is first written or stepped.
    pub fn open_person_records(&mut self) -> usize {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.person_owners().len())
            .unwrap_or_default()
    }

    /// The person's own record of `P` the running app keeps, as its views
    /// open it.
    pub fn person<P: Resource>(&mut self) -> Option<super::PersonRecord<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.person::<P>())
            .flatten()
    }

    /// The record `P` every app of the workspace opens that `owner` keeps, as
    /// the app's views open it.
    pub fn shared_workspace<P: Resource>(
        &mut self,
        owner: &'static str,
    ) -> Option<super::WorkspaceRecord<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.shared_workspace::<P>(owner))
            .flatten()
    }

    /// The record `id` that `owner` keeps, as the app's views open it.
    pub fn shared_record<P: Resource>(
        &mut self,
        owner: &'static str,
        id: &str,
    ) -> Option<super::WorkspaceRecord<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.shared_record::<P>(owner, id))
            .flatten()
    }

    /// The person's shared record `P`, as the app's views open it.
    pub fn shared_person<P: tilo_resource::SharedRecord>(
        &mut self,
    ) -> Option<super::PersonRecord<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.shared_person::<P>())
            .flatten()
    }

    /// The frame `--shots --scenario` writes for `scenario` (`None` for a
    /// plain `--shots`): the app built for a capture on a fresh profile,
    /// from the scenario's fixture, with its actions applied and its view
    /// state staged, at the scenario's viewport.
    pub fn capture(scenario: Option<&str>) -> Result<Self, LaunchError> {
        Self::capture_at(scenario, A::shot_viewport(scenario))
    }

    /// [`Self::capture`] at `viewport`, as `--shots --viewport` draws it.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn capture_at(scenario: Option<&str>, viewport: (u32, u32)) -> Result<Self, LaunchError> {
        let capture = Capture::new(scenario.map(str::to_owned));
        Self::capture_on(capture, viewport, &[])
    }

    /// The window `--headless-drive` drives when it names `scenario`
    /// (`"scenario"` in the spec, or `--scenario NAME` beside it): built and
    /// staged as [`Self::capture`] is, from the scenario's fixtures, but the
    /// actions invoked on it run, as the drive's `invoke` steps run them. It
    /// starts no work on its own, so what it shows stays the scenario's.
    /// A drive that names no scenario drives the app someone uses
    /// ([`Self::new`]).
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn drive(scenario: &str) -> Result<Self, LaunchError> {
        Self::drive_at(scenario, A::shot_viewport(Some(scenario)))
    }

    /// [`Self::drive`] at `viewport`, as [`Self::capture_at`] is a capture's: the scenario's
    /// fixtures staged in a window of the size the test drives. Unlike a capture, the window
    /// moves: a press plays the motion the theme gives it.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn drive_at(scenario: &str, viewport: (u32, u32)) -> Result<Self, LaunchError> {
        let capture = Capture::driven(Some(scenario.to_owned()));
        Self::capture_on(capture, viewport, &[])
    }

    /// [`Self::capture`] of the app launched with `arguments`, as
    /// `--shots --scenario NAME ARGUMENTS` draws it: a capture of a file the
    /// app was launched on.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn capture_launched(
        scenario: Option<&str>,
        arguments: &[&str],
    ) -> Result<Self, LaunchError> {
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        let capture = Capture::new(scenario.map(str::to_owned));
        Self::capture_on(capture, A::shot_viewport(scenario), &arguments)
    }

    fn capture_on(
        capture: Capture,
        viewport: (u32, u32),
        arguments: &[String],
    ) -> Result<Self, LaunchError> {
        let scenario = capture.scenario().map(str::to_owned);
        let scenario = scenario.as_deref();
        let profile = Rc::new(profile_directory().map_err(operation)?);
        let mut app = Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            arguments,
            None,
            None,
            Some((capture, viewport)),
            |_| {},
        )?;
        super::stage::stage::<A>(app.harness(), scenario)?;
        Ok(app)
    }

    /// The app launched by its owner with `arguments`, as `app ARGUMENTS`
    /// on the command line: on the record [`ResourceApp::launch_record`]
    /// names, after its [`ResourceApp::launch`] actions. The app's own
    /// [options](ResourceApp::OPTIONS) and [switches](ResourceApp::FLAGS)
    /// among them reach it as options.
    pub fn launched(arguments: &[&str]) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &arguments,
            None,
            None,
            None,
            |_| {},
        )
    }

    /// Launch on a private receiving profile after hydrating an existing
    /// record in the same workspace. The production installer validates its
    /// identity before the app can initialize or open it.
    pub fn launched_replica(
        arguments: &[&str],
        document: &mut tilo_resource::Doc,
    ) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        let native =
            ui_core::application::NativeProfile::open(&profile.path().join("state"), None)?;
        // A test thread may wait: the record is on the disk once installed.
        let _installed = ui_core::record::transfer::install(
            native.store(),
            super::app::hooks::<A>().id(),
            TEST_WORKSPACE,
            document,
        )
        .map_err(operation)?;
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &arguments,
            None,
            None,
            None,
            |_| {},
        )
    }

    /// A new window of the app on a fresh profile that already holds
    /// `documents`, each a record a window of the app kept in the test workspace
    /// (`window_record`): how an app that now keeps one record per workspace
    /// first opens where it used to keep one per window.
    pub fn with_window_records(documents: &mut [tilo_resource::Doc]) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        {
            let native =
                ui_core::application::NativeProfile::open(&profile.path().join("state"), None)?;
            for document in documents {
                let _installed = ui_core::record::transfer::install(
                    native.store(),
                    super::app::hooks::<A>().id(),
                    TEST_WORKSPACE,
                    document,
                )
                .map_err(operation)?;
            }
        }
        Self::open(
            Profile::Window(
                profile,
                TEST_WORKSPACE.to_owned(),
                TEST_DEVICE.to_owned(),
                INSTANCE + 1,
            ),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// A new window of the app on a fresh profile whose workspace already
    /// holds `documents`, each stamped with the identity beside it and named
    /// in the app's workspace index by its name: records this device was
    /// given from elsewhere — a window on another device, an agent in a
    /// hosted workspace — rather than started here. What a relay publishes
    /// reaches a reader this way.
    pub fn with_named_records(
        documents: &mut [(&str, ui_core::record::RecordId, tilo_resource::Doc)],
    ) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        {
            let native =
                ui_core::application::NativeProfile::open(&profile.path().join("state"), None)?;
            let app = super::app::hooks::<A>().id();
            let kind = super::app::definition::<A>().record_kind.clone();
            let mut index = automerge::AutoCommit::new();
            for (name, id, document) in documents.iter_mut() {
                if !ui_core::record::init_scoped(
                    document,
                    id,
                    &kind,
                    TEST_WORKSPACE,
                    Some(TEST_WORKSPACE),
                ) {
                    return Err(operation("a record could not be stamped for the workspace"));
                }
                let (checkout, _landing) = ui_core::record::transfer::install(
                    native.store(),
                    app,
                    TEST_WORKSPACE,
                    document,
                )
                .map_err(operation)?;
                ui_core::record::index::reference(&mut index, &checkout.id, &checkout.kind);
                ui_core::record::index::set_name(&mut index, &checkout.id, Some(name));
            }
            let _landing = ui_core::record::transfer::install_workspace_document(
                native.store(),
                app,
                TEST_WORKSPACE,
                &mut index,
            )
            .map_err(operation)?;
        }
        Self::open(
            Profile::Window(
                profile,
                TEST_WORKSPACE.to_owned(),
                TEST_DEVICE.to_owned(),
                INSTANCE + 2,
            ),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// A new window of an app that [keeps one record per workspace](ResourceApp::WORKSPACE_RECORD)
    /// on a fresh profile whose workspace record is already `document`: how a
    /// workspace an earlier version of the app wrote opens.
    pub fn with_workspace_record(document: &mut tilo_resource::Doc) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        {
            let native =
                ui_core::application::NativeProfile::open(&profile.path().join("state"), None)?;
            let _installed = ui_core::record::transfer::install_workspace_document(
                native.store(),
                super::app::hooks::<A>().id(),
                TEST_WORKSPACE,
                document,
            )
            .map_err(operation)?;
        }
        Self::open(
            Profile::Window(
                profile,
                TEST_WORKSPACE.to_owned(),
                TEST_DEVICE.to_owned(),
                INSTANCE + 1,
            ),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app launched with `arguments` to continue `handoff`'s playback,
    /// as the session launches the target of a media handoff.
    pub fn handed_off(arguments: &[&str], handoff: PlaybackHandoff) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &arguments,
            None,
            Some(handoff),
            None,
            |_| {},
        )
    }

    /// The app handed `file` to open by a host that already chose its
    /// record (the default one here), as the shell's "open with" does.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure, and the refusal
    /// of a file of a type the app does not [open](ResourceApp::OPENS).
    pub fn handed(file: OpenFile) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            Some(file),
            None,
            None,
            |_| {},
        )
    }

    /// The app replaying the recording `file` holds, as the session opens a
    /// recording a report carries: in a private window of its own.
    pub fn replaying(file: OpenFile) -> Result<Self, LaunchError> {
        let profile = Rc::new(profile_directory().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            Some(file),
            None,
            None,
            HostContext::provide_private,
        )
    }

    /// The app on the profile `--data-dir directory` opens, as its owner.
    /// Only one writer may hold a profile: this fails while the app (or
    /// another `HeadlessApp`) has it open.
    pub fn open_data_dir(directory: &std::path::Path) -> Result<Self, LaunchError> {
        Self::open(
            Profile::DataDir(directory.to_path_buf()),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    fn open(
        profile: Profile,
        role: Role,
        author: &str,
        arguments: &[String],
        file: Option<OpenFile>,
        handoff: Option<PlaybackHandoff>,
        capture: Option<(Capture, (u32, u32))>,
        configure: impl FnOnce(&mut HostContext),
    ) -> Result<Self, LaunchError> {
        let mut configure = Some(configure);
        // A capture photographs the window at rest: a surface drawn for one
        // is the surface settled, not the frame of its arrival the moment the
        // shutter opens.
        let settled = capture
            .as_ref()
            .filter(|(capture, _)| !capture.is_driven())
            .map(|_| ui_core::motion::testing::settled());
        let opened = open_app(
            &Opening::of::<A>(),
            Launch {
                profile,
                role,
                author,
                arguments,
                file,
                handoff,
                capture,
            },
            &mut |context| {
                if let Some(configure) = configure.take() {
                    configure(context);
                }
            },
        )?;
        let mut harness = opened.harness;
        serve_kept::<A>(&mut harness, &opened.profile.settings(), author);
        Ok(Self {
            harness,
            closed: false,
            records: opened.records,
            checkout: opened.checkout,
            instance: opened.instance,
            profile: opened.profile,
            role,
            author: author.to_owned(),
            last_effect: None,
            workspace: None,
            permissions: opened.permissions,
            _settled: settled,
            marker: std::marker::PhantomData,
        })
    }

    /// Hand the records the app declares kept by other providers
    /// ([`Records::kept_by`](super::Records::kept_by)) to `host`, in place of
    /// the test's own stand-in: a host under test (a page keeping the
    /// person's settings) is told the window's frame, the profile's settings
    /// document and the person at the window, and serves them as it does
    /// for a live window.
    pub fn kept_by_host(
        &mut self,
        host: impl FnOnce(&mut FrameHarness, &ui_core::storage::SettingsDocument, &str),
    ) {
        let mut declared = super::Records::declaring();
        A::records(&mut declared);
        for (provider, _) in declared.into_kept() {
            self.harness.stop_serving_provider(provider);
        }
        host(&mut self.harness, &self.profile.settings(), &self.author);
    }

    /// The app's file areas in this profile, which the running app writes
    /// instead of the person's.
    pub fn files(&self) -> ui_core::storage::AppFiles {
        ui_core::storage::AppFiles::at(ui_core::storage::AppPaths::under(
            self.profile.root().join("files"),
            super::app::hooks::<A>().id(),
        ))
    }

    /// The app's typed settings in this profile's settings, which
    /// the running app follows: a write here is what another window, the
    /// Settings app or another of the person's devices writing them does.
    pub fn settings<T: ui_core::storage::SettingsSchema>(&self) -> ui_core::storage::Settings<T> {
        ui_core::storage::Settings::for_app(super::app::hooks::<A>().id())
            .in_document(self.profile.settings())
    }

    /// Change the participant's role while the app runs, as the record's
    /// owner changing their access does. Takes effect on the next frame,
    /// which this runs.
    pub fn set_role(&mut self, role: Role) {
        self.permissions
            .set(PermissionContext::member(self.author.as_str(), role));
        self.role = role;
        self.harness().relayout();
    }

    /// The participant's permissions in force now.
    #[must_use]
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }

    /// Change the participant's permissions while the app runs, as the host
    /// does when the workspace's grants change. Takes effect on the next frame,
    /// which this runs.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        self.permissions.set(permissions);
        self.harness().relayout();
    }

    /// Take the participant out of the workspace while the app runs: no role and
    /// no grant left, as when the owner removes them.
    pub fn leave_workspace(&mut self) {
        self.permissions
            .set(PermissionContext::stranger(self.author.as_str()));
        self.harness().relayout();
    }

    /// Close the app (flushing its record) and open it again on the same
    /// profile, as a relaunch would.
    pub fn reopen(self) -> Result<Self, LaunchError> {
        self.relaunch(&[])
    }

    /// Close the app and launch it again on the same profile with
    /// `arguments`, as `app ARGUMENTS` run again would.
    pub fn relaunch(mut self, arguments: &[&str]) -> Result<Self, LaunchError> {
        self.close()?;
        let profile = match &self.profile {
            Profile::Temporary(directory, workspace, device) => {
                Profile::Temporary(Rc::clone(directory), workspace.clone(), device.clone())
            }
            Profile::Window(directory, workspace, device, window) => Profile::Window(
                Rc::clone(directory),
                workspace.clone(),
                device.clone(),
                *window,
            ),
            Profile::DataDir(directory) => Profile::DataDir(directory.clone()),
        };
        let role = self.role;
        let author = std::mem::take(&mut self.author);
        // Release this instance's record writer before the next one opens it.
        drop(self);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(profile, role, &author, &arguments, None, None, None, |_| {})
    }

    /// Close the app and open a new window of it on the same profile and in
    /// the same workspace, as the launcher opens one: an instance it never had,
    /// whose fallback record is a fresh record of that window's own unless
    /// the app [keeps one per workspace](ResourceApp::WORKSPACE_RECORD).
    pub fn another_window(mut self) -> Result<Self, LaunchError> {
        self.close()?;
        let window = self.instance.wrapping_add(1).max(INSTANCE + 1);
        let profile = match &self.profile {
            Profile::Temporary(directory, workspace, device)
            | Profile::Window(directory, workspace, device, _) => Profile::Window(
                Rc::clone(directory),
                workspace.clone(),
                device.clone(),
                window,
            ),
            Profile::DataDir(_) => {
                return Err(operation("a --data-dir profile has one window"));
            }
        };
        let role = self.role;
        let author = std::mem::take(&mut self.author);
        drop(self);
        Self::open(profile, role, &author, &[], None, None, None, |_| {})
    }

    /// Open another window of the app beside this one, on the same profile
    /// and in the same workspace, while this one stays open: two windows a
    /// person has open at once. Its fallback record is the workspace's when the
    /// app [keeps one per workspace](ResourceApp::WORKSPACE_RECORD), so both show one
    /// record; [`Self::deliver_changes_to`] carries edits between them.
    pub fn open_window(&self) -> Result<Self, LaunchError> {
        let window = self.instance.wrapping_add(1).max(INSTANCE + 1);
        let profile = match &self.profile {
            Profile::Temporary(directory, workspace, device)
            | Profile::Window(directory, workspace, device, _) => Profile::Window(
                Rc::clone(directory),
                workspace.clone(),
                device.clone(),
                window,
            ),
            Profile::DataDir(_) => {
                return Err(operation("a --data-dir profile has one window"));
            }
        };
        Self::open(
            profile,
            self.role,
            &self.author,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// Deliver this window's changes to the record to `other`, a window
    /// showing the same record, as the workspace's broker fans each window's
    /// document changes out to every other window on the record; `other`
    /// takes them in and draws its next frame, without reopening. Whether
    /// `other` changed.
    pub fn deliver_changes_to(&mut self, other: &mut Self) -> Result<bool, String> {
        if self.checkout().id != other.checkout().id {
            return Err("the windows show different records".into());
        }
        let mut frontier = ui_core::DocumentFrontier::default();
        let (changes, _) = self
            .records
            .replication_delta(&self.checkout().id, &mut frontier)
            .map_err(|error| error.to_string())?;
        let took_in = other
            .records
            .merge_peer_changes(other.checkout(), changes)?;
        other.harness().relayout();
        Ok(took_in)
    }

    /// Deliver what this window holds of `id`, a record the app keeps
    /// beside its own (a part of a chain), to `other`, which holds it too,
    /// as the workspace's record sync sends a device a record it opened: `other`
    /// takes it in and draws its next frame. Whether `other` changed.
    pub fn deliver_record_to(&mut self, other: &mut Self, id: &RecordId) -> Result<bool, String> {
        let checkout = other
            .records
            .open_records()
            .into_iter()
            .find(|checkout| checkout.id == *id)
            .ok_or_else(|| format!("the other window does not hold {id}"))?;
        let mut frontier = ui_core::DocumentFrontier::default();
        let (changes, _) = self
            .records
            .replication_delta(id, &mut frontier)
            .map_err(|error| error.to_string())?;
        let took_in = other.records.merge_peer_changes(&checkout, changes)?;
        other.harness().relayout();
        Ok(took_in)
    }

    /// Take this window's records to come from its workspace's hosts, as a
    /// page's do ([`ui_core::record::RecordStore::ask_workspace_for_missing`]):
    /// a record not here is asked for, and new only once they answered.
    pub fn asks_the_workspace(&mut self) {
        self.records.ask_workspace_for_missing();
    }

    /// What this window asks its workspace's hosts for
    /// ([`ui_core::record::RecordStore::wanted_records`]).
    #[must_use]
    pub fn wanted_records(&self) -> Vec<RecordId> {
        self.records.wanted_records()
    }

    /// The workspace's hosts sent everything they hold of `id`, a record this
    /// window asked for, or none is connected to ask
    /// ([`ui_core::record::RecordStore::note_workspace_answered`]).
    pub fn workspace_answered(&mut self, id: RecordId) {
        self.records.note_workspace_answered(id);
    }

    /// The workspace's default records of the app's windows kept on this
    /// profile, as [`SeedCx::window_records`](super::SeedCx::window_records)
    /// reads them.
    pub fn window_records(&self) -> Vec<(RecordId, tilo_resource::Doc)> {
        self.records.window_records(&definition::<A>().record_kind)
    }

    /// Open what a launch naming `arguments` opens in this window, as the
    /// app's view does with [`RecordEnv::open_here`](super::RecordEnv::open_here),
    /// and run the frame that shows it.
    ///
    /// # Errors
    /// Returns why the record cannot be opened: the window keeps the one it
    /// shows.
    pub fn open_here(&mut self, arguments: &[&str]) -> Result<(), String> {
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        let outcome = self
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.run.open_here(&arguments))
            .ok_or_else(|| format!("{} is not hosted here", super::app::hooks::<A>().title()))??;
        self.harness().relayout();
        outcome
            .get()
            .unwrap_or_else(|| Err("the host did not take the request".into()))
    }

    pub fn record(&self) -> &RecordId {
        &self.checkout().id
    }

    /// The record the window shows now: the one it was opened on, or one
    /// its view opened in its place ([`RecordEnv::open_here`](super::RecordEnv::open_here)).
    fn checkout(&self) -> &RecordCheckout {
        self.harness.component_record().unwrap_or(&self.checkout)
    }

    /// Consume the hosted window's explicit save-retry request, as its
    /// record owner does before reporting durability again.
    pub fn take_save_retry(&mut self) -> Result<bool, String> {
        self.harness()
            .with_widget_mut(|root: &mut super::root::ResourceRoot| {
                super::root::RootApp::take_save_retry(root)
            })
            .ok_or_else(|| "the framework root is not mounted".to_owned())
    }

    /// Make every edit so far durable, as closing the app does.
    pub fn flush(&mut self) -> Result<(), LaunchError> {
        self.harness().flush_component_records().map_err(operation)
    }

    fn close(&mut self) -> Result<(), LaunchError> {
        if !self.closed {
            self.harness.flush_component_records().map_err(operation)?;
            self.harness.close_components();
            self.closed = true;
        }
        Ok(())
    }

    /// The custom surface of type `S` the view hosts, without running a
    /// frame: for a test reading it, or setting it up, between the events it
    /// sends.
    #[allow(
        clippy::expect_used,
        reason = "a test naming a surface its app does not host is wrong"
    )]
    pub fn surface_mut<S: 'static>(&mut self) -> &mut S {
        surface_in::<A, S>(self.harness()).expect("the view hosts this surface")
    }

    /// The headless frame harness, for input-level tests (`click_named`,
    /// `type_text`, `targets`).
    pub fn harness(&mut self) -> &mut FrameHarness {
        &mut self.harness
    }

    /// Every text the window shows now, as assistive technology reads it:
    /// what the app's view drew of its state.
    pub fn texts(&mut self) -> Vec<String> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| ui_core::a11y::texts(root.app_view()))
            .unwrap_or_default()
    }

    /// The records this window's store has open: what the app process opened
    /// itself, which an app the session serves opens none of.
    pub fn open_records(&self) -> Vec<RecordId> {
        self.records
            .open_records()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect()
    }

    /// The whole document of `id` as this window holds it: its own record,
    /// or one it keeps beside it (a part of a chain).
    pub fn record_document(&self, id: &RecordId) -> Option<Vec<u8>> {
        self.records.read(id, |doc| doc.clone().save())
    }

    /// Submit `submit` through the running view's own dispatcher, as a
    /// control's press or a field's edit hands it over: over the session's
    /// connection when the app is the session's, into the record here when
    /// it is not.
    pub fn submit(&mut self, submit: impl super::Submit<A::Resource>) -> Result<(), String> {
        let refused = self
            .harness
            .with_widget_mut(|root: &mut ResourceRoot| {
                let Some(typed) = root.typed::<A::Resource>() else {
                    return Err("the window's root is another resource type".to_owned());
                };
                typed.dispatch.submit(submit);
                Ok(typed.dispatch.refused().get().map(|refused| refused.reason))
            })
            .ok_or_else(|| "the window's root is not reachable".to_owned())?
            .and_then(|refused| refused.map_or(Ok(()), Err));
        self.settle();
        refused
    }

    /// Visit the custom surface of type `S` the app's view hosts
    /// ([`ViewCx::surface`](super::ViewCx::surface)), as the frame shows it
    /// now; `None` when the view hosts none. The frame is laid out again
    /// after the visit, so what the visit changed is what the next frame
    /// draws.
    pub fn with_surface<S: 'static, T>(&mut self, visit: impl FnOnce(&mut S) -> T) -> Option<T> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                super::root::with_surface::<A::Resource, S, T>(root, visit)
            })
            .flatten()
    }

    /// Invoke a declared action through the host door, exactly as a control,
    /// the palette, an agent or a script does.
    pub fn invoke(&mut self, invoke: Invoke<A::Resource>) -> Result<(), tilo_actions::DoorError> {
        self.invoke_as(invoke, false)
    }

    /// Invoke an action of the resource nested in this one at `resource` —
    /// a child resource a view edits through [`Dispatcher::child`](super::Dispatcher::child)
    /// — through the host door, as the palette, an agent or a script
    /// addresses it.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_child<C: Resource>(
        &mut self,
        resource: &str,
        invoke: Invoke<C>,
    ) -> Result<(), tilo_actions::DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::DoorError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(resource, id),
        };
        let harness = self.harness();
        harness.invoke_bound_action(&address, &arguments)?;
        harness.relayout();
        Ok(())
    }

    /// [`Self::invoke`] after the caller confirmed it, as the host's
    /// confirmation prompt does for a destructive action.
    ///
    /// # Errors
    /// As [`Self::invoke`], except that no confirmation is required.
    pub fn invoke_confirmed(
        &mut self,
        invoke: Invoke<A::Resource>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.invoke_as(invoke, true)
    }

    /// Discover the actions this participant may invoke on the running app,
    /// including item and child resources. Descriptors include the complete
    /// parameter, permission, confirmation and result contracts; discovery
    /// never invokes an action. Addresses stay stable across relayouts.
    ///
    /// A saved descriptor is not an authority grant: every call checks the
    /// running app's current permissions and resource state again.
    pub fn actions(
        &mut self,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, tilo_actions::DoorError> {
        let instance = self.instance;
        let resources = self.harness().bound_resources_of(Some(instance), None)?;
        let mut actions = tilo_actions::door::survey(&resources);
        actions.sort_by(|left, right| {
            (&left.address.resource, &left.address.action)
                .cmp(&(&right.address.resource, &right.address.action))
        });
        Ok(actions)
    }

    /// What an agent sees of the running app: every published action with
    /// its address, label, description and typed parameters, in a stable
    /// order. Snapshot it so a change shows how it altered the vocabulary an
    /// agent, a script or a screen reader works with, and what it costs in
    /// context.
    pub fn agent_surface(&mut self) -> serde_json::Value {
        let mut resources = self.harness().resources();
        // Another provider publishes what it runs, not this window.
        resources.iter_mut().for_each(published_here);
        let mut rows: Vec<serde_json::Value> = tilo_actions::door::survey(&resources)
            .into_iter()
            .map(|descriptor| {
                serde_json::json!({
                    "resource": descriptor.address.resource.as_str(),
                    "action": descriptor.address.action,
                    "resource_kind": descriptor.resource_kind,
                    "label": descriptor.action_label,
                    "description": descriptor.description,
                    "operation": descriptor.kind,
                    "params": descriptor.params,
                })
            })
            .collect();
        rows.sort_by_key(|row| (row["resource"].to_string(), row["action"].to_string()));
        serde_json::Value::Array(rows)
    }

    /// Run a view or effect operation (`look`, `export`) through the host
    /// door, as an agent or script does, and answer with what it produced;
    /// `None` while it is still running.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn operate(
        &mut self,
        invoke: Invoke<A::Resource>,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::DoorError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id),
        };
        let harness = self.harness();
        let started = harness.invoke_bound_action(&address, &arguments)?;
        harness.relayout();
        Ok(started.completed())
    }

    /// Invoke one of the app's effects through the host door, as a button,
    /// the palette or an agent does, and wait for its answer: what it
    /// reported at once, or the result of the job it started. Progress the
    /// job reported is in [`FrameHarness::job_updates`].
    pub fn effect<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
            false,
        )
    }

    /// [`Self::effect`] invoked by `actor` — an agent or a collaborator the
    /// host admitted for the effect — rather than the person at this
    /// device: what the effect submits is applied as theirs, and only as
    /// far as the effect they were admitted for allows.
    ///
    /// # Errors
    /// As [`Self::effect`]; the refusal of an edit the effect submitted
    /// fails it.
    pub fn effect_by<E: Effect>(
        &mut self,
        actor: &str,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_invoked(
            Some(tilo_actions::ActionInvoker::actor(actor)),
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
            false,
        )
    }

    /// Any published action of `resource`, by id with JSON arguments,
    /// delivered for `invoker` as the host delivers an admitted execution's
    /// invocation: what it builds is bound to the invoker's own participant
    /// view, and what it submits is applied as theirs.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn call_by(
        &mut self,
        invoker: tilo_actions::ActionInvoker,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        self.effect_invoked(Some(invoker), resource, id, &arguments, false)
    }

    /// [`Self::effect`] after the caller confirmed it, as the host's
    /// confirmation prompt does for a destructive effect.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn effect_confirmed<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
            true,
        )
    }

    /// [`Self::effect`] as the published child or live row `row` offers it,
    /// bound to its arguments ([`Effect::bound`]): the caller names only the
    /// row and the effect, as the palette or an agent does on an item.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn effect_on<E: Effect>(
        &mut self,
        row: &str,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(row, effect.id(), &serde_json::Map::new(), false)
    }

    /// [`Self::effect_on`] after the caller confirmed it.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn effect_on_confirmed<E: Effect>(
        &mut self,
        row: &str,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(row, effect.id(), &serde_json::Map::new(), true)
    }

    /// Start `effect` through the host door without waiting for it, as a
    /// caller that may cancel it does, confirmed: the invocation its job
    /// runs under.
    ///
    /// # Errors
    /// The host's refusal, the effect's failure, or that it answered at
    /// once and so started no job.
    pub fn start_effect<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::InvocationId, tilo_actions::DoorError> {
        self.start_effect_at(
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
        )
    }

    /// [`Self::start_effect`] as the published child or live row `row`
    /// offers it, bound to its arguments.
    ///
    /// # Errors
    /// As [`Self::start_effect`].
    pub fn start_effect_on<E: Effect>(
        &mut self,
        row: &str,
        effect: &E,
    ) -> Result<tilo_actions::InvocationId, tilo_actions::DoorError> {
        self.start_effect_at(row, effect.id(), &serde_json::Map::new())
    }

    fn start_effect_at(
        &mut self,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::InvocationId, tilo_actions::DoorError> {
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(resource, id),
        };
        let (invocation, started) = self
            .harness()
            .start_bound_job_for(None, &address, arguments, true)?;
        self.last_effect = Some(invocation);
        if started.completed().is_some() {
            return Err(tilo_actions::DoorError::Failed(format!(
                "`{id}` answered at once; it started no job"
            )));
        }
        Ok(invocation)
    }

    /// Cancel the job of an effect [`Self::start_effect`] started, as its
    /// caller does: what it ended as, `Cancelled` when it stopped. `None`
    /// when it already finished.
    pub fn cancel_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.harness().cancel_job(invocation)
    }

    /// What the job of the last [`Self::effect`] reported, in order: its
    /// progress, then its result.
    pub fn effect_progress(&mut self) -> Vec<tilo_actions::InvocationUpdate> {
        let Some(invocation) = self.last_effect else {
            return Vec::new();
        };
        self.harness().job_updates(invocation).to_vec()
    }

    fn effect_as(
        &mut self,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_invoked(None, resource, id, arguments, confirmed)
    }

    fn effect_invoked(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(resource, id),
        };
        // What the action declares it answers, checked as the broker checks
        // it: an answer outside it fails here as it fails live.
        let declared = self
            .harness()
            .resources()
            .into_iter()
            .find(|candidate| candidate.id.as_str() == resource)
            .and_then(|found| found.actions.into_iter().find(|spec| spec.id == id))
            .map(|spec| spec.contract.result.clone());
        let within = move |output: tilo_actions::ActionOutput| match &declared {
            Some(declared) => declared
                .validate(&output)
                .map(|()| output)
                .map_err(|error| {
                    tilo_actions::DoorError::Failed(format!(
                        "`{id}` answered outside the result it declares: {error}"
                    ))
                }),
            None => Ok(output),
        };
        let (invocation, started) = self
            .harness()
            .start_bound_job_for(invoker, &address, arguments, confirmed)?;
        self.last_effect = Some(invocation);
        let harness = self.harness();
        if let Some(output) = started.completed() {
            harness.relayout();
            return within(output);
        }
        let finished = |harness: &FrameHarness| {
            harness
                .job_updates(invocation)
                .iter()
                .find(|update| update.is_terminal())
                .cloned()
        };
        harness.wait_until(ui_core::time::Duration::from_secs(30), |harness| {
            finished(harness).is_some()
        });
        match finished(harness) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => within(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(tilo_actions::DoorError::Failed(error.message))
            }
            Some(_) => Err(tilo_actions::DoorError::Failed("cancelled".into())),
            None => Err(tilo_actions::DoorError::Failed(format!(
                "`{id}` did not finish"
            ))),
        }
    }

    /// Run `visit` on the app's widget of type `T` — a custom surface, a
    /// widget its view built — for assertions about view state the
    /// accessibility tree does not show, then run the frame after it.
    /// `None` when the view has no such widget.
    pub fn with_widget<T: 'static, O>(&mut self, visit: impl FnOnce(&mut T) -> O) -> Option<O> {
        self.harness()
            .with_widget_mut(|root: &mut super::root::ResourceRoot| {
                find_mut::<T>(root.content.as_mut()).map(visit)
            })
            .flatten()
    }

    /// What the system's persistent search index keeps of this window now:
    /// the entries the app publishes to it, found after it closes too.
    pub fn search_index_entries(&mut self) -> Vec<ui_core::search::index_publication::IndexEntry> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                super::root::RootSearch::search_index_entries(root)
            })
            .unwrap_or_default()
    }

    /// The last action refused here ([`super::Dispatcher::refused`]): by
    /// the door, or by the workspace after this window applied it.
    pub fn refused(&mut self) -> Option<super::Refused> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.dispatch.refused.get())
            .flatten()
    }

    /// The workspace refused `changes` (Automerge hashes) of this window's
    /// record, `reason` what a person is told (a host replicating the
    /// record reads it from the workspace's words with
    /// `tilo_relay_protocol::refusal::plain_reason`): withdraw them and what
    /// was built on them, as that host does, and run the frame that tells
    /// the app. Whether anything was withdrawn.
    pub fn refused_by_workspace(
        &mut self,
        changes: &[[u8; 32]],
        reason: &str,
    ) -> Result<bool, String> {
        let refused: Vec<_> = changes
            .iter()
            .map(|hash| ui_core::automerge::ChangeHash(*hash))
            .collect();
        let withdrawn = self
            .records
            .withdraw_refused(&self.checkout.id, &refused, reason)?;
        self.harness().relayout();
        self.settle();
        Ok(withdrawn.is_some())
    }

    /// What the workspace kept that the app said it did
    /// ([`super::Dispatcher::say_when_kept`]): the last such text.
    pub fn said(&mut self) -> Option<String> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.dispatch.said.get())
            .flatten()
    }

    /// From now on a host replicates this window's record to a workspace that
    /// decides its changes, as a browser page in a workspace does: what the app
    /// says it did waits for the workspace ([`Self::workspace_publishes`],
    /// [`Self::workspace_keeps`]).
    pub fn shared_in_workspace(&mut self) -> Result<(), String> {
        let (_, heads) = self
            .records
            .replication_snapshot(&self.checkout.id)
            .map_err(|error| error.to_string())?;
        let mut frontier = ui_core::state::DocumentFrontier::default();
        frontier.advance(heads);
        self.workspace = Some(frontier);
        self.records.workspace_settled(&self.checkout.id);
        Ok(())
    }

    /// The host publishes what changed since it last did, as its next pass
    /// does; whether anything was published.
    pub fn workspace_publishes(&mut self) -> Result<bool, String> {
        self.settle();
        let frontier = self
            .workspace
            .as_mut()
            .ok_or("the record is not shared in a workspace")?;
        let (delta, heads) = self
            .records
            .replication_delta(&self.checkout.id, frontier)
            .map_err(|error| error.to_string())?;
        frontier.advance(heads);
        Ok(!delta.is_empty())
    }

    /// The workspace answered every commit the host published, keeping them,
    /// and the frame that tells the app runs.
    pub fn workspace_keeps(&mut self) {
        self.records.workspace_settled(&self.checkout.id);
        self.harness().relayout();
        self.settle();
    }

    /// Run the frame the app runs after an event: edits the views and
    /// effects submitted are applied, and what they change is drawn. Every
    /// component syncs and settles, as after an event that reached it: a
    /// test may have changed what its views read past the framework
    /// ([`ui_core::component_inputs`]).
    pub fn settle(&mut self) {
        ui_core::component_inputs::forcing(|| {
            self.harness()
                .wait_until(ui_core::time::Duration::ZERO, |_| true);
        });
    }

    /// What the live window does once input stops, and no more: what its
    /// components' workers woke them for taken up, the frame the input
    /// asked for, control invocations dispatched and edits offered to the
    /// record's writer ([`FrameHarness::idle`]). Unlike [`Self::settle`] it
    /// forces no frame to sync and publish, so it costs what the live window
    /// costs: what the perf measurements run after each interaction.
    pub fn idle(&mut self) {
        self.harness().idle();
    }

    /// One frame of the live window, when it asks for one: what its
    /// components' workers woke them for taken up, as the window's frame
    /// takes it, then the frame a redraw, an animation or a due deadline
    /// asks for. Whether a frame ran.
    pub fn frame(&mut self) -> bool {
        self.harness().poll_frame().unwrap_or(false)
    }

    /// Invoke any published action of the resource by id with JSON
    /// arguments, as an agent's `tilo_invoke` does — the framework's own
    /// (`undo`, `history`, `render`) as well as the app's — and answer with
    /// what it reported.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call(
        &mut self,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_within(None, id, arguments)
    }

    /// [`Self::call`] as one agent run: every call naming the same `run`
    /// belongs to it, the way an agent's connection groups its invocations.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_in_run(
        &mut self,
        run: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        let ambient = tilo_actions::ActionContext::default().with_group(
            run,
            run,
            ui_core::history_source::steps::AGENT_RUN,
        );
        self.call_within(Some(&ambient), id, arguments)
    }

    /// [`Self::call`] on another published resource than the app's own: a
    /// child, an item, or a live row a surface shows (`"job:7"`), addressed
    /// by its id.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_on(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_at(None, resource, id, arguments, false)
    }

    /// [`Self::call_on`], waiting for the job the action started, as
    /// [`Self::effect`] does: what it finished with.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn call_on_finished(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        self.effect_invoked(None, resource, id, &arguments, false)
    }

    /// [`Self::call_on`], answering the whole output: the value and the
    /// resources the call produced, such as the items a verb created.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_on_output(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::DoorError> {
        self.call_output_at(None, resource, id, arguments, false)
    }

    /// What the host is asking the person to confirm right now — a
    /// destructive verb or a removal that a button, a menu, the palette or
    /// an immediate application submitted — and the question it asks.
    /// `None` while nothing is asked.
    pub fn asking(&mut self) -> Option<(tilo_actions::ActionAddress, String)> {
        self.settle();
        self.harness().asking()
    }

    /// Confirm what the host asks, through its real dialog, as the person
    /// does (Enter on its question), and settle: the
    /// action applies. Answers the action confirmed; `None`, changing
    /// nothing, when nothing was asked. A test of a delete from a menu
    /// clicks the menu item, then calls this.
    pub fn confirm_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        // What the last event submitted reaches the host first.
        self.settle();
        let confirmed = self.harness().confirm_asked();
        self.settle();
        confirmed
    }

    /// [`Self::call`] after the caller explicitly confirmed this invocation.
    /// Argument validation and current permissions are still enforced.
    ///
    /// # Errors
    /// As [`Self::call`], except that no confirmation is required.
    pub fn call_confirmed(
        &mut self,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_confirmed_on(<A::Resource as Resource>::KIND, id, arguments)
    }

    /// [`Self::call_on`] after the caller explicitly confirmed this invocation
    /// on an item or child resource. Use the address returned by
    /// [`Self::actions`]; no item id needs to be guessed or parsed from a label.
    /// Confirmation applies to this call only and grants no authority.
    ///
    /// # Errors
    /// As [`Self::call_on`], except that no confirmation is required.
    pub fn call_confirmed_on(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_at(None, resource, id, arguments, true)
    }

    fn call_within(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_at(
            ambient,
            <A::Resource as Resource>::KIND,
            id,
            arguments,
            false,
        )
    }

    fn call_at(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
        confirmed: bool,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_output_at(ambient, resource, id, arguments, confirmed)
            .map(|output| output.and_then(|output| output.value))
    }

    fn call_output_at(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
        confirmed: bool,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::DoorError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(resource, id),
        };
        let harness = self.harness();
        let started = if confirmed {
            harness.invoke_bound_action_confirmed(&address, &arguments)?
        } else {
            match ambient {
                Some(ambient) => {
                    harness.invoke_bound_action_within(&address, &arguments, ambient)?
                }
                None => harness.invoke_bound_action(&address, &arguments)?,
            }
        };
        harness.relayout();
        Ok(started.completed())
    }

    fn invoke_as(
        &mut self,
        invoke: Invoke<A::Resource>,
        confirmed: bool,
    ) -> Result<(), tilo_actions::DoorError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::DoorError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id),
        };
        let harness = self.harness();
        if confirmed {
            harness.invoke_bound_action_confirmed(&address, &arguments)?;
        } else {
            harness.invoke_bound_action(&address, &arguments)?;
        }
        harness.relayout();
        Ok(())
    }

    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces corruption loudly"
    )]
    pub fn state(&self) -> <A::Resource as Resource>::State {
        self.records
            .read(&self.checkout().id, |doc| {
                <A::Resource as Resource>::read(doc)
            })
            .expect("record is open")
            .expect("resource reads")
    }

    /// Read the stored document itself — the parts of a large resource its
    /// state does not hold until a view demands them.
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces a closed record loudly"
    )]
    pub fn read_document<T>(&self, read: impl FnOnce(&tilo_resource::Doc) -> T) -> T {
        self.records
            .read(&self.checkout().id, read)
            .expect("record is open")
    }

    /// A copy of the stored document written by another participant: a
    /// collaborator's replica, for a test to edit and [`Self::merge_peer`]
    /// back.
    pub fn peer_document(&self) -> tilo_resource::Doc {
        self.read_document(|doc| doc.clone().fork())
    }

    /// Exchange edits with a collaborator's replica, as sync does both ways:
    /// the peer's edits arrive in the running app (and its views), and the
    /// app's edits arrive in `peer`.
    pub fn merge_peer(&mut self, peer: &mut tilo_resource::Doc) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming;
        let mut ours = self.read_document(Clone::clone);
        peer.merge_incoming(&mut ours)
            .map_err(|error| error.to_string())?;
        self.records
            .merge_peer_document(self.checkout(), peer.save())?;
        self.harness().relayout();
        Ok(())
    }

    /// Another replica's change to `record`, a record open in the app's
    /// store (its own, or a part of a chain beside it), arriving as a workspace's
    /// replication delivers it: `edit` writes on that replica as it stands
    /// now, under its own actor, and the change is merged into the store.
    /// Nothing else runs: no frame is laid out or forced, so what the window
    /// shows of it is what its own frames ([`Self::frame`]) take up, as for
    /// a person who is only watching. Whether the record changed.
    pub fn replica_arrives(
        &mut self,
        record: &RecordId,
        edit: impl FnOnce(&mut tilo_resource::Doc),
    ) -> Result<bool, String> {
        let checkout = self
            .records
            .open_records()
            .into_iter()
            .find(|open| open.id == *record)
            .ok_or("the record is not open here")?;
        let mut replica = self
            .records
            .read(record, |doc| doc.clone().fork())
            .ok_or("the record is not open here")?;
        let before = replica.get_heads();
        edit(&mut replica);
        replica.commit();
        let change = replica.save_after(&before);
        self.records
            .merge_replica(
                &checkout,
                ui_core::collaboration::DocumentMutation::Incremental(change),
            )
            .map(|(changed, _)| changed)
    }

    /// The record's version history.
    pub fn versions(&self) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(&self.checkout().id, |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
    }

    /// What each version did through an action, oldest first.
    pub fn history(&self) -> Vec<tilo_actions::ActionContext> {
        self.versions()
            .into_iter()
            .filter_map(|version| version.context)
            .collect()
    }

    /// The sentences History shows for `entries` in en-US, through the app's
    /// own catalogue.
    pub fn sentences<'e>(
        &self,
        entries: impl IntoIterator<Item = &'e tilo_actions::ActionContext>,
    ) -> Vec<String> {
        let source = A::catalog().and_then(ui_core::i18n::EmbeddedCatalog::source);
        let catalogue = tilo_history::Catalogue::new("en-US", &Vec::from_iter(source));
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        entries
            .into_iter()
            .map(|entry| reader.sentence(entry))
            .collect()
    }

    /// The version at `heads` as History mode previews it: the app's own
    /// view, read-only, over the document as it was then.
    pub fn past_version(
        &self,
        heads: &[ui_core::automerge::ChangeHash],
    ) -> Result<super::preview::PastVersion<A::Resource>, String> {
        let past = self
            .records
            .read(&self.checkout().id, |doc| doc.clone().fork_at(heads))
            .ok_or("the record is not open")?
            .map_err(|error| error.to_string())?;
        super::preview::render_past::<A>(past, A::shot_viewport(None))
    }

    /// A collaborator's edit arriving: `edit` writes on their own replica of
    /// the record as it stands now, under their own actor, and the change is
    /// merged in as sync merges a peer's. It is not the local participant's
    /// action, so it never lands on their undo stack.
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces a failed merge loudly"
    )]
    pub fn merge_peer_edit(&mut self, edit: impl FnOnce(&mut tilo_resource::Doc)) {
        use tilo_automerge::ApplyIncoming as _;
        let mut peer = self
            .records
            .with(&self.checkout().id, tilo_resource::Doc::fork)
            .expect("record is open");
        edit(&mut peer);
        peer.commit();
        self.records
            .with(&self.checkout().id, |doc| doc.merge_incoming(&mut peer))
            .expect("record is open")
            .expect("a peer's change merges");
        self.harness().relayout();
    }

    /// Run `visit` on the app's custom surface of type `S` — its view state,
    /// which no action reaches — then the frames that follow. `None` when the
    /// app's view is not an `S`.
    pub fn surface<S: 'static, O>(&mut self, visit: impl FnOnce(&mut S) -> O) -> Option<O> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                root.content
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<S>())
                    .map(visit)
            })
            .flatten()
    }

    /// Undo the local participant's last action, as Ctrl+Z does.
    pub fn undo(&mut self) -> bool {
        let harness = self.harness();
        let undone = harness.perform_undo();
        harness.relayout();
        undone
    }

    /// Redo, as Ctrl+Shift+Z does.
    pub fn redo(&mut self) -> bool {
        let harness = self.harness();
        let redone = harness.perform_redo();
        harness.relayout();
        redone
    }
}

/// The custom surface of type `S` that `A`'s view hosts in `harness`,
/// without running a frame (see [`super::testing::with_surface`]).
pub fn surface_in<A: ResourceApp, S: 'static>(harness: &mut FrameHarness) -> Option<&mut S> {
    surface_of::<A, S>(harness.widget_mut::<ResourceRoot>()?)
}

pub(super) fn surface_of<A: ResourceApp, S: 'static>(root: &mut ResourceRoot) -> Option<&mut S> {
    fn find<S: 'static>(widget: &mut dyn ui_core::widget::Widget) -> Option<&mut S> {
        if widget.as_any().is_some_and(<dyn std::any::Any>::is::<S>) {
            return widget.as_any_mut()?.downcast_mut::<S>();
        }
        widget.children_mut().into_iter().find_map(find::<S>)
    }
    root.typed::<A::Resource>()?;
    find::<S>(root.content.as_mut())
}

impl<A: ResourceApp> Drop for HeadlessApp<A> {
    fn drop(&mut self) {
        // A test that failed is unwinding: close the app without running
        // its frames again, so whatever failed it cannot fail closing too
        // and abort the whole test binary.
        if std::thread::panicking() {
            if !self.closed {
                self.closed = true;
                self.harness.close_components();
            }
            return;
        }
        let _ = self.close();
    }
}

/// The first widget of type `T` in `widget`, whatever wraps it: a custom
/// surface is found by its own type.
fn find_mut<T: 'static>(widget: &mut dyn ui_core::widget::Widget) -> Option<&mut T> {
    if widget.as_any_mut().is_some_and(|any| any.is::<T>()) {
        return widget.as_any_mut()?.downcast_mut::<T>();
    }
    widget.children_mut().into_iter().find_map(find_mut::<T>)
}

/// Match the native runner's disposable private profile and built-in flag.
/// What opening a headless app needs of its type: everything else about
/// opening it is the same for every app ([`open_app`]).
struct Opening {
    definition: &'static ui_core::application::ApplicationDefinition,
    id: &'static str,
    options: &'static [&'static str],
    flags: &'static [&'static str],
    private_windows: bool,
    launch_record: fn(&LaunchInput<'_>) -> Option<RecordId>,
    shot_viewport: fn(Option<&str>) -> (u32, u32),
}

impl Opening {
    fn of<A: ResourceApp>() -> Self {
        Self {
            definition: definition::<A>(),
            id: super::app::hooks::<A>().id(),
            options: A::OPTIONS,
            flags: A::FLAGS,
            private_windows: A::PRIVATE_WINDOWS,
            launch_record: A::launch_record,
            shot_viewport: A::shot_viewport,
        }
    }
}

/// How one headless app is opened: on which profile, as whom, with what.
struct Launch<'a> {
    profile: Profile,
    role: Role,
    author: &'a str,
    arguments: &'a [String],
    file: Option<OpenFile>,
    handoff: Option<PlaybackHandoff>,
    capture: Option<(Capture, (u32, u32))>,
}

/// A headless app [`open_app`] opened.
struct Opened {
    harness: FrameHarness,
    records: RecordStore,
    checkout: RecordCheckout,
    instance: u64,
    profile: Profile,
    permissions: SharedPermissions,
}

/// The application host `profile` opens in, and the instance its window
/// takes.
fn profile_host(
    profile: &Profile,
    author: UserId,
    permissions: &SharedPermissions,
) -> Result<(ApplicationHost, u64), LaunchError> {
    Ok(match profile {
        Profile::Temporary(directory, workspace, device)
        | Profile::Window(directory, workspace, device, _) => (
            ApplicationHost::new_with_profile_lifetime(
                directory.path().join("state"),
                device.clone(),
                workspace,
                author,
                permissions.clone(),
                tilo_actions::ActionContext::default(),
                directory.clone(),
            )?,
            match profile {
                Profile::Window(_, _, _, window) => *window,
                _ => INSTANCE,
            },
        ),
        Profile::DataDir(directory) => (
            data_dir_host(directory, &author, permissions.clone())?,
            DATA_DIR_INSTANCE,
        ),
    })
}

/// Open the app `opening` describes as `launch` says, `configure` given its
/// host context before it is instantiated.
fn open_app(
    opening: &Opening,
    launch: Launch<'_>,
    configure: &mut dyn FnMut(&mut HostContext),
) -> Result<Opened, LaunchError> {
    strict_bindings();
    let (profile, private, private_arguments) =
        private_launch(opening.private_windows, launch.profile, launch.arguments)?;
    let (arguments, options) =
        split_command_line(&private_arguments, opening.options, opening.flags);
    let arguments = arguments.as_slice();
    let person = launch.author.to_owned();
    let author = UserId(person.clone());
    let permissions = role_permissions(&person, launch.role);
    let host_phase = tilo_resource::profiling::Phase::start("start.host");
    let (mut host, instance) = profile_host(&profile, author, &permissions)?;
    host_phase.end();
    let definition = opening.definition;
    let prepare_phase = tilo_resource::profiling::Phase::start("start.prepare");
    let records = host.records_for(definition)?;
    let named = (opening.launch_record)(&LaunchInput::new(arguments, None).with_options(&options));
    let inputs = ComponentRunnerInputs::from_native_process();
    let mut prepared = match (&profile, named) {
        (Profile::Window(..), None) => host.prepare_default(
            definition,
            instance,
            RecordCheckout::new(
                RecordId::window_default(instance),
                definition.record_kind.clone(),
            ),
            None,
            inputs,
        )?,
        (_, named) => prepare_on(&mut host, definition, instance, named, inputs)?,
    };
    let checkout = prepared.context.record().clone();
    // The harness holds the workspace's keys, as the record's route does on a
    // device: what the app stores is sealed under its members' secret.
    if let (Some(workspace), Ok(route)) = (
        profile.workspace(),
        prepared.context.records().wire_route(&checkout.id),
    ) {
        ui_core::collaboration::record_content::ContentLane::of(&route)
            .hold_secret(workspace_secret(workspace));
    }
    hand_launch(definition, arguments, &mut prepared)?;
    prepared.context.provide_launch_options(options);
    if private {
        prepared.context.provide_private();
    }
    if let Some(file) = launch.file {
        prepared.context.provide_open_file(file);
    }
    if let Some(handoff) = launch.handoff {
        prepared.context.provide_handoff(handoff);
    }
    let captured = launch.capture.is_some();
    let viewport = match launch.capture {
        Some((capture, viewport)) => {
            prepared.context.provide_capture(capture);
            viewport
        }
        None => (opening.shot_viewport)(None),
    };
    prepared.context.provide_content(profile.content());
    let mut prepared = in_profile(prepared, &profile.root(), opening.id);
    configure(&mut prepared.context);
    prepare_phase.end();
    let component = {
        let _phase = tilo_resource::profiling::Phase::start("start.instantiate");
        definition.instantiate(prepared.context, prepared.target, prepared.restore)?
    };
    let mut harness = {
        let _phase = tilo_resource::profiling::Phase::start("start.first_frame");
        FrameHarness::from_component(component, viewport, prepared.runner_inputs)?
    };
    {
        let _phase = tilo_resource::profiling::Phase::start("start.settle");
        settle_launch(&mut harness);
    }
    // The live host withdraws a component's whole action surface (the
    // palette's, agents', scripts') when any of its actions breaks its
    // contract. Every test that opens the app proves it would not.
    // A capture stages fixtures, not the app someone uses.
    let violations = if captured {
        Vec::new()
    } else {
        let _phase = tilo_resource::profiling::Phase::start("start.contract_check");
        harness.component_contract_violations()
    };
    if !violations.is_empty() {
        return Err(LaunchError::Operation(format!(
            "{} publishes an action surface the host would refuse: {}",
            definition.app_id(),
            violations
                .iter()
                .map(|violation| format!("{}: {}", violation.address, violation.reason))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(Opened {
        harness,
        records,
        checkout,
        instance,
        profile,
        permissions,
    })
}

fn private_launch(
    private_windows: bool,
    profile: Profile,
    arguments: &[String],
) -> Result<(Profile, bool, Vec<String>), LaunchError> {
    let private = private_windows && arguments.iter().any(|word| word == "--private");
    let arguments = arguments
        .iter()
        .filter(|word| !private || word.as_str() != "--private")
        .cloned()
        .collect();
    let profile = if private {
        let device = match &profile {
            Profile::Temporary(_, _, device) | Profile::Window(_, _, device, _) => device.clone(),
            Profile::DataDir(_) => TEST_DEVICE.to_owned(),
        };
        Profile::Temporary(
            Rc::new(profile_directory().map_err(operation)?),
            "private".into(),
            device,
        )
    } else {
        profile
    };
    Ok((profile, private, arguments))
}

/// Split a command line into its arguments and the app's own `options`
/// (each taking the next word, or written `--name=value`) and `flags`, as
/// the launcher reads it: a word naming neither is an argument.
fn split_command_line(
    words: &[String],
    options: &[&str],
    flags: &[&str],
) -> (Vec<String>, Vec<ui_core::application::LaunchOption>) {
    let mut arguments = Vec::new();
    let mut named = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if flags.contains(&word.as_str()) {
            named.push(ui_core::application::LaunchOption {
                name: word.clone(),
                value: None,
            });
        } else if let Some((name, value)) = word.split_once('=')
            && options.contains(&name)
        {
            named.push(ui_core::application::LaunchOption {
                name: name.to_owned(),
                value: Some(value.to_owned()),
            });
        } else if options.contains(&word.as_str()) {
            named.push(ui_core::application::LaunchOption {
                name: word.clone(),
                value: Some(words.next().cloned().unwrap_or_default()),
            });
        } else {
            arguments.push(word.clone());
        }
    }
    (arguments, named)
}

/// What `A` publishes and how its view binds to it, as `tilo-app surface`
/// commits it and `tilo-app check` compares it: the resource's
/// [`Surface`](tilo_resource::Surface), those of the person's records it
/// declares ([`ResourceApp::PERSON_RECORDS`]) and those of the records its
/// views open beside its own (the parts of its chains, the settings
/// sections), every control its view, built headless on an empty record,
/// binds to something the resource does not publish, the ids of its effects
/// and named views, and the ids the framework keeps for its own actions
/// ([`reserved_action_ids`](super::reserved_action_ids)).
#[must_use]
pub fn surface_report<A: ResourceApp>() -> serde_json::Value {
    report_surface(
        &Opening::of::<A>(),
        <A::Resource as Resource>::surface(),
        A::PERSON_RECORDS,
        &published_ids::<A>,
    )
}

/// What a built view publishes beside its resource's verbs: its effects and
/// the resource's named views, and what the effects declare, which its surface
/// publishes as actions of the resource.
#[derive(Default)]
struct PublishedActions {
    /// The ids of every effect, views included.
    effects: Vec<String>,
    /// The ids of the resource's named views.
    views: Vec<String>,
    /// Every read effect's declaration: what its surface publishes it as.
    rows: Vec<tilo_resource::effect::EffectRow>,
}

/// [`PublishedActions`] of `A`'s root `harness` hosts.
fn published_ids<A: ResourceApp>(harness: &mut FrameHarness) -> PublishedActions {
    harness
        .with_widget_mut(|root: &mut ResourceRoot| {
            let typed = root.typed::<A::Resource>()?;
            let views = <A::Resource as Resource>::views(&typed.live.get());
            Some(PublishedActions {
                effects: typed
                    .effects
                    .specs()
                    .iter()
                    .map(|spec| spec.id.clone())
                    .collect(),
                views: views.into_iter().map(|view| view.id).collect(),
                rows: typed.effects.rows(),
            })
        })
        .flatten()
        .unwrap_or_default()
}

/// [`surface_report`] of the app `opening` describes, whose resource's
/// surface is `surface` and whose person records are `person`; `published`
/// reads the ids of the effects and the named views a built view publishes.
fn report_surface(
    opening: &Opening,
    mut surface: tilo_resource::Surface,
    person: &[super::PersonKind],
    published: &dyn Fn(&mut FrameHarness) -> PublishedActions,
) -> serde_json::Value {
    let collecting = collect_unbound();
    let profile = profile_directory().map(Rc::new).map_err(operation);
    let built = profile.and_then(|profile| {
        open_app(
            opening,
            Launch {
                profile: Profile::Temporary(
                    profile,
                    TEST_WORKSPACE.to_owned(),
                    TEST_DEVICE.to_owned(),
                ),
                role: Role::Owner,
                author: AUTHOR,
                arguments: &[],
                file: None,
                handoff: None,
                capture: None,
            },
            &mut |_| {},
        )
    });
    // What the view publishes besides the resource's verbs: its effects and
    // the resource's named views, whose ids must not be the framework's. The
    // app's read effects are actions of its own resource and are published
    // with it, as a verb of the surface.
    let mut declared = PublishedActions::default();
    let mut view = None;
    match built {
        Ok(mut opened) => declared = published(&mut opened.harness),
        Err(error) => view = Some(error.to_string()),
    }
    add_effect_verbs(&mut surface, &declared.rows);
    let PublishedActions { effects, views, .. } = declared;
    let (unbound, opened) = collecting.finish();
    let mut resources = vec![surface];
    resources.extend(opened);
    // The person's records the app declares are part of its surface whether
    // or not a view of it was showing: the host admits the app's changes to
    // them, and its reads of them, against this.
    for declared in person {
        match resources
            .iter_mut()
            .find(|resource| resource.kind == declared.kind())
        {
            Some(resource) => resource.person = true,
            None => resources.push(declared.surface()),
        }
    }
    serde_json::json!({
        "resources": resources,
        "unbound": unbound,
        "effects": effects,
        "views": views,
        "reserved": super::reserved_action_ids(),
        "view": view,
    })
}

/// Publish the app's read effects on `surface`: its resource's verbs take
/// them, each with its parameters and the value it answers with, and the
/// records and enums that value names join the surface's types.
fn add_effect_verbs(
    surface: &mut tilo_resource::Surface,
    rows: &[tilo_resource::effect::EffectRow],
) {
    use tilo_resource::surface::{FieldType, NamedType};

    let mut types = tilo_resource::surface::TypeSet::new();
    let mut verbs = tilo_resource::surface::effect_verbs(rows, &mut types);
    let declared: Vec<NamedType> = types.into_types();
    // A record the resource's fields already declare, named again by a value
    // an action answers with, keeps the one name: the surface declares it
    // once.
    let mut renames: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for named in &declared {
        let same = surface.types.iter().find(|known| {
            known.name != named.name && known.fields == named.fields && known.cases == named.cases
        });
        if let Some(known) = same {
            renames.insert(named.name.clone(), known.name.clone());
        }
    }
    let renamed_type = |ty: &FieldType| ty.renamed(&renames);
    for verb in &mut verbs {
        for param in &mut verb.params {
            param.ty = renamed_type(&param.ty);
        }
        if let Some(returns) = &mut verb.returns {
            returns.ty = renamed_type(&returns.ty);
        }
    }
    let kept: Vec<NamedType> = declared
        .into_iter()
        .filter(|named| !renames.contains_key(&named.name))
        .map(|mut named| {
            for field in &mut named.fields {
                field.ty = renamed_type(&field.ty);
            }
            for case in &mut named.cases {
                for field in &mut case.fields {
                    field.ty = renamed_type(&field.ty);
                }
            }
            named
        })
        .filter(|named| !surface.types.iter().any(|known| known.name == named.name))
        .collect();
    surface.types.extend(kept);
    surface.verbs.extend(verbs);
}

/// Collects the controls bound to something the resource does not publish
/// until [`Self::finish`], instead of failing on the first.
struct Collecting(());

/// Start collecting unbound controls on this thread.
fn collect_unbound() -> Collecting {
    super::controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = Some(Vec::new()));
    super::controls::OPENED.with(|opened| *opened.borrow_mut() = Some(Vec::new()));
    Collecting(())
}

impl Collecting {
    /// What was collected: one sentence per unbound control, and the
    /// surfaces of the other records the views opened.
    fn finish(self) -> (Vec<String>, Vec<tilo_resource::Surface>) {
        let unbound = super::controls::UNBOUND
            .with(|unbound| unbound.borrow_mut().take().unwrap_or_default());
        let opened =
            super::controls::OPENED.with(|opened| opened.borrow_mut().take().unwrap_or_default());
        drop(self);
        (unbound, opened)
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        super::controls::UNBOUND.with(|unbound| *unbound.borrow_mut() = None);
        super::controls::OPENED.with(|opened| *opened.borrow_mut() = None);
    }
}

/// Make a control bound to an action the resource does not publish a panic
/// on this thread, as [`HeadlessApp`] and
/// `tilo-app check` do: a typo in a binding must fail, not grey a button.
fn strict_bindings() {
    super::controls::STRICT.with(|strict| strict.set(true));
}

/// `resource` without the actions another provider runs and publishes
/// ([`tilo_actions::ActionSpec::run_by`]).
fn published_here(resource: &mut tilo_actions::ResourceSpec) {
    resource.actions.retain(|spec| spec.provider().is_none());
    resource.children.iter_mut().for_each(published_here);
}

/// A host that runs documents of its own, mounted headlessly: for proving what the host adds to an
/// app that is not a [`ResourceApp`] (tilolang runs other apps' packages in one window). The same
/// opening as [`HeadlessApp`]: the real component runtime, storage, permissions and history, and the
/// host's refusal of an action surface that breaks the contract.
pub struct HostedApp {
    harness: Option<FrameHarness>,
    records: RecordStore,
    checkout: RecordCheckout,
    instance: u64,
    _profile: Profile,
}

impl HostedApp {
    /// The app `definition` as its owner `author` opens it on a fresh private profile.
    pub fn open(
        definition: &'static ui_core::application::ApplicationDefinition,
        author: &str,
    ) -> Result<Self, LaunchError> {
        let profile = Profile::Temporary(
            Rc::new(profile_directory().map_err(operation)?),
            TEST_WORKSPACE.to_owned(),
            TEST_DEVICE.to_owned(),
        );
        Self::open_in(definition, author, profile)
    }

    /// The app `definition` as its owner `author` opens it on the profile `--data-dir
    /// directory` opens: what a test opens again to see what an earlier window kept. Only one
    /// writer may hold a profile.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_data_dir(
        definition: &'static ui_core::application::ApplicationDefinition,
        author: &str,
        directory: &std::path::Path,
    ) -> Result<Self, LaunchError> {
        Self::open_in(
            definition,
            author,
            Profile::DataDir(directory.to_path_buf()),
        )
    }

    fn open_in(
        definition: &'static ui_core::application::ApplicationDefinition,
        author: &str,
        profile: Profile,
    ) -> Result<Self, LaunchError> {
        let opening = Opening {
            definition,
            id: definition.app_id(),
            options: &[],
            flags: &[],
            private_windows: false,
            launch_record: |_| None,
            shot_viewport: |_| (1000, 700),
        };
        let opened = open_app(
            &opening,
            Launch {
                profile,
                role: Role::Owner,
                author,
                arguments: &[],
                file: None,
                handoff: None,
                capture: None,
            },
            &mut |_| {},
        )?;
        Ok(Self {
            harness: Some(opened.harness),
            records: opened.records,
            checkout: opened.checkout,
            instance: opened.instance,
            _profile: opened.profile,
        })
    }

    /// The headless window.
    ///
    /// # Panics
    /// Panics once the app is closed.
    #[allow(clippy::expect_used, reason = "closing consumes the host")]
    pub fn harness(&mut self) -> &mut FrameHarness {
        self.harness.as_mut().expect("app is open")
    }

    /// Invoke `action` of the published `resource` through the host's door, as `invoker` (the
    /// person at the window when `None`), within `ambient`: the task the invoker is carrying
    /// out, recorded with every change it makes. Answers what the operation answered.
    pub fn invoke(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        ambient: Option<&tilo_actions::ActionContext>,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(resource, action),
        };
        let (invocation, started) = self
            .harness()
            .start_bound_job_within(invoker, ambient, &address, arguments, confirmed)?;
        let harness = self.harness();
        if let Some(output) = started.completed() {
            harness.relayout();
            return Ok(output);
        }
        let finished = |harness: &FrameHarness| {
            harness
                .job_updates(invocation)
                .iter()
                .find(|update| update.is_terminal())
                .cloned()
        };
        harness.wait_until(ui_core::time::Duration::from_secs(30), |harness| {
            finished(harness).is_some()
        });
        match finished(harness) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(tilo_actions::DoorError::Failed(error.message))
            }
            Some(_) => Err(tilo_actions::DoorError::Failed("cancelled".into())),
            None => Err(tilo_actions::DoorError::Failed(format!(
                "`{action}` did not finish"
            ))),
        }
    }

    /// Read the app's record as it is now, after everything the window has committed.
    ///
    /// # Panics
    /// Panics when the record cannot be read.
    #[allow(
        clippy::expect_used,
        reason = "a record that cannot be read fails the test"
    )]
    pub fn read<R>(&mut self, read: impl FnOnce(&mut tilo_resource::Doc) -> R) -> R {
        let harness = self.harness();
        harness.relayout();
        harness
            .flush_component_records()
            .expect("the window's changes reach the record");
        self.records
            .read(&self.checkout.id, |doc| read(&mut doc.clone()))
            .expect("the record reads")
    }

    fn close(&mut self) -> Result<(), LaunchError> {
        if let Some(mut harness) = self.harness.take() {
            harness.flush_component_records().map_err(operation)?;
            harness.close_components();
        }
        Ok(())
    }
}

impl Drop for HostedApp {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

// Retained results come from the real framework semantic door.
#[cfg(test)]
mod results_tests;
