//! Testing a framework app the way it runs: the real component runtime,
//! storage, permissions, undo and history, without a window.
//!
//! Prefer [`tilo_resource::Replica`] for the resource's own behaviour (it
//! needs no UI). Use [`TestApp`] to prove what the host adds: that controls
//! invoke actions, that undo reverts them, that a viewer is refused, that
//! state survives a reopen.

use std::cell::RefCell;
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
use ui_core::permissions::{PermissionContext, Role, UserId};

const AUTHOR: &str = "test-author";
use ui_core::record::{RecordCheckout, RecordId, RecordStore};

use super::app::{
    DATA_DIR_INSTANCE, ResourceApp, data_dir_host, definition, hand_launch, prepare_on,
    role_permissions, settle_launch,
};
use super::launch::LaunchInput;
use super::root::ResourceRoot;

const INSTANCE: u64 = 7;
/// The room a test opens the app in unless it names one.
const TEST_ROOM: &str = "test-workspace";
/// The device a test app runs on unless it names another.
const TEST_DEVICE: &str = "test-device";

enum Profile {
    /// A private temporary profile, removed with the app, the room the app
    /// opens in and the device it runs on.
    Temporary(Rc<tempfile::TempDir>, String, String),
    /// An explicit `--data-dir` profile the app itself would open.
    DataDir(PathBuf),
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
        .provide_settings(ui_core::storage::SettingsDocument::File(
            root.join("settings.json"),
        ));
    prepared
        .context
        .provide_files(ui_core::storage::AppFiles::at(
            ui_core::storage::AppPaths::under(root.join("files"), app_id),
        ));
    prepared
}

/// The root of a `--data-dir` profile.
pub(crate) fn data_dir_root(directory: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf())
}

/// The content store of a `--data-dir` profile.
pub(crate) fn data_dir_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    super::app::profile_content(
        &std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf()),
    )
}

impl Profile {
    /// The profile's settings document, which the app's typed settings use
    /// instead of the person's.
    fn settings(&self) -> PathBuf {
        self.root().join("settings.json")
    }

    /// The directory the profile keeps everything in.
    fn root(&self) -> PathBuf {
        match self {
            Self::Temporary(directory, ..) => directory.path().to_path_buf(),
            Self::DataDir(directory) => data_dir_root(directory),
        }
    }

    /// The profile's content store, which the app keeps handed files and
    /// other bytes its record names in, instead of the person's.
    fn content(&self) -> tilo_content::ContentStore {
        match self {
            Self::Temporary(directory, ..) => super::app::profile_content(directory.path()),
            Self::DataDir(directory) => data_dir_content(directory),
        }
    }
}

/// One framework app mounted headlessly: a test, a fixture seeder or an
/// inspection tool drives it through the real runtime.
pub struct HeadlessApp<A: ResourceApp> {
    harness: Option<FrameHarness>,
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
    /// Shared with the host: changing it is a role change the running app
    /// sees, as when the record's owner changes someone's access.
    permissions: Rc<RefCell<PermissionContext>>,
    marker: std::marker::PhantomData<A>,
}

/// The name tests use for [`HeadlessApp`].
pub type TestApp<A> = HeadlessApp<A>;

fn operation(error: impl std::fmt::Display) -> LaunchError {
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
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn new() -> Result<Self, LaunchError> {
        Self::with_role(Role::Owner)
    }

    /// The app as its owner opens it on a fresh private profile, in `room`:
    /// one of several windows of the app in one process, each in a room of
    /// its own.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn in_room(room: &str) -> Result<Self, LaunchError> {
        Self::fresh(room, Role::Owner, AUTHOR)
    }

    /// The app as its owner opens it on a fresh profile on the device
    /// `device`: one of two devices sharing a record, each carrying out what
    /// is queued for it.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn on_device(device: &str) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), device.to_owned()),
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
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn with_role(role: Role) -> Result<Self, LaunchError> {
        Self::fresh(TEST_ROOM, role, AUTHOR)
    }

    /// The app as `person` with `role` opens it on a fresh private profile,
    /// in `room`.
    fn fresh(room: &str, role: Role, person: &str) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, room.to_owned(), TEST_DEVICE.to_owned()),
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
    /// fresh profile: one of that person's devices, whose person records
    /// ([`RecordEnv::person`](super::RecordEnv::person)) are theirs.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn for_person(person: &str) -> Result<Self, LaunchError> {
        Self::fresh(TEST_ROOM, Role::Owner, person)
    }

    /// Open a fresh person profile with a service lent by its mounting host.
    /// The loan follows production `HostContext::lend` and is not retained on
    /// reopen: the next host must authorize and lend its own service again.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn for_person_with_lent<S: 'static>(person: &str, service: S) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            person,
            &[],
            None,
            None,
            None,
            move |context| context.lend(service),
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
    ///
    /// # Errors
    /// Returns any launch failure, or the host's refusal of a scenario
    /// action.
    pub fn capture(scenario: Option<&str>) -> Result<Self, LaunchError> {
        Self::capture_at(scenario, A::shot_viewport(scenario))
    }

    /// [`Self::capture`] at `viewport`, as `--shots --viewport` draws it.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn capture_at(scenario: Option<&str>, viewport: (u32, u32)) -> Result<Self, LaunchError> {
        Self::capture_on(scenario, viewport, &[])
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
        Self::capture_on(scenario, A::shot_viewport(scenario), &arguments)
    }

    fn capture_on(
        scenario: Option<&str>,
        viewport: (u32, u32),
        arguments: &[String],
    ) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        let mut app = Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            arguments,
            None,
            None,
            Some((Capture::new(scenario.map(str::to_owned)), viewport)),
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
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn launched(arguments: &[&str]) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
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
    /// record in the same room. The production installer validates its
    /// identity before the app can initialize or open it.
    ///
    /// # Errors
    /// Returns invalid record scope, storage or launch failures.
    pub fn launched_replica(
        arguments: &[&str],
        document: &mut tilo_resource::Doc,
    ) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        let native =
            ui_core::application::NativeProfile::open(&profile.path().join("state"), None)?;
        ui_core::record::transfer::install(native.store(), A::ID, TEST_ROOM, document)
            .map_err(operation)?;
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &arguments,
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app launched with `arguments` to continue `handoff`'s playback,
    /// as the session launches the target of a media handoff.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn handed_off(arguments: &[&str], handoff: PlaybackHandoff) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
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
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        Self::open(
            Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            Some(file),
            None,
            None,
            |_| {},
        )
    }

    /// The app on the profile `--data-dir directory` opens, as its owner.
    /// Only one writer may hold a profile: this fails while the app (or
    /// another `HeadlessApp`) has it open.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
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
        Ok(Self {
            harness: Some(opened.harness),
            records: opened.records,
            checkout: opened.checkout,
            instance: opened.instance,
            profile: opened.profile,
            role,
            author: author.to_owned(),
            last_effect: None,
            permissions: opened.permissions,
            marker: std::marker::PhantomData,
        })
    }

    /// The app's file areas in this profile, which the running app writes
    /// instead of the person's.
    pub fn files(&self) -> ui_core::storage::AppFiles {
        ui_core::storage::AppFiles::at(ui_core::storage::AppPaths::under(
            self.profile.root().join("files"),
            A::ID,
        ))
    }

    /// The app's typed settings in this profile's settings document, which
    /// the running app follows: a write here is what another window, the
    /// Settings app or another of the person's devices writing them does.
    pub fn settings<T: ui_core::storage::SettingsSchema>(&self) -> ui_core::storage::Settings<T> {
        ui_core::storage::Settings::for_app(A::ID).in_file(self.profile.settings())
    }

    /// Change the participant's role while the app runs, as the record's
    /// owner changing their access does. Takes effect on the next frame,
    /// which this runs.
    pub fn set_role(&mut self, role: Role) {
        *self.permissions.borrow_mut() = PermissionContext::member(self.author.as_str(), role);
        self.role = role;
        self.harness().relayout();
    }

    /// The participant's permissions in force now.
    #[must_use]
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }

    /// Change the participant's permissions while the app runs, as the host
    /// does when the room's grants change. Takes effect on the next frame,
    /// which this runs.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        *self.permissions.borrow_mut() = permissions;
        self.harness().relayout();
    }

    /// Take the participant out of the room while the app runs: no role and
    /// no grant left, as when the owner removes them.
    pub fn leave_room(&mut self) {
        *self.permissions.borrow_mut() = PermissionContext::stranger(self.author.as_str());
        self.harness().relayout();
    }

    /// Close the app (flushing its record) and open it again on the same
    /// profile, as a relaunch would.
    ///
    /// # Errors
    /// Returns the flush or relaunch failure.
    pub fn reopen(self) -> Result<Self, LaunchError> {
        self.relaunch(&[])
    }

    /// Close the app and launch it again on the same profile with
    /// `arguments`, as `app ARGUMENTS` run again would.
    ///
    /// # Errors
    /// Returns the flush or relaunch failure.
    pub fn relaunch(mut self, arguments: &[&str]) -> Result<Self, LaunchError> {
        self.close()?;
        let profile = match &self.profile {
            Profile::Temporary(directory, room, device) => {
                Profile::Temporary(Rc::clone(directory), room.clone(), device.clone())
            }
            Profile::DataDir(directory) => Profile::DataDir(directory.clone()),
        };
        let role = self.role;
        let author = std::mem::take(&mut self.author);
        // Release this instance's record writer before the next one opens it.
        drop(self);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(profile, role, &author, &arguments, None, None, None, |_| {})
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
            .ok_or_else(|| format!("{} is not hosted here", A::TITLE))??;
        self.harness().relayout();
        outcome
            .get()
            .unwrap_or_else(|| Err("the host did not take the request".into()))
    }

    /// The record the app shows.
    pub fn record(&self) -> &RecordId {
        &self.checkout().id
    }

    /// The record the window shows now: the one it was opened on, or one
    /// its view opened in its place ([`RecordEnv::open_here`](super::RecordEnv::open_here)).
    fn checkout(&self) -> &RecordCheckout {
        self.harness
            .as_ref()
            .and_then(FrameHarness::component_record)
            .unwrap_or(&self.checkout)
    }

    /// Consume the hosted window's explicit save-retry request, as its
    /// record owner does before reporting durability again.
    ///
    /// # Errors
    /// Returns an error if the framework root is absent.
    pub fn take_save_retry(&mut self) -> Result<bool, String> {
        self.harness()
            .with_widget_mut(|root: &mut super::root::ResourceRoot| {
                super::root::RootApp::take_save_retry(root)
            })
            .ok_or_else(|| "the framework root is not mounted".to_owned())
    }

    /// Make every edit so far durable, as closing the app does.
    ///
    /// # Errors
    /// Returns the record writer's storage failure.
    pub fn flush(&mut self) -> Result<(), LaunchError> {
        self.harness().flush_component_records().map_err(operation)
    }

    fn close(&mut self) -> Result<(), LaunchError> {
        if let Some(mut harness) = self.harness.take() {
            harness.flush_component_records().map_err(operation)?;
            harness.close_components();
        }
        Ok(())
    }

    /// The custom surface of type `S` the view hosts, without running a
    /// frame: for a test reading it, or setting it up, between the events it
    /// sends.
    ///
    /// # Panics
    /// Panics when the view hosts no surface of type `S`.
    #[allow(
        clippy::expect_used,
        reason = "a test naming a surface its app does not host is wrong"
    )]
    pub fn surface_mut<S: 'static>(&mut self) -> &mut S {
        surface_in::<A, S>(self.harness()).expect("the view hosts this surface")
    }

    /// The headless frame harness, for input-level tests (`click_named`,
    /// `type_text`, `targets`).
    ///
    /// # Panics
    /// Panics after the app was closed, which the API does not allow.
    #[allow(clippy::expect_used, reason = "closing consumes the TestApp")]
    pub fn harness(&mut self) -> &mut FrameHarness {
        self.harness.as_mut().expect("app is open")
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
    ///
    /// # Errors
    /// Returns the host's refusal: unknown action, bad arguments, denied
    /// permission or the action's own refusal.
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
    ///
    /// # Errors
    /// Returns a refusal if the hosted component cannot be observed.
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
        let resources = self.harness().resources();
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
    ///
    /// # Errors
    /// Returns the host's refusal or the effect's failure.
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
        let (invocation, started) = self
            .harness()
            .start_bound_job_for(invoker, &address, arguments, confirmed)?;
        self.last_effect = Some(invocation);
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

    /// Run the frame the app runs after an event: edits the views and
    /// effects submitted are applied, and what they change is drawn.
    pub fn settle(&mut self) {
        self.harness()
            .wait_until(ui_core::time::Duration::ZERO, |_| true);
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

    /// What the host is asking the person to confirm right now — a
    /// destructive verb or a removal that a button, a menu, the palette or
    /// an immediate application submitted — and the text that confirms it.
    /// `None` while nothing is asked.
    pub fn asking(&mut self) -> Option<(tilo_actions::ActionAddress, String)> {
        self.settle();
        self.harness().asking()
    }

    /// Confirm what the host asks, through its real dialog, as the person
    /// does (type the confirming text, press Enter), and settle: the
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
        Ok(started.completed().and_then(|output| output.value))
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

    /// The resource state as stored.
    ///
    /// # Panics
    /// Panics when the record is unreadable, which a test should surface.
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
    ///
    /// # Panics
    /// Panics when the record is not open, which the API does not allow.
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
    ///
    /// # Errors
    /// Returns why either side refused the other's document.
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

    /// The record's version history.
    pub fn versions(&self) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(&self.checkout().id, |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
    }

    /// The version at `heads` as History mode previews it: the app's own
    /// view, read-only, over the document as it was then.
    ///
    /// # Errors
    /// Returns why that version cannot be read or drawn.
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
    ///
    /// # Panics
    /// Panics when the record is not open or the change does not merge,
    /// which a test should surface.
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

/// Run `visit` on the custom surface of type `S` that `A`'s view hosts in
/// `harness` (what [`ViewCx::surface`](super::ViewCx::surface) mounted), then
/// the frames that follow; `None` when the view hosts no such surface at its
/// root. For a test that drives a component mounted another way than
/// [`HeadlessApp`] (two windows over one record).
pub fn with_surface<A: ResourceApp, S: 'static, T>(
    harness: &mut FrameHarness,
    visit: impl FnOnce(&mut S) -> T,
) -> Option<T> {
    harness
        .with_widget_mut(|root: &mut ResourceRoot| surface_of::<A, S>(root).map(visit))
        .flatten()
}

/// The custom surface of type `S` that `A`'s view hosts in `harness`,
/// without running a frame (see [`with_surface`]).
pub fn surface_in<A: ResourceApp, S: 'static>(harness: &mut FrameHarness) -> Option<&mut S> {
    surface_of::<A, S>(harness.widget_mut::<ResourceRoot>()?)
}

/// The custom surface of type `S` that `A`'s view hosts, when `widget` is
/// `A`'s root as a host mounted it: a tab of `A` in a shell, found in the
/// host's own widget map.
pub fn surface_in_root<A: ResourceApp, S: 'static>(
    widget: &mut dyn ui_core::widget::Widget,
) -> Option<&mut S> {
    surface_of::<A, S>(widget.as_any_mut()?.downcast_mut::<ResourceRoot>()?)
}

fn surface_of<A: ResourceApp, S: 'static>(root: &mut ResourceRoot) -> Option<&mut S> {
    root.typed::<A::Resource>()?;
    root.content
        .as_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<S>())
}

/// A dispatcher outside any host, for a unit test of a surface (or of the
/// model behind it) that edits through one: each immediate action
/// ([`Dispatcher::apply_now`]) applies to the document handed to it,
/// attributed to `author`, through the same executor as a hosted one, so
/// the test exercises the surface's real write path. Queued invocations
/// ([`Dispatcher::apply`]) are not taken by anyone: drive those through
/// [`TestApp`].
pub fn detached_dispatcher<R: Resource>(author: &str) -> super::Dispatcher<R> {
    super::Dispatcher::new("test", R::KIND, false, Some(author))
}

/// The live state of `doc`, for a surface a unit test attaches outside any
/// host (see [`detached_dispatcher`]); the surface keeps it current from the
/// document its `sync` receives, as in a host.
///
/// # Errors
/// Returns why `doc` does not read as `R`.
pub fn detached_live<R: Resource>(
    doc: &tilo_resource::Doc,
) -> Result<super::Live<R>, tilo_resource::ModelError> {
    Ok(super::Live::new(tilo_resource::Reader::read(doc)?))
}

/// Inspect the window `A`'s live launch opens: the same definition and
/// component, mounted in a native [`ui_core::App`] on a fresh private
/// profile, without running its event loop. For what the window itself
/// declares, such as capture protection.
///
/// # Errors
/// Returns any launch, storage or construction failure.
pub fn inspect_live_window<A: ResourceApp, T>(
    inspect: impl FnOnce(&ui_core::App) -> T,
) -> Result<T, LaunchError> {
    drive_live_window::<A, T>(|app| inspect(app))
}

/// Drive the window `A`'s live launch opens, mounted as
/// [`inspect_live_window`] mounts it: the real [`ui_core::App`], which a
/// test or benchmark feeds input through [`ui_core::App::surface_input`].
///
/// # Errors
/// Returns any launch, storage or construction failure.
pub fn drive_live_window<A: ResourceApp, T>(
    drive: impl FnOnce(&mut ui_core::App) -> T,
) -> Result<T, LaunchError> {
    let profile = tempfile::tempdir().map_err(operation)?;
    let mut host = ApplicationHost::new(
        profile.path().join("state"),
        "test-device".into(),
        "test-workspace",
        UserId(AUTHOR.into()),
        role_permissions(AUTHOR, Role::Owner),
        tilo_actions::ActionContext::default(),
    )?;
    let definition = definition::<A>();
    let prepared = prepare_on(
        &mut host,
        definition,
        INSTANCE,
        None,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    let mut app = ui_core::App::from_component(component, prepared.runner_inputs)?;
    let seen = drive(&mut app);
    app.close_components();
    Ok(seen)
}

/// A dispatcher of `R` for testing a surface's own methods without a host:
/// the edits it applies now ([`Dispatcher::apply_now`]) go through the same
/// replayable executor a hosted surface's do, attributed to a test author,
/// onto the document the test hands the surface. Invocations it queues are
/// resolved by a host only; test those through [`TestApp`].
pub fn dispatcher<R: Resource>(app_id: &'static str) -> super::Dispatcher<R> {
    super::Dispatcher::new(app_id, R::KIND, false, Some(AUTHOR))
}

/// Resolve what `dispatcher` ([`dispatcher`]) queued since the last call —
/// its actions, typed values and groups — onto `doc` as a host does after an
/// event, as confirmed: each invocation one step, attributed to the test
/// author. An action its resource refuses (its clip is gone) changes
/// nothing and is reported through [`Dispatcher::refused`], as the host
/// reports it. Returns what each applied invocation applied, in order: one
/// entry per undo step (a group's entry holds all of its actions).
///
/// # Errors
/// Returns the first invocation that does not build, and an invocation of
/// something other than the resource's own actions (an effect), which only
/// a host runs.
pub fn resolve<R: Resource>(
    dispatcher: &super::Dispatcher<R>,
    doc: &mut tilo_resource::Doc,
) -> Result<Vec<Vec<R::Action>>, String> {
    let mut applied = Vec::new();
    while let Some((invocation, _)) = dispatcher.take_attributed() {
        if invocation.address.resource != dispatcher.resource_id() {
            return Err(format!(
                "`{}` on `{}` is not an action of {}",
                invocation.address.action,
                invocation.address.resource,
                R::LABEL
            ));
        }
        let now_ms = tilo_resource::origin_now_ms();
        let actions = if invocation.address.action == tilo_resource::GROUP {
            tilo_resource::group::build_group::<R>(&invocation.arguments, now_ms)?.actions
        } else {
            vec![tilo_resource::build_action::<R>(
                &invocation.address.action,
                &invocation.arguments,
                now_ms,
            )?]
        };
        let mut refused = None;
        for action in &actions {
            if let Err(error) =
                dispatcher.apply_confirmed(&mut ui_core::widget::Edits::lend(doc), action)
            {
                refused = Some(error.to_string());
                break;
            }
        }
        match refused {
            Some(reason) => dispatcher.refuse(&invocation.address.action, reason),
            None => applied.push(actions),
        }
    }
    Ok(applied)
}

/// The search source the framework builds for `R` from `entries` — its
/// resource's and its surfaces' — for testing what an app's entries find and
/// how their hits read without a window.
pub fn search_source<R: Resource>(
    entries: Vec<tilo_resource::SearchEntry>,
) -> impl ui_core::search::SearchProvider {
    super::root::ResourceSearch::new(
        R::LABEL,
        std::sync::Arc::new(super::root::IndexedEntries::built(entries, 0)),
    )
}

impl<A: ResourceApp> Drop for HeadlessApp<A> {
    fn drop(&mut self) {
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
            id: A::ID,
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
    permissions: Rc<RefCell<PermissionContext>>,
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
    let (mut host, instance) = match &profile {
        Profile::Temporary(directory, room, device) => (
            ApplicationHost::new_with_profile_lifetime(
                directory.path().join("state"),
                device.clone(),
                room,
                author,
                Rc::clone(&permissions),
                tilo_actions::ActionContext::default(),
                directory.clone(),
            )?,
            INSTANCE,
        ),
        Profile::DataDir(directory) => (
            data_dir_host(directory, &author, Rc::clone(&permissions))?,
            DATA_DIR_INSTANCE,
        ),
    };
    let definition = opening.definition;
    let records = host.records_for(definition)?;
    let named = (opening.launch_record)(&LaunchInput::new(arguments, None).with_options(&options));
    let mut prepared = prepare_on(
        &mut host,
        definition,
        instance,
        named,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let checkout = prepared.context.record().clone();
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
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    let mut harness = FrameHarness::from_component(component, viewport, prepared.runner_inputs)?;
    settle_launch(&mut harness);
    // The live host withdraws a component's whole action surface (the
    // palette's, agents', scripts') when any of its actions breaks its
    // contract. Every test that opens the app proves it would not.
    // A capture stages fixtures, not the app someone uses.
    let violations = if captured {
        Vec::new()
    } else {
        harness.component_contract_violations()
    };
    if !violations.is_empty() {
        return Err(LaunchError::Operation(format!(
            "{} publishes an action surface the host would refuse: {}",
            definition.app_id,
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
            Profile::Temporary(_, _, device) => device.clone(),
            Profile::DataDir(_) => TEST_DEVICE.to_owned(),
        };
        Profile::Temporary(
            Rc::new(tempfile::tempdir().map_err(operation)?),
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
/// [`Surface`](tilo_resource::Surface) and those of the person's records its
/// views open, every control its view, built headless on an empty record,
/// binds to something the resource does not publish, the ids of its effects
/// and named views, and the ids the framework keeps for its own actions
/// ([`reserved_action_ids`](super::reserved_action_ids)).
#[must_use]
pub fn surface_report<A: ResourceApp>() -> serde_json::Value {
    report_surface(
        &Opening::of::<A>(),
        <A::Resource as Resource>::surface(),
        &published_ids,
    )
}

/// The ids of the effects and of the named views a built view publishes.
type PublishedIds = (Vec<String>, Vec<String>);

/// [`PublishedIds`] of the root `harness` hosts.
fn published_ids(harness: &mut FrameHarness) -> PublishedIds {
    harness
        .with_widget_mut(|root: &mut ResourceRoot| root.published_ids())
        .unwrap_or_default()
}

/// [`surface_report`] of the app `opening` describes, whose resource's
/// surface is `surface`; `published` reads the ids of the effects and the
/// named views a built view publishes.
fn report_surface(
    opening: &Opening,
    surface: tilo_resource::Surface,
    published: &dyn Fn(&mut FrameHarness) -> PublishedIds,
) -> serde_json::Value {
    let collecting = collect_unbound();
    let profile = tempfile::tempdir().map(Rc::new).map_err(operation);
    let built = profile.and_then(|profile| {
        open_app(
            opening,
            Launch {
                profile: Profile::Temporary(profile, TEST_ROOM.to_owned(), TEST_DEVICE.to_owned()),
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
    // the resource's named views, whose ids must not be the framework's.
    let (effects, views, view) = match built {
        Ok(mut opened) => {
            let (effects, views) = published(&mut opened.harness);
            (effects, views, None)
        }
        Err(error) => (Vec::new(), Vec::new(), Some(error.to_string())),
    };
    let (unbound, opened) = collecting.finish();
    let mut resources = vec![surface];
    resources.extend(opened);
    serde_json::json!({
        "resources": resources,
        "unbound": unbound,
        "effects": effects,
        "views": views,
        "reserved": super::reserved_action_ids(),
        "view": view,
    })
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
/// on this thread, as [`TestApp`](super::testing::HeadlessApp) and
/// `tilo-app check` do: a typo in a binding must fail, not grey a button.
fn strict_bindings() {
    super::controls::STRICT.with(|strict| strict.set(true));
}
