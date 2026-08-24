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
    let bytes = std::fs::read(&image.path)
        .map_err(|e| tracing::warn!(path = %image.path, "cannot read the photograph: {e}"))
        .ok()?;
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
        product: nothing,
    },
    AppContract {
        app_id: "dev.qos.file-explorer",
        title: "the file manager",
        namespace: "files",
        recognises: file_explorer::actions::is_explorer_document,
        resources: file_explorer_resources,
        register: file_explorer::actions::register,
        product: nothing,
    },
    AppContract {
        app_id: "dev.qos.chat",
        title: "the conversation",
        namespace: "chat",
        recognises: chat::actions::is_chat_document,
        resources: chat_resources,
        register: chat::actions::register,
        product: nothing,
    },
    AppContract {
        app_id: "dev.qos.photos",
        title: "the photo editor",
        namespace: "photos",
        recognises: photos_document,
        resources: photos_resources,
        register: photos::actions::register,
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
    CONTRACTS.iter().find(|c| (c.recognises)(doc))
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
    out.extend(openable(held, perms));
    out
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
/// And it is the apps that are **not already here**. A held app needs no
/// opening; offering it anyway invites a second window over the first, and
/// invites a request to be answered by opening the thing it is about. So the
/// set empties itself as the room fills, and the moment an app arrives its own
/// operations replace the one that asked for it.
fn openable(
    held: &[(&'static AppContract, &AutoCommit)],
    perms: &PermissionContext,
) -> Vec<Offer> {
    CONTRACTS
        .iter()
        .filter(|c| !held.iter().any(|(h, _)| h.namespace == c.namespace))
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
                input: qos_actions::InputSpec::None,
            },
        })
        // The same gate every other operation passes, asked in the same place.
        // Denied means absent here too: an agent that may not open the photo
        // editor is never shown opening it as something it could choose.
        .filter(|o: &Offer| permitted(perms, o.address()))
        .collect()
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
    /// room does not have — so it empties itself as the room fills, and an app
    /// that is here is never offered as something to open.
    ///
    /// The second half is what stops a second window landing on top of the
    /// first: an agent that still offered to open what is already open would
    /// answer the next request about that app by opening a copy of it.
    #[test]
    fn only_the_apps_this_room_is_missing_are_offered_as_openable() {
        let perms = granting(vec![grant(Scope::Workspace)]);
        let view = View::default();

        let empty = openable(&[], &perms);
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

        let now = openable(&held, &perms);
        assert!(
            !now.iter().any(|o| o.app == "photos"),
            "an app whose state is in the room must stop being offered as \
             something to open"
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
