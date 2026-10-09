//! The app worker's side: prepare, confine, then serve one record.

use std::cell::RefCell;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use collab_types::DocumentMutation;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use ui::application::{
    ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, UserId};
use ui::portable_storage::{PortableStorage, StorageKey};
use ui::record::{RecordCheckout, RecordId, RecordOwners};

use super::proto::{FRAMING, Mode, PROTOCOL_VERSION, ProbeReport, Reply, Request, WireDoorError};
use crate::apps::{AppCatalogue, Replica, Seed};
use crate::room_actions::ViewReadPolicy;

/// Serve as a worker in `mode` on the socket that is this process's stdin.
/// Returns the exit code.
pub(super) fn serve(mode: Mode) -> i32 {
    // SAFETY: descriptor 0 is the socket the supervisor handed this process
    // as stdin; it is taken over here and nothing else in the worker reads
    // stdin.
    let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) });
    let mut worker = Worker::new(mode);
    // The app's view lays out text once the filesystem is gone.
    if !ui::text_render::hold_catalog_fonts_in_memory() {
        tracing::warn!("app worker: no font catalog; views will lay out no text");
    }
    if let Err(reason) = super::confine::confine() {
        let _ = FRAMING.write_value(&mut &socket, &Reply::Unconfined(reason));
        return 1;
    }
    if FRAMING
        .write_value(
            &mut &socket,
            &Reply::Ready {
                version: PROTOCOL_VERSION,
            },
        )
        .is_err()
    {
        return 1;
    }
    loop {
        let request: Request = match FRAMING.read_value(&mut &socket) {
            Ok(Some(request)) => request,
            // The supervisor went away or spoke garbage: nothing left to do.
            Ok(None) | Err(_) => return 0,
        };
        let reply = worker.handle(request);
        if FRAMING.write_value(&mut &socket, &reply).is_err() {
            return 0;
        }
    }
}

/// What the worker holds.
struct Worker {
    mode: Mode,
    catalogue: AppCatalogue,
    replica: Option<Replica>,
    views: Option<(String, automerge::AutoCommit)>,
    /// Loaded before the confinement, for the one component a reader mounts.
    text: Option<ui::TextRenderer>,
    read: Option<ReadSession>,
}

impl Worker {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            catalogue: AppCatalogue::native(),
            replica: None,
            views: None,
            text: (mode == Mode::Read).then(resident_text_renderer),
            read: None,
        }
    }

    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::Probe { path } => Reply::Probe(probe(&path)),
            Request::Open {
                app_id,
                room,
                participant,
                agent,
                seed,
            } => self.open(&app_id, &room, &participant, agent, seed),
            Request::ViewsLoad { app_id, doc } => self.load_views(app_id, &doc),
            Request::Views { wanted, policy } => self.views(&wanted, &policy),
            Request::ReadPrepare {
                app_id,
                room,
                record,
                source,
                policy,
            } => self.prepare_read(&app_id, &room, &record, source, &policy),
            Request::ReadPolicy { policy } => match (self.read.as_ref(), decode(&policy)) {
                (Some(read), Ok(policy)) => {
                    read.set_policy(&policy);
                    Reply::Done
                }
                (None, _) => Reply::Failed("no component is mounted".into()),
                (_, Err(reason)) => Reply::Failed(reason),
            },
            Request::ReadOperations => match self.read.as_ref() {
                Some(read) => descriptors(&read.operations()),
                None => Reply::Failed("no component is mounted".into()),
            },
            Request::ReadInvoke { address, arguments } => {
                let Some(read) = self.read.as_mut() else {
                    return Reply::Failed("no component is mounted".into());
                };
                match (decode(&address), decode(&arguments)) {
                    (Ok(address), Ok(arguments)) => {
                        output(read.invoke(&address, &arguments), &address)
                    }
                    (Err(reason), _) | (_, Err(reason)) => Reply::Failed(reason),
                }
            }
            request => self.replica_request(request),
        }
    }

    fn replica_request(&mut self, request: Request) -> Reply {
        let Some(replica) = self.replica.as_mut() else {
            return Reply::Failed("no record is held".into());
        };
        match request {
            Request::Survey => descriptors(&replica.survey()),
            Request::Invoke {
                address,
                arguments,
                confirm,
            } => match (decode(&address), decode(&arguments)) {
                (Ok(address), Ok(arguments)) => {
                    output(replica.invoke(&address, &arguments, confirm), &address)
                }
                (Err(reason), _) | (_, Err(reason)) => Reply::Failed(reason),
            },
            Request::TakeMutation => {
                Reply::Mutation(replica.take_mutation().map(|mutation| match mutation {
                    DocumentMutation::Snapshot(bytes) => (true, bytes),
                    DocumentMutation::Incremental(bytes) => (false, bytes),
                }))
            }
            Request::Apply { snapshot, bytes } => {
                let mutation = if snapshot {
                    DocumentMutation::Snapshot(bytes)
                } else {
                    DocumentMutation::Incremental(bytes)
                };
                match replica.apply(&mutation) {
                    Ok(()) => Reply::Done,
                    Err(error) => Reply::Failed(error.to_string()),
                }
            }
            Request::Presence => Reply::Presence(
                replica
                    .presence()
                    .and_then(|place| serde_json::to_string(&place).ok()),
            ),
            _ => Reply::Failed("not a request a held record answers".into()),
        }
    }

    fn open(
        &mut self,
        app_id: &str,
        room: &str,
        participant: &str,
        agent: bool,
        seed: Option<(String, Vec<Vec<u8>>)>,
    ) -> Reply {
        if self.mode != Mode::Replica || self.replica.is_some() {
            return Reply::Failed("this worker holds one record".into());
        }
        let seed = match seed {
            None => Seed::New,
            Some((record, changes)) => match parse_record(&record) {
                Some(record) => Seed::Existing { record, changes },
                None => return Reply::Failed("invalid record id".into()),
            },
        };
        match Replica::open(&self.catalogue, app_id, room, participant, agent, seed) {
            Ok(replica) => {
                let record = replica.record_id().as_str().to_owned();
                self.replica = Some(replica);
                Reply::Opened { record }
            }
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    fn load_views(&mut self, app_id: String, bytes: &[u8]) -> Reply {
        if self.mode != Mode::Views || self.views.is_some() {
            return Reply::Failed("this worker reads one document".into());
        }
        let mut doc = automerge::AutoCommit::new();
        if let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes) {
            return Reply::Failed(error.to_string());
        }
        self.views = Some((app_id, doc));
        Reply::Done
    }

    fn views(&self, wanted: &str, policy: &str) -> Reply {
        let Some((app_id, doc)) = self.views.as_ref() else {
            return Reply::Failed("no document is loaded".into());
        };
        match (
            decode::<tilo_actions::remote::ViewResourceQuery>(wanted),
            decode::<ViewReadPolicy>(policy),
        ) {
            (Ok(wanted), Ok(policy)) => descriptors(&self.catalogue.resource_views(
                app_id,
                doc,
                &wanted,
                &policy.permissions(),
            )),
            (Err(reason), _) | (_, Err(reason)) => Reply::Failed(reason),
        }
    }

    fn prepare_read(
        &mut self,
        app_id: &str,
        room: &str,
        record: &str,
        source: Vec<u8>,
        policy: &str,
    ) -> Reply {
        let Some(text_renderer) = self.text.take() else {
            return Reply::Failed("this worker mounts one component".into());
        };
        let (Some(record), Ok(policy)) = (parse_record(record), decode(policy)) else {
            return Reply::Failed("invalid read request".into());
        };
        let source = ReadSource {
            app_id,
            room,
            record,
            bytes: source,
        };
        match ReadSession::prepare(&self.catalogue, source, &policy, text_renderer) {
            Ok(read) => {
                self.read = Some(read);
                Reply::Done
            }
            Err(reason) => Reply::Failed(reason),
        }
    }
}

/// The toolkit's text renderer with every font face's data mapped now.
///
/// Font databases name system faces by path and open the file the first time
/// a face is used; once confined, the worker can open no file, and a
/// component would find no font to lay its text out in. So every face is
/// mapped before the confinement, and shaping reads the mappings.
fn resident_text_renderer() -> ui::TextRenderer {
    let mut fonts = ui::configured_font_system();
    let database = fonts.db_mut();
    let faces: Vec<_> = database.faces().map(|face| face.id).collect();
    for face in faces {
        // SAFETY: maps an installed font file read-only. fontdb's caveat is
        // that a file truncated while mapped faults on access; font files of
        // the image are not rewritten under a running worker, and a fault
        // costs only this worker.
        let _ = unsafe { database.make_shared_face_data(face) };
    }
    ui::TextRenderer::with_font_system(fonts)
}

/// A record id as [`RecordId::as_str`] spells it.
pub(super) fn parse_record(text: &str) -> Option<RecordId> {
    let primary = RecordId::package_primary();
    if text == primary.as_str() {
        Some(primary)
    } else {
        RecordId::parse(text)
    }
}

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}

fn descriptors(descriptors: &[ActionDescriptor]) -> Reply {
    match serde_json::to_string(descriptors) {
        Ok(text) => Reply::Descriptors(text),
        Err(error) => Reply::Failed(error.to_string()),
    }
}

fn output(result: Result<tilo_actions::ActionOutput, DoorError>, at: &ActionAddress) -> Reply {
    match result {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(text) => Reply::Output(text),
            Err(error) => Reply::Refused(WireDoorError::from_door(&DoorError::Failed(format!(
                "{at:?}: the output does not encode: {error}"
            )))),
        },
        Err(error) => Reply::Refused(WireDoorError::from_door(&error)),
    }
}

/// What the confinement lets this worker do at `path`.
fn probe(path: &str) -> ProbeReport {
    let path = std::path::Path::new(path);
    let created = path.with_extension("probe");
    ProbeReport {
        opened_file: std::fs::File::open(path).is_ok(),
        created_file: std::fs::File::create(&created).is_ok(),
        inet_socket: std::net::TcpStream::connect("127.0.0.1:9").is_ok()
            || std::net::UdpSocket::bind("127.0.0.1:0").is_ok(),
        exec: std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("true")
            .status()
            .is_ok(),
    }
}

/// The record a reader mounts: its app, room and id, and the retained
/// source the host replayed.
struct ReadSource<'a> {
    app_id: &'a str,
    room: &'a str,
    record: RecordId,
    bytes: Vec<u8>,
}

/// One record's component, mounted headless over its retained source, for
/// its registered reads.
struct ReadSession {
    frame: FrameHarness,
    permissions: Rc<RefCell<PermissionContext>>,
}

const INSTANCE: u64 = 1;

impl ReadSession {
    fn prepare(
        catalogue: &AppCatalogue,
        source: ReadSource<'_>,
        policy: &ViewReadPolicy,
        text_renderer: ui::TextRenderer,
    ) -> Result<Self, String> {
        let definition = catalogue.get(source.app_id).ok_or("unknown source app")?;
        let checkout = RecordCheckout::new(source.record, definition.record_kind.clone());
        if definition.keeps_room_record() != checkout.id.is_package_primary() {
            return Err("source record scope mismatch".into());
        }
        // Hydrate existing history; a new-record initializer must never
        // replace its deterministic genesis actor with an arriving snapshot.
        let cache = PortableStorage::new("canonical-reader", "canonical-reader")?;
        let key = if checkout.id.is_package_primary() {
            StorageKey::Primary {
                package: definition.app_id.into(),
                workspace: source.room.to_owned(),
            }
        } else {
            StorageKey::Record {
                package: definition.app_id.into(),
                workspace: source.room.to_owned(),
                record: checkout.id.as_str().into(),
            }
        };
        cache.hydrate(
            key,
            source.bytes,
            Some(automerge::ActorId::random().to_bytes().to_vec()),
        )?;
        let records = RecordOwners::portable(cache.clone())
            .open(definition.app_id, source.room)
            .map_err(|error| error.to_string())?;
        let views = Rc::new(
            InstanceViewStore::portable(cache, source.room).map_err(|error| error.to_string())?,
        );
        let permissions = Rc::new(RefCell::new(policy.permissions()));
        let target = LaunchTarget::Record {
            checkout,
            reveal: None,
        };
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views,
                device_id: "canonical-reader".into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: UserId(policy.person.clone()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )
        .map_err(|error| error.to_string())?;
        let component = definition
            .instantiate(context, target, RestoreInput::None)
            .map_err(|error| error.to_string())?;
        let frame = FrameHarness::from_component(
            component,
            (1280, 720),
            ComponentRunnerInputs {
                theme: ui::Theme::default(),
                text_renderer,
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(Self { frame, permissions })
    }

    fn set_policy(&self, policy: &ViewReadPolicy) {
        *self.permissions.borrow_mut() = policy.permissions();
    }

    fn operations(&self) -> Vec<ActionDescriptor> {
        ui::semantic_document_read::registered_operations(
            self.frame.root_widget(),
            &self.frame.resources(),
        )
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_actions::ActionOutput, DoorError> {
        let refused = || DoorError::Unknown(address.clone());
        if !self
            .operations()
            .iter()
            .any(|entry| &entry.address == address)
        {
            return Err(refused());
        }
        let started = self.frame.invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: address.clone(),
            },
            arguments,
        )?;
        started.completed().ok_or_else(refused)
    }
}
