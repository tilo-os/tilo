//! Explicit process-host services shared by standalone application runners. Apps supply their
//! definition and migration; this owner supplies storage, identity, authority and local presentation.
use super::{
    ApplicationDefinition, ApplicationLocalView, ComponentRunnerInputs, DocumentCheckout,
    DocumentId, InstanceId, InstanceViewStore, LaunchError, LaunchTarget, MemberId,
    PermissionContext, PreparedComponentInputs, ProcessContext, ProcessContextInputs,
    ResourceOpenError, ResourceStore, RestoreInput, SavedInstanceView, View, validate_identity,
};
use crate::resource::ResourceOwners;
use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The workspace a launch outside every workspace keeps its records in: this
/// device's own. A workspace is named by an id, so the records kept in it have
/// a route to travel on and the content their windows store has somewhere to
/// go.
pub const PERSONAL_WORKSPACE: &str = "d1a4f6b2-3c58-4e07-9b6a-70e2c5d81f43";
/// Names the directory the launcher keeps the person records of an app in ([`crate::resource::person`]):
/// one per app, bound into its launches in every workspace, so a record of the person's is one record
/// on the device.
pub const PERSON_DIR_ENV: &str = "TILO_PERSON_DIR";
const DEVICE_ID_FILE: &str = ".tilo-device-id";

/// The host inputs a normal application process receives from the trusted launcher.
pub struct NativeProcess {
    host: ApplicationOwner,
    instance_id: InstanceId,
    default_resource: DocumentId,
    workspace_scoped: bool,
}

impl NativeProcess {
    /// Resolve the current process's launcher-owned namespace and authority.
    ///
    /// A workspace launch decides nothing itself: its controls show and its presses run as the
    /// device's dispatch decides them. A standalone launch owns its app data, without gaining
    /// filesystem or network capabilities outside the process sandbox.
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
        author: MemberId,
    ) -> Result<Self, LaunchError> {
        let launcher_instance = environment.continuity_instance.is_some();
        let resolved = environment.resolve(definition.app_id(), &author)?;
        let permissions = crate::permissions::SharedPermissions::new(resolved.permissions);
        let mut host = ApplicationOwner::with_store(
            Rc::new(app_state::shared_store(resolved.profile_root)),
            resolved.device_id,
            &resolved.workspace_id,
            author,
            permissions,
            tilo_actions::ActionContext::default(),
        )?;
        host.package_id = Some(resolved.package_id);
        if let Some(person) = resolved.person_dir {
            host.keep_person_resources_in(Rc::new(app_state::FsStore::provisioned(person)));
        }
        Ok(Self {
            host,
            instance_id: resolved.instance_id,
            default_resource: if launcher_instance {
                DocumentId::window_default(resolved.instance_id)
            } else {
                DocumentId::workspace_default()
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

    pub fn host_mut(&mut self) -> &mut ApplicationOwner {
        &mut self.host
    }

    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Prepare this native window's named record or saved continuation.
    /// A launcher-issued fresh instance has its durable fallback record;
    /// standalone profiles retain the workspace default. Workspace-record applications
    /// still use their package primary through the normal host policy.
    ///
    /// # Errors
    /// Refuses contested writers, invalid saved views and denied authority.
    pub fn prepare_launch(
        &mut self,
        definition: &ApplicationDefinition,
        named: Option<DocumentId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let kind = definition.resource_type.clone();
        match named {
            Some(id) => self.host.prepare_resource(
                definition,
                self.instance_id,
                DocumentCheckout::new(id, kind),
                inputs,
            ),
            None => self.host.prepare_default(
                definition,
                self.instance_id,
                DocumentCheckout::new(self.default_resource.clone(), kind),
                None,
                inputs,
            ),
        }
    }

    pub fn workspace_scoped(&self) -> bool {
        self.workspace_scoped
    }

    /// Prepare a window of this process: its first, or one the shell opened in it later
    /// (`crate::window_link`).
    pub fn prepare_window(
        definition: &ApplicationDefinition,
        named: Option<DocumentId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        Self::with_process_host(
            definition,
            |host| match crate::window_link::requested_instance() {
                Some(instance) => host.prepare_instance(definition, instance, named, inputs),
                None => host.prepare_launch(definition, named, inputs),
            },
        )
    }

    /// The file or folder the window this process prepares next showed when it was last saved, while
    /// it is still there ([`crate::application_view::WindowShows`]): a launch naming nothing opens
    /// it again, as the launch that first opened it did.
    ///
    /// # Errors
    /// Refuses what [`Self::open`] refuses, and an unreadable saved view.
    pub fn window_shows(
        definition: &ApplicationDefinition,
    ) -> Result<Option<PathBuf>, LaunchError> {
        Self::with_process_host(definition, |host| {
            let instance = crate::window_link::requested_instance().unwrap_or(host.instance_id);
            Ok(host
                .host
                .saved_view(instance)?
                .and_then(|saved| saved.shown().map(Path::to_path_buf)))
        })
    }

    /// Run `with` on the host of this process's windows, opened for its first.
    fn with_process_host<T>(
        definition: &ApplicationDefinition,
        with: impl FnOnce(&mut Self) -> Result<T, LaunchError>,
    ) -> Result<T, LaunchError> {
        thread_local! {
            static PROCESS_HOST: RefCell<Option<NativeProcess>> = const { RefCell::new(None) };
        }
        PROCESS_HOST.with(|slot| {
            let mut slot = slot.borrow_mut();
            // A window that held a launch's place (its workspace's join)
            // wrote as no one; the window that replaces it writes as the
            // person, on a host of theirs.
            let author = crate::collaboration::local_user_id();
            if slot.as_ref().is_some_and(|host| host.host.author != author) {
                *slot = None;
            }
            let host = match &mut *slot {
                Some(host) => host,
                empty => empty.insert(Self::open(definition)?),
            };
            with(host)
        })
    }

    fn prepare_instance(
        &mut self,
        definition: &ApplicationDefinition,
        instance: InstanceId,
        named: Option<DocumentId>,
        inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let kind = definition.resource_type.clone();
        match named {
            Some(id) => self.host.prepare_resource(
                definition,
                instance,
                DocumentCheckout::new(id, kind),
                inputs,
            ),
            None => self.host.prepare_default(
                definition,
                instance,
                DocumentCheckout::new(DocumentId::window_default(instance), kind),
                None,
                inputs,
            ),
        }
    }
}

fn refuse_copy_while_joining(target: &LaunchTarget) -> Result<(), LaunchError> {
    if matches!(target, LaunchTarget::New | LaunchTarget::PrimaryNew)
        && crate::collaboration::native_initial::joining()
    {
        return Err(LaunchError::ResourceNotHere);
    }
    Ok(())
}

#[derive(Default)]
struct NativeProcessEnvironment {
    package_id: Option<String>,
    surface: Option<PathBuf>,
    surface_hash: Option<String>,
    quick_app_id: Option<String>,
    workspace_id: Option<String>,
    continuity_instance: Option<String>,
    person_dir: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
}

struct ResolvedNativeProcessHost {
    package_id: String,
    profile_root: PathBuf,
    device_id: String,
    workspace_id: String,
    workspace_scoped: bool,
    instance_id: InstanceId,
    permissions: PermissionContext,
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
            continuity_instance: string("TILO_CONTINUITY_INSTANCE"),
            person_dir: path(PERSON_DIR_ENV),
            xdg_data_home: path("XDG_DATA_HOME"),
            local_app_data: path("LOCALAPPDATA"),
            user_profile: path("USERPROFILE"),
            home: path("HOME"),
        }
    }

    fn resolve(
        self,
        app_id: &str,
        author: &MemberId,
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
        let permissions = if workspace_scoped {
            PermissionContext::new(author.clone())
        } else {
            PermissionContext::member_of(&workspace_id, author, crate::permissions::Role::Owner)
        };
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

fn launcher_workspace(workspace: Option<&str>) -> Result<(String, bool), LaunchError> {
    match workspace {
        Some(value) => {
            let id = uuid::Uuid::parse_str(value).map_err(|_| {
                LaunchError::Operation("invalid launcher workspace identity".into())
            })?;
            Ok((id.to_string(), true))
        }
        None => Ok((PERSONAL_WORKSPACE.to_owned(), false)),
    }
}

impl ProcessContext {
    /// The binding a component's controls, History and effects act under.
    pub(crate) fn callback_binding(&self) -> crate::bound_dispatch::CallbackBinding {
        crate::bound_dispatch::CallbackBinding {
            lifetime: self.wake_scope.handle(),
            closing: self.closing.clone(),
            registry_owner: Rc::downgrade(&self.construction),
            lifetime_id: self.lifetime_id,
            queued_controls: self.queued_controls.clone(),
            search_requests: self.search_requests.clone(),
            control_domain: self.control_domain.downgrade(),
            instance_id: self.instance_id,
            app_id: self.app_id.clone(),
            device_id: self.device_id.clone(),
            records: self.records.clone(),
            checkout: self.record.clone(),
            permissions: self.permissions.clone(),
            author: self.author.clone(),
            local_person: self.local_person.clone(),
            operation_context: std::rc::Rc::new(self.action_context.clone()),
            writes_as_author: self.writes_as_author,
        }
    }
}

/// One app's launcher-owned profile in one workspace, opened by the trusted host from outside the app's
/// sandbox: the same store, device identity and storage workspace [`NativeProcess`] resolves inside
/// it.
pub struct NativeProfile {
    store: Rc<dyn durable_store::DurableStore>,
    views: InstanceViewStore,
    device_id: String,
    workspace_id: String,
}

impl NativeProfile {
    /// `profile_root` is the directory the launcher points the app's `XDG_DATA_HOME` at; `workspace` is
    /// its launcher workspace id.
    pub fn open(profile_root: &Path, workspace: Option<&str>) -> Result<Self, LaunchError> {
        if !profile_root.is_absolute() {
            return Err(LaunchError::Operation(
                "application data directory must be absolute".into(),
            ));
        }
        let (workspace_id, _) = launcher_workspace(workspace)?;
        let device_id = persistent_device_id(profile_root)?;
        let store: Rc<dyn durable_store::DurableStore> =
            Rc::new(app_state::shared_store(profile_root));
        let views = InstanceViewStore::with_store(Rc::clone(&store), &device_id, &workspace_id)
            .map_err(LaunchError::Restore)?;
        Ok(Self {
            store,
            views,
            device_id,
            workspace_id,
        })
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
    pub fn saved_view(
        &self,
        instance: InstanceId,
    ) -> Result<Option<SavedInstanceView>, LaunchError> {
        self.views.load(instance).map_err(LaunchError::Restore)
    }

    /// Give `instance` in this profile the presentation `view` had elsewhere,
    /// now showing `record`: what a window carried into this workspace reopens to,
    /// once the write, when there is one to wait for, has landed in
    /// [`Self::store`].
    ///
    /// # Errors
    /// An invalid record identity or a store that refused the write.
    pub fn adopt_view(
        &self,
        view: &SavedInstanceView,
        instance: InstanceId,
        record: &DocumentCheckout,
    ) -> Result<Option<crate::resource::transfer::Landing>, LaunchError> {
        let adopted = SavedInstanceView {
            device_id: self.device_id.clone(),
            workspace_id: self.workspace_id.clone(),
            instance_id: instance,
            document_id: record.id.as_str().to_owned(),
            resource_type: record.kind.as_str().to_owned(),
            ..view.clone()
        };
        self.views
            .save_landing(&adopted)
            .map_err(LaunchError::Restore)
    }
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
    let written = match options.open(&path) {
        Ok(mut file) => file
            .write_all(generated.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .and_then(|()| tilo_files::native::atomic::sync_directory(profile_root)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            return Err(LaunchError::Operation(format!(
                "creating {}: {error}",
                path.display()
            )));
        }
    };
    if let Err(error) = written {
        let _ = std::fs::remove_file(&path);
        return Err(LaunchError::Operation(format!(
            "writing {}: {error}",
            path.display()
        )));
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

pub struct ApplicationOwner {
    /// The launcher-bound package id of a native process; only that app's definition may open records
    /// through this host.
    package_id: Option<String>,
    models: crate::models::Models,
    owners: ResourceOwners,
    views: Rc<InstanceViewStore>,
    device_id: String,
    permissions: crate::permissions::SharedPermissions,
    author: MemberId,
    action_context: tilo_actions::ActionContext,
    /// Where the person records are kept: the host's store unless the
    /// host names another ([`Self::keep_person_resources_in`]).
    person: ResourceOwners,
}

impl ApplicationOwner {
    /// Use the model capability authorized for this host's authenticated session.
    #[must_use]
    pub fn with_models(mut self, models: crate::models::Models) -> Self {
        self.models = models;
        self
    }

    /// The view store this host keeps: where a saved view of an instance is read and written.
    #[must_use]
    pub fn views(&self) -> &Rc<InstanceViewStore> {
        &self.views
    }

    /// The device this host runs the app on.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Who is at this device, as the host prepared them.
    #[must_use]
    pub fn author(&self) -> &MemberId {
        &self.author
    }

    /// The member's permissions, as the host follows them.
    #[must_use]
    pub fn permissions(&self) -> &crate::permissions::SharedPermissions {
        &self.permissions
    }

    /// Another window of the same profile, as the launcher opens one: it
    /// reads and writes the documents this window has open, and keeps its
    /// own saved view.
    ///
    /// # Errors
    ///
    /// The view store could not be opened.
    pub fn beside(
        &self,
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        let views = InstanceViewStore::with_store(
            self.views.durable_store(),
            &self.device_id,
            self.views.workspace_id(),
        )
        .map_err(LaunchError::Restore)?;
        Ok(Self {
            package_id: None,
            models: self.models.clone(),
            owners: self.owners.clone(),
            views: Rc::new(views),
            device_id: self.device_id.clone(),
            permissions,
            author,
            action_context,
            person: self.person.clone(),
        })
    }

    /// Keep a host-owned profile resource (such as a temporary capture directory)
    /// alive through record writers and failed record or presentation save retries.
    /// The resource must own the supplied profile root for its entire lifetime.
    pub fn new_with_profile_lifetime(
        profile_root: PathBuf,
        device_id: String,
        workspace_id: &str,
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
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
    pub fn new(
        profile_root: PathBuf,
        device_id: String,
        workspace_id: &str,
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        if !profile_root.is_absolute() {
            return Err(LaunchError::Restore(crate::view::ThawError::InvalidView(
                "profile root must be absolute".into(),
            )));
        }
        Self::with_store(
            Rc::new(app_state::shared_store(profile_root)),
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
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        validate_identity(author.id()).map_err(LaunchError::Restore)?;
        let views = Rc::new(
            InstanceViewStore::with_store(Rc::clone(&store), &device_id, workspace_id)
                .map_err(LaunchError::Restore)?,
        );
        Ok(Self {
            package_id: None,
            models: crate::models::Models::default(),
            person: ResourceOwners::with_store(Rc::clone(&store)),
            owners: ResourceOwners::with_store(store),
            views,
            device_id,
            permissions,
            author,
            action_context,
        })
    }

    /// Keep the person records ([`crate::resource::person`]) in `store` rather than the host's: the
    /// directory a launcher shares between an app's launches in every workspace.
    pub fn keep_person_resources_in(&mut self, store: Rc<dyn durable_store::DurableStore>) {
        self.person = ResourceOwners::with_store(store);
    }

    fn with_person_resources(
        &mut self,
        definition: &ApplicationDefinition,
        mut context: ProcessContext,
    ) -> ProcessContext {
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        match crate::resource::person::PersonResources::open(
            &mut self.person,
            package_id,
            self.author.clone(),
        ) {
            Ok(records) => context.provide_person_resources(records),
            Err(error) => tracing::warn!(%error, "the person's records are unavailable"),
        }
        context.provide_workspace_resources(
            crate::resource::workspace::WorkspaceResources::in_workspace(
                self.owners.clone(),
                self.views.workspace_id().to_owned(),
                self.author.clone(),
            ),
        );
        context
    }

    /// Return the unique canonical writer registry for this app and workspace.
    /// Migration adapters use this owner too; they must never start a second saver.
    /// # Errors
    /// Rejects denied requirements and invalid package/storage namespaces.
    pub fn resources_for(
        &mut self,
        definition: &ApplicationDefinition,
    ) -> Result<ResourceStore, LaunchError> {
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        if package_id != definition.app_id() {
            return Err(LaunchError::BindingMismatch);
        }
        self.owners
            .open(package_id, self.views.workspace_id())
            .map_err(LaunchError::Record)
    }

    /// What `instance_id` saved of its window, unless the file or folder it showed is gone: nothing
    /// of that window applies any more, and it opens as a window on nothing does.
    fn saved_view(
        &self,
        instance_id: InstanceId,
    ) -> Result<Option<SavedInstanceView>, LaunchError> {
        Ok(self
            .views
            .load(instance_id)
            .map_err(LaunchError::Restore)?
            .filter(|saved| saved.shows.is_none() || saved.shown().is_some()))
    }

    /// The record this instance's saved presentation names, with that presentation, when it names
    /// one of `definition`'s kind: a window carried in from another workspace arrives showing its
    /// record rather than the workspace's default.
    fn saved_resource(
        &self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
    ) -> Result<Option<(DocumentCheckout, SavedInstanceView)>, LaunchError> {
        let Some(saved) = self.saved_view(instance_id)? else {
            return Ok(None);
        };
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
        let id = DocumentId::parse(&saved.document_id).ok_or(LaunchError::BindingMismatch)?;
        if package_id != definition.app_id()
            || saved.app_id != package_id
            || definition.resource_type.as_str() != saved.resource_type
            || id.is_package_primary()
        {
            return Err(LaunchError::BindingMismatch);
        }
        Ok(Some((
            DocumentCheckout::new(id, definition.resource_type.clone()),
            saved,
        )))
    }

    /// Open the record this instance's saved presentation names, else the
    /// host's stable default record, or reserve that for the definition's New
    /// factory. Restore presentation from this exact device/workspace/instance.
    /// A saved view pointing at a missing record is an error, never a blank replacement, unless the
    /// window shows a file or folder: the record is made again from that, as the launch on it made it.
    /// A record moved out of this workspace is never reopened: a view still naming
    /// it is dropped, and a retired record gives way to its successor.
    /// `initial_view` is used only when this instance has no saved presentation.
    /// # Errors
    /// Rejects invalid/foreign views, contested writers, corrupt records and denied authority.
    pub fn prepare_default(
        &mut self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        default: DocumentCheckout,
        initial_view: Option<ApplicationLocalView>,
        runner_inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let records = self.resources_for(definition)?;
        if definition.keeps_workspace_resource() {
            let primary = DocumentCheckout::package_primary(definition.resource_type.clone());
            let target = if definition.opens_before_resource
                && records
                    .begin_opening(&primary)
                    .map_err(LaunchError::Record)?
            {
                LaunchTarget::Record {
                    checkout: primary,
                    reveal: None,
                }
            } else {
                definition
                    .unsaved_target(&records)
                    .map_err(LaunchError::Record)?
            };
            refuse_copy_while_joining(&target)?;
            let mut context = ProcessContext::prepare(
                definition,
                ProcessContextInputs {
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
            let package_id = self.package_id.as_deref().unwrap_or(definition.app_id());
            let saved = if matches!(target, LaunchTarget::Record { .. }) {
                self.saved_view(instance_id)?.filter(|saved| {
                    saved.app_id == package_id
                        && saved.document_id == context.record().id.as_str()
                        && definition.resource_type.names(&saved.resource_type)
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
                context: self.with_person_resources(definition, context),
                target,
                restore,
                runner_inputs,
            });
        }
        let (checkout, saved) = match self.saved_resource(definition, instance_id)? {
            Some((checkout, saved)) => (checkout, Some(saved)),
            None => (default, None),
        };
        let (context, target, restore) = self.prepare_checkout(
            definition,
            records,
            instance_id,
            checkout,
            saved,
            initial_view,
        )?;
        Ok(PreparedComponentInputs {
            context: self.with_person_resources(definition, context),
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
    pub fn prepare_resource(
        &mut self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        checkout: DocumentCheckout,
        runner_inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let records = self.resources_for(definition)?;
        if checkout.kind != definition.resource_type
            || DocumentId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
            || checkout.id.is_package_primary()
        {
            return Err(LaunchError::BindingMismatch);
        }
        let saved = self.saved_view(instance_id)?.filter(|saved| {
            saved.document_id == checkout.id.as_str()
                && checkout.kind.as_str() == saved.resource_type
        });
        let (context, target, restore) =
            self.prepare_checkout(definition, records, instance_id, checkout, saved, None)?;
        Ok(PreparedComponentInputs {
            context: self.with_person_resources(definition, context),
            target,
            restore,
            runner_inputs,
        })
    }

    fn prepare_checkout(
        &self,
        definition: &ApplicationDefinition,
        records: ResourceStore,
        instance_id: InstanceId,
        mut checkout: DocumentCheckout,
        mut saved: Option<SavedInstanceView>,
        initial_view: Option<ApplicationLocalView>,
    ) -> Result<(ProcessContext, LaunchTarget, RestoreInput), LaunchError> {
        const MAX_SUCCESSORS: usize = 64;
        let mut successors = 0;
        let target = loop {
            match records.try_open_resource(&checkout) {
                Ok(_) => {
                    break LaunchTarget::Record {
                        checkout: checkout.clone(),
                        reveal: None,
                    };
                }
                Err(ResourceOpenError::MissingResource)
                    if saved.as_ref().is_none_or(|saved| saved.shown().is_some()) =>
                {
                    refuse_copy_while_joining(&LaunchTarget::New)?;
                    saved = None;
                    break LaunchTarget::New;
                }
                Err(ResourceOpenError::Retired) if successors < MAX_SUCCESSORS => {
                    successors += 1;
                    saved = None;
                    checkout = DocumentCheckout::new(
                        DocumentId::derived("successor", &checkout.id),
                        checkout.kind.clone(),
                    );
                }
                Err(error) => return Err(LaunchError::Record(error)),
            }
        };
        let mut context = if matches!(target, LaunchTarget::New) {
            ProcessContext::prepare_new_resource(
                definition,
                ProcessContextInputs {
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
            ProcessContext::prepare(
                definition,
                ProcessContextInputs {
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
        _: ProcessContext,
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
            resource_type: crate::resource::ResourceType::new("host.probe"),
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: rejected_factory,
        }
    }

    fn environment(root: &Path) -> NativeProcessEnvironment {
        NativeProcessEnvironment {
            xdg_data_home: Some(root.to_owned()),
            ..NativeProcessEnvironment::default()
        }
    }

    /// A window opened on a file names it in its saved view before the file's record is written:
    /// the next start opens that file again, and once the file is gone, the window opens on the
    /// app's own default with nothing to complain about.
    #[test]
    fn a_window_on_a_file_opens_on_it_again_and_on_nothing_once_it_is_gone() {
        use crate::application_test_support as probe;
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("Budget for Friday.txt");
        std::fs::write(&file, "Room 4: 40 EUR\n").unwrap();
        let definition = definition(probe::APP);
        let launch = || {
            NativeProcess::open_with(
                &definition,
                NativeProcessEnvironment {
                    workspace_id: Some("3f2b1c00-0000-4000-8000-00000000005a".into()),
                    continuity_instance: Some("51".into()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let of_file = DocumentId::for_file(&file);
        {
            let mut native = launch();
            let opened = native
                .prepare_launch(&definition, Some(of_file.clone()), probe::runner_inputs())
                .unwrap();
            assert!(matches!(opened.target, LaunchTarget::New));
            opened.context.window_shows().show(&file);
        }

        let mut native = launch();
        let shown = native.host.saved_view(51).unwrap().unwrap();
        assert_eq!(shown.shown(), Some(file.as_path()));
        let again = native
            .prepare_launch(&definition, Some(of_file.clone()), probe::runner_inputs())
            .expect("the file the window showed opens again though its record was never written");
        assert_eq!(again.context.record().id, of_file);
        assert!(matches!(again.target, LaunchTarget::New));
        drop(again);

        std::fs::remove_file(&file).unwrap();
        let mut native = launch();
        assert!(native.host.saved_view(51).unwrap().is_none());
        let fresh = native
            .prepare_launch(&definition, None, probe::runner_inputs())
            .expect("a window whose file is gone opens on the app's default");
        assert_eq!(fresh.context.record().id, DocumentId::window_default(51));
        assert!(matches!(fresh.restore, RestoreInput::None));
    }

    tilo_test_env::scenario! {
        fn native_window_records_open_share_and_keep_their_writers;

        phase fresh_native_window_records_do_not_contest_an_existing_writer;
        {
            use crate::application_test_support as probe;
            let temp = tempfile::tempdir().unwrap();
            let definition = definition(probe::APP);
            let workspace = "3f2b1c00-0000-4000-8000-00000000000a";
            let launch = |instance: u64| {
                NativeProcess::open_with(
                    &definition,
                    NativeProcessEnvironment {
                        workspace_id: Some(workspace.into()),
                        continuity_instance: Some(instance.to_string()),
                        ..environment(temp.path())
                    },
                    probe::author(),
                )
                .unwrap()
            };
            let prepare = |native: &mut NativeProcess| {
                native.prepare_launch(&definition, None, probe::runner_inputs())
            };
            let mut first = launch(41);
            let mut first_resource = prepare(&mut first).unwrap();
            first_resource
                .context
                .initialize_new_resource(|_| Ok(()))
                .unwrap();
            let mut second = launch(42);
            let second_resource = prepare(&mut second).unwrap();
            assert_ne!(
                first_resource.context.record(),
                second_resource.context.record()
            );
            // Naming the first record explicitly still contends with its actual
            // live writer: distinct defaults must never weaken the storage lock.
            assert!(
                second
                    .prepare_launch(
                        &definition,
                        Some(first_resource.context.record().id.clone()),
                        probe::runner_inputs(),
                    )
                    .is_err()
            );
            let first_id = first_resource.context.record().id.clone();
            first
                .host
                .views
                .save(&first_resource.context.envelope(View::default(), None))
                .unwrap();
            drop(first_resource);
            drop(first);
            let mut restored = launch(41);
            let restored_resource = prepare(&mut restored).unwrap();
            assert_eq!(restored_resource.context.record().id, first_id);
            assert!(matches!(
                restored_resource.target,
                LaunchTarget::Record { .. }
            ));
        }

        phase members_open_the_same_workspace_primary;
        {
            use crate::application_test_support as probe;
            let mut definition = definition(probe::APP);
            definition.workspace_resource = true;
            let workspace = "3f2b1c00-0000-4000-8000-00000000000c";
            let resource_of = |person: &str| {
                let temp = tempfile::tempdir().unwrap();
                let mut native = NativeProcess::open_with(
                    &definition,
                    NativeProcessEnvironment {
                        workspace_id: Some(workspace.into()),
                        continuity_instance: Some("61".into()),
                        ..environment(temp.path())
                    },
                    MemberId::person(person),
                )
                .unwrap();
                native
                    .prepare_launch(&definition, None, probe::runner_inputs())
                    .unwrap()
                    .context
                    .record()
                    .id
                    .clone()
            };
            let (ada, bo) = ("ab".repeat(32), "cd".repeat(32));
            assert_eq!(
                resource_of(&ada),
                DocumentId::package_primary(),
                "Ada opens the workspace record"
            );
            assert_eq!(
                resource_of(&bo),
                DocumentId::package_primary(),
                "Bo opens that same workspace record"
            );
            assert!(
                resource_of("person-a").is_package_primary(),
                "a standalone identity also opens the workspace record"
            );
        }

        phase native_window_defaults_preserve_standalone_primary_and_saved_records;
        {
            use crate::application_test_support as probe;
            let temp = tempfile::tempdir().unwrap();
            let definition = definition(probe::APP);
            let mut standalone =
                NativeProcess::open_with(&definition, environment(temp.path()), probe::author())
                    .unwrap();
            let standalone_resource = standalone
                .prepare_launch(&definition, None, probe::runner_inputs())
                .unwrap();
            assert_eq!(
                standalone_resource.context.record().id,
                DocumentId::workspace_default()
            );
            drop(standalone_resource);
            drop(standalone);

            let workspace = "3f2b1c00-0000-4000-8000-00000000000b";
            let launch = || {
                NativeProcess::open_with(
                    &definition,
                    NativeProcessEnvironment {
                        workspace_id: Some(workspace.into()),
                        continuity_instance: Some("51".into()),
                        ..environment(temp.path())
                    },
                    probe::author(),
                )
                .unwrap()
            };
            // A saved pre-fix window can still name the old workspace-default record.
            // Changing the fresh fallback must not replace its existing data.
            let mut legacy = launch();
            let mut old = legacy
                .host_mut()
                .prepare_default(
                    &definition,
                    51,
                    DocumentCheckout::new(
                        DocumentId::workspace_default(),
                        definition.resource_type.clone(),
                    ),
                    None,
                    probe::runner_inputs(),
                )
                .unwrap();
            old.context.initialize_new_resource(|_| Ok(())).unwrap();
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
            assert_eq!(old.context.record().id, DocumentId::workspace_default());
            assert!(matches!(old.target, LaunchTarget::Record { .. }));
            drop(old);
            drop(restored);

            let mut primary_definition = self::definition(probe::APP);
            primary_definition.workspace_resource = true;
            let mut primary = launch();
            let prepared = primary
                .prepare_launch(&primary_definition, None, probe::runner_inputs())
                .unwrap();
            assert!(prepared.context.record().id.is_package_primary());
            assert!(matches!(prepared.target, LaunchTarget::PrimaryNew));
        }

        phase workspace_record_windows_share_one_record_and_see_the_per_window_ones;
        {
            use crate::application_test_support as probe;
            use automerge::{ReadDoc, transaction::Transactable};
            let temp = tempfile::tempdir().unwrap();
            let per_window = definition(probe::APP);
            let mut per_workspace = definition(probe::APP);
            per_workspace.workspace_resource = true;
            let workspace = "3f2b1c00-0000-4000-8000-00000000000c";
            let launch = |definition: &ApplicationDefinition, instance: u64| {
                NativeProcess::open_with(
                    definition,
                    NativeProcessEnvironment {
                        workspace_id: Some(workspace.into()),
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
            for (instance, text) in [(42, "second"), (41, "first")] {
                let mut window = launch(&per_window, instance);
                let mut prepared = window
                    .prepare_launch(&per_window, None, probe::runner_inputs())
                    .unwrap();
                assert_eq!(
                    prepared.context.record().id,
                    DocumentId::window_default(instance)
                );
                prepared
                    .context
                    .initialize_new_resource(|doc| {
                        doc.put(automerge::ROOT, "note", text)
                            .map_err(|e| e.to_string())
                    })
                    .unwrap();
            }
            let mut first = launch(&per_workspace, 43);
            let mut created = first
                .prepare_launch(&per_workspace, None, probe::runner_inputs())
                .unwrap();
            assert!(created.context.record().id.is_package_primary());
            assert!(matches!(created.target, LaunchTarget::PrimaryNew));
            let windows = created.context.window_resources();
            assert_eq!(
                windows
                    .iter()
                    .map(|(id, doc)| (id.clone(), note(doc)))
                    .collect::<Vec<_>>(),
                [
                    (DocumentId::window_default(41), Some("first".into())),
                    (DocumentId::window_default(42), Some("second".into())),
                ]
            );
            created
                .context
                .initialize_new_resource(|doc| {
                    doc.put(automerge::ROOT, "note", "shared")
                        .map_err(|e| e.to_string())
                })
                .unwrap();
            drop(created);
            drop(first);
            let mut second = launch(&per_workspace, 44);
            let reopened = second
                .prepare_launch(&per_workspace, None, probe::runner_inputs())
                .unwrap();
            assert!(reopened.context.record().id.is_package_primary());
            assert!(matches!(reopened.target, LaunchTarget::Record { .. }));
            assert_eq!(
                reopened.context.read_resource(note).unwrap(),
                Some("shared".into())
            );
            assert_eq!(reopened.context.window_resources().len(), 2);
            drop(reopened);
            drop(second);
            let mut old = launch(&per_window, 41);
            let old = old
                .prepare_launch(&per_window, None, probe::runner_inputs())
                .unwrap();
            assert!(matches!(old.target, LaunchTarget::Record { .. }));
            assert_eq!(old.context.read_resource(note).unwrap(), Some("first".into()));
            assert!(old.context.window_resources().is_empty());
        }

        phase a_live_primary_kind_mismatch_reports_the_original_error_and_preserves_data;
        {
            use crate::application_test_support as probe;
            use automerge::{ReadDoc, transaction::Transactable};

            let temp = tempfile::tempdir().unwrap();
            let mut original = definition(probe::APP);
            original.workspace_resource = true;
            let launch = |definition: &ApplicationDefinition| {
                NativeProcess::open_with(
                    definition,
                    NativeProcessEnvironment {
                        workspace_id: Some("3f2b1c00-0000-4000-8000-00000000000c".into()),
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
                .initialize_new_resource(|doc| {
                    doc.put(automerge::ROOT, "body", "retained original")
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            prepared
                .context
                .records
                .try_flush_resource(&prepared.context.record().id)
                .unwrap();
            let mut incompatible = definition(probe::APP);
            incompatible.workspace_resource = true;
            incompatible.resource_type = crate::resource::ResourceType::new("host.changed");
            let existing = native.host.resources_for(&incompatible).unwrap();
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
                    LaunchError::Record(crate::resource::ResourceOpenError::IdentityMismatch)
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
                    .read_resource(|doc| {
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

        phase window_default_storage_keys_name_exactly_their_record;
        {
            for instance in [0, 0x42, 0xf276_0000_0000_0044, u64::MAX] {
                let id = DocumentId::window_default(instance);
                assert_eq!(
                    DocumentId::window_default_of_storage_key(&id.storage_key()),
                    Some(id)
                );
            }
            assert_eq!(DocumentId::window_default(0), DocumentId::workspace_default());
            for other in [
                DocumentId::package_primary(),
                DocumentId::for_tab(0x42),
                DocumentId::derived("notes", &DocumentId::for_tab(0x42)),
            ] {
                assert_eq!(
                    DocumentId::window_default_of_storage_key(&other.storage_key()),
                    None
                );
            }
            let real = DocumentId::window_default(0x42).storage_key();
            let last = if real.ends_with('0') { '1' } else { '0' };
            let forged = format!("{}{last}", real.get(..real.len() - 1).unwrap());
            assert_eq!(DocumentId::window_default_of_storage_key(&forged), None);
        }

        phase a_saved_view_names_the_record_and_a_retired_one_gives_way;
        {
            use crate::application_test_support as probe;
            use automerge::transaction::Transactable as _;
            let temp = tempfile::tempdir().unwrap();
            let workspace = "3f2b1c00-0000-4000-8000-0000000000c1";
            let definition = definition(probe::APP);
            let default = DocumentCheckout::new(
                DocumentId::derived("default", &DocumentId::for_tab(0)),
                definition.resource_type.clone(),
            );
            let carried = DocumentCheckout::new(DocumentId::minted(), definition.resource_type.clone());
            {
                let mut owners = ResourceOwners::new(temp.path().to_owned());
                let records = owners.open(probe::APP, workspace).unwrap();
                records.create_resource(&carried).unwrap();
                records
                    .with(&carried.id, |doc| {
                        doc.put(automerge::ROOT, "body", "arrived").unwrap();
                    })
                    .unwrap();
                records.try_flush().unwrap();
            }
            let profile = NativeProfile::open(temp.path(), Some(workspace)).unwrap();
            let view = SavedInstanceView {
                version: SavedInstanceView::VERSION,
                app_id: probe::APP.into(),
                device_id: "elsewhere".into(),
                workspace_id: "elsewhere".into(),
                instance_id: 7,
                document_id: default.id.as_str().into(),
                resource_type: default.kind.as_str().into(),
                view: View::default(),
                application: None,
                shows: None,
            };
            let _ = profile.adopt_view(&view, 7, &carried).unwrap();
            let host = |temp: &Path| {
                ApplicationOwner::new(
                    temp.to_owned(),
                    profile.device_id().to_owned(),
                    workspace,
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
                "the instance reopens the record its view names, not the workspace default"
            );
            assert!(matches!(prepared.restore, RestoreInput::Local(_)));
            drop(prepared);

            let _ = crate::resource::transfer::retire(
                profile.store(),
                probe::APP,
                workspace,
                &crate::resource::transfer::Origin::Record(carried.clone()),
                &crate::resource::transfer::Retirement::Moved(None),
            )
            .unwrap();
            let prepared = host(temp.path())
                .prepare_default(&definition, 7, default, None, probe::runner_inputs())
                .unwrap();
            assert!(
                matches!(prepared.target, LaunchTarget::New),
                "a record that left the workspace is never reopened; its successor starts fresh"
            );
            assert!(matches!(prepared.restore, RestoreInput::None));
            assert_ne!(prepared.context.record().id, carried.id);
        }
    }

    #[test]
    #[cfg(all(unix, feature = "broker-transport"))]
    fn a_record_not_here_is_not_created_while_the_workspace_joins() {
        use crate::application_test_support as probe;
        use crate::collaboration::native_initial::NativeConstruction;
        let temp = tempfile::tempdir().unwrap();
        let workspace = "3f2b1c00-0000-4000-8000-00000000000c";
        let launch = || {
            NativeProcess::open_with(
                &definition(probe::APP),
                NativeProcessEnvironment {
                    workspace_id: Some(workspace.into()),
                    continuity_instance: Some("52".into()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let joining = || {
            let scope = crate::wake::WakeScope::new();
            let (manager, broker) = {
                let _entry = scope.enter();
                crate::collaboration::CollaborationManager::native_joining_fixture_for_test(
                    crate::collaboration::RouteSpec::app(probe::APP, workspace.parse().unwrap()),
                    "probe",
                    automerge::AutoCommit::new().save(),
                    runtime_collab::WorkspaceStatus::Ready,
                )
            };
            (
                NativeConstruction::enter_component(manager, scope).unwrap(),
                broker,
            )
        };
        let mut per_workspace = definition(probe::APP);
        per_workspace.workspace_resource = true;
        let per_window = definition(probe::APP);

        for definition in [&per_workspace, &per_window] {
            let mut host = launch();
            let (construction, _broker) = joining();
            assert!(matches!(
                host.prepare_launch(definition, None, probe::runner_inputs()),
                Err(LaunchError::ResourceNotHere)
            ));
            drop(construction);
            let prepared = host
                .prepare_launch(definition, None, probe::runner_inputs())
                .unwrap();
            assert!(matches!(
                prepared.target,
                LaunchTarget::New | LaunchTarget::PrimaryNew
            ));
        }

        let mut host = launch();
        let mut created = host
            .prepare_launch(&per_workspace, None, probe::runner_inputs())
            .unwrap();
        created.context.initialize_new_resource(|_| Ok(())).unwrap();
        drop(created);
        drop(host);
        let mut host = launch();
        let (_construction, _broker) = joining();
        let prepared = host
            .prepare_launch(&per_workspace, None, probe::runner_inputs())
            .unwrap();
        assert!(matches!(prepared.target, LaunchTarget::Record { .. }));
        assert!(prepared.context.record().id.is_package_primary());
    }

    /// A window that shows before the workspace's record (a tilolang app) opens the record this device
    /// has off the frame.
    #[test]
    #[cfg(all(unix, feature = "broker-transport"))]
    fn a_window_opening_its_workspace_record_off_the_frame_is_admitted_on_every_launch() {
        use crate::application_test_support as probe;
        use crate::collaboration::native_initial::NativeConstruction;
        use automerge::{ReadDoc, transaction::Transactable};
        let temp = tempfile::tempdir().unwrap();
        let workspace = "3f2b1c00-0000-4000-8000-00000000000d";
        let mut definition = definition(probe::APP);
        definition.workspace_resource = true;
        definition.opens_before_resource = true;
        let launch = |instance: u64| {
            NativeProcess::open_with(
                &definition,
                NativeProcessEnvironment {
                    workspace_id: Some(workspace.into()),
                    continuity_instance: Some(instance.to_string()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let mut host = launch(61);
        let mut created = host
            .prepare_launch(&definition, None, probe::runner_inputs())
            .unwrap();
        created
            .context
            .initialize_new_resource(|doc| {
                doc.put(automerge::ROOT, "note", "kept")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        drop(created);
        drop(host);

        for instance in [61, 61, 62] {
            let mut host = launch(instance);
            let scope = crate::wake::WakeScope::new();
            let (manager, _broker) = {
                let _entry = scope.enter();
                crate::collaboration::CollaborationManager::native_joining_fixture_for_test(
                    crate::collaboration::RouteSpec::app(probe::APP, workspace.parse().unwrap()),
                    definition.resource_type.as_str(),
                    automerge::AutoCommit::new().save(),
                    runtime_collab::WorkspaceStatus::Ready,
                )
            };
            let construction = NativeConstruction::enter_component(manager, scope).unwrap();
            let prepared = host
                .prepare_launch(&definition, None, probe::runner_inputs())
                .unwrap();
            assert!(matches!(prepared.target, LaunchTarget::Record { .. }));
            let opening = prepared
                .context
                .resource_opening()
                .expect("the record this device holds opens off the frame");
            construction
                .bind_component(&prepared.context.callback_binding())
                .expect("the relaunched component is admitted on the workspace's route");
            assert!(opening.take_in(true).unwrap());
            prepared
                .context
                .read_resource(|doc| {
                    assert_eq!(
                        doc.get(automerge::ROOT, "note")
                            .unwrap()
                            .and_then(|(value, _)| value.into_string().ok())
                            .as_deref(),
                        Some("kept")
                    );
                })
                .unwrap();
            drop(prepared);
            drop(construction);
            drop(host);
        }
    }

    tilo_test_env::scenario! {
        fn a_native_host_keeps_authority_and_identity_explicit;

        phase native_host_identity_authority_and_inputs_are_explicit;
        {
            {
                let temp = tempfile::tempdir().unwrap();
                let author = MemberId::person("person-a");
                let first = NativeProcess::open_with(
                    &definition("so.tilo.one"),
                    environment(temp.path()),
                    author.clone(),
                )
                .unwrap();
                let device = first.host.device_id.clone();
                assert!(!first.workspace_scoped());
                assert_eq!(first.host.views.workspace_id(), PERSONAL_WORKSPACE);
                assert_eq!(
                    first.host.permissions.borrow().verdict(
                        &crate::permissions::ShownAction::of_app(
                            "so.tilo.one",
                            crate::permissions::ActionKind::Undoable
                        )
                    ),
                    Some(crate::permissions::ActionVerdict::Allowed)
                );
                assert_eq!(first.host.permissions.borrow().user(), &author);
                drop(first);

                let reopened = NativeProcess::open_with(
                    &definition("so.tilo.one"),
                    environment(temp.path()),
                    author.clone(),
                )
                .unwrap();
                let other = NativeProcess::open_with(
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
                let workspace = "3f2b1c00-0000-4000-8000-000000000001";
                let resolved = NativeProcessEnvironment {
                    workspace_id: Some(workspace.into()),
                    continuity_instance: Some("42".into()),
                    xdg_data_home: Some(temp.path().to_owned()),
                    ..NativeProcessEnvironment::default()
                }
                .resolve("so.tilo.notes", &MemberId::person("person-a"))
                .unwrap();
                assert!(resolved.workspace_scoped);
                assert_eq!(resolved.workspace_id, workspace);
                assert_eq!(resolved.instance_id, 42);
                assert!(
                    resolved.permissions.host().is_none(),
                    "a workspace launch decides nothing itself: the dispatch does"
                );
            }

            {
                let temp = tempfile::tempdir().unwrap();
                let author = MemberId::person("person-a");
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
                        workspace_id: Some("not-a-workspace".into()),
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

        phase signed_package_id_must_name_the_launched_definition;
        {
            let temp = tempfile::tempdir().unwrap();
            let author = MemberId::person("person-a");
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
}
