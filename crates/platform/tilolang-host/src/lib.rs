//! The tilolang host: one process per workspace and author, without the toolkit, that runs each of
//! that author's tilolang apps in that workspace as an instance of its own
//! (`docs/designs/app-language/ui-host.md`, step 2b).
//!
//! The host has no app identity. The session starts it as the launch kind "tilolang host", keyed
//! by the workspace and the author as the launcher attributes it, in a sandbox narrower than any
//! app's: it holds only its door and its storage. Everything an instance is arrives over the door
//! from the session (`runtime_portal::tilolang_host`): its broker connection and its
//! identity-service connection, which the session opened and attested as that app's launch, its
//! verified module, and its window's patch connection. The broker attributes everything the
//! instance says to the app from the session's own record of the connection, never from the
//! host.
//!
//! Each instance ([`hosted::Hosted`]) holds its workspace record with its undo log and save state,
//! publishes the app's actions and runs what the broker delivers, all on its own connection, and
//! drives its window with component patches; the window sends back what the person did.
//!
//! **Storage.** One directory per workspace and author, granted to the host at its start, keyed by
//! package inside: `<storage>/<package>/documents` holds the workspace record's on-disk copy, and
//! `<storage>/<package>/indexes` the indexes its instances read the record through, one per
//! record, shared by every window of it.

pub mod files;
pub mod hosted;
pub mod phrases;
pub mod stop;
pub mod task;
pub mod wait;
pub mod web;

use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use std::sync::mpsc::{Receiver, TryRecvError};

use collaboration::CommittedDocument;
use collaboration::native_initial::PrimaryRead;
/// The person's formats and language, as the session says them on the door
/// ([`TilolangHost::set_locale`]).
pub use runtime_portal::tilolang_host::Locale;
use runtime_portal::tilolang_host::{InstanceFds, InstanceRequest, Module};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;

use hosted::{Hosted, Kept, Spec};
use phrases::Phrases;
use tilolang_tilo::instance::record::InstanceRecord;

/// The record kind of an app's workspace record: every tilolang package keeps its resources in it.
pub const RECORD_KIND: &str = tilolang_tilo::RECORD_KIND;

/// The route an instance of `meta`'s package for `person` in `workspace` joins: the person's own
/// record of the workspace for an app whose records are each member's own (`records: .own`),
/// which only their devices are admitted to, else the workspace's record of the app.
#[must_use]
pub fn route_of(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    workspace: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    route_under(meta, &meta.app, workspace, person)
}

/// [`route_of`], for the records the app keeps under `records`: its own id, or for an app
/// renamed the earliest id it had (`InstanceRequest::records_under`), whose record it keeps using
/// where it is.
#[must_use]
pub fn route_under(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    records: &str,
    workspace: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    match tilolang_tilo::own_record(meta, person) {
        Some(record) => collaboration::RouteSpec::record(records.to_owned(), workspace, record),
        None => collaboration::RouteSpec::app(records.to_owned(), workspace),
    }
}

/// Whether an instance joining `route` opens the record `asked`, as a launch names it: a member's
/// own, by its id. The workspace's record of the app is no record a launch names.
fn opens_record(route: &collaboration::RouteSpec, asked: &str) -> bool {
    collaboration::record::RecordId::parse(asked)
        .is_some_and(|asked| asked.route() == route.record.as_deref().unwrap_or_default())
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
        "the session opened the records of `{records}` for `{app}`, an id the module does not \
         say it had before"
    )]
    Records { app: String, records: String },
    #[error("the workspace `{0}` is no workspace id")]
    Workspace(String),
    #[error(
        "the window was asked to open the record `{asked}`, but an instance of `{app}` opens \
         only its own record of the workspace"
    )]
    OtherRecord { app: String, asked: String },
    #[error("the instance's connection could not join its workspace: {0}")]
    Join(String),
    #[error(transparent)]
    Open(#[from] tilolang_tilo::instance::record::RecordError),
    #[error("the window's connection is unusable: {0}")]
    Window(std::io::Error),
}

/// One running instance and its window's connection, watched for the window closing.
struct Running {
    hosted: Hosted,
    window: Option<OwnedFd>,
}

/// An instance being opened: its module read, its connection joining its workspace's route and
/// the broker's committed read of its record, each waited for off the host's loop, so the
/// instances running meanwhile keep producing frames.
struct Opening {
    app: String,
    /// The window's instance.
    instance: u64,
    stage: Stage,
    /// The window's connection, until the instance runs.
    window: Option<Channel>,
    /// For a change applied at the record's origin instead of a window: the change's
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
    /// The committed read of the record, taken on a thread of its own.
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
pub struct TilolangHost {
    storage: Option<PathBuf>,
    /// The words this host's own process speaks, which dates and clocks read in where a session
    /// names no language for a window: every window's words are chosen from it and the languages
    /// its package ships ([`hosted::Spec::language`]).
    phrases: Rc<Phrases>,
    /// The person's formats, which every instance's clock and dates read by: the session hands
    /// them over the door, and says again when they change in Settings.
    formats: hosted::Formats,
    wake: Waker,
    scope: tilo_wake::WakeScope,
    running: Vec<Running>,
    opening: Vec<Opening>,
    /// Changes being applied at their record's origin, each until the session lets go of it.
    appliers: Vec<hosted::Applier>,
    /// The windows whose instances ended since the session was last told.
    ended: Vec<u64>,
    /// The agent task whose programs this host runs, when its author is a task.
    task: Option<task::Task>,
}

impl TilolangHost {
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
        }
    }

    /// Run the programs of the task the session handed over, until the door closes it.
    pub fn begin_task(
        &mut self,
        request: &runtime_portal::tilolang_host::TaskRequest,
        fds: runtime_portal::tilolang_host::TaskFds,
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

    /// Answer what the task's door sent, and every guest connection with something; the task
    /// ends when the door closed it.
    pub fn serve_task(&mut self) {
        if self.task.as_ref().is_some_and(|task| !task.serve_all()) {
            tracing::info!("task ended: the door closed it");
            self.task = None;
        }
    }

    /// Where the instances of `app` keep their workspace record's copy and indexes.
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
        std::fs::create_dir_all(&documents)?;
        // What was made is durable up to the storage the session granted; nothing above it is
        // the host's, and its sandbox opens nothing there.
        for made in [&package, storage] {
            std::fs::File::open(made)?.sync_all()?;
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

    /// Run `hosted`, until its window closes (`window` hangs up) or the host ends.
    pub fn add(&mut self, hosted: Hosted, window: Option<OwnedFd>) {
        tracing::info!(
            app = hosted.spec().app,
            instance = hosted.spec().instance,
            "instance started"
        );
        self.running.push(Running { hosted, window });
    }

    /// Open the instance the session handed over on the door: its module is read, its
    /// connection joins and its record is read off the host's loop, and it runs once all three
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
        });
        Ok(())
    }

    /// Open the instance `spec` of `bundle` on `manager`, whose route it joins (or joined), with
    /// its window at the far end of `window`: it runs once the route is joined and its record
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
                    // Nothing is delivered on a route before it is joined: what does come is
                    // said, as nothing answers it.
                    said_early(&spec.app, manager.poll());
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
                        "a tilolang instance reads its record's committed history"
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
                            Err(AdmitError::Join("the record's read ended".to_owned()))
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
        // The record of an id the app had before, as the session names it: one the module says
        // it had. Whether the same publisher signed both the broker checks as it admits the join.
        let records = match &request.records_under {
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
        let workspace = uuid::Uuid::parse_str(&request.room)
            .map_err(|_| AdmitError::Workspace(request.room.clone()))?;
        let identity =
            identity.ok_or_else(|| AdmitError::Join("no identity connection".to_owned()))?;
        let route = route_under(&bundle.meta, &records, workspace, &request.person);
        // A window asked for a record: its instance opens the one its route names, and only that
        // one is what it was asked for.
        if let Some(asked) = request.record.as_deref()
            && !opens_record(&route, asked)
        {
            return Err(AdmitError::OtherRecord {
                app: request.app,
                asked: asked.to_owned(),
            });
        }
        let _scope = self.scope.enter();
        let manager = collaboration::CollaborationManager::join_instance(
            &collaboration::record::RecordKind::new(RECORD_KIND),
            broker,
            identity.as_fd(),
            route,
            Some(request.person.clone()),
        )
        .map_err(|error| AdmitError::Join(format!("{error:?}")))?;
        Ok(Stage::Joining {
            spec: Spec {
                installed: request.installed,
                language: request.language,
                ..Spec::new(request.app, request.room, request.instance, request.person)
            },
            bundle: Arc::new(bundle),
            manager,
        })
    }

    /// The instance, its route joined and its record read: open the record and run it.
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
        collaboration::app_surface::register_package(&bundle.meta.app, &surface);
        // The records the route names: the app's own, or those of the id it had before, kept
        // where that app kept them. The changes made there name the app's surface too.
        let records = manager
            .primary_route()
            .map_or_else(|| spec.app.clone(), |joined| joined.route().app_id.clone());
        if records != bundle.meta.app {
            collaboration::app_surface::register_package(&records, &surface);
        }
        let kept = self
            .kept(&records)
            .map_err(|error| AdmitError::Join(error.to_string()))?;
        let _scope = self.scope.enter();
        let record =
            InstanceRecord::open_read(manager, read, &kept.documents, spec.person.clone())?;
        if let Some(change) = opening.change.take() {
            // A change at the record's origin: the record and the package, no instance.
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
        let mut hosted = Hosted::with_record(
            spec,
            record,
            bundle,
            &kept,
            Box::new(transport),
            &self.phrases,
            &self.formats,
            Arc::clone(&self.wake),
        );
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

    /// One frame of every instance; an instance whose window closed ends, its record written
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
    /// The units and their changes come from the records themselves, lazily, unit by unit: an
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
                workspace = Some(route.cluster_id.to_string());
            }
            parts.push((
                route.app_id.clone(),
                route.record.clone().unwrap_or_default(),
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
                route.record.clone().unwrap_or_default(),
            );
            let Some((_, _, changes)) = parts
                .iter()
                .find(|(unit_app, unit_record, _)| unit_app == &app && unit_record == &record)
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
            // What the unit's reverts wrote, in whichever chunk: each part names the record it
            // travels on, so an action undone across a unit's chunks still goes as one commit.
            for (record, change) in running.hosted.unsent_parts() {
                units.push((app.clone(), record, change));
            }
        }
        if units.is_empty() {
            return false;
        }
        let undo = collaboration::WorkspaceCommit::of_units(
            &workspace,
            &collaboration::CommitAuthor {
                principal: None,
                device: [0; 32],
            },
            &units
                .iter()
                .map(|(app, record, mutation)| collaboration::UnitMutation {
                    app,
                    record,
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

    /// How long until the time a view of some instance read (`today` in a query) next reads
    /// differently: the host's wait ends then, and that turn runs again the views that read it.
    /// `None` when no view read the time: the host waits for nothing but its descriptors.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        self.instances()
            .filter_map(|hosted| hosted.instance().wakes_in())
            .min()
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
    /// reaches it — its door, a window, or a wake — so a frame taken then is the same frame the
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
/// joined: nothing answers it.
fn said_early(app: &str, events: Vec<collaboration::CollabEvent>) {
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

    /// A window asked for the workspace's record, or a member's own, opens on an instance whose
    /// route names that record, and on no other.
    #[test]
    fn an_instance_opens_only_the_record_its_route_names() {
        let workspace = uuid::Uuid::from_u128(7);
        let own = collaboration::record::RecordId::for_file_record(uuid::Uuid::from_u128(9));
        let workspaces = collaboration::RouteSpec::app("so.tilo.lang.notes", workspace);
        let owns = collaboration::RouteSpec::record("so.tilo.lang.notes", workspace, own.route());
        assert!(opens_record(&owns, own.as_str()));
        assert!(
            !opens_record(&workspaces, own.as_str()),
            "the workspace's record is never named: a launch that names one names another"
        );
        let other = collaboration::record::RecordId::for_file_record(uuid::Uuid::from_u128(10));
        assert!(!opens_record(&owns, other.as_str()));
        assert!(!opens_record(&owns, "not a record"));
    }
}
