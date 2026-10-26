//! Project accepted action policy and interrupt withdrawn sandbox authority.
//! This observer never creates or expands grants.
//!
//! It sits beside the workspace model rather than in the launcher: every save
//! of the tree publishes through it, the compositor-free runtime host included.
//! The launcher attaches a launch to it ([`attach_optional`]) and holds the
//! [`Lease`] for the launch's lifetime.
//!
//! Nothing here runs on a clock. A projection is current while this host
//! holds its file (`tilo_files::native::held`), so it ends with the host or a
//! failed replacement without being renewed. What the judgements read is
//! judged again when it changes: a publication, a policy announcement
//! (`tilo_identity_host::policy_change`), the native writer retiring, or the
//! expiry of a grant a running sandbox captured, at that instant.
use crate::shell::state::RoomId;
use collab_types::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
use collab_types::caps::CapOverride;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tilo_lock::MutexRecover as _;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock_recover()
}

fn runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
}

#[cfg(target_os = "linux")]
mod sandbox_revocation;

#[derive(Clone)]
struct RoomPolicy {
    policy: tilo_action_policy::RoomPolicy,
    sandbox: Vec<CapOverride>,
    runtime: crate::shell::native_workspace_owner::NativeStateWitness,
    authority: Arc<crate::shell::native_workspace_owner::NativeAuthorityStorage>,
}

impl RoomPolicy {
    /// Whether the record this policy was exported with is still the one its
    /// writer holds. A record the writer accepted after it, whose own export
    /// is taken but not yet published, does not make it stale: that export
    /// replaces this policy and judges every sandbox again when it lands.
    fn accepted_status(
        &self,
        room: RoomId,
        exporting: &Mutex<Vec<Exporting>>,
    ) -> Result<(), crate::shell::native_workspace_owner::store::AuthorityRejection> {
        self.runtime
            .authority_status_awaiting(&self.authority, room, |accepted| {
                // Read after the writer's record: an owner publication takes
                // its export before it changes that record.
                let exporting: Vec<_> = lock(exporting)
                    .iter()
                    .filter(|export| export.1.same_writer(&self.runtime))
                    .map(|export| export.2.clone())
                    .collect();
                exporting.iter().any(|authority| {
                    crate::shell::native_workspace_owner::store::encode_authority(authority, None)
                        .is_ok_and(|digest| digest == accepted)
                })
            })
    }
}

type Binding = (RoomId, String, String);
struct BindingState {
    projection: AppActionAuthorityProjection,
    /// This host's hold on the projection's file; `None` once withdrawn.
    held: Option<tilo_files::native::held::Held>,
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
    #[cfg(target_os = "linux")]
    sandboxes: BTreeMap<u32, sandbox_revocation::Sandbox>,
    /// The exports of this writer's tree that are on their way here.
    exporting: Arc<Mutex<Vec<Exporting>>>,
}
#[derive(Clone)]
struct SaveFailure {
    revision: u64,
    kind: std::io::ErrorKind,
    message: String,
}

/// The policy one state writer's tree has published, and what is judged
/// against it: app projections and running sandboxes. Clones of a tree share
/// their writer and so this; a shell has one writer, so it has one of these.
pub(crate) struct Authority {
    /// Tells this authority's jobs on the state writer from another's.
    id: u64,
    published: Mutex<Published>,
    /// The same list as `published.exporting`, reachable without taking
    /// `published`. Taken after `published` when both are held, never
    /// before it.
    exporting: Arc<Mutex<Vec<Exporting>>>,
    /// Signalled whenever [`Export::publish`] replaces the published policy.
    changed: std::sync::Condvar,
    /// Wakes a judgement on the next policy announcement; armed while there
    /// is a projection or sandbox to judge.
    watch: std::sync::OnceLock<tilo_identity_host::policy_change::Watch>,
    /// The writer its queued publications and renewals run on when not the
    /// shell's own: a test that holds its writer still then stalls no other
    /// test's saves.
    #[cfg(test)]
    writer: std::sync::OnceLock<Arc<crate::shell::durable_writer::DurableWriter>>,
}

impl Default for Authority {
    fn default() -> Self {
        static NEXT_AUTHORITY: AtomicU64 = AtomicU64::new(0);
        let published = Published::default();
        Self {
            id: NEXT_AUTHORITY.fetch_add(1, Ordering::Relaxed),
            exporting: published.exporting.clone(),
            published: Mutex::new(published),
            changed: std::sync::Condvar::new(),
            watch: std::sync::OnceLock::new(),
            #[cfg(test)]
            writer: std::sync::OnceLock::new(),
        }
    }
}

impl Authority {
    /// The state writer this authority's queued jobs run on.
    #[cfg_attr(
        not(test),
        allow(
            clippy::unused_self,
            reason = "tests give each authority a writer of its own; outside tests every authority shares the one writer"
        )
    )]
    fn writer(&self) -> &crate::shell::durable_writer::DurableWriter {
        #[cfg(test)]
        if let Some(writer) = self.writer.get() {
            return writer;
        }
        crate::shell::durable_writer::global()
    }

    /// Run this authority's queued jobs on a writer of its own, which the
    /// caller may hold without holding up anything else.
    #[cfg(test)]
    pub(crate) fn own_writer(&self) -> Arc<crate::shell::durable_writer::DurableWriter> {
        self.writer
            .get_or_init(|| {
                Arc::new(crate::shell::durable_writer::DurableWriter::spawn(
                    "tilo-test-authority-writer",
                ))
            })
            .clone()
    }

    /// The state writer's key for this authority's `job`. A queued job is
    /// replaced by a later one with the same key, so a publication may only
    /// supersede an older one of the same writer's tree.
    fn job_key(&self, job: &str) -> PathBuf {
        authority_root().join(job).join(self.id.to_string())
    }

    /// The watch that judges again on policy announcements.
    fn watch(self: &Arc<Self>) -> &tilo_identity_host::policy_change::Watch {
        self.watch.get_or_init(|| {
            let authority = Arc::downgrade(self);
            // Runs on the announcing thread: queue the judgement, no more.
            tilo_identity_host::policy_change::Watch::new(move || {
                if let Some(authority) = authority.upgrade() {
                    authority.judge_again();
                }
            })
        })
    }

    /// Something the projections or sandboxes are judged by may have
    /// changed: judge them again on the state writer.
    pub(crate) fn judge_again(self: &Arc<Self>) {
        let authority = self.clone();
        self.writer()
            .submit(self.job_key("judgement"), move || authority.judge());
    }

    /// Judge the running sandboxes and every projection again, rewriting a
    /// projection only when it changed, and wake again only at the next
    /// instant that changes a judgement by itself.
    fn judge(self: &Arc<Self>) {
        let mut published = lock(&self.published);
        #[cfg(target_os = "linux")]
        let idle = published.bindings.is_empty() && published.sandboxes.is_empty();
        #[cfg(not(target_os = "linux"))]
        let idle = published.bindings.is_empty();
        if idle {
            return;
        }
        // Armed before reading, so a change made while judging wakes again.
        let _ = self.watch().arm();
        #[cfg(target_os = "linux")]
        sandbox_revocation::reconcile(&mut published);
        let bindings: Vec<_> = published.bindings.keys().cloned().collect();
        for binding in bindings {
            if let Err(error) = write_binding(&mut published, &binding) {
                withdraw(&mut published, &binding);
                tracing::warn!(room = %binding.0, package_id = %binding.1, %error, "host action authority update failed closed");
            }
        }
        #[cfg(target_os = "linux")]
        let expiry = sandbox_revocation::next_expiry(&published);
        drop(published);
        #[cfg(target_os = "linux")]
        if let Some(expiry) = expiry {
            tilo_identity_host::policy_change::wake_at(expiry.saturating_mul(1_000_000_000));
        }
    }
}

/// Stop vouching for `binding`'s projection: a stale accepted policy kept
/// current would turn a revoke into a grant. Its file is removed, and the
/// hold is let go even if removal fails, so the app reads it as absent.
fn withdraw(published: &mut Published, binding: &Binding) {
    let _ = std::fs::remove_file(binding_path(binding));
    if let Some(state) = published.bindings.get_mut(binding) {
        state.held = None;
    }
}

/// Every [`Export`] taken and neither published nor dropped yet: its id, the
/// writer it was taken from and the record that writer held for it.
type Exporting = (
    u64,
    crate::shell::native_workspace_owner::NativeStateWitness,
    Arc<crate::shell::native_workspace_owner::NativeAuthorityStorage>,
);
static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

/// Capture policy for publication only after the normal state writer accepts
/// this exact tree. Superseded/refused state writes never call `publish`.
pub(crate) struct Export(BTreeMap<RoomId, RoomPolicy>, u64, Pending, Arc<Authority>);

/// An export's place among its writer's exports on their way, from when it
/// is taken until it is published or dropped.
struct Pending {
    id: u64,
    authority: Arc<Authority>,
    published: bool,
}

impl Drop for Pending {
    fn drop(&mut self) {
        lock(&self.authority.exporting).retain(|export| export.0 != self.id);
        // A record its writer accepted while this export was on its way kept
        // the published policy current; dropped unpublished, it no longer
        // does.
        if !self.published {
            self.authority.judge_again();
        }
    }
}

impl Export {
    pub(crate) fn of(tree: &crate::shell::state::WorkspaceTree) -> Self {
        let state = tree.native_runtime.action_authority();
        let runtime = tree.native_runtime.authority_witness();
        let authority = Arc::new(tree.native_authority.clone());
        let id = NEXT_EXPORT.fetch_add(1, Ordering::Relaxed);
        lock(&state.exporting).push((id, runtime.clone(), authority.clone()));
        Self(
            tree.desktop
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace.room(),
                        RoomPolicy {
                            policy: workspace.policy.clone(),
                            sandbox: workspace.sandbox.clone(),
                            runtime: runtime.clone(),
                            authority: authority.clone(),
                        },
                    )
                })
                .collect(),
            tree.policy_revision,
            Pending {
                id,
                authority: state.clone(),
                published: false,
            },
            state,
        )
    }

    pub(crate) fn publish(self) {
        let mut published = lock(&self.3.published);
        published.revisions = self.0.keys().map(|room| (*room, self.1)).collect();
        published
            .failures
            .retain(|_, failure| failure.revision > self.1);
        published.rooms = self.0;
        #[cfg(target_os = "linux")]
        sandbox_revocation::reconcile(&mut published);
        let bindings: Vec<_> = published.bindings.keys().cloned().collect();
        for binding in bindings {
            if let Err(error) = write_binding(&mut published, &binding) {
                withdraw(&mut published, &binding);
                tracing::warn!(room = %binding.0, package_id = %binding.1, %error, "host action authority update failed closed");
            }
        }
        #[cfg(target_os = "linux")]
        let expiry = sandbox_revocation::next_expiry(&published);
        // Only now is this export's record what observers hold.
        let mut pending = self.2;
        pending.published = true;
        drop(pending);
        drop(published);
        self.3.changed.notify_all();
        #[cfg(target_os = "linux")]
        if let Some(expiry) = expiry {
            tilo_identity_host::policy_change::wake_at(expiry.saturating_mul(1_000_000_000));
        }
    }

    /// What the save of this export reports if its write fails.
    pub(crate) fn failure_handle(&self) -> FailureHandle {
        FailureHandle(self.0.keys().copied().collect(), self.1, self.3.clone())
    }

    pub(crate) fn submit(self) {
        let authority = self.3.clone();
        authority
            .writer()
            .submit(authority.job_key("publication"), move || self.publish());
    }
}

/// Reports the failure of the save that would have published a revision.
pub(crate) struct FailureHandle(Vec<RoomId>, u64, Arc<Authority>);

impl FailureHandle {
    /// A launch waiting on this revision, or an earlier one, stops waiting
    /// and reports the save's own error. A later successful save clears it.
    pub(crate) fn fail(self, error: &std::io::Error) {
        let mut published = lock(&self.2.published);
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
        self.2.changed.notify_all();
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
    // A policy the tree holds is judged once its save has published it, as a
    // launch's grants are: a person acting right after a policy write would
    // otherwise be refused for the save's duration. The tree is not held
    // while waiting, so the save it waits for can read it.
    #[cfg(target_os = "linux")]
    {
        let (authority, revision) = {
            let tree = tree.read();
            (tree.native_runtime.action_authority(), tree.policy_revision)
        };
        await_published(&authority, room, revision).map_err(|error| error.to_string())?;
    }
    let tree = tree.read();
    if tree.requires_native_owner(room) {
        return Err("This room requires its current native owner.".into());
    }
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .ok_or("The admitted Settings room is unavailable.")?;
    let state = tree.native_runtime.action_authority();
    let published = lock(&state.published);
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
                .accepted_status(binding.0, &published.exporting)
                .is_ok()
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
    AppActionAuthorityProjection::project(&policy, &binding.1, &binding.2, false)
}

/// Publish `binding`'s projection unless the one held is the same.
fn write_binding(published: &mut Published, binding: &Binding) -> std::io::Result<PathBuf> {
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
    if published
        .bindings
        .get(binding)
        .is_none_or(|state| state.held.is_none() || state.projection != projection)
    {
        // In the runtime tmpfs and unsynced: `published` is held here, so a
        // sync under I/O pressure would hold up every launch and judgement.
        // The sandbox reads the file through the page cache, and a crash
        // discards it with the session (and lets go of it with this host).
        if let Some(parent) = path.parent() {
            tilo_files::native::atomic::create_private_directory(parent)?;
        }
        let held = tilo_files::native::held::publish(&path, &bytes, 0o600)?;
        match published.bindings.get_mut(binding) {
            Some(state) => {
                state.projection = projection;
                state.held = Some(held);
            }
            None => {
                published.bindings.insert(
                    binding.clone(),
                    BindingState {
                        projection,
                        held: Some(held),
                        borrowers: 0,
                    },
                );
            }
        }
    }
    Ok(path)
}

/// Stage `package_id`'s projection of `room`'s accepted policy and pass its
/// path to the launch in `args` (`--action-authority <path>`).
pub(crate) fn attach(
    package_id: &str,
    room: RoomId,
    args: &mut Vec<String>,
) -> std::io::Result<Lease> {
    attach_to(followed_authority(), package_id, room, args)
}

fn attach_to(
    authority: Arc<Authority>,
    package_id: &str,
    room: RoomId,
    args: &mut Vec<String>,
) -> std::io::Result<Lease> {
    let person = crate::shell::state::local_user_id_for_room(&room);
    let binding = (room, package_id.to_owned(), person);
    // Armed before the projection is judged, so a change after it is heard.
    let _ = authority.watch().arm();
    let mut published = lock(&authority.published);
    let path = write_binding(&mut published, &binding)?;
    if let Some(state) = published.bindings.get_mut(&binding) {
        state.borrowers = state.borrowers.saturating_add(1);
    }
    drop(published);
    args.push("--action-authority".into());
    args.push(path.display().to_string());
    Ok(Lease {
        authority,
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
pub(crate) fn attach_optional(
    package_id: &str,
    room: RoomId,
    args: &mut Vec<String>,
) -> Option<Lease> {
    match attach(package_id, room, args) {
        Ok(lease) => Some(lease),
        Err(error) => {
            tracing::warn!(%room, %package_id, %error, "could not stage host action authority; Admin actions remain unavailable");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn withdrawn() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "sandbox authority was withdrawn while the application launch was pending",
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

/// The tree launches capture their grants from, and the authority its
/// writer publishes their policy to.
struct Followed {
    source: Arc<dyn PolicySource>,
    authority: Arc<Authority>,
}

static FOLLOWED: Mutex<Option<Followed>> = Mutex::new(None);

/// The authority launches are judged against: the followed tree's, or,
/// before any tree is followed, one that has published nothing, so every
/// projection names nobody.
fn followed_authority() -> Arc<Authority> {
    static NOTHING_FOLLOWED: LazyLock<Arc<Authority>> = LazyLock::new(Arc::default);
    lock(&FOLLOWED).as_ref().map_or_else(
        || NOTHING_FOLLOWED.clone(),
        |followed| followed.authority.clone(),
    )
}

struct TreePolicySource(crate::shell::state::WeakWorkspaceTreeHandle);
impl PolicySource for TreePolicySource {
    fn revision(&self) -> u64 {
        self.0.upgrade().map_or(0, |tree| tree.policy_revision())
    }
}

/// Publish policy for sandboxes launched from `tree`.
pub(crate) fn follow_tree(tree: &crate::shell::state::WorkspaceTreeHandle) {
    let authority = tree.read().native_runtime.action_authority();
    *lock(&FOLLOWED) = Some(Followed {
        source: Arc::new(TreePolicySource(tree.downgrade())),
        authority,
    });
}

/// The state of `room`'s policy that a launch must see published before its
/// captured grants are judged: the tree's revision when they were captured.
fn captured_revision() -> Option<u64> {
    let source = lock(&FOLLOWED).as_ref()?.source.clone();
    Some(source.revision())
}

/// Block until `room`'s published policy is at `revision` or later. Publication
/// signals [`Authority::changed`], so this neither polls nor guesses a delay: a
/// slow writer delays the launch and a fast one does not. Only a writer that
/// completes no write for a whole [`PUBLICATION_STALL`] window is a fault.
///
/// # Errors
/// The error of a failed save of that revision or a later one, at once;
/// `TimedOut` when the state writer has made no progress.
#[cfg(target_os = "linux")]
fn await_published(authority: &Authority, room: RoomId, revision: u64) -> std::io::Result<()> {
    let writer = authority.writer();
    let mut published = lock(&authority.published);
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
        let (guard, waited) = authority
            .changed
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
    authority: Arc<Authority>,
    binding: Binding,
    capabilities: Vec<CapOverride>,
    pid: Option<u32>,
    capabilities_file: Option<tempfile::NamedTempFile>,
    retained_projection: Option<tempfile::TempDir>,
    /// The launcher's selected-target lifetime, held as long as the lease.
    #[cfg(target_os = "linux")]
    selected_target: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// The tree's policy revision when the grants above were captured.
    revision: Option<u64>,
}
impl Lease {
    #[cfg(target_os = "linux")]
    pub(crate) fn retain_selected_target(
        mut self,
        target: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Self {
        self.selected_target = Some(target);
        self
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn retain_home_projection(mut self, projection: tempfile::TempDir) -> Self {
        self.retained_projection = Some(projection);
        self
    }

    pub(crate) fn with_sandbox_capabilities(
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
    pub(crate) fn retained_release(
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
    pub(crate) fn register_sandbox(&mut self, pid: u32) -> std::io::Result<()> {
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
            await_published(&self.authority, self.binding.0, revision)?;
        }
        let _ = self.authority.watch().arm();
        let mut published = lock(&self.authority.published);
        if let Err(reason) = sandbox.authorization_status(&published) {
            // The reason is a static category: no path, peer or grant.
            tracing::error!(room = %self.binding.0, package_id = %self.binding.1, authority_rejection = reason,
                "application launch refused: its sandbox authority is not the accepted policy");
            return Err(withdrawn());
        }
        published.sandboxes.insert(pid, sandbox);
        let expiry = sandbox_revocation::next_expiry(&published);
        drop(published);
        if let Some(expiry) = expiry {
            tilo_identity_host::policy_change::wake_at(expiry.saturating_mul(1_000_000_000));
        }
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
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the file is removed under the lock, so a launch attaching meanwhile never has its new projection removed"
    )]
    fn drop(&mut self) {
        let mut published = lock(&self.authority.published);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::Subject;

    #[test]
    fn ordinary_display_backend_borrows_real_owner_policy_and_rejects_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
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
        // Revocation fails closed while its publication is queued: custody
        // waits for it rather than act on the stale accepted policy, and
        // neither that cache nor a fabricated UI role grants access.
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
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
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
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let room = workspace.room();
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
    /// What the app reads: the projection is replaced when the policy
    /// changes, stays in place (the same file) while it does not, and is
    /// current only while this host holds it.
    #[test]
    fn action_authority_publication_revokes_live_projection_held_by_the_host() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = RoomId::new();
        tree.desktop.workspaces[0] =
            crate::shell::state::rehomed(&tree.desktop.workspaces[0], room);
        let workspace = &mut tree.desktop.workspaces[0];
        workspace.set_owner("person-a");
        let binding = (room, "so.tilo.store".into(), "person-a".into());
        let mut published = Published {
            rooms: Export::of(&tree).0,
            ..Published::default()
        };
        let read = |path: &std::path::Path| {
            tilo_files::native::held::read(path, MAX_PROJECTION_BYTES)
                .unwrap()
                .map(|bytes| {
                    let read =
                        serde_json::from_slice::<AppActionAuthorityProjection>(&bytes).unwrap();
                    // As the launcher and the app judge it before using it.
                    read.validate().unwrap();
                    read
                })
        };
        // Written under `published`: it must never wait for the disk.
        #[cfg(target_os = "linux")]
        let path = crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            write_binding(&mut published, &binding)
        })
        .unwrap();
        #[cfg(not(target_os = "linux"))]
        let path = write_binding(&mut published, &binding).unwrap();
        assert_eq!(path.file_name().unwrap(), "current.json");
        let first = read(&path).expect("the host holds its projection");
        assert_eq!(first.policy.owner.as_deref(), Some("person-a"));

        // Judged again with nothing changed: the same file, not a new one.
        #[cfg(unix)]
        let inode = |path: &std::path::Path| {
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap())
        };
        #[cfg(unix)]
        let before = inode(&path);
        write_binding(&mut published, &binding).unwrap();
        #[cfg(unix)]
        assert_eq!(inode(&path), before, "an unchanged policy is not rewritten");

        tree.desktop.workspaces[0].policy.owner = None;
        published.rooms = Export::of(&tree).0;
        write_binding(&mut published, &binding).unwrap();
        let revoked = read(&path).expect("the replacement is held");
        assert_eq!(revoked.policy.owner, None);
        assert_ne!(first, revoked);

        // A replacement that fails lets go of what the app could still read.
        withdraw(&mut published, &binding);
        assert_eq!(read(&path), None);
        // So does the host going away with its projection still in place.
        write_binding(&mut published, &binding).unwrap();
        assert!(read(&path).is_some());
        drop(published);
        assert!(path.exists());
        assert_eq!(read(&path), None, "a host that is gone vouches for nothing");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// An idle desktop running an app, blanked or not, has nothing to wake
    /// for here: the projection stays current with no renewal, nothing is
    /// queued on the state writer and no instant is registered, until
    /// something it is judged by changes.
    #[test]
    fn a_running_app_with_no_pending_deadline_schedules_no_wakeups() {
        let package_id = "so.tilo.authority-probe";
        let room = RoomId::new();
        let mut args = Vec::new();
        let authority = Arc::new(Authority::default());
        // Judgements run on this authority's own writer, which other tests'
        // saves do not queue on.
        let writer = authority.own_writer();
        let first = attach_to(authority.clone(), package_id, room, &mut args).unwrap();
        let binding = first.binding.clone();
        let path = binding_path(&binding);
        assert_eq!(args, vec!["--action-authority", path.to_str().unwrap()]);
        let read = || {
            tilo_files::native::held::read(&path, MAX_PROJECTION_BYTES)
                .unwrap()
                .map(|bytes| {
                    let read =
                        serde_json::from_slice::<AppActionAuthorityProjection>(&bytes).unwrap();
                    // As the launcher and the app judge it before using it.
                    read.validate().unwrap();
                    read
                })
        };
        let before = read().expect("the projection is current while held");
        assert_eq!(
            before.policy.owner, None,
            "unknown room must have no management authority"
        );
        let second = attach_to(authority.clone(), package_id, room, &mut args).unwrap();
        assert_eq!(lock(&authority.published).bindings[&binding].borrowers, 2);

        // Idle: nothing queued, and no sandbox with a grant that expires.
        writer.flush();
        let progress = writer.progress();
        let epoch = tilo_identity_host::policy_change::epoch();
        #[cfg(target_os = "linux")]
        assert_eq!(
            sandbox_revocation::next_expiry(&lock(&authority.published)),
            None
        );
        std::thread::sleep(Duration::from_millis(1_500));
        writer.flush();
        // Another test of this process announcing wakes it, as it should.
        if tilo_identity_host::policy_change::epoch() == epoch {
            assert_eq!(writer.progress(), progress, "nothing ran while idle");
        }
        assert_eq!(read(), Some(before.clone()), "still current, never renewed");

        // A policy announcement is what wakes it: judged once, unchanged.
        tilo_identity_host::policy_change::announce();
        writer.flush();
        assert_eq!(read(), Some(before));

        drop(first);
        assert_eq!(lock(&authority.published).bindings[&binding].borrowers, 1);
        assert!(path.exists());
        drop(second);
        let published = lock(&authority.published);
        assert!(published.bindings.is_empty());
        assert!(!path.exists());
        drop(published);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// A grant a running sandbox captured that expires is the one instant
    /// judged by itself; a grant without an expiry schedules nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_an_expiring_captured_grant_schedules_a_judgement() {
        let room = RoomId::new();
        let lasting = grant("so.tilo.lasting", "/disposable/lasting");
        let mut expiring = grant("so.tilo.expiring", "/disposable/expiring");
        expiring.expiry = collab_types::caps::Expiry::AtUnix(4_000_000_000);
        let mut children = [live_child(), live_child()];
        let mut published = Published::default();
        let capture = |child: &std::process::Child, grant: &CapOverride| {
            sandbox_revocation::Sandbox::capture(
                child.id(),
                (room, grant.id.clone(), "person".into()),
                std::slice::from_ref(grant),
            )
            .unwrap()
        };
        published
            .sandboxes
            .insert(children[0].id(), capture(&children[0], &lasting));
        assert_eq!(sandbox_revocation::next_expiry(&published), None);
        published
            .sandboxes
            .insert(children[1].id(), capture(&children[1], &expiring));
        assert_eq!(
            sandbox_revocation::next_expiry(&published),
            Some(4_000_000_000)
        );
        published.sandboxes.remove(&children[1].id());
        assert_eq!(sandbox_revocation::next_expiry(&published), None);
        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    // Sandbox registration (`Lease::register_sandbox`) exists only on Linux,
    // so do these fixtures and the tests below that use them.
    #[cfg(target_os = "linux")]
    fn live_child() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap()
    }

    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    fn register(
        authority: Arc<Authority>,
        room: RoomId,
        app: &str,
        grant: &CapOverride,
        revision: u64,
    ) -> std::io::Result<()> {
        let mut child = live_child();
        let mut lease = Lease {
            authority,
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
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
        tree.policy_revision = crate::shell::state::next_policy_revision();
        Export::of(&tree).publish();
        let granted = grant("so.tilo.unsaved", "/disposable/unsaved");
        tree.desktop.workspaces[0].sandbox.push(granted.clone());
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let revision = tree.policy_revision;
        let authority = tree.native_runtime.action_authority();
        let export = Export::of(&tree);
        let writer = authority.own_writer();
        let saver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            writer.submit(std::path::PathBuf::from("late-save"), move || {
                export.publish();
            });
        });
        let registered = register(authority, room, "so.tilo.unsaved", &granted, revision);
        saver.join().unwrap();
        registered.expect("the launch waits for the policy revision it captured");
    }

    /// The grant is removed in a later revision before publication. The
    /// policy reaches the captured revision, no longer holds the grant, and
    /// the launch is refused as a withdrawal rather than waited on forever.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_grant_removed_in_a_later_revision_is_refused_as_withdrawn() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room();
        let granted = grant("so.tilo.withdrawn", "/disposable/withdrawn");
        tree.desktop.workspaces[0].sandbox.push(granted.clone());
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let captured = tree.policy_revision;
        // Before anything is published, the grant is withdrawn again.
        tree.desktop.workspaces[0].sandbox.clear();
        tree.policy_revision = crate::shell::state::next_policy_revision();
        let authority = tree.native_runtime.action_authority();
        let export = Export::of(&tree);
        let writer = authority.own_writer();
        let saver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            writer.submit(std::path::PathBuf::from("withdrawn-save"), move || {
                export.publish();
            });
        });
        let registered = register(authority, room, "so.tilo.withdrawn", &granted, captured);
        saver.join().unwrap();
        let error = registered.expect_err("a withdrawn grant refuses the launch");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// A write that changes a room's grants submits its own publication, so
    /// a launch that captured the resulting revision is never waiting on a
    /// caller that forgot to save.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_released_grant_write_publishes_its_revision_unprompted() {
        let tree = crate::shell::state::WorkspaceTree::bootstrap_default().shared();
        let room = tree.read().desktop.workspaces[0].room();
        let granted = grant("so.tilo.unprompted", "/disposable/unprompted");
        tree.write().desktop.workspaces[0]
            .sandbox
            .push(granted.clone());
        let revision = tree.policy_revision();
        let authority = tree.read().native_runtime.action_authority();
        let registered = register(authority, room, "so.tilo.unprompted", &granted, revision);
        registered.expect("the write's own save publishes the revision");
    }

    /// A saved window reopens while the frame holds the tree for writing, so
    /// its launch captures the policy revision on the thread that holds it.
    /// That used to wait for a read lock its own thread held and stop the
    /// compositor. The launch now captures the revision the write publishes
    /// when it is released, and the publication it waits for arrives.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_launch_inside_a_tree_write_captures_the_revision_that_write_publishes() {
        let tree = crate::shell::state::WorkspaceTree::bootstrap_default().shared();
        let room = tree.read().desktop.workspaces[0].room();
        let authority = tree.read().native_runtime.action_authority();
        let source = TreePolicySource(tree.downgrade());
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let guard = tree.write();
            let captured = source.revision();
            drop(guard);
            let _ = done.send(captured);
        });
        let captured = tilo_test_env::recv(
            "the revision captured inside a tree write, which must not wait on that write",
            &finished,
        );
        let published = await_published(&authority, room, captured);
        published.expect("releasing the write publishes the revision the launch captured");
    }

    /// The save that would publish the revision fails (its directory cannot
    /// exist). The launch waiting on it ends at once with that save's error,
    /// not after the writer's no-progress window and not as a withdrawal.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_save_ends_the_wait_with_its_own_error() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        std::fs::write(&blocker, b"file").unwrap();
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(blocker.join("state.json"));
        let tree = tree.shared();
        let room = tree.read().desktop.workspaces[0].room();
        let granted = grant("so.tilo.failed-save", "/disposable/failed-save");
        tree.write().desktop.workspaces[0]
            .sandbox
            .push(granted.clone());
        let revision = tree.policy_revision();
        let authority = tree.read().native_runtime.action_authority();
        let registered = register(authority, room, "so.tilo.failed-save", &granted, revision);
        let error = registered.expect_err("a save that failed cannot publish");
        assert!(
            error
                .to_string()
                .contains("saving filesystem authority failed"),
            "{error}"
        );
        // A stall would end it as `TimedOut` with the writer's own message.
        assert_ne!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_ne!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    /// A document commit in an admitted room (a paste, undo or redo there)
    /// changes the record the state writer holds but no grant. The writer
    /// accepts the record before the export carrying it is published, and
    /// meanwhile the state writer is still busy with an earlier write. A
    /// judgement of the sandboxes in that gap interrupts none of them.
    /// A grant withdrawn afterwards still interrupts its sandbox, and so does
    /// an export of a record the writer has moved past with nothing newer on
    /// the way.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_owner_commit_awaiting_its_export_interrupts_no_sandbox() {
        let (_directory, owner, room, now, key) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let tree = crate::shell::native_workspace_owner::tree_of(&owner);
        let authority = tree.read().native_runtime.action_authority();
        let kept = grant("so.tilo.kept", "/disposable/kept");
        let withdrawn = grant("so.tilo.withdrawn", "/disposable/withdrawn");
        let mut write = tree.write();
        write.append_empty_workspace("Ordinary room");
        let workspace = write.desktop.workspaces.last_mut().unwrap();
        workspace.sandbox.extend([kept.clone(), withdrawn.clone()]);
        let ordinary = workspace.room();
        drop(write);
        crate::shell::persist::wait_for_saves();
        let mut children = [live_child(), live_child()];
        {
            let mut published = lock(&authority.published);
            for (child, grant) in children.iter().zip([&kept, &withdrawn]) {
                let sandbox = sandbox_revocation::Sandbox::capture(
                    child.id(),
                    (ordinary, grant.id.clone(), "person".into()),
                    std::slice::from_ref(grant),
                )
                .unwrap();
                sandbox.authorization_status(&published).unwrap();
                published.sandboxes.insert(child.id(), sandbox);
            }
        }
        let running = |child: &std::process::Child| {
            lock(&authority.published)
                .sandboxes
                .contains_key(&child.id())
                && crate::shell::host_runtime::proc_stat(child.id()).is_some()
        };
        let before = Export::of(&tree.read());

        // The commit's export queues behind an earlier write on the writer
        // that publishes it. Held on this authority's own writer, the wait
        // holds up no other test's saves.
        let writer = authority.own_writer();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (started, starting) = std::sync::mpsc::channel();
        writer.submit(PathBuf::from("earlier-write"), move || {
            let _ = started.send(());
            let _ = held.recv();
        });
        starting.recv().unwrap();
        let (request, _) =
            crate::shell::native_workspace_owner::test_document_edit(&owner, room, &key, now, 1);
        owner.commit_document(request, now).unwrap();
        authority.judge();
        assert!(
            children.iter().all(running),
            "the accepted record's export is on its way; nothing was withdrawn"
        );
        drop(release);
        writer.flush();
        assert!(children.iter().all(running));

        tree.write()
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == ordinary)
            .unwrap()
            .sandbox
            .retain(|cap| cap.id != withdrawn.id);
        crate::shell::persist::wait_for_saves();
        assert!(!running(&children[1]), "a withdrawn grant interrupts");
        assert!(running(&children[0]));

        before.publish();
        assert!(
            !running(&children[0]),
            "an export of a record the writer moved past, with nothing newer pending, is stale"
        );
        for child in &mut children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
