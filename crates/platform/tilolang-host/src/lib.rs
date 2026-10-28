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

use std::os::fd::{AsFd as _, OwnedFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use runtime_portal::tilolang_host::{InstanceFds, InstanceRequest, Module};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilolang_tilo::background::Waker;
use tilolang_tilo::bundle::Bundle;

use hosted::{Hosted, Kept, Spec};
use phrases::Phrases;

/// The record kind of an app's room record: every tilolang package keeps its resources in it.
pub const RECORD_KIND: &str = "tilolang.room";

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

/// The host: its storage, what wakes it, and its instances.
pub struct TilolangHost {
    storage: Option<PathBuf>,
    phrases: Rc<Phrases>,
    wake: Waker,
    scope: tilo_wake::WakeScope,
    running: Vec<Running>,
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
        Ok(Kept {
            documents: Rc::new(app_state::FsStore::new(&documents)),
            indexes: Some(package.join("indexes")),
        })
    }

    /// The instances running.
    pub fn instances(&self) -> impl Iterator<Item = &Hosted> {
        self.running.iter().map(|running| &running.hosted)
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

    /// Run the instance the session handed over on the door.
    ///
    /// # Errors
    /// Its module is unreadable or declares another app, its room is no room, its connection
    /// cannot join, or its window's connection is unusable.
    pub fn admit(&mut self, request: InstanceRequest, fds: InstanceFds) -> Result<(), AdmitError> {
        let bundle = module_bundle(&fds.module, request.module_len)?;
        if bundle.meta.app != request.app {
            return Err(AdmitError::IdMismatch {
                launched: request.app,
                declared: bundle.meta.app,
            });
        }
        let room = uuid::Uuid::parse_str(&request.room)
            .map_err(|_| AdmitError::Room(request.room.clone()))?;
        // Receivers decide each change against the app's surface: its package's own, as the
        // launcher names it for a launched app.
        collaboration::app_surface::register_package(&bundle.meta.app, &bundle.surface());
        let window = Channel::from_socket(fds.window).map_err(AdmitError::Window)?;
        let watched = window
            .as_fd()
            .try_clone_to_owned()
            .map_err(AdmitError::Window)?;
        let window = ChannelTransport::new(window).map_err(AdmitError::Window)?;
        let kept = self
            .kept(&request.app)
            .map_err(|error| AdmitError::Join(error.to_string()))?;
        let _scope = self.scope.enter();
        let manager = collaboration::CollaborationManager::join_instance(
            &collaboration::record::RecordKind::new(RECORD_KIND),
            fds.broker,
            fds.identity.as_fd(),
            collaboration::RouteSpec::app(request.app.clone(), room),
            Some(request.person.clone()),
        )
        .map_err(|error| AdmitError::Join(format!("{error:?}")))?
        .finish_join()
        .map_err(|error| AdmitError::Join(error.to_string()))?;
        let spec = Spec {
            app: request.app,
            room: request.room,
            instance: request.instance,
            person: request.person,
        };
        let hosted = Hosted::open(
            spec,
            manager,
            Arc::new(bundle),
            &kept,
            Box::new(window),
            Rc::clone(&self.phrases),
            Arc::clone(&self.wake),
        )?
        .holding_identity(fds.identity);
        self.add(hosted, Some(watched));
        Ok(())
    }

    /// The window connections to watch: one closing ends its instance.
    pub fn windows(&self) -> impl Iterator<Item = std::os::fd::BorrowedFd<'_>> {
        self.running
            .iter()
            .filter_map(|running| running.window.as_ref().map(OwnedFd::as_fd))
    }

    /// One frame of every instance; an instance whose window closed ends, its record written
    /// first. Whether anything changed.
    pub fn turn(&mut self) -> bool {
        let _scope = self.scope.enter();
        let mut changed = false;
        for running in &mut self.running {
            changed |= running.hosted.turn();
        }
        let before = self.running.len();
        self.running.retain_mut(|running| {
            let open = running.window.as_ref().is_none_or(|fd| !hung_up(fd));
            if !open {
                running.hosted.flush();
                tracing::info!(
                    app = running.hosted.spec().app,
                    instance = running.hosted.spec().instance,
                    "instance ended: its window closed"
                );
            }
            open
        });
        changed || self.running.len() != before
    }

    /// Whether the host runs no instance.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
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
fn hung_up(fd: &OwnedFd) -> bool {
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
