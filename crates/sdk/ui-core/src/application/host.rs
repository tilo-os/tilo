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
const DEVICE_ID_FILE: &str = ".tilo-device-id";

/// The host inputs a normal application process receives from the trusted
/// launcher. Storage and presentation identity are resolved once, before an
/// application factory is allowed to open a record.
pub struct NativeProcessHost {
    host: ApplicationHost,
    instance_id: InstanceId,
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
        let resolved = environment.resolve(definition.app_id, &author)?;
        let mut host = ApplicationHost::with_store(
            Rc::new(app_state::FsStore::new(resolved.profile_root)),
            resolved.device_id,
            &resolved.workspace_id,
            author,
            Rc::new(RefCell::new(resolved.permissions)),
            tilo_actions::ActionContext::default(),
        )?;
        host.package_id = Some(resolved.package_id);
        Ok(Self {
            host,
            instance_id: resolved.instance_id,
            workspace_scoped: resolved.workspace_scoped,
        })
    }

    pub fn host_mut(&mut self) -> &mut ApplicationHost {
        &mut self.host
    }

    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    pub fn workspace_scoped(&self) -> bool {
        self.workspace_scoped
    }
}

#[derive(Default)]
struct NativeProcessEnvironment {
    package_id: Option<String>,
    quick_app_id: Option<String>,
    workspace_id: Option<String>,
    quick_workspace_id: Option<String>,
    participation: Option<String>,
    continuity_instance: Option<String>,
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
            quick_app_id: string("QUICK_APP_ID"),
            workspace_id: string("TILO_WORKSPACE_ID"),
            quick_workspace_id: string("QUICK_CLUSTER_ID"),
            participation: string("TILO_PARTICIPATION"),
            continuity_instance: string("TILO_CONTINUITY_INSTANCE"),
            xdg_data_home: path("XDG_DATA_HOME"),
            local_app_data: path("LOCALAPPDATA"),
            user_profile: path("USERPROFILE"),
            home: path("HOME"),
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
        let profile_root = self.persistent_profile_root()?;
        let device_id = persistent_device_id(&profile_root)?;
        let workspace = self
            .workspace_id
            .as_deref()
            .or(self.quick_workspace_id.as_deref());
        let (workspace_id, workspace_scoped) = launcher_workspace(workspace)?;
        let permissions =
            permissions_for_participation(author, workspace_scoped, self.participation.as_deref())?;
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
        let room = environment
            .workspace_id
            .as_deref()
            .or(environment.quick_workspace_id.as_deref());
        Self::open(&root, room)
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
    /// now showing `record`: what a window carried into this room reopens to.
    ///
    /// # Errors
    /// An invalid record identity or a store that refused the write.
    pub fn adopt_view(
        &self,
        view: &SavedInstanceView,
        instance: InstanceId,
        record: &RecordCheckout,
    ) -> Result<(), LaunchError> {
        let adopted = SavedInstanceView {
            device_id: self.device_id.clone(),
            workspace_id: self.workspace_id.clone(),
            instance_id: instance,
            record_id: record.id.as_str().to_owned(),
            record_kind: record.kind.as_str().to_owned(),
            ..view.clone()
        };
        self.views.save(&adopted).map_err(LaunchError::Restore)
    }
}

fn permissions_for_participation(
    author: &UserId,
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
    Ok(PermissionContext::from_capabilities(
        crate::permissions::Actor::person(Some(role), author.0.clone()),
        Vec::new(),
    ))
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
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
    device_id: String,
    permissions: Rc<RefCell<PermissionContext>>,
    author: UserId,
    action_context: tilo_actions::ActionContext,
}

impl ApplicationHost {
    /// Keep a host-owned profile resource (such as a temporary capture directory)
    /// alive through record writers and failed record or presentation save retries.
    /// The resource must own the supplied profile root for its entire lifetime.
    /// # Errors
    /// Rejects the same explicit namespace and identity errors as `new`.
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
    /// # Errors
    /// Rejects invalid identity inputs. Nothing is read or written.
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
            owners: RecordOwners::with_store(store),
            views,
            device_id,
            permissions,
            author,
            action_context,
        })
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
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id);
        if package_id != definition.app_id {
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
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id);
        if package_id != definition.app_id
            || saved.as_ref().is_some_and(|saved| {
                saved.app_id != package_id
                    || saved.record_id != checkout.id.as_str()
                    || !checkout.kind.names(&saved.record_kind)
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
        let package_id = self.package_id.as_deref().unwrap_or(definition.app_id);
        let id = RecordId::parse(&saved.record_id).ok_or(LaunchError::BindingMismatch)?;
        if saved.app_id != package_id
            || !definition.record_kind.names(&saved.record_kind)
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
            let target = definition.unsaved_target(&records);
            let context = HostContext::prepare(
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
            let restore = initial_view.map_or(RestoreInput::None, |application| {
                RestoreInput::Local(context.envelope(View::default(), Some(application)))
            });
            return Ok(PreparedComponentInputs {
                context,
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
            context,
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
                saved.record_id == checkout.id.as_str() && checkout.kind.names(&saved.record_kind)
            });
        let (context, target, restore) =
            self.prepare_checkout(definition, records, instance_id, checkout, saved, None)?;
        Ok(PreparedComponentInputs {
            context,
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
        let context = if matches!(target, LaunchTarget::New) {
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
            app_id,
            title: "Host probe",
            icon: crate::widget::IconDescriptor::Glyph("H"),
            record_kind: crate::record::RecordKind::new("host.probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
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
    fn native_host_identity_authority_and_inputs_are_explicit() {
        // Case `standalone_native_host_has_durable_device_and_app_unique_view_identity`.
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

        // Case `workspace_participation_is_host_authority_and_missing_policy_is_viewer`.
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

        // Case `malformed_native_host_inputs_are_refused_without_fallback_storage`.
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
        profile.adopt_view(&view, 7, &carried).unwrap();
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

        crate::record::transfer::retire(
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
