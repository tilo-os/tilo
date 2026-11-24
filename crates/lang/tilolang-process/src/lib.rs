//! The tilolang app process, the one sandbox for app code. Its binary is the tilolang runtime
//! bundle's only one, started in one of these roles:
//!
//! - **the host** of a workspace's instances of an app (this module), started by the device as
//!   the launch kind "tilolang process";
//! - **an app's windows** and **its origin connectors** ([`window`], with `app-launch`), started
//!   by the launcher for an app's launch, as every tilolang package's entrypoint;
//! - **a task's programs** ([`task`]), started beside `tilo` for one task.
//!
//! The host has no app identity. The session starts it as the launch kind "tilolang process", keyed by
//! the workspace and the author as the launcher attributes it, in a sandbox narrower than any
//! app's: it holds only its api and its storage. Everything an instance is arrives over the API
//! from the session (`runtime_portal::process_link`): its broker connection and its
//! identity-service connection, which the session opened and attested as that app's launch, its
//! verified module, and — for an instance that shows a window — its window's patch connection. The
//! broker attributes everything the instance says to the app from the session's own document of
//! the connection, never from the process.
//!
//! Each instance ([`hosted::Hosted`]) reads and writes the app's resources in the workspace's one
//! store over its connection ([`connection`]), publishes the app's actions and runs what the
//! broker delivers, all on that connection. A window's instance drives its window with component
//! patches, and the window sends back what the person did; an instance that shows nothing
//! ([`Shows::Nothing`]) runs the app's actions for whoever invokes them and draws nothing.
//!
//! **Storage.** One directory per workspace and author, granted to the process at its start: it
//! keeps the modules the process compiled, so a later start does not compile them again. The
//! apps' resources are the store's, never the process's.
#![cfg(unix)]

pub mod call;
pub mod canvas;
pub mod connection;
#[cfg(feature = "app-launch")]
pub mod containers_engine;
#[cfg(feature = "app-launch")]
pub mod feed_engine;
pub mod files;
pub mod hosted;
pub mod invocations;
pub mod page_prompts;
pub mod pdf;
pub mod personal;
pub mod phrases;
pub mod presence;
pub mod shell_page;
pub mod stop;
pub mod task;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(feature = "app-launch")]
pub mod video_engine;
pub mod wait;
pub mod web;
#[cfg(feature = "app-launch")]
pub mod window;

/// The shared toolkit and `libstd`: a thin build links both dynamically so the window's process
/// can map a new build of this crate.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The symbols `tilolang dev` reloads the window's code through. The process that owns the
/// window keeps it and maps a new build of this library, which builds the app's root again over
/// the state the running code wrote.
#[cfg(feature = "thin")]
mod reload_in_place {
    /// Hand one window's state back to the process that owns it.
    ///
    /// # Safety
    /// `state` points at `state_len` readable bytes, `out` at `out_cap` writable ones, both
    /// live for this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn tilo_dev_apply(
        id: *const u8,
        id_len: usize,
        state: *const u8,
        state_len: usize,
        out: *mut u8,
        out_cap: usize,
    ) -> isize {
        let _ = (id, id_len);
        unsafe { ui::framework::dev_reload::copy_window_state(state, state_len, out, out_cap) }
    }

    /// The address of the toolkit this library linked, which the process must share.
    #[unsafe(no_mangle)]
    pub extern "C" fn tilo_dev_linked_ui() -> *const () {
        ui::framework::dev_reload::linked_ui()
    }

    /// Build the app's root again, for the process's window to take.
    #[unsafe(no_mangle)]
    pub extern "C" fn tilo_dev_publish_root() -> i32 {
        // This library holds its own copy of everything static in this crate: the package the
        // process runs, and the build it shows. Take both before the window asks for them.
        if let Err(error) = crate::window::install_from_env() {
            eprintln!("tilolang: the new runtime cannot run the package it was built for: {error}");
            return 0;
        }
        if let Some(bundle) = crate::window::installed_bundle() {
            // The window showed the latest of the package's builds: the new root shows that
            // one, and follows the ones after it.
            crate::window::updates::catch_up(&bundle.meta.app);
            crate::window::updates::start(&bundle.meta.app);
        }
        ui::framework::dev_reload::publish_root::<crate::window::app::TlApp>()
    }
}

use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use std::sync::mpsc::{Receiver, TryRecvError};

/// The person's formats and language, as the session says them on the API
/// ([`TilolangProcess::set_locale`]).
pub use runtime_portal::process_link::Locale;
use runtime_portal::process_link::{AppBuild, InstanceFds, InstanceRequest, Module, Shows};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::meta::MetaExt as _;

use hosted::{Hosted, Kept, Spec};
use phrases::Phrases;
use tilolang_tilo::instance::LaterBuilds;

/// The route an instance of `meta`'s package for `person` in `workspace` joins: the person's own
/// document of the workspace for an app whose documents are each member's own (`documents: .own`),
/// which only their devices are admitted to, else the workspace's document of the app.
#[must_use]
pub fn route_of(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    workspace: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    if meta.keeps_own_resources()
        && let Some(profile) = collab_types::person_workspace_of(person)
    {
        let kind = format!("tl.{}", meta.app);
        let id = collab_types::person_document_for_workspace(&kind, workspace);
        return collaboration::RouteSpec::document(&meta.app, profile, id);
    }
    collaboration::RouteSpec::app(meta.app.clone(), workspace)
}

/// A restricted package closes its window when its read grant is removed.
fn reads_protected_route(
    route: &collaboration::RouteSpec,
    access: Option<&tilo_action_policy::MemberAccess>,
) -> bool {
    use tilo_action_policy::{ActionKind, Answer, Context};
    let document = route.document.as_deref().unwrap_or("");
    if !collab_types::validator::requires_independent_document_keys(&route.app_id, document) {
        return true;
    }
    let node = collab_types::document_node(&route.cluster_id.to_string(), &route.app_id, document);
    access.is_some_and(|access| {
        access.answer(
            &node,
            ActionKind::Read,
            &Context::at_secs(tilo_clock::unix_now()),
        ) == Answer::Allow
    })
}

/// Whether an instance joining `route` opens the document `asked`, as a launch names it: a member's
/// own, by its id. The workspace's document of the app is no document a launch names.
fn opens_document(route: &collaboration::RouteSpec, asked: &str) -> bool {
    collab_types::document::DocumentId::parse(asked)
        .is_some_and(|asked| asked.route() == route.document.as_deref().unwrap_or_default())
}

/// Why an instance the session handed over does not run.
#[derive(Debug, thiserror::Error)]
pub enum AdmitError {
    #[error("the instance's module could not be read: {0}")]
    Module(String),
    #[error(
        "the module declares the app `{declared}`, but the session launched `{launched}`; an \
         instance runs a module only under the id it was launched as"
    )]
    IdMismatch { launched: String, declared: String },
    #[error("the workspace `{0}` is no workspace id")]
    Workspace(String),
    #[error(
        "the window was asked to open the document `{asked}`, but an instance of `{app}` opens \
         only its own document of the workspace"
    )]
    OtherResource { app: String, asked: String },
    #[error("the instance's connection could not join its workspace: {0}")]
    Join(String),
    #[error("the instance did not open: {0}")]
    Open(String),
    #[error("the window's connection is unusable: {0}")]
    Window(std::io::Error),
}

/// The device content store over the connection the session opened for this instance: the app's
/// in its workspace, which the launcher never started the process with. A connection that does
/// not hold leaves the instance running without one, as a launched app runs without media.
fn store_of(fd: Option<OwnedFd>) -> Option<tilo_content::ContentStore> {
    let stream = std::os::unix::net::UnixStream::from(fd?);
    tilo_content_device::for_connection(stream)
        .inspect_err(|error| tracing::warn!(%error, "the instance's content store is unusable"))
        .ok()
}

/// The person's chooser over the connection the session opened for this instance, over the
/// chooser the launch brought for what nobody is at the computer to answer.
fn chooser_of(fd: Option<OwnedFd>) -> Option<Arc<dyn tilo_dialogs::FilePicker>> {
    let stream = std::os::unix::net::UnixStream::from(fd?);
    let elsewhere = std::env::var_os(tilo_dialogs::portal::SAVE_DIRECTORY_ENV)
        .filter(|directory| !directory.is_empty())
        .and_then(|directory| {
            tilo_dialogs::unattended_picker(directory.into(), tilo_dialogs::system_picker())
        })
        .unwrap_or_else(tilo_dialogs::system_picker);
    tilo_dialogs::shell::picker(stream, elsewhere)
        .inspect_err(|error| tracing::warn!(%error, "the person's chooser is unusable"))
        .ok()
}

/// The person's clipboard over the connection the session opened for this instance: what
/// `clipboard.copy` and `clipboard.paste` reach, since an instance's process holds no display to
/// offer or read a selection on.
fn clipboard_of(fd: Option<OwnedFd>) -> Option<Arc<tilo_clipboard::service::Connected>> {
    let stream = std::os::unix::net::UnixStream::from(fd?);
    let Some(connected) = tilo_clipboard::service::Connected::connect(stream) else {
        tracing::warn!("the person's clipboard is unusable");
        return None;
    };
    Some(Arc::new(connected))
}

/// The session's tools service over the connection it made for this instance, for the app's
/// workspace: what `File.open` asks which app opens a file and starts it. The session fixed who
/// asks when it made the connection, so the requests on it name no app.
fn tools_of(fd: Option<OwnedFd>, workspace: &str) -> Option<Arc<runtime_portal::tools::Service>> {
    let stream = std::os::unix::net::UnixStream::from(fd?);
    Some(Arc::new(runtime_portal::tools::Service::connect(
        stream, workspace,
    )))
}

/// One running instance and its window's connection, watched for the window closing.
struct Running {
    hosted: Hosted,
    window: Option<OwnedFd>,
}

/// An instance being opened: its module read and its connection joining its workspace's route,
/// each waited for off the host's loop, so the instances running meanwhile keep producing frames.
struct Opening {
    app: String,
    /// The window's instance.
    instance: u64,
    stage: Stage,
    /// The window's connection, until the instance runs: `None` for an instance that shows
    /// nothing ([`Shows::Nothing`]).
    window: Option<Channel>,
    identity: Option<OwnedFd>,
    /// The connection to the app's origin connector, for an app whose types hold an outside
    /// service's resources.
    origin: Option<OwnedFd>,
    /// The device content store of the app in its workspace, over the connection the session
    /// opened for this instance.
    content: Option<tilo_content::ContentStore>,
    /// The person's chooser, over the connection the session opened for this instance.
    chooser: Option<Arc<dyn tilo_dialogs::FilePicker>>,
    /// The person's clipboard, over the connection the session opened for this instance.
    clipboard: Option<Arc<tilo_clipboard::service::Connected>>,
    /// The session's tools service, over the connection it made for this instance.
    tools: Option<Arc<runtime_portal::tools::Service>>,
    /// When the host was handed the instance, and the stages it reached since, with how long
    /// after: what an instance's launch spends in the host.
    received: std::time::Instant,
    reached: Vec<(&'static str, std::time::Duration)>,
    /// The person's environment changed while it opened — their formats, or the name the window
    /// calls someone: its first frame shows what only the window knows again.
    person_changed: bool,
    /// The language the person chose while it opened: its window reads in it from its first
    /// frame.
    language: Option<String>,
    /// The item its window was asked to open on ([`InstanceRequest::reveal`]).
    reveal: Option<String>,
    /// What the app was opened with ([`InstanceRequest::open`]): a file, which the instance runs
    /// its package's action for, or a link, which it hands its `open-link`, once its surface is
    /// published.
    open: Option<runtime_portal::process_link::OpenRequest>,
    /// What its connection delivered while the route joined that the instance takes once it
    /// runs: the workspace's member list, which the broker hands a connection once.
    early: Vec<collaboration::CollabEvent>,
}

impl Opening {
    /// The instance reached the stage `stage` now.
    fn reached(&mut self, stage: &'static str) {
        self.reached.push((stage, self.received.elapsed()));
    }
}

enum Stage {
    /// The module, decoded on a thread of its own.
    Module {
        request: InstanceRequest,
        broker: OwnedFd,
        decoded: Receiver<Result<Bundle, AdmitError>>,
    },
    /// The connection joining the route, in the background of its manager.
    Joining {
        spec: Spec,
        bundle: Arc<Bundle>,
        manager: collaboration::CollaborationManager,
    },
    /// Taken out for a step.
    Moved,
}

/// What became of an opening after a step.
enum Step {
    Waiting,
    /// It runs, its window's connection watched by the second (none for an instance that shows
    /// nothing).
    Open(Box<Hosted>, Option<OwnedFd>),
}

/// The host: what wakes it, and its instances.
pub struct TilolangProcess {
    /// The words this host's own process speaks, which dates and clocks read in where a session
    /// names no language for a window: every window's words are chosen from it and the languages
    /// its package ships ([`hosted::Spec::language`]).
    phrases: Rc<Phrases>,
    /// The person's formats, which every instance's clock and dates read by: the session hands
    /// them over the API, and says again when they change in Settings.
    formats: hosted::Formats,
    wake: Waker,
    scope: tilo_wake::WakeScope,
    running: Vec<Running>,
    opening: Vec<Opening>,
    /// Written whenever an instance's connection has something new: what a resource command
    /// waits on for its answer ([`connection::CallPort`]).
    arrived: Option<Rc<wait::Wake>>,
    /// The windows whose instances ended since the session was last told.
    ended: Vec<u64>,
    /// The agent task whose programs this host runs, when its author is a task.
    task: Option<task::Task>,
    /// The later builds of each app it runs, which its instances of the app follow.
    builds: std::collections::HashMap<String, Arc<LaterBuilds>>,
    /// The device's preview cache, opened the first time a preview is asked for and then shared
    /// by every instance of this host: what `File.thumbnail` answers, so work is done once
    /// across windows.
    thumbnails: Arc<std::sync::OnceLock<Arc<thumbnail::ThumbnailService>>>,
    /// What an earlier version of the host's app kept on this device ([`Self::reading_earlier`]):
    /// the directory the session granted read-only, and the copy read from it once it was read
    /// whole.
    earlier: Option<Earlier>,
}

/// What an earlier version of an app kept on this device: a store the session granted read-only,
/// read once into memory, where a conversion leases and reads it as it would any store and nothing
/// it does reaches the disk.
struct Earlier {
    root: PathBuf,
    read: std::cell::OnceCell<Rc<durable_store::MemoryStore>>,
}

impl Earlier {
    /// Every document the granted store holds, of each app and workspace in it; `None` while one
    /// does not read, so the conversion waits for a start that reads them all.
    fn read(&self) -> Option<Rc<durable_store::MemoryStore>> {
        use durable_store::DurableStore as _;
        if let Some(read) = self.read.get() {
            return Some(Rc::clone(read));
        }
        let store = app_state::FsStore::provisioned(&self.root);
        let read = Rc::new(durable_store::MemoryStore::default());
        let copied = (|| -> Result<(), durable_store::StoreError> {
            for (app, workspace) in store.workspace_indexes()? {
                for document in store.documents(&app, &workspace)? {
                    let key =
                        durable_store::StorageKey::document(app.as_str(), &workspace, &document);
                    if let Some(stored) = store.read(&key)? {
                        read.hydrate(key, stored)?;
                    }
                }
            }
            Ok(())
        })();
        match copied {
            Ok(()) => Some(Rc::clone(self.read.get_or_init(|| read))),
            Err(error) => {
                tracing::warn!(
                    root = %self.root.display(),
                    %error,
                    "what an earlier version kept does not read; carrying it waits"
                );
                None
            }
        }
    }
}

impl TilolangProcess {
    /// A host woken by `wake` whenever an instance's connection or work off the frame has
    /// something for it.
    #[must_use]
    pub fn new(wake: Waker) -> Self {
        let mut scope = tilo_wake::WakeScope::new();
        let arrived = wait::Wake::new()
            .inspect_err(|error| tracing::warn!(%error, "the host's store wake did not open"))
            .ok()
            .map(Rc::new);
        let sink = Arc::clone(&wake);
        let also = arrived.as_ref().map(|arrived| arrived.waker());
        if let Err(error) = scope.bind(move |_| {
            sink();
            if let Some(also) = &also {
                also();
            }
        }) {
            tracing::warn!(?error, "the host's wake scope did not bind");
        }
        Self {
            phrases: Rc::new(Phrases::from_env()),
            formats: hosted::Formats::default(),
            wake,
            scope,
            running: Vec::new(),
            opening: Vec::new(),
            arrived,
            ended: Vec::new(),
            task: None,
            builds: std::collections::HashMap::new(),
            thumbnails: Arc::new(std::sync::OnceLock::new()),
            earlier: None,
        }
    }

    /// The host, reading what an earlier version of its app kept on this device from the store
    /// at `root`: the documents the session granted it read-only, of that app in its workspace.
    /// Read once, never written.
    #[must_use]
    pub fn reading_earlier(mut self, root: PathBuf) -> Self {
        self.earlier = Some(Earlier {
            root,
            read: std::cell::OnceCell::new(),
        });
        self
    }

    /// Run the programs of the task the session handed over, until the API closes it.
    pub fn begin_task(
        &mut self,
        request: &runtime_portal::process_link::TaskRequest,
        fds: runtime_portal::process_link::TaskFds,
    ) -> Result<(), String> {
        self.task = Some(task::Task::open(request, fds)?);
        Ok(())
    }

    /// The task's program connection, to wait on.
    pub fn task_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.task.as_ref().map(task::Task::fd)
    }

    /// The task's further connections, to wait on: the person's lines in its run's
    /// conversation, each a connection of its own into the same session.
    pub fn task_guest_fds(&self) -> Vec<std::os::fd::OwnedFd> {
        self.task
            .as_ref()
            .map(task::Task::guest_fds)
            .unwrap_or_default()
    }

    /// Answer what the task's API sent, and every guest connection with something; the task
    /// ends when the API closed it.
    pub fn serve_task(&mut self) {
        if self.task.as_ref().is_some_and(|task| !task.serve_all()) {
            tracing::info!("task ended: the API closed it");
            self.task = None;
        }
    }

    /// What the instances of `app` are handed beside the workspace's store: what an earlier app
    /// of the same id kept on this device. The workspace is the one store of an app's resources,
    /// so a host keeps no copy of its own, on a device and on a server alike.
    pub fn kept(&self, _app: &str) -> Kept {
        // What an earlier version of the app — a native app the package replaced — kept on this
        // device, read once by the package's own conversion (`earlier_documents`): never
        // written, as the session granted it.
        let earlier = self
            .earlier
            .as_ref()
            .and_then(Earlier::read)
            .map(|read| read as Rc<dyn durable_store::DurableStore>);
        Kept {
            earlier,
            content: None,
            chooser: None,
            clipboard: None,
            thumbnails: Some(Arc::clone(&self.thumbnails)),
            tools: None,
        }
    }

    /// The person's formats, as the session set them: what an instance opened here writes its
    /// times and days by.
    #[must_use]
    pub fn formats(&self) -> &hosted::Formats {
        &self.formats
    }

    /// The person's formats changed (they set another clock or date style in Settings): the
    /// instances write their times again in it and show what they wrote. Whether it was a change:
    /// the same formats said again are nothing to do.
    pub fn set_formats(&mut self, formats: tilo_formats::RegionFormats) -> bool {
        if self.formats.get() == formats {
            return false;
        }
        self.formats.set(formats);
        for running in &mut self.running {
            running.hosted.person_changed();
        }
        for opening in &mut self.opening {
            // An instance still opening reads them when it runs: what it already wrote is
            // written again on its first frame.
            opening.person_changed = true;
        }
        true
    }

    /// The person's formats or language changed in Settings ([`Locale`]): the instances write
    /// their times again in the formats, and their windows read their words in the language,
    /// where they stand. Whether anything changed.
    pub fn set_locale(&mut self, locale: &Locale) -> bool {
        let mut changed = self.set_formats(locale.formats);
        if locale.language.is_empty() {
            return changed;
        }
        self.phrases.speak(&locale.language);
        for running in &mut self.running {
            changed |= running.hosted.speak(&locale.language);
        }
        for opening in &mut self.opening {
            // An instance still opening speaks it on its first frame.
            opening.language = Some(locale.language.clone());
        }
        changed
    }

    /// What the person may do in `app` changed: its instances, running or opening, offer and
    /// invoke by `access` from now on. Whether any of them changed.
    pub fn set_access(
        &mut self,
        app: &str,
        access: Option<&tilo_action_policy::MemberAccess>,
    ) -> bool {
        let mut changed = false;
        let ended = &mut self.ended;
        self.running.retain_mut(|running| {
            if running.hosted.spec().app != app {
                return true;
            }
            let route = running.hosted.connection().route();
            if !reads_protected_route(route, access) {
                running.hosted.flush();
                ended.push(running.hosted.spec().instance);
                changed = true;
                return false;
            }
            changed |= running.hosted.set_access(access.cloned());
            true
        });
        // An instance still opening runs with it from its first frame.
        for opening in self.opening.iter_mut().filter(|opening| opening.app == app) {
            match &mut opening.stage {
                Stage::Module { request, .. } => request.access = access.cloned(),
                Stage::Joining { spec, .. } => {
                    spec.access = access.cloned();
                }
                Stage::Moved => {}
            }
            changed = true;
        }
        changed
    }

    /// The instances running.
    pub fn instances(&self) -> impl Iterator<Item = &Hosted> {
        self.running.iter().map(|running| &running.hosted)
    }

    /// The instances running, to drive one.
    pub fn instances_mut(&mut self) -> impl Iterator<Item = &mut Hosted> {
        self.running.iter_mut().map(|running| &mut running.hosted)
    }

    /// How many instances are being opened.
    #[must_use]
    pub fn opening(&self) -> usize {
        self.opening.len()
    }

    /// Run `hosted`, until its window closes (`window` hangs up) or the host ends. It takes over
    /// each later build of its app as it arrives ([`Self::take_build`]).
    pub fn add(&mut self, hosted: Hosted, window: Option<OwnedFd>) {
        let builds = self.builds.entry(hosted.spec().app.clone()).or_default();
        hosted.follow_builds(Arc::clone(builds));
        tracing::info!(
            app = hosted.spec().app,
            instance = hosted.spec().instance,
            "instance started"
        );
        self.running.push(Running { hosted, window });
    }

    /// The later builds of `app` its instances follow are `builds`: those `tilolang dev` publishes
    /// for a window without a session, which runs its instance in its own process.
    pub fn follow_builds(&mut self, app: &str, builds: Arc<LaterBuilds>) {
        self.builds.insert(app.to_owned(), builds);
    }

    /// A development session verified a later build of an app this host runs, and handed its
    /// module over the API in the immutable shared buffer `module`: every instance of the app
    /// takes it over, in place when it reads the state the one before wrote, else starting over
    /// on its data.
    ///
    /// # Errors
    /// The module does not read, or it is another app's.
    pub fn take_build(&mut self, build: &AppBuild, module: &OwnedFd) -> Result<(), AdmitError> {
        let bundle = module_bundle(module, build.module_len)?;
        if bundle.meta.app != build.app {
            return Err(AdmitError::IdMismatch {
                launched: build.app.clone(),
                declared: bundle.meta.app,
            });
        }
        self.builds
            .entry(build.app.clone())
            .or_default()
            .hand_over(bundle);
        Ok(())
    }

    /// Open the instance the session handed over on the API: its module is read, its
    /// connection joins and its document is read off the host's loop, and it runs once all three
    /// are done ([`Self::turn`]).
    pub fn admit(&mut self, request: InstanceRequest, fds: InstanceFds) -> Result<(), AdmitError> {
        let window = match (request.shows, fds.window) {
            (Shows::View, Some(window)) => {
                Some(Channel::from_socket(window).map_err(AdmitError::Window)?)
            }
            // The session draws nothing for it: it has no window to hand, and none to expect.
            (Shows::Nothing, _) => None,
            (Shows::View, None) => {
                return Err(AdmitError::Window(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a window's instance arrived with no window connection",
                )));
            }
        };
        // The formats the session hands with any instance are the person's: the same for all of
        // them, so the last one said holds for the host, and the instances already running
        // write their times again if they changed.
        self.set_formats(request.formats);
        let (sender, decoded) = std::sync::mpsc::channel();
        let (module, len, wake) = (fds.module, request.module_len, Arc::clone(&self.wake));
        let (app, instance) = (request.app.clone(), request.instance);
        let reveal = request.reveal.clone();
        let open = request.open.clone();
        let tools = tools_of(fds.tools, &request.workspace);
        std::thread::Builder::new()
            .name("tilolang-module".into())
            .spawn(move || {
                // Only an opening that already ended dropped the receiver: nobody waits on it.
                let _ = sender.send(module_bundle(&module, len));
                wake();
            })
            .map_err(AdmitError::Window)?;
        self.opening.push(Opening {
            app,
            instance,
            stage: Stage::Module {
                request,
                broker: fds.broker,
                decoded,
            },
            window,
            identity: Some(fds.identity),
            origin: fds.origin,
            content: store_of(fds.content),
            chooser: chooser_of(fds.chooser),
            clipboard: clipboard_of(fds.clipboard),
            tools,
            received: std::time::Instant::now(),
            reached: Vec::new(),
            person_changed: false,
            language: None,
            reveal,
            open,
            early: Vec::new(),
        });
        Ok(())
    }

    /// Open the instance `spec` of `bundle` on `manager`, whose route it joins (or joined), with
    /// its window at the far end of `window`: it runs once the route is joined, waited for off
    /// the host's loop.
    pub fn open(
        &mut self,
        spec: Spec,
        bundle: Arc<Bundle>,
        manager: collaboration::CollaborationManager,
        window: Channel,
    ) {
        self.opening.push(Opening {
            app: spec.app.clone(),
            instance: spec.instance,
            stage: Stage::Joining {
                spec,
                bundle,
                manager,
            },
            window: Some(window),
            identity: None,
            origin: None,
            content: None,
            chooser: None,
            clipboard: None,
            tools: None,
            received: std::time::Instant::now(),
            reached: Vec::new(),
            person_changed: false,
            language: None,
            reveal: None,
            open: None,
            early: Vec::new(),
        });
    }

    /// The next step of `opening`, without waiting for anything.
    fn step(&self, opening: &mut Opening) -> Result<Step, AdmitError> {
        loop {
            match std::mem::replace(&mut opening.stage, Stage::Moved) {
                Stage::Module {
                    request,
                    broker,
                    decoded,
                } => {
                    let bundle = match decoded.try_recv() {
                        Ok(bundle) => bundle?,
                        Err(TryRecvError::Empty) => {
                            opening.stage = Stage::Module {
                                request,
                                broker,
                                decoded,
                            };
                            return Ok(Step::Waiting);
                        }
                        Err(TryRecvError::Disconnected) => {
                            return Err(AdmitError::Module("its reader ended".to_owned()));
                        }
                    };
                    opening.reached("module");
                    opening.stage =
                        self.join(request, broker, opening.identity.as_ref(), bundle)?;
                }
                Stage::Joining {
                    spec,
                    bundle,
                    mut manager,
                } => {
                    // Nothing is delivered on a route before it is joined: a request that does
                    // come is said, as nothing answers it. The rest waits for the instance.
                    arrived_early(&spec.app, manager.poll(), &mut opening.early);
                    if let Some(refused) = manager.join_refusal() {
                        return Err(AdmitError::Join(refused.to_owned()));
                    }
                    if manager.primary_route().is_none() {
                        if let collaboration::CollaborationState::Offline(reason) = manager.state()
                        {
                            return Err(AdmitError::Join(reason));
                        }
                        opening.stage = Stage::Joining {
                            spec,
                            bundle,
                            manager,
                        };
                        return Ok(Step::Waiting);
                    }
                    opening.reached("joined");
                    return self.run(opening, spec, bundle, manager);
                }
                Stage::Moved => return Ok(Step::Waiting),
            }
        }
    }

    /// Join the route of the instance `request` on its connection `broker`: the manager joins in
    /// its background, and the host is woken as it does.
    fn join(
        &self,
        request: InstanceRequest,
        broker: OwnedFd,
        identity: Option<&OwnedFd>,
        bundle: Bundle,
    ) -> Result<Stage, AdmitError> {
        if bundle.meta.app != request.app {
            return Err(AdmitError::IdMismatch {
                launched: request.app,
                declared: bundle.meta.app,
            });
        }
        let workspace = uuid::Uuid::parse_str(&request.workspace)
            .map_err(|_| AdmitError::Workspace(request.workspace.clone()))?;
        let identity =
            identity.ok_or_else(|| AdmitError::Join("no identity connection".to_owned()))?;
        if bundle.meta.keeps_own_resources()
            && collab_types::person_workspace_of(&request.person).is_none()
        {
            return Err(AdmitError::Join(
                "this app keeps its records in the person's own workspace".into(),
            ));
        }
        let route = route_of(&bundle.meta, workspace, &request.person);
        // A window asked for a document: its instance opens the one its route names, and only that
        // one is what it was asked for.
        if let Some(asked) = request.document.as_deref()
            && !opens_document(&route, asked)
        {
            return Err(AdmitError::OtherResource {
                app: request.app,
                asked: asked.to_owned(),
            });
        }
        let _scope = self.scope.enter();
        let manager = collaboration::CollaborationManager::join_instance(
            &collab_types::document::ResourceType::new(bundle.meta.resource_type()),
            broker,
            identity.as_fd(),
            route,
        )
        .map_err(|error| AdmitError::Join(format!("{error:?}")))?;
        Ok(Stage::Joining {
            spec: Spec {
                installed: request.installed,
                // The daemon made this JSON from its registry: one that does not read is a
                // daemon and a process of different builds, which never run together.
                provided: if request.provided.is_empty() {
                    Vec::new()
                } else {
                    serde_json::from_str(&request.provided).map_err(|error| {
                        AdmitError::Module(format!("the provided packages: {error}"))
                    })?
                },
                language: request.language,
                access: request.access,
                shows: request.shows,
                link_home: request.link_home.as_deref().and_then(|home| {
                    match serde_json::from_str(home) {
                        Ok(home) => Some(home),
                        Err(error) => {
                            // The session made this JSON; one that does not read leaves the
                            // window's links refused, which the session's log says why.
                            tracing::warn!(
                                %error,
                                "a workspace's page the session named does not read: this \
                                 window's resources have no link"
                            );
                            None
                        }
                    }
                }),
                ..Spec::new(
                    request.app,
                    request.workspace,
                    request.instance,
                    request.person,
                )
            },
            bundle: Arc::new(bundle),
            manager,
        })
    }

    /// The instance, its route joined: run it over the workspace's store.
    fn run(
        &self,
        opening: &mut Opening,
        spec: Spec,
        bundle: Arc<Bundle>,
        manager: collaboration::CollaborationManager,
    ) -> Result<Step, AdmitError> {
        // Receivers decide each change against the app's surface: its package's own, as the
        // launcher names it for a launched app.
        let surface = bundle
            .surface()
            .map_err(|error| AdmitError::Module(error.to_string()))?;
        collab_types::app_surface::register_package(&bundle.meta.app, &surface);
        // The documents the route names: the app's own, or those of the id it had before, kept
        // where that app kept them. The changes made there name the app's surface too.
        let route = manager
            .primary_route()
            .map(collaboration::AcceptedRoute::route);
        if route.is_some_and(|route| !reads_protected_route(route, spec.access.as_ref())) {
            return Err(AdmitError::Join(
                "the workspace role does not grant read access to this record".into(),
            ));
        }
        let records = route.map_or_else(|| spec.app.clone(), |route| route.app_id.clone());
        if records != bundle.meta.app {
            collab_types::app_surface::register_package(&records, &surface);
        }
        let mut kept = self.kept(&records);
        // The device's files travel with the instance as its own store and the person's chooser,
        // opened by the session for the app in its workspace.
        kept.content = opening.content.take();
        kept.chooser = opening.chooser.take();
        kept.clipboard = opening.clipboard.take();
        kept.thumbnails = Some(Arc::clone(&self.thumbnails));
        kept.tools = opening.tools.take();
        let _scope = self.scope.enter();
        let connection = connection::Connection::new(manager).map_err(AdmitError::Open)?;
        let store = tilolang_tilo::resources::ResourceStore::new(Rc::new(
            connection.port(self.arrived.clone()),
        ));
        // A window's instance draws and hears its window; one that shows nothing sends its
        // patches nowhere and hears nothing.
        let (transport, watched): (Box<dyn tilo_render_protocol::patch::Transport>, _) =
            match spec.shows {
                Shows::View => {
                    let window = opening
                        .window
                        .take()
                        .ok_or_else(|| AdmitError::Join("its window is gone".to_owned()))?;
                    let watched = window
                        .as_fd()
                        .try_clone_to_owned()
                        .map_err(AdmitError::Window)?;
                    let transport = ChannelTransport::new(window).map_err(AdmitError::Window)?;
                    (Box::new(transport), Some(watched))
                }
                Shows::Nothing => (Box::new(hosted::NoView), None),
            };
        let shows = spec.shows;
        // What the instance's actions keep as content lives in the device's content store, as a
        // native window's does: it outlives the instance.
        let content = kept
            .content
            .clone()
            .unwrap_or_else(tilo_content_device::for_app);
        let mut hosted = Hosted::new(
            spec,
            connection,
            (store, content),
            bundle,
            &kept,
            transport,
            (&self.phrases, &self.formats),
            Arc::clone(&self.wake),
        );
        hosted.arrived_early(std::mem::take(&mut opening.early));
        if opening.person_changed {
            hosted.person_changed();
            opening.person_changed = false;
        }
        if let Some(language) = opening.language.take() {
            hosted.speak(&language);
        }
        let mut hosted = match opening.identity.take() {
            Some(identity) => hosted.holding_identity(identity),
            None => hosted,
        };
        // What a window was asked to show: an instance that shows nothing opens on nothing.
        if shows == Shows::View {
            // The item its window was asked to open on, shown as its `open` shows one.
            if let Some(anchor) = opening.reveal.take() {
                hosted.reveal(anchor);
            }
            // What the app was opened with, which its package's actions take once the window's
            // surface is published: a file's action, or `open-link` with the link.
            match opening.open.take() {
                Some(runtime_portal::process_link::OpenRequest::File(file)) => {
                    hosted.open_file(file);
                }
                Some(runtime_portal::process_link::OpenRequest::Uri(link)) => {
                    hosted.open_link(link);
                }
                None => {}
            }
        }
        self.attach_origin(opening, &mut hosted)?;
        opening.reached("opened");
        let stages: Vec<String> = opening
            .reached
            .iter()
            .map(|(stage, after)| format!("{stage}={}", after.as_micros()))
            .collect();
        tracing::info!(
            target: tilo_startup_metrics::LAUNCH_LATENCY_TARGET,
            app = opening.app,
            instance = opening.instance,
            stages_us = stages.join(","),
            "tilolang instance opened"
        );
        Ok(Step::Open(Box::new(hosted), watched))
    }

    fn attach_origin(&self, opening: &mut Opening, hosted: &mut Hosted) -> Result<(), AdmitError> {
        // Its types with an outside service's resources as origin are kept in step through the
        // app's connector, which holds the network and the credentials this process has none of.
        // Which service is the package's own: a type names it (`origin: imap.message`).
        if let Some(origin) = opening.origin.take() {
            let served = ["imap", "caldav"]
                .into_iter()
                .find(|name| tilolang_tilo::origin::uses_origin(&hosted.bundle().meta, name));
            if let Some(name) = served {
                let origin = tilolang_tilo::origin::remote(
                    name,
                    std::os::unix::net::UnixStream::from(origin),
                    Arc::clone(&self.wake),
                )
                .map_err(|error| AdmitError::Join(format!("its {name} connector: {error}")))?;
                hosted.add_origin(origin);
            } else {
                tracing::warn!(
                    app = opening.app,
                    "a connection to a connector the app has no origin for"
                );
            }
        }
        Ok(())
    }

    /// Take the openings a step further: those done run, those that failed end, and those whose
    /// window closed meanwhile end too.
    fn advance(&mut self) -> bool {
        if self.opening.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut opening = std::mem::take(&mut self.opening);
        let mut started = Vec::new();
        let mut ended = Vec::new();
        opening.retain_mut(|open| {
            if open
                .window
                .as_ref()
                .is_some_and(|window| hung_up(window.as_fd()))
            {
                tracing::info!(
                    app = open.app,
                    "an instance's window closed before it opened"
                );
                ended.push(open.instance);
                changed = true;
                return false;
            }
            match self.step(open) {
                Ok(Step::Waiting) => true,
                Ok(Step::Open(hosted, watched)) => {
                    started.push((*hosted, watched));
                    false
                }
                Err(error) => {
                    tracing::error!(app = open.app, %error, "an instance did not start");
                    ended.push(open.instance);
                    changed = true;
                    false
                }
            }
        });
        self.opening = opening;
        self.ended.extend(ended);
        for (hosted, watched) in started {
            changed = true;
            self.add(hosted, watched);
        }
        changed
    }

    /// The running instances' window connections, to wait on: what one sends is its instance's
    /// next frame's, and one closing ends its instance. An instance that shows nothing has none.
    pub fn windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.running
            .iter()
            .filter_map(|running| running.window.as_ref().map(OwnedFd::as_fd))
    }

    /// The window connections of the instances being opened, to wait on for closing only: what
    /// one sends stays in it until its instance runs, and one closing ends its opening.
    pub fn opening_windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.opening
            .iter()
            .filter_map(|open| open.window.as_ref().map(AsFd::as_fd))
    }

    /// One frame of every instance; an instance whose window closed ends, its document written
    /// first; an instance being opened goes as far as it can without waiting. Whether anything
    /// changed.
    pub fn turn(&mut self) -> bool {
        let mut changed = self.advance();
        let _scope = self.scope.enter();
        for running in &mut self.running {
            changed |= running.hosted.turn();
        }
        let before = self.running.len();
        let ended = &mut self.ended;
        self.running.retain_mut(|running| {
            let open = running
                .window
                .as_ref()
                .is_none_or(|fd| !hung_up(fd.as_fd()));
            if !open {
                running.hosted.page_closed();
                running.hosted.flush();
                tracing::info!(
                    app = running.hosted.spec().app,
                    instance = running.hosted.spec().instance,
                    "instance ended: its window closed"
                );
                ended.push(running.hosted.spec().instance);
            }
            open
        });
        changed || self.running.len() != before
    }

    /// How long until some instance shows something else with nothing arriving
    /// ([`Hosted::wakes_in`]): the host's wait ends then, and that turn runs the views again.
    /// `None` when none waits: the host waits for nothing but its descriptors.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        self.instances().filter_map(Hosted::wakes_in).min()
    }

    /// The windows whose instances ended since this was last asked: closed, or never opened.
    pub fn take_ended(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.ended)
    }

    /// What each instance plays now, when it moved since the last turn said it: the system's
    /// media controls show it, and a media key reaches it. `None` for an instance that plays
    /// nothing.
    pub fn take_media(&mut self) -> Vec<(u64, Option<runtime_portal::process_link::MediaView>)> {
        let mut said = Vec::new();
        for running in &mut self.running {
            if let Some(view) = running.hosted.media() {
                said.push((running.hosted.spec().instance, view));
            }
        }
        said
    }

    /// Run the media control `command` as the Playback's own action on the instance `instance`:
    /// what the desktop's media keys, its now-playing surface and a script ask.
    pub fn media_command(
        &mut self,
        instance: u64,
        command: runtime_portal::process_link::MediaCommand,
    ) {
        let Some(running) = self
            .running
            .iter_mut()
            .find(|running| running.hosted.spec().instance == instance)
        else {
            tracing::warn!(instance, "a media control reached no instance");
            return;
        };
        if let Err(error) = running.hosted.run_media_command(command) {
            tracing::warn!(instance, %error, "a media control did not run");
        }
    }

    /// Whether the host runs no instance and opens none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty() && self.opening.is_empty() && self.task.is_none()
    }

    /// Whether the host owes work of its own: an instance being opened, or a window with a frame
    /// coming. A host that owes none draws nothing more until something reaches it — its api, a
    /// window, or a wake — so a frame taken then is the same frame the next one would be.
    #[must_use]
    pub fn owes_work(&self) -> bool {
        !self.opening.is_empty()
            || self
                .running
                .iter()
                .any(|running| running.hosted.owes_a_frame())
    }

    /// What [`Self::owes_work`] owes, in words: each app whose instance is opening, and each
    /// window whose frame is coming. What a caller that waited longer than it may says of the
    /// host it left.
    #[must_use]
    pub fn owed(&self) -> Vec<String> {
        let mut owed: Vec<String> = self
            .opening
            .iter()
            .map(|opening| format!("{} opening", opening.app))
            .collect();
        for running in &self.running {
            if running.hosted.owes_a_frame() {
                owed.push(format!(
                    "a frame for {}",
                    running.hosted.connection().route().app_id
                ));
            }
        }
        owed
    }
}

/// Say each request in `events`, which reached the instance of `app` before its route was
/// joined: nothing answers it. The rest is kept in `kept`, for the instance once it runs.
fn arrived_early(
    app: &str,
    events: Vec<collaboration::CollabEvent>,
    kept: &mut Vec<collaboration::CollabEvent>,
) {
    for early in events {
        if matches!(
            early,
            collaboration::CollabEvent::Invocation { .. }
                | collaboration::CollabEvent::Handoff { .. }
                | collaboration::CollabEvent::DocumentSnapshotRequested { .. }
        ) {
            tracing::warn!(
                app,
                event = ?early,
                "a request reached an instance before its route was joined"
            );
        } else {
            kept.push(early);
        }
    }
}

/// The bundle the immutable shared buffer `fd` holds, `len` bytes of it.
fn module_bundle(fd: &OwnedFd, len: u64) -> Result<Bundle, AdmitError> {
    let module_error = |error: &dyn std::fmt::Display| AdmitError::Module(error.to_string());
    let len = usize::try_from(len).map_err(|error| module_error(&error))?;
    let mapping = tilo_render_protocol::shm::Mapping::map_sealed(fd, true, len, len)
        .map_err(|error| module_error(&error))?;
    let bytes = mapping
        .get(0, len)
        .ok_or_else(|| AdmitError::Module("the module is shorter than it says".to_owned()))?;
    let module = Module::decode(bytes).map_err(|error| module_error(&error))?;
    Bundle::from_parts(module.wasm, module.ids, &module.imports)
        .map_err(|error| module_error(&error))
}

/// Whether the peer of the socket `fd` hung up.
fn hung_up(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // SAFETY: one initialized pollfd naming an owned descriptor; a zero timeout makes it a
    // nonblocking query.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if ready > 0 && poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    // A channel on macOS is a datagram pair, whose peer's close `poll` does not report as a
    // hang-up: the close disconnects it instead.
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: zeroed storage is a valid `sockaddr_storage`.
        let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut len = libc::socklen_t::try_from(size_of::<libc::sockaddr_storage>())
            .unwrap_or(libc::socklen_t::MAX);
        // SAFETY: `peer` is writable for `len` bytes, and `len` says so.
        let named = unsafe {
            libc::getpeername(
                fd.as_raw_fd(),
                (&raw mut peer).cast::<libc::sockaddr>(),
                &raw mut len,
            )
        };
        if named != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOTCONN) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_open_in_the_persons_workspace_with_the_workspace_as_context() {
        let source = r#"
app so.tilo.lang.views "Views" (resources: .own)

type Views { title: Text }
root Views
default view saved_views for Views

view saved_views(v: Views) {
    ui.Text(v.title)
}
"#;
        let ids = tilolang_tilo::tilolang::IdsLock::default();
        let package = tilolang_tilo::tilolang::compile_package(source, &ids, &[])
            .expect("a package with its records in each member's own workspace")
            .meta;
        let a = uuid::Uuid::from_u128(7);
        let b = uuid::Uuid::from_u128(8);
        let ada = "ad".repeat(32);
        let bo = "b0".repeat(32);
        let route = route_of(&package, a, &ada);
        let document = route.document.as_deref().unwrap();
        assert_eq!(
            route.cluster_id,
            collab_types::person_workspace_of(&ada).unwrap()
        );
        assert_ne!(route.cluster_id, a);
        assert_eq!(collab_types::person_document_context(document), Some(a));
        assert_eq!(
            collab_types::person_resource_type(document),
            Some("tl.so.tilo.lang.views")
        );
        assert_eq!(route_of(&package, b, &ada).cluster_id, route.cluster_id);
        assert_ne!(route_of(&package, b, &ada).document, route.document);
        assert_ne!(route_of(&package, a, &bo).cluster_id, route.cluster_id);
    }

    /// A window asked for the workspace's document, or a member's own, opens on an instance whose
    /// route names that document, and on no other.
    #[test]
    fn an_instance_opens_only_the_document_its_route_names() {
        let workspace = uuid::Uuid::from_u128(7);
        let own = collab_types::document::DocumentId::for_file_document(uuid::Uuid::from_u128(9));
        let workspaces = collaboration::RouteSpec::app("so.tilo.notes", workspace);
        let owns = collaboration::RouteSpec::document("so.tilo.notes", workspace, own.route());
        assert!(opens_document(&owns, own.as_str()));
        assert!(
            !opens_document(&workspaces, own.as_str()),
            "the workspace's document is never named: a launch that names one names another"
        );
        let other =
            collab_types::document::DocumentId::for_file_document(uuid::Uuid::from_u128(10));
        assert!(!opens_document(&owns, other.as_str()));
        assert!(!opens_document(&owns, "not a document"));
        // Naming nothing names no document: not the workspace's, whose route names none, by the
        // empty name or by the key its ownership keeps it under, and not a member's own.
        let primary = collab_types::document::DocumentId::package_primary();
        for unnamed in ["", primary.as_str()] {
            assert!(
                !opens_document(&workspaces, unnamed),
                "`{unnamed}` opened the workspace's document"
            );
            assert!(
                !opens_document(&owns, unnamed),
                "`{unnamed}` opened a member's own document"
            );
        }
    }
}
