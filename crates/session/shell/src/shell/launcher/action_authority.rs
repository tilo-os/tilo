//! Read-only action admission projection; never sandbox or record grants.
use super::{InstalledApp, LaunchCommand, lock, runtime_dir};
use crate::shell::state::RoomId;
use collab_types::caps::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
use collab_types::caps::{Actor, CapOverride, Scope, Subject};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct RoomPolicy {
    local_id: String,
    owner: Option<String>,
    capabilities: Vec<CapOverride>,
    runtime: crate::shell::native_workspace_owner::NativeStateRuntime,
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
                            runtime: tree.native_runtime.clone(),
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
    Ok(Lease(binding))
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
pub(crate) struct Lease(Binding);
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionAuthorityLease")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut published = lock(&PUBLISHED);
        if let Some(state) = published.bindings.get_mut(&self.0) {
            state.borrowers = state.borrowers.saturating_sub(1);
            if state.borrowers == 0 {
                published.bindings.remove(&self.0);
                let _ = std::fs::remove_file(binding_path(&self.0));
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
        let binding = first.0.clone();
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
}
