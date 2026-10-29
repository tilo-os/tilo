//! The tilolang host: one process per room and author, without the toolkit, that runs each of
//! that author's tilolang apps in that room as an instance of its own
//! (`docs/designs/app-language/ui-host.md`, step 2b).
//!
//! The host has no app identity. The session starts it as the launch kind "tilolang host", keyed
//! by the room and the author as the launcher attributes it, in a sandbox narrower than any
//! app's: it holds only its door and its storage. Everything an instance is arrives over the door
//! from the session (`runtime_portal::tilolang_host`): its broker connection and its
//! identity-service connection, which the session opened and attested as that app's launch, its
//! verified module, and its window's patch connection. The broker attributes everything the
//! instance says to the app from the session's own record of the connection, never from the
//! host.
//!
//! Each instance ([`hosted::Hosted`]) holds its room record with its undo log and save state,
//! publishes the app's actions and runs what the broker delivers, all on its own connection, and
//! drives its window with component patches; the window sends back what the person did.
//!
//! **Storage.** One directory per room and author, granted to the host at its start, keyed by
//! package inside: `<storage>/<package>/documents` holds the room record's on-disk copy, and
//! `<storage>/<package>/indexes` the room's indexes.

pub mod hosted;
pub mod phrases;
pub mod task;

use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use std::sync::mpsc::{Receiver, TryRecvError};

use collaboration::CommittedDocument;
use collaboration::native_initial::PrimaryRead;
use runtime_portal::tilolang_host::{InstanceFds, InstanceRequest, Module};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;

use hosted::{Hosted, Kept, Spec};
use phrases::Phrases;
use tilolang_tilo::instance::room::RoomRecord;

/// The record kind of an app's room record: every tilolang package keeps its resources in it.
pub const RECORD_KIND: &str = tilolang_tilo::RECORD_KIND;

/// The route an instance of `meta`'s package for `person` in `room` joins: the person's own
/// record of the workspace for an app whose records are each member's own (`records: .own`),
/// which only their devices are admitted to, else the workspace's record of the app.
#[must_use]
pub fn route_of(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    room: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    route_under(meta, &meta.app, room, person)
}

/// [`route_of`], for the records the app keeps under `records`: its own id, or for an app
/// renamed the earliest id it had (`InstanceRequest::records_under`), whose record it keeps using
/// where it is.
#[must_use]
pub fn route_under(
    meta: &tilolang_tilo::tilolang::PackageMeta,
    records: &str,
    room: uuid::Uuid,
    person: &str,
) -> collaboration::RouteSpec {
    match tilolang_tilo::own_record(meta, person) {
        Some(record) => collaboration::RouteSpec::record(records.to_owned(), room, record),
        None => collaboration::RouteSpec::app(records.to_owned(), room),
    }
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
    #[error("the room `{0}` is no room id")]
    Room(String),
    #[error("the instance's connection could not join its room: {0}")]
    Join(String),
    #[error(transparent)]
    Open(#[from] tilolang_tilo::instance::room::RoomError),
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
    identity: Option<OwnedFd>,
    /// The connection to the app's origin connector, for an app whose types hold an outside
    /// service's resources.
    origin: Option<OwnedFd>,
    /// When the host was handed the instance, and the stages it reached since, with how long
    /// after: what an instance's launch spends in the host.
    received: std::time::Instant,
    reached: Vec<(&'static str, std::time::Duration)>,
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
    /// It runs, its window's connection watched by the second.
    Open(Box<Hosted>, OwnedFd),
}

/// The host: its storage, what wakes it, and its instances.
pub struct TilolangHost {
    storage: Option<PathBuf>,
    phrases: Rc<Phrases>,
    wake: Waker,
    scope: tilo_wake::WakeScope,
    running: Vec<Running>,
    opening: Vec<Opening>,
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
            wake,
            scope,
            running: Vec::new(),
            opening: Vec::new(),
            ended: Vec::new(),
            task: None,
        }
    }

    /// Run the programs of the task the session handed over, until the door closes it.
    ///
    /// # Errors
    /// The task's modules cannot be read or loaded.
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

    /// Answer what the task's door sent; the task ends when the door closed it.
    pub fn serve_task(&mut self) {
        if self.task.as_ref().is_some_and(|task| !task.serve()) {
            tracing::info!("task ended: the door closed it");
            self.task = None;
        }
    }

    /// Where the instances of `app` keep their room record's copy and indexes.
    ///
    /// # Errors
    /// The package's directory cannot be made.
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
    ///
    /// # Errors
    /// Its window's connection is unusable.
    pub fn admit(&mut self, request: InstanceRequest, fds: InstanceFds) -> Result<(), AdmitError> {
        let window = Channel::from_socket(fds.window).map_err(AdmitError::Window)?;
        let (sender, decoded) = std::sync::mpsc::channel();
        let (module, len, wake) = (fds.module, request.module_len, Arc::clone(&self.wake));
        let (app, instance) = (request.app.clone(), request.instance);
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
            window: Some(window),
            identity: Some(fds.identity),
            origin: fds.origin,
            received: std::time::Instant::now(),
            reached: Vec::new(),
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
            identity: None,
            origin: None,
            received: std::time::Instant::now(),
            reached: Vec::new(),
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
        let room = uuid::Uuid::parse_str(&request.room)
            .map_err(|_| AdmitError::Room(request.room.clone()))?;
        let identity =
            identity.ok_or_else(|| AdmitError::Join("no identity connection".to_owned()))?;
        let _scope = self.scope.enter();
        let manager = collaboration::CollaborationManager::join_instance(
            &collaboration::record::RecordKind::new(RECORD_KIND),
            broker,
            identity.as_fd(),
            route_under(&bundle.meta, &records, room, &request.person),
            Some(request.person.clone()),
        )
        .map_err(|error| AdmitError::Join(format!("{error:?}")))?;
        Ok(Stage::Joining {
            spec: Spec {
                app: request.app,
                room: request.room,
                instance: request.instance,
                person: request.person,
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
        let record = RoomRecord::open_read(manager, read, &kept.documents, spec.person.clone())?;
        let window = opening
            .window
            .take()
            .ok_or_else(|| AdmitError::Join("its window is gone".to_owned()))?;
        let watched = window
            .as_fd()
            .try_clone_to_owned()
            .map_err(AdmitError::Window)?;
        let transport = ChannelTransport::new(window).map_err(AdmitError::Window)?;
        let hosted = Hosted::with_record(
            spec,
            record,
            bundle,
            &kept,
            Box::new(transport),
            Rc::clone(&self.phrases),
            Arc::clone(&self.wake),
        );
        let mut hosted = match opening.identity.take() {
            Some(identity) => hosted.holding_identity(identity),
            None => hosted,
        };
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
            self.add(hosted, Some(watched));
        }
        changed
    }

    /// The window connections to watch: one closing ends its instance.
    pub fn windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.running
            .iter()
            .filter_map(|running| running.window.as_ref().map(OwnedFd::as_fd))
            .chain(
                self.opening
                    .iter()
                    .filter_map(|open| open.window.as_ref().map(AsFd::as_fd)),
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
        changed || self.running.len() != before
    }

    /// The windows whose instances ended since this was last asked: closed, or never opened.
    pub fn take_ended(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.ended)
    }

    /// Whether the host runs no instance and opens none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty() && self.opening.is_empty() && self.task.is_none()
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
