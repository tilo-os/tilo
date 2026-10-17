//! Project accepted action policy and interrupt withdrawn sandbox authority.
//! This observer never creates or expands grants.
use super::{InstalledApp, LaunchCommand, lock, runtime_dir};
use crate::shell::state::RoomId;
use collab_types::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
use collab_types::caps::CapOverride;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod sandbox_revocation;

#[derive(Clone)]
struct RoomPolicy {
    policy: tilo_action_policy::RoomPolicy,
    sandbox: Vec<CapOverride>,
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
    /// For each published room, the policy revision it was exported at.
    revisions: BTreeMap<RoomId, u64>,
    /// For a room whose newest save failed, the revision that save carried
    /// and why it failed. Cleared by a publication that reaches the revision.
    failures: BTreeMap<RoomId, SaveFailure>,
    bindings: BTreeMap<Binding, BindingState>,
    next_refresh: Option<Instant>,
    #[cfg(target_os = "linux")]
    sandboxes: BTreeMap<u32, sandbox_revocation::Sandbox>,
}
#[derive(Clone)]
struct SaveFailure {
    revision: u64,
    kind: std::io::ErrorKind,
    message: String,
}
static PUBLISHED: LazyLock<Mutex<Published>> = LazyLock::new(|| Mutex::new(Published::default()));
/// Signalled whenever [`Export::publish`] replaces the published policy.
static PUBLISHED_CHANGED: std::sync::Condvar = std::sync::Condvar::new();

/// Capture policy for publication only after the normal state writer accepts
/// this exact tree. Superseded/refused state writes never call `publish`.
pub(crate) struct Export(BTreeMap<RoomId, RoomPolicy>, u64);

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
                            policy: workspace.policy.clone(),
                            sandbox: workspace.sandbox.clone(),
                            runtime: tree.native_runtime.authority_witness(),
                            authority: tree.native_authority.clone(),
                        },
                    )
                })
                .collect(),
            tree.policy_revision,
        )
    }

    pub(crate) fn publish(self) {
        let mut published = lock(&PUBLISHED);
        published.revisions = self.0.keys().map(|room| (*room, self.1)).collect();
        published
            .failures
            .retain(|_, failure| failure.revision > self.1);
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
        PUBLISHED_CHANGED.notify_all();
    }

    /// What the save of this export reports if its write fails.
    pub(crate) fn failure_handle(&self) -> FailureHandle {
        FailureHandle(self.0.keys().copied().collect(), self.1)
    }

    pub(crate) fn submit(self) {
        crate::shell::durable_writer::global()
            .submit(authority_root().join("publication"), move || self.publish());
    }
}

/// Reports the failure of the save that would have published a revision.
pub(crate) struct FailureHandle(Vec<RoomId>, u64);

impl FailureHandle {
    /// A launch waiting on this revision, or an earlier one, stops waiting
    /// and reports the save's own error. A later successful save clears it.
    pub(crate) fn fail(self, error: &std::io::Error) {
        let mut published = lock(&PUBLISHED);
        for room in self.0 {
            if published
                .failures
                .get(&room)
                .is_none_or(|old| old.revision <= self.1)
            {
                published.failures.insert(
                    room,
                    SaveFailure {
                        revision: self.1,
                        kind: error.kind(),
                        message: error.to_string(),
                    },
                );
            }
        }
        drop(published);
        PUBLISHED_CHANGED.notify_all();
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
        if workspace.answer_at(
            person,
            &workspace
                .node()
                .app(crate::shell::display_control::SETTINGS_APP_ID),
            tilo_action_policy::ActionKind::Outside,
            now,
        ) != tilo_action_policy::Answer::Allow
        {
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
    if workspace.policy != policy.policy || workspace.sandbox != policy.sandbox {
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
    let current = published
        .rooms
        .get(&binding.0)
        .filter(|policy| {
            policy
                .runtime
                .authority_is_current(&policy.authority, binding.0)
        })
        .map(|policy| policy.policy.clone());
    // A room whose accepted policy is not current names nobody: the app's
    // host decides every action against it, so it allows nothing.
    let policy = current.unwrap_or_else(|| {
        tilo_action_policy::RoomPolicy::unowned(
            room,
            &tilo_action_policy::Subject::Role(tilo_action_policy::Role::Owner),
        )
    });
    let issued_at_unix_ms = tilo_clock::unix_now_ms();
    AppActionAuthorityProjection::project(
        &policy,
        &binding.1,
        &binding.2,
        false,
        issued_at_unix_ms,
        issued_at_unix_ms.saturating_add(3_000),
    )
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
        // In the runtime tmpfs and unsynced: `PUBLISHED` is held here, and
        // the compositor's loop takes it every iteration, so a sync under I/O
        // pressure would freeze the desktop. The sandbox reads the file
        // through the page cache, and a crash discards it with the session.
        super::write_runtime_private(&path, &bytes)?;
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
        retained_projection: None,
        #[cfg(target_os = "linux")]
        selected_target: None,
        revision: None,
    })
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

#[cfg(target_os = "linux")]
fn withdrawn() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "filesystem authority was withdrawn while the application launch was pending",
    )
}

/// The live workspace tree the published policy is derived from. A launch
/// captures its grants from the tree and the policy follows in a save, so the
/// launch records the tree's revision at capture and later waits for the
/// policy to reach it.
pub(crate) trait PolicySource: Send + Sync {
    /// The revision of the authorization state the tree holds now.
    fn revision(&self) -> u64;
}

static POLICY_SOURCE: Mutex<Option<std::sync::Arc<dyn PolicySource>>> = Mutex::new(None);

pub(crate) fn set_policy_source(source: std::sync::Arc<dyn PolicySource>) {
    *lock(&POLICY_SOURCE) = Some(source);
}

struct TreePolicySource(crate::shell::state::WeakWorkspaceTreeHandle);
impl PolicySource for TreePolicySource {
    fn revision(&self) -> u64 {
        self.0.upgrade().map_or(0, |tree| tree.policy_revision())
    }
}

/// Publish policy for sandboxes launched from `tree`.
pub(crate) fn follow_tree(tree: &crate::shell::state::WorkspaceTreeHandle) {
    set_policy_source(std::sync::Arc::new(TreePolicySource(tree.downgrade())));
}

/// The state of `room`'s policy that a launch must see published before its
/// captured grants are judged: the tree's revision when they were captured.
fn captured_revision() -> Option<u64> {
    let source = lock(&POLICY_SOURCE).clone()?;
    Some(source.revision())
}

/// Block until `room`'s published policy is at `revision` or later. Publication
/// signals [`PUBLISHED_CHANGED`], so this neither polls nor guesses a delay: a
/// slow writer delays the launch and a fast one does not. Only a writer that
/// completes no write for a whole [`PUBLICATION_STALL`] window is a fault.
///
/// # Errors
/// The error of a failed save of that revision or a later one, at once;
/// `TimedOut` when the state writer has made no progress.
#[cfg(target_os = "linux")]
fn await_published(room: RoomId, revision: u64) -> std::io::Result<()> {
    let writer = crate::shell::durable_writer::global();
    let mut published = lock(&PUBLISHED);
    let mut seen = writer.progress();
    while published.revisions.get(&room).copied().unwrap_or(0) < revision {
        if let Some(failure) = published.failures.get(&room)
            && failure.revision >= revision
        {
            return Err(std::io::Error::new(
                failure.kind,
                format!("saving filesystem authority failed: {}", failure.message),
            ));
        }
        let (guard, waited) = PUBLISHED_CHANGED
            .wait_timeout(published, PUBLICATION_STALL)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        published = guard;
        if waited.timed_out() {
            let now = writer.progress();
            if now == seen && published.revisions.get(&room).copied().unwrap_or(0) < revision {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the state writer made no progress publishing filesystem authority",
                ));
            }
            seen = now;
        }
    }
    drop(published);
    Ok(())
}

/// How long the state writer may go without completing any write before a
/// launch waiting on a publication stops waiting. Progress restarts it, so a
/// slow disk delays a launch but never fails it.
#[cfg(target_os = "linux")]
const PUBLICATION_STALL: Duration = Duration::from_secs(30);

/// Keeps the lease tied to the existing launch supervisor, including a held
/// launch, rather than an unrelated timer or a package-name process scan.
pub(crate) struct Lease {
    binding: Binding,
    capabilities: Vec<CapOverride>,
    pid: Option<u32>,
    capabilities_file: Option<tempfile::NamedTempFile>,
    retained_projection: Option<tempfile::TempDir>,
    #[cfg(target_os = "linux")]
    selected_target: Option<Arc<super::retained_home::selected_target::TargetLifetime>>,
    /// The tree's policy revision when the grants above were captured.
    revision: Option<u64>,
}
impl Lease {
    #[cfg(target_os = "linux")]
    pub(super) fn retain_selected_target(
        mut self,
        target: Arc<super::retained_home::selected_target::TargetLifetime>,
    ) -> Self {
        self.selected_target = Some(target);
        self
    }

    #[cfg(target_os = "linux")]
    pub(super) fn retain_home_projection(mut self, projection: tempfile::TempDir) -> Self {
        self.retained_projection = Some(projection);
        self
    }

    pub(super) fn with_sandbox_capabilities(
        mut self,
        capabilities: Vec<CapOverride>,
        file: Option<tempfile::NamedTempFile>,
    ) -> Self {
        self.capabilities = capabilities;
        self.capabilities_file = file;
        self.revision = captured_revision();
        self
    }

    #[cfg(target_os = "linux")]
    pub(super) fn retained_release(
        &mut self,
        pid: u32,
        room: RoomId,
        package: &str,
        person: &str,
    ) -> std::io::Result<()> {
        if self.pid != Some(pid) || self.binding != (room, package.to_owned(), person.to_owned()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "retained launch differs from accepted ordinary policy lease",
            ));
        }
        self.register_sandbox(pid)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn register_sandbox(&mut self, pid: u32) -> std::io::Result<()> {
        let sandbox =
            sandbox_revocation::Sandbox::capture(pid, self.binding.clone(), &self.capabilities)?;
        // The published policy lags the tree its grants were captured from:
        // a grant a document open just made, or any unrelated save still on
        // the state writer (slow when the machine is busy), leaves the policy
        // at an older revision for a while although nothing was withdrawn.
        // Wait until the policy reaches the revision the grants came from,
        // then judge once: at or past it, a refusal is a withdrawal. This
        // runs on the launch worker, before the child gate opens.
        if let Some(revision) = self.revision
            && !self.capabilities.is_empty()
        {
            await_published(self.binding.0, revision)?;
        }
        let mut published = lock(&PUBLISHED);
        if let Err(reason) = sandbox.authorization_status(&published) {
            // The reason is a static category: no path, peer or grant.
            tracing::error!(room = %self.binding.0, package_id = %self.binding.1, authority_rejection = reason,
                "application launch refused: its filesystem authority is not the accepted policy");
            return Err(withdrawn());
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
    use tilo_action_policy::Subject;

    /// The published policy is one process-wide value that each tree's
    /// publication replaces whole, so tests that publish take turns.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static TURN: Mutex<()> = Mutex::new(());
        lock(&TURN)
    }

    #[test]
    fn ordinary_display_backend_borrows_real_owner_policy_and_rejects_revocation() {
        let _turn = serial();
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
        tree.write().desktop.workspaces[0].policy.owner = None;
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
        let _turn = serial();
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
            projection(&published, &binding).policy.owner.as_deref(),
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
            revoked.policy.owner, None,
            "a reopened owner cannot revive the old witness"
        );
        assert_eq!(
            revoked
                .authority(&[])
                .decide(
                    &revoked.app_node(),
                    tilo_action_policy::ActionKind::Read,
                    &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                )
                .answer,
            tilo_action_policy::Answer::Deny
        );
        drop(reopened);
    }

    #[test]
    fn action_authority_export_retains_only_selected_app_person_and_accepted_room_policy() {
        let _turn = serial();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let room = workspace.room;
        workspace.set_owner("person-a");
        let node = workspace.node();
        for (id, subject, node) in [
            (
                "a-store",
                Subject::person("person-a"),
                node.clone().app("so.tilo.store"),
            ),
            (
                "b-store",
                Subject::person("person-b"),
                node.clone().app("so.tilo.store"),
            ),
            ("store-room", Subject::app("so.tilo.store"), node.clone()),
            (
                "a-settings",
                Subject::person("person-a"),
                node.app("so.tilo.settings"),
            ),
            (
                "a-foreign",
                Subject::person("person-a"),
                tilo_action_policy::TreePath::room("foreign-room"),
            ),
        ] {
            workspace.grant(tilo_action_policy::Grant::new(
                id,
                subject,
                node,
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                Subject::person("person-a"),
            ));
        }
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        let mut published = Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        };
        let allowed = projection(&published, &binding);
        allowed.validate().unwrap();
        assert_eq!(allowed.policy.owner.as_deref(), Some("person-a"));
        let ids: Vec<&str> = allowed
            .policy
            .grants
            .iter()
            .map(|grant| grant.id.as_str())
            .collect();
        for kept in ["a-store", "b-store", "store-room"] {
            assert!(ids.contains(&kept), "{kept} can decide the app's actions");
        }
        for dropped in ["a-settings", "a-foreign"] {
            assert!(
                !ids.contains(&dropped),
                "{dropped} reaches no action of the app"
            );
        }
        published.rooms.remove(&room);
        let removed = projection(&published, &binding);
        assert_eq!(removed.policy.owner, None);
    }
    #[test]
    fn action_authority_atomic_publication_revokes_live_projection_and_renews_lease() {
        let _turn = serial();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let room = RoomId::new();
        workspace.set_room(room);
        workspace.set_owner("person-a");
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        let mut published = Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        };
        // Written under `PUBLISHED`, which the compositor's loop takes every
        // iteration: it must never wait for the disk.
        #[cfg(target_os = "linux")]
        let path = crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            write_binding(&mut published, &binding, true)
        })
        .unwrap();
        #[cfg(not(target_os = "linux"))]
        let path = write_binding(&mut published, &binding, true).unwrap();
        let first: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(first.policy.owner.as_deref(), Some("person-a"));
        assert!(first.is_current_at(tilo_clock::unix_now_ms()));
        assert_eq!(path.file_name().unwrap(), "current.json");
        tree.desktop.workspaces[0].policy.owner = None;
        published.rooms = Export::of(&tree).0;
        write_binding(&mut published, &binding, false).unwrap();
        let revoked: AppActionAuthorityProjection =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(revoked.policy.owner, None);
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
        let _turn = serial();
        let app = InstalledApp {
            package_id: "so.tilo.authority-probe".into(),
            display_name: "Authority probe".into(),
            localized_names: BTreeMap::default(),
            keywords: Vec::new(),
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
            before.policy.owner, None,
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

    fn live_child() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap()
    }

    fn grant(app: &str, path: &str) -> CapOverride {
        CapOverride::grant(
            collab_types::Subject::Principal(collab_types::Principal::App(app.into())),
            collab_types::Scope::FsPath(path.into()),
            collab_types::Access::Write,
        )
        .with_id(app)
    }

    /// Register a lease for `app` in `room` that captured `grant` at
    /// `revision`, then clean up everything it published.
    fn register(
        room: RoomId,
        app: &str,
        grant: &CapOverride,
        revision: u64,
    ) -> std::io::Result<()> {
        let mut child = live_child();
        let mut lease = Lease {
            binding: (room, app.into(), "person".into()),
            capabilities: vec![grant.clone()],
            pid: None,
            capabilities_file: None,
            retained_projection: None,
            #[cfg(target_os = "linux")]
            selected_target: None,
            revision: Some(revision),
        };
        let registered = lease.register_sandbox(child.id());
        let _ = child.kill();
        let _ = child.wait();
        drop(lease);
        registered
    }

    /// The launch captured a grant that is in the tree, but the save that
    /// publishes it has not even been queued when the sandbox registers (the
    /// writer is idle). The policy is at an older revision, so the launch
    /// waits for it and proceeds once it is published; the writer being idle
    /// is not a verdict.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_grant_whose_save_is_not_queued_yet_proceeds_once_published() {
        let _turn = serial();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        tree.policy_revision = crate::shell::state::next_policy_revision();
        Export::of(&tree).publish();
        let granted = grant("so.tilo.unsaved", "/disposable/unsaved");
        tree.desktop.workspaces[0].sandbox.push(granted.clone());
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let revision = tree.policy_revision;
        let export = Export::of(&tree);
        let saver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            crate::shell::durable_writer::global().submit(
                std::path::PathBuf::from("late-save"),
                move || {
                    export.publish();
                },
            );
        });
        let registered = register(room, "so.tilo.unsaved", &granted, revision);
        saver.join().unwrap();
        lock(&PUBLISHED).rooms.remove(&room);
        registered.expect("the launch waits for the policy revision it captured");
    }

    /// The grant is removed in a later revision before publication. The
    /// policy reaches the captured revision, no longer holds the grant, and
    /// the launch is refused as a withdrawal rather than waited on forever.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_grant_removed_in_a_later_revision_is_refused_as_withdrawn() {
        let _turn = serial();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        let granted = grant("so.tilo.withdrawn", "/disposable/withdrawn");
        tree.desktop.workspaces[0].sandbox.push(granted.clone());
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let captured = tree.policy_revision;
        // Before anything is published, the grant is withdrawn again.
        tree.desktop.workspaces[0].sandbox.clear();
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let export = Export::of(&tree);
        let saver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            crate::shell::durable_writer::global().submit(
                std::path::PathBuf::from("withdrawn-save"),
                move || {
                    export.publish();
                },
            );
        });
        let registered = register(room, "so.tilo.withdrawn", &granted, captured);
        saver.join().unwrap();
        lock(&PUBLISHED).rooms.remove(&room);
        let error = registered.expect_err("a withdrawn grant refuses the launch");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// A write that changes a room's grants submits its own publication, so
    /// a launch that captured the resulting revision is never waiting on a
    /// caller that forgot to save.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_released_grant_write_publishes_its_revision_unprompted() {
        let _turn = serial();
        let tree = crate::shell::state::WorkspaceTree::bootstrap_default().shared();
        let room = tree.read().desktop.workspaces[0].room;
        let granted = grant("so.tilo.unprompted", "/disposable/unprompted");
        tree.write().desktop.workspaces[0]
            .sandbox
            .push(granted.clone());
        let revision = tree.policy_revision();
        let registered = register(room, "so.tilo.unprompted", &granted, revision);
        lock(&PUBLISHED).rooms.remove(&room);
        registered.expect("the write's own save publishes the revision");
    }

    /// The save that would publish the revision fails (its directory cannot
    /// exist). The launch waiting on it ends at once with that save's error,
    /// not after the writer's no-progress window and not as a withdrawal.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_save_ends_the_wait_with_its_own_error() {
        let _turn = serial();
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        std::fs::write(&blocker, b"file").unwrap();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(blocker.join("state.json"));
        let tree = tree.shared();
        let room = tree.read().desktop.workspaces[0].room;
        let granted = grant("so.tilo.failed-save", "/disposable/failed-save");
        tree.write().desktop.workspaces[0]
            .sandbox
            .push(granted.clone());
        let revision = tree.policy_revision();
        let started = std::time::Instant::now();
        let registered = register(room, "so.tilo.failed-save", &granted, revision);
        let waited = started.elapsed();
        {
            let mut published = lock(&PUBLISHED);
            published.rooms.remove(&room);
            published.failures.remove(&room);
        }
        let error = registered.expect_err("a save that failed cannot publish");
        assert!(
            error
                .to_string()
                .contains("saving filesystem authority failed"),
            "{error}"
        );
        assert_ne!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            waited < PUBLICATION_STALL,
            "ended by the failure, not the stall"
        );
    }
}
