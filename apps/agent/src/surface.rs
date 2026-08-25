//! What the room's apps currently have open, and what may be done to it.
//!
//! The agent used to work from a written-down list of four things it knew how
//! to ask the word processor for. That list was checkable — a test read the
//! action names back off the app's own encoder — but it was still a list, and
//! it had exactly one app in it. Anything else in the room was unreachable, and
//! reaching a second app meant writing a second list.
//!
//! This module replaces the list with a question. An app on the qOS action
//! model publishes a [`Resource`] tree and the operations each resource exports
//! (`docs/action-model-recipe.md`); [`qos_actions::door`] turns a live tree into
//! inert, stably-addressed descriptors and performs one of them attributed to
//! whoever asked. So the agent does not need to know what an app can do — it
//! asks, every time, and what comes back is whatever the app is publishing at
//! that moment.
//!
//! ## What the agent supplies, and what it must not
//!
//! A resource tree is a function of the document **and** of what is open: the
//! word processor's sections come from the document, but which block its
//! structural inserts anchor on is a caret, and the explorer's entries are a
//! directory. [`View`] is that half, and it is the agent's *own* — the caret it
//! publishes to the room, the directory it is looking at. It is deliberately
//! not read off anybody's screen, and it carries no coordinates.
//!
//! Everything else comes from the app: the resource ids, the action ids, the
//! labels a person would see, whether an operation takes text, and the encoded
//! action itself. The agent never mints any of it, which is what keeps
//! "discovered" from meaning "guessed".
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"`, the same spelling the shell's own action
//! surface uses (`shell::shell::search::providers::app_resources`), so one
//! address names one operation whether it arrives through the shell's catalogue
//! or is built here. Never a screen position: the same address keeps meaning
//! the same operation across layouts, themes and sessions.
//!
//! ## Authority
//!
//! A denied operation is **absent** from [`offers`], not marked unavailable in
//! it — there is no "allowed" flag on an [`Offer`] for anything downstream to
//! ignore. The question asked is the one the shell's own gate asks of a caller
//! reaching an app through the door: `Write` on `(app, action)`, resolved
//! against the capability set the agent holds, and denied unless something
//! explicitly permits. The check runs again at dispatch, because a survey is a
//! snapshot and the thing that must not happen is acting on a stale one.

use std::path::PathBuf;

use automerge::AutoCommit;
use qos_actions::door::{ActionAddress, ActionDescriptor};
use qos_actions::{ActionRegistry, Resource};
use ui::permissions::{PermissionContext, PermissionScope, ResourceRef, Verb};

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/` — a file entry's id
/// is an absolute path — while an action id is a plain identifier, and neither
/// half ever contains `#`. Splitting on the last occurrence is therefore
/// unambiguous, and the whole address survives a trip through a JSON reply
/// legibly enough for a person debugging one to read it.
pub const ADDRESS_SEP: char = '#';

/// The single-string form of an address.
pub fn encode_address(address: &ActionAddress) -> String {
    format!("{}{}{}", address.resource, ADDRESS_SEP, address.action)
}

/// Recover an address from [`encode_address`]. `None` when the string is not
/// one — an address a model made up is refused here rather than looked up
/// against a half-parsed id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The app a resource id belongs to: the segment before the first `.`.
///
/// Resource ids are app-namespaced by convention across the tree
/// (`word-processor.document`, `files.entry:/…`), which is what lets the
/// capability question name the app being acted on without a second table of
/// who owns what.
pub fn app_of(resource_id: &str) -> &str {
    resource_id
        .split_once('.')
        .map(|(head, _)| head)
        .unwrap_or(resource_id)
}

/// The action id an application exports so that it can be opened.
pub const OPEN_ACTION: &str = "open";

/// The tail of the resource id an application has *as an application* rather
/// than as a document.
pub const APP_RESOURCE: &str = "app";

/// The stable kind tag on that resource. One tag across every app, because
/// what it names is the same thing in each of them.
pub const APPLICATION_KIND: &str = "applications.app";

/// The address that opens the app `namespace` names.
///
/// Inside the app's own namespace on purpose, so [`app_of`] answers it exactly
/// as it answers every other address and one vocabulary covers both questions
/// a capability can be asked about an application.
pub fn open_address(namespace: &str) -> ActionAddress {
    ActionAddress::new(format!("{namespace}.{APP_RESOURCE}"), OPEN_ACTION)
}

/// The app `address` would open, or `None` when it opens nothing.
pub fn opens(address: &ActionAddress) -> Option<&str> {
    if address.action != OPEN_ACTION {
        return None;
    }
    let (namespace, tail) = address.resource.split_once('.')?;
    (tail == APP_RESOURCE).then_some(namespace)
}

/// Where the agent is looking.
///
/// Its own view state, and the only thing it contributes to a resource tree.
/// Nothing here is a screen position: a block id and a directory path are
/// addresses, and both keep meaning the same thing on a machine with no display
/// at all.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// The block the agent's caret is on, for the app whose subject is a
    /// document. Structural inserts anchor here.
    pub focus_block: Option<String>,
    /// Where in that block.
    pub focus_offset: usize,
    /// The blocks a block-level operation would target — the agent's
    /// selection, or just the focus block.
    pub target_blocks: Vec<String>,
    /// The resource the request named, when it named one.
    ///
    /// An address, like everything else here, and the most precise thing the
    /// agent ever has: the gesture that produces it is somebody right-clicking
    /// an object and saying something about *that*, so the app that owns the
    /// object minted the id. `View::directory` says where the agent happens to
    /// be looking; this says what it was pointed at, and the two are not the
    /// same question — a request can name a photograph in a directory nobody
    /// asked the agent to browse.
    pub subject: Option<String>,
    /// The directory the agent is browsing, for an app whose subject is the
    /// filesystem.
    pub directory: Option<PathBuf>,
}

/// One app whose contract the agent speaks.
///
/// Every field is the app's own code. Adding an app is one entry: what it
/// announces, how its documents are told apart from every other app's in the
/// room, the function that builds its tree, and the decoders it registers.
/// Nothing here describes *what* the app can do — that is the tree's job, and
/// it is asked afresh each time.
pub struct AppContract {
    /// What a launched copy of this app announces to the broker.
    pub app_id: &'static str,
    /// What this app is called, in the words a sentence about it would use.
    ///
    /// The one field here that exists to be *said* rather than matched. It is
    /// the app's own name — the noun phrase that reads correctly in "I'm not
    /// allowed to open the photo editor here" — and it is written down for
    /// the same reason [`Self::app_id`] is: an app that is not open in the
    /// room has published nothing to be asked, so there is nowhere else to
    /// get it from at the moment it is needed.
    pub title: &'static str,
    /// The namespace its resource ids start with. Also the app name the
    /// capability question is phrased about.
    pub namespace: &'static str,
    /// Whether a document in the room is one this app authored.
    ///
    /// A room carries every app's state and none of it is labelled; the broker
    /// fans a frame out by address, and a participant that announced no app id
    /// reaches every subscriber. So a held replica has to be attributable, and
    /// the app is the only thing that knows its own schema.
    pub recognises: fn(&AutoCommit) -> bool,
    /// The app's own resource tree for a view of a document.
    pub resources: fn(&View, &AutoCommit) -> Vec<Resource>,
    /// The app's own decoders, so the door can prove what it would run before
    /// running it.
    pub register: fn(&mut ActionRegistry),
    /// Whether [`Self::recognises`] is a claim on **any** document rather
    /// than on this app's own.
    ///
    /// The word processor's test is "this parses as a rich-text document",
    /// and every application built on the toolkit has one of those — the
    /// toolkit seeds it. So that test is true of the photo editor's document
    /// too, and since [`recognise`] takes the first contract that claims a
    /// document, the word processor claimed all of them and no other app was
    /// ever recognised. The photo editor could be launched, could load a
    /// photograph and could publish it, and the agent still reported that
    /// nothing came back: its state arrived, was attributed to the word
    /// processor, and merged into the page.
    ///
    /// A contract marked here is tried **last**, so it wins a document only
    /// when nothing with a test of its own claims it. That is what the word
    /// processor's claim actually means: "a document, and nobody else's".
    pub fallback: bool,

    /// What this app's document currently *denotes*, as something another
    /// app's operation could be handed — or `None` for an app whose subject is
    /// not transferable.
    ///
    /// The app's own reading, for the same reason [`Self::resources`] is: only
    /// the photo editor knows that a crop rectangle and a path add up to a
    /// photograph, and an agent that worked that out for itself would be
    /// publishing its own idea of the result under the app's name.
    pub product: fn(&View, &AutoCommit) -> Option<Product>,
}

/// What one operation left behind for the next one.
///
/// The reason this exists rather than the second operation looking at the
/// document again: "put *it* under Sound check" names the thing the crop
/// produced, and a step that went back to the photo editor's document to
/// work out what that was would be re-deriving what it *hopes* happened.
/// Carrying the result forward means the second operation is handed the
/// first one's actual outcome, or nothing at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Product {
    /// Bytes, in this machine's blob store, addressed by their content hash.
    ///
    /// The hash is the whole of what travels: a document referencing it
    /// carries the descriptor (`mime`/`len`/`w`/`h`) and a peer fetches the
    /// bytes when it wants them, so a photograph never becomes a change too
    /// large to cross the wire.
    Content { hash: String },
}

impl Product {
    /// The one-string form an operation that collects content is invoked with.
    pub fn argument(&self) -> &str {
        match self {
            Product::Content { hash } => hash,
        }
    }
}

/// Nothing an app's document denotes can be handed to another operation.
fn nothing(_view: &View, _doc: &AutoCommit) -> Option<Product> {
    None
}

/// The picture in the directory the agent is browsing, as content another
/// application could be handed.
///
/// A file manager's subject is the filesystem, and a photograph sitting in it
/// is the commonest thing a request is about: "make this square" is said in a
/// room where the picture is a file, not yet anything's document. Until the
/// explorer could hand one over there was no way for that sentence to reach
/// an application that could crop it — the photo editor would be opened with
/// nothing in it and export no operation on a photograph it did not have.
///
/// The bytes are read through the explorer's own directory listing and put in
/// the blob store, because a hash is what an operation in another application
/// collects. Copying them is the point rather than a cost: what the next step
/// works on is a value, fixed at the moment it was taken, and not a file that
/// may be moved or rewritten between two steps of one sentence.
///
/// # Which picture, when there is more than one
///
/// The directory's only one. Several, and it denotes **nothing**: a step
/// bound to it is refused, which is the honest outcome when "this" could mean
/// any of them. Hidden files are skipped exactly as the explorer skips them,
/// so what can be denoted is what a person looking at the window can see.
fn file_explorer_product(view: &View, _doc: &AutoCommit) -> Option<Product> {
    // What the request pointed at, before where the agent is looking. A
    // subject naming one of this app's entries *is* the answer — it is an
    // address the app minted for a file, and it needs no directory listing to
    // resolve. Asking the browsed directory first would answer a question
    // about the photograph somebody selected with whatever happens to be in
    // the folder the agent was started on, and the film's own gesture carries
    // a subject and gives the agent no directory at all.
    if let Some(id) = view.subject.as_deref() {
        if let Some(path) = file_explorer::actions::entry_path(id) {
            return content_of(std::path::Path::new(&path));
        }
    }
    let dir = view.directory.as_ref()?;
    let mut entries = file_explorer::model::read_dir_sync(dir).ok()?;
    entries.retain(|e| {
        !e.is_dir && !e.name.starts_with('.') && e.mime.starts_with("image/")
    });
    let [only] = entries.as_slice() else {
        return None;
    };
    content_of(&only.path)
}

/// The bytes at `path`, in the blob store, as something another operation
/// could be handed. `None` when they cannot be read — a product nothing can
/// resolve is worse than no product.
fn content_of(path: &std::path::Path) -> Option<Product> {
    let bytes = std::fs::read(path)
        .map_err(|e| tracing::warn!(path = %path.display(), "cannot read the picture: {e}"))
        .ok()?;
    Some(Product::Content {
        hash: qdoc::blobstore::put(&bytes),
    })
}

/// The picture in the open document, as content another application could be
/// handed.
///
/// A word processor's document denotes its prose, which is not transferable —
/// but a picture *in* it is exactly the kind of thing another application is
/// asked to work on. "Make this square" is said in a document with a
/// photograph in it, and until the document could hand that photograph over
/// there was no way for the sentence to reach an application that could crop
/// it: the photo editor would open with nothing in it and export no operation
/// on a photograph it did not have.
///
/// The block already holds the content hash — `PlaceImage` and `InsertImage`
/// both write `blob` — so nothing is derived here and nothing is re-encoded.
/// The document's own answer about its own picture is passed along as it
/// stands.
///
/// # Which picture, when there is more than one
///
/// The one the agent's view targets, and otherwise the document's only one.
/// A document with several pictures and nothing said about which denotes
/// **nothing**: a step bound to it is then refused, which is the honest
/// outcome. Picking the first would be answering a question nobody asked, and
/// the wrong picture cropped into somebody's document is worse than a refusal
/// that says it could not tell which.
fn word_processor_product(view: &View, doc: &AutoCommit) -> Option<Product> {
    let c = qdoc::model::open(doc, &automerge::ROOT).ok()?;
    let snap = qdoc::model::materialize(doc, &c);
    let images: Vec<&qdoc::snapshot::BlockView> =
        snap.blocks.iter().filter(|b| b.kind == "image").collect();
    let blob_of = |b: &qdoc::snapshot::BlockView| match b.attrs.get("blob") {
        Some(qdoc::AttrVal::Str(h)) if !h.is_empty() => Some(h.clone()),
        // An image block with no blob is a picture whose bytes never arrived.
        // Naming its hash would hand the next operation an address for
        // content that is not there.
        _ => None,
    };
    // Targeted first: the agent's selection is an address, and a request that
    // named its subject has already said which picture it means.
    if let Some(b) = images
        .iter()
        .find(|b| view.target_blocks.iter().any(|t| *t == b.id))
    {
        return blob_of(b).map(|hash| Product::Content { hash });
    }
    match images.as_slice() {
        [only] => blob_of(only).map(|hash| Product::Content { hash }),
        _ => None,
    }
}

/// The photograph as its edits leave it: the file the document names, cropped
/// and turned the way the document says, encoded as a PNG and put in this
/// machine's blob store.
///
/// The app's own decode and its own render — the crop is stored in the
/// original's coordinates, so "the cropped photograph" is a well-defined
/// value at any moment rather than a file somebody has to save first.
fn photos_product(_view: &View, doc: &AutoCommit) -> Option<Product> {
    let image = photos::doc::PhotosDoc::image(doc)?;
    let edits = photos::doc::PhotosDoc::edits(doc);
    // Through the app's own resolver, not `fs::read`, because the path in a
    // shared document is true on exactly one machine. The photograph is staged
    // under a different absolute path in every session that has it, and this
    // participant has no window and no staging directory of its own — so the
    // path it reads here is somebody else's, and reading it works only by the
    // accident of the demo running three sessions under one uid. On two
    // machines it is a file that is not there, and what fails is the whole
    // errand: no bytes, no product, nothing to hand to the document the
    // picture was going into, and every step still reporting that it applied.
    let bytes = photos::doc::bytes_of(&image).or_else(|| {
        tracing::warn!(
            path = %image.path, content = ?image.content,
            "cannot get the photograph's bytes: neither the path in the \
             document nor its content is available to this participant",
        );
        None
    })?;
    let decoded = photos::pixels::decode(&bytes)?;
    let rendered = photos::pixels::render(&decoded, edits.crop, edits.rotate_quarters);
    let png = photos::pixels::encode_png(&rendered)?;
    Some(Product::Content {
        hash: qdoc::blobstore::put(&png),
    })
}

fn photos_resources(_view: &View, doc: &AutoCommit) -> Vec<Resource> {
    use qos_actions::ResourceProvider;
    photos::actions::PhotosResources.resources(doc)
}

fn word_processor_resources(view: &View, doc: &AutoCommit) -> Vec<Resource> {
    // The anchor for a structural insert. The agent's caret when it has one,
    // else the document's first block — the same fallback a freshly opened
    // window makes, and never an invented id.
    let anchor = view.focus_block.clone().or_else(|| {
        let c = qdoc::model::open(doc, &automerge::ROOT).ok()?;
        qdoc::model::materialize(doc, &c)
            .blocks
            .first()
            .map(|b| b.id.clone())
    });
    // A document with nothing in it is still a document, and it used to be
    // surveyed as though it were not there at all: no blocks meant no anchor,
    // no anchor meant an empty tree, and an empty tree meant a room holding an
    // open word processor reported nothing that could be done to it. Since
    // applications start empty, that is the state a document is in exactly
    // when somebody first wants to put something into it.
    //
    // The empty anchor is not an invented id — it names no block, and every
    // operation anchored on it appends to the end of the body, which is the
    // only place there is. Block-level formatting targets nothing, because
    // there is nothing to format yet.
    let anchor = anchor.unwrap_or_default();
    let targets = if !view.target_blocks.is_empty() {
        view.target_blocks.clone()
    } else if anchor.is_empty() {
        Vec::new()
    } else {
        vec![anchor.clone()]
    };
    word_processor::actions::provide_resources(
        &word_processor::actions::WpResourceCtx {
            caret_block: anchor,
            caret_offset: view.focus_offset,
            target_blocks: targets,
        },
        doc,
    )
}

fn file_explorer_resources(view: &View, doc: &AutoCommit) -> Vec<Resource> {
    let Some(directory) = view.directory.clone() else {
        return Vec::new();
    };
    // The explorer's own reader, filtered the way it filters: an agent that
    // listed the directory itself would be publishing its own idea of what is
    // there, under the app's ids.
    let mut entries = file_explorer::model::read_dir_sync(&directory).unwrap_or_default();
    entries.retain(|e| !e.name.starts_with('.'));
    file_explorer::model::sort_entries(
        file_explorer::model::SortBy::Name,
        file_explorer::model::SortOrder::Ascending,
        &mut entries,
    );
    file_explorer::actions::provide_resources(
        &file_explorer::actions::FilesResourceCtx {
            directory,
            entries,
        },
        doc,
    )
}

/// The room as chat publishes it, seen by a participant with no window open.
///
/// Every channel of the first workspace, and no active one. A live chat client
/// fills `active_channel` in from what is on screen; the agent has no screen,
/// and claiming one channel is "open" would be inventing view state rather
/// than reporting it. What it needs from the tree is the room's channels — the
/// message-level resources belong to whoever is actually looking at them.
fn chat_resources(_view: &View, doc: &AutoCommit) -> Vec<Resource> {
    let Some(ws) = chat::doc::ChatDoc::list_workspaces(doc).into_iter().next() else {
        return Vec::new();
    };
    let channels = chat::doc::ChatDoc::list_channels(doc)
        .into_iter()
        .filter(|c| c.workspace == ws.id)
        .map(|c| chat::actions::ChannelResource {
            id: c.id,
            name: c.name,
            kind: c.kind,
            archived: c.archived,
        })
        .collect();
    chat::actions::provide_resources(
        &chat::actions::ChatResourceCtx {
            me: String::new(),
            workspace_id: ws.id,
            workspace_name: ws.name,
            channels,
            active_channel: String::new(),
            messages: Vec::new(),
            participants: chat::doc::ChatDoc::list_users(doc)
                .into_iter()
                .map(|u| chat::mention::Participant {
                    id: u.id,
                    name: u.name,
                    is_agent: u.is_agent,
                })
                .collect(),
            // The agent is not in a call and cannot be. This view is built
            // from the room's document, which carries who is in a call but
            // not this peer's own audio — and there is no peer here, only a
            // participant reading. `None` is the honest answer rather than a
            // placeholder: it is what keeps mute and hang-up out of the
            // catalogue, so the agent is never offered an operation on
            // something that is not happening to it.
            call: None,
        },
        doc,
    )
}

/// Every app the agent can speak to.
///
/// Four, deliberately of different shapes: one whose subject is a rich-text
/// document, one whose subject is a host filesystem, one whose subject is a
/// conversation, and one whose subject is a photograph. None is special-cased
/// anywhere below — everything past this point works from the tree.
///
/// The photo editor is the one that makes an instruction naming two apps
/// worth having. Its operations change a photograph, and what a photograph is
/// *for* is being put somewhere else; every other app in this list can be
/// asked to do something to what it holds, and only this one produces
/// something another app can be handed.
///
/// Chat earns its place twice over. It is somewhere the agent can be asked to
/// do something, like the other two — and it is the place people are already
/// talking, which is what makes [`crate::listen`] possible at all: an agent
/// that holds the room's conversation can be spoken to where the conversation
/// is, instead of only in the one document somebody remembered to type its
/// name into.
pub static CONTRACTS: &[AppContract] = &[
    AppContract {
        app_id: "dev.qos.word-processor",
        title: "the word processor",
        namespace: "word-processor",
        recognises: |doc| qdoc::model::open(doc, &automerge::ROOT).is_ok(),
        resources: word_processor_resources,
        register: word_processor::actions::register,
        // Its test is "this parses as a rich-text document", which every
        // toolkit app's document does. Tried last, so it means "a document,
        // and nobody else's".
        fallback: true,
        product: word_processor_product,
    },
    AppContract {
        app_id: "dev.qos.file-explorer",
        title: "the file manager",
        namespace: "files",
        recognises: file_explorer::actions::is_explorer_document,
        resources: file_explorer_resources,
        register: file_explorer::actions::register,
        fallback: false,
        product: file_explorer_product,
    },
    AppContract {
        app_id: "dev.qos.chat",
        title: "the conversation",
        namespace: "chat",
        recognises: chat::actions::is_chat_document,
        resources: chat_resources,
        register: chat::actions::register,
        fallback: false,
        product: nothing,
    },
    AppContract {
        app_id: "dev.qos.photos",
        title: "the photo editor",
        namespace: "photos",
        recognises: photos_document,
        resources: photos_resources,
        register: photos::actions::register,
        fallback: false,
        product: photos_product,
    },
];

/// Whether a document is the photo editor's — asked of its own schema.
///
/// `is_empty` is the app's own test for "never written to", so its negation is
/// "this app has written here". A document it has never touched is not claimed,
/// which is what keeps a freshly seeded replica of some other app from being
/// attributed to it.
fn photos_document(doc: &AutoCommit) -> bool {
    !photos::doc::PhotosDoc::is_empty(doc)
}

/// The contract for a document, or `None` when nothing in the room's apps
/// claims it.
pub fn recognise(doc: &AutoCommit) -> Option<&'static AppContract> {
    // Two passes, and the order is the whole of it. An app with a test of its
    // own gets to claim its own document before an app whose test claims
    // anything shaped like a document at all. See [`AppContract::fallback`].
    CONTRACTS
        .iter()
        .find(|c| !c.fallback && (c.recognises)(doc))
        .or_else(|| CONTRACTS.iter().find(|c| c.fallback && (c.recognises)(doc)))
}

/// The contract whose resource ids start with `namespace`.
pub fn for_namespace(namespace: &str) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| c.namespace == namespace)
}

/// The contract for an announced app id.
pub fn for_app_id(app_id: &str) -> Option<&'static AppContract> {
    CONTRACTS.iter().find(|c| c.app_id == app_id)
}

/// Decoders for every app the agent speaks to.
///
/// One registry rather than one per app: the door decodes an encoded action
/// through it before dispatching, and a type no app registered is a refusal
/// rather than a best-effort apply.
pub fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    for c in CONTRACTS {
        (c.register)(&mut r);
    }
    r
}

/// One operation the room exports right now.
///
/// Inert: the address and the words the app puts on it, and nothing that could
/// perform it. Read-only is a property of this type's shape rather than a
/// promise — there is no callback, token or handle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The app that published it, by namespace.
    pub app: &'static str,
    pub descriptor: ActionDescriptor,
}

impl Offer {
    /// The address, in the one-string form an answer names it by.
    pub fn id(&self) -> String {
        encode_address(&self.descriptor.address)
    }

    pub fn address(&self) -> &ActionAddress {
        &self.descriptor.address
    }

    /// Whether performing it must supply text.
    pub fn wants_text(&self) -> bool {
        self.descriptor.input.wants_text()
    }

    /// Whether the text it wants is the address of content some other
    /// operation produced, rather than words the instruction can quote.
    ///
    /// The distinction the agent needs before it can supply an argument at
    /// all. There is nowhere honest to get a content hash from an instruction:
    /// nobody writes one down, and inventing one would name bytes that do not
    /// exist. So an operation that wants content can only be performed as a
    /// step *after* something that produced some.
    pub fn wants_content(&self) -> bool {
        self.descriptor.input.wants_content()
    }

    /// One line, in the app's own words: what this does and what it does it to.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} — on {} ({})",
            self.descriptor.action_label, self.descriptor.resource_label, self.descriptor.resource_kind
        );
        if let qos_actions::InputSpec::Text { prompt, collects, .. } = &self.descriptor.input {
            match collects {
                qos_actions::Collects::Words => s.push_str(&format!("; takes text: {prompt}")),
                // Said differently on purpose. A reader that is offered this
                // alongside a rename would otherwise conclude it can be
                // performed by saying something, and there is nothing anybody
                // can say that names content.
                qos_actions::Collects::Content => s.push_str(&format!(
                    "; takes what an earlier step produced, not words: {prompt}"
                )),
            }
        }
        if let Some(d) = &self.descriptor.description {
            s.push_str(&format!(". {d}"));
        }
        s
    }
}

/// The permission question performing `address` has to pass.
///
/// Phrased about the app the resource id names, not about the agent's own
/// namespace: a room grants edit rights per app, so folding every app's
/// operations under one name would make a grant in one of them a grant in all.
pub fn scope_for(address: &ActionAddress) -> PermissionScope {
    // Opening an application is not an operation on its content, so it is not
    // asked about as one. `ResourceRef::App` names the application itself —
    // the same resource the shell's own launch gate names
    // (`shell::shell::search::visibility::required_capability`, which resolves
    // a launch to `CapResource::App(package)` and `Verb::Write`) — and that is
    // what keeps the two grants distinguishable:
    //
    // * `Scope::Action { app, action }` authorises one operation inside an app
    //   that is *already running*, and confers no ability to start one. An
    //   agent handed a photograph to crop cannot spawn a process with it.
    // * `Scope::App(app)` authorises this application: both its operations and
    //   opening it. "May open **this** application."
    // * `Scope::Workspace` authorises any of them. "May open applications."
    //
    // Three different sentences in the one permission model, rather than one
    // sentence with a footnote — and the verb stays `Write` throughout,
    // because a window appearing in the room is a change to the room.
    if let Some(app) = opens(address) {
        return PermissionScope {
            resource: ResourceRef::App(app.to_string()),
            op: Verb::Write,
        };
    }
    PermissionScope {
        resource: ResourceRef::Action {
            app: app_of(&address.resource).to_string(),
            action: address.action.clone(),
        },
        op: Verb::Write,
    }
}

/// Whether the agent may perform `address`. Fail-closed: anything not
/// affirmatively allowed is denied.
pub fn permitted(perms: &PermissionContext, address: &ActionAddress) -> bool {
    perms.check(&scope_for(address))
}

/// The tree `contract` publishes for `doc` as it stands, seen from `view`.
///
/// Rebuilt on every call, never cached. An app's tree changes with every edit —
/// a new heading is a new addressable section, a renamed file is a new label —
/// and the whole value of an address is that it names what is there *now*.
pub fn tree(contract: &AppContract, view: &View, doc: &AutoCommit) -> Vec<Resource> {
    (contract.resources)(view, doc)
}

/// Everything the room exports that the agent may perform.
///
/// The union across every document it holds, in the order the apps are declared
/// and then depth-first within each — parents before children, so a caller sees
/// the document before its sections and the folder before its files.
pub fn offers(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Vec<Offer> {
    let mut out = Vec::new();
    for (contract, doc) in held {
        let resources = tree(contract, view, doc);
        for descriptor in qos_actions::door::survey(&resources) {
            // Denied means absent. Nothing downstream is handed an operation
            // it may not perform, so nothing downstream can decide to perform
            // one anyway.
            if !permitted(perms, &descriptor.address) {
                continue;
            }
            out.push(Offer {
                app: contract.namespace,
                descriptor,
            });
        }
    }
    out.extend(openable(held, view, perms));
    out
}

/// Whether the room holds `namespace`'s state **with something in it**.
///
/// Two facts wear the same word. An application is *here* when a document it
/// authored has arrived and its contract claims it. It is **surveyable** when
/// the tree it publishes for that document is not empty. The photo editor
/// writes its `edits` field the moment it starts and publishes nothing at all
/// until a photograph is in it, so between those two moments it is here and
/// exports nothing — and a step that follows an open, asking what the open
/// produced, gets the second answer while the first is what it was told.
///
/// One expression, in one place, because [`openable`] decides whether an
/// application still needs opening by this question and the session's own
/// `open_app` decides whether it has *been* opened by it. Asked two ways, the
/// survey offers an open that the open then answers with "already open",
/// having opened nothing.
pub fn surveyable(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    namespace: &str,
) -> bool {
    held.iter()
        .any(|(c, doc)| c.namespace == namespace && !(c.resources)(view, doc).is_empty())
}

/// The applications this room does not have open, as the operations that
/// would open them.
///
/// Two things bound the set, and both are the point rather than a limitation.
///
/// It is the **contracts the agent speaks**, because an app it cannot read
/// afterwards is one it could start and then have nothing to say about — an
/// agent that opened something and went quiet is worse than one that said it
/// could not.
///
/// And it is the apps that are **not already here** *with something in them*.
/// A held app needs no opening; offering it anyway invites a second window
/// over the first, and invites a request to be answered by opening the thing
/// it is about. So the set empties itself as the room fills, and the moment an
/// app arrives its own operations replace the one that asked for it.
///
/// # An open application holding nothing
///
/// "Already here" is not the same as "has something to work on", and treating
/// them as the same made an app invisible in both directions at once. The
/// photo editor opened by a person from the palette holds no photograph, so
/// [`photos::actions::PhotosResources`] exports nothing — correctly, since
/// there is no picture for a crop to be an operation *on*. Being held, it was
/// also filtered out of this set. The room therefore offered no way to crop
/// and no way to open the editor onto anything, and a model asked to make a
/// photograph square answered, accurately, that nothing there could do it.
///
/// So an app that is present but exports nothing is still openable: the open
/// is what gives it something, and the operations it then publishes are what
/// answer the rest of the sentence. The claim the exclusion protects — that a
/// request is not answered by opening the thing it is about — still holds,
/// because an app with resources of its own is still excluded.
fn openable(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Vec<Offer> {
    // Whether anything in this room denotes content an application could be
    // opened onto. Asked of the apps that are here, through the same
    // `product` an ordinary hand-off between two steps goes through — so an
    // open is offered content on exactly the terms every other operation is.
    let onto = if held
        .iter()
        .any(|(c, doc)| (c.product)(view, doc).is_some())
    {
        qos_actions::InputSpec::Text {
            prompt: "The content to open it onto".to_string(),
            placeholder: String::new(),
            collects: qos_actions::Collects::Content,
        }
    } else {
        qos_actions::InputSpec::None
    };
    CONTRACTS
        .iter()
        // Held *and holding something*. An app present but exporting nothing
        // has nothing for a request to be answered by, so offering to open it
        // onto content is the only thing that can move the work forward.
        .filter(|c| !surveyable(held, view, c.namespace))
        .map(|c| Offer {
            app: c.namespace,
            descriptor: ActionDescriptor {
                address: open_address(c.namespace),
                resource_kind: APPLICATION_KIND.to_string(),
                resource_label: c.title.to_string(),
                action_label: format!("open {}", c.title),
                description: Some(format!(
                    "{} is not open in this room, so none of what it can do is \
                     here to be named yet. Opening it is what puts its own \
                     operations on this list.",
                    c.title
                )),
                // Inert on this descriptor, but not because an open goes
                // nowhere — it is dispatched, just not through the door.
                // `Session::perform` recognises the address before the door
                // sees it and asks the session over the socket instead, so
                // nothing here is ever encoded or run against a replica.
                // Named for where the work does land: the session hosting
                // the room is the only place an application can start.
                locus: qos_actions::ExecutionLocus::ServerPreferred,
                // An open collects **content** exactly when the room has
                // content to open it onto, and nothing otherwise.
                //
                // That is not a special case: a resource tree is a function of
                // what is open, and so is this. A room holding a document with
                // a photograph in it can offer "open the photo editor onto the
                // photograph"; a room holding no picture can only offer "open
                // the photo editor", because there is nothing an argument
                // could name. Declaring content unconditionally would make the
                // plain open impossible — [`Offer::wants_content`] is what
                // sets [`Step::carries`], and a carried step with nothing
                // before it is refused rather than performed with something
                // invented.
                //
                // Read the same way the word processor's own `place-image` is
                // read, which is what makes the hand-off work in both
                // directions with one rule and no second vocabulary.
                input: onto.clone(),
            },
        })
        // The same gate every other operation passes, asked in the same place.
        // Denied means absent here too: an agent that may not open the photo
        // editor is never shown opening it as something it could choose.
        .filter(|o: &Offer| permitted(perms, o.address()))
        .collect()
}

/// One resource as its app publishes it, with the operations taken out.
///
/// The half of a resource tree a flat survey throws away. [`Offer`] carries an
/// operation and the label of whatever publishes it; a resource that exports
/// **nothing** never becomes an offer at all, so anything an app says by
/// publishing one is invisible to a caller working from the survey alone.
///
/// Apps do say things that way, and deliberately. The photo editor puts the
/// pixel dimensions of the open photograph in the label of an action-less
/// child resource — "so a caller that can only read ids and titles still
/// learns the size it is working with" (`apps/photos/src/actions.rs`) — and
/// until this existed that sentence was not true: [`qos_actions::door::survey`]
/// walks past a resource with no actions on it, and the size reached nobody.
/// Which is the difference between computing a square crop and guessing one.
///
/// Nothing here is derived. The label and the kind are the app's own strings,
/// and a note is a child resource's own label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFacts {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// The labels of the child resources that export nothing.
    pub notes: Vec<String>,
}

/// Every resource the room's apps are publishing, whether or not it exports
/// anything.
///
/// Rebuilt on every call for the same reason [`tree`] is: a resource tree is a
/// function of the document, and a fact cached from an earlier one is a claim
/// about a page that has moved.
pub fn facts(held: &[(&'static AppContract, &AutoCommit)], view: &View) -> Vec<ResourceFacts> {
    fn walk(r: &Resource, out: &mut Vec<ResourceFacts>) {
        out.push(ResourceFacts {
            id: r.id.as_str().to_string(),
            kind: r.kind.clone(),
            label: r.label.clone(),
            notes: r
                .children
                .iter()
                .filter(|c| c.actions.is_empty())
                .map(|c| c.label.clone())
                .collect(),
        });
        for c in &r.children {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for (contract, doc) in held {
        for r in tree(contract, view, doc) {
            walk(&r, &mut out);
        }
    }
    out
}

/// What the room exports and what it says about itself, taken together.
///
/// The two halves of one survey. They are taken at the same moment against the
/// same documents on purpose: a caller that asked for the operations and then
/// asked separately for the facts would be reading two different rooms, and
/// the mismatch would show up as an operation described in terms of a resource
/// that had already changed.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    /// Everything the agent may perform. Denied is absent, as in [`offers`].
    pub offers: Vec<Offer>,
    /// Every resource the room's apps publish, gate or no gate.
    ///
    /// Not gated, and that is the rule the shell already states: showing a
    /// resource is non-destructive and needs no permission; acting on one does
    /// (`shell/src/shell/actions.rs`). A fact here can only be read alongside
    /// an operation that passed the gate, because the only way to ask for one
    /// is to name an address, and an address the caller may not perform is not
    /// in [`Self::offers`] to be named.
    pub facts: Vec<ResourceFacts>,
    /// The namespaces of the apps the room is actually holding.
    ///
    /// Read off the held replicas rather than inferred from what is on offer,
    /// because the two answers differ exactly where it matters. An app that is
    /// here but has nothing open exports no operations — the photo editor with
    /// no photograph in it publishes an empty tree — and it is *also* offered
    /// as openable, because the open is what gives it something. Guessing
    /// "open" from the offers would therefore report a running application as
    /// absent, which is the one thing a caller asking what is open must not be
    /// told wrongly.
    pub held: Vec<&'static str>,
}

impl Catalogue {
    /// The facts about the resource `address` is published by.
    pub fn about(&self, resource: &str) -> Option<&ResourceFacts> {
        self.facts.iter().find(|f| f.id == resource)
    }

    /// The same catalogue narrowed to one resource, keeping the operations
    /// that name no resource at all.
    ///
    /// What a request that arrived carrying its own subject is decided
    /// against: an address the message named is the resource the agent may act
    /// on, and offering the rest of the room would answer a question about one
    /// thing with an operation on another.
    ///
    /// The opens stay, and that is not an exception to the narrowing. An open
    /// names an *application*, performs nothing to anything, and is the only
    /// way an operation on the named resource can come to exist when the
    /// application that would export it is not running — drop it and "make
    /// this square", asked of a photograph in the file manager, can only come
    /// back as rename or delete.
    pub fn scoped_to(&self, resource: &str) -> Catalogue {
        Catalogue {
            held: self.held.clone(),
            offers: self
                .offers
                .iter()
                .filter(|o| o.address().resource == resource || opens(o.address()).is_some())
                .cloned()
                .collect(),
            facts: self
                .facts
                .iter()
                .filter(|f| f.id == resource)
                .cloned()
                .collect(),
        }
    }
}

/// Everything the room exports and everything it says, in one survey.
pub fn catalogue(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Catalogue {
    Catalogue {
        offers: offers(held, view, perms),
        facts: facts(held, view),
        held: held.iter().map(|(c, _)| c.namespace).collect(),
    }
}

/// Find the offer an id names, in a freshly taken survey.
pub fn find<'a>(offers: &'a [Offer], id: &str) -> Option<&'a Offer> {
    let address = decode_address(id)?;
    offers.iter().find(|o| *o.address() == address)
}

/// The app's own words for the operation an id names.
///
/// What a refusal or a confirmation says out loud. `None` when the survey no
/// longer carries it, and the caller then says something that names no
/// operation at all rather than falling back to the address — an identifier in
/// a sentence tells a reader nothing and makes a working agent look broken.
pub fn label_of(offers: &[Offer], id: &str) -> Option<String> {
    find(offers, id).map(|o| o.descriptor.action_label.clone())
}

/// A short, human list of what is on offer.
///
/// De-duplicated by label and capped: a folder of fifty files exports fifty
/// `Rename`s, and a refusal that recites all of them is a paragraph a person
/// skims and learns nothing from.
pub fn in_words(offers: &[Offer], most: usize) -> String {
    let mut names: Vec<&str> = Vec::new();
    for o in offers {
        let label = o.descriptor.action_label.as_str();
        if !names.iter().any(|n| n.eq_ignore_ascii_case(label)) {
            names.push(label);
        }
    }
    if names.is_empty() {
        return String::new();
    }
    let shown: Vec<String> = names.iter().take(most).map(|n| n.to_lowercase()).collect();
    let rest = names.len().saturating_sub(shown.len());
    let mut s = match shown.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, head)) => format!("{}, or {last}", head.join(", ")),
        None => String::new(),
    };
    if rest > 0 {
        s.push_str(&format!(", among {rest} other thing{}", if rest == 1 { "" } else { "s" }));
    }
    s
}

// ---------------------------------------------------------------------------
// The deterministic floor
// ---------------------------------------------------------------------------

/// Words that carry no intent and must not decide anything.
const NOISE: &[&str] = &[
    "the", "this", "that", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "please",
    "can", "you", "could", "would", "it", "its", "my", "our", "me", "us", "into", "from", "at",
    "by", "as", "is", "are", "be", "do", "does", "make", "made", "up", "out",
];

fn significant(word: &str) -> bool {
    word.chars().count() >= 3 && !NOISE.contains(&word)
}

/// The instruction's words, lowercased, with punctuation dropped and the noise
/// removed.
fn keywords(rest: &str) -> Vec<String> {
    rest.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| significant(w))
        .map(|w| w.to_string())
        .collect()
}

/// A literal the instruction quoted, if it quoted exactly one.
///
/// The only argument the floor will ever supply. An operation that collects
/// text needs words that are not in the offer, and with no model there is
/// nowhere honest to get them from except somewhere the instruction said them
/// outright. Anything less than a quoted literal would be the agent inventing
/// content and calling it the user's.
///
/// Straight and typographic double quotes only. An apostrophe is a letter in
/// English prose far more often than it is a quotation mark, and reading
/// "don't rename it" as a quoted `t rename it` is exactly the kind of
/// confident nonsense this vocabulary exists to avoid.
pub fn quoted_literal(rest: &str) -> Option<String> {
    let mut found: Vec<String> = Vec::new();

    // Straight quotes: splitting alternates outside/inside, so every odd
    // segment is quoted. An unterminated quote leaves a final odd segment,
    // which is why the count has to be even to have closed.
    let parts: Vec<&str> = rest.split('"').collect();
    if parts.len() % 2 == 1 {
        for inside in parts.iter().skip(1).step_by(2) {
            let lit = inside.trim();
            if !lit.is_empty() {
                found.push(lit.to_string());
            }
        }
    }

    // Typographic quotes, which open and close with different characters.
    let mut haystack = rest;
    while let Some(open) = haystack.find('\u{201c}') {
        let after = &haystack[open + '\u{201c}'.len_utf8()..];
        let Some(close) = after.find('\u{201d}') else {
            break;
        };
        let lit = after[..close].trim();
        if !lit.is_empty() {
            found.push(lit.to_string());
        }
        haystack = &after[close + '\u{201d}'.len_utf8()..];
    }

    match found.len() {
        1 => found.pop(),
        // Two literals is an instruction naming a thing *and* a value, which
        // this vocabulary cannot take apart. Refusing beats picking one.
        _ => None,
    }
}

/// How well an offer answers `words`: how many of them the app's own labels
/// carry. Zero means it does not answer at all.
fn score(offer: &Offer, words: &[String]) -> usize {
    let action = offer.descriptor.action_label.to_lowercase();
    let resource = offer.descriptor.resource_label.to_lowercase();
    let kind = offer.descriptor.resource_kind.to_lowercase();
    words
        .iter()
        .filter(|w| {
            // The verb has to land on the operation. Matching only the thing
            // being acted on says which file, never what to do to it.
            action.contains(w.as_str()) || resource.contains(w.as_str()) || kind.contains(w.as_str())
        })
        .count()
}

/// What the deterministic floor decided, when it could not decide.
///
/// Every string here is a **label** — the app's own words for an operation, the
/// ones it would put in front of a person. Never an address. What the agent
/// writes back goes into a document somebody is reading, and
/// `word-processor.document#insert-table` is an internal identifier: it belongs
/// in the log line next to the refusal, not in the sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatch {
    /// Nothing the room exports carries a word of the instruction.
    Nothing,
    /// Several operations answer it equally well, and choosing between them
    /// would be a guess. Carries their labels.
    Ambiguous(Vec<String>),
    /// The operation collects text and the instruction quoted none, so there is
    /// nothing to perform it with that the instruction actually said. Carries
    /// its label.
    NeedsText(String),
}

/// The floor: what the agent decides on its own when no model answered.
///
/// A keyword vocabulary cannot enumerate an open-ended action set, so this does
/// not try to. It matches the instruction's own words against the words the
/// apps put on their operations, and it insists on a clear winner: an
/// instruction that fits two operations equally is [`NoMatch::Ambiguous`], not
/// whichever came first. An operation that needs text gets the literal the
/// instruction quoted or nothing at all.
///
/// The refusals are the point. A run with no model reachable either does the
/// one thing the instruction plainly names, or says which of these three it
/// ran into — it never reports success for something it did not do, and never
/// substitutes the nearest available operation for the one that was asked for.
pub fn from_verbs<'a>(
    rest: &str,
    offers: &'a [Offer],
) -> Result<(&'a Offer, Option<String>), NoMatch> {
    let words = keywords(rest);
    if words.is_empty() || offers.is_empty() {
        return Err(NoMatch::Nothing);
    }
    let mut best: Vec<&Offer> = Vec::new();
    let mut best_score = 0usize;
    for offer in offers {
        let s = score(offer, &words);
        if s == 0 {
            continue;
        }
        if s > best_score {
            best_score = s;
            best = vec![offer];
        } else if s == best_score {
            best.push(offer);
        }
    }
    if best.is_empty() {
        return Err(NoMatch::Nothing);
    }
    if best.len() > 1 {
        return Err(NoMatch::Ambiguous(
            best.iter()
                .take(4)
                .map(|o| o.descriptor.action_label.clone())
                .collect(),
        ));
    }
    let chosen = best[0];
    // An operation that collects *content* collects a content hash, and there
    // is no content hash anywhere in a sentence — nobody writes one down and
    // inventing one would name bytes that do not exist. So it is not asked for
    // here. Either an earlier step produced content and the caller binds it
    // (see `plan::Step`), or the operation is refused at dispatch for having
    // nothing to place.
    if !chosen.wants_text() || chosen.wants_content() {
        return Ok((chosen, None));
    }
    match quoted_literal(rest) {
        Some(text) => Ok((chosen, Some(text))),
        None => Err(NoMatch::NeedsText(chosen.descriptor.action_label.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_actions::door::ActionAddress;

    #[test]
    fn an_address_with_a_path_in_it_round_trips() {
        let a = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
        assert_eq!(decode_address(&encode_address(&a)), Some(a));
    }

    /// A word processor holding a document with nothing in it is still a
    /// word processor with something to be done to it.
    ///
    /// This used to report an empty tree: no blocks meant no anchor, and no
    /// anchor meant the survey returned nothing at all — so a room containing
    /// an open, untouched document looked exactly like a room containing no
    /// word processor. Since applications start empty, that is the state a
    /// document is in precisely when somebody first wants to put something
    /// into it.
    #[test]
    fn a_document_with_no_blocks_is_still_something_to_act_on() {
        let mut doc = AutoCommit::new();
        qdoc::model::init(&mut doc, &automerge::ROOT);
        doc.commit();
        let c = qdoc::model::open(&doc, &automerge::ROOT).expect("the schema is there");
        assert!(
            qdoc::model::materialize(&doc, &c).blocks.is_empty(),
            "this test is about a document with nothing in it",
        );

        // No caret either: the agent has no screen, and a block id it made up
        // would be worse than none.
        let tree = word_processor_resources(&View::default(), &doc);
        assert!(
            !tree.is_empty(),
            "an empty document reported nothing that could be done to it",
        );
        let places = tree[0].actions.iter().any(|a| a.id == "place-image");
        assert!(
            places,
            "the document exports {:?}, none of which places a picture",
            tree[0].actions.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn the_app_is_read_off_the_resource_namespace() {
        assert_eq!(app_of("word-processor.document"), "word-processor");
        assert_eq!(app_of("files.entry:/var/workspace/plan.md"), "files");
    }

    #[test]
    fn one_quoted_literal_is_an_argument_and_two_are_not() {
        assert_eq!(
            quoted_literal(r#"rename it to "launch.md""#),
            Some("launch.md".into())
        );
        assert_eq!(quoted_literal(r#"rename "a.md" to "b.md""#), None);
        assert_eq!(quoted_literal("rename it to launch.md"), None);
        // Typographic quotes are what a word processor actually produces.
        assert_eq!(
            quoted_literal("rename it to \u{201c}launch.md\u{201d}"),
            Some("launch.md".into())
        );
        // An apostrophe is a letter, not a quotation mark.
        assert_eq!(quoted_literal("don't rename anything"), None);
        // An unterminated quote has quoted nothing.
        assert_eq!(quoted_literal(r#"rename it to "launch.md"#), None);
    }

    // -- opening an application ------------------------------------------

    use ui::permissions::{Actor, CapOverride, Scope, Subject};

    fn granting(caps: Vec<CapOverride>) -> PermissionContext {
        PermissionContext::from_capabilities(Actor::agent("Claude"), caps)
    }

    fn grant(scope: Scope) -> CapOverride {
        CapOverride::grant(Subject::agent("Claude"), scope, Verb::Write).with_id("t")
    }

    /// An open is asked about as the application, not as an operation inside
    /// it — which is what makes the three grants a room can write about it
    /// say three different things.
    ///
    /// Asserted through `permitted`, the same call the survey and the dispatch
    /// both make, against the capability model's own `decide`. A table written
    /// here would prove nothing about the model.
    #[test]
    fn the_grants_that_open_an_application_and_the_one_that_does_not() {
        let open = open_address("photos");
        let crop = ActionAddress::new("photos.image", "crop");

        assert_eq!(
            scope_for(&open).resource,
            ResourceRef::App("photos".to_string()),
            "opening is asked about as the application itself"
        );
        assert_eq!(
            scope_for(&crop).resource,
            ResourceRef::Action {
                app: "photos".to_string(),
                action: "crop".to_string(),
            },
            "and an operation is still asked about as an operation"
        );

        // Holding nothing opens nothing. An agent carries no role, so there
        // is no base authority underneath these.
        let none = granting(Vec::new());
        assert!(!permitted(&none, &open));

        // "May do one thing inside an application that is already running."
        // Emphatically not leave to start it: an agent handed a photograph to
        // crop must not be able to spawn a process with it.
        let one_action = granting(vec![grant(Scope::Action {
            app: "photos".to_string(),
            action: "crop".to_string(),
        })]);
        assert!(permitted(&one_action, &crop));
        assert!(
            !permitted(&one_action, &open),
            "leave to perform one operation in an app must not be leave to \
             start the app"
        );

        // "May open *this* application."
        let this_app = granting(vec![grant(Scope::App("photos".to_string()))]);
        assert!(permitted(&this_app, &open));
        assert!(
            !permitted(&this_app, &open_address("files")),
            "a grant naming one application must not open the catalogue"
        );

        // "May open applications."
        let any = granting(vec![grant(Scope::Workspace)]);
        assert!(permitted(&any, &open));
        assert!(permitted(&any, &open_address("files")));
    }

    /// The set of applications offered as openable is exactly the ones this
    /// room has nothing open in — so it empties itself as the room fills, and
    /// an app that is here *with something in it* is never offered as
    /// something to open.
    ///
    /// That second half is what stops a second window landing on top of the
    /// first: an agent that still offered to open what is already open would
    /// answer the next request about that app by opening a copy of it.
    ///
    /// "Here" is not the same as "has something to work on", and this used to
    /// treat them as one. The rule the code states is the one asserted below
    /// (see [`openable`]): an application present but exporting nothing is
    /// still openable, because the open is what gives it something, and an
    /// application publishing resources of its own is not. Asserted in both
    /// directions, because getting either wrong makes an app invisible — one
    /// way it can never be reached, the other way it is opened twice.
    #[test]
    fn only_the_apps_this_room_is_missing_are_offered_as_openable() {
        let perms = granting(vec![grant(Scope::Workspace)]);
        let view = View::default();

        let empty = openable(&[], &View::default(), &perms);
        let missing: Vec<&str> = empty.iter().map(|o| o.app).collect();
        assert_eq!(
            missing,
            CONTRACTS.iter().map(|c| c.namespace).collect::<Vec<_>>(),
            "an empty room is missing every app the agent speaks"
        );
        assert!(
            empty.iter().all(|o| !o.wants_text()),
            "opening an application takes no argument; there is nothing \
             anybody could say that would change which app it is"
        );

        // Now hold one of them. Any document the contract recognises will do —
        // what is being asserted is the bookkeeping, not the schema.
        let photos = CONTRACTS
            .iter()
            .find(|c| c.namespace == "photos")
            .expect("the photo editor's contract");
        let mut doc = automerge::AutoCommit::new();
        ::photos::doc::PhotosDoc::seed(&mut doc);
        doc.commit();
        assert!(
            (photos.recognises)(&doc),
            "the fixture has to be a document the contract claims, or this \
             test proves nothing"
        );
        let held: Vec<(&'static AppContract, &automerge::AutoCommit)> = vec![(photos, &doc)];

        // Here, and holding nothing. It exports no operations — there is no
        // photograph for a crop to be an operation *on* — so opening it onto
        // something is the only thing that can move any request forward, and
        // it stays offered.
        assert!(
            tree(photos, &view, &doc).is_empty(),
            "this half of the test is about an application with nothing open \
             in it; the fixture exports {:?}",
            tree(photos, &view, &doc).len()
        );
        let empty_app = openable(&held, &view, &perms);
        assert!(
            empty_app.iter().any(|o| o.app == "photos"),
            "an application that is here but has nothing open exports no \
             operations, so the open is the only way in and must stay on \
             offer: {:?}",
            empty_app.iter().map(Offer::id).collect::<Vec<_>>()
        );
        assert_eq!(
            empty_app.len(),
            empty.len(),
            "and nothing else may change: {:?}",
            empty_app.iter().map(Offer::id).collect::<Vec<_>>()
        );

        // Now give it a photograph. It publishes its own operations, so the
        // request has somewhere to go and offering to open it again would put
        // a second window over the first.
        let img = ::photos::model::OpenImage::from_path("/var/tmp/qos/harbour.png", 512, 320);
        ::photos::doc::PhotosDoc::set_image(&mut doc, Some(&img));
        doc.commit();
        let held: Vec<(&'static AppContract, &automerge::AutoCommit)> = vec![(photos, &doc)];
        assert!(
            !tree(photos, &view, &doc).is_empty(),
            "the fixture has to publish something, or this proves nothing"
        );

        let now = openable(&held, &view, &perms);
        assert!(
            !now.iter().any(|o| o.app == "photos"),
            "an app that is here *and* has something open must stop being \
             offered as something to open"
        );
        assert_eq!(
            now.len(),
            empty.len() - 1,
            "and nothing else may change: {:?}",
            now.iter().map(Offer::id).collect::<Vec<_>>()
        );

        // The full survey carries both halves: what the held app exports, and
        // what is still missing.
        let all = offers(&held, &view, &perms);
        assert!(
            all.iter().any(|o| opens(o.address()).is_some()),
            "the survey has to carry the openable apps, or nothing can choose \
             to open one"
        );
    }

    /// **Here is not the same as ready**, asked of one document at the two
    /// moments an open takes it through.
    ///
    /// Both questions were being answered with the first one. An open
    /// returned as soon as the application's state arrived, and the step
    /// after it asks what that application exports — which, for a photo
    /// editor that has started and has no photograph yet, is nothing. The
    /// same confusion sat on the other side of the same call: an application
    /// present and exporting nothing is *offered* as openable, and an open
    /// answering that offer with "already open" would open nothing and leave
    /// the next step exactly the empty room the offer existed to fill.
    #[test]
    fn an_application_that_is_here_and_empty_is_not_yet_surveyable() {
        let view = View::default();
        let photos = CONTRACTS
            .iter()
            .find(|c| c.namespace == "photos")
            .expect("the photo editor's contract");

        // The photo editor's document the instant it starts: its own seed,
        // and no photograph in it.
        let mut doc = automerge::AutoCommit::new();
        ::photos::doc::PhotosDoc::seed(&mut doc);
        doc.commit();
        assert!(
            (photos.recognises)(&doc),
            "this document is the photo editor's own, so an agent holding it \
             is holding the photo editor — which is all `holds` ever meant"
        );
        let held: Vec<(&'static AppContract, &automerge::AutoCommit)> = vec![(photos, &doc)];
        assert!(
            !surveyable(&held, &view, "photos"),
            "and it publishes nothing, so a step that followed an open here \
             would survey a room with no operation on a photograph in it"
        );

        // The photograph the open was made for, arriving.
        let img = ::photos::model::OpenImage::from_path("/var/tmp/qos/harbour.png", 512, 320);
        ::photos::doc::PhotosDoc::set_image(&mut doc, Some(&img));
        doc.commit();
        let held: Vec<(&'static AppContract, &automerge::AutoCommit)> = vec![(photos, &doc)];
        assert!(
            surveyable(&held, &view, "photos"),
            "now it publishes its own operations, which is what the open was \
             for"
        );
        let perms = granting(vec![grant(Scope::Workspace)]);
        assert!(
            offers(&held, &view, &perms)
                .iter()
                .any(|o| o.id().ends_with("#crop")),
            "and the crop is among them, or this proves nothing about the \
             step that wanted it: {:?}",
            offers(&held, &view, &perms)
                .iter()
                .map(Offer::id)
                .collect::<Vec<_>>()
        );
    }

    /// The picture a participant was asked to work on is one it can get the
    /// bytes of, whoever opened it.
    ///
    /// A participant with no window has no staging directory of its own: the
    /// path in the shared document belongs to whichever session opened the
    /// photograph, and this one is not it. Reading that path is the failure
    /// that ends the whole errand quietly — no bytes, no product, nothing to
    /// hand to the document the picture was going into, and every step still
    /// reporting that it applied. The content hash is the same word in every
    /// session, so it is what this resolves through.
    #[test]
    fn a_photograph_is_rendered_from_its_content_and_not_from_somebody_elses_path() {
        // The bytes, as the room holds them: in the content store, addressed
        // by their hash, exactly as they reach a participant that never
        // touched the file.
        let png = ::photos::pixels::encode_png(&::photos::shots_pattern(64, 32))
            .expect("the fixture encodes");
        let hash = qdoc::blobstore::put(&png);

        // The document, as this participant receives it: a path under another
        // session's runtime directory, which is not a path here.
        let theirs = format!("/run/user/1000/qlvx-another-session/qos-opened/{hash}.png");
        assert!(
            std::fs::read(&theirs).is_err(),
            "the stand-in for another session's path is readable here, so it \
             is not standing in for one"
        );
        let mut doc = automerge::AutoCommit::new();
        ::photos::doc::PhotosDoc::seed(&mut doc);
        ::photos::doc::PhotosDoc::set_image(
            &mut doc,
            Some(&::photos::model::OpenImage::from_path(&theirs, 64, 32).with_content(&hash)),
        );
        // And an edit in force, because the product is the picture *as asked
        // for* — a crop nobody can render is the same failure one step later.
        ::photos::doc::PhotosDoc::set_edits(
            &mut doc,
            &::photos::model::Edits::default().rotated_right(),
        );
        doc.commit();

        let product = photos_product(&View::default(), &doc)
            .expect("a participant holding the content can render what it was asked for");
        let Product::Content { hash: made } = product;
        assert_ne!(
            made, hash,
            "the product has to be the edited picture, not the original \
             handed straight back"
        );
        assert!(
            qdoc::blobstore::get(&made).is_some(),
            "the product names content this participant does not hold, so \
             the document it is placed into would reference a permanent hole"
        );
    }

    /// No app's own resource tree may publish an address that reads as an
    /// open, or opening would be indistinguishable from something an app
    /// exports and the capability question would be asked about the wrong
    /// thing.
    #[test]
    fn no_app_publishes_an_address_that_reads_as_an_open() {
        let view = View::default();
        for contract in CONTRACTS {
            let doc = automerge::AutoCommit::new();
            for descriptor in qos_actions::door::survey(&tree(contract, &view, &doc)) {
                assert!(
                    opens(&descriptor.address).is_none(),
                    "{} publishes {:?}, which reads as an open",
                    contract.namespace,
                    descriptor.address,
                );
            }
        }
    }
}
