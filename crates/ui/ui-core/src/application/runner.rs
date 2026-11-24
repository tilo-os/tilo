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
const DEVICE_ID_FILE: &str = ".tilo-device-id";

/// The workspace's store over this process's connection to Tilo on the device.
fn connected_documents() -> Result<Rc<dyn crate::resource::DocumentPort>, LaunchError> {
    #[cfg(feature = "broker-transport")]
    if let Some(port) = crate::resource::ConnectionDocuments::connected() {
        return Ok(Rc::new(port));
    }
    Err(LaunchError::NotConnected)
}

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
    /// A workspace launch fails closed to viewer authority when the launcher
    /// does not explicitly mark it as an author. A standalone launch receives
    /// owner authority over its app data, without gaining filesystem or
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
        author: MemberId,
    ) -> Result<Self, LaunchError> {
        let launcher_instance = environment.continuity_instance.is_some();
        let resolved = environment.resolve(definition.app_id(), &author)?;
        let permissions = crate::permissions::SharedPermissions::new(resolved.permissions);
        if resolved.workspace_scoped {
            crate::workspace_sync::follow_participation(&permissions);
        }
        // The window's records are the workspace's: read over the connection the window joins,
        // and what it changes published on the records' routes. Without one nothing it does
        // would be kept, which the person is told rather than finding out later.
        let documents = match resolved.documents {
            Some(documents) => documents,
            None => (connected_documents()?, false),
        };
        let mut host = ApplicationOwner::with_port(
            Some(documents),
            &resolved.package_id,
            resolved.device_id,
            &resolved.workspace_id,
            author,
            permissions,
            tilo_actions::ActionContext::default(),
        )?;
        host.package_id = Some(resolved.package_id);
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
    participation: Option<String>,
    continuity_instance: Option<String>,
    /// Where the window's records are read, and whether it writes them back itself; the
    /// workspace's store over the window's connection when `None`, its routes publishing what it
    /// changes.
    documents: Option<(Rc<dyn crate::resource::DocumentPort>, bool)>,
    xdg_data_home: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    home: Option<PathBuf>,
    action_authority: Option<HostAuthority>,
}

#[derive(Clone)]
struct HostAuthority {
    read: tilo_process_services::action_authority::AuthoritySource,
    watch: AuthorityWatcher,
}

type AuthorityWatcher = std::sync::Arc<
    dyn Fn(
            Box<dyn Fn() + Send>,
        ) -> Result<tilo_process_services::action_authority::AuthorityWatch, String>
        + Send
        + Sync,
>;

struct ResolvedNativeProcessHost {
    package_id: String,
    device_id: String,
    workspace_id: String,
    workspace_scoped: bool,
    instance_id: InstanceId,
    permissions: PermissionContext,
    documents: Option<(Rc<dyn crate::resource::DocumentPort>, bool)>,
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
            documents: None,
            xdg_data_home: path("XDG_DATA_HOME"),
            local_app_data: path("LOCALAPPDATA"),
            user_profile: path("USERPROFILE"),
            home: path("HOME"),
            action_authority: Some(HostAuthority {
                read: tilo_process_services::action_authority::source(
                    collab_types::action_authority::PROJECTION_PATH.into(),
                    collab_types::action_authority::MAX_PROJECTION_BYTES,
                ),
                watch: std::sync::Arc::new(|changed| {
                    tilo_process_services::action_authority::watch(
                        Path::new(collab_types::action_authority::PROJECTION_PATH),
                        changed,
                    )
                }),
            }),
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
        let mut permissions = permissions_for_participation(
            author,
            &workspace_id,
            workspace_scoped,
            self.participation.as_deref(),
        )?;
        if workspace_scoped
            && let Some(authority) = self.action_authority
            && (authority.read)().ok().flatten().is_some()
        {
            match FollowedAuthority::follow(authority, &workspace_id, app_id, author) {
                Ok(followed) => {
                    permissions = permissions.with_action_authority(
                        app_id.to_owned(),
                        std::sync::Arc::new(move || followed.current()),
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "the host's action authority is not followed");
                }
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
            device_id,
            workspace_id,
            workspace_scoped,
            instance_id,
            permissions,
            documents: self.documents,
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

/// One app's windows on this device in one workspace, as the trusted host opens them from outside
/// the app's sandbox: the same device identity, workspace and saved views [`NativeProcess`]
/// resolves inside it.
pub struct NativeProfile {
    port: Option<Rc<dyn crate::resource::DocumentPort>>,
    views: InstanceViewStore,
    device_id: String,
    workspace_id: String,
}

impl NativeProfile {
    /// `profile_root` is the directory the launcher points the app's `XDG_DATA_HOME` at, which
    /// names the device; `workspace` is its launcher workspace id; `port` reaches the workspace's
    /// one store, where `app_id`'s records and saved views are.
    pub fn open(
        profile_root: &Path,
        workspace: Option<&str>,
        app_id: &str,
        port: Option<Rc<dyn crate::resource::DocumentPort>>,
    ) -> Result<Self, LaunchError> {
        if !profile_root.is_absolute() {
            return Err(LaunchError::Operation(
                "application data directory must be absolute".into(),
            ));
        }
        let (workspace_id, _) = launcher_workspace(workspace)?;
        let device_id = persistent_device_id(profile_root)?;
        let views = InstanceViewStore::new(port.clone(), app_id, &device_id, &workspace_id)
            .map_err(LaunchError::Restore)?;
        Ok(Self {
            port,
            views,
            device_id,
            workspace_id,
        })
    }

    /// Where the app's records in the workspace are read and written.
    pub fn port(&self) -> Option<&Rc<dyn crate::resource::DocumentPort>> {
        self.port.as_ref()
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

    /// Give `instance` in this profile the presentation `view` had elsewhere, now showing
    /// `record`: what a window carried into this workspace reopens to.
    ///
    /// # Errors
    /// An invalid record identity or a workspace that refused the write.
    pub fn adopt_view(
        &self,
        view: &SavedInstanceView,
        instance: InstanceId,
        record: &DocumentCheckout,
    ) -> Result<(), LaunchError> {
        let adopted = SavedInstanceView {
            device_id: self.device_id.clone(),
            workspace_id: self.workspace_id.clone(),
            instance_id: instance,
            document_id: record.id.as_str().to_owned(),
            resource_type: record.kind.as_str().to_owned(),
            ..view.clone()
        };
        self.views.save(&adopted).map_err(LaunchError::Restore)
    }
}

fn permissions_for_participation(
    author: &MemberId,
    workspace: &str,
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
    Ok(PermissionContext::member_of(workspace, author, role))
}

/// The host's view of the person's authority over one app's actions, read
/// again only when the host changes it: what deciding reads is the last
/// read, never the file. Where its changes cannot be heard (the process has
/// no watch left), it is read again at each decision instead, so a missing
/// watch costs reads, never the person's authority.
struct FollowedAuthority {
    state: std::sync::Arc<FollowedState>,
    /// What hears the host change it; `None` when it cannot be heard.
    watch: Option<std::sync::Mutex<tilo_process_services::action_authority::AuthorityWatch>>,
}

struct FollowedState {
    authority: HostAuthority,
    workspace: String,
    app: String,
    person: MemberId,
    /// Held across a read and the store of what it read, so a read never
    /// replaces a later one.
    reading: std::sync::Mutex<()>,
    current: std::sync::RwLock<Result<PermissionContext, String>>,
}

impl FollowedState {
    fn read(&self) {
        let _reading = self
            .reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let read = action_authority_context(
            &self.authority.read,
            &self.workspace,
            &self.app,
            &self.person,
        );
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = read;
    }
}

impl FollowedAuthority {
    /// Read the host's view now and again on each change of it.
    ///
    /// # Errors
    /// When it cannot be read now.
    fn follow(
        authority: HostAuthority,
        workspace: &str,
        app: &str,
        person: &MemberId,
    ) -> Result<Self, String> {
        let state = std::sync::Arc::new(FollowedState {
            authority: authority.clone(),
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            person: person.clone(),
            reading: std::sync::Mutex::new(()),
            current: std::sync::RwLock::new(Err("process action policy is unread".into())),
        });
        let changed = std::sync::Arc::downgrade(&state);
        let watch = (authority.watch)(Box::new(move || {
            if let Some(state) = changed.upgrade() {
                state.read();
                crate::permissions::outside_changed();
            }
        }))
        .map_err(
            |error| tracing::warn!(%error, "the host's action authority is read at each decision"),
        )
        .ok();
        state.read();
        if let Err(error) = &*state
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(error.clone());
        }
        Ok(Self {
            state,
            watch: watch.map(std::sync::Mutex::new),
        })
    }

    fn current(&self) -> Result<PermissionContext, String> {
        if self.watch.is_none() {
            self.state.read();
        }
        let current = self
            .state
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let now_ms =
            crate::time::try_unix_now_ms().ok_or("process action policy time is unavailable")?;
        Ok(current.at_unix_time(now_ms / 1000))
    }
}

/// `context` deciding `app`'s actions by the host's view `read` gives of its person's authority in
/// `workspace`, read again whenever `watch` reports a change.
#[cfg(test)]
pub(crate) fn follow_process_authority(
    context: PermissionContext,
    read: tilo_process_services::action_authority::AuthoritySource,
    watch: AuthorityWatcher,
    workspace: &str,
    app: &str,
) -> Result<PermissionContext, String> {
    let person = context.user().clone();
    let followed =
        FollowedAuthority::follow(HostAuthority { read, watch }, workspace, app, &person)?;
    Ok(context.with_action_authority(
        app.to_owned(),
        std::sync::Arc::new(move || followed.current()),
    ))
}

/// A watch a test reports the host's changes through, as the kernel would.
#[cfg(test)]
pub(crate) fn manual_authority_watch() -> (AuthorityWatcher, std::sync::Arc<dyn Fn() + Send + Sync>)
{
    type Listener = Box<dyn Fn() + Send>;
    let listener: std::sync::Arc<std::sync::Mutex<Option<Listener>>> = std::sync::Arc::default();
    let installed = listener.clone();
    let watch: AuthorityWatcher = std::sync::Arc::new(move |changed| {
        *installed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(changed);
        Ok(Box::new(()))
    });
    let changed = std::sync::Arc::new(move || {
        if let Some(changed) = &*listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            changed();
        }
    });
    (watch, changed)
}

fn action_authority_context(
    source: &tilo_process_services::action_authority::AuthoritySource,
    workspace: &str,
    app: &str,
    person: &MemberId,
) -> Result<PermissionContext, String> {
    let projection = read_action_authority_projection(source, workspace, app, person)?;
    Ok(PermissionContext::new(
        crate::permissions::Acting::member(person.clone()),
        Vec::new(),
        projection.access(),
    ))
}

fn read_action_authority_projection(
    source: &tilo_process_services::action_authority::AuthoritySource,
    workspace: &str,
    app: &str,
    person: &MemberId,
) -> Result<collab_types::action_authority::AppActionAuthorityProjection, String> {
    use collab_types::action_authority::AppActionAuthorityProjection;
    let bytes = source()?.ok_or("process action policy is unavailable")?;
    let projection: AppActionAuthorityProjection =
        serde_json::from_slice(&bytes).map_err(|_| "process action policy is invalid")?;
    projection.validate().map_err(str::to_owned)?;
    if !person
        .person_id()
        .is_some_and(|person| projection.matches(workspace, app, person))
    {
        return Err("process action policy binding mismatch".into());
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
    /// Where the person records are kept: through the host's store, on the person's own
    /// workspace's routes.
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
        let views = self
            .views
            .beside(&self.device_id, self.views.workspace_id())
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

    /// A host whose records and saved views live in memory alone: tests, captures and runs that
    /// must leave nothing behind.
    ///
    /// # Errors
    /// Rejects empty or control-character identities.
    pub fn in_memory(
        app_id: &str,
        device_id: String,
        workspace_id: &str,
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        Self::with_port(
            None,
            app_id,
            device_id,
            workspace_id,
            author,
            permissions,
            action_context,
        )
    }

    /// A host reading `app_id`'s records and saved views through the port of `documents`,
    /// writing each committed change back through it when its flag says so (a host with no
    /// window, whose routes would publish them), in memory alone without a port.
    ///
    /// # Errors
    /// Rejects empty or control-character identities.
    pub fn with_port(
        documents: Option<(Rc<dyn crate::resource::DocumentPort>, bool)>,
        app_id: &str,
        device_id: String,
        workspace_id: &str,
        author: MemberId,
        permissions: crate::permissions::SharedPermissions,
        action_context: tilo_actions::ActionContext,
    ) -> Result<Self, LaunchError> {
        validate_identity(author.id()).map_err(LaunchError::Restore)?;
        let views = Rc::new(
            InstanceViewStore::new(
                documents.as_ref().map(|(port, _)| Rc::clone(port)),
                app_id,
                &device_id,
                workspace_id,
            )
            .map_err(LaunchError::Restore)?
            .read_by(author.as_person()),
        );
        let owners = match documents {
            Some((port, true)) => ResourceOwners::writing(port),
            Some((port, false)) => ResourceOwners::reading(port),
            None => ResourceOwners::in_memory(),
        }
        .on_device(&device_id);
        Ok(Self {
            package_id: None,
            models: crate::models::Models::default(),
            person: owners.clone(),
            owners,
            views,
            device_id,
            permissions,
            author,
            action_context,
        })
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
    /// A record moved out of this workspace is gone from it: a view still naming it is dropped.
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
            let opening = |records: &ResourceStore| {
                records
                    .begin_opening(&primary)
                    .map_err(LaunchError::Record)
                    .map(|opening| {
                        opening.then(|| LaunchTarget::Record {
                            checkout: primary.clone(),
                            reveal: None,
                        })
                    })
            };
            let opened = if definition.opens_before_resource {
                opening(&records)?
            } else {
                None
            };
            let target = match opened {
                Some(target) => target,
                None => match definition.unsaved_target(&records) {
                    // The workspace has not answered for it yet: the window opens with it on its
                    // way; one it answered meanwhile opens as any other.
                    Err(ResourceOpenError::Arriving) => match opening(&records)? {
                        Some(target) => target,
                        None => definition
                            .unsaved_target(&records)
                            .map_err(LaunchError::Record)?,
                    },
                    target => target.map_err(LaunchError::Record)?,
                },
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
        checkout: DocumentCheckout,
        mut saved: Option<SavedInstanceView>,
        initial_view: Option<ApplicationLocalView>,
    ) -> Result<(ProcessContext, LaunchTarget, RestoreInput), LaunchError> {
        let mut asked_again = false;
        let target = loop {
            match records.try_open_resource(&checkout) {
                Ok(_) => {
                    break LaunchTarget::Record {
                        checkout: checkout.clone(),
                        reveal: None,
                    };
                }
                // The workspace has not answered for it yet: the window opens with it on its way.
                Err(ResourceOpenError::Arriving) => {
                    if records
                        .begin_opening(&checkout)
                        .map_err(LaunchError::Record)?
                    {
                        break LaunchTarget::Record {
                            checkout: checkout.clone(),
                            reveal: None,
                        };
                    }
                    // It answered meanwhile: it opens as any record the workspace answered for.
                    if std::mem::replace(&mut asked_again, true) {
                        return Err(LaunchError::Record(ResourceOpenError::Arriving));
                    }
                }
                Err(ResourceOpenError::MissingResource)
                    if saved.as_ref().is_none_or(|saved| saved.shown().is_some()) =>
                {
                    refuse_copy_while_joining(&LaunchTarget::New)?;
                    saved = None;
                    break LaunchTarget::New;
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
            required: vec![],
            optional: vec![],
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: rejected_factory,
        }
    }

    thread_local! {
        /// The workspace each test profile's launches read and write: one per profile, so a
        /// launch after another finds what that one wrote.
        static WORKSPACES: std::cell::RefCell<
            std::collections::HashMap<PathBuf, Rc<crate::resource::MemoryWorkspace>>,
        > = std::cell::RefCell::default();
    }

    fn environment(root: &Path) -> NativeProcessEnvironment {
        let workspace = WORKSPACES.with_borrow_mut(|workspaces| {
            workspaces
                .entry(root.to_owned())
                .or_insert_with(Rc::default)
                .clone()
        });
        NativeProcessEnvironment {
            xdg_data_home: Some(root.to_owned()),
            documents: Some((workspace, true)),
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
                    participation: Some("author".into()),
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

        phase two_windows_of_one_record_stay_one_document;
        // The store keeps no writer of its own: two windows of one record write through the
        // workspace, and what one wrote the other opens.
        {
            use crate::application_test_support as probe;
            use automerge::ReadDoc as _;
            let temp = tempfile::tempdir().unwrap();
            let definition = definition(probe::APP);
            let workspace = "3f2b1c00-0000-4000-8000-00000000000a";
            let launch = |instance: u64| {
                NativeProcess::open_with(
                    &definition,
                    NativeProcessEnvironment {
                        workspace_id: Some(workspace.into()),
                        participation: Some("author".into()),
                        continuity_instance: Some(instance.to_string()),
                        ..environment(temp.path())
                    },
                    probe::author(),
                )
                .unwrap()
            };
            let mut first = launch(41);
            let mut first_resource = first
                .prepare_launch(&definition, None, probe::runner_inputs())
                .unwrap();
            let first_id = first_resource.context.record().id.clone();
            first_resource
                .context
                .initialize_new_resource(|doc| {
                    use automerge::transaction::Transactable as _;
                    doc.put(automerge::ROOT, "body", "from the first window")
                        .map_err(|error| error.to_string())?;
                    doc.commit();
                    Ok(())
                })
                .unwrap();
            first_resource
                .context
                .records
                .flush()
                .expect("the workspace takes the first window's record");
            drop(first_resource);

            // The second window opens the very record the first wrote, and reads it.
            let mut second = launch(42);
            let second_resource = second
                .prepare_launch(&definition, Some(first_id.clone()), probe::runner_inputs())
                .expect("a second window opens the record the first holds");
            assert_eq!(
                second_resource.context.record().id,
                first_id,
                "two windows of one record are one document"
            );
            assert_eq!(
                second_resource
                    .context
                    .records
                    .read(&first_id, |doc| {
                        doc.get(automerge::ROOT, "body")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .map(str::to_owned)
                    })
                    .unwrap(),
                Some("from the first window".to_owned())
            );
            second_resource
                .context
                .records
                .with(&first_id, |doc| {
                    use automerge::transaction::Transactable as _;
                    doc.put(automerge::ROOT, "body", "from the second window")
                        .unwrap();
                    doc.commit();
                })
                .unwrap();
            second_resource
                .context
                .records
                .flush()
                .expect("the workspace takes the second window's change");
            drop((second_resource, second, first));

            // A window opened again reads what the other wrote: the record stayed one.
            let mut again = launch(41);
            let restored = again
                .prepare_launch(&definition, Some(first_id.clone()), probe::runner_inputs())
                .unwrap();
            assert!(matches!(restored.target, LaunchTarget::Record { .. }));
            assert_eq!(
                restored
                    .context
                    .records
                    .read(&first_id, |doc| {
                        doc.get(automerge::ROOT, "body")
                            .unwrap()
                            .unwrap()
                            .0
                            .as_str()
                            .map(str::to_owned)
                    })
                    .unwrap(),
                Some("from the second window".to_owned())
            );
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
                        participation: Some("author".into()),
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
                        participation: Some("author".into()),
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

        phase workspace_record_windows_share_one_record;
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
            drop(reopened);
            drop(second);
            let mut old = launch(&per_window, 41);
            let old = old
                .prepare_launch(&per_window, None, probe::runner_inputs())
                .unwrap();
            assert!(matches!(old.target, LaunchTarget::Record { .. }));
            assert_eq!(old.context.read_resource(note).unwrap(), Some("first".into()));
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

        phase a_saved_view_names_the_record;
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
            let port: Rc<dyn crate::resource::DocumentPort> =
                Rc::new(crate::resource::MemoryWorkspace::default());
            {
                let mut owners = ResourceOwners::writing(port.clone());
                let records = owners.open(probe::APP, workspace).unwrap();
                records.create_resource(&carried).unwrap();
                records
                    .with(&carried.id, |doc| {
                        doc.put(automerge::ROOT, "body", "arrived").unwrap();
                    })
                    .unwrap();
                records.try_flush().unwrap();
            }
            let profile =
                NativeProfile::open(temp.path(), Some(workspace), probe::APP, Some(port.clone()))
                    .unwrap();
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
            profile.adopt_view(&view, 7, &carried).unwrap();
            let host = |_: &Path| {
                ApplicationOwner::with_port(
                    Some((port.clone(), true)),
                    probe::APP,
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
                    default,
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
                    participation: Some("author".into()),
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

    /// An app started where Tilo does not run cannot keep anything it does: it says so and opens
    /// no window, rather than one whose work is lost when it closes.
    #[test]
    fn a_window_with_no_connection_to_tilo_says_so_and_does_not_open() {
        use crate::application_test_support as probe;
        let temp = tempfile::tempdir().unwrap();
        let refused = NativeProcess::open_with(
            &definition(probe::APP),
            NativeProcessEnvironment {
                documents: None,
                ..environment(temp.path())
            },
            probe::author(),
        );
        let Err(error) = refused else {
            panic!("a window that keeps nothing opened");
        };
        assert!(matches!(error, LaunchError::NotConnected));
        assert!(
            error
                .to_string()
                .starts_with("Tilo is not running on this device")
        );
    }

    /// The workspace's service is still starting when a window opens (a window restored at login):
    /// the window prepares at once with its record on its way rather than waiting for it, takes
    /// it in once the workspace answers, and what is typed in it then is kept.
    #[test]
    fn a_window_whose_workspace_answers_late_opens_at_once_and_takes_its_record_in() {
        use crate::application_test_support as probe;
        use automerge::{ReadDoc, transaction::Transactable};
        let temp = tempfile::tempdir().unwrap();
        let workspace = "3f2b1c00-0000-4000-8000-0000000000e2";
        let definition = definition(probe::APP);
        let launch = || {
            NativeProcess::open_with(
                &definition,
                NativeProcessEnvironment {
                    workspace_id: Some(workspace.into()),
                    participation: Some("author".into()),
                    continuity_instance: Some("72".into()),
                    ..environment(temp.path())
                },
                probe::author(),
            )
            .unwrap()
        };
        let note = |doc: &automerge::AutoCommit| {
            doc.get(automerge::ROOT, "note")
                .unwrap()
                .and_then(|(value, _)| value.into_string().ok())
        };
        let mut host = launch();
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
        let record = created.context.record().id.clone();
        drop(created);
        drop(host);
        let held = WORKSPACES.with_borrow(|workspaces| Rc::clone(&workspaces[temp.path()]));
        held.withhold(record.route());

        let mut host = launch();
        let prepared = host
            .prepare_launch(&definition, None, probe::runner_inputs())
            .expect("a window whose workspace has not answered yet opens");
        assert!(matches!(prepared.target, LaunchTarget::Record { .. }));
        assert_eq!(prepared.context.record().id, record);
        let opening = prepared
            .context
            .resource_opening()
            .expect("the record is on its way");
        assert!(
            !opening.take_in(true).unwrap(),
            "nothing waits for the workspace"
        );
        assert!(prepared.context.read_resource(note).is_err());
        let window = crate::wake::WakeScope::new();
        {
            let _entered = window.enter();
            opening.wake_when_decoded(crate::wake::WakeHandle::new());
        }
        let _ = window.take_unbound();
        assert!(!window.is_pending());

        held.answer(record.route());
        assert!(
            window.is_pending(),
            "the workspace's answer wakes the window"
        );
        assert!(opening.take_in(true).unwrap());
        assert_eq!(
            prepared.context.read_resource(note).unwrap().as_deref(),
            Some("kept")
        );
        let records = prepared.context.records().clone();
        records
            .with(&record, |doc| {
                doc.put(automerge::ROOT, "note", "typed once it arrived")
                    .unwrap();
                doc.commit();
            })
            .expect("the record is open");
        records.flush().expect("the workspace keeps the edit");
        let kept = automerge::AutoCommit::load(
            &held
                .saved(probe::APP, workspace, record.route())
                .expect("the workspace holds the record"),
        )
        .unwrap();
        assert_eq!(note(&kept).as_deref(), Some("typed once it arrived"));
    }

    /// The workspace's service on this device is gone: the window cannot read what it shows, and
    /// says so at once in words a person reads, rather than opening an empty record whose edits
    /// nothing would keep, or waiting for an answer that will not come.
    #[test]
    fn a_window_whose_workspace_is_gone_says_so_at_once_and_does_not_open() {
        use crate::application_test_support as probe;
        struct Down;
        impl crate::resource::DocumentPort for Down {
            fn read(
                &self,
                _: &str,
                _: &str,
                _: Option<Vec<crate::resource::DocumentAsk>>,
            ) -> Result<Vec<(String, crate::resource::DocumentRead)>, String> {
                Err("the workspace service is gone".into())
            }
            fn write(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: Vec<u8>,
                _: bool,
                _: Option<&str>,
            ) -> crate::resource::Written {
                Box::new(|_| Some(Err("the workspace did not answer in time".into())))
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let definition = definition(probe::APP);
        let down: Rc<dyn crate::resource::DocumentPort> = Rc::new(Down);
        let mut host = NativeProcess::open_with(
            &definition,
            NativeProcessEnvironment {
                workspace_id: Some("3f2b1c00-0000-4000-8000-0000000000e1".into()),
                participation: Some("author".into()),
                continuity_instance: Some("71".into()),
                documents: Some((down, false)),
                ..environment(temp.path())
            },
            probe::author(),
        )
        .unwrap();
        let Err(error) = host.prepare_launch(&definition, None, probe::runner_inputs()) else {
            panic!("a window opened on a record its workspace never read");
        };
        assert!(
            matches!(
                error,
                LaunchError::Record(crate::resource::ResourceOpenError::Unreachable(_))
                    | LaunchError::Restore(crate::view::ThawError::Unreachable(_))
            ),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .starts_with("Your workspace didn't answer, so this window can't open"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .ends_with("(the workspace service is gone)"),
            "{error}"
        );
    }

    /// A window that shows before the workspace's record (a tilolang app) reads the record off the
    /// frame on every launch, and takes it in as the workspace holds it.
    #[test]
    fn a_window_opening_its_workspace_record_off_the_frame_takes_it_in_on_every_launch() {
        use crate::application_test_support as probe;
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
                    participation: Some("author".into()),
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
            let prepared = host
                .prepare_launch(&definition, None, probe::runner_inputs())
                .unwrap();
            assert!(matches!(prepared.target, LaunchTarget::Record { .. }));
            let opening = prepared
                .context
                .resource_opening()
                .expect("the record the workspace holds opens off the frame");
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
        }
    }

    tilo_test_env::scenario! {
        fn a_native_host_keeps_authority_and_identity_explicit;

        phase native_host_action_authority_is_current_and_scoped_to_the_app;
        {
            use crate::permissions::{ActionKind, Answer, TreePath};
            use collab_types::action_authority::AppActionAuthorityProjection;
            use std::sync::{Arc, Mutex};
            let temp = tempfile::tempdir().unwrap();
            let workspace = "3f2b1c00-0000-4000-8000-000000000001";
            let app = "so.tilo.store";
            let author = MemberId::person("person-a");
            let original = AppActionAuthorityProjection::project(
                &tilo_action_policy::WorkspacePolicy::new(workspace, author.id()),
                app,
                author.id(),
                false,
            );
            let bytes = Arc::new(Mutex::new(Some(serde_json::to_vec(&original).unwrap())));
            let source: tilo_process_services::action_authority::AuthoritySource = {
                let bytes = bytes.clone();
                Arc::new(move || Ok(bytes.lock().unwrap().clone()))
            };
            let (watch, changed) = manual_authority_watch();
            let environment = NativeProcessEnvironment {
                workspace_id: Some(workspace.into()),
                participation: Some("read-only".into()),
                action_authority: Some(HostAuthority {
                    read: source.clone(),
                    watch,
                }),
                ..environment(temp.path())
            };
            let publish = |value: Option<Vec<u8>>| {
                *bytes.lock().unwrap() = value;
                changed();
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
            let allowed = permissions.directory_observation().unwrap();
            let mut revoked = original.clone();
            revoked.policy.owner = None;
            publish(Some(serde_json::to_vec(&revoked).unwrap()));
            assert_ne!(permissions.directory_observation().unwrap(), allowed);
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
            {
                let mut invalid = original.clone();
                invalid.schema_version = 2;
                publish(Some(serde_json::to_vec(&invalid).unwrap()));
                assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
            }
            for (field, value) in [
                ("workspace", "foreign-workspace"),
                ("app_id", "so.tilo.other"),
                ("person", "person-b"),
            ] {
                let mut invalid = serde_json::to_value(&original).unwrap();
                invalid[field] = value.into();
                publish(Some(serde_json::to_vec(&invalid).unwrap()));
                assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
                assert!(permissions.directory_observation().is_err());
            }
            for invalid in [None, Some(b"invalid".to_vec())] {
                publish(invalid);
                assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Deny);
                assert!(permissions.directory_observation().is_err());
            }
            publish(Some(serde_json::to_vec(&original).unwrap()));
            assert_eq!(permissions.answer(&own, ActionKind::Outside), Answer::Allow);
            assert_eq!(permissions.directory_observation().unwrap(), allowed);
        }

        phase a_host_authority_whose_changes_cannot_be_heard_is_read_at_each_decision;
        {
            use crate::permissions::{ActionKind, Answer, TreePath};
            use collab_types::action_authority::AppActionAuthorityProjection;
            use std::sync::{Arc, Mutex};
            let temp = tempfile::tempdir().unwrap();
            let workspace = "3f2b1c00-0000-4000-8000-000000000001";
            let app = "so.tilo.store";
            let author = MemberId::person("person-a");
            let original = AppActionAuthorityProjection::project(
                &tilo_action_policy::WorkspacePolicy::new(workspace, author.id()),
                app,
                author.id(),
                false,
            );
            let bytes = Arc::new(Mutex::new(Some(serde_json::to_vec(&original).unwrap())));
            let source: tilo_process_services::action_authority::AuthoritySource = {
                let bytes = bytes.clone();
                Arc::new(move || Ok(bytes.lock().unwrap().clone()))
            };
            let watch: AuthorityWatcher =
                Arc::new(|_| Err("Too many open files (os error 24)".to_owned()));
            let environment = NativeProcessEnvironment {
                workspace_id: Some(workspace.into()),
                participation: Some("read-only".into()),
                action_authority: Some(HostAuthority {
                    read: source,
                    watch,
                }),
                ..environment(temp.path())
            };
            let permissions = environment.resolve(app, &author).unwrap().permissions;
            let own = TreePath::root().app(app).action("buy");
            assert_eq!(
                permissions.answer(&own, ActionKind::Outside),
                Answer::Allow,
                "the person's authority holds without a watch"
            );
            let mut revoked = original.clone();
            revoked.policy.owner = None;
            *bytes.lock().unwrap() = Some(serde_json::to_vec(&revoked).unwrap());
            assert_eq!(
                permissions.answer(&own, ActionKind::Outside),
                Answer::Deny,
                "and what the host holds now is what decides"
            );
        }

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
                    first.host.permissions.borrow().role(),
                    Some(crate::permissions::Role::Owner)
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
                let make = |participation: Option<&str>| NativeProcessEnvironment {
                    workspace_id: Some(workspace.into()),
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
                        .resolve("so.tilo.notes", &MemberId::person("person-a"))
                        .unwrap();
                    assert!(resolved.workspace_scoped);
                    assert_eq!(resolved.workspace_id, workspace);
                    assert_eq!(resolved.instance_id, 42);
                    assert_eq!(resolved.permissions.role(), Some(role));
                }
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
