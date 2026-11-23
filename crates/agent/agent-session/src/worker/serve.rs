//! The app worker's side: prepare, confine, then serve the records, views
//! and reads the supervisor holds in it, each in a slot of its own.

use std::collections::HashMap;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Arc;

use collab_types::DocumentMutation;
use tilo_actions::api::{ActionAddress, ActionDescriptor, ApiError};
use tilo_content::ContentStore;
use ui::application::{
    ComponentRunnerInputs, LaunchTarget, ProcessContext, ProcessContextInputs, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::collaboration::document_content::{ContentLink, DocumentContent};
use ui::headless::FrameHarness;
use ui::permissions::{MemberId, SharedPermissions};
use ui::portable_storage::{PortableStorage, StorageKey};
use ui::resource::{DocumentCheckout, DocumentId, ResourceOwners};

use super::proto::PageAction;
use super::proto::{
    CONTENT_BATCH_BYTES, FRAMING, MAX_CONTENT_ITEM_BYTES, Mode, PROTOCOL_VERSION, ProbeReport,
    Reply, Request, Slot, WireApiError,
};
use super::region::Region;
use crate::apps::{AppCatalogue, Replica, ReplicaActor, Seed};
use crate::workspace_actions::ViewReadPolicy;

/// Serve as a worker on the socket that is this process's stdin.
/// Returns the exit code.
///
/// One worker serves many slots ([`Slot`]): each record, view or read the
/// supervisor holds here has a slot of its own, with its own content store
/// and wake scope, opened by its first request and let go by
/// [`Request::Close`]. What the process prepares before it confines itself
/// (the app catalogue, the fonts) is shared by every slot, so a worker costs
/// its process once and each record only what the record holds.
///
/// Each slot answers on a thread of its own, its requests in the order they
/// came, every reply sent with its slot: a request that takes long holds up
/// its own slot and no other.
pub(super) fn serve() -> i32 {
    // SAFETY: descriptor 0 is the socket the supervisor handed this process
    // as stdin; it is taken over here and nothing else in the worker reads
    // stdin.
    let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) });
    // Each slot answers on a thread of its own: without a bound, the
    // allocator would give each its own arena, and keep what each freed.
    tilo_sandbox::confine::share_allocator_arenas();
    let catalogue = AppCatalogue::native();
    // The app's view lays out text once the filesystem is gone.
    let catalog_held = ui::text_render::hold_catalog_fonts_in_memory();
    if !catalog_held {
        tracing::warn!("app worker: no font catalog; views will lay out no text");
    }
    // A reader lays its component's text out: in the catalog held in memory,
    // or, without one, in the one renderer mapped now, for its first slot.
    let mut fonts = Fonts {
        held: catalog_held,
        resident: (!catalog_held).then(resident_text_renderer),
    };
    // The confinement denies the GPU driver its IOKit and Mach services, so
    // no adapter starts here: an app's pictures are drawn on the CPU.
    ui::offscreen::disable_gpu();
    // Read here and written from every slot's thread: the confinement
    // closes every other descriptor, a copy of this one too.
    let replies = Arc::new(ReplySocket {
        socket,
        writing: std::sync::Mutex::new(()),
    });
    let answer = |slot: Slot, reply: &Reply| answer(&replies, slot, reply);
    if let Err(reason) = super::confine::confine() {
        let _ = answer(0, &Reply::Unconfined(reason));
        return 1;
    }
    if !answer(
        0,
        &Reply::Ready {
            version: PROTOCOL_VERSION,
        },
    ) {
        return 1;
    }
    let mut slots: HashMap<Slot, std::sync::mpsc::Sender<(Region, Request)>> = HashMap::new();
    loop {
        let (slot, slot_mode, rendered_in, request): (Slot, Mode, Region, Request) =
            match FRAMING.read_value(&mut &replies.socket) {
                Ok(Some(request)) => request,
                // The supervisor went away or spoke garbage: nothing left to do.
                Ok(None) | Err(_) => return 0,
            };
        match request {
            // The slot's thread lets go of what it holds once what it was
            // asked before is answered; no one waits for that.
            Request::Close => {
                slots.remove(&slot);
            }
            Request::Probe { path } => {
                if !answer(slot, &Reply::Probe(probe(&path))) {
                    return 0;
                }
            }
            request => {
                let sent = match slots.entry(slot) {
                    std::collections::hash_map::Entry::Occupied(held) => held.into_mut(),
                    std::collections::hash_map::Entry::Vacant(free) => {
                        let text = (slot_mode == Mode::Read)
                            .then(|| fonts.renderer())
                            .flatten();
                        match open_slot(slot, slot_mode, &catalogue, text, &replies) {
                            Ok(sender) => free.insert(sender),
                            Err(reason) => {
                                if !answer(slot, &Reply::Failed(reason)) {
                                    return 0;
                                }
                                continue;
                            }
                        }
                    }
                }
                .send((rendered_in, request));
                if sent.is_err() && !answer(slot, &Reply::Failed("the slot has ended".into())) {
                    return 0;
                }
            }
        }
    }
}

/// The socket to the supervisor: requests are read from it on the main
/// thread, and replies written to it, one whole frame at a time, from
/// whichever slot's thread answers.
struct ReplySocket {
    socket: UnixStream,
    writing: std::sync::Mutex<()>,
}

type Replies = Arc<ReplySocket>;

/// Send `reply` for `slot`; `false` once the supervisor is gone.
fn answer(replies: &Replies, slot: Slot, reply: &Reply) -> bool {
    let _writing = replies
        .writing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    FRAMING
        .write_value(&mut &replies.socket, &(slot, reply))
        .is_ok()
}

/// Start `slot`'s thread: it holds what the slot holds and answers its
/// requests in order until the slot is closed.
fn open_slot(
    slot: Slot,
    mode: Mode,
    catalogue: &AppCatalogue,
    text: Option<ui::TextRenderer>,
    replies: &Replies,
) -> Result<std::sync::mpsc::Sender<(Region, Request)>, String> {
    let (sender, requests) = std::sync::mpsc::channel::<(Region, Request)>();
    let catalogue = catalogue.clone();
    let replies = Arc::clone(replies);
    std::thread::Builder::new()
        .name(format!("app-worker-slot-{slot}"))
        .stack_size(super::WORKER_STACK)
        .spawn(move || {
            let mut held = Held::new(mode, catalogue, text);
            let mut region = super::region::Installed::default();
            for (rendered_in, request) in requests {
                // What it renders shows dates and clocks as the person it is for reads them.
                region.install(rendered_in);
                // The app's code finds the slot's content store wherever it
                // runs, in or out of its view's own scope.
                let reply = {
                    let _entered = held.scope.enter();
                    held.worker.handle(request)
                };
                if !answer(&replies, slot, &reply) {
                    return;
                }
            }
        })
        .map(|_| sender)
        .map_err(|error| format!("no thread for the slot: {error}"))
}

/// How a reader's slots get the renderer their component lays its text out
/// with.
struct Fonts {
    /// The product catalog is held in memory: every slot gets a renderer
    /// over it.
    held: bool,
    /// Without it, the one renderer mapped before the confinement.
    resident: Option<ui::TextRenderer>,
}

impl Fonts {
    fn renderer(&mut self) -> Option<ui::TextRenderer> {
        if self.held {
            Some(ui::TextRenderer::with_font_system(
                ui::configured_font_system(),
            ))
        } else {
            self.resident.take()
        }
    }
}

/// One slot: what it holds, and the scope its app's code runs in. The
/// worker is dropped before the scope it was made in.
struct Held {
    worker: Worker,
    scope: ui::WakeScope,
}

impl Held {
    fn new(mode: Mode, catalogue: AppCatalogue, text: Option<ui::TextRenderer>) -> Self {
        let scope = ui::WakeScope::new();
        let worker = Worker::new(mode, catalogue, text);
        ui::tilo_resource::content::register(&scope.handle(), worker.content.store.clone());
        Self { worker, scope }
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
    /// The page the held document's view is shown as ([`Mode::Page`]): the
    /// numbers its elements carry and what it last answered.
    page: Option<ui::composition::page::Page>,
    /// What the held record's view is lent as its content store.
    content: WorkerContent,
}

/// The content store a held record's view reads and writes in the worker:
/// kept in memory, with what the app stores and what it asked for and
/// lacks held for the supervisor to take.
struct WorkerContent {
    /// What the supervisor put and the app stored.
    local: ContentStore,
    link: Arc<ContentLink>,
    /// `local` joined to `link`: the store the view is lent.
    store: ContentStore,
}

impl WorkerContent {
    fn new() -> Self {
        // It holds only what the supervisor put for the one record the slot
        // holds, of its one workspace, so a reference an action names
        // resolves in it.
        let local = ContentStore::memory().for_one_workspace();
        let link = Arc::new(ContentLink::default());
        let store = DocumentContent::store(local.clone(), Arc::clone(&link));
        Self { local, link, store }
    }

    /// Keep `bytes` as `reference` names them: sealed under its key when it
    /// carries one, so what the app reads is exactly what the workspace keeps.
    fn put(&self, reference: &str, bytes: &[u8]) -> Reply {
        match self.local.put_as(reference, bytes) {
            Ok(()) => Reply::Stored(reference.to_owned()),
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    /// What the app stored since the last take, up to a batch, and what it
    /// asked for and lacks.
    fn take(&self) -> Reply {
        let requests = self.link.take_requests();
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
                self.link.retry_offer(hash, bytes);
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
    fn new(mode: Mode, catalogue: AppCatalogue, text: Option<ui::TextRenderer>) -> Self {
        Self {
            mode,
            catalogue,
            replica: None,
            views: None,
            text,
            read: None,
            page: None,
            content: WorkerContent::new(),
        }
    }

    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::PutContent { reference, bytes } => self.content.put(&reference, &bytes),
            Request::TakeContent => self.content.take(),
            #[cfg(test)]
            Request::Stall { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(millis));
                Reply::Done
            }
            Request::Open {
                app_id,
                workspace,
                member,
                actor,
                seed,
                asks_workspace,
            } => match decode(&actor) {
                Ok(actor) => self.open(&app_id, &workspace, &member, actor, seed, asks_workspace),
                Err(reason) => Reply::Failed(reason),
            },
            Request::ViewsLoad { app_id, doc } => self.load_views(app_id, &doc),
            Request::Views { wanted, policy } => self.views(&wanted, &policy),
            Request::Title { app_id, doc } => self.title(&app_id, &doc),
            Request::Entries { app_id, doc } => self.entries(&app_id, &doc),
            Request::ReadPrepare {
                app_id,
                workspace,
                record,
                source,
                policy,
            } => self.prepare_read(&app_id, &workspace, &record, source, &policy),
            Request::ReadPolicy { policy } => match (self.read.as_ref(), decode(&policy)) {
                (Some(read), Ok(policy)) => {
                    read.set_policy(&policy);
                    Reply::Done
                }
                (None, _) => Reply::Failed("no component is mounted".into()),
                (_, Err(reason)) => Reply::Failed(reason),
            },
            Request::ReadRegisters => match self.read.as_ref() {
                Some(read) => Reply::Registers(ui::semantic_document_read::registers_readers(
                    read.frame.root_widget(),
                )),
                None => Reply::Failed("no component is mounted".into()),
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
            Request::PageShow { show } => self.page_show(show.as_deref()),
            Request::PageAct { id, target, action } => {
                self.page_act(id, target.as_deref(), &action)
            }
            Request::PagePatch => self.page_patch(),
            request => self.replica_request(request),
        }
    }

    /// The held document's view as a whole page, after activating `show`.
    fn page_show(&mut self, show: Option<&str>) -> Reply {
        let Some(replica) = self.replica.as_mut().filter(|_| self.mode == Mode::Page) else {
            return Reply::Failed("this worker shows no page here".into());
        };
        // The address names what to show by the semantic target its element
        // carries (a note's row): activated as a click on that row is.
        if let Some(show) = show {
            let (composition, _) = replica.page();
            let shown = widget_of_target(&composition, show)
                .ok_or_else(|| "no element carries it".to_owned())
                .and_then(|widget| {
                    replica.act_on_page(&widget, Some(show), &ui::a11y::Action::Activate)
                });
            if let Err(reason) = shown {
                tracing::debug!(%reason, show, "app worker: the page's address names nothing it shows");
            }
        }
        let (composition, theme) = replica.page();
        let title = self
            .catalogue
            .get(replica.app_id())
            .map(|app| app.title().to_owned())
            .unwrap_or_default();
        let page = self.page.insert(ui::composition::page::Page::new());
        Reply::Shown {
            title,
            css: ui::composition::page::theme_css(&theme) + ui::composition::page::PAGE_CSS,
            body: page.render(&composition),
        }
    }

    /// What a person did on the page, acted on the view, and what it changed.
    /// A page not rendered here yet (its worker was replaced) acts on
    /// nothing and answers itself whole.
    fn page_act(&mut self, id: u32, target: Option<&str>, action: &PageAction) -> Reply {
        let Some(replica) = self.replica.as_mut().filter(|_| self.mode == Mode::Page) else {
            return Reply::Failed("this worker shows no page here".into());
        };
        let Some(semantic) = super::proto::semantic(action) else {
            if let PageAction::Resize { width, height } = action {
                // The view lays itself out at the page's size, as a window at its own: what a
                // node's bounds say is then where the page shows it.
                if let Err(reason) = replica.resize_page((*width, *height)) {
                    tracing::debug!(%reason, "app worker: the page's view did not lay out at its size");
                }
            }
            return self.page_patch();
        };
        // An element a page numbers for a part of a widget (a document's block,
        // a control) is acted on through the widget it is a part of.
        if let Some(path) = self.page.as_ref().and_then(|page| page.path(id)) {
            let widget = path
                .split_once('#')
                .map_or(path, |(widget, _)| widget)
                .to_owned();
            if let Err(reason) = replica.act_on_page(&widget, target, &semantic) {
                tracing::debug!(%reason, widget, "app worker: the page's action was refused");
            }
        }
        self.page_patch()
    }

    /// What changed on the page since it last answered.
    fn page_patch(&mut self) -> Reply {
        let Some(replica) = self.replica.as_ref().filter(|_| self.mode == Mode::Page) else {
            return Reply::Failed("this worker shows no page here".into());
        };
        let (composition, _) = replica.page();
        let patches = self
            .page
            .get_or_insert_with(ui::composition::page::Page::new)
            .patch(&composition);
        Reply::Patches(
            patches
                .into_iter()
                .map(|patch| (patch.id, patch.html))
                .collect(),
        )
    }

    fn replica_request(&mut self, request: Request) -> Reply {
        let Some(replica) = self.replica.as_mut() else {
            return Reply::Failed("no record is held".into());
        };
        match request {
            Request::Survey => descriptors(&replica.survey()),
            Request::SurveyRows => {
                let held = replica.survey_rows();
                let total = u64::try_from(held.len()).unwrap_or(u64::MAX);
                match serde_json::to_string(&held) {
                    Ok(rows) => Reply::Page {
                        total,
                        rows,
                        arriving: false,
                    },
                    Err(error) => Reply::Failed(error.to_string()),
                }
            }
            Request::SurveyPage {
                query,
                can_render,
                offset,
                take,
            } => match decode::<tilo_actions::remote::CatalogueQuery>(&query) {
                Ok(query) => {
                    let window = (
                        usize::try_from(offset).unwrap_or(usize::MAX),
                        usize::try_from(take).unwrap_or(usize::MAX),
                    );
                    let (total, page, arriving) =
                        replica.survey_page_arriving(&query, (can_render, window));
                    match serde_json::to_string(&page) {
                        Ok(rows) => Reply::Page {
                            total: u64::try_from(total).unwrap_or(u64::MAX),
                            rows,
                            arriving,
                        },
                        Err(error) => Reply::Failed(error.to_string()),
                    }
                }
                Err(reason) => Reply::Failed(reason),
            },
            Request::Resolve { address } => match decode(&address) {
                Ok(address) => {
                    descriptors(&replica.descriptor(&address).into_iter().collect::<Vec<_>>())
                }
                Err(reason) => Reply::Failed(reason),
            },
            Request::Invoke {
                address,
                arguments,
                confirm,
                invoker,
                context,
                appearance,
                members,
                given,
            } => {
                ui::tilo_resource::members::know(members);
                // The arguments naming content the workspace keeps already are
                // taken by reference: no bytes of it crossed for them.
                ui::tilo_resource::content::given(&given, || {
                    invoked(
                        replica,
                        (&address, &arguments, confirm),
                        (invoker, context, appearance),
                    )
                })
            }
            Request::TakeMutation => taken_mutation(replica),
            Request::Apply { snapshot, bytes } => {
                applied(replica.apply(&mutation(snapshot, bytes)))
            }
            Request::ApplyDocument {
                record,
                snapshot,
                bytes,
            } => document_change(&record, mutation(snapshot, bytes), |record, change| {
                applied(replica.apply_document(record, &change))
            }),
            Request::TakeDocumentMutations => taken_document_mutations(replica),
            request @ (Request::AskWorkspace | Request::Wanted | Request::Answered { .. }) => {
                workspace_reply(replica, request)
            }
            Request::Renew { actor } => renewed(replica, &actor),
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
        workspace: &str,
        member: &MemberId,
        actor: ReplicaActor,
        seed: Option<(String, Vec<Vec<u8>>)>,
        asks_workspace: bool,
    ) -> Reply {
        if !matches!(self.mode, Mode::Replica | Mode::Page) || self.replica.is_some() {
            return Reply::Failed("this slot holds one record".into());
        }
        let seed = match seed {
            None => Seed::New,
            Some((record, changes)) => match parse_document(&record) {
                Some(record) if changes.is_empty() => Seed::NewAt(record),
                Some(record) => Seed::Existing { record, changes },
                None => return Reply::Failed("invalid record id".into()),
            },
        };
        match Replica::open_lending(
            &self.catalogue,
            app_id,
            workspace,
            member,
            actor,
            seed,
            asks_workspace,
            Some(self.content.store.clone()),
        ) {
            Ok(replica) => {
                let record = replica.document_id().as_str().to_owned();
                self.replica = Some(replica);
                Reply::Opened { record }
            }
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    fn load_views(&mut self, app_id: String, bytes: &[u8]) -> Reply {
        if self.mode != Mode::Views || self.views.is_some() {
            return Reply::Failed("this slot reads one document".into());
        }
        let mut doc = automerge::AutoCommit::new();
        if let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes) {
            return Reply::Failed(error.to_string());
        }
        self.views = Some((app_id, doc));
        Reply::Done
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

    fn entries(&self, app_id: &str, bytes: &[u8]) -> Reply {
        if self.mode != Mode::Views {
            return Reply::Failed("this worker reads no records for search".into());
        }
        let mut doc = automerge::AutoCommit::new();
        if let Err(error) = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes) {
            return Reply::Failed(error.to_string());
        }
        let entries = ui::framework::resource_index::hosted_entries(app_id, &doc);
        match serde_json::to_string(&entries) {
            Ok(entries) => Reply::Entries(entries),
            Err(error) => Reply::Failed(error.to_string()),
        }
    }

    /// The views of one resource under a policy: of the document loaded for
    /// it, or of the record this worker holds, as it holds it now.
    fn views(&self, wanted: &str, policy: &str) -> Reply {
        let (wanted, policy) = match (
            decode::<tilo_actions::remote::ViewResourceQuery>(wanted),
            decode::<ViewReadPolicy>(policy),
        ) {
            (Ok(wanted), Ok(policy)) => (wanted, policy),
            (Err(reason), _) | (_, Err(reason)) => return Reply::Failed(reason),
        };
        let permissions = policy.permissions();
        match (self.views.as_ref(), self.replica.as_ref()) {
            (Some((app_id, doc)), _) => descriptors(&self.catalogue.resource_views(
                app_id,
                doc,
                &wanted,
                &permissions,
            )),
            (None, Some(replica)) => {
                descriptors(&replica.resource_views(&wanted.id, &wanted.kind, &permissions))
            }
            (None, None) => Reply::Failed("no document is loaded".into()),
        }
    }

    fn prepare_read(
        &mut self,
        app_id: &str,
        workspace: &str,
        record: &str,
        source: Vec<u8>,
        policy: &str,
    ) -> Reply {
        let Some(text_renderer) = self.text.take() else {
            return Reply::Failed("this slot mounts one component".into());
        };
        let (Some(record), Ok(policy)) = (parse_document(record), decode(policy)) else {
            return Reply::Failed("invalid read request".into());
        };
        let source = ReadSource {
            app_id,
            workspace,
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
///
/// A database read from the toolkit's font index already names its faces
/// by shared data, but maps each file only when its bytes are first read;
/// reading every face's data once here maps those too.
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
        let _ = database.with_face_data(face, |data, _| data.len());
    }
    ui::TextRenderer::with_font_system(fonts)
}

/// The change a wire pair names: a whole snapshot or one incremental change.
fn mutation(snapshot: bool, bytes: Vec<u8>) -> DocumentMutation {
    if snapshot {
        DocumentMutation::Snapshot(bytes)
    } else {
        DocumentMutation::Incremental(bytes)
    }
}

/// A change as the wire names it: whether it is a snapshot, and its bytes.
fn wire_mutation(mutation: DocumentMutation) -> (bool, Vec<u8>) {
    match mutation {
        DocumentMutation::Snapshot(bytes) => (true, bytes),
        DocumentMutation::Incremental(bytes) => (false, bytes),
    }
}

/// What the record `wire` names answers about `change`, or the reply that says
/// `wire` names no record.
fn document_change(
    wire: &str,
    change: DocumentMutation,
    asked: impl FnOnce(&DocumentId, DocumentMutation) -> Reply,
) -> Reply {
    match parse_document(wire) {
        Some(record) => asked(&record, change),
        None => Reply::Failed(format!("{wire} is not a record id")),
    }
}

/// The reply to a change the record took, or why it did not take it.
fn applied(taken: Result<(), crate::apps::AppError>) -> Reply {
    match taken {
        Ok(()) => Reply::Done,
        Err(error) => Reply::Failed(error.to_string()),
    }
}

/// What a held record's store answers about the records it reads through its
/// workspace's hosts.
fn workspace_reply(replica: &mut crate::Replica, request: Request) -> Reply {
    match request {
        Request::AskWorkspace => {
            replica.ask_workspace_for_missing();
            Reply::Done
        }
        Request::Wanted => Reply::Wanted(
            replica
                .wanted_resources()
                .into_iter()
                .map(|record| record.as_str().to_owned())
                .collect(),
        ),
        Request::Answered { record, answer } => match parse_document(&record) {
            Some(record) => {
                replica.workspace_answered(record, answer);
                Reply::Done
            }
            None => Reply::Failed("invalid record id".into()),
        },
        _ => Reply::Failed("not a request about the workspace's records".into()),
    }
}

/// A record id as [`DocumentId::as_str`] spells it.
pub(super) fn parse_document(text: &str) -> Option<DocumentId> {
    let primary = DocumentId::package_primary();
    if text == primary.as_str() {
        Some(primary)
    } else {
        DocumentId::parse(text)
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

fn output(result: Result<tilo_actions::ActionOutput, ApiError>, at: &ActionAddress) -> Reply {
    match result {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(text) => Reply::Output(text),
            Err(error) => Reply::Refused(WireApiError::from_api(&ApiError::Failed(format!(
                "{at:?}: the output does not encode: {error}"
            )))),
        },
        Err(error) => Reply::Refused(WireApiError::from_api(&error)),
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

/// The record a reader mounts: its app, workspace and id, and the retained
/// source the host replayed.
struct ReadSource<'a> {
    app_id: &'a str,
    workspace: &'a str,
    record: DocumentId,
    bytes: Vec<u8>,
}

/// One record's component, mounted headless over its retained source, for
/// its registered reads.
struct ReadSession {
    frame: FrameHarness,
    permissions: SharedPermissions,
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
        let checkout = DocumentCheckout::new(source.record, definition.resource_type.clone());
        if definition.keeps_workspace_resource() != checkout.id.is_package_primary() {
            return Err("source record scope mismatch".into());
        }
        // Hydrate existing history; a new-record initializer must never
        // replace its deterministic genesis actor with an arriving snapshot.
        let cache = PortableStorage::new("canonical-reader", "canonical-reader")?;
        let key = if checkout.id.is_package_primary() {
            StorageKey::Primary {
                package: definition.app_id().into(),
                workspace: source.workspace.to_owned(),
            }
        } else {
            StorageKey::Record {
                package: definition.app_id().into(),
                workspace: source.workspace.to_owned(),
                record: checkout.id.as_str().into(),
            }
        };
        cache.hydrate(
            key,
            source.bytes,
            Some(automerge::ActorId::random().to_bytes().to_vec()),
        )?;
        let records = ResourceOwners::portable(cache.clone())
            .open(definition.app_id(), source.workspace)
            .map_err(|error| error.to_string())?;
        let views = Rc::new(
            InstanceViewStore::portable(cache, source.workspace)
                .map_err(|error| error.to_string())?,
        );
        let permissions = SharedPermissions::new(policy.permissions());
        let target = LaunchTarget::Record {
            checkout,
            reveal: None,
        };
        let context = ProcessContext::prepare(
            definition,
            ProcessContextInputs {
                records,
                views,
                device_id: "canonical-reader".into(),
                instance_id: INSTANCE,
                permissions: permissions.clone(),
                author: MemberId::person(policy.access.person_id().unwrap_or_default().to_owned()),
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
        self.permissions.set(policy.permissions());
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
    ) -> Result<tilo_actions::ActionOutput, ApiError> {
        let refused = || ApiError::Unknown(address.clone());
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

/// The replica's own changes since the last take, or why they could not be
/// collected: a failure, never "nothing changed", so a write that made them
/// is not answered as done.
/// [`crate::Replica::renew`] as the worker answers it.
/// `replica`'s answer to an invocation of `address` with `arguments`, as the caller pinned who
/// invoked it, in what context and appearing how (each JSON, when given).
fn invoked(
    replica: &mut crate::Replica,
    (address, arguments, confirm): (&str, &str, bool),
    (invoker, context, appearance): (Option<String>, Option<String>, Option<String>),
) -> Reply {
    let invoker = invoker.as_deref().map(decode).transpose();
    let context = context.as_deref().map(decode).transpose();
    let appearance = appearance.as_deref().map(decode).transpose();
    let scope = invoker.and_then(|invoker| {
        context.and_then(|context| appearance.map(|appearance| (invoker, context, appearance)))
    });
    match (decode(address), decode(arguments), scope) {
        (Ok(address), Ok(arguments), Ok((invoker, context, appearance))) => output(
            replica.invoke_within(invoker, context, appearance, &address, &arguments, confirm),
            &address,
        ),
        (Err(reason), ..) | (_, Err(reason), _) | (.., Err(reason)) => Reply::Failed(reason),
    }
}

fn renewed(replica: &mut crate::Replica, actor: &str) -> Reply {
    match decode(actor).map(|actor| replica.renew(actor)) {
        Ok(Ok(())) => Reply::Done,
        Ok(Err(error)) => Reply::Failed(error.to_string()),
        Err(reason) => Reply::Failed(reason),
    }
}

fn taken_mutation(replica: &mut crate::Replica) -> Reply {
    match replica.take_mutation() {
        Ok(mutation) => Reply::Mutation(mutation.map(wire_mutation)),
        Err(error) => Reply::Failed(error.to_string()),
    }
}

/// [`taken_mutation`], for the records held beside the replica's own: the
/// changes it made, and, for a part it started, the part whole.
fn taken_document_mutations(replica: &mut crate::Replica) -> Reply {
    match replica.take_document_handover() {
        Ok(taken) => Reply::DocumentMutations(
            taken
                .into_iter()
                .map(|handed| super::proto::TakenDocument {
                    record: handed.document.as_str().to_owned(),
                    changes: match handed.changes {
                        DocumentMutation::Snapshot(bytes)
                        | DocumentMutation::Incremental(bytes) => bytes,
                    },
                    whole: handed.whole,
                })
                .collect(),
        ),
        Err(error) => Reply::Failed(error.to_string()),
    }
}

/// The path of the widget whose element carries the semantic target `target`.
fn widget_of_target(composition: &ui::composition::Composition, target: &str) -> Option<String> {
    fn find(node: &ui::composition::Composed, target: &str) -> Option<String> {
        if node
            .semantics
            .as_ref()
            .and_then(|semantics| semantics.action_target.as_deref())
            == Some(target)
        {
            return Some(node.path.clone());
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    std::iter::once(&composition.root)
        .chain(&composition.layers)
        .find_map(|node| find(node, target))
}
