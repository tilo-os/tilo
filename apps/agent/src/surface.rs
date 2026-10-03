//! What the room's apps currently have open, and what may be done to it.
//!
//! An app on the Tilo action model publishes a [`Resource`] tree and the
//! operations each resource exports (`docs/action-model-recipe.md`), and
//! [`tilo_actions::door`] turns a live tree into inert, stably-addressed
//! descriptors the agent can perform. The agent queries that tree each time,
//! so it receives the operations the app currently publishes.
//!
//! ## What the agent supplies, and what it must not
//!
//! A resource tree is a function of the document **and** of what is open:
//! the word processor's sections come from the document, but which block its
//! structural inserts anchor on is a caret. [`View`] is that half, and it is
//! the agent's own — deliberately not read off anybody's screen, and
//! carrying no coordinates.
//!
//! Everything else comes from the app: the resource ids, the action ids, the
//! labels a person would see, whether an operation takes text, and the
//! encoded action itself. The agent never mints any of it.
//!
//! ## Addresses
//!
//! `"<resource id>#<action id>"`, the same spelling the shell's own action
//! surface uses (`shell::shell::search::providers::app_resources`), so one
//! address names one operation wherever it is built. Never a screen
//! position: the same address keeps meaning the same operation across
//! layouts, themes and sessions.
//!
//! ## Authority
//!
//! A denied operation is **absent** from [`offers`], not marked unavailable
//! in it. The question asked is the shell's own gate: `Write` on
//! `(app, action)`, resolved against the agent's capability set and denied
//! unless something explicitly permits. It runs again at dispatch, because a
//! survey is a snapshot and acting on a stale one must not happen.

use std::fmt::Write as _;
use std::path::PathBuf;

use automerge::AutoCommit;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_actions::{ActionContract, ActionRegistry, PermissionRequirement, Resource};
use ui::permissions::{PermissionContext, PermissionScope, ResourceRef, Verb};

/// Separator between a resource id and an action id in an address.
///
/// `#` because a resource id may contain `.`, `:` and `/`, while an action id
/// is a plain identifier and neither half ever contains `#`. Splitting on the
/// last occurrence is unambiguous.
pub const ADDRESS_SEP: char = '#';

/// The single-string form of an address.
pub fn encode_address(address: &ActionAddress) -> String {
    format!("{}{}{}", address.resource, ADDRESS_SEP, address.action)
}

/// Recover an address from [`encode_address`]. `None` when the string is not
/// one — an address a model made up is refused rather than looked up
/// against a half-parsed id.
pub fn decode_address(id: &str) -> Option<ActionAddress> {
    let id = if let Some(rest) = id.strip_prefix("broker:") {
        rest.split_once(':')?.1
    } else {
        id
    };
    let (resource, action) = id.rsplit_once(ADDRESS_SEP)?;
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some(ActionAddress::new(resource, action))
}

/// The app a resource id belongs to: the segment before the first `.`.
///
/// Resource ids are app-namespaced by convention (`word-processor.document`,
/// `files.entry:/…`), which lets a capability question name the app being
/// acted on without a second table of who owns what.
pub fn app_of(resource_id: &str) -> &str {
    resource_id
        .split_once('.')
        .map_or(resource_id, |(head, _)| head)
}

/// The action id an application exports so that it can be opened.
pub const OPEN_ACTION: &str = "open";

/// The tail of the resource id an application has *as an application* rather
/// than as a document.
pub const APP_RESOURCE: &str = "app";

/// The stable kind tag on that resource, shared across every app.
pub const APPLICATION_KIND: &str = "applications.app";

/// The address that opens the app `namespace` names.
///
/// Inside the app's own namespace, so [`app_of`] answers it exactly as it
/// answers every other address.
pub fn open_address(namespace: &str) -> ActionAddress {
    ActionAddress::new(format!("{namespace}.{APP_RESOURCE}"), OPEN_ACTION)
}

/// The app `address` would open, or `None` when it opens nothing.
pub fn opens(address: &ActionAddress) -> Option<&str> {
    if address.action != OPEN_ACTION {
        return None;
    }
    if let Some(app_id) = address.resource.strip_prefix("app.") {
        return (!app_id.is_empty()).then_some(app_id);
    }
    let (namespace, tail) = address.resource.split_once('.')?;
    (tail == APP_RESOURCE).then_some(namespace)
}

/// Where the agent is looking.
///
/// Its own view state, and the only thing it contributes to a resource tree.
/// Nothing here is a screen position: a block id and a directory path are
/// addresses, and both keep meaning the same thing on a machine with no
/// display at all.
#[derive(Clone, Debug)]
pub struct View {
    /// The block the agent's caret is on, for an app whose subject is a
    /// document. Structural inserts anchor here.
    pub focus_block: Option<String>,
    /// Where in that block.
    pub focus_offset: usize,
    /// The blocks a block-level operation would target — the agent's
    /// selection, or just the focus block.
    pub target_blocks: Vec<String>,
    /// Exact character ranges selected in the document. Keeping these in the
    /// semantic view lets the agent discover the same inline-format actions
    /// as an on-screen selection instead of silently losing that surface.
    pub format_spans: Vec<word_processor::actions::Span>,
    /// The ordered semantic selection used by operations which replace text.
    /// Keeping it separate from formatting spans preserves a single stable
    /// resource address for a selection that crosses block boundaries.
    pub selection: Option<word_processor::actions::SelectionTarget>,
    /// Page region containing the caret/selection.
    pub region: word_processor::actions::RegionKind,
    /// The resource a request named, when it named one. An address, like
    /// everything else here: the gesture that produces it is somebody
    /// pointing at an object and saying something about *that*, so the app
    /// that owns the object minted the id.
    pub subject: Option<String>,
    /// The directory the agent is browsing, for an app whose subject is the
    /// filesystem.
    pub directory: Option<PathBuf>,
    /// The agent's content store: where content one operation produced (an
    /// image to place) is read from when the next one consumes it.
    pub content: tilo_content::ContentStore,
}

impl Default for View {
    fn default() -> Self {
        Self {
            focus_block: None,
            focus_offset: 0,
            target_blocks: Vec::new(),
            format_spans: Vec::new(),
            selection: None,
            region: word_processor::actions::RegionKind::Body,
            subject: None,
            directory: None,
            content: tilo_content::ContentStore::memory(),
        }
    }
}

/// One app whose contract the agent speaks.
///
/// Every field is the app's own code. Adding an app is one entry: what it
/// announces, how its documents are told apart from every other app's in the
/// room, the function that builds its tree, and the decoders it registers.
/// Nothing here describes *what* the app can do — that is the tree's job,
/// asked afresh each time.
pub struct AppContract {
    /// What a launched copy of this app announces to the broker.
    pub app_id: &'static str,
    /// What this app is called, in the words a sentence about it would use
    /// — "I'm not allowed to open the word processor here".
    pub title: &'static str,
    /// The namespace its resource ids start with, and the app name a
    /// capability question is phrased about.
    pub namespace: &'static str,
    /// Whether a document in the room is one this app authored.
    ///
    /// A room carries every app's state and none of it is labelled; a
    /// participant that announced no app id reaches every subscriber. So a
    /// held replica has to be attributable, and the app is the only thing
    /// that knows its own schema.
    pub recognises: fn(&AutoCommit) -> bool,
    /// The app's own resource tree for a view of a document.
    pub resources: fn(&View, &AutoCommit) -> Vec<Resource>,
    /// The app's own decoders, so the door can prove what it would run
    /// before running it.
    pub register: fn(&mut ActionRegistry),
    /// Whether [`Self::recognises`] is a claim on **any** document rather
    /// than on this app's own.
    ///
    /// Every app built on the toolkit's document schema will answer "yes,
    /// this parses" to that same test, so a contract using it has to be
    /// tried **last** — it wins a document only when nothing with a test of
    /// its own claims it first.
    pub fallback: bool,
    /// What this app's document currently *denotes*, as something another
    /// app's operation could be handed — or `None` for an app whose subject
    /// is not transferable.
    ///
    /// The app's own reading, for the same reason [`Self::resources`] is: an
    /// agent that worked this out for itself would be publishing its own
    /// idea of the result under the app's name.
    pub product: fn(&View, &AutoCommit) -> Option<Product>,
}

/// What one operation left behind for the next one.
///
/// Carrying a result forward means the second operation receives the first
/// one's outcome, or nothing at all, rather than re-deriving it by reading the
/// document again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Product {
    /// Bytes, in this machine's content store, addressed by their content hash.
    /// The hash is the whole of what travels: a document referencing it
    /// carries the descriptor and a peer fetches the bytes when it wants
    /// them, so a picture never becomes a change too large to cross the
    /// wire.
    Content { hash: String },
}

impl Product {
    /// The one-string form an operation that collects content is invoked
    /// with.
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

/// The picture in the open document, as content another application could
/// be handed.
///
/// A word processor's document denotes its prose, which is not
/// transferable — but a picture *in* it is exactly the kind of thing
/// another application could be asked to work on. The block already holds
/// the content hash (`InsertImage` writes `blob`), so nothing is derived
/// here and nothing is re-encoded.
///
/// The one the agent's view targets, and otherwise the document's only one.
/// Several with nothing said about which denotes **nothing**: picking the
/// first would answer a question nobody asked.
fn word_processor_product(view: &View, doc: &AutoCommit) -> Option<Product> {
    let c = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
    let snap = tilo_doc::model::materialize(doc, &c);
    let images: Vec<&tilo_doc::snapshot::BlockView> =
        snap.blocks.iter().filter(|b| b.kind == "image").collect();
    let blob_of = |b: &tilo_doc::snapshot::BlockView| match b.attrs.get("blob") {
        Some(tilo_doc::AttrVal::Str(h)) if !h.is_empty() => Some(h.clone()),
        _ => None,
    };
    if let Some(b) = images.iter().find(|b| view.target_blocks.contains(&b.id)) {
        return blob_of(b).map(|hash| Product::Content { hash });
    }
    match images.as_slice() {
        [only] => blob_of(only).map(|hash| Product::Content { hash }),
        _ => None,
    }
}

fn word_processor_resources(view: &View, doc: &AutoCommit) -> Vec<Resource> {
    // The anchor for a structural insert: the agent's caret when it has one,
    // else the document's first block. A document with nothing in it is
    // still a document, and reports its own empty anchor rather than
    // reading as though nothing were open.
    let anchor = view.focus_block.clone().or_else(|| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).ok()?;
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .first()
            .map(|b| b.id.clone())
    });
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
            format_spans: view.format_spans.clone(),
            selection: view.selection.clone(),
            region: view.region,
            // An agent's headless view does not own the Word Processor's
            // Find session. Search result resources are therefore absent
            // here; collaborative review resources are materialized from
            // the shared document below, independently of presentation
            // state.
            search_hits: Vec::new(),
            content: view.content.clone(),
        },
        doc,
    )
}

/// The room as chat publishes it, seen by a participant with no window open.
///
/// Every channel of the first workspace, and no active one: a live chat
/// client fills `active_channel` in from what is on screen, and the agent
/// has no screen. The message-level resources belong to whoever is actually
/// looking at them.
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

fn photos_resources(_view: &View, doc: &AutoCommit) -> Vec<Resource> {
    photos::actions::PhotosResources.publish_resources(doc)
}

/// Whether a document is the photo editor's — asked of its own schema.
/// `is_empty` is the app's own test for "never written to", so its negation
/// is "this app has written here".
fn photos_document(doc: &AutoCommit) -> bool {
    !photos::doc::PhotosDoc::is_empty(doc)
}

/// The word processor's contract, named on its own so a caller that needs
/// exactly this app — [`crate::apply::Editor::empty`], the replica used
/// where no peer snapshot exists to attribute — never has to index into
/// [`CONTRACTS`] to get it.
pub const WORD_PROCESSOR: AppContract = AppContract {
    app_id: "so.tilo.word-processor",
    title: "the word processor",
    namespace: "word-processor",
    recognises: |doc| tilo_doc::model::open(doc, &automerge::ROOT).is_ok(),
    resources: word_processor_resources,
    register: word_processor::actions::register,
    // "This parses as a rich-text document" is true of every document
    // built on the toolkit's schema, so this contract must be tried
    // last: it means "a document, and nobody else's".
    fallback: true,
    product: word_processor_product,
};

/// Every app the agent can speak to: a rich-text document, a conversation, a
/// photograph — deliberately different shapes, and none special-cased past
/// this point. Adding a fourth is one entry here.
///
/// The photo editor is what makes an instruction naming two apps worth
/// having: a photograph is edited here to be put somewhere else. Chat is
/// also the room people are already talking in, which is what makes
/// [`crate::listen`] possible.
pub static CONTRACTS: &[AppContract] = &[
    WORD_PROCESSOR,
    AppContract {
        app_id: "so.tilo.chat",
        title: "the conversation",
        namespace: "chat",
        recognises: chat::doc::ChatDoc::is_seeded,
        resources: chat_resources,
        register: chat::actions::register,
        fallback: false,
        product: nothing,
    },
    AppContract {
        app_id: "so.tilo.photos",
        title: "the photo editor",
        namespace: "photos",
        recognises: photos_document,
        resources: photos_resources,
        register: photos::actions::register,
        fallback: false,
        product: nothing,
    },
];

/// The contract for a document, or `None` when nothing in the room's apps
/// claims it.
pub fn recognise(doc: &AutoCommit) -> Option<&'static AppContract> {
    // Two passes: an app with a test of its own claims its document before
    // an app whose test claims anything shaped like a document at all. See
    // [`AppContract::fallback`].
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
    CONTRACTS.iter().find(|contract| contract.app_id == app_id)
}

/// Decoders for every app the agent speaks to, in one registry: the door
/// decodes an encoded action through it before dispatching, and a type no
/// app registered is a refusal rather than a best-effort apply.
pub fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    for c in CONTRACTS {
        (c.register)(&mut r);
    }
    r
}

/// One operation the room exports right now.
///
/// Inert: the address and the words the app puts on it, and nothing that
/// could perform it — there is no callback, token or handle in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The app that published it, by namespace.
    pub app: String,
    /// Exact live publisher selected by the broker catalogue. `None` only
    /// for the agent's local editorial/open helper offers.
    pub broker_peer_id: Option<u64>,
    pub descriptor: ActionDescriptor,
}

impl Offer {
    /// The address, in the one-string form an answer names it by.
    pub fn id(&self) -> String {
        let address = encode_address(&self.descriptor.address);
        self.broker_peer_id.map_or_else(
            || address.clone(),
            |peer| format!("broker:{peer}:{address}"),
        )
    }

    pub fn address(&self) -> &ActionAddress {
        &self.descriptor.address
    }

    /// Whether performing it requires a structured argument object.
    pub fn wants_arguments(&self) -> bool {
        !self.descriptor.params.is_empty()
    }

    /// Whether the text it wants is the address of content some other
    /// operation produced, rather than words the instruction can quote.
    ///
    /// Nobody writes a content hash into an instruction, and inventing one
    /// would name bytes that do not exist — so an operation that wants
    /// content can only be performed as a step after something that
    /// produced some.
    pub fn wants_content(&self) -> bool {
        self.descriptor.input.wants_content()
    }

    /// Deterministic floor for the common one-text-parameter action. Richer
    /// shapes are deliberately left to the structured planner; guessing how
    /// prose maps onto multiple or non-text fields would violate the schema.
    pub fn arguments_from_literal(
        &self,
        instruction: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let [param] = self.descriptor.params.as_slice() else {
            return None;
        };
        if !matches!(
            param.ty,
            tilo_actions::ParamType::Text | tilo_actions::ParamType::Secret
        ) {
            return None;
        }
        let literal = quoted_literal(instruction)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert(param.name.to_string(), serde_json::Value::String(literal));
        Some(arguments)
    }

    /// One line, in the app's own words: what this does and what it does it
    /// to.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} — on {} ({})",
            self.descriptor.action_label,
            self.descriptor.resource_label,
            self.descriptor.resource_kind
        );
        if !self.descriptor.params.is_empty() {
            let names = self
                .descriptor
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(s, "; arguments {{{names}}}");
        }
        if let Some(d) = &self.descriptor.description {
            let _ = write!(s, ". {d}");
        }
        s
    }
}

/// Convert the broker's permission-filtered, live catalogue into the same
/// inert offers the planner already consumes. No application registry or
/// document reconstruction is involved: arbitrary apps appear here solely
/// because they published a descriptor to the authenticated broker.
///
/// Accepts either the action host's fetched [`tilo_actions::remote::CataloguePage`]
/// or the broker's pushed [`ui::collaboration::BrokerActionSurface`] directory.
pub fn broker_offers<C: BrokerCatalogue + ?Sized>(catalogue: &C) -> Vec<Offer> {
    catalogue.broker_offers()
}

/// A live, permission-filtered action directory the broker supplies.
pub trait BrokerCatalogue {
    /// Every live action it names, as planner offers.
    fn broker_offers(&self) -> Vec<Offer>;
}

impl BrokerCatalogue for tilo_actions::remote::CataloguePage {
    fn broker_offers(&self) -> Vec<Offer> {
        catalogue_page_offers(self)
    }
}

impl BrokerCatalogue for [ui::collaboration::BrokerActionSurface] {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

impl BrokerCatalogue for Vec<ui::collaboration::BrokerActionSurface> {
    fn broker_offers(&self) -> Vec<Offer> {
        surface_offers(self)
    }
}

/// Offers from the broker's pushed surface directory: only live surfaces.
fn surface_offers(surfaces: &[ui::collaboration::BrokerActionSurface]) -> Vec<Offer> {
    surfaces
        .iter()
        .filter(|surface| surface.live)
        .flat_map(|surface| {
            surface.actions.iter().map(|action| {
                let input = if action.input.kind == "text" {
                    tilo_actions::InputSpec::Text {
                        prompt: action.input.prompt.clone(),
                        placeholder: action.input.placeholder.clone(),
                        collects: if action.input.wants_content {
                            tilo_actions::Collects::Content
                        } else {
                            tilo_actions::Collects::Words
                        },
                    }
                } else {
                    tilo_actions::InputSpec::None
                };
                Offer {
                    app: surface.app_id.clone(),
                    broker_peer_id: Some(surface.peer_id),
                    descriptor: ActionDescriptor {
                        address: ActionAddress::new(&action.resource, &action.action),
                        resource_kind: action.resource_kind.clone(),
                        resource_label: action.resource_label.clone(),
                        action_label: action.action_label.clone(),
                        description: action.description.clone(),
                        locus: tilo_actions::ExecutionLocus::ServerPreferred,
                        kind: action.operation_kind,
                        contract: action.contract.clone(),
                        params: action.params.clone(),
                        input,
                        view: None,
                    },
                }
            })
        })
        .collect()
}

fn catalogue_page_offers(catalogue: &tilo_actions::remote::CataloguePage) -> Vec<Offer> {
    catalogue
        .rows
        .iter()
        .filter_map(|entry| {
            // A live application's actions name its broker connection; the
            // workspace's own actions (governance, opening applications)
            // keep the reserved publisher zero.
            let peer = if let Ok(peer) = entry.provider.parse::<u64>() {
                peer
            } else {
                tilo_actions::remote::workspace_provider_room(&entry.provider)?;
                0
            };
            let mut descriptor = entry.action.clone()?;
            descriptor.locus = tilo_actions::ExecutionLocus::ServerPreferred;
            Some(Offer {
                app: entry.app_id.clone(),
                broker_peer_id: Some(peer),
                descriptor,
            })
        })
        .collect()
}

/// The permission question performing `address` has to pass.
///
/// Phrased about the app the resource id names, not about the agent's own
/// namespace: a room grants edit rights per app, so folding every app's
/// operations under one name would make a grant in one of them a grant in
/// all.
///
/// * `Scope::Action { app, action }` authorises one operation inside an app
///   that is *already running*, and confers no ability to start one.
/// * `Scope::App(app)` authorises this application: both its operations and
///   opening it.
/// * `Scope::Workspace` authorises any of them.
///
/// The verb stays `Write` throughout: a window appearing in the room is a
/// change to the room.
pub fn scope_for(address: &ActionAddress) -> PermissionScope {
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

/// Check the authority the operation itself declares. The address gate stays
/// the fallback for legacy descriptors; a read-only view must not silently be
/// upgraded to Write, and an administrative action must not be downgraded.
pub(crate) fn permitted_descriptor(
    perms: &PermissionContext,
    descriptor: &ActionDescriptor,
) -> bool {
    let mut scope = scope_for(&descriptor.address);
    scope.op = match descriptor.contract.permission {
        PermissionRequirement::None | PermissionRequirement::Read => Verb::Read,
        PermissionRequirement::Comment => Verb::Comment,
        PermissionRequirement::Write => Verb::Write,
        PermissionRequirement::Invite => Verb::Invite,
        PermissionRequirement::Admin => Verb::Admin,
    };
    perms.check(&scope)
}

/// The tree `contract` publishes for `doc` as it stands, seen from `view`.
///
/// Rebuilt on every call, never cached — an app's tree changes with every
/// edit, and the whole value of an address is that it names what is there
/// *now*.
pub fn tree(contract: &AppContract, view: &View, doc: &AutoCommit) -> Vec<Resource> {
    (contract.resources)(view, doc)
}

/// Everything the room exports that the agent may perform.
///
/// The union across every document it holds, in the order the apps are
/// declared and then depth-first within each.
pub fn offers(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Vec<Offer> {
    let mut out = Vec::new();
    for (contract, doc) in held {
        let resources = tree(contract, view, doc);
        for descriptor in tilo_actions::door::survey(&resources) {
            // Denied means absent, so nothing downstream can decide to
            // perform an operation it may not.
            if !permitted_descriptor(perms, &descriptor) {
                continue;
            }
            out.push(Offer {
                app: contract.namespace.to_string(),
                broker_peer_id: None,
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
/// authored has arrived and its contract claims it; it is **surveyable**
/// when the tree it publishes for that document is not empty. An app can be
/// here and export nothing — freshly opened, before it holds anything a
/// request could act on — and a step that follows an open, asking what the
/// open produced, needs the second answer while the first is what it was
/// told.
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
/// Bounded by the contracts the agent speaks — an app it cannot read
/// afterwards is one it could start and then have nothing to say about —
/// and by the apps that are **not already here with something in them**. A
/// held app with resources of its own needs no opening; offering it anyway
/// would invite a second window over the first.
///
/// # An open application holding nothing
///
/// "Already here" is not the same as "has something to work on". An app
/// present but exporting nothing is still openable: the open is what gives
/// it something, and the operations it then publishes answer the rest of
/// the request. An app with resources of its own stays excluded.
fn openable(
    held: &[(&'static AppContract, &AutoCommit)],
    view: &View,
    perms: &PermissionContext,
) -> Vec<Offer> {
    // Whether anything in this room denotes content an application could be
    // opened onto, asked through the same `product` an ordinary hand-off
    // between two steps goes through — so an open is offered content on
    // exactly the terms every other operation is.
    let onto = if held.iter().any(|(c, doc)| (c.product)(view, doc).is_some()) {
        tilo_actions::InputSpec::Text {
            prompt: "The content to open it onto".to_string(),
            placeholder: String::new(),
            collects: tilo_actions::Collects::Content,
        }
    } else {
        tilo_actions::InputSpec::None
    };
    CONTRACTS
        .iter()
        .filter(|c| !surveyable(held, view, c.namespace))
        .map(|c| Offer {
            app: c.namespace.to_string(),
            broker_peer_id: None,
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
                // Inert on this descriptor: `Session::perform` recognises
                // the address before the door sees it and asks the session
                // over the socket instead, since starting an application is
                // work only the session hosting the room can do.
                locus: tilo_actions::ExecutionLocus::ServerPreferred,
                kind: tilo_actions::OperationKind::Effect,
                contract: ActionContract::default(),
                params: Vec::new(),
                input: onto.clone(),
                view: None,
            },
        })
        .filter(|o: &Offer| permitted(perms, o.address()))
        .collect()
}

/// One resource as its app publishes it, with the operations taken out.
///
/// [`Offer`] carries an operation; a resource that exports **nothing**
/// never becomes an offer, so anything an app says by publishing one alone
/// — a note in a child resource's label, say a picture's pixel dimensions —
/// is invisible to a caller working from the survey alone. This is that
/// other half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFacts {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// The labels of the child resources that export nothing.
    pub notes: Vec<String>,
}

/// Every resource the room's apps are publishing, whether or not it exports
/// anything. Rebuilt on every call for the same reason [`tree`] is.
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

/// What the room exports and what it says about itself, taken together, at
/// the same moment against the same documents — asking separately would be
/// reading two different rooms.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    /// Everything the agent may perform. Denied is absent, as in [`offers`].
    pub offers: Vec<Offer>,
    /// Every resource the room's apps publish, gate or no gate: showing a
    /// resource is non-destructive and needs no permission, acting on one
    /// does — and the only way to ask for a fact is to name an address that
    /// already passed the gate to be in [`Self::offers`].
    pub facts: Vec<ResourceFacts>,
    /// The namespaces of the apps the room is actually holding, read off
    /// the held replicas rather than inferred from [`Self::offers`]: an app
    /// that is here but has nothing open exports no operations and is also
    /// offered as openable, so guessing "open" from the offers would report
    /// a running application as absent.
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
    /// against. The opens stay: dropping them would mean the application
    /// that would export an operation on the named resource, when it is not
    /// running, can never be started to answer the request.
    #[must_use]
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
    offers.iter().find(|offer| offer.id() == id)
}

/// The app's own words for the operation an id names.
///
/// `None` when the survey no longer carries it, so the caller says
/// something that names no operation at all rather than falling back to the
/// address — an identifier in a sentence tells a reader nothing.
pub fn label_of(offers: &[Offer], id: &str) -> Option<String> {
    find(offers, id).map(|o| o.descriptor.action_label.clone())
}

/// A short, human list of what is on offer.
///
/// De-duplicated by label and capped: a folder of fifty files exports fifty
/// `Rename`s, and a refusal reciting all of them is a paragraph a person
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
        let _ = write!(
            s,
            ", among {rest} other thing{}",
            if rest == 1 { "" } else { "s" }
        );
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

/// The instruction's words, lowercased, with punctuation dropped and the
/// noise removed.
fn keywords(rest: &str) -> Vec<String> {
    rest.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| significant(w))
        .map(std::string::ToString::to_string)
        .collect()
}

/// A literal the instruction quoted, if it quoted exactly one.
///
/// The only argument the floor will ever supply: an operation that collects
/// text needs words that are not in the offer, and with no model the only
/// honest source is somewhere the instruction said them outright.
///
/// Straight and typographic double quotes only. An apostrophe is a letter in
/// English prose far more often than a quotation mark, and reading "don't
/// rename it" as a quoted `t rename it` is exactly the confident nonsense
/// this vocabulary exists to avoid.
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
        let Some(after) = haystack.get(open + '\u{201c}'.len_utf8()..) else {
            break;
        };
        let Some(close) = after.find('\u{201d}') else {
            break;
        };
        let Some(lit) = after.get(..close) else {
            break;
        };
        let lit = lit.trim();
        if !lit.is_empty() {
            found.push(lit.to_string());
        }
        let Some(rest_after) = after.get(close + '\u{201d}'.len_utf8()..) else {
            break;
        };
        haystack = rest_after;
    }

    match found.len() {
        1 => found.pop(),
        // Two literals names a thing *and* a value, which this vocabulary
        // cannot take apart. Refusing beats picking one.
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
            // The verb has to land on the operation. Matching only the
            // thing being acted on says which file, never what to do to it.
            action.contains(w.as_str())
                || resource.contains(w.as_str())
                || kind.contains(w.as_str())
        })
        .count()
}

/// What the deterministic floor decided, when it could not decide.
///
/// Every string here is a **label**, never an address: what the agent
/// writes back goes into a document somebody is reading, and an internal
/// identifier belongs in the log line next to the refusal, not in the
/// sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatch {
    /// Nothing the room exports carries a word of the instruction.
    Nothing,
    /// Several operations answer it equally well, and choosing between them
    /// would be a guess. Carries their labels.
    Ambiguous(Vec<String>),
    /// The operation collects text and the instruction quoted none, so
    /// there is nothing to perform it with that the instruction actually
    /// said. Carries its label.
    NeedsText(String),
}

/// The floor: what the agent decides on its own when no model answered.
///
/// A keyword vocabulary cannot enumerate an open-ended action set, so this
/// does not try to. It matches the instruction's own words against the
/// words the apps put on their operations, and insists on a clear winner:
/// an instruction that fits two operations equally is [`NoMatch::Ambiguous`],
/// not whichever came first — unless what the instruction *supplied* breaks
/// the tie itself (see `collects_a_value_said`). A run with no model
/// reachable either does the one thing plainly named, or says which of
/// these three it ran into — it never reports success for something it did
/// not do, and never substitutes the nearest available operation for the
/// one asked for.
///
/// # Errors
///
/// Returns [`NoMatch::Nothing`] when no offer's words match the
/// instruction, [`NoMatch::Ambiguous`] when more than one offer answers
/// equally well and nothing the instruction supplied breaks the tie, and
/// [`NoMatch::NeedsText`] when the one offer left collects a value but the
/// instruction quoted none.
pub fn from_verbs<'a>(
    rest: &str,
    offers: &'a [Offer],
) -> Result<(&'a Offer, serde_json::Map<String, serde_json::Value>), NoMatch> {
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
    let said = quoted_literal(rest);
    if best.len() > 1 {
        // Two operations answering to one verb need not be two meanings.
        // Whether the instruction supplied a value is a fact about which of
        // them it named — the same fact dispatch reads a moment later, off
        // [`Offer::wants_text`] — so a tie between an operation that takes
        // one and an operation that does not is not a guess.
        //
        // Only ever narrowing, and never to nothing: if every tied
        // operation refuses what the instruction supplied, the tie stands
        // and is refused, which is what keeps a genuine ambiguity (two
        // operations that both take nothing) a question rather than a coin
        // toss.
        let fits: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|o| collects_a_value_said(o) == said.is_some())
            .collect();
        if !fits.is_empty() {
            best = fits;
        }
    }
    if best.len() > 1 {
        // Equal keyword hits still leave a shorter label closer to what
        // was said: "insert table" beats "generate table of contents" for
        // an instruction that only said "table". Align left / Align centre
        // stay tied (same length) and remain Ambiguous.
        let label_words = |offer: &Offer| keywords(&offer.descriptor.action_label).len().max(1);
        let fewest = best
            .iter()
            .map(|offer| label_words(offer))
            .min()
            .unwrap_or(1);
        let denser: Vec<&Offer> = best
            .iter()
            .copied()
            .filter(|offer| label_words(offer) == fewest)
            .collect();
        if !denser.is_empty() {
            best = denser;
        }
    }
    if best.len() > 1 {
        return Err(NoMatch::Ambiguous(
            best.iter()
                .take(4)
                .map(|o| o.descriptor.action_label.clone())
                .collect(),
        ));
    }
    // Unreachable given the checks above — `best` is non-empty and, past
    // the `best.len() > 1` refusal, has exactly one entry — but handled like
    // any other malformed state: refuse rather than panic.
    let Some(&chosen) = best.first() else {
        return Err(NoMatch::Nothing);
    };
    if !collects_a_value_said(chosen) {
        return Ok((chosen, serde_json::Map::new()));
    }
    match chosen.arguments_from_literal(rest) {
        Some(arguments) => Ok((chosen, arguments)),
        None => Err(NoMatch::NeedsText(chosen.descriptor.action_label.clone())),
    }
}

/// Whether performing `offer` needs a value the instruction could have said.
///
/// Narrower than [`Offer::wants_text`]. An operation that collects
/// *content* wants text too, but the text it wants is a content hash —
/// there is none anywhere in a sentence, nobody writes one down, and
/// inventing one would name bytes that do not exist. So it is never asked
/// of the words: either an earlier step produced content and the caller
/// binds it, or the operation is refused at dispatch for having nothing to
/// place. This is the question to ask of an instruction and an operation
/// together — whether what the one said is what the other takes.
fn collects_a_value_said(offer: &Offer) -> bool {
    offer.wants_arguments() && !offer.wants_content()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::door::ActionAddress;

    #[test]
    fn offer_addresses_round_trip_name_their_app_and_stay_distinct_per_publisher() {
        // Case `an_address_with_a_path_in_it_round_trips`.
        {
            let a = ActionAddress::new("files.entry:/var/workspace/a.b/plan.md", "rename");
            assert_eq!(decode_address(&encode_address(&a)), Some(a));
        }

        // Case `the_app_is_read_off_the_resource_namespace`.
        {
            assert_eq!(app_of("word-processor.document"), "word-processor");
            assert_eq!(app_of("files.entry:/var/workspace/plan.md"), "files");
        }

        // Case `duplicate_addresses_from_two_live_publishers_remain_distinct`.
        {
            let entry = |peer_id: u64| tilo_actions::remote::CatalogueRow {
                provider: peer_id.to_string(),
                app_id: "so.tilo.calculator".into(),
                parent_resource: String::new(),
                resource: tilo_actions::remote::CatalogueResource {
                    id: "calculator.document".into(),
                    kind: "calculator.document".into(),
                    label: "Calculator".into(),
                    presentation: None,
                },
                has_children: false,
                visibility: None,
                action: Some(ActionDescriptor {
                    address: ActionAddress::new("calculator.document", "clear"),
                    resource_kind: "calculator.document".into(),
                    resource_label: "Calculator".into(),
                    action_label: "Clear".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: tilo_actions::ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                }),
            };
            let catalogue = tilo_actions::remote::CataloguePage {
                revision: "1".into(),
                rows: vec![entry(41), entry(42)],
                ..tilo_actions::remote::CataloguePage::default()
            };
            let offers = broker_offers(&catalogue);
            assert_eq!(offers.len(), 2);
            assert_ne!(offers[0].id(), offers[1].id());
            assert_eq!(
                find(&offers, &offers[0].id()).unwrap().broker_peer_id,
                Some(41)
            );
            assert_eq!(
                find(&offers, &offers[1].id()).unwrap().broker_peer_id,
                Some(42)
            );
        }

        // Case `broker_prefixed_shell_launch_is_still_recognised_as_an_open`.
        {
            let address = ActionAddress::new("app.so.tilo.diffmerge", OPEN_ACTION);
            let offer = Offer {
                app: "so.tilo.diffmerge".into(),
                broker_peer_id: Some(0),
                descriptor: ActionDescriptor {
                    address,
                    resource_kind: APPLICATION_KIND.into(),
                    resource_label: "Diffmerge".into(),
                    action_label: "Open Diffmerge".into(),
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ServerPreferred,
                    kind: tilo_actions::OperationKind::Effect,
                    contract: ActionContract::default(),
                    params: Vec::new(),
                    input: tilo_actions::InputSpec::None,
                    view: None,
                },
            };
            let decoded = decode_address(&offer.id()).expect("broker offer id remains decodable");
            assert_eq!(opens(&decoded), Some("so.tilo.diffmerge"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn a_room_offers_what_held_documents_publish_and_opens_only_missing_granted_apps() {
        // Case `a_document_with_no_blocks_is_still_something_to_act_on`.
        {
            let mut doc = AutoCommit::new();
            tilo_doc::model::init(&mut doc, &automerge::ROOT);
            doc.commit();
            let c = tilo_doc::model::open(&doc, &automerge::ROOT).expect("the schema is there");
            assert!(
                tilo_doc::model::materialize(&doc, &c).blocks.is_empty(),
                "this test is about a document with nothing in it",
            );

            let tree = word_processor_resources(&View::default(), &doc);
            assert!(
                !tree.is_empty(),
                "an empty document reported nothing that could be done to it",
            );
            let inserts = tree[0].actions.iter().any(|a| a.id == "insert-table");
            assert!(
                inserts,
                "the document exports {:?}, none of which inserts into it",
                tree[0]
                    .actions
                    .iter()
                    .map(|a| a.id.clone())
                    .collect::<Vec<_>>(),
            );
        }

        // Case `photos_requires_an_explicit_export_result_for_cross_app_content`.
        {
            let png = photos::pixels::encode_png(&photos::shots_pattern(64, 32)).expect("fixture");
            let hash = tilo_content::hash_bytes(&png);
            let mut doc = automerge::AutoCommit::new();
            photos::doc::PhotosDoc::seed(&mut doc);
            photos::doc::PhotosDoc::set_image(
                &mut doc,
                Some(&photos::OpenImage::from_path("/tmp/source.png", 64, 32).with_content(&hash)),
            );
            photos::doc::PhotosDoc::set_edits(&mut doc, &photos::Edits::default().rotated_right());
            doc.commit();

            assert!(
                (for_app_id("so.tilo.photos")
                    .expect("Photos contract")
                    .product)(&View::default(), &doc)
                .is_none(),
                "holding a Photos replica must not silently rerender content; only the explicit export action may produce tilo.content"
            );
        }

        // Case `the_grants_that_open_an_application_and_the_one_that_does_not`.
        {
            let open = open_address("chat");
            let post = ActionAddress::new("chat.channel:c1", "post");

            assert_eq!(
                scope_for(&open).resource,
                ResourceRef::App("chat".to_string()),
                "opening is asked about as the application itself"
            );
            assert_eq!(
                scope_for(&post).resource,
                ResourceRef::Action {
                    app: "chat".to_string(),
                    action: "post".to_string(),
                },
                "and an operation is still asked about as an operation"
            );

            let none = granting(Vec::new());
            assert!(!permitted(&none, &open));

            // "May do one thing inside an application that is already
            // running." Not leave to start it.
            let one_action = granting(vec![grant(Scope::Action {
                app: "chat".to_string(),
                action: "post".to_string(),
            })]);
            assert!(permitted(&one_action, &post));
            assert!(
                !permitted(&one_action, &open),
                "leave to perform one operation in an app must not be leave to \
                 start the app"
            );

            // "May open *this* application."
            let this_app = granting(vec![grant(Scope::App("chat".to_string()))]);
            assert!(permitted(&this_app, &open));
            assert!(
                !permitted(&this_app, &open_address("word-processor")),
                "a grant naming one application must not open the catalogue"
            );

            // "May open applications."
            let any = granting(vec![grant(Scope::Workspace)]);
            assert!(permitted(&any, &open));
            assert!(permitted(&any, &open_address("word-processor")));
        }

        // Case `only_the_apps_this_room_is_missing_are_offered_as_openable`.
        {
            let perms = granting(vec![grant(Scope::Workspace)]);
            let view = View::default();

            let empty = openable(&[], &View::default(), &perms);
            let missing: Vec<&str> = empty.iter().map(|o| o.app.as_str()).collect();
            assert_eq!(
                missing,
                CONTRACTS.iter().map(|c| c.namespace).collect::<Vec<_>>(),
                "an empty room is missing every app the agent speaks"
            );
            assert!(
                empty.iter().all(|o| !o.wants_arguments()),
                "opening an application takes no argument"
            );

            let chat = chat_contract();
            let doc = empty_chat();
            assert!(
                (chat.recognises)(&doc),
                "the fixture has to be a document the contract claims"
            );
            let held: Vec<(&'static AppContract, &AutoCommit)> = vec![(chat, &doc)];
            assert!(
                tree(chat, &view, &doc).is_empty(),
                "this half is about an application with nothing open in it"
            );
            let empty_app = openable(&held, &view, &perms);
            assert!(
                empty_app.iter().any(|o| o.app == "chat"),
                "an application here but with nothing open exports no \
                 operations, so the open must stay on offer"
            );
            assert_eq!(empty_app.len(), empty.len(), "and nothing else may change");

            let mut doc = doc;
            let _ = chat::doc::ChatDoc::create_workspace(&mut doc, "Test");
            doc.commit();
            let held: Vec<(&'static AppContract, &AutoCommit)> = vec![(chat, &doc)];
            assert!(
                !tree(chat, &view, &doc).is_empty(),
                "the fixture has to publish something, or this proves nothing"
            );
            let now = openable(&held, &view, &perms);
            assert!(
                !now.iter().any(|o| o.app == "chat"),
                "an app that is here *and* has something open must stop being \
                 offered as something to open"
            );
            assert_eq!(now.len(), empty.len() - 1, "and nothing else may change");

            let all = offers(&held, &view, &perms);
            assert!(
                all.iter().any(|o| opens(o.address()).is_some()),
                "the survey has to carry the openable apps"
            );
        }

        // Case `an_application_that_is_here_and_empty_is_not_yet_surveyable`.
        {
            let view = View::default();
            let chat = chat_contract();

            let doc = empty_chat();
            assert!(
                (chat.recognises)(&doc),
                "this document is the app's own, so an agent holding it is \
                 holding the app — which is all `holds` ever meant"
            );
            let held: Vec<(&'static AppContract, &AutoCommit)> = vec![(chat, &doc)];
            assert!(
                !surveyable(&held, &view, "chat"),
                "and it publishes nothing, so a step that followed an open here \
                 would survey a room with no operation in it"
            );

            let mut doc = doc;
            let _ = chat::doc::ChatDoc::create_workspace(&mut doc, "Test");
            doc.commit();
            let held: Vec<(&'static AppContract, &AutoCommit)> = vec![(chat, &doc)];
            assert!(
                surveyable(&held, &view, "chat"),
                "now it publishes its own operations, which is what the open \
                 was for"
            );
            let perms = granting(vec![grant(Scope::Workspace)]);
            assert!(
                offers(&held, &view, &perms)
                    .iter()
                    .any(|o| o.id().ends_with("#new-channel")),
                "and creating a channel is among them, or this proves nothing \
                 about the step that wanted it"
            );
        }

        // Case `no_app_publishes_an_address_that_reads_as_an_open`.
        {
            let view = View::default();
            for contract in CONTRACTS {
                let doc = automerge::AutoCommit::new();
                for descriptor in tilo_actions::door::survey(&tree(contract, &view, &doc)) {
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn a_quoted_literal_is_the_value_that_tells_same_verb_operations_apart() {
        // Case `one_quoted_literal_is_an_argument_and_two_are_not`.
        {
            assert_eq!(
                quoted_literal(r#"rename it to "launch.md""#),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal(r#"rename "a.md" to "b.md""#), None);
            assert_eq!(quoted_literal("rename it to launch.md"), None);
            assert_eq!(
                quoted_literal("rename it to \u{201c}launch.md\u{201d}"),
                Some("launch.md".into())
            );
            assert_eq!(quoted_literal("don't rename anything"), None);
            assert_eq!(quoted_literal(r#"rename it to "launch.md"#), None);
        }

        // Case `a_supplied_value_tells_two_same_verb_operations_apart`.
        {
            let offers = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop",
                    "Crop to a region",
                    tilo_actions::InputSpec::Text {
                        prompt: "Region to keep".to_string(),
                        placeholder: "0.25,0.25,0.5,0.5".to_string(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-square",
                    "Crop to a square",
                    tilo_actions::InputSpec::None,
                ),
            ];

            let (chosen, argument) =
                from_verbs(r#"crop the photograph to "0.25,0.25,0.5,0.5""#, &offers)
                    .expect("a quoted region tells the two crops apart");
            assert_eq!(
                chosen.address().action,
                "crop",
                "a quoted region must reach the crop that collects one, not {}",
                chosen.descriptor.action_label,
            );
            assert_eq!(
                argument.get("value"),
                Some(&serde_json::Value::String("0.25,0.25,0.5,0.5".into()))
            );

            let (chosen, argument) = from_verbs("crop the photograph", &offers)
                .expect("with no region said, the crop that needs none is the one that can act");
            assert_eq!(chosen.address().action, "crop-square");
            assert!(argument.is_empty());

            // What is gone is a false ambiguity, not the mechanism: two
            // operations that both take nothing are still not told apart by an
            // instruction that supplies nothing, and that is still a genuine
            // question rather than a guess.
            let both_bare = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-left",
                    "Rotate left",
                    tilo_actions::InputSpec::None,
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "rotate-right",
                    "Rotate right",
                    tilo_actions::InputSpec::None,
                ),
            ];
            match from_verbs("rotate the photograph", &both_bare) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!("a genuine ambiguity must still be refused: {other:?}"),
            }

            // Nor is it dropped when the split would leave nothing: two tied
            // operations that both *want* a value are still ambiguous when the
            // instruction supplied none — narrowing must give up and leave the
            // original tie standing rather than choose from an empty set.
            let both_prompted = vec![
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-left-half",
                    "Crop the left half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
                offer(
                    "photos",
                    "photos.image:1",
                    "crop-right-half",
                    "Crop the right half",
                    tilo_actions::InputSpec::Text {
                        prompt: "Which half".to_string(),
                        placeholder: String::new(),
                        collects: tilo_actions::Collects::Words,
                    },
                ),
            ];
            match from_verbs("crop the photograph", &both_prompted) {
                Err(NoMatch::Ambiguous(labels)) => assert_eq!(labels.len(), 2, "{labels:?}"),
                other => panic!(
                    "with no value supplied and both operations wanting one, the \
                     tie must stand rather than pick from nothing: {other:?}"
                ),
            }
        }
    }

    // -- telling two operations of one verb apart -------------------------

    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        input: tilo_actions::InputSpec,
    ) -> Offer {
        let params = matches!(&input, tilo_actions::InputSpec::Text { .. })
            .then(|| {
                vec![tilo_actions::ActionParam::required(
                    "value",
                    tilo_actions::ParamType::Text,
                    "value",
                )]
            })
            .unwrap_or_default();
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            descriptor: ActionDescriptor {
                address: ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: "a thing".to_string(),
                action_label: label.to_string(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: ActionContract::default(),
                params,
                input,
                view: None,
            },
        }
    }

    #[test]
    fn contract_lookup_accepts_only_the_exact_app_id() {
        assert_eq!(
            for_app_id("so.tilo.photos")
                .expect("Photos contract")
                .namespace,
            "photos"
        );
        assert!(for_app_id("dev.tilo.photos").is_none());
        assert!(for_app_id("so.tilo.unknown").is_none());
    }

    // -- opening an application ------------------------------------------

    use ui::permissions::{Actor, CapOverride, Scope, Subject};

    fn granting(caps: Vec<CapOverride>) -> PermissionContext {
        PermissionContext::from_capabilities(Actor::agent("Claude"), caps)
    }

    fn grant(scope: Scope) -> CapOverride {
        CapOverride::grant(Subject::agent("Claude"), scope, Verb::Write).with_id("t")
    }

    /// A seeded chat document with no workspace in it — the state the
    /// document is in the instant it exists and nothing more.
    fn empty_chat() -> AutoCommit {
        use automerge::transaction::Transactable;
        let mut doc = AutoCommit::new();
        let _ = doc.put(&automerge::ROOT, "seeded", true);
        doc.commit();
        doc
    }

    fn chat_contract() -> &'static AppContract {
        CONTRACTS
            .iter()
            .find(|c| c.namespace == "chat")
            .expect("the chat contract")
    }

    /// The agent publishes nothing of its own: what it can do is the tree each
    /// contract builds from its headless view of a held document. Every
    /// immediate action in that tree is one the agent may perform, so each
    /// must honour the action contract through the agent's registry.
    mod chat_contract {
        use super::*;

        fn seeded() -> AutoCommit {
            let mut doc = AutoCommit::new();
            chat::doc::ChatDoc::seed_if_empty(&mut doc);
            doc
        }

        fn contract() -> &'static AppContract {
            for_app_id("so.tilo.chat").expect("chat is a contract")
        }

        ui::action_contract_tests! {
            seed: seeded(),
            registry: registry(),
            resources: |doc| tree(contract(), &View::default(), doc),
            palette: ["New channel", "Rename channel"],
        }
    }

    mod word_processor_contract {
        use super::*;

        /// A short document built through qdoc's own editing path.
        fn seeded() -> AutoCommit {
            let mut document = tilo_doc::Document::new();
            let heading = document.insert_block(None, "heading");
            document.splice_text(&heading, 0, 0, "Launch plan");
            let body = document.insert_block(Some(&heading), "paragraph");
            document.splice_text(&body, 0, 0, "Ship the beta on Friday.");
            AutoCommit::load(&document.save()).expect("a saved document loads")
        }

        ui::action_contract_tests! {
            seed: seeded(),
            registry: registry(),
            resources: |doc| tree(&WORD_PROCESSOR, &View::default(), doc),
            palette: ["Insert table"],
        }
    }
}
