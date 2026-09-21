//! Explicit process-host services shared by standalone application runners.
//! Apps supply their definition and migration; this owner supplies storage,
//! identity, authority and local presentation. No ambient profile discovery.
use super::{
    ApplicationDefinition, ApplicationLocalView, ComponentRunnerInputs, HostContext, InstanceId,
    InstanceViewStore, LaunchError, LaunchTarget, PermissionContext, PreparedComponentInputs,
    RecordCheckout, RecordId, RecordOpenError, RecordStore, RestoreInput, SavedInstanceView,
    UserId, View, validate_identity,
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
        let mut host = ApplicationHost::new(
            resolved.profile_root,
            resolved.device_id,
            &resolved.workspace_id,
            author,
            Rc::new(RefCell::new(resolved.permissions)),
            tilo_actions::ActionContext::default(),
        )?;
        host.durable_app_id = Some(resolved.durable_app_id);
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
    durable_app_id: Option<String>,
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
    durable_app_id: String,
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
            durable_app_id: string("TILO_DURABLE_APP_ID"),
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
        if !super::lookup_compatible_first_party_app_ids(&package_id, app_id) {
            return Err(LaunchError::BindingMismatch);
        }
        let durable_app_id = self
            .durable_app_id
            .as_deref()
            .unwrap_or(&package_id)
            .to_owned();
        if !super::lookup_compatible_first_party_app_ids(&durable_app_id, app_id) {
            return Err(LaunchError::BindingMismatch);
        }
        let profile_root = self.persistent_profile_root()?;
        let device_id = persistent_device_id(&profile_root)?;
        let workspace = self
            .workspace_id
            .as_deref()
            .or(self.quick_workspace_id.as_deref());
        let (workspace_id, workspace_scoped) = match workspace {
            Some(value) => {
                let id = uuid::Uuid::parse_str(value).map_err(|_| {
                    LaunchError::Operation("invalid launcher workspace identity".into())
                })?;
                (id.to_string(), true)
            }
            None => (PERSONAL_WORKSPACE.to_owned(), false),
        };
        let permissions =
            permissions_for_participation(author, workspace_scoped, self.participation.as_deref())?;
        let instance_id = self.continuity_instance.map_or_else(
            || Ok(stable_instance_id(&durable_app_id)),
            |value| {
                value.parse::<u64>().map_err(|_| {
                    LaunchError::Operation("invalid launcher continuation identity".into())
                })
            },
        )?;
        Ok(ResolvedNativeProcessHost {
            durable_app_id,
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
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in app_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
                .and_then(|()| atomic_file::sync_directory(profile_root))
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
    /// Exact launcher-authenticated record route for native processes.
    /// Explicit/capture hosts select it from the definition passed to `records_for`.
    durable_app_id: Option<String>,
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
            profile_root.clone(),
            device_id,
            workspace_id,
            author,
            permissions,
            action_context,
        )?;
        host.owners = RecordOwners::with_profile_lifetime(profile_root, lifetime.clone());
        let views = Rc::try_unwrap(host.views).map_err(|_| LaunchError::BindingMismatch)?;
        host.views = Rc::new(views.with_profile_lifetime(lifetime));
        Ok(host)
    }

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
        validate_identity(&author.0).map_err(LaunchError::Restore)?;
        let views = Rc::new(
            InstanceViewStore::new(profile_root.clone(), &device_id, workspace_id)
                .map_err(LaunchError::Restore)?,
        );
        Ok(Self {
            durable_app_id: None,
            owners: RecordOwners::new(profile_root),
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
        let package_id = self.durable_app_id.as_deref().unwrap_or(definition.app_id);
        if !super::lookup_compatible_first_party_app_ids(package_id, definition.app_id) {
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
        let package_id = self.durable_app_id.as_deref().unwrap_or(definition.app_id);
        if !super::lookup_compatible_first_party_app_ids(package_id, definition.app_id)
            || saved.as_ref().is_some_and(|saved| {
                saved.app_id != package_id
                    || saved.record_id != checkout.id.as_str()
                    || saved.record_kind != checkout.kind.as_str()
            })
        {
            return Err(LaunchError::BindingMismatch);
        }
        Ok(saved)
    }

    /// Open the host's stable default record, or reserve it for the definition's
    /// New factory. Restore presentation from this exact device/workspace/instance.
    /// A saved view pointing at a missing record is an error, never a blank replacement.
    /// `initial_view` is used only when this instance has no saved presentation.
    /// # Errors
    /// Rejects invalid/foreign views, contested writers, corrupt records and denied authority.
    pub fn prepare_default(
        &mut self,
        definition: &ApplicationDefinition,
        instance_id: InstanceId,
        checkout: RecordCheckout,
        initial_view: Option<ApplicationLocalView>,
        runner_inputs: ComponentRunnerInputs,
    ) -> Result<PreparedComponentInputs, LaunchError> {
        let saved = self.saved_view_for(definition, instance_id, &checkout)?;
        let records = self.records_for(definition)?;
        let target = match records.try_open_record(&checkout) {
            Ok(_) => LaunchTarget::Record {
                checkout: checkout.clone(),
                reveal: None,
            },
            Err(RecordOpenError::MissingRecord) if saved.is_none() => LaunchTarget::New,
            Err(error) => return Err(LaunchError::Record(error)),
        };
        let context = if matches!(target, LaunchTarget::New) {
            HostContext::prepare_new_record(
                definition,
                records,
                self.views.clone(),
                self.device_id.clone(),
                instance_id,
                self.permissions.clone(),
                self.author.clone(),
                self.action_context.clone(),
                checkout.id,
            )?
        } else {
            HostContext::prepare(
                definition,
                records,
                self.views.clone(),
                self.device_id.clone(),
                instance_id,
                self.permissions.clone(),
                self.author.clone(),
                self.action_context.clone(),
                &target,
            )?
        };
        let restore = saved
            .or_else(|| {
                initial_view.map(|application| context.envelope(View::default(), Some(application)))
            })
            .map_or(RestoreInput::None, RestoreInput::Local);
        Ok(PreparedComponentInputs {
            context,
            target,
            restore,
            runner_inputs,
        })
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
    fn standalone_native_host_has_durable_device_and_app_unique_view_identity() {
        let temp = tempfile::tempdir().unwrap();
        let author = UserId("person-a".into());
        let first = NativeProcessHost::open_with(
            &definition("dev.tilo.one"),
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
            &definition("dev.tilo.one"),
            environment(temp.path()),
            author.clone(),
        )
        .unwrap();
        let other = NativeProcessHost::open_with(
            &definition("dev.tilo.two"),
            environment(temp.path()),
            author,
        )
        .unwrap();
        assert_eq!(reopened.host.device_id, device);
        assert_eq!(other.host.device_id, device);
        assert_eq!(reopened.instance_id(), stable_instance_id("dev.tilo.one"));
        assert_ne!(reopened.instance_id(), other.instance_id());
    }

    #[test]
    fn renamed_native_launcher_keeps_exact_durable_package_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let author = UserId("person-a".into());
        let mut legacy = environment(temp.path());
        legacy.package_id = Some("so.tilo.notes".into());
        legacy.durable_app_id = Some("dev.qos.notes".into());
        let resolved = legacy.resolve("so.tilo.notes", &author).unwrap();
        assert_eq!(resolved.durable_app_id, "dev.qos.notes");
        assert_eq!(resolved.instance_id, stable_instance_id("dev.qos.notes"));

        let mut legacy = environment(temp.path());
        legacy.package_id = Some("so.tilo.notes".into());
        legacy.durable_app_id = Some("dev.qos.notes".into());
        let mut native =
            NativeProcessHost::open_with(&definition("so.tilo.notes"), legacy, author).unwrap();
        let current = definition("so.tilo.notes");
        let records = native.host_mut().records_for(&current).unwrap();
        assert_eq!(records.canonical_namespace().unwrap().1, "dev.qos.notes");

        let checkout = RecordCheckout::new(RecordId::for_tab(9), current.record_kind.clone());
        let saved = SavedInstanceView {
            version: 2,
            app_id: "dev.qos.notes".into(),
            device_id: native.host.device_id.clone(),
            workspace_id: PERSONAL_WORKSPACE.into(),
            instance_id: native.instance_id,
            record_id: checkout.id.as_str().into(),
            record_kind: checkout.kind.as_str().into(),
            view: View::default(),
            application: None,
        };
        native.host.views.save(&saved).unwrap();
        assert!(
            native
                .host
                .saved_view_for(&current, native.instance_id, &checkout)
                .unwrap()
                .is_some()
        );
        let mut aliased = saved;
        aliased.app_id = current.app_id.into();
        native.host.views.save(&aliased).unwrap();
        assert!(matches!(
            native
                .host
                .saved_view_for(&current, native.instance_id, &checkout),
            Err(LaunchError::BindingMismatch)
        ));
    }

    #[test]
    fn signed_package_and_exact_durable_route_are_validated_independently() {
        let temp = tempfile::tempdir().unwrap();
        let author = UserId("person-a".into());
        for package in ["dev.qos.notes", "dev.tilo.notes", "so.tilo.notes"] {
            for durable in ["dev.qos.notes", "dev.tilo.notes", "so.tilo.notes"] {
                let mut environment = environment(temp.path());
                environment.package_id = Some(package.into());
                environment.durable_app_id = Some(durable.into());
                let resolved = environment
                    .resolve("so.tilo.notes", &author)
                    .expect("each explicit deployed package and route pair remains available");
                assert_eq!(resolved.durable_app_id, durable);
                assert_eq!(resolved.instance_id, stable_instance_id(durable));
            }
        }

        let mut foreign_package = environment(temp.path());
        foreign_package.package_id = Some("so.tilo.photos".into());
        foreign_package.durable_app_id = Some("dev.qos.notes".into());
        assert!(matches!(
            foreign_package.resolve("so.tilo.notes", &author),
            Err(LaunchError::BindingMismatch)
        ));

        let mut foreign_durable = environment(temp.path());
        foreign_durable.package_id = Some("so.tilo.notes".into());
        foreign_durable.durable_app_id = Some("dev.qos.photos".into());
        assert!(matches!(
            foreign_durable.resolve("so.tilo.notes", &author),
            Err(LaunchError::BindingMismatch)
        ));
    }

    #[test]
    fn workspace_participation_is_host_authority_and_missing_policy_is_viewer() {
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
                .resolve("dev.tilo.notes", &UserId("person-a".into()))
                .unwrap();
            assert!(resolved.workspace_scoped);
            assert_eq!(resolved.workspace_id, room);
            assert_eq!(resolved.instance_id, 42);
            assert_eq!(resolved.permissions.role(), Some(role));
        }
    }

    #[test]
    fn malformed_native_host_inputs_are_refused_without_fallback_storage() {
        let temp = tempfile::tempdir().unwrap();
        let author = UserId("person-a".into());
        let mut mismatched = environment(temp.path());
        mismatched.package_id = Some("dev.tilo.other".into());
        assert!(matches!(
            mismatched.resolve("dev.tilo.notes", &author),
            Err(LaunchError::BindingMismatch)
        ));
        let relative = NativeProcessEnvironment {
            xdg_data_home: Some(PathBuf::from("relative")),
            ..NativeProcessEnvironment::default()
        };
        assert!(relative.resolve("dev.tilo.notes", &author).is_err());
        assert!(
            NativeProcessEnvironment::default()
                .resolve("dev.tilo.notes", &author)
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
            assert!(invalid.resolve("dev.tilo.notes", &author).is_err());
        }
        std::fs::write(temp.path().join(DEVICE_ID_FILE), b"damaged\n").unwrap();
        assert!(
            environment(temp.path())
                .resolve("dev.tilo.notes", &author)
                .is_err()
        );
        assert_eq!(
            std::fs::read(temp.path().join(DEVICE_ID_FILE)).unwrap(),
            b"damaged\n"
        );
    }
}
