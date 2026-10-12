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
    // Close inherited authority before creating any private runtime handles.
    // Confinement must not close the driver's descriptors again.
    if let Err(error) = tilo_sandbox::confine::detach() {
        let _ = FRAMING.write_value(&mut &socket, &Reply::Unconfined(error.to_string()));
        return 1;
    }
    // Tokio's I/O and signal drivers create sockets. Prepare them before
    // denying socket creation; seccomp then confines every existing thread
    // (including this driver) and every thread it subsequently starts.
    if let Err(error) = ui::rt::try_app() {
        let _ = FRAMING.write_value(&mut &socket, &Reply::Unconfined(error.to_string()));
        return 1;
    }
    let mut worker = Worker::new(mode);
    // The app's code finds the worker's content store wherever it runs: every
    // request is served inside this one scope.
    let scope = ui::WakeScope::new();
    ui::tilo_resource::content::register(&scope.handle(), worker.content.store.clone());
    let _entered = scope.enter();
    // The confinement denies the driver its IOKit and Mach services, so no
    // GPU starts here: an app's pictures are drawn on the CPU instead.
    ui::offscreen::disable_gpu();
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
    /// Whether an agent holds the record and may invoke its reads and effects.
    agent: bool,
    views: Option<(String, automerge::AutoCommit)>,
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
            agent: false,
            views: None,
            read: None,
            content: WorkerContent::new(),
        }
    }

    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::Probe { path } => probe_threads(path),
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
                let mut offered = if effects && self.agent {
                    replica.survey_with_effects()
                } else {
                    replica.survey()
                };
                for descriptor in &mut offered {
                    if descriptor.kind != OperationKind::Document {
                        descriptor.params.retain(|param| param.name != "path");
                    }
                }
                descriptors(&offered)
            }
            Request::Permissions { standing } => {
                match decode::<ui::permissions::policy::Standing>(&standing) {
                    Ok(standing) => {
                        replica.set_permissions(standing);
                        Reply::Done
                    }
                    Err(reason) => Reply::Failed(reason),
                }
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
        match Replica::open_lending(
            &self.catalogue,
            app_id,
            room,
            participant,
            agent,
            seed,
            Some(self.content.store.clone()),
        ) {
            Ok(replica) => {
                let record = replica.record_id().as_str().to_owned();
                self.replica = Some(replica);
                self.agent = agent;
                Reply::Opened { record }
            }
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    /// Run every operation in the record's one live attached view. The
    /// worker restricts device access; the view's door enforces the current
    /// action kind, grants, confirmation and strict argument binding.
    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<tilo_actions::ActionOutput, DoorError> {
        let Some(replica) = self.replica.as_mut() else {
            return Err(DoorError::Unknown(address.clone()));
        };
        let resources = replica.resources();
        let spec = door::resolve(&resources, address)
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        if self.agent {
            admit_agent(spec, arguments)?;
        } else if spec.kind != OperationKind::Document {
            return Err(DoorError::Unknown(address.clone()));
        }
        replica.invoke(address, arguments, confirm)
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
        if self.mode != Mode::Read || self.read.is_some() {
            return Reply::Failed("this worker mounts one component".into());
        }
        let (Some(record), Ok(policy)) = (parse_record(record), decode(policy)) else {
            return Reply::Failed("invalid read request".into());
        };
        let source = Source {
            app_id,
            room,
            record,
            bytes: source,
        };
        match ReadSession::prepare(&self.catalogue, source, &policy, self.content.store.clone()) {
            Ok(read) => {
                self.read = Some(read);
                Reply::Done
            }
            Err(reason) => Reply::Failed(reason),
        }
    }
}

/// Host confinement is stricter than action grants: even a confirmed
/// Outside action cannot reach a person's device or choose a file path.
fn admit_agent(
    spec: &tilo_actions::ActionSpec,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), DoorError> {
    if spec
        .contract
        .tags
        .iter()
        .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY)
    {
        return Err(DoorError::Unavailable {
            code: "local_device_only".into(),
            reason: format!(
                "{} uses the files or hardware of a person's device; an agent does not run it",
                spec.label
            ),
        });
    }
    if spec.kind != OperationKind::Document && arguments.contains_key("path") {
        return Err(DoorError::Unavailable {
            code: "effect_path".into(),
            reason: "an agent's effect writes to no path; leave `path` out and what it makes is \
                     kept as content"
                .into(),
        });
    }
    Ok(())
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

/// Probe both the pre-existing runtime driver and a thread it creates after
/// confinement, not just the thread that installed the policy.
fn probe_threads(path: String) -> Reply {
    let mut report = probe(&path);
    let runtime = ui::rt::app();
    let on_driver = path.clone();
    let result = runtime.block_on(async {
        let driver = runtime.spawn(async move { probe(&on_driver) }).await?;
        let blocking = runtime.spawn_blocking(move || probe(&path)).await?;
        Ok::<_, tokio::task::JoinError>([driver, blocking])
    });
    match result {
        Ok(Ok(threads)) => {
            for thread in threads {
                report.opened_file |= thread.opened_file;
                report.created_file |= thread.created_file;
                report.inet_socket |= thread.inet_socket;
                report.exec |= thread.exec;
            }
            Reply::Probe(report)
        }
        other => Reply::Failed(format!("the runtime confinement probe failed: {other:?}")),
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
    app_id: &'static str,
    frame: FrameHarness,
    permissions: Rc<RefCell<PermissionContext>>,
}

impl ReadSession {
    fn prepare(
        catalogue: &AppCatalogue,
        source: Source<'_>,
        policy: &ViewReadPolicy,
        content: ContentStore,
    ) -> Result<Self, String> {
        let app_id = catalogue
            .get(source.app_id)
            .ok_or("unknown source app")?
            .app_id;
        let permissions = Rc::new(RefCell::new(policy.permissions()));
        let frame = mount_component(
            catalogue,
            Mount {
                source,
                device: "canonical-reader",
                permissions: permissions.clone(),
                author: UserId(policy.standing.person.clone()),
                content: Some(content),
            },
            ui::TextRenderer::new(),
        )?;
        Ok(Self {
            app_id,
            frame,
            permissions,
        })
    }

    fn set_policy(&self, policy: &ViewReadPolicy) {
        *self.permissions.borrow_mut() = policy.permissions();
    }

    fn operations(&self) -> Vec<ActionDescriptor> {
        let permissions = self.permissions.borrow();
        let resources: Vec<_> = self
            .frame
            .resources()
            .into_iter()
            .filter_map(|resource| permissions.permitted_resource(self.app_id, resource))
            .collect();
        ui::semantic_document_read::registered_operations(self.frame.root_widget(), &resources)
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
