//! The tilolang app process: one process per workspace and author, without the toolkit, that runs
//! each of that author's tilolang apps in that workspace as an instance of its own
//! (`docs/designs/app-language/app-window.md`, step 2b).
//!
//! It has no app identity. The session starts it as the launch kind "tilolang process", keyed by
//! the workspace and the author as the launcher attributes it, in a sandbox narrower than any
//! app's: it holds only its api and its storage. Everything an instance is arrives over the API
//! from the session (`runtime_portal::process_link`): its broker connection and its
//! identity-service connection, which the session opened and attested as that app's launch, its
//! verified module, and its window's patch connection. The broker attributes everything the
//! instance says to the app from the session's own document of the connection, never from the
//! process.
//!
//! Each instance ([`hosted::Hosted`]) holds its workspace document with its undo log and save state,
//! publishes the app's actions and runs what the broker delivers, all on its own connection, and
//! drives its window with component patches; the window sends back what the person did.
//!
//! **Storage.** One directory per workspace and author, granted to the process at its start, keyed by
//! package inside: `<storage>/<package>/documents` holds the workspace document's on-disk copy, and
//! `<storage>/<package>/indexes` the indexes its instances read the document through, one per
//! document, shared by every window of it.

pub mod files;
pub mod hosted;
mod mail_recovery;
pub mod personal;
pub mod phrases;
pub mod presence;
pub mod stop;
pub mod task;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod wait;
pub mod web;

use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use std::sync::mpsc::{Receiver, TryRecvError};

use collaboration::CommittedDocument;
use collaboration::native_initial::PrimaryRead;
/// The person's formats and language, as the session says them on the API
/// ([`TilolangProcess::set_locale`]).
pub use runtime_portal::process_link::Locale;
use runtime_portal::process_link::{AppBuild, InstanceFds, InstanceRequest, Module};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::meta::MetaExt as _;

use hosted::{Hosted, Kept, Spec};
use phrases::Phrases;
use tilolang_tilo::instance::LaterBuilds;
use tilolang_tilo::instance::document::InstanceDocument;

/// The route an instance of `meta`'s package for `person` in `workspace` joins: the person's own
/// document of the workspace for an app whose documents are each member's own (`documents: .own`),
/// which only their devices are admitted to, else the workspace's document of the app.
#[must_use]
pub fn route_of(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    workspace: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    route_under(meta, &meta.app, workspace, person)
}

/// [`route_of`], for the documents the app keeps under `documents`: its own id, or for an app
/// renamed the earliest id it had (`InstanceRequest::documents_under`), whose document it keeps using
/// where it is.
#[must_use]
pub fn route_under(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    records: &str,
    workspace: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    if meta.uses_personal_profile()
        && let Some(profile) = collab_types::personal_cluster_of(person)
    {
        let kind = format!("tl.{}", meta.app);
        let id = collab_types::person_document_for_workspace(&kind, workspace);
        return collaboration::RouteSpec::document(records, profile, id);
    }
    collaboration::RouteSpec::app(records.to_owned(), workspace)
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
    #[error(
        "the session opened the documents of `{records}` for `{app}`, an id the module does not \
         say it had before"
    )]
    Records { app: String, records: String },
    #[error("the workspace `{0}` is no workspace id")]
    Workspace(String),
    #[error(
        "the window was asked to open the document `{asked}`, but an instance of `{app}` opens \
         only its own document of the workspace"
    )]
    OtherResource { app: String, asked: String },
    #[error("the instance's connection could not join its workspace: {0}")]
    Join(String),
    #[error(transparent)]
    Open(#[from] tilolang_tilo::instance::document::DocumentError),
    #[error("the window's connection is unusable: {0}")]
    Window(std::io::Error),
}

/// One running instance and its window's connection, watched for the window closing.
struct Running {
    hosted: Hosted,
    window: Option<OwnedFd>,
}

/// An instance being opened: its module read, its connection joining its workspace's route and
/// the broker's committed read of its document, each waited for off the host's loop, so the
/// instances running meanwhile keep producing frames.
struct Opening {
    app: String,
    /// The window's instance.
    instance: u64,
    stage: Stage,
    /// The window's connection, until the instance runs.
    window: Option<Channel>,
    /// For a change applied at the document's origin instead of a window: the change's
    /// connection ([`InstanceRequest::applies_change`]).
    change: Option<OwnedFd>,
    identity: Option<OwnedFd>,
    /// The connection to the app's origin connector, for an app whose types hold an outside
    /// service's resources.
    origin: Option<OwnedFd>,
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
    /// The committed read of the document, taken on a thread of its own.
    Reading {
        spec: Spec,
        bundle: Arc<Bundle>,
        manager: collaboration::CollaborationManager,
        read: Receiver<Result<(PrimaryRead, CommittedDocument), String>>,
    },
    /// Taken out for a step.
    Moved,
}

/// What became of an opening after a step.
enum Step {
    Waiting,
    /// A change applies, its connection watched by the applier.
    Apply(Box<hosted::Applier>),
    /// It runs, its window's connection watched by the second.
    Open(Box<Hosted>, OwnedFd),
}

/// The host: its storage, what wakes it, and its instances.
pub struct TilolangProcess {
    storage: Option<PathBuf>,
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
    /// Changes being applied at their document's origin, each until the session lets go of it.
    appliers: Vec<hosted::Applier>,
    /// The windows whose instances ended since the session was last told.
    ended: Vec<u64>,
    /// The agent task whose programs this host runs, when its author is a task.
    task: Option<task::Task>,
    /// The later builds of each app it runs, which its instances of the app follow.
    builds: std::collections::HashMap<String, Arc<LaterBuilds>>,
}

impl TilolangProcess {
    /// A host keeping its instances' data under `storage` (in memory without), woken by `wake`
    /// whenever an instance's connection or work off the frame has something for it.
    #[must_use]
    pub fn new(storage: Option<PathBuf>, wake: Waker) -> Self {
        let mut scope = tilo_wake::WakeScope::new();
        let sink = Arc::clone(&wake);
        if let Err(error) = scope.bind(move |_| sink()) {
            tracing::warn!(?error, "the host's wake scope did not bind");
        }
        Self {
            storage,
            phrases: Rc::new(Phrases::from_env()),
            formats: hosted::Formats::default(),
            wake,
            scope,
            running: Vec::new(),
            opening: Vec::new(),
            appliers: Vec::new(),
            ended: Vec::new(),
            task: None,
            builds: std::collections::HashMap::new(),
        }
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

    /// Where the instances of `app` keep their workspace document's copy and indexes.
    pub fn kept(&self, app: &str) -> std::io::Result<Kept> {
        let Some(storage) = &self.storage else {
            return Ok(Kept {
                documents: Rc::new(durable_store::MemoryStore::default()),
                indexes: None,
            });
        };
        if app.is_empty() || app.contains(['/', '\0']) || app.starts_with('.') {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let package = storage.join(app);
        let documents = package.join("documents");
        // What was made is durable up to the storage the session granted; nothing above it is
        // the host's, and its sandbox opens nothing there. A start over directories an earlier
        // one made waits for no disk.
        if !documents.is_dir() {
            std::fs::create_dir_all(&documents)?;
            for made in [&package, storage] {
                std::fs::File::open(made)?.sync_all()?;
            }
        }
        Ok(Kept {
            documents: Rc::new(app_state::FsStore::provisioned(&documents)),
            indexes: Some(package.join("indexes")),
        })
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
            let route = running.hosted.record().route();
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
                Stage::Joining { spec, .. } | Stage::Reading { spec, .. } => {
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
    /// module over the API in the sealed memfd `module`: every instance of the app takes it over,
    /// in place when it reads the state the one before wrote, else starting over on its data.
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
        let (window, change) = if request.applies_change {
            (None, Some(fds.window))
        } else {
            (
                Some(Channel::from_socket(fds.window).map_err(AdmitError::Window)?),
                None,
            )
        };
        // The formats the session hands with any instance are the person's: the same for all of
        // them, so the last one said holds for the host, and the instances already running
        // write their times again if they changed.
        self.set_formats(request.formats);
        let (sender, decoded) = std::sync::mpsc::channel();
        let (module, len, wake) = (fds.module, request.module_len, Arc::clone(&self.wake));
        let (app, instance) = (request.app.clone(), request.instance);
        let reveal = request.reveal.clone();
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
            change,
            identity: Some(fds.identity),
            origin: fds.origin,
            received: std::time::Instant::now(),
            reached: Vec::new(),
            person_changed: false,
            language: None,
            reveal,
            early: Vec::new(),
        });
        Ok(())
    }

    /// Open the instance `spec` of `bundle` on `manager`, whose route it joins (or joined), with
    /// its window at the far end of `window`: it runs once the route is joined and its document
    /// read, both waited for off the host's loop.
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
            change: None,
            identity: None,
            origin: None,
            received: std::time::Instant::now(),
            reached: Vec::new(),
            person_changed: false,
            language: None,
            reveal: None,
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
                    let Some(reader) = PrimaryRead::of(&manager).map_err(AdmitError::Join)? else {
                        return self.run(opening, spec, bundle, manager, None);
                    };
                    // The read waits for as long as the broker takes: say that it does, so an
                    // instance that never opens shows where it waits.
                    tracing::info!(
                        app = opening.app,
                        instance = opening.instance,
                        "a tilolang instance reads its document's committed history"
                    );
                    let (sender, read) = std::sync::mpsc::channel();
                    let wake = Arc::clone(&self.wake);
                    std::thread::Builder::new()
                        .name("tilolang-read".into())
                        .spawn(move || {
                            // Only an opening that already ended dropped the receiver:
                            // nobody waits on it.
                            let _ = sender.send(reader.read().map(|read| (reader, read)));
                            wake();
                        })
                        .map_err(AdmitError::Window)?;
                    opening.stage = Stage::Reading {
                        spec,
                        bundle,
                        manager,
                        read,
                    };
                }
                Stage::Reading {
                    spec,
                    bundle,
                    manager,
                    read,
                } => {
                    return match read.try_recv() {
                        Ok(read) => {
                            let read = read.map_err(AdmitError::Join)?;
                            opening.reached("read");
                            self.run(opening, spec, bundle, manager, Some(read))
                        }
                        Err(TryRecvError::Empty) => {
                            opening.stage = Stage::Reading {
                                spec,
                                bundle,
                                manager,
                                read,
                            };
                            Ok(Step::Waiting)
                        }
                        Err(TryRecvError::Disconnected) => {
                            Err(AdmitError::Join("the document's read ended".to_owned()))
                        }
                    };
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
        // The document of an id the app had before, as the session names it: one the module says
        // it had. Whether the same publisher signed both the broker checks as it admits the join.
        let records = match &request.documents_under {
            Some(records) if !bundle.meta.previously.contains(records) => {
                return Err(AdmitError::Records {
                    app: request.app,
                    records: records.clone(),
                });
            }
            Some(records) => records.clone(),
            None => request.app.clone(),
        };
        // The ids the session believes each app had before are the ones this host honours: a
        // module's own `previously:` is a claim the installed catalogue decides on.
        for (app, under) in std::iter::once((&request.app, &records)).chain(&request.installed) {
            if under != app {
                tilolang_tilo::schema::register_previously(app, std::slice::from_ref(under));
            }
        }
        let workspace = uuid::Uuid::parse_str(&request.workspace)
            .map_err(|_| AdmitError::Workspace(request.workspace.clone()))?;
        let identity =
            identity.ok_or_else(|| AdmitError::Join("no identity connection".to_owned()))?;
        if bundle.meta.uses_personal_profile()
            && collab_types::personal_cluster_of(&request.person).is_none()
        {
            return Err(AdmitError::Join(
                "this app needs the person's certified profile".into(),
            ));
        }
        let route = route_under(&bundle.meta, &records, workspace, &request.person);
        // A window asked for a document: its instance opens the one its route names, and only that
        // one is what it was asked for.
        if let Some(asked) = request.document.as_deref()
            && !opens_document(&route, asked)
            && !(bundle.meta.uses_personal_profile()
                && [&records, &bundle.meta.app].into_iter().any(|app| {
                    tilolang_tilo::member_document_of(app, &request.person).as_deref()
                        == Some(asked)
                }))
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
                language: request.language,
                access: request.access,
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

    /// The instance, its route joined and its document read: open the document and run it.
    fn run(
        &self,
        opening: &mut Opening,
        spec: Spec,
        bundle: Arc<Bundle>,
        manager: collaboration::CollaborationManager,
        read: Option<(PrimaryRead, CommittedDocument)>,
    ) -> Result<Step, AdmitError> {
        // Receivers decide each change against the app's surface: its package's own, as the
        // launcher names it for a launched app.
        let surface = bundle
            .surface()
            .map_err(|error| AdmitError::Module(error.to_string()))?;
        collab_types::app_surface::register_package(&bundle.meta.app, &surface);
        // The documents the route names: the app's own, or those of the id it had before, kept
        // where that app kept them. The changes made there name the app's surface too.
        let route = manager.primary_route().map(|joined| joined.route());
        if route.is_some_and(|route| !reads_protected_route(route, spec.access.as_ref())) {
            return Err(AdmitError::Join(
                "the workspace role does not grant read access to this record".into(),
            ));
        }
        let records = route.map_or_else(|| spec.app.clone(), |route| route.app_id.clone());
        if records != bundle.meta.app {
            collab_types::app_surface::register_package(&records, &surface);
        }
        let kept = self
            .kept(&records)
            .map_err(|error| AdmitError::Join(error.to_string()))?;
        let _scope = self.scope.enter();
        // What the document's actions keep as content lives in the device's content store, as a
        // native window's document content does: it outlives the instance and reaches the document's
        // other readers on its route.
        let record = InstanceDocument::open_read(
            manager,
            read,
            &kept.documents,
            spec.person.clone(),
            bundle.meta.resource_type(),
        )?
        .keeping_content_in(tilo_content_device::for_app());
        if let Some(change) = opening.change.take() {
            // A change at the document's origin: the document and the package, no instance.
            let applier = hosted::Applier::open(
                spec,
                record,
                &bundle,
                &self.phrases,
                Arc::clone(&self.wake),
                change,
            )
            .map_err(AdmitError::Join)?;
            opening.reached("opened");
            return Ok(Step::Apply(Box::new(applier)));
        }
        let window = opening
            .window
            .take()
            .ok_or_else(|| AdmitError::Join("its window is gone".to_owned()))?;
        let watched = window
            .as_fd()
            .try_clone_to_owned()
            .map_err(AdmitError::Window)?;
        let transport = ChannelTransport::new(window).map_err(AdmitError::Window)?;
        let mut hosted = Hosted::with_document(
            spec,
            record,
            bundle,
            &kept,
            Box::new(transport),
            &self.phrases,
            &self.formats,
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
        // The item its window was asked to open on, shown as its `open` shows one.
        if let Some(anchor) = opening.reveal.take() {
            hosted.reveal(anchor);
        }
        // Its types with a mail server as origin are kept in step through the app's connector,
        // which holds the network and the credentials this process has none of.
        if let Some(origin) = opening.origin.take() {
            if tilolang_tilo::origin::uses_origin(&hosted.bundle().meta, "imap") {
                let origin = tilolang_mail::remote_origin(
                    std::os::unix::net::UnixStream::from(origin),
                    Arc::clone(&self.wake),
                )
                .map_err(|error| AdmitError::Join(format!("its mail connector: {error}")))?;
                hosted.add_origin(origin);
            } else {
                tracing::warn!(
                    app = opening.app,
                    "a connection to a connector the app has no origin for"
                );
            }
        }
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

    /// Take the openings a step further: those done run, those that failed end, and those whose
    /// window closed meanwhile end too.
    fn advance(&mut self) -> bool {
        if self.opening.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut opening = std::mem::take(&mut self.opening);
        let mut started = Vec::new();
        let mut applying = Vec::new();
        let mut ended = Vec::new();
        opening.retain_mut(|open| {
            if open
                .window
                .as_ref()
                .is_some_and(|window| hung_up(window.as_fd()))
                || open
                    .change
                    .as_ref()
                    .is_some_and(|change| hung_up(change.as_fd()))
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
                Ok(Step::Apply(applier)) => {
                    applying.push(*applier);
                    changed = true;
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
        self.appliers.extend(applying);
        for (hosted, watched) in started {
            changed = true;
            self.add(hosted, Some(watched));
        }
        changed
    }

    /// The running instances' window connections, to wait on: what one sends is its instance's
    /// next frame's, and one closing ends its instance.
    pub fn windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.running
            .iter()
            .filter_map(|running| running.window.as_ref().map(OwnedFd::as_fd))
            // A change's connection: its line arriving, and the session letting go of it.
            .chain(self.appliers.iter().map(hosted::Applier::connection))
    }

    /// The window connections of the instances being opened, to wait on for closing only: what
    /// one sends stays in it until its instance runs, and one closing ends its opening.
    pub fn opening_windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.opening
            .iter()
            .filter_map(|open| open.window.as_ref().map(AsFd::as_fd))
            .chain(
                self.opening
                    .iter()
                    .filter_map(|open| open.change.as_ref().map(AsFd::as_fd)),
            )
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
        // An undo of an action that wrote several units takes every unit back through its own
        // instance, and the reverts go out as one action.
        let asks: Vec<(u64, tilo_actions::InvocationId)> = self
            .running
            .iter_mut()
            .filter_map(|running| {
                let invocation = running.hosted.take_undo_ask()?;
                Some((running.hosted.spec().instance, invocation))
            })
            .collect();
        for (asked_in, invocation) in asks {
            changed |= self.undo(asked_in, invocation);
        }
        let before = self.running.len();
        let ended = &mut self.ended;
        self.running.retain_mut(|running| {
            let open = running
                .window
                .as_ref()
                .is_none_or(|fd| !hung_up(fd.as_fd()));
            if !open {
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
        // A change runs, is answered, and ends once the session let go of its connection: the
        // workspace was handed what it wrote by then.
        for applier in &mut self.appliers {
            changed |= applier.turn();
        }
        let applying = self.appliers.len();
        let ended = &mut self.ended;
        self.appliers.retain(|applier| {
            let gone = hung_up(applier.connection());
            if gone {
                if !applier.answered() {
                    tracing::warn!(
                        app = applier.spec().app,
                        "a change's session left before it was answered"
                    );
                }
                ended.push(applier.spec().instance);
            }
            !gone
        });
        changed || self.running.len() != before || self.appliers.len() != applying
    }

    /// Take the step the person asked to undo in `asked_in` back: in that unit alone when no
    /// other unit of the workspace this host runs holds a change of its action, and otherwise in
    /// every unit that does, each through its own instance, the reverts published as one action.
    ///
    /// The units and their changes come from the documents themselves, lazily, unit by unit: an
    /// undo works after a restart that lost every log the transport kept, because the action's
    /// invocation names its changes in every document it wrote. Whether anything was taken back.
    fn undo(&mut self, asked_in: u64, invocation: tilo_actions::InvocationId) -> bool {
        let mut parts: Vec<(String, String, Vec<[u8; 32]>)> = Vec::new();
        let mut workspace = None;
        for running in &self.running {
            let route = running.hosted.record().route();
            let changes = running.hosted.changes_of(invocation);
            if changes.is_empty() {
                continue;
            }
            if running.hosted.spec().instance == asked_in {
                workspace = Some(route.cluster_id);
            }
            parts.push((
                route.app_id.clone(),
                route.document.clone().unwrap_or_default(),
                changes,
            ));
        }
        let Some(workspace) = workspace else {
            return false;
        };
        if parts.len() <= 1 {
            // The step is this unit's alone: its own instance takes it back.
            return self
                .running
                .iter_mut()
                .find(|running| running.hosted.spec().instance == asked_in)
                .is_some_and(|running| running.hosted.undo_here());
        }
        // The reverts are one new action of the person's: one fresh invocation names it in every
        // unit's change, which is what makes the parts one commit.
        let ambient = tilo_actions::ActionContext {
            source_invocation: Some(tilo_actions::InvocationId::new()),
            ..tilo_actions::ActionContext::default()
        };
        let mut units: Vec<(String, String, collaboration::DocumentMutation)> = Vec::new();
        for running in &mut self.running {
            let route = running.hosted.record().route();
            let (app, record) = (
                route.app_id.clone(),
                route.document.clone().unwrap_or_default(),
            );
            let Some((_, _, changes)) = parts
                .iter()
                .find(|(unit_app, unit_document, _)| unit_app == &app && unit_document == &record)
            else {
                continue;
            };
            for change in changes {
                if !running.hosted.can_revert(change) {
                    continue;
                }
                if let Err(error) = running.hosted.revert_step(change, &ambient) {
                    tracing::warn!(
                        app, record, %error,
                        "a unit of an action did not take its step back"
                    );
                }
            }
            // What the unit's reverts wrote, in whichever chunk: each part names the document it
            // travels on, so an action undone across a unit's chunks still goes as one commit.
            for (record, change) in running.hosted.unsent_parts() {
                units.push((app.clone(), record, change));
            }
        }
        if units.is_empty() {
            return false;
        }
        let undo = collaboration::WorkspaceCommit::of_units(
            workspace,
            &collaboration::CommitAuthor {
                actor: None,
                device: [0; 32],
            },
            &units
                .iter()
                .map(|(app, record, mutation)| collaboration::UnitMutation {
                    app,
                    document_id: record,
                    mutation,
                    document: None,
                })
                .collect::<Vec<_>>(),
        )
        .ok()
        .and_then(|commits| commits.into_iter().flatten().next());
        for running in &mut self.running {
            let app = running.hosted.record().route().app_id.clone();
            let sent = units.iter().any(|(unit_app, _, _)| *unit_app == app);
            if !sent {
                continue;
            }
            match &undo {
                Some(commit) => running.hosted.share_under(commit),
                None => running.hosted.share(),
            }
        }
        true
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

    /// Whether the host runs no instance and opens none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
            && self.opening.is_empty()
            && self.appliers.is_empty()
            && self.task.is_none()
    }

    /// Whether the host owes work of its own: an instance being opened, a change being applied,
    /// or a window with a frame coming. A host that owes none draws nothing more until something
    /// reaches it — its api, a window, or a wake — so a frame taken then is the same frame the
    /// next one would be.
    #[must_use]
    pub fn owes_work(&self) -> bool {
        !self.opening.is_empty()
            || self.appliers.iter().any(hosted::Applier::owes)
            || self
                .running
                .iter()
                .any(|running| running.hosted.owes_a_frame())
    }

    /// What [`Self::owes_work`] owes, in words: each app whose instance is opening, each app
    /// whose change is being applied, and each window whose frame is coming. What a caller that
    /// waited longer than it may says of the host it left.
    #[must_use]
    pub fn owed(&self) -> Vec<String> {
        let mut owed: Vec<String> = self
            .opening
            .iter()
            .map(|opening| format!("{} opening", opening.app))
            .collect();
        owed.extend(
            self.appliers
                .iter()
                .map(|applier| format!("{} applying a change", applier.spec().app)),
        );
        for running in &self.running {
            if running.hosted.owes_a_frame() {
                owed.push(format!(
                    "a frame for {}",
                    running.hosted.record().route().app_id
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

/// The bundle the sealed memfd `fd` holds, `len` bytes of it.
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
    ready > 0 && poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_open_in_the_persons_profile_with_the_workspace_as_context() {
        let source = r#"
app so.tilo.lang.views "Views" (resources: .profile)

type Views { title: Text }
root Views
default view saved_views for Views

view saved_views(v: Views) {
    ui.Text(v.title)
}
"#;
        let ids = tilolang_tilo::tilolang::IdsLock::default();
        let package = tilolang_tilo::tilolang::compile_package(source, &ids, &[])
            .expect("a package with profile storage")
            .meta;
        let a = uuid::Uuid::from_u128(7);
        let b = uuid::Uuid::from_u128(8);
        let ada = "ad".repeat(32);
        let bo = "b0".repeat(32);
        let route = route_of(&package, a, &ada);
        let document = route.document.as_deref().unwrap();
        assert_eq!(
            route.cluster_id,
            collab_types::personal_cluster_of(&ada).unwrap()
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

    #[test]
    fn a_legacy_package_opens_in_the_certified_profile() {
        let source = r#"
app test.legacynotes "Earlier notes" (resources: .own)

type Notes { title: Text }
root Notes
default view saved_notes for Notes

view saved_notes(notes: Notes) {
    ui.Text(notes.title)
}
"#;
        let package = tilolang_tilo::tilolang::compile_package(source, &Default::default(), &[])
            .expect("an earlier private package")
            .meta;
        assert!(package.own_resources);
        let person = "ad".repeat(32);
        let workspace = uuid::Uuid::from_u128(18);
        let route = route_of(&package, workspace, &person);
        assert_eq!(
            route.cluster_id,
            collab_types::personal_cluster_of(&person).unwrap()
        );
        let document = route.document.as_deref().unwrap();
        assert_eq!(
            collab_types::person_document_context(document),
            Some(workspace)
        );
        assert_eq!(
            collab_types::person_resource_type(document),
            Some("tl.test.legacynotes")
        );
    }

    /// A window asked for the workspace's document, or a member's own, opens on an instance whose
    /// route names that document, and on no other.
    #[test]
    fn an_instance_opens_only_the_document_its_route_names() {
        let workspace = uuid::Uuid::from_u128(7);
        let own = collab_types::document::DocumentId::for_file_document(uuid::Uuid::from_u128(9));
        let workspaces = collaboration::RouteSpec::app("so.tilo.lang.notes", workspace);
        let owns = collaboration::RouteSpec::document("so.tilo.lang.notes", workspace, own.route());
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
