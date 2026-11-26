//! One framework app mounted headlessly: the real component runtime,
//! storage, permissions, undo and history, without a window. Tests drive it
//! as [`HeadlessApp`](super::testing::HeadlessApp); the app binary's own modes drive
//! it too, so it ships in every release app: `--surface` reports the
//! surface it publishes, `--bench` captures its fixtures, and `--data-dir`
//! and `--shots` open it on a profile of their own.

use std::fmt::Write as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_resource::{Effect, Invoke, Resource};
use tilo_ui_base::{
    permissions::MemberId, permissions::PermissionContext, permissions::Role,
    permissions::SharedPermissions,
};
use ui_core::application::{
    ApplicationOwner, Capture, ComponentRunnerInputs, OpenFile, PlaybackHandoff,
    PreparedComponentInputs, ProcessContext,
};
use ui_core::headless::Driver;
use ui_core::launch::LaunchError;

pub(super) const AUTHOR: &str = "test-author";
use ui_core::resource::{
    DocumentCheckout, DocumentId, DocumentPort, MemoryWorkspace, ResourceStore,
};

use super::app::{
    ResourceApp, definition, hand_launch, prepare_on, role_permissions, settle_launch,
};
use super::launch::LaunchInput;
use super::root::ResourceRoot;

mod report;

pub use report::surface_report;

pub(super) const INSTANCE: u64 = 7;
/// The workspace a test opens the app in unless it names one. A workspace is
/// named by a UUID: a record kept in one has a route to travel on only then,
/// and what its window stores is sealed for that workspace's members.
pub const TEST_WORKSPACE: &str = "5b1f7c0e-2a84-4d63-9f51-0c8e7a4b6d92";
/// The device a test app runs on unless it names another.
const TEST_DEVICE: &str = "test-device";

/// A fresh directory for a test's or a capture's private profile, removed
/// with it. It is in memory where the system has one
/// ([`tilo_paths::memory_temp_dir`], `/dev/shm` on Linux):
/// the store still syncs every write as it does for a person, but a sync of
/// a profile that is deleted moments later then costs nothing, where on a
/// shared disk it waits for every other writer's data to reach it, and a
/// capture syncs on each staged edit. Elsewhere it is the system's
/// temporary directory.
pub(crate) fn profile_directory() -> std::io::Result<tempfile::TempDir> {
    if let Some(memory) = tilo_paths::memory_temp_dir()
        && let Ok(directory) = tempfile::Builder::new()
            .prefix("tilo-profile-")
            .tempdir_in(memory)
    {
        return Ok(directory);
    }
    tempfile::tempdir()
}

/// A private profile: its directory, removed with it, and the workspace's store its records are
/// kept in, which every launch on the profile reads and writes, so a launch after another finds
/// what that one wrote.
pub(crate) struct Kept {
    directory: tempfile::TempDir,
    records: MemoryWorkspace,
    /// Whether a write is answered only to a writer that waits for it, until the workspace
    /// catches up ([`Self::catch_up`]): a workspace slower than the frames that write, which a
    /// window looking without waiting finds still saving.
    slow: bool,
    /// The slow workspace caught up: a look without waiting is answered too.
    caught_up: Rc<std::cell::Cell<bool>>,
    /// The workspace's store over a connection to Tilo, which keeps the profile's records in
    /// place of [`Self::records`] when a test hands one over ([`HeadlessApp::over_workspace`]).
    workspace: Option<Rc<dyn DocumentPort>>,
}

impl Kept {
    /// A fresh private profile, holding nothing.
    pub(crate) fn fresh() -> Result<Rc<Self>, LaunchError> {
        Self::answering(false, None)
    }

    /// [`Self::fresh`], whose workspace answers a write only to a writer that waits for it, until
    /// it catches up ([`Self::catch_up`]).
    pub(crate) fn slow() -> Result<Rc<Self>, LaunchError> {
        Self::answering(true, None)
    }

    /// A fresh private profile whose records `workspace` keeps.
    fn over(workspace: Rc<dyn DocumentPort>) -> Result<Rc<Self>, LaunchError> {
        Self::answering(false, Some(workspace))
    }

    fn answering(
        slow: bool,
        workspace: Option<Rc<dyn DocumentPort>>,
    ) -> Result<Rc<Self>, LaunchError> {
        Ok(Rc::new(Self {
            directory: profile_directory().map_err(operation)?,
            records: MemoryWorkspace::default(),
            slow,
            caught_up: Rc::default(),
            workspace,
        }))
    }

    /// The slow workspace catches up: every write, before and after, is answered to a look that
    /// does not wait as well. A window's close waits for its writes without holding a frame, so
    /// it only ever looks, and never ends over a workspace that answers no look.
    pub(crate) fn catch_up(&self) {
        self.caught_up.set(true);
    }

    /// Take `app`'s documents from now on to come from the workspace's hosts, as a page's do
    /// ([`MemoryWorkspace::await_hosts`]): one not held here is on its way until they send it
    /// or say they hold none of it.
    fn await_hosts(&self, app: &str) {
        self.records.await_hosts(app);
    }

    /// The workspace's hosts answered for the document `id` with `answer`. Only their word that
    /// they hold none of it makes it new; another person's own record stays on its way here,
    /// never started in its place.
    fn hosts_answered(&self, id: &DocumentId, answer: ui_core::resource::WorkspaceAnswer) {
        if answer == ui_core::resource::WorkspaceAnswer::HoldsNone {
            self.records.hosts_hold_none(id.route());
        }
    }

    /// The workspace's hosts send the document `id` whole, as they send what a device asked
    /// for: it is held from now on, and read as sent.
    fn hosts_send(
        &self,
        app: &str,
        workspace: &str,
        id: &DocumentId,
        whole: &[u8],
    ) -> Result<(), String> {
        if self.workspace.is_some() {
            return Err(
                "a profile over a workspace's store holds only what is written to it".into(),
            );
        }
        self.records.hold(app, workspace, id.route(), whole)
    }

    /// The profile's directory.
    pub(crate) fn path(&self) -> &std::path::Path {
        self.directory.path()
    }

    /// The workspace's store the profile's records are kept in. A store reading through it holds
    /// the profile, whose directory then stays while the store does.
    pub(crate) fn records(self: &Rc<Self>) -> Rc<dyn DocumentPort> {
        Rc::clone(self) as Rc<dyn DocumentPort>
    }

    /// Hold `document` as `app`'s document `id` in `workspace`, as the device holds a record it
    /// synced.
    fn hold(
        &self,
        app: &str,
        workspace: &str,
        id: &DocumentId,
        document: &mut tilo_resource::Doc,
    ) -> Result<(), LaunchError> {
        if self.workspace.is_some() {
            return Err(operation(
                "a profile over a workspace's store holds only what is written to it",
            ));
        }
        self.records
            .hold(app, workspace, id.route(), &document.save())
            .map_err(operation)
    }

    /// [`Self::hold`] of a record under the identity it is stamped with, which this returns.
    fn hold_record(
        &self,
        app: &str,
        workspace: &str,
        document: &mut tilo_resource::Doc,
    ) -> Result<DocumentCheckout, LaunchError> {
        let checkout = ui_core::resource::identity(document)
            .ok_or_else(|| operation("the record carries no identity"))?;
        self.hold(app, workspace, &checkout.id, document)?;
        Ok(checkout)
    }
}

impl DocumentPort for Kept {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<ui_core::resource::DocumentAsk>>,
    ) -> Result<Vec<(String, ui_core::resource::DocumentRead)>, String> {
        if let Some(port) = &self.workspace {
            return port.read(app, workspace, reads);
        }
        self.records.read(app, workspace, reads)
    }

    fn when_answered(&self, wake: ui_core::resource::ReadyWake) -> bool {
        match &self.workspace {
            Some(port) => port.when_answered(wake),
            None => self.records.when_answered(wake),
        }
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        kind: &str,
        changes: Vec<u8>,
    ) -> ui_core::resource::Written {
        let mut written = match &self.workspace {
            Some(port) => port.write(app, workspace, id, kind, changes),
            None => self.records.write(app, workspace, id, kind, changes),
        };
        if !self.slow {
            return written;
        }
        let caught_up = Rc::clone(&self.caught_up);
        Box::new(move |wait: std::time::Duration| {
            if wait.is_zero() && !caught_up.get() {
                None
            } else {
                written(wait)
            }
        })
    }
}

#[derive(Clone)]
enum Profile {
    /// A private temporary profile, removed with the app, the workspace the app
    /// opens in and the device it runs on.
    Temporary(Rc<Kept>, String, String),
    /// An explicit `--data-dir` profile the app itself would open.
    DataDir(PathBuf),
    /// Another window a launcher opens on a temporary profile, in a workspace on
    /// a device:
    /// an instance of its own, whose fallback record is that window's own
    /// unless the app keeps one per workspace, as the native host decides.
    Window(Rc<Kept>, String, String, u64),
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
        .provide_settings(ui_core::storage::SettingsDocument::Profile(
            root.to_path_buf(),
        ));
    prepared
        .context
        .provide_files(ui_core::storage::AppFiles::at(
            ui_core::storage::AppPaths::under(root.join("files"), app_id),
        ));
    prepared
}

pub(crate) fn data_dir_root(directory: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf())
}

pub(crate) fn data_dir_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    super::app::profile_content(
        &std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf()),
    )
}

/// The instance a `--data-dir` profile mounts.
pub(crate) const DATA_DIR_INSTANCE: u64 = 0;
/// The workspace a `--data-dir` profile keeps its records in. A workspace is
/// named by a UUID, so the records kept in it have a route to travel on.
pub const DATA_DIR_WORKSPACE: &str = "a7d3e1f4-6b28-4c05-9e3a-71f2b8c4d590";

thread_local! {
    /// The records of each `--data-dir` profile this thread opened. A window outside every
    /// workspace keeps its records in memory only: a second window or a relaunch on the same
    /// profile finds them while the process runs, and none of them outlives it. Records that must
    /// outlive the process belong to a workspace, which the device's daemon keeps.
    static DATA_DIR_RECORDS: std::cell::RefCell<HashMap<PathBuf, Rc<MemoryWorkspace>>> =
        std::cell::RefCell::default();
}

/// The host of `app`'s window on an explicit `--data-dir` profile: one personal workspace for that
/// directory, whose records this process holds ([`DATA_DIR_RECORDS`]). Nothing else writes them,
/// so the window writes its own. Tools that seed or inspect such a profile open it the same way
/// the app does.
pub(crate) fn data_dir_host(
    directory: &std::path::Path,
    app: &str,
    author: &MemberId,
    permissions: SharedPermissions,
) -> Result<ApplicationOwner, LaunchError> {
    let records = DATA_DIR_RECORDS.with_borrow_mut(|profiles| {
        Rc::clone(profiles.entry(data_dir_root(directory)).or_default())
    });
    ApplicationOwner::with_port(
        Some((records as Rc<dyn DocumentPort>, true)),
        app,
        "local-device".into(),
        DATA_DIR_WORKSPACE,
        author.clone(),
        permissions,
        tilo_actions::ActionContext::default(),
    )
}

/// Hold `workspace`'s content secret on the route of `context`'s record, as a
/// host that keeps the workspace itself does: what the window stores is then
/// sealed once for that workspace, so the same bytes stored twice are one item
/// under one reference. A workspace whose keys come from the collaboration
/// peer network holds its members' secret instead, once they arrive
/// (`collaboration::broker_key_fetch`).
///
/// # Errors
/// A record with no route ([`super::app::resource_route`]).
pub(super) fn hold_workspace_keys(
    context: &ProcessContext,
    workspace: &str,
) -> Result<(), LaunchError> {
    let route = super::app::resource_route(context)?;
    tilo_ui_base::collaboration::document_content::ContentLink::of(&route)
        .hold_secret(workspace_secret(workspace));
    Ok(())
}

/// The content secret of `workspace`'s members, which every window of it in
/// this process holds: a workspace this device keeps itself — a profile of its
/// own, a launch outside every workspace — has no other member to take one
/// from.
fn workspace_secret(workspace: &str) -> Option<tilo_seal::SymmetricKey> {
    static SECRETS: OnceLock<Mutex<HashMap<String, tilo_seal::SymmetricKey>>> = OnceLock::new();
    let generated = tilo_seal::SymmetricKey::generate().ok()?;
    Some(
        SECRETS
            .get_or_init(Mutex::default)
            .lock_recover()
            .entry(workspace.to_owned())
            .or_insert(generated)
            .clone(),
    )
}

impl Profile {
    /// The workspace the app opens in: the profile's own, or the one a
    /// `--data-dir` profile is kept in.
    fn workspace(&self) -> &str {
        match self {
            Self::Temporary(_, workspace, _) | Self::Window(_, workspace, ..) => workspace,
            Self::DataDir(_) => DATA_DIR_WORKSPACE,
        }
    }

    /// The private profile's store of records; `None` for a `--data-dir` one.
    fn kept(&self) -> Option<&Rc<Kept>> {
        match self {
            Self::Temporary(kept, ..) | Self::Window(kept, ..) => Some(kept),
            Self::DataDir(_) => None,
        }
    }

    /// The profile's settings, which the app's typed settings use instead of
    /// the person's.
    fn settings(&self) -> ui_core::storage::SettingsDocument {
        ui_core::storage::SettingsDocument::Profile(self.root())
    }

    /// The directory the profile keeps everything in.
    fn root(&self) -> PathBuf {
        match self {
            Self::Temporary(directory, ..) | Self::Window(directory, ..) => {
                directory.path().to_path_buf()
            }
            Self::DataDir(directory) => data_dir_root(directory),
        }
    }

    /// The profile's content store, which the app keeps handed files and
    /// other bytes its record names in, instead of the person's. It stands in
    /// for the device store of the one workspace the app is mounted in, so a
    /// content reference resolves in it as on a device.
    fn content(&self) -> tilo_content::ContentStore {
        match self {
            Self::Temporary(directory, ..) | Self::Window(directory, ..) => {
                window_content(directory.path())
            }
            Self::DataDir(directory) => data_dir_content(directory).for_one_workspace(),
        }
    }
}

/// The content store of the temporary profile `directory`, as a window of it
/// holds it: one workspace's, named by plain address.
pub(super) fn window_content(directory: &std::path::Path) -> tilo_content::ContentStore {
    super::app::profile_content(directory).for_one_workspace()
}

/// What hosts a headless window beside the app, in place of the device's daemon: a window
/// names its host the actions of the records it shows that another workspace holds (a person's
/// profile records), and only a host holding that workspace decides them.
pub trait Host {
    /// Keep the settings of the profile at `root` a window is about to open on, before the window
    /// first reads them, as this host keeps a person's. A host that keeps none leaves the profile
    /// its own document. Another window on the same profile finds them as the first left them.
    fn keeps_settings(_root: &std::path::Path) {}

    /// Take up the window `driver` opened for `author`, before its first frame.
    fn opened(driver: &mut Driver, author: &str);
}

/// No host beside the window, as in a capture or a fixture seeder run on its own: the actions
/// of records another workspace holds stay undecided, so their controls say they are being
/// checked.
pub struct Alone;

impl Host for Alone {
    fn opened(_: &mut Driver, _: &str) {}
}

/// One framework app mounted headlessly: a test, a fixture seeder or an
/// inspection tool drives it through the real runtime, hosted as `H` says.
pub struct HeadlessApp<A: ResourceApp, H: Host = Alone> {
    /// Boxed: a test moves its app between the helpers that open, relaunch and capture it, and
    /// each move of a driver held by value took its whole size on the stack again.
    driver: Box<Driver>,
    /// Where the window's documents are kept: another window of this profile
    /// shares them ([`Self::open_window`]).
    owner: ApplicationOwner,
    /// Whether the app is closed: its record written and its components closed, once.
    closed: bool,
    records: ResourceStore,
    /// The stores of another app's documents that take theirs from the workspace's holders
    /// ([`Self::awaiting_the_workspace`]): held, so that they stay the ones the window reads.
    awaited: Vec<ResourceStore>,
    checkout: DocumentCheckout,
    instance: u64,
    profile: Profile,
    role: Role,
    /// Who the app is open for: the author of its edits and the person its
    /// person records belong to.
    author: String,
    /// The last effect [`Self::effect`] invoked.
    last_effect: Option<tilo_actions::InvocationId>,
    /// What a host replicating the record to a workspace published of it
    /// ([`Self::shared_in_workspace`]); `None` while nobody replicates it.
    workspace: Option<ui_core::state::DocumentFrontier>,
    /// Shared with the host: changing it is a role change the running app
    /// sees, as when the record's owner changes someone's access.
    permissions: SharedPermissions,
    /// The window is a capture: every motion in it reads arrived while this
    /// is held, so what it photographs is the window at rest.
    _settled: Option<tilo_ui_base::motion::testing::Settled>,
    /// The app in the person's own workspace, with no window of its own, where this window is
    /// in another one and the app keeps records of the person's: it holds those records and
    /// answers this window's reads and changes of them, as the app the device starts there does.
    persons_own: Option<Rc<std::cell::RefCell<HeadlessApp<A>>>>,
    marker: std::marker::PhantomData<(A, H)>,
}

/// Stand in for the person's own workspace, for a window of `author`'s in another one whose app
/// keeps records of theirs: the app runs there with no window, on the same profile, and answers
/// what the window reads of those records and the changes it invokes on them, as the app the
/// device starts in the person's workspace does. `None` where the window is in that workspace,
/// the author has no identity, or the app keeps nothing of the person's.
fn serve_persons_own<A: ResourceApp>(
    driver: &mut Driver,
    profile: &Profile,
    author: &str,
) -> Result<Option<Rc<std::cell::RefCell<HeadlessApp<A>>>>, LaunchError> {
    let Some(own) = tilo_actions::policy::roles::workspace_of_person(author) else {
        return Ok(None);
    };
    let (kept, device) = match profile {
        Profile::Temporary(kept, workspace, device)
        | Profile::Window(kept, workspace, device, _)
            if !own.eq_ignore_ascii_case(workspace) =>
        {
            (kept, device)
        }
        _ => return Ok(None),
    };
    let mut declared = super::Resources::declaring();
    A::resources(&mut declared);
    let keeps = declared.declares_profile()
        || A::PERSON_RESOURCES
            .iter()
            .any(|record| record.owner().is_none());
    if !keeps {
        return Ok(None);
    }
    let provider = Rc::new(std::cell::RefCell::new(HeadlessApp::<A>::open(
        Profile::Temporary(Rc::clone(kept), own.clone(), device.clone()),
        Role::Owner,
        author,
        &[],
        None,
        None,
        None,
        |_| {},
    )?));
    let served = Rc::clone(&provider);
    driver.serve_provider(
        tilo_actions::remote::application_provider_token(&own, A::ID),
        Box::new(move |invocation| {
            // Who called, as the workspace admitted them: an agent acts for the person its
            // certificate chains to.
            let invoker = match invocation.actor.member_id() {
                Some(MemberId::Agent { id, person }) => {
                    tilo_actions::ActionInvoker::agent(id.clone(), person.clone())
                        .acting_for(person.clone())
                }
                Some(member) => tilo_actions::ActionInvoker::member(member.clone()),
                None => {
                    return Err(tilo_actions::ApiError::Unavailable {
                        code: "permission_denied".into(),
                        reason: "only a member changes their own records".into(),
                    });
                }
            };
            let arguments = match invocation.argument {
                tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
                tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
                tilo_actions::remote::InvokeArgument::Text(_) => {
                    return Err(tilo_actions::ApiError::InvalidArguments(
                        "a change to a record carries typed arguments".into(),
                    ));
                }
            };
            served.borrow_mut().call_by(
                invoker,
                &invocation.address.resource,
                &invocation.address.action,
                serde_json::Value::Object(arguments),
            )
        }),
    );
    let read = Rc::clone(&provider);
    driver.serve_records(
        own,
        Box::new(move |request| {
            let id = DocumentId::parse(&request.record)
                .ok_or_else(|| format!("`{}` names no record", request.record))?;
            let held = read.try_borrow().map_err(|_| "the app is busy".to_owned())?;
            // The record as the app there holds it now: a step it has not committed yet is
            // committed first, as a save is, so a reader in another workspace sees what the app
            // shows.
            let reading = held.records.replication_snapshot(&id).ok().map(|(document, mut heads)| {
                heads.sort_unstable();
                (
                    heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("."),
                    document,
                )
            });
            Ok(match reading {
                Some((revision, document)) => serde_json::json!({
                    "revision": revision,
                    "records": [{
                        "record": request.record,
                        "document": document.iter().fold(String::new(), |mut text, byte| {
                            let _ = write!(text, "{byte:02x}");
                            text
                        }),
                    }],
                }),
                None => serde_json::json!({
                    "revision": "",
                    "records": [{"record": request.record, "absent": true}],
                }),
            })
        }),
    );
    Ok(Some(provider))
}

/// Stand in for the hosts keeping the records `A` declares kept by them
/// ([`Resources::kept_by`](super::Resources::kept_by)), as the session does:
/// each provider's settings sections are held over the profile's settings
/// document `settings`, and what the window's controls invoke there is
/// changed as `author`'s.
fn serve_kept<A: ResourceApp>(
    driver: &mut Driver,
    settings: &ui_core::storage::SettingsDocument,
    author: &str,
) {
    let mut declared = super::Resources::declaring();
    A::resources(&mut declared);
    let mut providers: Vec<(&'static str, Vec<super::Declare>)> = Vec::new();
    for (provider, declare) in declared.into_kept() {
        match providers.iter_mut().find(|(known, _)| *known == provider) {
            Some((_, kept)) => kept.push(declare),
            None => providers.push((provider, vec![declare])),
        }
    }
    for (provider, kept) in providers {
        let records = super::SettingsResources::in_document(
            super::app::hooks::<A>().id(),
            &kept,
            settings.clone(),
        );
        driver.serve_provider(provider, records.served_for(&MemberId::person(author)));
    }
}

pub(super) fn operation(error: impl std::fmt::Display) -> LaunchError {
    LaunchError::Operation(error.to_string())
}

impl<A: ResourceApp, H: Host> HeadlessApp<A, H> {
    /// The content store the app's profile keeps the bytes its record names
    /// in: what `cx.content()` reads and writes on this device.
    #[must_use]
    pub fn content(&self) -> tilo_content::ContentStore {
        self.profile.content()
    }

    /// The app as its owner opens it on a fresh private profile.
    pub fn new() -> Result<Self, LaunchError> {
        Self::with_role(Role::Owner)
    }

    /// The app as its owner opens it on a fresh private profile, in `workspace`:
    /// one of several windows of the app in one process, each in a workspace of
    /// its own. A workspace is named by a UUID, so a record kept in it has a
    /// route to travel on.
    pub fn in_workspace(workspace: &str) -> Result<Self, LaunchError> {
        Self::fresh(workspace, Role::Owner, AUTHOR)
    }

    /// The app as its owner opens it in `workspace`, in the window of the shell's tab `window`, on
    /// a fresh profile whose records `documents` keeps: the workspace's store over a connection to
    /// Tilo, which the window reads its records from and writes them to as a host with no window
    /// of its own does. A launch over another connection to the same workspace is the app started
    /// again, which finds what this one kept only there: in the same tab, as the shell restores
    /// it, or in a new one, as the shell opens the app again. It writes as `author`, the person
    /// the session runs as, as a window the shell starts does: a member the workspace grants their
    /// own edits.
    ///
    /// # Errors
    /// As any launch.
    pub fn over_workspace(
        documents: Rc<dyn DocumentPort>,
        workspace: &str,
        window: u64,
        author: &str,
    ) -> Result<Self, LaunchError> {
        Self::open(
            Profile::Window(
                Kept::over(documents)?,
                workspace.to_owned(),
                TEST_DEVICE.to_owned(),
                window,
            ),
            Role::Owner,
            author,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app as its owner opens it on a fresh profile on the device
    /// `device`: one of two devices sharing a record, each carrying out what
    /// is queued for it.
    pub fn on_device(device: &str) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), device.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app as a member with `role` opens it on a fresh profile.
    pub fn with_role(role: Role) -> Result<Self, LaunchError> {
        Self::fresh(TEST_WORKSPACE, role, AUTHOR)
    }

    /// The app as `person` with `role` opens it on a fresh private profile,
    /// in `workspace`.
    fn fresh(workspace: &str, role: Role, person: &str) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        Self::open(
            Profile::Temporary(profile, workspace.to_owned(), TEST_DEVICE.to_owned()),
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
    /// fresh profile: one of that person's devices, whose shared records
    /// ([`ResourceEnv::profile_resource`](super::ResourceEnv::profile_resource)) are
    /// theirs.
    pub fn for_person(person: &str) -> Result<Self, LaunchError> {
        Self::fresh(TEST_WORKSPACE, Role::Owner, person)
    }

    /// [`Self::for_person`], with `role` in the workspace: a member who may
    /// only read the workspace still opens their own record of an app each
    /// member keeps their own of.
    pub fn for_person_with_role(person: &str, role: Role) -> Result<Self, LaunchError> {
        Self::fresh(TEST_WORKSPACE, role, person)
    }

    /// [`Self::for_person`], with the app open in the workspace `workspace`
    /// (a workspace id), whose records sync on their routes.
    pub fn for_person_in(person: &str, workspace: &str) -> Result<Self, LaunchError> {
        Self::fresh(workspace, Role::Owner, person)
    }

    /// Open a fresh person profile with a service lent by its mounting host.
    /// The loan follows production `ProcessContext::lend` and is not retained on
    /// reopen: the next host must authorize and lend its own service again.
    pub fn for_person_with_lent<S: 'static>(person: &str, service: S) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            person,
            &[],
            None,
            None,
            None,
            move |context| context.lend(service),
        )
    }

    /// Run `act` on the app as it runs in the person's own workspace with no window, where this
    /// window is in another one and keeps records of theirs there: what holds those records, as
    /// another device of theirs or the app the device starts there. `None` where there is none.
    pub fn in_persons_own_workspace<T>(
        &self,
        act: impl FnOnce(&mut HeadlessApp<A>) -> T,
    ) -> Option<T> {
        self.persons_own
            .as_ref()
            .map(|provider| act(&mut provider.borrow_mut()))
    }

    /// The record `P` every app of the workspace opens that `owner` keeps, as
    /// the app's views open it.
    pub fn shared_workspace<P: Resource>(
        &mut self,
        owner: &'static str,
    ) -> Option<super::WorkspaceResource<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.shared_workspace::<P>(owner))
            .flatten()
    }

    /// The record `id` that `owner` keeps, as the app's views open it.
    pub fn shared_resource<P: Resource>(
        &mut self,
        owner: &'static str,
        id: &str,
    ) -> Option<super::WorkspaceResource<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.shared_resource::<P>(owner, id))
            .flatten()
    }

    /// The member's private workspace record, as the app's views open it.
    pub fn own_workspace_resource<P: Resource>(
        &mut self,
        owner: &'static str,
    ) -> Option<super::WorkspaceResource<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                root.storage.own_workspace_resource::<P>(owner)
            })
            .flatten()
    }

    /// The app's profile record of `P`, as its views open it.
    pub fn profile_resource<P: Resource>(&mut self) -> Option<super::PersonResource<P>> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.storage.profile_resource::<P>())
            .flatten()
    }

    /// The frame `--shots --scenario` writes for `scenario` (`None` for a
    /// plain `--shots`): the app built for a capture on a fresh profile,
    /// from the scenario's fixture, with its actions applied and its view
    /// state staged, at the scenario's viewport.
    pub fn capture(scenario: Option<&str>) -> Result<Self, LaunchError> {
        Self::capture_at(scenario, A::shot_viewport(scenario))
    }

    /// [`Self::capture`] at `viewport`, as `--shots --viewport` draws it.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn capture_at(scenario: Option<&str>, viewport: (u32, u32)) -> Result<Self, LaunchError> {
        let capture = Capture::new(scenario.map(str::to_owned));
        Self::capture_on(capture, viewport, &[])
    }

    /// The window `--headless-drive` drives when it names `scenario`
    /// (`"scenario"` in the spec, or `--scenario NAME` beside it): built and
    /// staged as [`Self::capture`] is, from the scenario's fixtures, but the
    /// actions invoked on it run, as the drive's `invoke` steps run them. It
    /// starts no work on its own, so what it shows stays the scenario's.
    /// A drive that names no scenario drives the app someone uses
    /// ([`Self::new`]).
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn drive(scenario: &str) -> Result<Self, LaunchError> {
        Self::drive_at(scenario, A::shot_viewport(Some(scenario)))
    }

    /// [`Self::drive`] at `viewport`, as [`Self::capture_at`] is a capture's: the scenario's
    /// fixtures staged in a window of the size the test drives. Unlike a capture, the window
    /// moves: a press plays the motion the theme gives it.
    ///
    /// # Errors
    /// As [`Self::capture`].
    pub fn drive_at(scenario: &str, viewport: (u32, u32)) -> Result<Self, LaunchError> {
        let capture = Capture::driven(Some(scenario.to_owned()));
        Self::capture_on(capture, viewport, &[])
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
        let capture = Capture::new(scenario.map(str::to_owned));
        Self::capture_on(capture, A::shot_viewport(scenario), &arguments)
    }

    fn capture_on(
        capture: Capture,
        viewport: (u32, u32),
        arguments: &[String],
    ) -> Result<Self, LaunchError> {
        let scenario = capture.scenario().map(str::to_owned);
        let scenario = scenario.as_deref();
        let profile = Kept::fresh()?;
        let mut app = Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            arguments,
            None,
            None,
            Some((capture, viewport)),
            |_| {},
        )?;
        super::stage::stage::<A>(app.harness(), scenario)?;
        Ok(app)
    }

    /// The app launched by its owner with `arguments`, as `app ARGUMENTS`
    /// on the command line: on the record [`ResourceApp::launch_resource`]
    /// names, after its [`ResourceApp::launch`] actions. The app's own
    /// [options](ResourceApp::OPTIONS) and [switches](ResourceApp::FLAGS)
    /// among them reach it as options.
    pub fn launched(arguments: &[&str]) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
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
    /// record in the same workspace. The production installer validates its
    /// identity before the app can initialize or open it.
    pub fn launched_replica(
        arguments: &[&str],
        document: &mut tilo_resource::Doc,
    ) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        profile.hold_record(super::app::hooks::<A>().id(), TEST_WORKSPACE, document)?;
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &arguments,
            None,
            None,
            None,
            |_| {},
        )
    }

    /// A new window of an app that [keeps one record per workspace](ResourceApp::WORKSPACE_RESOURCE)
    /// on a fresh profile whose workspace record is already `document`: how a
    /// workspace an earlier version of the app wrote opens.
    pub fn with_workspace_resource(document: &mut tilo_resource::Doc) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        profile.hold(
            super::app::hooks::<A>().id(),
            TEST_WORKSPACE,
            &DocumentId::package_primary(),
            document,
        )?;
        Self::open(
            Profile::Window(
                profile,
                TEST_WORKSPACE.to_owned(),
                TEST_DEVICE.to_owned(),
                INSTANCE + 1,
            ),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// A window of the app on a fresh profile in this window's workspace that
    /// holds what this window holds now: its resource and every part of a chain
    /// it keeps beside it, as a member whose device synced them opens the app. The two share
    /// nothing afterwards: what one writes reaches the other through
    /// [`Self::deliver_changes_to`] and [`Self::deliver_record_to`].
    pub fn another_device(&self) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        let workspace = self.profile.workspace().to_owned();
        let app = super::app::hooks::<A>().id();
        let own = &self.checkout().id;
        for checkout in self.records.open_resources() {
            if checkout.id != *own && checkout.id.chain_part().is_none() {
                continue;
            }
            let Some(mut doc) = self.records.read(&checkout.id, Clone::clone) else {
                continue;
            };
            profile.hold(app, &workspace, &checkout.id, &mut doc)?;
        }
        Self::open(
            Profile::Window(profile, workspace, TEST_DEVICE.to_owned(), INSTANCE + 1),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            |_| {},
        )
    }

    /// The app launched with `arguments` to continue `handoff`'s playback,
    /// as the session launches the target of a media handoff.
    pub fn handed_off(arguments: &[&str], handoff: PlaybackHandoff) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
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
        let profile = Kept::fresh()?;
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            Some(file),
            None,
            None,
            |_| {},
        )
    }

    /// The app replaying the recording `file` holds, as the session opens a
    /// recording a report carries: in a private window of its own.
    pub fn replaying(file: OpenFile) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        Self::open(
            Profile::Temporary(profile, TEST_WORKSPACE.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            Some(file),
            None,
            None,
            ProcessContext::provide_private,
        )
    }

    /// The app on the profile `--data-dir directory` opens, as its owner: with
    /// the records an earlier launch on it in this process wrote.
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
        configure: impl FnOnce(&mut ProcessContext),
    ) -> Result<Self, LaunchError> {
        Self::open_beside(
            WindowLaunch {
                profile,
                role,
                author,
                arguments,
                file,
                handoff,
                capture,
            },
            None,
            configure,
        )
    }

    /// [`Self::open`] as a window beside `beside`, another window of the same
    /// profile: both read and write the one store, as a person's windows in
    /// one session do.
    fn open_beside(
        launch: WindowLaunch<'_>,
        beside: Option<&ApplicationOwner>,
        configure: impl FnOnce(&mut ProcessContext),
    ) -> Result<Self, LaunchError> {
        let WindowLaunch {
            profile,
            role,
            author,
            arguments,
            file,
            handoff,
            capture,
        } = launch;
        let mut configure = Some(configure);
        // A capture photographs the window at rest: a surface drawn for one
        // is the surface settled, not the frame of its arrival the moment the
        // shutter opens.
        let settled = capture
            .as_ref()
            .filter(|(capture, _)| !capture.is_driven())
            .map(|_| tilo_ui_base::motion::testing::settled());
        if let ui_core::storage::SettingsDocument::Profile(root) = profile.settings() {
            H::keeps_settings(&root);
        }
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
            beside,
            &mut |context| {
                if let Some(configure) = configure.take() {
                    configure(context);
                }
            },
        )?;
        let mut driver = opened.driver;
        serve_kept::<A>(&mut driver, &opened.profile.settings(), author);
        let persons_own = serve_persons_own::<A>(&mut driver, &opened.profile, author)?;
        H::opened(&mut driver, author);
        Ok(Self {
            driver,
            owner: opened.owner,
            closed: false,
            records: opened.records,
            awaited: Vec::new(),
            checkout: opened.checkout,
            instance: opened.instance,
            profile: opened.profile,
            role,
            author: author.to_owned(),
            last_effect: None,
            workspace: None,
            permissions: opened.permissions,
            _settled: settled,
            persons_own,
            marker: std::marker::PhantomData,
        })
    }

    /// Hand the records the app declares kept by other providers
    /// ([`Resources::kept_by`](super::Resources::kept_by)) to `host`, in place of
    /// the test's own stand-in: a host under test (a page keeping the
    /// person's settings) is told the window's frame, the profile's settings
    /// document and the person at the window, and serves them as it does
    /// for a live window.
    pub fn kept_by_host(
        &mut self,
        host: impl FnOnce(&mut Driver, &ui_core::storage::SettingsDocument, &str),
    ) {
        let mut declared = super::Resources::declaring();
        A::resources(&mut declared);
        for (provider, _) in declared.into_kept() {
            self.driver.stop_serving_provider(provider);
        }
        host(&mut self.driver, &self.profile.settings(), &self.author);
    }

    /// The app's file areas in this profile, which the running app writes
    /// instead of the person's.
    pub fn files(&self) -> ui_core::storage::AppFiles {
        ui_core::storage::AppFiles::at(ui_core::storage::AppPaths::under(
            self.profile.root().join("files"),
            super::app::hooks::<A>().id(),
        ))
    }

    /// The app's typed settings in this profile's settings, which
    /// the running app follows: a write here is what another window, the
    /// Settings app or another of the person's devices writing them does.
    pub fn settings<T: settings_model::SettingsSchema>(&self) -> ui_core::storage::Settings<T> {
        ui_core::storage::Settings::for_app(super::app::hooks::<A>().id())
            .in_document(self.profile.settings())
    }

    /// Change the member's role while the app runs, as the record's
    /// owner changing their access does. Takes effect on the next frame,
    /// which this runs.
    pub fn set_role(&mut self, role: Role) {
        self.permissions.set(PermissionContext::member(
            &tilo_ui_base::permissions::MemberId::person(self.author.as_str()),
            role,
        ));
        self.role = role;
        self.harness().relayout();
    }

    /// The member's permissions in force now.
    #[must_use]
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }

    /// Change the member's permissions while the app runs, as the host
    /// does when the workspace's grants change. Takes effect on the next frame,
    /// which this runs.
    pub fn set_permissions(&mut self, permissions: PermissionContext) {
        self.permissions.set(permissions);
        self.harness().relayout();
    }

    /// Take the member out of the workspace while the app runs: no role and
    /// no grant left, as when the owner removes them.
    pub fn leave_workspace(&mut self) {
        self.permissions.set(PermissionContext::stranger(
            &tilo_ui_base::permissions::MemberId::person(self.author.as_str()),
        ));
        self.harness().relayout();
    }

    /// Close the app (flushing its record) and open it again on the same
    /// profile, as a relaunch would.
    pub fn reopen(self) -> Result<Self, LaunchError> {
        self.relaunch(&[])
    }

    /// Close the app and launch it again on the same profile with
    /// `arguments`, as `app ARGUMENTS` run again would.
    pub fn relaunch(mut self, arguments: &[&str]) -> Result<Self, LaunchError> {
        self.close()?;
        self.launch_again(arguments)
    }

    /// Close the app and open it again on the same profile as `author`: the person who signed in
    /// on the device since it last ran.
    pub fn relaunch_as(mut self, author: &str) -> Result<Self, LaunchError> {
        self.close()?;
        author.clone_into(&mut self.author);
        self.launch_again(&[])
    }

    /// Open the app again without closing it, as a reloaded page does: its window's view is
    /// what the window kept while open, never what a close would save.
    pub fn reload(mut self) -> Result<Self, LaunchError> {
        self.driver.flush_component_resources().map_err(operation)?;
        self.closed = true;
        self.launch_again(&[])
    }

    /// Launch the app on this one's profile with `arguments`, once this one is closed or let go.
    fn launch_again(mut self, arguments: &[&str]) -> Result<Self, LaunchError> {
        let profile = self.profile.clone();
        let role = self.role;
        let author = std::mem::take(&mut self.author);
        // Release this instance's record writer before the next one opens it.
        drop(self);
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        Self::open(profile, role, &author, &arguments, None, None, None, |_| {})
    }

    /// The profile of the next window a launcher opens beside this one.
    fn next_window(&self) -> Result<Profile, LaunchError> {
        let window = self.instance.wrapping_add(1).max(INSTANCE + 1);
        match &self.profile {
            Profile::Temporary(directory, workspace, device)
            | Profile::Window(directory, workspace, device, _) => Ok(Profile::Window(
                Rc::clone(directory),
                workspace.clone(),
                device.clone(),
                window,
            )),
            Profile::DataDir(_) => Err(operation("a --data-dir profile has one window")),
        }
    }

    /// Close the app and open a new window of it on the same profile and in
    /// the same workspace, as the launcher opens one: an instance it never had,
    /// whose fallback record is a fresh record of that window's own unless
    /// the app [keeps one per workspace](ResourceApp::WORKSPACE_RESOURCE).
    pub fn another_window(mut self) -> Result<Self, LaunchError> {
        self.close()?;
        let profile = self.next_window()?;
        let role = self.role;
        let author = std::mem::take(&mut self.author);
        drop(self);
        Self::open(profile, role, &author, &[], None, None, None, |_| {})
    }

    /// Open another window of the app beside this one, on the same profile
    /// and in the same workspace, while this one stays open: two windows a
    /// person has open at once. Its fallback record is the workspace's when the
    /// app [keeps one per workspace](ResourceApp::WORKSPACE_RESOURCE), so both show one
    /// record; [`Self::deliver_changes_to`] carries edits between them.
    pub fn open_window(&self) -> Result<Self, LaunchError> {
        let profile = self.next_window()?;
        Self::open_beside(
            WindowLaunch {
                profile,
                role: self.role,
                author: &self.author,
                arguments: &[],
                file: None,
                handoff: None,
                capture: None,
            },
            Some(&self.owner),
            |_| {},
        )
    }

    /// Deliver this window's changes to the record to `other`, a window
    /// showing the same record, as the workspace's broker fans each window's
    /// document changes out to every other window on the record; `other`
    /// takes them in and draws its next frame, without reopening. Whether
    /// `other` changed.
    pub fn deliver_changes_to(&mut self, other: &mut Self) -> Result<bool, String> {
        if self.checkout().id != other.checkout().id {
            return Err("the windows show different records".into());
        }
        let mut frontier = ui_core::DocumentFrontier::default();
        let (changes, _) = self
            .records
            .replication_delta(&self.checkout().id, &mut frontier)
            .map_err(|error| error.to_string())?;
        let took_in = other
            .records
            .merge_peer_changes(other.checkout(), changes)?;
        other.harness().relayout();
        Ok(took_in)
    }

    /// Deliver what this window holds of `id`, a record the app keeps
    /// beside its own (a part of a chain), to `other`, which holds it too or
    /// takes it from its workspace's hosts ([`Self::asks_the_workspace`]), as
    /// the workspace's record sync sends a device a record it opened: `other`
    /// takes it in and draws its next frame. Whether `other` changed.
    pub fn deliver_resource_to(
        &mut self,
        other: &mut Self,
        id: &DocumentId,
    ) -> Result<bool, String> {
        let held = other
            .records
            .open_resources()
            .into_iter()
            .find(|checkout| checkout.id == *id);
        let Some(checkout) = held else {
            // Not open there (asked for and on its way, or said to be held by no one): the
            // hosts send it whole, it lands in the store the window reads, and the window
            // opens it at its next frame.
            let whole = self
                .records
                .with(id, ui_core::automerge::AutoCommit::save)
                .ok_or_else(|| format!("this window does not hold {id}"))?;
            let kept = other
                .profile
                .kept()
                .ok_or("a --data-dir profile takes nothing from a workspace")?;
            kept.hosts_send(
                definition::<A>().app_id(),
                other.profile.workspace(),
                id,
                &whole,
            )?;
            other.records.note_kept();
            other.harness().relayout();
            return Ok(true);
        };
        let mut frontier = ui_core::DocumentFrontier::default();
        let (changes, _) = self
            .records
            .replication_delta(id, &mut frontier)
            .map_err(|error| error.to_string())?;
        let took_in = other.records.merge_peer_changes(&checkout, changes)?;
        other.harness().relayout();
        Ok(took_in)
    }

    /// Take this window's records to come from its workspace's hosts, as a
    /// page's do ([`ui_core::resource::ResourceStore::ask_workspace_for_missing`]):
    /// a record not here is asked for, and new only once they answered.
    pub fn asks_the_workspace(&mut self) {
        // The profile's store answers what it does not hold of the app as on its way, as a
        // page's hosts do: answered absent, the store would take it as the workspace's word
        // that it holds none of it, and start it.
        if let Some(kept) = self.profile.kept() {
            kept.await_hosts(definition::<A>().app_id());
        }
        self.records.ask_workspace_for_missing();
        // What the window read before it asked (a chain's newest part, opened with its first
        // frame) took a record not here as one the workspace holds none of: read again.
        self.records.note_kept();
    }

    /// What this window asks its workspace's hosts for
    /// ([`ui_core::resource::ResourceStore::wanted_resources`]).
    #[must_use]
    pub fn wanted_resources(&self) -> Vec<DocumentId> {
        let mut wanted = self.records.wanted_resources();
        for store in &self.awaited {
            wanted.extend(store.wanted_resources());
        }
        wanted
    }

    /// The workspace's hosts sent everything they hold of `id`, a record this
    /// window asked for, or none is connected to ask
    /// ([`ui_core::resource::ResourceStore::note_workspace_answered`]).
    pub fn workspace_answered(
        &mut self,
        id: DocumentId,
        answer: ui_core::resource::WorkspaceAnswer,
    ) {
        if let Some(kept) = self.profile.kept() {
            kept.hosts_answered(&id, answer);
        }
        for store in &self.awaited {
            store.note_workspace_answer(id.clone(), answer);
        }
        self.records.note_workspace_answer(id, answer);
    }

    /// The app as its owner opens it in `workspace` on a device that holds nothing yet of
    /// what `keeper`, another app, keeps for that workspace, and takes it from the
    /// workspace's holders as a page does: each such document is on its way from the
    /// window's first view on, until the workspace delivers it
    /// ([`Self::workspace_delivers`]) or answers that it holds none of it
    /// ([`Self::workspace_answered`]).
    ///
    /// # Errors
    /// As any launch; and a window that opens no documents of its workspace.
    pub fn awaiting_the_workspace(
        workspace: &str,
        keeper: &'static str,
    ) -> Result<Self, LaunchError> {
        let profile = Kept::fresh()?;
        let hosts = Rc::clone(&profile);
        let asked = Rc::new(std::cell::RefCell::new(None));
        let asking = Rc::clone(&asked);
        let mut app = Self::open(
            Profile::Temporary(profile, workspace.to_owned(), TEST_DEVICE.to_owned()),
            Role::Owner,
            AUTHOR,
            &[],
            None,
            None,
            None,
            move |context| {
                let store = context
                    .workspace_resources()
                    .and_then(|beside| beside.store_of(keeper).ok());
                if let Some(store) = &store {
                    // What the window opens of `keeper`'s from here on is the hosts' to send.
                    hosts.await_hosts(keeper);
                    store.ask_workspace_for_missing();
                }
                *asking.borrow_mut() = store;
            },
        )?;
        let store = asked
            .borrow_mut()
            .take()
            .ok_or_else(|| operation("the window opens no documents of its workspace"))?;
        app.awaited.push(store);
        Ok(app)
    }

    /// The workspace delivers the document `P` that `keeper` keeps for it, the first of it
    /// this device gets, as `write` leaves it: another writer on this device's store founds
    /// it and applies `write`, as the workspace's replication keeps what its holders sent
    /// of a document no window has open, and the stores this window awaits `keeper`'s
    /// documents in ([`Self::awaiting_the_workspace`]) are told that something arrived.
    /// Nothing else is asked of the window: no frame is laid out or forced, so what it
    /// shows of the document is what its own frames ([`Self::frame`]) take up.
    ///
    /// # Errors
    /// The document does not open in the device's store, or `write` refuses.
    pub fn workspace_delivers<P: Resource>(
        &mut self,
        keeper: &'static str,
        write: impl FnOnce(&super::WorkspaceResource<P>) -> Result<(), String>,
    ) -> Result<(), String> {
        let id = ui_core::resource::workspace::document_id(P::KIND);
        self.workspace_delivers_record::<P>(keeper, &id, write)
    }

    /// [`Self::workspace_delivers`] of the record `id` that `keeper` keeps, for a record of the
    /// app's that the workspace names otherwise than by its kind (Terminal's machine secrets).
    ///
    /// # Errors
    /// As [`Self::workspace_delivers`]; and `id` names no record.
    pub fn workspace_delivers_record<P: Resource>(
        &mut self,
        keeper: &'static str,
        id: &str,
        write: impl FnOnce(&super::WorkspaceResource<P>) -> Result<(), String>,
    ) -> Result<(), String> {
        let kept = self
            .profile
            .kept()
            .ok_or("a --data-dir profile takes nothing from a workspace")?;
        // The writer standing for the hosts founds it, which only a document they hold none of
        // is; what it writes is then held, and read as sent.
        if let Some(record) = DocumentId::parse(id) {
            kept.hosts_answered(&record, ui_core::resource::WorkspaceAnswer::HoldsNone);
        }
        // The same store the window's own reads the profile's records from, written once more:
        // what lands there is what the window's stores read.
        let theirs = ui_core::resource::workspace::WorkspaceResources::in_workspace(
            ui_core::resource::ResourceOwners::writing(kept.records()),
            self.profile.workspace().to_owned(),
            MemberId::person(self.author.as_str()),
        );
        let document = super::WorkspaceResource::<P>::open_at(
            &theirs,
            keeper,
            id,
            (keeper, keeper),
            None,
            None,
        )
        .ok_or_else(|| format!("{} does not open in the device's store", P::KIND))?;
        write(&document)?;
        // It has landed on the device before the window is told of it.
        theirs
            .store_of(keeper)
            .map_err(|error| error.to_string())?
            .flush()?;
        drop(document);
        for store in &self.awaited {
            store.note_kept();
        }
        Ok(())
    }

    /// Open what a launch naming `arguments` opens in this window, as the
    /// app's view does with [`ResourceEnv::open_here`](super::ResourceEnv::open_here),
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
            .ok_or_else(|| format!("{} is not hosted here", super::app::hooks::<A>().title()))??;
        self.harness().relayout();
        outcome
            .get()
            .unwrap_or_else(|| Err("the host did not take the request".into()))
    }

    pub fn record(&self) -> &DocumentId {
        &self.checkout().id
    }

    /// The person this window is open for, by their id: whose steps what it changes are.
    #[must_use]
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The document the window shows now: the one it was opened on, or one
    /// its view opened in its place ([`ResourceEnv::open_here`](super::ResourceEnv::open_here)).
    pub fn checkout(&self) -> &DocumentCheckout {
        self.driver.component_checkout().unwrap_or(&self.checkout)
    }

    /// Consume the hosted window's explicit save-retry request, as its
    /// record owner does before reporting durability again.
    pub fn take_save_retry(&mut self) -> Result<bool, String> {
        self.harness()
            .with_widget_mut(|root: &mut super::root::ResourceRoot| {
                super::root::RootApp::take_save_retry(root)
            })
            .ok_or_else(|| "the framework root is not mounted".to_owned())
    }

    /// Make every edit so far durable, as closing the app does.
    pub fn flush(&mut self) -> Result<(), LaunchError> {
        self.harness()
            .flush_component_resources()
            .map_err(operation)
    }

    fn close(&mut self) -> Result<(), LaunchError> {
        if !self.closed {
            self.driver.flush_component_resources().map_err(operation)?;
            self.driver.close_components();
            self.closed = true;
        }
        if let Some(persons_own) = &self.persons_own {
            persons_own.borrow_mut().close()?;
        }
        Ok(())
    }

    /// The custom surface of type `S` the view hosts, without running a
    /// frame: for a test reading it, or setting it up, between the events it
    /// sends.
    #[allow(
        clippy::expect_used,
        reason = "a test naming a surface its app does not host is wrong"
    )]
    pub fn surface_mut<S: tilo_ui_base::widget::Widget + 'static>(&mut self) -> &mut S {
        surface_in::<A, S>(self.harness()).expect("the view hosts this surface")
    }

    /// The display-less driver the app runs on, for input-level tests
    /// (`click_named`, `type_text`, `targets`).
    pub fn harness(&mut self) -> &mut Driver {
        &mut self.driver
    }

    /// Every text the window shows now, as assistive technology reads it:
    /// what the app's view drew of its state.
    pub fn texts(&mut self) -> Vec<String> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| tilo_ui_base::a11y::texts(root.app_view()))
            .unwrap_or_default()
    }

    /// The records this window's store has open: what the app process opened
    /// itself, which an app the session serves opens none of.
    pub fn open_resources(&self) -> Vec<DocumentId> {
        self.records
            .open_resources()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect()
    }

    /// The whole document of `id` as this window holds it: its own record,
    /// or one it keeps beside it (a part of a chain).
    pub fn resource_document(&self, id: &DocumentId) -> Option<Vec<u8>> {
        self.records.read(id, |doc| doc.clone().save())
    }

    /// Read the document of `id` as this window holds it, without a copy:
    /// its own record, or one it keeps beside it (a part of a chain).
    pub fn read_document_of<T>(
        &self,
        id: &DocumentId,
        read: impl FnOnce(&tilo_resource::Doc) -> T,
    ) -> Option<T> {
        self.records.read(id, read)
    }

    /// Submit `submit` through the running view's own dispatcher, as a
    /// control's press or a field's edit hands it over: over the session's
    /// connection when the app is the session's, into the record here when
    /// it is not.
    pub fn submit(&mut self, submit: impl super::Submit<A::Resource>) -> Result<(), String> {
        let refused = self
            .driver
            .with_widget_mut(|root: &mut ResourceRoot| {
                let Some(typed) = root.typed::<A::Resource>() else {
                    return Err("the window's root is another resource type".to_owned());
                };
                typed.dispatch.submit(submit);
                Ok(typed.dispatch.refused().get().map(|refused| refused.reason))
            })
            .ok_or_else(|| "the window's root is not reachable".to_owned())?
            .and_then(|refused| refused.map_or(Ok(()), Err));
        self.settle();
        refused
    }

    /// Visit the custom surface of type `S` the app's view hosts
    /// ([`ViewCx::surface`](super::ViewCx::surface)), as the frame shows it
    /// now; `None` when the view hosts none. The frame is laid out again
    /// after the visit, so what the visit changed is what the next frame
    /// draws.
    pub fn with_surface<S: tilo_ui_base::widget::Widget + 'static, T>(
        &mut self,
        visit: impl FnOnce(&mut S) -> T,
    ) -> Option<T> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                super::root::with_surface::<A::Resource, S, T>(root, visit)
            })
            .flatten()
    }

    /// Invoke a declared action through the host API, exactly as a control,
    /// the palette, an agent or a script does.
    pub fn invoke(&mut self, invoke: Invoke<A::Resource>) -> Result<(), tilo_actions::ApiError> {
        self.invoke_as(invoke, false)
    }

    /// Invoke an action of the resource nested in this one at `resource` —
    /// a child resource a view edits through [`Dispatcher::child`](super::Dispatcher::child)
    /// — through the host API, as the palette, an agent or a script
    /// addresses it.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_child<C: Resource>(
        &mut self,
        resource: &str,
        invoke: Invoke<C>,
    ) -> Result<(), tilo_actions::ApiError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::ApiError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = tilo_actions::ActionAddress::new(resource, id);
        self.harness().invoke_action(&address, &arguments, None)?;
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
    ) -> Result<(), tilo_actions::ApiError> {
        self.invoke_as(invoke, true)
    }

    /// Discover the actions this member may invoke on the running app,
    /// including item and child resources. Descriptors include the complete
    /// parameter, permission, confirmation and result contracts; discovery
    /// never invokes an action. Addresses stay stable across relayouts.
    ///
    /// A saved descriptor is not an authority grant: every call checks the
    /// running app's current permissions and resource state again.
    pub fn actions(
        &mut self,
    ) -> Result<Vec<tilo_actions::ActionDescriptor>, tilo_actions::ApiError> {
        let instance = self.instance;
        let resources = self.harness().bound_resources_of(Some(instance), None)?;
        let mut actions = tilo_actions::api::survey(&resources);
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
        let mut resources = self.harness().resources();
        // Another provider publishes what it runs, not this window.
        resources.iter_mut().for_each(published_here);
        let mut rows: Vec<serde_json::Value> = tilo_actions::api::survey(&resources)
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
    /// API, as an agent or script does, and answer with what it produced;
    /// `None` while it is still running.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn operate(
        &mut self,
        invoke: Invoke<A::Resource>,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::ApiError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::ApiError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id);
        let started = self.harness().invoke_action(&address, &arguments, None)?;
        Ok(started.completed())
    }

    /// Invoke one of the app's effects through the host API, as a button,
    /// the palette or an agent does, and wait for its answer: what it
    /// reported at once, or the result of the job it started. Progress the
    /// job reported is in [`Driver::job_updates`].
    pub fn effect<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        self.effect_as(
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
            false,
        )
    }

    /// [`Self::effect`] invoked by `actor` — an agent or a member the
    /// host admitted for the effect — rather than the person at this
    /// device: what the effect submits is applied as theirs, and only as
    /// far as the effect they were admitted for allows.
    ///
    /// # Errors
    /// As [`Self::effect`]; the refusal of an edit the effect submitted
    /// fails it.
    pub fn effect_by<E: Effect>(
        &mut self,
        actor: &MemberId,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        self.effect_invoked(
            Some(tilo_actions::ActionInvoker::member(actor.clone())),
            <A::Resource as Resource>::KIND,
            effect.id(),
            &effect.arguments(),
            false,
        )
    }

    /// Any published action of `resource`, by id with JSON arguments,
    /// delivered for `invoker` as the host delivers an admitted execution's
    /// invocation: what it builds is bound to the invoker's own member
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
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::ApiError::InvalidArguments(
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
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
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
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
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
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        self.effect_as(row, effect.id(), &serde_json::Map::new(), true)
    }

    /// Start `effect` through the host API without waiting for it, as a
    /// caller that may cancel it does, confirmed: the invocation its job
    /// runs under.
    ///
    /// # Errors
    /// The host's refusal, the effect's failure, or that it answered at
    /// once and so started no job.
    pub fn start_effect<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::InvocationId, tilo_actions::ApiError> {
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
    ) -> Result<tilo_actions::InvocationId, tilo_actions::ApiError> {
        self.start_effect_at(row, effect.id(), &serde_json::Map::new())
    }

    fn start_effect_at(
        &mut self,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::InvocationId, tilo_actions::ApiError> {
        let address = tilo_actions::ActionAddress::new(resource, id);
        let driver = self.harness();
        let approval = driver.approval();
        let (invocation, started) =
            driver.start_bound_job_for(None, &address, arguments, Some(&approval))?;
        self.last_effect = Some(invocation);
        if started.completed().is_some() {
            return Err(tilo_actions::ApiError::Failed(format!(
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
        self.harness().job_updates(invocation)
    }

    fn effect_as(
        &mut self,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        self.effect_invoked(None, resource, id, arguments, confirmed)
    }

    fn effect_invoked(
        &mut self,
        invoker: Option<tilo_actions::ActionInvoker>,
        resource: &str,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        let address = tilo_actions::ActionAddress::new(resource, id);
        // What the action declares it answers, checked as the broker checks
        // it: an answer outside it fails here as it fails live.
        let declared = self
            .harness()
            .resources()
            .into_iter()
            .find(|candidate| candidate.id.as_str() == resource)
            .and_then(|found| found.actions.into_iter().find(|spec| spec.id == id))
            .map(|spec| spec.contract.result.clone());
        let within = move |output: tilo_actions::ActionOutput| match &declared {
            Some(declared) => declared
                .validate(&output)
                .map(|()| output)
                .map_err(|error| {
                    tilo_actions::ApiError::Failed(format!(
                        "`{id}` answered outside the result it declares: {error}"
                    ))
                }),
            None => Ok(output),
        };
        let harness = self.harness();
        let approval = confirmed.then(|| harness.approval());
        let (invocation, started) =
            harness.start_bound_job_for(invoker, &address, arguments, approval.as_ref())?;
        self.last_effect = Some(invocation);
        let driver = self.harness();
        if let Some(output) = started.completed() {
            driver.relayout();
            return within(output);
        }
        match driver.await_job(invocation, ui_core::time::Duration::from_secs(30)) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => within(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(tilo_actions::ApiError::Failed(error.message))
            }
            Some(_) => Err(tilo_actions::ApiError::Failed("cancelled".into())),
            None => Err(tilo_actions::ApiError::Failed(format!(
                "`{id}` did not finish"
            ))),
        }
    }

    /// Run `visit` on the app's widget of type `T` — a custom surface, a
    /// widget its view built — for assertions about view state the
    /// accessibility tree does not show, then run the frame after it.
    /// `None` when the view has no such widget.
    pub fn with_widget<T: tilo_ui_base::widget::Widget + 'static, O>(
        &mut self,
        visit: impl FnOnce(&mut T) -> O,
    ) -> Option<O> {
        self.harness()
            .with_widget_mut(|root: &mut super::root::ResourceRoot| {
                tilo_ui_base::widget::find_mut::<T>(root.content.as_mut()).map(visit)
            })
            .flatten()
    }

    /// Every presence this window shows besides its person, and where.
    pub fn presences(&mut self) -> Vec<tilo_ui_base::widget::ViewPresence> {
        let doc = self
            .read_document_of(&self.checkout().id, Clone::clone)
            .unwrap_or_default();
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                tilo_ui_base::widget::presence_topology::Presences::presences(root, &doc)
            })
            .unwrap_or_default()
    }

    /// What the system's persistent search index keeps of this window now:
    /// the entries the app publishes to it, found after it closes too.
    pub fn search_index_entries(&mut self) -> Vec<ui_core::search::index_publication::IndexEntry> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                super::root::RootSearch::search_index_entries(root)
            })
            .unwrap_or_default()
    }

    /// Hand the window the presentation it kept (`payload` at `version`),
    /// as a host reopening it does, at the moment the test picks: after an
    /// `open` already arrived, say.
    pub fn restore_kept_view(&mut self, version: u32, payload: &[u8]) -> Result<(), String> {
        let local = ui_core::application_view::ApplicationLocalView {
            app_id: A::ID.to_owned(),
            version,
            payload: payload.to_vec(),
        };
        let doc = ui_core::automerge::AutoCommit::new();
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| {
                super::root::RootApp::restore_application_view(root, &local, &doc)
            })
            .ok_or_else(|| "the window has no resource root".to_owned())?
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// The last action refused here ([`super::Dispatcher::refused`]): by
    /// the API, or by the workspace after this window applied it.
    pub fn refused(&mut self) -> Option<super::Refused> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.dispatch.refused.get())
            .flatten()
    }

    /// The workspace refused `changes` (Automerge hashes) of this window's
    /// record, `reason` what a person is told (a host replicating the
    /// record reads it from the workspace's words with
    /// `tilo_server_protocol::refusal::Refusal::plain`): withdraw them and what
    /// was built on them, as that host does, and run the frame that tells
    /// the app. Whether anything was withdrawn.
    pub fn refused_by_workspace(
        &mut self,
        changes: &[[u8; 32]],
        reason: &str,
    ) -> Result<bool, String> {
        let refused: Vec<_> = changes
            .iter()
            .map(|hash| ui_core::automerge::ChangeHash(*hash))
            .collect();
        let withdrawn = self
            .records
            .withdraw_refused(&self.checkout.id, &refused, reason)?;
        self.harness().relayout();
        self.settle();
        Ok(withdrawn.is_some())
    }

    /// What the workspace kept that the app said it did
    /// ([`super::Dispatcher::say_when_kept`]): the last such text.
    pub fn said(&mut self) -> Option<String> {
        self.harness()
            .with_widget_mut(|root: &mut ResourceRoot| root.dispatch.said.get())
            .flatten()
    }

    /// From now on a host replicates this window's record to a workspace that
    /// decides its changes, as a browser page in a workspace does: what the app
    /// says it did waits for the workspace ([`Self::workspace_publishes`],
    /// [`Self::workspace_keeps`]).
    pub fn shared_in_workspace(&mut self) -> Result<(), String> {
        let (_, heads) = self
            .records
            .replication_snapshot(&self.checkout.id)
            .map_err(|error| error.to_string())?;
        let mut frontier = ui_core::state::DocumentFrontier::default();
        frontier.advance(heads);
        self.workspace = Some(frontier);
        self.records.workspace_settled(&self.checkout.id);
        Ok(())
    }

    /// The host publishes what changed since it last did, as its next pass
    /// does; whether anything was published.
    pub fn workspace_publishes(&mut self) -> Result<bool, String> {
        self.settle();
        let frontier = self
            .workspace
            .as_mut()
            .ok_or("the record is not shared in a workspace")?;
        let (delta, heads) = self
            .records
            .replication_delta(&self.checkout.id, frontier)
            .map_err(|error| error.to_string())?;
        frontier.advance(heads);
        Ok(!delta.is_empty())
    }

    /// The workspace answered every commit the host published, keeping them,
    /// and the frame that tells the app runs.
    pub fn workspace_keeps(&mut self) {
        self.records.workspace_settled(&self.checkout.id);
        self.harness().relayout();
        self.settle();
    }

    /// Run the frame the app runs after an event: edits the views and
    /// effects submitted are applied, and what they change is drawn. Every
    /// component syncs and settles, as after an event that reached it: a
    /// test may have changed what its views read past the framework
    /// ([`tilo_ui_base::component_inputs`]).
    pub fn settle(&mut self) {
        // The app in the person's own workspace keeps their records: settle it first, so what
        // this window's call changed there is committed, and the readings this window takes
        // below see it.
        if let Some(persons_own) = &self.persons_own {
            persons_own.borrow_mut().driver.persist_tick();
        }
        // The frame takes up what the event brought, the record's writer is offered what it
        // changed, and the frame after takes up what that answered.
        let driver = self.harness();
        driver.relayout();
        driver.persist_tick();
        driver.relayout();
    }

    /// What the live window does once input stops, and no more: what its
    /// components' workers woke them for taken up, the frame the input
    /// asked for, control invocations dispatched and edits offered to the
    /// record's writer ([`Driver::idle`]). Unlike [`Self::settle`] it
    /// forces no frame to sync and publish, so it costs what the live window
    /// costs: what the perf measurements run after each interaction.
    pub fn idle(&mut self) {
        self.harness().idle();
    }

    /// One live frame: the frame the window asked for, and one drawn now when none did.
    pub fn frame(&mut self) {
        if !self.frame_if_owed() {
            self.harness().relayout();
        }
    }

    /// The frame the window draws when something asks for one, and none when nothing does.
    pub fn frame_if_owed(&mut self) -> bool {
        self.harness().poll_frame().unwrap_or(false)
    }

    /// Invoke any published action of the resource by id with JSON
    /// arguments, as an agent's `tilo_do` does — the framework's own
    /// (`undo`, `history`, `render`) as well as the app's — and answer with
    /// what it reported.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call(
        &mut self,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
        self.call_within(None, id, arguments)
    }

    /// [`Self::call`] as one agent session: every call naming the same
    /// `session` belongs to it, the way an agent's connection groups its
    /// invocations. The session was asked nothing, so it has no label: history
    /// names it by its id alone.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_in_run(
        &mut self,
        session: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
        let ambient = tilo_actions::ActionContext {
            group: Some(tilo_actions::WorkGroup {
                id: session.to_owned(),
                label: None,
                kind: ui_core::history_source::steps::AGENT_SESSION.to_owned(),
                declared_id: None,
                session: None,
            }),
            ..tilo_actions::ActionContext::default()
        };
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
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
        self.call_at(None, resource, id, arguments, false)
    }

    /// [`Self::call_on`], waiting for the job the action started, as
    /// [`Self::effect`] does: what it finished with.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn call_on_finished(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        self.effect_invoked(None, resource, id, &arguments, false)
    }

    /// [`Self::call_on`], answering the whole output: the value and the
    /// resources the call produced, such as the items a verb created.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_on_output(
        &mut self,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::ApiError> {
        self.call_output_at(None, resource, id, arguments, false)
    }

    /// What the person is asked right now — a press of theirs whose action waits for their yes,
    /// from a button, a menu, the palette or an immediate application — and the action's name
    /// its card shows. `None` while nothing is asked.
    pub fn asking(&mut self) -> Option<(tilo_actions::ActionAddress, String)> {
        self.settle();
        self.harness().asking()
    }

    /// Allow what the person is asked, as they do on its card, and settle: the call comes back
    /// with their yes and the action applies. Answers the action allowed; `None`, changing
    /// nothing, when nothing was asked. A test of a delete from a menu clicks the menu item,
    /// then calls this.
    pub fn confirm_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        // What the last event submitted reaches the host first.
        self.settle();
        let allowed = self.harness().confirm_asked();
        self.settle();
        allowed
    }

    /// Decline what the person is asked, as they do on its card, and settle: nothing of it runs.
    /// Answers the action declined; `None` when nothing was asked.
    pub fn decline_asked(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.settle();
        let declined = self.harness().decline_asked();
        self.settle();
        declined
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
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
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
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
        self.call_at(None, resource, id, arguments, true)
    }

    fn call_within(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
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
    ) -> Result<Option<serde_json::Value>, tilo_actions::ApiError> {
        self.call_output_at(ambient, resource, id, arguments, confirmed)
            .map(|output| output.and_then(|output| output.value))
    }

    fn call_output_at(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        resource: &str,
        id: &str,
        arguments: serde_json::Value,
        confirmed: bool,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::ApiError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::ApiError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        let address = tilo_actions::ActionAddress::new(resource, id);
        let driver = self.harness();
        // A confirmed call is sent again with the person's approve, as the device's ask sends it.
        let approval = confirmed.then(|| driver.approval());
        let started = match ambient {
            Some(ambient) => {
                driver.invoke_action_within(&address, &arguments, approval.as_ref(), ambient)?
            }
            // A call the app hands its peer before running it answers once that comes back: a
            // caller that asked for a value waits for it, as an agent's call waits for its
            // receipt.
            None => driver.invoke_action_answering(&address, &arguments, approval.as_ref())?,
        };
        Ok(started.completed())
    }

    fn invoke_as(
        &mut self,
        invoke: Invoke<A::Resource>,
        confirmed: bool,
    ) -> Result<(), tilo_actions::ApiError> {
        if let Some(refusal) = invoke.refusal() {
            return Err(tilo_actions::ApiError::Failed(refusal.to_owned()));
        }
        let Invoke { id, arguments, .. } = invoke;
        let address = tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id);
        let driver = self.harness();
        let approval = confirmed.then(|| driver.approval());
        driver.invoke_action(&address, &arguments, approval.as_ref())?;
        Ok(())
    }

    /// The parts of the chain of `P` the app's store holds, oldest first,
    /// each read as the window reads it: what the app keeps beside its own
    /// document, with everything a part loads of it (an event's notes
    /// among it).
    pub fn chain_parts<P: super::Chained>(&self) -> Vec<(u64, std::rc::Rc<P::State>)> {
        let chain = P::chain_beside(&self.checkout().id);
        self.records
            .held_parts(&chain)
            .into_iter()
            .filter_map(|index| {
                let state = self
                    .records
                    .read(&chain.part(index), |doc| {
                        tilo_resource::Reader::<P>::read_all(doc)
                    })?
                    .ok()?;
                Some((index, std::rc::Rc::clone(state.state())))
            })
            .collect()
    }

    /// Apply `invoke` to the chain of `P` beside the record the app shows,
    /// as the window does when a person, an agent or the palette asks: what a
    /// test changes of what the app keeps in parts.
    ///
    /// # Errors
    /// Why the chain cannot be opened here, or why the action was refused.
    pub fn chain_invoke<P: super::Chained<Base = A::Resource>>(
        &mut self,
        invoke: &Invoke<P>,
    ) -> Result<(), String> {
        let base = self.checkout().id.clone();
        self.chain_invoke_beside(&base, invoke).map(drop)
    }

    /// Apply an action beside an earlier base record in the same workspace.
    pub fn chain_invoke_beside<P: super::Chained<Base = A::Resource>>(
        &mut self,
        id: &DocumentId,
        invoke: &Invoke<P>,
    ) -> Result<Option<u64>, String> {
        let me = tilo_resource::MemberId::person(&self.author);
        let base = Rc::new(self.state());
        let chain = super::Chain::<P>::detached_beside_in(
            self.records.clone(),
            <A as super::ResourceApp>::ID,
            Some(id),
            &me,
            move || Rc::clone(&base),
        )
        .ok_or_else(|| format!("the {} cannot be opened", P::LABEL))?;
        chain.invoke(invoke).map_err(|error| error.to_string())
    }

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
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces a closed record loudly"
    )]
    pub fn read_document<T>(&self, read: impl FnOnce(&tilo_resource::Doc) -> T) -> T {
        self.records
            .read(&self.checkout().id, read)
            .expect("record is open")
    }

    /// A copy of the stored document written by another member: a
    /// member's replica, for a test to edit and [`Self::merge_peer`]
    /// back.
    pub fn peer_document(&self) -> tilo_resource::Doc {
        self.read_document(|doc| doc.clone().fork())
    }

    /// Exchange edits with a member's replica, as sync does both ways:
    /// the peer's edits arrive in the running app (and its views), and the
    /// app's edits arrive in `peer`.
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

    /// A copy of `id`, a record the app keeps beside its own (a part of a
    /// chain), written by another member: their's replica, for a
    /// test to edit and [`Self::merge_peer_document_of`] back. `None` while the
    /// record is not open here.
    pub fn peer_document_of(&self, id: &DocumentId) -> Option<tilo_resource::Doc> {
        // What the window has written and not committed yet is its own,
        // committed here, before the member's replica starts.
        self.records.with(id, tilo_resource::Doc::fork)
    }

    /// [`Self::merge_peer`] for `id`, a record the app keeps beside its own
    /// (a part of a chain): the peer's edits arrive in the running app, and
    /// the app's edits arrive in `peer`.
    pub fn merge_peer_document_of(
        &mut self,
        id: &DocumentId,
        peer: &mut tilo_resource::Doc,
    ) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming;
        let checkout = self
            .records
            .open_resources()
            .into_iter()
            .find(|open| open.id == *id)
            .ok_or("the record is not open here")?;
        let mut ours = self
            .records
            .read(id, Clone::clone)
            .ok_or("the record is not open here")?;
        peer.merge_incoming(&mut ours)
            .map_err(|error| error.to_string())?;
        self.records.merge_peer_document(&checkout, peer.save())?;
        self.harness().relayout();
        Ok(())
    }

    /// Another replica's change to `record`, a record open in the app's
    /// store (its own, or a part of a chain beside it), arriving as a workspace's
    /// replication delivers it: `edit` writes on that replica as it stands
    /// now, under its own actor, and the change is merged into the store.
    /// Nothing else runs: no frame is laid out or forced, so what the window
    /// shows of it is what its own frames ([`Self::frame`]) take up, as for
    /// a person who is only watching. Whether the record changed.
    pub fn replica_arrives(
        &mut self,
        record: &DocumentId,
        edit: impl FnOnce(&mut tilo_resource::Doc),
    ) -> Result<bool, String> {
        let checkout = self
            .records
            .open_resources()
            .into_iter()
            .find(|open| open.id == *record)
            .ok_or("the record is not open here")?;
        let mut replica = self
            .records
            .read(record, |doc| doc.clone().fork())
            .ok_or("the record is not open here")?;
        let before = replica.get_heads();
        edit(&mut replica);
        replica.commit();
        let change = replica.save_after(&before);
        self.records
            .merge_replica(
                &checkout,
                tilo_ui_base::collaboration::DocumentMutation::Incremental(change),
            )
            .map(|(changed, _)| changed)
    }

    /// The record's version history.
    pub fn versions(&self) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(&self.checkout().id, |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
    }

    /// What each version of the newest part of the chain of `P` did through
    /// an action, oldest first: what the parts' history says, as
    /// [`Self::history`] says it of the record.
    pub fn chain_history<P: super::Chained>(&self) -> Vec<tilo_actions::ActionContext> {
        let chain = P::chain_beside(&self.checkout().id);
        let Some(head) = self.records.held_parts(&chain).last().copied() else {
            return Vec::new();
        };
        self.records
            .read(&chain.part(head), |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
            .into_iter()
            .filter_map(|version| version.context)
            .collect()
    }

    /// What each version did through an action, oldest first.
    pub fn history(&self) -> Vec<tilo_actions::ActionContext> {
        self.versions()
            .into_iter()
            .filter_map(|version| version.context)
            .collect()
    }

    /// What each version of the resource `id` did through an action, oldest
    /// first: a resource the app keeps beside its own, such as a part of a
    /// chain, as this window holds it.
    pub fn history_of(&self, id: &DocumentId) -> Vec<tilo_actions::ActionContext> {
        self.versions_of(id)
            .into_iter()
            .filter_map(|version| version.context)
            .collect()
    }

    /// The versions of the record `id`, oldest first ([`Self::history_of`]).
    pub fn versions_of(&self, id: &DocumentId) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(id, |doc| tilo_resource::history::versions(&mut doc.clone()))
            .unwrap_or_default()
    }

    /// The sentences History shows for `entries` in en-US, through the app's
    /// own catalogue.
    pub fn sentences<'e>(
        &self,
        entries: impl IntoIterator<Item = &'e tilo_actions::ActionContext>,
    ) -> Vec<String> {
        let source = A::catalog().and_then(tilo_ui_base::i18n::EmbeddedCatalog::source);
        let catalogue = tilo_history::Catalogue::new("en-US", &Vec::from_iter(source));
        let reader = tilo_history::Reader::new("en-US")
            .with_app(&catalogue)
            .with_labels(&ui_core::history_source::sentence::ThreadLabels);
        entries
            .into_iter()
            .map(|entry| reader.sentence(entry))
            .collect()
    }

    /// The version at `heads` as History mode previews it: the app's own
    /// view, read-only, over the document as it was then.
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

    /// A member's edit arriving: `edit` writes on their own replica of
    /// the record as it stands now, under their own actor, and the change is
    /// merged in as sync merges a peer's. It is not the local member's
    /// action, so it never lands on their undo stack.
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

    /// Undo the local member's last action, as Ctrl+Z does.
    pub fn undo(&mut self) -> bool {
        let driver = self.harness();
        let undoes = driver.undo_status().can_undo;
        driver.perform_undo();
        undoes
    }

    /// Redo, as Ctrl+Shift+Z does.
    pub fn redo(&mut self) -> bool {
        let driver = self.harness();
        let redoes = driver.undo_status().can_redo;
        driver.perform_redo();
        redoes
    }
}

/// The custom surface of type `S` that `A`'s view hosts in `driver`,
/// without running a frame (see [`super::testing::with_surface`]).
pub fn surface_in<A: ResourceApp, S: tilo_ui_base::widget::Widget + 'static>(
    driver: &mut Driver,
) -> Option<&mut S> {
    surface_of::<A, S>(driver.widget_mut::<ResourceRoot>()?)
}

pub(super) fn surface_of<A: ResourceApp, S: tilo_ui_base::widget::Widget + 'static>(
    root: &mut ResourceRoot,
) -> Option<&mut S> {
    root.typed::<A::Resource>()?;
    tilo_ui_base::widget::find_mut::<S>(root.content.as_mut())
}

impl<A: ResourceApp, H: Host> Drop for HeadlessApp<A, H> {
    #[expect(
        clippy::panic,
        reason = "a test window that loses what it wrote fails its test"
    )]
    fn drop(&mut self) {
        // A test that failed is unwinding: close the app without running
        // its frames again, so whatever failed it cannot fail closing too
        // and abort the whole test binary.
        if std::thread::panicking() {
            if !self.closed {
                self.closed = true;
                self.driver.close_components();
            }
            return;
        }
        // A window that could not keep what it wrote has lost it: the test that closed it
        // fails rather than passing over the loss.
        if let Err(error) = self.close() {
            panic!("the window closed without keeping what it wrote: {error}");
        }
    }
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
    launch_resource: fn(&LaunchInput<'_>) -> Option<DocumentId>,
    shot_viewport: fn(Option<&str>) -> (u32, u32),
}

impl Opening {
    fn of<A: ResourceApp>() -> Self {
        Self {
            definition: definition::<A>(),
            id: super::app::hooks::<A>().id(),
            options: A::OPTIONS,
            flags: A::FLAGS,
            private_windows: A::PRIVATE_WINDOWS,
            launch_resource: A::launch_resource,
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

/// What a window opens on: its profile, the role and person opening it, and
/// what the launch named and was handed.
struct WindowLaunch<'a> {
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
    driver: Box<Driver>,
    owner: ApplicationOwner,
    records: ResourceStore,
    checkout: DocumentCheckout,
    instance: u64,
    profile: Profile,
    permissions: SharedPermissions,
}

/// The application owner `profile` opens in, and the instance its window
/// takes: a window opened beside `beside`, another of the same profile,
/// reads and writes its documents.
fn opening_owner(
    profile: &Profile,
    app: &str,
    author: MemberId,
    permissions: &SharedPermissions,
    beside: Option<&ApplicationOwner>,
) -> Result<(ApplicationOwner, u64), LaunchError> {
    let Some(beside) = beside else {
        return opening_owner_of_profile(profile, app, author, permissions);
    };
    let owner = beside.beside(
        author,
        permissions.clone(),
        tilo_actions::ActionContext::default(),
    )?;
    let instance = match profile {
        Profile::Window(_, _, _, window) => *window,
        _ => INSTANCE,
    };
    Ok((owner, instance))
}

/// The application owner `profile` opens in, and the instance its window
/// takes.
///
/// A private profile's window writes its own records to the profile's store: nothing else would,
/// as no route carries them anywhere.
fn opening_owner_of_profile(
    profile: &Profile,
    app: &str,
    author: MemberId,
    permissions: &SharedPermissions,
) -> Result<(ApplicationOwner, u64), LaunchError> {
    Ok(match profile {
        Profile::Temporary(kept, workspace, device)
        | Profile::Window(kept, workspace, device, _) => (
            ApplicationOwner::with_port(
                Some((kept.records(), true)),
                app,
                device.clone(),
                workspace,
                author,
                permissions.clone(),
                tilo_actions::ActionContext::default(),
            )?,
            match profile {
                Profile::Window(_, _, _, window) => *window,
                _ => INSTANCE,
            },
        ),
        Profile::DataDir(directory) => (
            data_dir_host(directory, app, &author, permissions.clone())?,
            DATA_DIR_INSTANCE,
        ),
    })
}

/// Open the app `opening` describes as `launch` says, `configure` given its
/// host context before it is instantiated.
fn open_app(
    opening: &Opening,
    launch: Launch<'_>,
    beside: Option<&ApplicationOwner>,
    configure: &mut dyn FnMut(&mut ProcessContext),
) -> Result<Opened, LaunchError> {
    strict_bindings();
    let (profile, private, private_arguments) =
        private_launch(opening.private_windows, launch.profile, launch.arguments)?;
    let (arguments, options) =
        split_command_line(&private_arguments, opening.options, opening.flags);
    let arguments = arguments.as_slice();
    let person = launch.author.to_owned();
    let author = MemberId::person(person.clone());
    let permissions = role_permissions(profile.workspace(), &person, launch.role);
    let host_phase = tilo_resource::profiling::Phase::start("start.host");
    let (mut owner, instance) = opening_owner(&profile, opening.id, author, &permissions, beside)?;
    host_phase.end();
    let definition = opening.definition;
    let prepare_phase = tilo_resource::profiling::Phase::start("start.prepare");
    let records = owner.resources_for(definition)?;
    let named =
        (opening.launch_resource)(&LaunchInput::new(arguments, None).with_options(&options));
    let inputs = ComponentRunnerInputs::from_native_process();
    let mut prepared = match (&profile, named) {
        (Profile::Window(..), None) => owner.prepare_default(
            definition,
            instance,
            DocumentCheckout::new(
                DocumentId::window_default(instance),
                definition.resource_type.clone(),
            ),
            None,
            inputs,
        )?,
        (_, named) => prepare_on(&mut owner, definition, instance, named, inputs)?,
    };
    let checkout = prepared.context.record().clone();
    hold_workspace_keys(&prepared.context, profile.workspace())?;
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
    prepare_phase.end();
    let component = {
        let _phase = tilo_resource::profiling::Phase::start("start.instantiate");
        definition.instantiate(prepared.context, prepared.target, prepared.restore)?
    };
    let mut driver = {
        let _phase = tilo_resource::profiling::Phase::start("start.first_frame");
        Box::new(Driver::from_component(
            component,
            viewport,
            prepared.runner_inputs,
        )?)
    };
    // The workspace the window answers to: what the app asks of it is kept, and calls
    // and jobs reach the app as the workspace delivers them.
    driver.connect_peer();
    {
        let _phase = tilo_resource::profiling::Phase::start("start.settle");
        settle_launch(&mut driver);
    }
    // The live host withdraws a component's whole action surface (the
    // palette's, agents', scripts') when any of its actions breaks its
    // contract. Every test that opens the app proves it would not.
    // A capture stages fixtures, not the app someone uses.
    let violations = if captured {
        Vec::new()
    } else {
        let _phase = tilo_resource::profiling::Phase::start("start.contract_check");
        driver.component_contract_violations()
    };
    if !violations.is_empty() {
        return Err(LaunchError::Operation(format!(
            "{} publishes an action surface the host would refuse: {}",
            definition.app_id(),
            violations
                .iter()
                .map(|violation| format!("{}: {}", violation.address, violation.reason))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    Ok(Opened {
        driver,
        owner,
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
            Profile::Temporary(_, _, device) | Profile::Window(_, _, device, _) => device.clone(),
            Profile::DataDir(_) => TEST_DEVICE.to_owned(),
        };
        Profile::Temporary(Kept::fresh()?, TEST_WORKSPACE.to_owned(), device)
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

/// Make a control bound to an action the resource does not publish a panic
/// on this thread, as [`HeadlessApp`] and
/// `tilolang check` do: a typo in a binding must fail, not grey a button.
fn strict_bindings() {
    super::controls::STRICT.with(|strict| strict.set(true));
}

/// `resource` without the actions another provider runs and publishes
/// ([`tilo_actions::ActionSpec::run_by`]).
fn published_here(resource: &mut tilo_actions::ResourceSpec) {
    resource.actions.retain(|spec| spec.provider().is_none());
    resource.children.iter_mut().for_each(published_here);
}

// Retained results come from the real framework semantic API.
#[cfg(test)]
mod results_tests;
