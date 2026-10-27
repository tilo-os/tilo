//! Explicit process-host services shared by standalone application runners.
//! Apps supply their definition and migration; this owner supplies storage,
//! identity, authority and local presentation. No ambient profile discovery.
use super::{
    ApplicationDefinition, ApplicationLocalView, ComponentRunnerInputs, HostContext,
    HostContextInputs, InstanceId, InstanceViewStore, LaunchError, LaunchTarget, PermissionContext,
    PreparedComponentInputs, RecordCheckout, RecordId, RecordOpenError, RecordStore, RestoreInput,
    SavedInstanceView, UserId, View, validate_identity,
};
use crate::record::RecordOwners;
use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const PERSONAL_WORKSPACE: &str = "personal";
/// Names the directory the launcher keeps the person's own records of an
/// app in ([`crate::record::person`]): one per app, bound into its launches
/// in every room, so a record of the person's is one record on the device.
pub const PERSON_DIR_ENV: &str = "TILO_PERSON_DIR";
const DEVICE_ID_FILE: &str = ".tilo-device-id";

/// The host inputs a normal application process receives from the trusted
/// launcher. Storage and presentation identity are resolved once, before an
/// application factory is allowed to open a record.
pub struct NativeProcessHost {
    host: ApplicationHost,
    instance_id: InstanceId,
    default_record: RecordId,
    workspace_scoped: bool,
}

impl NativeProcessHost {
    /// Resolve the current process's launcher-owned namespace and authority.
    ///
    /// A workspace launch fails closed to viewer authority when the launcher
    /// does not explicitly mark it as an author. A standalone launch receives
    /// owner authority over its own app data, without gaining filesystem or
    /// network capabilities outside the process sandbox.
    ///
    /// # Errors
    /// Refuses mismatched package identity, malformed launcher values,
    /// relative or unavailable persistent storage, and corrupt device state.
    pub fn open(definition: &ApplicationDefinition) -> Result<Self, LaunchError> {
        Self::open_with(
            definition,
            NativeProcessEnvironment::capture(),
            crate::collaboration::local_user_id(),
        )
    }

    fn open_with(
        definition: &ApplicationDefinition,
        environment: NativeProcessEnvironment,
        author: UserId,
    ) -> Result<Self, LaunchError> {
        let launcher_instance = environment.continuity_instance.is_some();
        let resolved = environment.resolve(definition.app_id(), &author)?;
        let permissions = Rc::new(RefCell::new(resolved.permissions));
        // A workspace launch's participation follows the room's grants, as
        // the shell tells the window (`crate::room_sync`): read-only becomes
        // writable the moment the app's authorship arrives, and back.
        if resolved.workspace_scoped {
            crate::room_sync::follow_participation(&permissions);
        }
        let mut host = ApplicationHost::with_store(
            Rc::new(app_state::FsStore::new(resolved.profile_root)),
            resolved.device_id,
            &resolved.workspace_id,
            author,
            permissions,
            tilo_actions::ActionContext::default(),
        )?;
        host.package_id = Some(resolved.package_id);
        if let Some(person) = resolved.person_dir {
            // The launcher binds the person's directory into the sandbox.
            host.keep_person_records_in(Rc::new(app_state::FsStore::provisioned(person)));
        }
        // Where a window that names no record gets its fallback. Every app
        // the framework builds keeps one record per workspace
        // (`ApplicationDefinition::room_record`): `prepare_default` opens the
        // workspace's package primary for every window of it, so they all
        // show the same data across closing, reopening and restarts, and the
        // first creates it (adopting what per-window records held before, see
        // `HostContext::window_records`). A window keeps only its view of it.
        // A definition that keeps no room record (a fixture) gets a record per
        // launcher-issued window.
        Ok(Self {
            host,
            instance_id: resolved.instance_id,
            default_record: if launcher_instance {
                RecordId::window_default(resolved.instance_id)
            } else {
                RecordId::room_default()
            },
            workspace_scoped: resolved.workspace_scoped,
        })
    }

    /// Opt in to a model capability owned by this process host. Opening a
    /// generic host must not also consume a legacy Agent client's descriptor.
    #[must_use]
    pub fn with_models(mut self, models: crate::models::Models) -> Self {
        self.host.models = models;
        self
    }

    pub fn host_mut(&mut self) -> &mut ApplicationHost {
        &mut self.host
    }

    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Prepare this native window's named record or saved continuation.
    /// A launcher-issued fresh instance has its own durable fallback record;
    /// standalone profiles retain the room default. Room-record applications
    /// still use their package primary through the normal host policy.
    ///
    /// # Errors
    /// Refuses contested writers, invalid saved views and denied authority.
    pub fn prepare_launch(
        &mut self,
        definition: &ApplicationDefinition,
        named: Option<RecordId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let kind = definition.record_kind.clone();
        match named {
            Some(id) => self.host.prepare_record(
                definition,
                self.instance_id,
                RecordCheckout::new(id, kind),
                inputs,
            ),
            None => self.host.prepare_default(
                definition,
                self.instance_id,
                RecordCheckout::new(self.default_record.clone(), kind),
                None,
                inputs,
            ),
        }
    }

    pub fn workspace_scoped(&self) -> bool {
        self.workspace_scoped
    }

    /// Prepare a window of this process: its first, or one the shell opened
    /// in it later (`crate::window_door`). Every window is prepared on the
    /// one host the process opens for its first, so all of them read and
    /// write the app's records through one store and one writer; a second
    /// host would contend for the store the first holds.
    ///
    /// A window the shell opened later is the instance it names, with a
    /// record of its own where the app keeps one per window, as a launch
    /// with that instance would have been.
    ///
    /// # Errors
    /// As [`Self::open`] for the first window, and [`Self::prepare_launch`].
    pub fn prepare_window(
        definition: &ApplicationDefinition,
        named: Option<RecordId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        thread_local! {
            static PROCESS_HOST: RefCell<Option<NativeProcessHost>> = const { RefCell::new(None) };
        }
        PROCESS_HOST.with(|slot| {
            let mut slot = slot.borrow_mut();
            // A window that waited for its workspace's identity
            // (`crate::workspace_identity`) wrote as no one; the window that
            // replaces it writes as the person chosen, on a host of theirs.
            let author = crate::collaboration::local_user_id();
            if slot.as_ref().is_some_and(|host| host.host.author != author) {
                *slot = None;
            }
            let host = match &mut *slot {
                Some(host) => host,
                empty => empty.insert(Self::open(definition)?),
            };
            match crate::window_door::requested_instance() {
                Some(instance) => host.prepare_instance(definition, instance, named, inputs),
                None => host.prepare_launch(definition, named, inputs),
            }
        })
    }

    /// [`Self::prepare_launch`] for the window `instance` the shell opened in
    /// this process.
    fn prepare_instance(
        &mut self,
        definition: &ApplicationDefinition,
        instance: InstanceId,
        named: Option<RecordId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let kind = definition.record_kind.clone();
        match named {
            Some(id) => self.host.prepare_record(
                definition,
                instance,
                RecordCheckout::new(id, kind),
                inputs,
            ),
            None => self.host.prepare_default(
                definition,
                instance,
                RecordCheckout::new(RecordId::window_default(instance), kind),
                None,
                inputs,
            ),
        }
    }
}

#[derive(Default)]
struct NativeProcessEnvironment {
    package_id: Option<String>,
    /// The app's `surface.toml` whoever started it named
    /// ([`crate::app_surface::SURFACE_FILE_ENV`]), and its hex hash.
    surface: Option<PathBuf>,
    surface_hash: Option<String>,
    quick_app_id: Option<String>,
    workspace_id: Option<String>,
    participation: Option<String>,
    continuity_instance: Option<String>,
    person_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
    action_authority: Option<tilo_host::action_authority::AuthoritySource>,
}

struct ResolvedNativeProcessHost {
    package_id: String,
    profile_root: PathBuf,
    device_id: String,
    workspace_id: String,
    workspace_scoped: bool,
    instance_id: InstanceId,
    permissions: PermissionContext,
    /// Where the launcher keeps the person's own records of this app,
    /// shared by its launches in every room.
    person_dir: Option<PathBuf>,
}

impl NativeProcessEnvironment {
    fn capture() -> Self {
        fn string(name: &str) -> Option<String> {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        }
        fn path(name: &str) -> Option<PathBuf> {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        }
        Self {
            package_id: string("TILO_PACKAGE_ID"),
            surface: path(crate::app_surface::SURFACE_FILE_ENV),
            surface_hash: string(crate::app_surface::SURFACE_HASH_ENV),
            quick_app_id: string("QUICK_APP_ID"),
            workspace_id: string("TILO_WORKSPACE_ID"),
            participation: string("TILO_PARTICIPATION"),
            continuity_instance: string("TILO_CONTINUITY_INSTANCE"),
            person_dir: path(PERSON_DIR_ENV),
            xdg_data_home: path("XDG_DATA_HOME"),
            local_app_data: path("LOCALAPPDATA"),
            user_profile: path("USERPROFILE"),
            home: path("HOME"),
            action_authority: Some(tilo_host::action_authority::source(
                collab_types::action_authority::PROJECTION_PATH.into(),
                collab_types::action_authority::MAX_PROJECTION_BYTES,
            )),
        }
    }

    fn resolve(
        self,
        app_id: &str,
        author: &UserId,
    ) -> Result<ResolvedNativeProcessHost, LaunchError> {
        let package_id = self
            .package_id
            .as_deref()
            .or(self.quick_app_id.as_deref())
            .map_or_else(|| app_id.to_owned(), str::to_owned);
        if package_id != app_id {
            return Err(LaunchError::BindingMismatch);
        }
        if let Some(file) = &self.surface {
            let named = |error: String| {
                LaunchError::Operation(format!("surface {}: {error}", file.display()))
            };
            let bytes = tilo_files::file_at(file)
                .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
                .map_err(|error| named(error.to_string()))?;
            let hash = self.surface_hash.as_deref().ok_or_else(|| {
                named(format!(
                    "named without {}",
                    crate::app_surface::SURFACE_HASH_ENV
                ))
            })?;
            crate::app_surface::register_named(app_id, &bytes, hash).map_err(named)?;
        }
        let profile_root = self.persistent_profile_root()?;
        let device_id = persistent_device_id(&profile_root)?;
        let (workspace_id, workspace_scoped) = launcher_workspace(self.workspace_id.as_deref())?;
        let mut permissions = permissions_for_participation(
            author,
            &workspace_id,
            workspace_scoped,
            self.participation.as_deref(),
        )?;
        if workspace_scoped
            && let Some(source) = self.action_authority
            && source().ok().flatten().is_some()
        {
            let app = app_id.to_owned();
            let room = workspace_id.clone();
            let person = author.clone();
            let current_source = source.clone();
            let initial = read_action_authority_projection(&source, &room, &app, &person);
            if initial.is_ok() {
                permissions = permissions.with_action_authority(
                    app.clone(),
                    std::sync::Arc::new(move || {
                        action_authority_context(&current_source, &room, &app, &person)
                    }),
                );
            }
        }
        let instance_id = self.continuity_instance.map_or_else(
            || Ok(stable_instance_id(&package_id)),
            |value| {
                value.parse::<u64>().map_err(|_| {
                    LaunchError::Operation("invalid launcher continuation identity".into())
                })
            },
        )?;
        Ok(ResolvedNativeProcessHost {
            package_id,
            profile_root,
            device_id,
            workspace_id,
            workspace_scoped,
            instance_id,
            permissions,
            person_dir: self.person_dir.filter(|dir| dir.is_absolute()),
        })
    }

    fn persistent_profile_root(&self) -> Result<PathBuf, LaunchError> {
        let root = self
            .xdg_data_home
            .clone()
            .or_else(|| cfg!(windows).then(|| self.local_app_data.clone()).flatten())
            .or_else(|| {
                cfg!(windows)
                    .then(|| {
                        self.user_profile
                            .as_ref()
                            .map(|profile| profile.join("AppData/Local"))
                    })
                    .flatten()
            })
            .or_else(|| self.home.as_ref().map(|home| home.join(".local/share")))
            .ok_or_else(|| {
                LaunchError::Operation("no persistent application data directory".into())
            })?;
        if !root.is_absolute() {
            return Err(LaunchError::Operation(
                "application data directory must be absolute".into(),
            ));
        }
        Ok(root)
    }
}

/// The storage workspace a launcher-supplied room names: its canonical uuid,
/// or the personal workspace when the launch has none.
fn launcher_workspace(room: Option<&str>) -> Result<(String, bool), LaunchError> {
    match room {
        Some(value) => {
            let id = uuid::Uuid::parse_str(value).map_err(|_| {
                LaunchError::Operation("invalid launcher workspace identity".into())
            })?;
            Ok((id.to_string(), true))
        }
        None => Ok((PERSONAL_WORKSPACE.to_owned(), false)),
    }
}

/// One app's launcher-owned profile in one room, opened by the trusted
/// host from outside the app's sandbox: the same store, device identity and
/// storage workspace [`NativeProcessHost`] resolves inside it. The shell
/// uses it to carry a window's record and presentation to another room
/// ([`crate::record::transfer`]); it grants nothing the app itself lacks.
pub struct NativeProfile {
    store: Rc<dyn durable_store::DurableStore>,
    views: InstanceViewStore,
    device_id: String,
    workspace_id: String,
}

impl NativeProfile {
    /// `profile_root` is the directory the launcher points the app's
    /// `XDG_DATA_HOME` at; `room` is its launcher workspace id.
    ///
    /// # Errors
    /// A relative root, a malformed room or corrupt device identity.
    pub fn open(profile_root: &Path, room: Option<&str>) -> Result<Self, LaunchError> {
        if !profile_root.is_absolute() {
            return Err(LaunchError::Operation(
                "application data directory must be absolute".into(),
            ));
        }
        let (workspace_id, _) = launcher_workspace(room)?;
        let device_id = persistent_device_id(profile_root)?;
        let store: Rc<dyn durable_store::DurableStore> =
            Rc::new(app_state::FsStore::new(profile_root.to_path_buf()));
        let views = InstanceViewStore::with_store(Rc::clone(&store), &device_id, &workspace_id)
            .map_err(LaunchError::Restore)?;
        Ok(Self {
            store,
            views,
            device_id,
            workspace_id,
        })
    }

    /// This process's own profile and continuity instance, when the trusted
    /// launcher started it as an instance of a room's app.
    pub(crate) fn of_launch() -> Option<(Self, InstanceId)> {
        let environment = NativeProcessEnvironment::capture();
        let instance = environment.continuity_instance.as_deref()?.parse().ok()?;
        let root = environment.persistent_profile_root().ok()?;
        Self::open(&root, environment.workspace_id.as_deref())
            .ok()
            .map(|profile| (profile, instance))
    }

    pub fn store(&self) -> &Rc<dyn durable_store::DurableStore> {
        &self.store
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The storage workspace the app's records are scoped to.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The presentation one instance last saved here, naming the record it shows.
    ///
    /// # Errors
    /// Unreadable, malformed or foreign presentation.
    pub fn saved_view(
        &self,
        instance: InstanceId,
    ) -> Result<Option<SavedInstanceView>, LaunchError> {
        self.views.load(instance).map_err(LaunchError::Restore)
    }

    /// Give `instance` in this profile the presentation `view` had elsewhere,
    /// now showing `record`: what a window carried into this room reopens to,
    /// once the write, when there is one to wait for, has landed in
    /// [`Self::store`].
    ///
    /// # Errors
    /// An invalid record identity or a store that refused the write.
    pub fn adopt_view(
        &self,
        view: &SavedInstanceView,
        instance: InstanceId,
        record: &RecordCheckout,
    ) -> Result<Option<crate::record::transfer::Landing>, LaunchError> {
        let adopted = SavedInstanceView {
            device_id: self.device_id.clone(),
            workspace_id: self.workspace_id.clone(),
            instance_id: instance,
            record_id: record.id.as_str().to_owned(),
            record_kind: record.kind.as_str().to_owned(),
            ..view.clone()
        };
        self.views
            .save_landing(&adopted)
            .map_err(LaunchError::Restore)
    }
}

fn permissions_for_participation(
    author: &UserId,
    room: &str,
    workspace_scoped: bool,
    participation: Option<&str>,
) -> Result<PermissionContext, LaunchError> {
    let role = match participation {
        Some("read-only") => crate::permissions::Role::Viewer,
        Some("author") => crate::permissions::Role::Editor,
        None if workspace_scoped => crate::permissions::Role::Viewer,
        None => crate::permissions::Role::Owner,
        Some(_) => {
            return Err(LaunchError::Operation(
                "invalid launcher participation mode".into(),
            ));
        }
    };
    Ok(PermissionContext::member_of(room, author.0.clone(), role))
}

fn action_authority_context(
    source: &tilo_host::action_authority::AuthoritySource,
    room: &str,
    app: &str,
    person: &UserId,
) -> Result<PermissionContext, String> {
    // Current for as long as the host holds it: the source reads a policy
    // the host let go of as absent.
    let projection = read_action_authority_projection(source, room, app, person)?;
    let now_ms = crate::time::try_unix_now_ms().ok_or("host action policy time is unavailable")?;
    let mut standing = crate::permissions::Standing::person(projection.policy, person.0.clone());
    if projection.foreign {
        standing = standing.on_foreign_device();
    }
    Ok(PermissionContext::new(
        crate::permissions::Actor::person(person.0.clone()).in_room(room),
        Vec::new(),
        standing,
    )
    .at_unix_time(now_ms / 1000))
}

fn read_action_authority_projection(
    source: &tilo_host::action_authority::AuthoritySource,
    room: &str,
    app: &str,
    person: &UserId,
) -> Result<collab_types::action_authority::AppActionAuthorityProjection, String> {
    use collab_types::action_authority::AppActionAuthorityProjection;
    let bytes = source()?.ok_or("host action policy is unavailable")?;
    let projection: AppActionAuthorityProjection =
        serde_json::from_slice(&bytes).map_err(|_| "host action policy is invalid")?;
    projection.validate().map_err(str::to_owned)?;
    if !projection.matches(room, app, &person.0) {
        return Err("host action policy binding mismatch".into());
    }
    Ok(projection)
}

fn stable_instance_id(app_id: &str) -> InstanceId {
    // FNV-1a is deliberately fixed rather than DefaultHasher, whose output is
    // not a persistence contract. The app id makes standalone view keys unique
    // even though InstanceViewStore itself is shared below the profile root.
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, app_id.as_bytes());
    std::hash::Hasher::finish(&hasher)
}

fn persistent_device_id(profile_root: &Path) -> Result<String, LaunchError> {
    let path = profile_root.join(DEVICE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(value) => return validate_device_id(&value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LaunchError::Operation(format!(
                "reading {}: {error}",
                path.display()
            )));
        }
    }
    std::fs::create_dir_all(profile_root).map_err(|error| {
        LaunchError::Operation(format!("creating {}: {error}", profile_root.display()))
    })?;
    let generated = format!("device-{}", uuid::Uuid::new_v4());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match options.open(&path) {
        Ok(mut file) => {
            if let Err(error) = file
                .write_all(generated.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .and_then(|()| tilo_files::native::atomic::sync_directory(profile_root))
            {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(LaunchError::Operation(format!(
                    "writing {}: {error}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(LaunchError::Operation(format!(
                "creating {}: {error}",
                path.display()
            )));
        }
    }
    let value = std::fs::read_to_string(&path)
        .map_err(|error| LaunchError::Operation(format!("reading {}: {error}", path.display())))?;
    validate_device_id(&value)
}

fn validate_device_id(value: &str) -> Result<String, LaunchError> {
    let value = value.trim().to_owned();
    validate_identity(&value).map_err(LaunchError::Restore)?;
    if value
        .strip_prefix("device-")
        .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(LaunchError::Operation(
            "stored native device identity is malformed".into(),
        ));
    }
    Ok(value)
}

pub struct ApplicationHost {
    /// The launcher-bound package id of a native process; only that app's
    /// definition may open records through this host. Explicit/capture hosts
    /// take the app id from the definition passed to `records_for`.
    package_id: Option<String>,
    models: crate::models::Models,
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
    device_id: String,
    permissions: Rc<RefCell<PermissionContext>>,
    author: UserId,
    action_context: tilo_actions::ActionContext,
    /// Where the person's own records are kept: the host's store unless the
    /// host names another ([`Self::keep_person_records_in`]).
    person: RecordOwners,
}

impl ApplicationHost {
    /// Use the model capability authorized for this host's authenticated session.
    /// Every subsequently prepared component receives the same session, without
    /// installing a process-wide client.
    #[must_use]
    pub fn with_models(mut self, models: crate::models::Models) -> Self {
        self.models = models;
        self
    }

    /// Keep a host-owned profile resource (such as a temporary capture directory)
    /// alive through record writers and failed record or presentation save retries.
    /// The resource must own the supplied profile root for its entire lifetime.
    pub fn new_with_profile_lifetime(
        profile_root: PathBuf,
        device_id: String,
        workspace_id: &str,
        author: UserId,
        permissions: Rc<RefCell<PermissionContext>>,
        action_context: tilo_actions::ActionContext,
        lifetime: Rc<dyn std::any::Any>,
    ) -> Result<Self, LaunchError> {
        let mut host = Self::new(
            profile_root,
            device_id,
            workspace_id,
            author,
            permissions,
            action_context,
        )?;
        host.owners.retain_profile(lifetime.clone());
        host.person.retain_profile(lifetime.clone());
        let views = Rc::try_unwrap(host.views).map_err(|_| LaunchError::BindingMismatch)?;
        host.views = Rc::new(views.with_profile_lifetime(lifetime));
        Ok(host)
    }

    /// A host persisting under a local profile directory.
    /// # Errors
    /// Rejects invalid explicit profile and identity inputs. No files are opened.
    pub fn new(
        profile_root: PathBuf,
        device_id: String,
        workspace_id: &str,
        author: UserId,
        permissions: Rc<RefCell<PermissionContext>>,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        if !profile_root.is_absolute() {
            return Err(LaunchError::Restore(crate::view::ThawError::InvalidView(
                "profile root must be absolute".into(),
            )));
        }
        Self::with_store(
            Rc::new(app_state::FsStore::new(profile_root)),
            device_id,
            workspace_id,
            author,
            permissions,
            action_context,
        )
    }

    /// A host persisting records and local views through `store`.
    pub fn with_store(
        store: Rc<dyn durable_store::DurableStore>,
        device_id: String,
        workspace_id: &str,
        author: UserId,
        permissions: Rc<RefCell<PermissionContext>>,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        validate_identity(&author.0).map_err(LaunchError::Restore)?;
        let views = Rc::new(
            InstanceViewStore::with_store(Rc::clone(&store), &device_id, workspace_id)
                .map_err(LaunchError::Restore)?,
        );
        Ok(Self {
            package_id: None,
            models: crate::models::Models::default(),
            person: RecordOwners::with_store(Rc::clone(&store)),
            owners: RecordOwners::with_store(store),
            views,
            device_id,
            permissions,
            author,
            action_context,
        })
    }

    /// Keep the person's own records ([`crate::record::person`]) in `store`
    /// rather than the host's: the directory a launcher shares between an
    /// app's launches in every room.
    pub fn keep_person_records_in(&mut self, store: Rc<dyn durable_store::DurableStore>) {
        self.person = RecordOwners::with_store(store);
    }

    /// Hand `context` the person's own records of `definition`'s app.
    fn with_person_records(
        &mut self,
        definition: &ApplicationDefinition,
        mut context: HostContext,
    ) -> HostContext {
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        match crate::record::person::PersonRecords::open(
            &mut self.person,
            package_id,
            self.author.clone(),
        ) {
            Ok(records) => context.provide_person_records(records),
            Err(error) => tracing::warn!(%error, "the person's records are unavailable"),
        }
        // The records every app of the room opens, kept beside the app's
        // own in the same owners: one writer per app and room here.
        context.provide_workspace_records(crate::record::workspace::WorkspaceRecords::in_room(
            self.owners.clone(),
            self.views.workspace_id().to_owned(),
            self.author.clone(),
        ));
        context
    }

    /// Return the unique canonical writer registry for this app and workspace.
    /// Migration adapters use this owner too; they must never start a second saver.
    /// # Errors
    /// Rejects denied requirements and invalid package/storage namespaces.
    pub fn records_for(
        &mut self,
        definition: &ApplicationDefinition,
    ) -> Result<RecordStore, LaunchError> {
        for required in &definition.required {
            if !self.permissions.borrow().check(required) {
                return Err(LaunchError::CapabilityDenied(required.clone()));
            }
        }
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        if package_id != definition.app_id() {
            return Err(LaunchError::BindingMismatch);
        }
        self.owners
            .open(package_id, self.views.workspace_id())
            .map_err(LaunchError::Record)
    }

    /// Validate the local binding before a migration adapter touches any source.
    /// # Errors
    /// Rejects unreadable, malformed or foreign presentation without repairing it.
    pub fn saved_view_for(
        &self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        checkout: &RecordCheckout,
    ) -> Result<Option<SavedInstanceView>, LaunchError> {
        if checkout.kind != definition.record_kind
            || RecordId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
        {
            return Err(LaunchError::BindingMismatch);
        }
        let saved = self.views.load(instance_id).map_err(LaunchError::Restore)?;
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        if package_id != definition.app_id()
            || saved.as_ref().is_some_and(|saved| {
                saved.app_id != package_id
                    || saved.record_id != checkout.id.as_str()
                    || checkout.kind.as_str() != saved.record_kind
            })
        {
            return Err(LaunchError::BindingMismatch);
        }
        Ok(saved)
    }

    /// The record this instance's saved presentation names, when it names
    /// one of `definition`'s kind: a window carried in from another room
    /// arrives showing its own record rather than the room's default.
    /// # Errors
    /// Unreadable or foreign presentation.
    pub fn saved_record(
        &self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
    ) -> Result<Option<RecordCheckout>, LaunchError> {
        let Some(saved) = self.views.load(instance_id).map_err(LaunchError::Restore)? else {
            return Ok(None);
        };
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        let id = RecordId::parse(&saved.record_id).ok_or(LaunchError::BindingMismatch)?;
        if saved.app_id != package_id
            || definition.record_kind.as_str() != saved.record_kind
            || id.is_package_primary()
        {
            return Err(LaunchError::BindingMismatch);
        }
        Ok(Some(RecordCheckout::new(
            id,
            definition.record_kind.clone(),
        )))
    }

    /// Open the record this instance's saved presentation names, else the
    /// host's stable default record, or reserve that for the definition's New
    /// factory. Restore presentation from this exact device/workspace/instance.
    /// A saved view pointing at a missing record is an error, never a blank replacement.
    /// A record moved out of this room is never reopened: a view still naming
    /// it is dropped, and a retired record gives way to its successor.
    /// `initial_view` is used only when this instance has no saved presentation.
    /// # Errors
    /// Rejects invalid/foreign views, contested writers, corrupt records and denied authority.
    pub fn prepare_default(
        &mut self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        default: RecordCheckout,
        initial_view: Option<ApplicationLocalView>,
        runner_inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let records = self.records_for(definition)?;
        if definition.keeps_room_record() {
            let target = definition
                .unsaved_target(&records)
                .map_err(LaunchError::Record)?;
            let mut context = HostContext::prepare(
                definition,
                HostContextInputs {
                    records,
                    views: self.views.clone(),
                    device_id: self.device_id.clone(),
                    instance_id,
                    permissions: self.permissions.clone(),
                    author: self.author.clone(),
                    action_context: self.action_context.clone(),
                },
                &target,
            )?;
            context.provide_models(self.models.clone());
            // Every window shares the room's record, but each keeps its own
            // presentation: this instance reopens where it was left (the
            // note it showed), when what it saved is of the room's record.
            // A view of a record it kept per window before is not restored.
            let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
            let saved = if matches!(target, LaunchTarget::Record { .. }) {
                self.views
                    .load(instance_id)
                    .map_err(LaunchError::Restore)?
                    .filter(|saved| {
                        saved.app_id == package_id
                            && saved.record_id == context.record().id.as_str()
                            && definition.record_kind.names(&saved.record_kind)
                    })
            } else {
                None
            };
            let restore = saved
                .or_else(|| {
                    initial_view
                        .map(|application| context.envelope(View::default(), Some(application)))
                })
                .map_or(RestoreInput::None, RestoreInput::Local);
            return Ok(PreparedComponentInputs {
                context: self.with_person_records(definition, context),
                target,
                restore,
                runner_inputs,
            });
        }
        let checkout = self
            .saved_record(definition, instance_id)?
            .unwrap_or(default);
        let saved = self.saved_view_for(definition, instance_id, &checkout)?;
        let (context, target, restore) = self.prepare_checkout(
            definition,
            records,
            instance_id,
            checkout,
            saved,
            initial_view,
        )?;
        Ok(PreparedComponentInputs {
            context: self.with_person_records(definition, context),
            target,
            restore,
            runner_inputs,
        })
    }

    /// Open `checkout` for `instance_id`, or reserve it for the definition's
    /// New factory when it does not exist yet: the record a launch named (the
    /// file a person opened), whatever the instance showed before. The
    /// instance's saved presentation is restored only when it is of that
    /// record; one of another record is left for the next save to replace.
    /// # Errors
    /// Rejects foreign kinds, contested writers, corrupt records and denied authority.
    pub fn prepare_record(
        &mut self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        checkout: RecordCheckout,
        runner_inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let records = self.records_for(definition)?;
        if checkout.kind != definition.record_kind
            || RecordId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
            || checkout.id.is_package_primary()
        {
            return Err(LaunchError::BindingMismatch);
        }
        let saved = self
            .views
            .load(instance_id)
            .map_err(LaunchError::Restore)?
            .filter(|saved| {
                saved.record_id == checkout.id.as_str()
                    && checkout.kind.as_str() == saved.record_kind
            });
        let (context, target, restore) =
            self.prepare_checkout(definition, records, instance_id, checkout, saved, None)?;
        Ok(PreparedComponentInputs {
            context: self.with_person_records(definition, context),
            target,
            restore,
            runner_inputs,
        })
    }

    /// The checked construction the default and the named record share.
    fn prepare_checkout(
        &self,
        definition: &ApplicationDefinition,
        records: RecordStore,
        instance_id: InstanceId,
        mut checkout: RecordCheckout,
        mut saved: Option<SavedInstanceView>,
        initial_view: Option<ApplicationLocalView>,
    ) -> Result<(HostContext, LaunchTarget, RestoreInput), LaunchError> {
        /// Retired records followed before giving up: each move out of a room
        /// retires one, so this only bounds a pathological history.
        const MAX_SUCCESSORS: usize = 64;
        let mut successors = 0;
        let target = loop {
            match records.try_open_record(&checkout) {
                Ok(_) => {
                    break LaunchTarget::Record {
                        checkout: checkout.clone(),
                        reveal: None,
                    };
                }
                Err(RecordOpenError::MissingRecord) if saved.is_none() => break LaunchTarget::New,
                Err(RecordOpenError::Retired) if successors < MAX_SUCCESSORS => {
                    successors += 1;
                    saved = None;
                    checkout = RecordCheckout::new(
                        RecordId::derived("successor", &checkout.id),
                        checkout.kind.clone(),
                    );
                }
                Err(error) => return Err(LaunchError::Record(error)),
            }
        };
        let mut context = if matches!(target, LaunchTarget::New) {
            HostContext::prepare_new_record(
                definition,
                HostContextInputs {
                    records,
                    views: self.views.clone(),
                    device_id: self.device_id.clone(),
                    instance_id,
                    permissions: self.permissions.clone(),
                    author: self.author.clone(),
                    action_context: self.action_context.clone(),
                },
                checkout.id,
            )?
        } else {
            HostContext::prepare(
                definition,
                HostContextInputs {
                    records,
                    views: self.views.clone(),
                    device_id: self.device_id.clone(),
                    instance_id,
                    permissions: self.permissions.clone(),
                    author: self.author.clone(),
                    action_context: self.action_context.clone(),
                },
                &target,
            )?
        };
        context.provide_models(self.models.clone());
        let restore = saved
            .or_else(|| {
                initial_view.map(|application| context.envelope(View::default(), Some(application)))
            })
            .map_or(RestoreInput::None, RestoreInput::Local);
        Ok((context, target, restore))
    }
}

#[cfg(test)]
mod native_process_tests {
    use super::*;
    use crate::application::AppComponent;

    fn rejected_factory(
        _: HostContext,
        _: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        Err(LaunchError::Operation("factory must not run".into()))
    }

    fn definition(app_id: &'static str) -> ApplicationDefinition {
        ApplicationDefinition {
            app_id: app_id.into(),
            title: "Host probe".into(),
            icon: crate::widget::IconDescriptor::Glyph("H"),
            record_kind: crate::record::RecordKind::new("host.probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            create: rejected_factory,
        }
    }

    fn environment(root: &Path) -> NativeProcessEnvironment {
        NativeProcessEnvironment {
            xdg_data_home: Some(root.to_owned()),
            ..NativeProcessEnvironment::default()
        }
    }

    #[test]
    fn fresh_native_window_records_do_not_contest_an_existing_writer() {
        use crate::application_test_support as probe;
        let temp = tempfile::tempdir().unwrap();
        let definition = definition(probe::APP);
        let room = "3f2b1c00-0000-4000-8000-00000000000a";
        let launch = |instance: u64| {
            NativeProcessHost::open_with(
                &definition,
                NativeProcessEnvironment {
                    workspace_id: Some(room.into()),
                    participation: Some("author".into()),
                    continuity_instance: Some(instance.to_string()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let prepare = |native: &mut NativeProcessHost| {
            native.prepare_launch(&definition, None, probe::runner_inputs())
        };
        let mut first = launch(41);
        let mut first_record = prepare(&mut first).unwrap();
        first_record
            .context
            .initialize_new_record(|_| Ok(()))
            .unwrap();
        let mut second = launch(42);
        let second_record = prepare(&mut second).unwrap();
        assert_ne!(
            first_record.context.record(),
            second_record.context.record()
        );
        // Naming the first record explicitly still contends with its actual
        // live writer: distinct defaults must never weaken the storage lock.
        assert!(
            second
                .prepare_launch(
                    &definition,
                    Some(first_record.context.record().id.clone()),
                    probe::runner_inputs(),
                )
                .is_err()
        );
        let first_id = first_record.context.record().id.clone();
        first
            .host
            .views
            .save(&first_record.context.envelope(View::default(), None))
            .unwrap();
        drop(first_record);
        drop(first);
        let mut restored = launch(41);
        let restored_record = prepare(&mut restored).unwrap();
        assert_eq!(restored_record.context.record().id, first_id);
        assert!(matches!(
            restored_record.target,
            LaunchTarget::Record { .. }
        ));
    }

    #[test]
    fn native_window_defaults_preserve_standalone_primary_and_saved_records() {
        use crate::application_test_support as probe;
        let temp = tempfile::tempdir().unwrap();
        let definition = definition(probe::APP);
        let mut standalone =
            NativeProcessHost::open_with(&definition, environment(temp.path()), probe::author())
                .unwrap();
        let standalone_record = standalone
            .prepare_launch(&definition, None, probe::runner_inputs())
            .unwrap();
        assert_eq!(
            standalone_record.context.record().id,
            RecordId::room_default()
        );
        drop(standalone_record);
        drop(standalone);

        let room = "3f2b1c00-0000-4000-8000-00000000000b";
        let launch = || {
            NativeProcessHost::open_with(
                &definition,
                NativeProcessEnvironment {
                    workspace_id: Some(room.into()),
                    participation: Some("author".into()),
                    continuity_instance: Some("51".into()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        // A saved pre-fix window can still name the old room-default record.
        // Changing the fresh fallback must not replace its existing data.
        let mut legacy = launch();
        let mut old = legacy
            .host_mut()
            .prepare_default(
                &definition,
                51,
                RecordCheckout::new(RecordId::room_default(), definition.record_kind.clone()),
                None,
                probe::runner_inputs(),
            )
            .unwrap();
        old.context.initialize_new_record(|_| Ok(())).unwrap();
        legacy
            .host
            .views
            .save(&old.context.envelope(View::default(), None))
            .unwrap();
        drop(old);
        drop(legacy);
        let mut restored = launch();
        let old = restored
            .prepare_launch(&definition, None, probe::runner_inputs())
            .unwrap();
        assert_eq!(old.context.record().id, RecordId::room_default());
        assert!(matches!(old.target, LaunchTarget::Record { .. }));
        drop(old);
        drop(restored);

        let mut primary_definition = self::definition(probe::APP);
        primary_definition.room_record = true;
        let mut primary = launch();
        let prepared = primary
            .prepare_launch(&primary_definition, None, probe::runner_inputs())
            .unwrap();
        assert!(prepared.context.record().id.is_package_primary());
        assert!(matches!(prepared.target, LaunchTarget::PrimaryNew));
    }

    #[test]
    fn room_record_windows_share_one_record_and_see_the_per_window_ones() {
        use crate::application_test_support as probe;
        use automerge::{ReadDoc, transaction::Transactable};
        let temp = tempfile::tempdir().unwrap();
        let per_window = definition(probe::APP);
        let mut per_room = definition(probe::APP);
        per_room.room_record = true;
        let room = "3f2b1c00-0000-4000-8000-00000000000c";
        let launch = |definition: &ApplicationDefinition, instance: u64| {
            NativeProcessHost::open_with(
                definition,
                NativeProcessEnvironment {
                    workspace_id: Some(room.into()),
                    participation: Some("author".into()),
                    continuity_instance: Some(instance.to_string()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let note = |doc: &automerge::AutoCommit| -> Option<String> {
            match doc.get(automerge::ROOT, "note").unwrap() {
                Some((automerge::Value::Scalar(value), _)) => match value.as_ref() {
                    automerge::ScalarValue::Str(text) => Some(text.to_string()),
                    _ => None,
                },
                _ => None,
            }
        };
        // Two windows from before: each kept its own record.
        for (instance, text) in [(42, "second"), (41, "first")] {
            let mut window = launch(&per_window, instance);
            let mut prepared = window
                .prepare_launch(&per_window, None, probe::runner_inputs())
                .unwrap();
            assert_eq!(
                prepared.context.record().id,
                RecordId::window_default(instance)
            );
            prepared
                .context
                .initialize_new_record(|doc| {
                    doc.put(automerge::ROOT, "note", text)
                        .map_err(|e| e.to_string())
                })
                .unwrap();
        }
        // The first window of the room record creates it, and sees what the
        // per-window ones hold, in id order, without taking them over.
        let mut first = launch(&per_room, 43);
        let mut created = first
            .prepare_launch(&per_room, None, probe::runner_inputs())
            .unwrap();
        assert!(created.context.record().id.is_package_primary());
        assert!(matches!(created.target, LaunchTarget::PrimaryNew));
        let windows = created.context.window_records();
        assert_eq!(
            windows
                .iter()
                .map(|(id, doc)| (id.clone(), note(doc)))
                .collect::<Vec<_>>(),
            [
                (RecordId::window_default(41), Some("first".into())),
                (RecordId::window_default(42), Some("second".into())),
            ]
        );
        created
            .context
            .initialize_new_record(|doc| {
                doc.put(automerge::ROOT, "note", "shared")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        drop(created);
        drop(first);
        let mut second = launch(&per_room, 44);
        let reopened = second
            .prepare_launch(&per_room, None, probe::runner_inputs())
            .unwrap();
        assert!(reopened.context.record().id.is_package_primary());
        assert!(matches!(reopened.target, LaunchTarget::Record { .. }));
        assert_eq!(
            reopened.context.read_record(note).unwrap(),
            Some("shared".into())
        );
        // The per-window records are still there, unchanged.
        assert_eq!(reopened.context.window_records().len(), 2);
        drop(reopened);
        drop(second);
        let mut old = launch(&per_window, 41);
        let old = old
            .prepare_launch(&per_window, None, probe::runner_inputs())
            .unwrap();
        assert!(matches!(old.target, LaunchTarget::Record { .. }));
        assert_eq!(old.context.read_record(note).unwrap(), Some("first".into()));
        assert!(old.context.window_records().is_empty());
    }

    #[test]
    fn a_live_primary_kind_mismatch_reports_the_original_error_and_preserves_data() {
        use crate::application_test_support as probe;
        use automerge::{ReadDoc, transaction::Transactable};

        let temp = tempfile::tempdir().unwrap();
        let mut original = definition(probe::APP);
        original.room_record = true;
        let launch = |definition: &ApplicationDefinition| {
            NativeProcessHost::open_with(
                definition,
                NativeProcessEnvironment {
                    workspace_id: Some("3f2b1c00-0000-4000-8000-00000000000c".into()),
                    participation: Some("author".into()),
                    continuity_instance: Some("61".into()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let mut native = launch(&original);
        let mut prepared = native
            .prepare_launch(&original, None, probe::runner_inputs())
            .unwrap();
        prepared
            .context
            .initialize_new_record(|doc| {
                doc.put(automerge::ROOT, "body", "retained original")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        prepared
            .context
            .records
            .try_flush_record(&prepared.context.record().id)
            .unwrap();
        let mut incompatible = definition(probe::APP);
        incompatible.room_record = true;
        incompatible.record_kind = crate::record::RecordKind::new("host.changed");
        let existing = native.host.records_for(&incompatible).unwrap();
        assert!(
            existing.same_owner(&prepared.context.records),
            "the new definition must address the original live owner"
        );
        drop(existing);
        let error = native
            .prepare_launch(&incompatible, None, probe::runner_inputs())
            .err()
            .expect("a live owner cannot be reopened as another kind");
        assert!(
            matches!(
                error,
                LaunchError::Record(crate::record::RecordOpenError::IdentityMismatch)
            ),
            "{error}"
        );
        drop(prepared);
        drop(native);

        let mut native = launch(&original);
        let restored = native
            .prepare_launch(&original, None, probe::runner_inputs())
            .unwrap();
        assert!(matches!(restored.target, LaunchTarget::Record { .. }));
        assert_eq!(
            restored
                .context
                .read_record(|doc| {
                    doc.get(automerge::ROOT, "body")
                        .unwrap()
                        .unwrap()
                        .0
                        .into_string()
                        .unwrap()
                })
                .unwrap(),
            "retained original"
        );
    }

    #[test]
    fn window_default_storage_keys_name_exactly_their_record() {
        for instance in [0, 0x42, 0xf276_0000_0000_0044, u64::MAX] {
            let id = RecordId::window_default(instance);
            assert_eq!(
                RecordId::window_default_of_storage_key(&id.storage_key()),
                Some(id)
            );
        }
        assert_eq!(RecordId::window_default(0), RecordId::room_default());
        for other in [
            RecordId::package_primary(),
            RecordId::for_tab(0x42),
            RecordId::derived("notes", &RecordId::for_tab(0x42)),
        ] {
            assert_eq!(
                RecordId::window_default_of_storage_key(&other.storage_key()),
                None
            );
        }
        let real = RecordId::window_default(0x42).storage_key();
        let last = if real.ends_with('0') { '1' } else { '0' };
        let forged = format!("{}{last}", real.get(..real.len() - 1).unwrap());
        assert_eq!(RecordId::window_default_of_storage_key(&forged), None);
    }

    #[test]
    fn native_host_action_authority_is_current_and_scoped_to_the_app() {
        use crate::permissions::{ActionKind, Answer, TreePath};
        use collab_types::action_authority::AppActionAuthorityProjection;
        use std::sync::{Arc, Mutex};
        let temp = tempfile::tempdir().unwrap();
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let app = "so.tilo.store";
        let author = UserId("person-a".into());
        let original = AppActionAuthorityProjection::project(
            &tilo_action_policy::RoomPolicy::new(room, author.0.clone()),
            app,
            &author.0,
            false,
        );
        let bytes = Arc::new(Mutex::new(Some(serde_json::to_vec(&original).unwrap())));
        let source: tilo_host::action_authority::AuthoritySource = {
            let bytes = bytes.clone();
            Arc::new(move || Ok(bytes.lock().unwrap().clone()))
        };
        let environment = NativeProcessEnvironment {
            workspace_id: Some(room.into()),
            participation: Some("read-only".into()),
            action_authority: Some(source.clone()),
            ..environment(temp.path())
        };
        let resolved = environment.resolve(app, &author).unwrap();
        let permissions = resolved.permissions;
        let own = TreePath::root().app(app).action("buy");
        assert_eq!(permissions.role(), Some(crate::permissions::Role::Viewer));
        assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Allow);
        assert_eq!(
            permissions.answer(
                &TreePath::root().app("so.tilo.settings").action("reset"),
                ActionKind::Outside
            ),
            Answer::Deny,
            "another app's actions keep the launch participation"
        );
        let allowed = permissions.directory_witness().unwrap();
        let mut revoked = original.clone();
        revoked.policy.owner = None;
        *bytes.lock().unwrap() = Some(serde_json::to_vec(&revoked).unwrap());
        assert_ne!(permissions.directory_witness().unwrap(), allowed);
        assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
        {
            let mut invalid = original.clone();
            invalid.schema_version = 2;
            *bytes.lock().unwrap() = Some(serde_json::to_vec(&invalid).unwrap());
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
        }
        for (field, value) in [
            ("room", "foreign-room"),
            ("app_id", "so.tilo.other"),
            ("person", "person-b"),
        ] {
            let mut invalid = serde_json::to_value(&original).unwrap();
            invalid[field] = value.into();
            *bytes.lock().unwrap() = Some(serde_json::to_vec(&invalid).unwrap());
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
            assert!(permissions.directory_witness().is_err());
        }
        for invalid in [None, Some(b"invalid".to_vec())] {
            *bytes.lock().unwrap() = invalid;
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
            assert!(permissions.directory_witness().is_err());
        }
        *bytes.lock().unwrap() = Some(serde_json::to_vec(&original).unwrap());
        assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Allow);
        assert_eq!(permissions.directory_witness().unwrap(), allowed);
    }

    #[test]
    fn native_host_identity_authority_and_inputs_are_explicit() {
        {
            let temp = tempfile::tempdir().unwrap();
            let author = UserId("person-a".into());
            let first = NativeProcessHost::open_with(
                &definition("so.tilo.one"),
                environment(temp.path()),
                author.clone(),
            )
            .unwrap();
            let device = first.host.device_id.clone();
            assert!(!first.workspace_scoped());
            assert_eq!(first.host.views.workspace_id(), PERSONAL_WORKSPACE);
            assert_eq!(
                first.host.permissions.borrow().role(),
                Some(crate::permissions::Role::Owner)
            );
            assert_eq!(first.host.permissions.borrow().user(), &author);
            drop(first);

            let reopened = NativeProcessHost::open_with(
                &definition("so.tilo.one"),
                environment(temp.path()),
                author.clone(),
            )
            .unwrap();
            let other = NativeProcessHost::open_with(
                &definition("so.tilo.two"),
                environment(temp.path()),
                author,
            )
            .unwrap();
            assert_eq!(reopened.host.device_id, device);
            assert_eq!(other.host.device_id, device);
            assert_eq!(reopened.instance_id(), stable_instance_id("so.tilo.one"));
            assert_ne!(reopened.instance_id(), other.instance_id());
        }

        {
            let temp = tempfile::tempdir().unwrap();
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let make = |participation: Option<&str>| NativeProcessEnvironment {
                workspace_id: Some(room.into()),
                participation: participation.map(str::to_owned),
                continuity_instance: Some("42".into()),
                xdg_data_home: Some(temp.path().to_owned()),
                ..NativeProcessEnvironment::default()
            };
            for (mode, role) in [
                (Some("read-only"), crate::permissions::Role::Viewer),
                (Some("author"), crate::permissions::Role::Editor),
                (None, crate::permissions::Role::Viewer),
            ] {
                let resolved = make(mode)
                    .resolve("so.tilo.notes", &UserId("person-a".into()))
                    .unwrap();
                assert!(resolved.workspace_scoped);
                assert_eq!(resolved.workspace_id, room);
                assert_eq!(resolved.instance_id, 42);
                assert_eq!(resolved.permissions.role(), Some(role));
            }
        }

        {
            let temp = tempfile::tempdir().unwrap();
            let author = UserId("person-a".into());
            let mut mismatched = environment(temp.path());
            mismatched.package_id = Some("so.tilo.other".into());
            assert!(matches!(
                mismatched.resolve("so.tilo.notes", &author),
                Err(LaunchError::BindingMismatch)
            ));
            let relative = NativeProcessEnvironment {
                xdg_data_home: Some(PathBuf::from("relative")),
                ..NativeProcessEnvironment::default()
            };
            assert!(relative.resolve("so.tilo.notes", &author).is_err());
            assert!(
                NativeProcessEnvironment::default()
                    .resolve("so.tilo.notes", &author)
                    .is_err()
            );
            for invalid in [
                NativeProcessEnvironment {
                    workspace_id: Some("not-a-room".into()),
                    ..environment(temp.path())
                },
                NativeProcessEnvironment {
                    participation: Some("owner".into()),
                    ..environment(temp.path())
                },
                NativeProcessEnvironment {
                    continuity_instance: Some("window".into()),
                    ..environment(temp.path())
                },
            ] {
                assert!(invalid.resolve("so.tilo.notes", &author).is_err());
            }
            std::fs::write(temp.path().join(DEVICE_ID_FILE), b"damaged\n").unwrap();
            assert!(
                environment(temp.path())
                    .resolve("so.tilo.notes", &author)
                    .is_err()
            );
            assert_eq!(
                std::fs::read(temp.path().join(DEVICE_ID_FILE)).unwrap(),
                b"damaged\n"
            );
        }
    }

    #[test]
    fn a_saved_view_names_the_record_and_a_retired_one_gives_way() {
        use crate::application_test_support as probe;
        use automerge::transaction::Transactable as _;
        let temp = tempfile::tempdir().unwrap();
        let room = "3f2b1c00-0000-4000-8000-0000000000c1";
        let definition = definition(probe::APP);
        let default = RecordCheckout::new(
            RecordId::derived("default", &RecordId::for_tab(0)),
            definition.record_kind.clone(),
        );
        let carried = RecordCheckout::new(RecordId::minted(), definition.record_kind.clone());
        {
            let mut owners = RecordOwners::new(temp.path().to_owned());
            let records = owners.open(probe::APP, room).unwrap();
            records.create_record(&carried).unwrap();
            records
                .with(&carried.id, |doc| {
                    doc.put(automerge::ROOT, "body", "arrived").unwrap();
                })
                .unwrap();
            records.try_flush().unwrap();
        }
        let profile = NativeProfile::open(temp.path(), Some(room)).unwrap();
        let view = SavedInstanceView {
            version: 2,
            app_id: probe::APP.into(),
            device_id: "elsewhere".into(),
            workspace_id: "elsewhere".into(),
            instance_id: 7,
            record_id: default.id.as_str().into(),
            record_kind: default.kind.as_str().into(),
            view: View::default(),
            application: None,
        };
        let _ = profile.adopt_view(&view, 7, &carried).unwrap();
        let host = |temp: &Path| {
            ApplicationHost::new(
                temp.to_owned(),
                profile.device_id().to_owned(),
                room,
                probe::author(),
                probe::runner_permissions(),
                probe::provenance(),
            )
            .unwrap()
        };

        let prepared = host(temp.path())
            .prepare_default(
                &definition,
                7,
                default.clone(),
                None,
                probe::runner_inputs(),
            )
            .unwrap();
        assert!(
            matches!(&prepared.target, LaunchTarget::Record { checkout, .. } if checkout == &carried),
            "the instance reopens the record its view names, not the room default"
        );
        assert!(matches!(prepared.restore, RestoreInput::Local(_)));
        drop(prepared);

        let _ = crate::record::transfer::retire(
            profile.store(),
            probe::APP,
            room,
            &crate::record::transfer::Origin::Record(carried.clone()),
            &crate::record::transfer::Retirement::Moved(None),
        )
        .unwrap();
        let prepared = host(temp.path())
            .prepare_default(&definition, 7, default, None, probe::runner_inputs())
            .unwrap();
        assert!(
            matches!(prepared.target, LaunchTarget::New),
            "a record that left the room is never reopened; its successor starts fresh"
        );
        assert!(matches!(prepared.restore, RestoreInput::None));
        assert_ne!(prepared.context.record().id, carried.id);
    }

    #[test]
    fn signed_package_id_must_name_the_launched_definition() {
        let temp = tempfile::tempdir().unwrap();
        let author = UserId("person-a".into());
        let mut exact = environment(temp.path());
        exact.package_id = Some("so.tilo.notes".into());
        let resolved = exact.resolve("so.tilo.notes", &author).unwrap();
        assert_eq!(resolved.package_id, "so.tilo.notes");
        assert_eq!(resolved.instance_id, stable_instance_id("so.tilo.notes"));

        let mut foreign_package = environment(temp.path());
        foreign_package.package_id = Some("so.tilo.photos".into());
        assert!(matches!(
            foreign_package.resolve("so.tilo.notes", &author),
            Err(LaunchError::BindingMismatch)
        ));
    }
}
