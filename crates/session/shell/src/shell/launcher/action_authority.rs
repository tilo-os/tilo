//! Project accepted action policy and interrupt withdrawn sandbox authority.
//! This observer never creates or expands grants.
use super::{InstalledApp, LaunchCommand, lock, runtime_dir};
use crate::shell::state::RoomId;
use collab_types::caps::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
use collab_types::caps::{Actor, CapOverride, Scope, Subject};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod sandbox_revocation;

#[derive(Clone)]
struct RoomPolicy {
    local_id: String,
    owner: Option<String>,
    capabilities: Vec<CapOverride>,
    runtime: crate::shell::native_workspace_owner::NativeStateWitness,
    authority: crate::shell::native_workspace_owner::NativeAuthorityStorage,
}

type Binding = (RoomId, String, String);
struct BindingState {
    projection: AppActionAuthorityProjection,
    borrowers: u64,
}

#[derive(Default)]
struct Published {
    rooms: BTreeMap<RoomId, RoomPolicy>,
    bindings: BTreeMap<Binding, BindingState>,
    next_refresh: Option<Instant>,
    #[cfg(target_os = "linux")]
    sandboxes: BTreeMap<u32, sandbox_revocation::Sandbox>,
}
static PUBLISHED: LazyLock<Mutex<Published>> = LazyLock::new(|| Mutex::new(Published::default()));

/// Capture policy for publication only after the normal state writer accepts
/// this exact tree. Superseded/refused state writes never call `publish`.
pub(crate) struct Export(BTreeMap<RoomId, RoomPolicy>);

impl Export {
    pub(crate) fn of(tree: &crate::shell::state::WorkspaceTree) -> Self {
        Self(
            tree.desktop
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace.room,
                        RoomPolicy {
                            local_id: workspace.id.raw().to_string(),
                            owner: workspace.owner.clone(),
                            capabilities: workspace.capabilities.clone(),
                            runtime: tree.native_runtime.authority_witness(),
                            authority: tree.native_authority.clone(),
                        },
                    )
                })
                .collect(),
        )
    }

    pub(crate) fn publish(self) {
        let mut published = lock(&PUBLISHED);
        published.rooms = self.0;
        #[cfg(target_os = "linux")]
        sandbox_revocation::reconcile(&mut published);
        let bindings: Vec<_> = published.bindings.keys().cloned().collect();
        for binding in bindings {
            if let Err(error) = write_binding(&mut published, &binding, false) {
                // Remove the previous grant if replacement fails; retaining
                // stale accepted policy would turn a revoke into a grant.
                let _ = std::fs::remove_file(binding_path(&binding));

                tracing::warn!(room = %binding.0, package_id = %binding.1, %error, "host action authority update failed closed");
            }
        }
        drop(published);
    }

    pub(crate) fn submit(self) {
        crate::shell::durable_writer::global()
            .submit(authority_root().join("publication"), move || self.publish());
    }
}

/// Host-only backend admission over the accepted room policy. No projection
/// bytes or caller-supplied roles participate. The real tree and writer stay
/// borrowed until the typed operation finishes, serializing revocation.
pub(crate) fn with_ordinary_person_admin<T>(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: RoomId,
    person: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_ordinary_room_current(tree, room, |workspace| {
        let now = tilo_clock::unix_now_ms() / 1000;
        let actor = workspace.actor_for_person_at(&person.to_owned(), now);
        if !workspace.allows_at(
            &actor,
            &collab_types::caps::ResourceRef::App("so.tilo.settings".into()),
            collab_types::caps::Verb::Admin,
            now,
        ) {
            return Err("The current owner policy does not permit display changes.".into());
        }
        operation()
    })
}

/// Borrow accepted ordinary-room custody without assigning a role to the
/// caller. Individual action contracts still apply their exact permissions.
pub(crate) fn with_ordinary_room_current<T>(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: RoomId,
    operation: impl FnOnce(&crate::shell::state::Workspace) -> Result<T, String>,
) -> Result<T, String> {
    let tree = tree.read();
    if tree.requires_native_owner(room) {
        return Err("This room requires its current native owner.".into());
    }
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .ok_or("The admitted Settings room is unavailable.")?;
    let published = lock(&PUBLISHED);
    let policy = published
        .rooms
        .get(&room)
        .ok_or("The accepted Settings room policy is unavailable.")?;
    if workspace.owner != policy.owner || workspace.capabilities != policy.capabilities {
        return Err("The accepted Settings room policy has changed.".into());
    }
    let result = policy.runtime.with_current_ordinary(
        &policy.authority,
        &tree.native_authority,
        room,
        || operation(workspace),
    );
    drop(published);
    drop(tree);
    result
}

fn authority_root() -> PathBuf {
    runtime_dir().join("tilo").join("action-authority")
}

fn binding_path(binding: &Binding) -> PathBuf {
    let identity = format!("{}\0{}", binding.1, binding.2);
    authority_root()
        .join(binding.0.to_string())
        .join(blake3::hash(identity.as_bytes()).to_hex().as_str())
        .join("current.json")
}

fn projection(published: &Published, binding: &Binding) -> AppActionAuthorityProjection {
    let room = binding.0.to_string();
    let person_actor = Actor::person(None, &binding.2).in_room(&room);
    let policy = published.rooms.get(&binding.0).filter(|policy| {
        policy
            .runtime
            .authority_is_current(&policy.authority, binding.0)
    });
    let capabilities = policy
        .into_iter()
        .flat_map(|policy| &policy.capabilities)
        .filter(|capability| {
            matches!(capability.subject, Subject::Role(_))
                || capability.subject.matches_actor(&person_actor)
        })
        .filter_map(|capability| {
            let mut current = capability.clone();
            match &current.scope {
                Scope::Workspace => {}
                // Membership is owned by this accepted workspace vector;
                // normalize its legacy local-handle spelling to the room.
                Scope::Room(scope)
                    if scope == &room || policy.is_some_and(|policy| scope == &policy.local_id) =>
                {
                    current.scope = Scope::Room(room.clone());
                }
                Scope::App(app) | Scope::Action { app, .. } if app == &binding.1 => {}
                _ => return None,
            }
            Some(current)
        })
        .collect();
    let issued_at_unix_ms = tilo_clock::unix_now_ms();
    AppActionAuthorityProjection {
        schema_version: 1,
        room,
        app_id: binding.1.clone(),
        person: binding.2.clone(),
        owner: policy.and_then(|policy| policy.owner.clone()),
        capabilities,
        issued_at_unix_ms,
        valid_until_unix_ms: issued_at_unix_ms.saturating_add(3_000),
    }
}

fn write_binding(
    published: &mut Published,
    binding: &Binding,
    renew: bool,
) -> std::io::Result<PathBuf> {
    let projection = projection(published, binding);
    projection.validate().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid host action policy projection",
        )
    })?;
    let bytes = serde_json::to_vec(&projection).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid host action policy projection",
        )
    })?;
    if bytes.len() > MAX_PROJECTION_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "host action policy exceeds its byte bound",
        ));
    }
    let path = binding_path(binding);
    if renew
        || published
            .bindings
            .get(binding)
            .is_none_or(|state| !state.projection.same_policy(&projection))
    {
        tilo_files::native::atomic::write_private(&path, &bytes)?;
        match published.bindings.get_mut(binding) {
            Some(state) => state.projection = projection,
            None => {
                published.bindings.insert(
                    binding.clone(),
                    BindingState {
                        projection,
                        borrowers: 0,
                    },
                );
            }
        }
    }
    Ok(path)
}

pub(super) fn attach(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
) -> std::io::Result<Lease> {
    let person = crate::shell::state::local_user_id_for_room(&room);
    let binding = (room, app.package_id.clone(), person);
    let mut published = lock(&PUBLISHED);
    let path = write_binding(&mut published, &binding, true)?;
    if let Some(state) = published.bindings.get_mut(&binding) {
        state.borrowers = state.borrowers.saturating_add(1);
    }
    published.next_refresh = Instant::now().checked_add(Duration::from_secs(1));
    drop(published);
    command.args.push("--action-authority".into());
    command.args.push(path.display().to_string());
    Ok(Lease {
        binding,
        capabilities: Vec::new(),
        pid: None,
        capabilities_file: None,
    })
}

#[cfg(test)]
pub(crate) fn bind_for_test(
    room: RoomId,
    app: &str,
    person: &str,
) -> std::io::Result<(Lease, PathBuf)> {
    let binding = (room, app.to_owned(), person.to_owned());
    let mut published = lock(&PUBLISHED);
    let path = write_binding(&mut published, &binding, true)?;
    if let Some(state) = published.bindings.get_mut(&binding) {
        state.borrowers = state.borrowers.saturating_add(1);
    }
    published.next_refresh = Instant::now().checked_add(Duration::from_secs(1));
    drop(published);
    Ok((
        Lease {
            binding,
            capabilities: Vec::new(),
            pid: None,
            capabilities_file: None,
        },
        path,
    ))
}

/// A missing projection does not grant Admin or refuse a manifest-only launch.
pub(super) fn attach_optional(
    app: &InstalledApp,
    room: RoomId,
    command: &mut LaunchCommand,
) -> Option<Lease> {
    match attach(app, room, command) {
        Ok(lease) => Some(lease),
        Err(error) => {
            tracing::warn!(%room, package_id = %app.package_id, %error, "could not stage host action authority; Admin actions remain unavailable");
            None
        }
    }
}

/// Keeps the lease tied to the existing launch supervisor, including a held
/// launch, rather than an unrelated timer or a package-name process scan.
pub(crate) struct Lease {
    binding: Binding,
    capabilities: Vec<CapOverride>,
    pid: Option<u32>,
    capabilities_file: Option<tempfile::NamedTempFile>,
}
impl Lease {
    pub(super) fn with_sandbox_capabilities(
        mut self,
        capabilities: Vec<CapOverride>,
        file: Option<tempfile::NamedTempFile>,
    ) -> Self {
        self.capabilities = capabilities;
        self.capabilities_file = file;
        self
    }

    #[cfg(target_os = "linux")]
    pub(super) fn register_sandbox(&mut self, pid: u32) -> std::io::Result<()> {
        self.register_in(pid, &PUBLISHED, crate::shell::durable_writer::global())
    }

    #[cfg(target_os = "linux")]
    fn register_in(
        &mut self,
        pid: u32,
        accepted: &Mutex<Published>,
        writer: &crate::shell::durable_writer::DurableWriter,
    ) -> std::io::Result<()> {
        let sandbox =
            sandbox_revocation::Sandbox::capture(pid, self.binding.clone(), &self.capabilities)?;
        // Opening a document queues its grant on the existing state writer.
        // Unless the accepted policy already holds every captured grant, wait
        // for that publication on this launch worker, before opening the
        // child gate; waiting when it does would only wait for unrelated
        // writes. A revocation still queued is enforced on its publication
        // (`sandbox_revocation::reconcile`), as one queued after the launch.
        // A refused/superseded write still cannot authorize it.
        if !sandbox.authorized(&lock(accepted)) && !writer.flush_timeout(Duration::from_secs(10)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "filesystem authority publication did not complete",
            ));
        }
        let mut published = lock(accepted);
        if !sandbox.authorized(&published) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "filesystem authority changed while the application launch was pending",
            ));
        }
        published.sandboxes.insert(pid, sandbox);
        drop(published);
        self.pid = Some(pid);
        Ok(())
    }
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionAuthorityLease")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut published = lock(&PUBLISHED);
        #[cfg(target_os = "linux")]
        if let Some(pid) = self.pid {
            published.sandboxes.remove(&pid);
        }
        if let Some(state) = published.bindings.get_mut(&self.binding) {
            state.borrowers = state.borrowers.saturating_sub(1);
            if state.borrowers == 0 {
                published.bindings.remove(&self.binding);
                let _ = std::fs::remove_file(binding_path(&self.binding));
            }
        }
        if published.bindings.is_empty() {
            published.next_refresh = None;
        }
    }
}

pub(crate) fn next_deadline() -> Option<Instant> {
    lock(&PUBLISHED).next_refresh
}

/// Renew only while the existing launch supervisor owns a live/held lease.
/// File work runs on the normal durable writer; no frame or GPU is requested.
pub(crate) fn tick(now: Instant) {
    let mut published = lock(&PUBLISHED);
    if published.next_refresh.is_none_or(|deadline| now < deadline) {
        return;
    }
    published.next_refresh = now.checked_add(Duration::from_secs(1));
    drop(published);
    crate::shell::durable_writer::global().submit(authority_root().join("heartbeat"), || {
        let mut published = lock(&PUBLISHED);
        #[cfg(target_os = "linux")]
        sandbox_revocation::reconcile(&mut published);
        let bindings: Vec<_> = published.bindings.keys().cloned().collect();
        for binding in bindings {
            if let Err(error) = write_binding(&mut published, &binding, true) {
                let _ = std::fs::remove_file(binding_path(&binding));
                tracing::warn!(room = %binding.0, package_id = %binding.1, %error, "host action authority renewal failed closed");
            }
        }
        drop(published);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{Principal, Verb};

    #[test]
    fn ordinary_display_backend_borrows_real_owner_policy_and_rejects_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        tree.desktop.workspaces[0].set_owner("person-a");
        tree.persist_path = Some(path);
        let tree = tree.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        Export::of(&tree.read()).publish();
        let source = settings_store::HostDocument::new(serde_json::Map::default(), |_| Ok(()));
        let observed: settings_store::hardware::DisplayHardwareSnapshot = serde_json::from_value(serde_json::json!({
            "outputs":[{"descriptor":{"connector":"eDP-1","modes":[{"width":2944,"height":1840,"refresh_mhz":59999},{"width":2944,"height":1840,"refresh_mhz":89999}],"hdr":false,"vrr":false,"color_profiles":[],"monitor":null},"current_mode":{"width":2944,"height":1840,"refresh_mhz":59999},"scale":2.0,"builtin":true,"sink_hdr":false,"hdr_unavailable_reason":null,"sink_vrr":false,"vrr_unavailable_reason":null,"color_profile_labels":{}}],
            "layout":[{"connector":"eDP-1","x":0,"y":0,"width":1472,"height":920}]
        })).unwrap();
        let candidate: settings_store::hardware::DisplayConfig = serde_json::from_value(serde_json::json!({"outputs":{"eDP-1":{"mode":{"width":2944,"height":1840,"refresh_mhz":89999},"scale":2.0}}})).unwrap();
        let command = settings_store::display_control::DisplayCommand::Apply {
            observed_set: settings_store::hardware::DisplaySet::of_descriptors(&[observed.outputs
                [0]
            .descriptor
            .clone()]),
            candidate,
        };
        let reply = with_ordinary_person_admin(&tree, room, "person-a", || {
            settings_store::display_control::execute(&source, observed.clone(), command)
        })
        .unwrap();
        let transaction = reply.transaction.unwrap();
        assert_eq!(
            transaction.state,
            settings_store::hardware::DisplayTransactionState::Requested
        );
        assert_eq!(
            transaction.previous.outputs["eDP-1"]
                .mode
                .unwrap()
                .refresh_mhz,
            59999
        );
        let revert = || {
            settings_store::display_control::execute(
                &source,
                observed.clone(),
                settings_store::display_control::DisplayCommand::Revert {
                    transaction: transaction.id,
                },
            )
        };
        assert!(with_ordinary_person_admin(&tree, room, "person-b", revert).is_err());
        // Revocation before its queued publication must already fail closed;
        // neither stale accepted cache nor a fabricated UI role grants access.
        tree.write().desktop.workspaces[0].owner = None;
        tree.write().desktop.workspaces[0].capabilities.clear();
        assert!(with_ordinary_person_admin(&tree, room, "person-a", revert).is_err());
        tree.read().queue_save();
        crate::shell::persist::wait_for_saves();
        assert!(with_ordinary_person_admin(&tree, room, "person-a", revert).is_err());
        assert_eq!(
            settings_store::SettingsSource::load(&source)["display_transaction"]["state"],
            "requested"
        );
        tree.write().desktop.workspaces[0].set_owner("person-a");
        tree.read().queue_save();
        crate::shell::persist::wait_for_saves();
        drop(owner);
        assert!(with_ordinary_person_admin(&tree, room, "person-a", revert).is_err());
    }

    #[test]
    fn action_authority_export_releases_file_custody_and_revokes_dropped_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        tree.desktop.workspaces[0].set_owner("person-a");
        tree.persist_path = Some(path.clone());
        let tree = tree.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        // Keep the actual accepted export alive across the owner's shutdown,
        // exactly as the publication cache and queued export do in production.
        let published = Published {
            rooms: Export::of(&tree.read()).0,
            ..Published::default()
        };
        assert_eq!(
            projection(&published, &binding).owner.as_deref(),
            Some("person-a")
        );
        drop(owner);
        drop(tree);
        let mut restored = crate::shell::persist::load_tree_from(&path).unwrap();
        restored.persist_path = Some(path);
        let reopened = crate::shell::native_workspace_owner::open(restored.shared())
            .expect("cached policy must not retain the state-file lease after shutdown");
        let revoked = projection(&published, &binding);
        assert_eq!(
            revoked.owner, None,
            "a reopened owner cannot revive the old witness"
        );
        assert!(revoked.capabilities.is_empty());
        drop(reopened);
    }

    #[test]
    fn action_authority_export_retains_only_selected_app_person_and_accepted_room_policy() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let room = workspace.room;
        workspace.set_owner("person-a");
        for (subject, scope) in [
            (
                Subject::Principal(Principal::Person("person-a".into())),
                Scope::App("so.tilo.store".into()),
            ),
            (
                Subject::Principal(Principal::Person("person-b".into())),
                Scope::App("so.tilo.store".into()),
            ),
            (
                Subject::Principal(Principal::App("so.tilo.store".into())),
                Scope::Workspace,
            ),
            (
                Subject::Principal(Principal::Person("person-a".into())),
                Scope::App("so.tilo.settings".into()),
            ),
            (
                Subject::Principal(Principal::Person("person-a".into())),
                Scope::Room("foreign-room".into()),
            ),
        ] {
            workspace.grant_capability(CapOverride::grant(subject, scope, Verb::Admin));
        }
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        let mut published = Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        };
        let allowed = projection(&published, &binding);
        allowed.validate().unwrap();
        assert_eq!(allowed.owner.as_deref(), Some("person-a"));
        assert_eq!(
            allowed.capabilities.len(),
            2,
            "owner membership and exact app grant only"
        );
        assert!(allowed.capabilities.iter().all(
            |cap| matches!(&cap.scope, Scope::Room(scope) if scope == &room.to_string())
                || matches!(&cap.scope, Scope::App(app) if app == "so.tilo.store")
        ));
        published.rooms.remove(&room);
        let removed = projection(&published, &binding);
        assert_eq!(removed.owner, None);
        assert!(removed.capabilities.is_empty());
    }
    #[test]
    fn action_authority_atomic_publication_revokes_live_projection_and_renews_lease() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let room = RoomId::new();
        workspace.room = room;
        workspace.set_owner("person-a");
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        let mut published = Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        };
        let path = write_binding(&mut published, &binding, true).unwrap();
        let first: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(first.owner.as_deref(), Some("person-a"));
        assert!(first.is_current_at(tilo_clock::unix_now_ms()));
        assert_eq!(path.file_name().unwrap(), "current.json");
        tree.desktop.workspaces[0].owner = None;
        tree.desktop.workspaces[0].capabilities.clear();
        published.rooms = Export::of(&tree).0;
        write_binding(&mut published, &binding, false).unwrap();
        let revoked: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(revoked.owner, None);
        assert!(revoked.capabilities.is_empty());
        assert!(!first.same_policy(&revoked));
        write_binding(&mut published, &binding, true).unwrap();
        let renewed: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(revoked.same_policy(&renewed));
        assert!(renewed.issued_at_unix_ms >= revoked.issued_at_unix_ms);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn action_authority_lease_follows_live_supervisor_borrowers_and_idle_renewal() {
        let app = InstalledApp {
            package_id: "so.tilo.authority-probe".into(),
            display_name: "Authority probe".into(),
            localized_names: BTreeMap::default(),
            category: None,
            version: "1".into(),
            manifest_path: "/unused/manifest".into(),
            payload_root: "/unused/payload".into(),
            has_gui: true,
            opens_extensions: vec![],
            opens_mime_types: vec![],
            opens_uri_schemes: vec![],
        };
        let room = RoomId::new();
        let mut command = LaunchCommand {
            program: "/unused/launcher".into(),
            args: vec![],
            env: vec![],
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
        };
        let first = attach(&app, room, &mut command).unwrap();
        let binding = first.binding.clone();
        let path = binding_path(&binding);
        assert_eq!(
            command.args,
            vec!["--action-authority", path.to_str().unwrap()]
        );
        let before: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            before.owner, None,
            "unknown room must have no management authority"
        );
        let second = attach(&app, room, &mut command).unwrap();
        assert_eq!(lock(&PUBLISHED).bindings[&binding].borrowers, 2);
        assert!(next_deadline().is_some());
        tick(Instant::now() + Duration::from_secs(2));
        assert!(crate::shell::durable_writer::global().flush_timeout(Duration::from_secs(3)));
        let renewed: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(renewed.issued_at_unix_ms >= before.issued_at_unix_ms);
        assert_eq!(
            renewed.valid_until_unix_ms - renewed.issued_at_unix_ms,
            3_000
        );
        drop(first);
        assert_eq!(lock(&PUBLISHED).bindings[&binding].borrowers, 1);
        assert!(path.exists());
        drop(second);
        let published = lock(&PUBLISHED);
        assert!(!published.bindings.contains_key(&binding));
        assert!(!path.exists());
        if published.bindings.is_empty() {
            assert!(published.next_refresh.is_none());
        }
        drop(published);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A launch whose grants the accepted policy already holds does not wait
    /// for the state writer, however busy it is with other writes; one whose
    /// grant is still queued waits for that publication and then registers.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_launch_waits_on_the_state_writer_only_for_a_grant_not_yet_published() {
        struct Process(std::process::Child);
        impl Drop for Process {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let sleeper = || {
            Process(
                std::process::Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .unwrap(),
            )
        };
        let app = "so.tilo.files";
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        let held = CapOverride::grant(
            Subject::app(app),
            Scope::FsPath("/disposable/held".into()),
            Verb::Read,
        )
        .with_id("held");
        tree.desktop.workspaces[0].capabilities.push(held.clone());
        let accepted: &'static Mutex<Published> = Box::leak(Box::new(Mutex::new(Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        })));
        let writer = crate::shell::durable_writer::DurableWriter::spawn("register-sandbox-test");
        let (release, busy) = std::sync::mpsc::channel::<()>();
        writer.submit(PathBuf::from("unrelated"), move || {
            let _ = busy.recv();
        });
        let lease = |capabilities: Vec<CapOverride>| Lease {
            binding: (room, app.into(), "person".into()),
            capabilities,
            pid: None,
            capabilities_file: None,
        };

        let first = sleeper();
        let started = Instant::now();
        let mut published_grant = lease(vec![held.clone()]);
        published_grant
            .register_in(first.0.id(), accepted, &writer)
            .expect("a published grant registers");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a published grant must not wait for unrelated writes"
        );
        assert!(lock(accepted).sandboxes.contains_key(&first.0.id()));

        let queued = CapOverride::grant(
            Subject::app(app),
            Scope::FsPath("/disposable/queued".into()),
            Verb::Read,
        )
        .with_id("queued");
        tree.desktop.workspaces[0].capabilities.push(queued.clone());
        let rooms = Export::of(&tree).0;
        writer.submit(PathBuf::from("publication"), move || {
            lock(accepted).rooms = rooms;
        });
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            release.send(()).unwrap();
        });
        let second = sleeper();
        let mut queued_grant = lease(vec![held, queued]);
        queued_grant
            .register_in(second.0.id(), accepted, &writer)
            .expect("a queued grant registers once published");
        assert!(lock(accepted).sandboxes.contains_key(&second.0.id()));
        releaser.join().unwrap();
    }
}
