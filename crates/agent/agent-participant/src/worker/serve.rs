//! The app worker's side: prepare, confine, then serve one record.

use std::cell::RefCell;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Arc;

use collab_types::DocumentMutation;
use tilo_actions::OperationKind;
use tilo_actions::door::{self, ActionAddress, ActionDescriptor, DoorError};
use tilo_content::ContentStore;
use ui::application::{
    ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::collaboration::record_content::{ContentLane, RecordContent};
use ui::headless::FrameHarness;
use ui::permissions::{PermissionContext, UserId};
use ui::portable_storage::{PortableStorage, StorageKey};
use ui::record::{RecordCheckout, RecordId, RecordOwners};

use super::effects::{EffectSession, Held};
use super::proto::{
    CONTENT_BATCH_BYTES, FRAMING, MAX_CONTENT_ITEM_BYTES, Mode, PROTOCOL_VERSION, ProbeReport,
    Reply, Request, WireDoorError,
};
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
    // The app's code finds the worker's content store wherever it runs: every
    // request is served inside this one scope.
    let scope = ui::WakeScope::new();
    ui::tilo_resource::content::register(&scope.handle(), worker.content.store.clone());
    let _entered = scope.enter();
    // The confinement denies the driver its IOKit and Mach services, so no
    // GPU starts here: an app's pictures are drawn on the CPU instead.
    ui::offscreen::disable_gpu();
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
    /// The held record's room, and whether an agent holds it.
    room: String,
    agent: bool,
    /// The held record's component, mounted for an agent's effects.
    effects: Option<EffectSession>,
    views: Option<(String, automerge::AutoCommit)>,
    /// Loaded before the confinement, for the components the worker mounts:
    /// a reader's one, or a held record's for an agent's effects.
    fonts: Option<ResidentFonts>,
    read: Option<ReadSession>,
    content: WorkerContent,
}

/// The content store the app's code reads and writes in the worker: kept in
/// memory, with what the app stores and what it lacks held for the
/// supervisor to take.
struct WorkerContent {
    /// What the supervisor put and the app stored.
    local: ContentStore,
    lane: Arc<ContentLane>,
    /// `local` joined to `lane`: the store the app is lent.
    store: ContentStore,
}

impl WorkerContent {
    fn new() -> Self {
        let local = ContentStore::memory();
        let lane = Arc::new(ContentLane::default());
        let store = RecordContent::store(local.clone(), Arc::clone(&lane));
        Self { local, lane, store }
    }

    fn put(&self, bytes: &[u8]) -> Reply {
        match self.local.put(bytes) {
            Ok(hash) => Reply::Stored(hash),
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    /// What the app stored since the last take, up to a batch, and what it
    /// asked for and lacks.
    fn take(&self) -> Reply {
        let requests = self.lane.take_requests();
        let mut offered = Vec::new();
        let mut size = 0usize;
        for (hash, bytes) in requests.offers {
            if bytes.len() > MAX_CONTENT_ITEM_BYTES {
                tracing::warn!(%hash, "app worker: stored content is too large to hand over");
            } else if offered.is_empty() || size.saturating_add(bytes.len()) <= CONTENT_BATCH_BYTES
            {
                size = size.saturating_add(bytes.len());
                offered.push(bytes);
            } else {
                self.lane.retry_offer(hash, bytes);
            }
        }
        let wanted = requests
            .wants
            .into_iter()
            .filter(|hash| !self.local.has(hash))
            .collect();
        Reply::Content { offered, wanted }
    }
}

impl Worker {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            catalogue: AppCatalogue::native(),
            replica: None,
            room: String::new(),
            agent: false,
            effects: None,
            views: None,
            fonts: matches!(mode, Mode::Read | Mode::Replica).then(ResidentFonts::load),
            read: None,
            content: WorkerContent::new(),
        }
    }

    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::Probe { path } => Reply::Probe(probe(&path)),
            Request::PutContent(bytes) => self.content.put(&bytes),
            Request::TakeContent => self.content.take(),
            Request::Open {
                app_id,
                room,
                participant,
                agent,
                seed,
            } => self.open(&app_id, &room, &participant, agent, seed),
            Request::Invoke {
                address,
                arguments,
                confirm,
            } => {
                if self.replica.is_none() {
                    return Reply::Failed("no record is held".into());
                }
                match (decode(&address), decode(&arguments)) {
                    (Ok(address), Ok(arguments)) => {
                        output(self.invoke(&address, &arguments, confirm), &address)
                    }
                    (Err(reason), _) | (_, Err(reason)) => Reply::Failed(reason),
                }
            }
            Request::ViewsLoad { app_id, doc } => self.load_views(app_id, &doc),
            Request::Views { wanted, policy } => self.views(&wanted, &policy),
            Request::Title { app_id, doc } => self.title(&app_id, &doc),
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
            Request::Survey { effects } => {
                let mut offered = replica.survey();
                if effects && self.agent {
                    match self.effect_session() {
                        Ok(session) => offered.extend(session.offered()),
                        Err(reason) => {
                            tracing::warn!(%reason, "app worker: no effects to offer");
                        }
                    }
                }
                descriptors(&offered)
            }
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
                room.clone_into(&mut self.room);
                self.agent = agent;
                Reply::Opened { record }
            }
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    /// Perform one operation of the held record: a document operation on
    /// the replica, or, for an agent, an effect or view in the record's
    /// mounted component.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<tilo_actions::ActionOutput, DoorError> {
        let Some(replica) = self.replica.as_mut() else {
            return Err(DoorError::Unknown(address.clone()));
        };
        // The record's tree carries its effects and views too (a row's
        // export, a document's read, a look at an artwork): only what it
        // names as a document operation runs on the replica, everything else
        // in the mounted component.
        let document = door::resolve(&replica.resources(), address)
            .is_some_and(|spec| spec.kind == OperationKind::Document);
        if !self.agent || document {
            return replica.invoke(address, arguments, confirm);
        }
        self.effect_session()
            .map_err(unmounted)?
            .invoke(address, arguments, confirm)
    }

    /// The held record's component, mounted for an agent's effects: the
    /// one mounted last while the replica has not moved on since, else
    /// mounted again from the replica as it stands.
    fn effect_session(&mut self) -> Result<&mut EffectSession, String> {
        let Some(replica) = self.replica.as_mut() else {
            return Err("no record is held".into());
        };
        let heads = replica.heads();
        if let Some(session) = self.effects.take().filter(|session| session.shows(&heads)) {
            return Ok(self.effects.insert(session));
        }
        let Some(fonts) = self.fonts.as_ref() else {
            return Err("this worker mounts no component".into());
        };
        let session = EffectSession::mount(
            &self.catalogue,
            Held {
                app_id: replica.app_id(),
                room: &self.room,
                replica,
            },
            self.content.store.clone(),
            fonts.renderer(),
        )?;
        Ok(self.effects.insert(session))
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

    fn title(&self, app_id: &str, bytes: &[u8]) -> Reply {
        if self.mode != Mode::Views {
            return Reply::Failed("this worker reads no titles".into());
        }
        let mut doc = automerge::AutoCommit::new();
        if let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes) {
            return Reply::Failed(error.to_string());
        }
        Reply::Title(self.catalogue.resource_title(app_id, &doc))
    }

    fn prepare_read(
        &mut self,
        app_id: &str,
        room: &str,
        record: &str,
        source: Vec<u8>,
        policy: &str,
    ) -> Reply {
        let fonts = match self.fonts.as_ref() {
            Some(fonts) if self.mode == Mode::Read && self.read.is_none() => fonts,
            _ => return Reply::Failed("this worker mounts one component".into()),
        };
        let (Some(record), Ok(policy)) = (parse_record(record), decode(policy)) else {
            return Reply::Failed("invalid read request".into());
        };
        let source = Source {
            app_id,
            room,
            record,
            bytes: source,
        };
        match ReadSession::prepare(&self.catalogue, source, &policy, fonts.renderer()) {
            Ok(read) => {
                self.read = Some(read);
                Reply::Done
            }
            Err(reason) => Reply::Failed(reason),
        }
    }
}

/// Why an effect's component could not be mounted.
fn unmounted(reason: String) -> DoorError {
    DoorError::Unavailable {
        code: "app_worker".into(),
        reason,
    }
}

/// The toolkit's fonts with every face's data mapped now.
///
/// Font databases name system faces by path and open the file the first time
/// a face is used; once confined, the worker can open no file, and a
/// component would find no font to lay its text out in. So every face is
/// mapped before the confinement, and shaping reads the mappings. The mapped
/// database is the process's own from then on: every text renderer made in
/// the worker — the one a component is mounted with, and those an app makes
/// for itself — is built over it.
struct ResidentFonts;

impl ResidentFonts {
    fn load() -> Self {
        let mut fonts = ui::configured_font_system();
        let database = fonts.db_mut();
        let faces: Vec<_> = database.faces().map(|face| face.id).collect();
        for face in faces {
            // SAFETY: maps an installed font file read-only. fontdb's caveat
            // is that a file truncated while mapped faults on access; font
            // files of the image are not rewritten under a running worker,
            // and a fault costs only this worker.
            let _ = unsafe { database.make_shared_face_data(face) };
        }
        // Shares the database as the one this process's host built, which
        // every later font system is made over.
        drop(ui::TextRenderer::with_browser_font_system(fonts));
        Self
    }

    /// A text renderer over the mapped faces.
    #[expect(
        clippy::unused_self,
        reason = "a renderer is made only where the fonts were mapped"
    )]
    fn renderer(&self) -> ui::TextRenderer {
        ui::TextRenderer::new()
    }
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

/// The record a component is mounted on: its app, room and id, and its
/// whole document.
pub(super) struct Source<'a> {
    pub(super) app_id: &'a str,
    pub(super) room: &'a str,
    pub(super) record: RecordId,
    pub(super) bytes: Vec<u8>,
}

/// How a record's component is mounted: on which record, as which device,
/// under whose permissions and with what content store.
pub(super) struct Mount<'a> {
    pub(super) source: Source<'a>,
    pub(super) device: &'a str,
    pub(super) permissions: Rc<RefCell<PermissionContext>>,
    pub(super) author: UserId,
    pub(super) content: Option<ContentStore>,
}

/// The instance id of the one component a worker mounts at a time.
pub(super) const INSTANCE: u64 = 1;

/// Mount a record's component headless over its document, in storage of
/// this worker's own.
pub(super) fn mount_component(
    catalogue: &AppCatalogue,
    mount: Mount<'_>,
    text_renderer: ui::TextRenderer,
) -> Result<FrameHarness, String> {
    let Mount {
        source,
        device,
        permissions,
        author,
        content,
    } = mount;
    let definition = catalogue.get(source.app_id).ok_or("unknown source app")?;
    let checkout = RecordCheckout::new(source.record, definition.record_kind.clone());
    if definition.keeps_room_record() != checkout.id.is_package_primary() {
        return Err("source record scope mismatch".into());
    }
    // Hydrate existing history; a new-record initializer must never
    // replace its deterministic genesis actor with an arriving snapshot.
    let cache = PortableStorage::new(device, device)?;
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
    let target = LaunchTarget::Record {
        checkout,
        reveal: None,
    };
    let mut context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: device.into(),
            instance_id: INSTANCE,
            permissions,
            author,
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )
    .map_err(|error| error.to_string())?;
    if let Some(content) = content {
        context.provide_content(content);
    }
    let component = definition
        .instantiate(context, target, RestoreInput::None)
        .map_err(|error| error.to_string())?;
    FrameHarness::from_component(
        component,
        (1280, 720),
        ComponentRunnerInputs {
            theme: ui::Theme::default(),
            text_renderer,
        },
    )
    .map_err(|error| error.to_string())
}

/// One record's component, mounted headless over its retained source, for
/// its registered reads.
struct ReadSession {
    frame: FrameHarness,
    permissions: Rc<RefCell<PermissionContext>>,
}

impl ReadSession {
    fn prepare(
        catalogue: &AppCatalogue,
        source: Source<'_>,
        policy: &ViewReadPolicy,
        text_renderer: ui::TextRenderer,
    ) -> Result<Self, String> {
        let permissions = Rc::new(RefCell::new(policy.permissions()));
        let frame = mount_component(
            catalogue,
            Mount {
                source,
                device: "canonical-reader",
                permissions: permissions.clone(),
                author: UserId(policy.person.clone()),
                content: None,
            },
            text_renderer,
        )?;
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
