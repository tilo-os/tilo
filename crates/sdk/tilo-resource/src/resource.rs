//! The resource contract: what any resource type provides so that it gets
//! history, undo, collaboration, permissions, palette/agent/script exposure,
//! search and replay without writing any of them.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_actions::{
    ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, PermissionRequirement,
};

use crate::delta::Delta;
use crate::model::{Doc, ModelError};
use crate::reader::Demand;

/// A resource type. Implement it by hand for a type with its own storage
/// layout and merge semantics (a timeline, a rich-text document, a photo edit
/// stack); `#[resource]` implements it for a `#[derive(Model)]` record.
///
/// The contract is deliberately small:
///
/// * [`read`](Self::read) — materialise the state views render, from the live
///   document *or* from any historical version (time travel is just `read` on
///   an older document);
/// * [`refresh`](Self::refresh) — optionally, bring a state up to date after a
///   change by re-reading only what the change touched, so an edit costs as
///   much as the edit rather than as much as the resource;
/// * [`load`](Self::load) — optionally, read on demand the parts of a large
///   resource that views are looking at (the pages of a long document on
///   screen), so opening or refreshing it never reads all of it;
/// * [`Action`](Self::Action) — the replayable mutation vocabulary: a
///   serializable value whose `apply` is a pure function of the document;
/// * [`declarations`](Self::declarations) — which mutations are offered by
///   name to the palette, agents and scripts, with typed parameters.
///
/// Everything else has a default.
pub trait Resource: Sized + 'static {
    /// Stable kind of record this resource lives in, e.g. `"notes.notebook"`.
    const KIND: &'static str;
    /// Human name of one resource of this kind, e.g. `"Notebook"`.
    const LABEL: &'static str;
    /// The registry type id every action of this resource is replayed under.
    const ACTION_TYPE: &'static str = Self::KIND;

    /// What views render. For a record resource this is the record itself.
    type State: 'static;
    /// The replayable mutation. Must be deterministic: no clock, no RNG, no
    /// local state — use [`ActionCx`](crate::ActionCx) for ids and time.
    type Action: tilo_actions::Action + Serialize + DeserializeOwned + Clone + 'static;

    /// Read the state from a document.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn read(doc: &Doc) -> Result<Self::State, ModelError>;

    /// Bring `state`, read earlier from this document, up to date after the
    /// changes in `delta`. Re-read only what they touched: this runs on every
    /// keystroke, local or a collaborator's. The default reads everything
    /// again, which is right for small resources.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn refresh(state: &mut Self::State, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if !delta.is_empty() {
            *state = Self::read(doc)?;
        }
        Ok(())
    }

    /// Opt in to reusing the document-owned publication between changes.
    ///
    /// Return an epoch for every non-document input used by declarations,
    /// children, views or local operations (for example the local date of a
    /// daily-note action). The framework also invalidates on state and locale
    /// revisions; surface bindings and effect availability remain live.
    /// `None` preserves publication on every request for dynamic resources.
    fn publication_cache_epoch(_state: &Self::State) -> Option<String> {
        None
    }

    /// Where `obj` sits in the document, when the state already knows: the
    /// path from the root to it. A change to an object the state has read
    /// (the paragraph being typed in) is then placed without asking the
    /// document, which for a large document is the costly part of reading a
    /// change. The default knows nothing, and the document is asked.
    fn locate(state: &Self::State, obj: &automerge::ObjId) -> Option<crate::delta::Path> {
        let _ = (state, obj);
        None
    }

    /// Make sure the parts of the state views asked for are read, and drop
    /// parts nobody asks for any more. For a resource too large to read
    /// whole, [`read`](Self::read) reads a spine (the order of a document's
    /// paragraphs, the rows of a sheet) and this reads the parts on screen.
    /// Called after every read and refresh and whenever the demand changes,
    /// so it must be cheap when everything demanded is already there. The
    /// default reads nothing: the state is read whole.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn load(state: &mut Self::State, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        let _ = (state, doc, demand);
        Ok(())
    }

    /// Write the initial content of a brand-new record: its genesis.
    ///
    /// The genesis is made under an actor derived from the record's identity,
    /// at time zero, so it is the same change byte for byte on every device:
    /// two people who each create the same record before they sync (a room's
    /// default record, the record of one file) converge on one record rather
    /// than two competing roots. It must therefore depend on nothing but the
    /// resource type — not on who creates it, the device, the clock or a
    /// file. Content that does depend on those (the creator as owner, a
    /// first-run import of settings that already exist) is the app's seed,
    /// applied as actions right after the genesis (`ResourceApp::seed` in
    /// `tilo-app`).
    ///
    /// # Errors
    /// Returns the document's write error.
    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        Ok(())
    }

    /// Whether `next`, applied right after `previous` by the same person,
    /// continues it as one undo step — the characters of a word typed one
    /// keystroke at a time, the values a slider passes through in one drag.
    /// The default keeps every action its own step.
    ///
    /// Immediate edits (`Dispatcher::apply_now` in `tilo-app`) and native
    /// keyboard/IME edits from action-bound text fields may continue one
    /// another. Programmatic/agent invocations remain separate steps.
    /// A surface ends an immediate pointer gesture
    /// (`Dispatcher::settle`) when the pointer is let go, so the next drag
    /// of the same slider is a step of its own.
    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        let _ = (previous, next);
        false
    }

    /// Whether a later action may continue `action` ([`coalesces`](Self::coalesces)),
    /// so its undo step stays open for it. One that never continues — a
    /// paragraph split after a typed word, a block moved — is a step of its
    /// own, recorded at once. The default keeps every step open for what may
    /// follow.
    fn continuable(action: &Self::Action) -> bool {
        let _ = action;
        true
    }

    /// Where `action`, just applied, leaves the participant who took it, read
    /// off the state after it: the paragraph an append wrote and the caret
    /// at its end. Every collaborator's view draws that participant there,
    /// so one with no pointer or caret of its own (an agent) shows where it
    /// works. The default places it nowhere in particular.
    fn lands(state: &Self::State, action: &Self::Action) -> Option<Place> {
        let _ = (state, action);
        None
    }

    /// The least a participant must be allowed to do for `action` to apply:
    /// [`Write`](PermissionRequirement::Write) by default, `Comment` for a
    /// reply or a note someone who may only comment can still add. The
    /// dispatcher a view or surface edits through checks it, and a declared
    /// action that builds it names the same level with
    /// [`ActionDecl::requiring`].
    fn permission(action: &Self::Action) -> PermissionRequirement {
        let _ = action;
        PermissionRequirement::Write
    }

    /// Publish the bounded audited mutation door. Default resources retain
    /// their existing catalogue. Enabling this also requires a closed
    /// [`allows_low_mutation`](Self::allows_low_mutation) whitelist.
    const AUDITED_LOW_MUTATIONS: bool = false;

    /// Opt in an audited state-only mutation to the bounded low-risk door.
    /// Default-deny: a default `Action::risk()` is insufficient authority.
    /// Implementations must use a closed whitelist excluding I/O, commands,
    /// capability grants and destructive mutations. The target also requires
    /// actual `RiskLevel::Low` and checks the action's current permission.
    fn allows_low_mutation(action: &Self::Action) -> bool {
        let _ = action;
        false
    }

    /// The least any of the resource's actions needs
    /// ([`permission`](Self::permission)): what the generic [`APPLY`] and
    /// group actions are offered under, each invocation then checked
    /// against what its value needs. Writing by default; `Comment` for a
    /// resource someone who may only comment can still reply in.
    fn least_permission() -> PermissionRequirement {
        PermissionRequirement::Write
    }

    /// Mutations offered by name. The generic [`APPLY`] action is always
    /// offered in addition, so any typed value can reach the host.
    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    /// Mutations whose arguments are bound from `state` at publish time
    /// (clear-all that names the current rows): typed builders of
    /// [`Action`](Self::Action), published beside
    /// [`declarations`](Self::declarations) and checked, confirmed and undone
    /// the same way. Prefer [`declarations`](Self::declarations) when the
    /// action does not need the state. An action bound to where a person is
    /// rather than to the document (the selection, the current folder) is
    /// bound by the view or surface that knows it.
    fn bound_declarations(_state: &Self::State) -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    /// View and effect operations that depend on `state` (start or stop,
    /// carrying the current on/off), published on the same resource as
    /// [`declarations`](Self::declarations). Static ones stay on
    /// [`local_operations`](Self::local_operations); a mutation bound from
    /// the state is a [`bound_declarations`](Self::bound_declarations) entry,
    /// never a hand-built document spec here.
    fn bound_actions(_state: &Self::State) -> Vec<tilo_actions::ActionSpec> {
        Vec::new()
    }

    /// The JSON Schema of [`Action`](Self::Action)'s serde form, published as
    /// the schema of the generic [`APPLY`] operation's argument, so agents
    /// and scripts can read the edit vocabulary from the descriptor and
    /// binding refuses a malformed edit by name. A variant that only
    /// restates a declaration is published as a reference to it. Return `Some(<Self::Action as
    /// JsonSchema>::json_schema())` for an action type deriving
    /// [`JsonSchema`](crate::JsonSchema); `#[resource]` describes its
    /// operations from their declarations. The default publishes no schema.
    fn action_schema() -> Option<serde_json::Value> {
        None
    }

    /// Resources nested inside this one (a track's clips, a list's items,
    /// a page's rich-text body), each exporting actions with their arguments
    /// already bound. Published beneath the root resource for the palette,
    /// agents and scripts — the same door as the root, not a second tree.
    fn children(_state: &Self::State) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }

    /// The nested resource `id`, when [`children`](Self::children) leaves it
    /// out. A resource with more children than a published tree should carry
    /// (a book of contacts, a long history) publishes a bounded page from
    /// `children` and resolves the rest here, so an action addressed to any
    /// of them still reaches it through the same door. The default resolves
    /// nothing beyond `children`.
    fn child(_state: &Self::State, _id: &str) -> Option<tilo_actions::Resource> {
        None
    }

    /// View and effect operations on this resource: local presentation and
    /// jobs that have no document delta (show a week, start a sync drain,
    /// open a system file picker). They are published on the same tree as
    /// [`declarations`](Self::declarations). A document mutation does not
    /// belong here — it is a [`declarations`](Self::declarations) entry or
    /// [`APPLY`]. `publish` drops a `Document` spec so it cannot become a
    /// second write path.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        Vec::new()
    }

    /// Named presentations of this resource (editor, reader) with stable ids.
    /// Not an action called `open`: ordinary presentation toggles never become
    /// composable views by accident. Defaults to none.
    fn views(_state: &Self::State) -> Vec<tilo_actions::ResourceViewSpec> {
        Vec::new()
    }

    /// Entries for the system search index.
    fn search_scopes(_state: &Self::State) -> Vec<ScopeDecl> {
        Vec::new()
    }

    fn search(_state: &Self::State) -> Vec<SearchEntry> {
        Vec::new()
    }

    /// Whether [`read_content`](Self::read_content) answers. A resource that
    /// does is published with the framework's `read` operation, so a script,
    /// an agent or an assistive tool reads what it holds as typed data through
    /// the same door, permissions and audit as every other action — never
    /// from a screenshot.
    const READABLE: bool = false;

    /// What the resource holds, as JSON a caller can act on: with no `item`,
    /// the resource itself (a notebook's pages); with `item`, the nested
    /// resource of that id from [`children`](Self::children) (one page's
    /// title and blocks). Read from `doc`, the committed document, so the
    /// answer is never older than the action that preceded it. Called only
    /// when [`READABLE`](Self::READABLE) is set.
    ///
    /// # Errors
    /// A message naming why: no such item, or a document that cannot be read.
    fn read_content(
        state: &Self::State,
        doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let _ = (state, doc, item);
        Err(format!("a {} does not answer reads", Self::LABEL))
    }

    /// What declared action `action` (one [`ActionDecl::answering`]) made,
    /// answered to its caller once it applied to `doc` as `changes`: the
    /// ids it minted, the area it selected, a warning that what it drew is
    /// hidden. A compact summary, so the caller needs no second read to
    /// see what happened. The default answers nothing.
    fn answer(
        _doc: &Doc,
        _action: &str,
        _changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        None
    }

    /// The stored layout of this resource's content, for services that act
    /// on it generically: History restores a past version through it, field
    /// by field and item by item. `#[resource]` returns the record's
    /// [`Model::SHAPE`](crate::Model::SHAPE); the default opts out.
    fn shape() -> Option<crate::Shape> {
        None
    }

    /// Register this resource's replay decoder. Override to register an
    /// existing vocabulary under additional type ids.
    fn register(registry: &mut ActionRegistry) {
        registry.register::<Self::Action>(Self::ACTION_TYPE);
    }
}

/// The id of the action every resource publishes that applies one typed
/// [`Resource::Action`] given as JSON. It is how a custom surface, an agent or
/// a script submits an edit that has no named declaration (a drag with its
/// final geometry) through the same permission-checked, undoable path.
///
/// It is programmatic: never offered in the palette or a menu, where a
/// person picks named actions. Its risk is the value's own
/// ([`tilo_actions::Action::risk`]), so a destructive value — deleting a clip —
/// needs the same confirmation a declared destructive action does, while an
/// ordinary edit applies directly.
pub const APPLY: &str = "apply";
/// The parameter of [`APPLY`] carrying the JSON-encoded action.
pub const APPLY_PARAM: &str = "action";

/// The edits of a resource whose document holds nothing. An app whose data
/// lives entirely outside any document — credentials in this device's
/// encrypted vault, a device's hardware — publishes that data as live rows
/// and effects, and its resource is only the door they are published on.
/// No value exists, so nothing is ever applied, undone or synced.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum NoAction {}

impl tilo_actions::Action for NoAction {
    // No value exists to apply.
    fn apply(&self, _doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        Ok(())
    }
}

/// A search index entry. `anchor` addresses what a hit opens (an item id);
/// `action`, when set, is the published action a hit invokes, so choosing it
/// goes through the same door, permissions and history as the palette.
///
/// How a hit reads is the entry's too: the result group it is listed under
/// (the resource's label unless `group` names another, such as "Timeline
/// markers"), a dimmer `subtitle` line, a named `icon` (a lucide kebab name,
/// `"star"`), and a `boost` that floats pinned or recent items over an equal
/// match. Long prose an entry is found by — a book's chapter, a long note —
/// is its `body`, which the index reads in place rather than the entry
/// copying it, and whose segments may each be a hit of their own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchEntry {
    /// The containing resource scope for a flat anchor.
    pub within: Option<String>,
    /// Presentation for the platform-rendered result row.
    pub row: Option<SearchRow>,
    pub anchor: Option<String>,
    pub title: String,
    pub text: String,
    pub action: Option<tilo_actions::ActionAddress>,
    pub group: Option<String>,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    pub boost: i32,
    pub body: Option<SearchBody>,
    /// More short text the entry is found by, each ranked by its own
    /// weight: a sender's address, a book's subject, a message's labels.
    pub fields: Vec<(String, SearchWeight)>,
    /// Structured filter values: token key, localized group label, value.
    pub facets: Vec<(String, String, String)>,
}

/// How much a match in one of a [`SearchEntry`]'s fields counts. The title
/// ranks as [`Self::Title`], the subtitle as [`Self::Subtitle`] and the text
/// as [`Self::Body`]; [`SearchEntry::field`] adds others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchWeight {
    /// The name a person looks for.
    Title,
    /// A stable identity typed verbatim: an address, an id, a path.
    Identity,
    /// A secondary line: an author, a sender, a folder.
    Subtitle,
    /// Other short fields: labels, tags, a subject.
    Metadata,
    /// Long prose.
    Body,
    /// Found, but never reorders hits.
    Unranked,
}

/// One run of a [`SearchText`] segment: styled text, say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    pub space_before: bool,
}

/// Long text a search entry is found by, owned by the app: the index reads
/// it while indexing and never copies it. It is a sequence of segments (the
/// unit a hit can point at: a paragraph, a block), each a sequence of
/// pieces. It must not change once handed to an entry; changed prose is a
/// new body.
pub trait SearchText: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<SearchPiece<'_>>;

    /// Whether a match in `segment` is a hit of its own ([`Self::location`])
    /// rather than the entry's.
    fn locates(&self, segment: usize) -> bool {
        let _ = segment;
        false
    }

    /// The hit a match in `segment` is — a reading location, invoking the
    /// action that opens the text there — when [`Self::locates`] says so.
    fn location(&self, segment: usize) -> Option<SearchEntry> {
        let _ = segment;
        None
    }
}

/// A [`SearchText`] an entry carries. Cloning one is cheap, and two bodies
/// are the same body only when they share one text, so an app that rebuilds
/// its entries over mostly unchanged prose keeps its bodies and hands the
/// same ones back.
#[derive(Clone)]
pub struct SearchBody(Arc<dyn SearchText>);

impl SearchBody {
    #[must_use]
    pub fn new(text: Arc<dyn SearchText>) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn text(&self) -> &dyn SearchText {
        self.0.as_ref()
    }

    /// The shared text, for an index that reads it later.
    #[must_use]
    pub fn shared(&self) -> Arc<dyn SearchText> {
        Arc::clone(&self.0)
    }
}

impl PartialEq for SearchBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SearchBody {}

impl std::fmt::Debug for SearchBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchBody")
            .field("segments", &self.0.segment_count())
            .finish()
    }
}

impl SearchEntry {
    /// Offer this entry under one value of a platform search facet.
    #[must_use]
    pub fn facet(
        mut self,
        key: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.facets.push((key.into(), label.into(), value.into()));
        self
    }

    /// Whether the entry lies in `scope`: its anchor, or the scope it is
    /// [`within`](Self::within), is the scope or lies beneath it.
    #[must_use]
    pub fn in_scope(&self, scope: &str) -> bool {
        [self.within.as_deref(), self.anchor.as_deref()]
            .into_iter()
            .flatten()
            .any(|anchor| scope_contains(scope, anchor))
    }

    /// An entry titled `title` whose body text is `text`.
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            ..Self::default()
        }
    }

    /// The item a hit opens.
    #[must_use]
    pub fn anchored(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// The published action a hit invokes.
    #[must_use]
    pub fn invoking(mut self, action: tilo_actions::ActionAddress) -> Self {
        self.action = Some(action);
        self
    }

    /// List hits under `group` rather than the resource's label.
    #[must_use]
    pub fn grouped(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// A secondary line under the title: where the item is, what it is.
    #[must_use]
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The glyph a hit leads with, by its lucide kebab name (`"star"`).
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Rank the hit `boost` above an equal match: a nudge, never an
    /// override of how well it matches.
    #[must_use]
    pub fn boosted(mut self, boost: i32) -> Self {
        self.boost = boost;
        self
    }

    /// Find the entry by `text` too, a match in it ranked as `weight`.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: SearchWeight) -> Self {
        let text = text.into();
        if !text.trim().is_empty() {
            self.fields.push((text, weight));
        }
        self
    }

    /// Find the entry by `body` too, read in place.
    #[must_use]
    pub fn with_body(mut self, body: SearchBody) -> Self {
        self.body = Some(body);
        self
    }
}

/// Where a participant is inside a resource, in the resource's own terms:
/// the same on every collaborator's screen whatever its layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// An item (a paragraph, a clip, a row).
    Item(String),
    /// A caret at a character position inside an item.
    Caret { item: String, at: usize },
    /// Characters `start..end` of an item.
    Range {
        item: String,
        start: usize,
        end: usize,
    },
}

/// Builds a resource's action from arguments already bound against the
/// declaration's parameters. `now_ms` is the origin's clock, captured once so
/// replay is deterministic.
pub type BuildAction<R> =
    Arc<dyn Fn(&ActionArgs, i64) -> Result<<R as Resource>::Action, String> + Send + Sync>;

/// One mutation offered by name.
pub struct ActionDecl<R: Resource> {
    pub id: &'static str,
    pub label: String,
    pub description: Option<String>,
    pub params: Vec<ActionParam>,
    pub permission: PermissionRequirement,
    pub destructive: bool,
    /// What the action answers its caller with, beside the change: the state
    /// it left, as the declaring resource's surface reports it.
    pub result: Option<tilo_actions::ResultSpec>,
    /// Contract tags its callers read: a precondition on the invoker's view
    /// (`ContextRequirement`), a text that may arrive in parts.
    pub tags: Vec<String>,
    /// The prompt of an action whose one input is content another action
    /// produced (an image to place), carried by its callers rather than
    /// typed: the build reads its content hash as the
    /// [`PROMPTED_PARAM`](tilo_actions::PROMPTED_PARAM) argument.
    pub content: Option<String>,
    /// A bound edit that refines the resource's declared action of the same
    /// id: it builds the same action from the same arguments, and also
    /// places its invoker (the caret an append leaves its writer at).
    /// See [`bind_declaration`].
    pub refines: bool,
    pub build: BuildAction<R>,
}

impl<R: Resource> Clone for ActionDecl<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            label: self.label.clone(),
            description: self.description.clone(),
            params: self.params.clone(),
            permission: self.permission,
            destructive: self.destructive,
            result: self.result.clone(),
            tags: self.tags.clone(),
            content: self.content.clone(),
            refines: self.refines,
            build: Arc::clone(&self.build),
        }
    }
}

impl<R: Resource> ActionDecl<R> {
    pub fn new(
        id: &'static str,
        label: impl Into<String>,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs, i64) -> Result<R::Action, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            description: None,
            params,
            permission: PermissionRequirement::Write,
            destructive: false,
            result: None,
            tags: Vec::new(),
            content: None,
            refines: false,
            build: Arc::new(build),
        }
    }

    /// Mark a bound edit as refining the resource's declared action of the
    /// same id ([`bind_declaration`]).
    #[must_use]
    pub fn refining(mut self) -> Self {
        self.refines = true;
        self
    }

    /// Tag the action's contract (`ContextRequirement::Caret.tag()`, an
    /// incremental-text declaration) for the callers that read it.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
        self
    }

    /// Take content another action produced as the one input, described by
    /// `prompt`; see [`Self::content`].
    #[must_use]
    pub fn collecting_content(mut self, prompt: impl Into<String>) -> Self {
        self.content = Some(prompt.into());
        self
    }

    #[must_use]
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Declare what the action answers its caller with (an outline of the
    /// state it left), which the hosting surface supplies after it applies.
    #[must_use]
    pub fn returns(mut self, result: tilo_actions::ResultSpec) -> Self {
        self.result = Some(result);
        self
    }

    #[must_use]
    pub fn requiring(mut self, permission: PermissionRequirement) -> Self {
        self.permission = permission;
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Answer the caller with what the action made, from
    /// [`Resource::answer`].
    #[must_use]
    pub fn answering(self) -> Self {
        self.returns(tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        })
    }
}

/// Milliseconds since the Unix epoch on this machine. Only ever read on the
/// origin, when an action is built; never inside `apply`.
#[must_use]
pub fn origin_now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

/// Encode a resource action under its registry type id.
///
/// # Errors
/// Returns [`ActionError::Encode`] when the action does not serialize.
pub fn encode<R: Resource>(action: &R::Action) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(R::ACTION_TYPE, action)
}

/// What history calls one action value of `R`: the label of the declared
/// action it names (a `remove-marks` value is "Remove marks"), else its
/// variant name in words, or `None` when it names neither.
///
/// The generic [`APPLY`] and a [`GROUP`](crate::GROUP) of one kind of edit
/// are recorded under this name, so a person's history says what each edit
/// did rather than that an edit was applied.
pub fn action_label<R: Resource>(action: &R::Action) -> Option<String> {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value_label::<R>(&value))
}

fn value_label<R: Resource>(value: &serde_json::Value) -> Option<String> {
    let declarations = R::declarations();
    let ids: Vec<&str> = declarations.iter().map(|decl| decl.id).collect();
    if let Some((id, _)) = crate::schema::declared_invocation(value, &ids) {
        return declarations
            .into_iter()
            .find(|decl| decl.id == id)
            .map(|decl| decl.label);
    }
    let value = value.get("op").unwrap_or(value);
    let variant = match value {
        serde_json::Value::String(variant) => variant.as_str(),
        serde_json::Value::Object(fields) => match fields.get("action") {
            Some(serde_json::Value::String(variant)) => variant.as_str(),
            _ if fields.len() == 1 => fields.keys().next()?.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    let words = in_words(variant);
    (!words.is_empty()).then_some(words)
}

/// `remove-marks`, `remove_marks` and `RemoveMarks` as "Remove marks".
fn in_words(identifier: &str) -> String {
    let mut words = String::with_capacity(identifier.len().saturating_add(4));
    let mut previous_lowercase_or_digit = false;
    for character in identifier.chars() {
        if matches!(character, '-' | '_' | ' ') {
            if !words.is_empty() && !words.ends_with(' ') {
                words.push(' ');
            }
            previous_lowercase_or_digit = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase_or_digit {
            words.push(' ');
        }
        previous_lowercase_or_digit = character.is_lowercase() || character.is_ascii_digit();
        if words.is_empty() {
            words.extend(character.to_uppercase());
        } else {
            words.extend(character.to_lowercase());
        }
    }
    words.trim_end().to_owned()
}

/// The history name of an encoded [`APPLY`]: its action's
/// [`action_label`].
pub(crate) fn apply_label<R: Resource>(encoded: &EncodedAction) -> Option<String> {
    if encoded.type_id != R::ACTION_TYPE {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(&encoded.payload).ok()?;
    value_label::<R>(&value)
}

/// The history name of an encoded [`GROUP`](crate::GROUP) whose members are
/// all one kind of edit (a selection's moves are "Edit marks"); a mixed group
/// keeps the group's own label.
pub(crate) fn group_label<R: Resource>(encoded: &EncodedAction) -> Option<String> {
    if encoded.type_id != crate::group::group_type::<R>() {
        return None;
    }
    let members = serde_json::from_slice::<Vec<serde_json::Value>>(&encoded.payload).ok()?;
    let mut labels = members.iter().map(value_label::<R>);
    let first = labels.next()??;
    labels
        .all(|label| label.as_deref() == Some(first.as_str()))
        .then_some(first)
}

/// The encoded form of a builder's result: the action, or a refusal that
/// carries the builder's reason.
pub(crate) fn encode_built<R: Resource>(built: Result<R::Action, String>) -> EncodedAction {
    match built {
        Ok(action) => encode::<R>(&action).unwrap_or_else(|error| refused::<R>(&error.to_string())),
        Err(reason) => refused::<R>(&reason),
    }
}

/// An action whose payload cannot be decoded, so the host refuses it with
/// the builder's reason instead of applying anything.
pub(crate) fn refused<R: Resource>(reason: &str) -> EncodedAction {
    let _ = R::ACTION_TYPE;
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: reason.as_bytes().to_vec(),
    }
}

/// The published spec of one declared or bound mutation: what the palette,
/// agents and scripts invoke, building `R::Action` from their arguments.
///
/// The action is built when it is invoked, never when it is published: one
/// that takes no arguments still mints its ids and reads the clock at the
/// moment it is taken, so two invocations make two distinct actions.
pub fn declaration_spec<R: Resource>(decl: &ActionDecl<R>) -> ActionSpec {
    let build = Arc::clone(&decl.build);
    let encoded =
        move |args: &tilo_actions::ActionArgs| encode_built::<R>(build(args, origin_now_ms()));
    let spec = if let Some(prompt) = &decl.content {
        let param = ActionParam::required_owned(
            tilo_actions::PROMPTED_PARAM,
            ParamType::Text,
            prompt.clone(),
        );
        ActionSpec::prompted_content(
            decl.id,
            decl.label.clone(),
            prompt.clone(),
            "content hash",
            ExecutionLocus::ClientLocal,
            move |hash| {
                let supplied = [(
                    tilo_actions::PROMPTED_PARAM.to_owned(),
                    tilo_actions::ArgValue::Text(hash.to_owned()),
                )];
                match ActionArgs::bind(std::slice::from_ref(&param), supplied) {
                    Ok(args) => encoded(&args),
                    Err(error) => refused::<R>(&error.to_string()),
                }
            },
        )
    } else if decl.params.is_empty() {
        ActionSpec::deferred(
            decl.id,
            decl.label.clone(),
            ExecutionLocus::ClientLocal,
            R::ACTION_TYPE,
            move || encoded(&tilo_actions::ActionArgs::none()),
        )
    } else {
        ActionSpec::typed(
            decl.id,
            decl.label.clone(),
            decl.label.clone(),
            ExecutionLocus::ClientLocal,
            decl.params.clone(),
            encoded,
        )
    };
    let mut spec = spec
        .requiring(decl.permission)
        // Every document action is undone by the host's undo.
        .reversible();
    if let Some(description) = &decl.description {
        spec = spec.with_description(description.clone());
    }
    if decl.destructive {
        spec = spec.destructive();
    }
    if let Some(result) = &decl.result {
        spec.contract.result = result.clone();
    }
    for tag in &decl.tags {
        if !spec.contract.tags.contains(tag) {
            spec.contract.tags.push(tag.clone());
        }
    }
    spec
}

/// Each declared action's arguments object, as serde reads it.
fn declared_args<R: Resource>() -> std::collections::BTreeMap<&'static str, serde_json::Value> {
    R::declarations()
        .iter()
        .map(|decl| (decl.id, crate::schema::args_schema(&decl.params)))
        .collect()
}

/// The parameter of [`APPLY`]: one action value, described by
/// [`Resource::action_schema`] when the resource has one, with what only
/// restates a named action folded into a reference to it.
pub(crate) fn apply_param<R: Resource>() -> ActionParam {
    let param = ActionParam::required_owned(
        APPLY_PARAM,
        ParamType::Object,
        format!("A {} action, as JSON", R::LABEL),
    );
    let schema = per_type::<R>(TypeSchema::Apply, || {
        R::action_schema()
            .map(|schema| crate::schema::fold_declared(&schema, &declared_args::<R>()))
    });
    match schema {
        Some(schema) => param.with_shared_schema(schema),
        None => param,
    }
}

/// The schemas a publication derives from the resource type alone.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum TypeSchema {
    Apply,
    Group,
}

#[derive(Default)]
struct CompiledSchemas {
    epoch: u64,
    entries: HashMap<(TypeId, TypeSchema), Option<Arc<serde_json::Value>>>,
}

static SCHEMAS: OnceLock<Mutex<CompiledSchemas>> = OnceLock::new();

/// Forget schemas derived by the previous app code before an in-place reload.
/// Hosts also call this when a reload is rejected, so the retained app rebuilds
/// its own definitions. Document and view changes do not invalidate type schemas.
#[doc(hidden)]
pub fn reset_type_schemas() {
    if let Some(cache) = SCHEMAS.get() {
        let mut store = cache.lock().unwrap_or_else(PoisonError::into_inner);
        store.epoch = store.epoch.wrapping_add(1);
        store.entries.clear();
    }
}

/// `build`'s schema for `R`, retained for this generation of app code.
fn per_type<R: Resource>(
    schema: TypeSchema,
    build: impl FnOnce() -> Option<serde_json::Value>,
) -> Option<Arc<serde_json::Value>> {
    let cache = SCHEMAS.get_or_init(Mutex::default);
    cached_schema(cache, (TypeId::of::<R>(), schema), build)
}

fn cached_schema(
    cache: &Mutex<CompiledSchemas>,
    key: (TypeId, TypeSchema),
    build: impl FnOnce() -> Option<serde_json::Value>,
) -> Option<Arc<serde_json::Value>> {
    let epoch = {
        let store = cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = store.entries.get(&key) {
            #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
            let _phase = crate::profiling::Phase::start("schema.cached_clone");
            return found.clone();
        }
        store.epoch
    };
    // Build outside the lock: a schema may publish another resource's.
    let value = build().map(Arc::new);
    let mut store = cache.lock().unwrap_or_else(PoisonError::into_inner);
    // A build started before a code reload cannot repopulate the new cache.
    if store.epoch == epoch {
        store.entries.insert(key, value.clone());
    }
    value
}

/// A JSON action value as [`APPLY`] reads it. A value naming a declared
/// action is checked against that declaration first, so it is refused with
/// the reason the named action would give.
fn read_apply<R: Resource>(value: serde_json::Value) -> Result<R::Action, String> {
    let declarations = R::declarations();
    let ids: Vec<&str> = declarations.iter().map(|decl| decl.id).collect();
    if let Some((id, fields)) = crate::schema::declared_invocation(&value, &ids)
        && let Some(decl) = declarations.iter().find(|decl| decl.id == id)
    {
        ActionArgs::bind_json(&decl.params, &fields).map_err(|error| format!("{id}: {error}"))?;
    }
    serde_json::from_value::<R::Action>(value).map_err(|error| error.to_string())
}

/// The parameter of [`GROUP`](crate::GROUP): invocations of the resource's
/// declared actions and of [`APPLY`]. A declared member's arguments are
/// checked against its own action's schema when the group is built, so this
/// names those actions rather than repeating their schemas; an `apply`
/// member carries `apply`'s schema, without its descriptions.
pub(crate) fn group_param<R: Resource>() -> ActionParam {
    use crate::schema::{
        array, constant, describe, nullable, object, one_of, string_enum, undescribed,
    };

    let schema = per_type::<R>(TypeSchema::Group, || {
        let ids: Vec<&str> = R::declarations().iter().map(|decl| decl.id).collect();
        let mut members = Vec::new();
        if !ids.is_empty() {
            members.push(describe(
                object(vec![
                    ("action", string_enum(&ids), true),
                    (
                        "args",
                        nullable(serde_json::json!({"type": "object"})),
                        false,
                    ),
                ]),
                "A named action, args as it takes them",
            ));
        }
        let action = apply_param::<R>().schema.map_or_else(
            || serde_json::json!({"type": "object"}),
            |schema| undescribed(&schema),
        );
        members.push(object(vec![
            (
                "action",
                if R::AUDITED_LOW_MUTATIONS {
                    string_enum(&[APPLY, crate::APPLY_LOW])
                } else {
                    constant(APPLY)
                },
                true,
            ),
            ("args", object(vec![(APPLY_PARAM, action, true)]), true),
        ]));
        Some(array(one_of(members)))
    })
    .unwrap_or_else(|| Arc::new(serde_json::json!({"type": "array"})));
    ActionParam::required_owned(
        crate::GROUP_PARAM,
        ParamType::Array,
        format!(
            "{} actions to apply in order as one step, each {{\"action\": id, \"args\": {{..}}}}",
            R::LABEL
        ),
    )
    .with_shared_schema(schema)
}

fn apply_spec<R: Resource>() -> ActionSpec {
    #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
    let _phase = crate::profiling::Phase::start("resource.apply_spec");
    ActionSpec::typed(
        APPLY,
        format!("Apply {} edit", R::LABEL),
        "Action as JSON",
        ExecutionLocus::ClientLocal,
        vec![apply_param::<R>()],
        |args| {
            let Some(tilo_actions::ArgValue::Object(value)) = args.get(APPLY_PARAM) else {
                return refused::<R>("missing action");
            };
            match read_apply::<R>(serde_json::Value::Object(value.clone())) {
                Ok(action) => {
                    encode::<R>(&action).unwrap_or_else(|error| refused::<R>(&error.to_string()))
                }
                Err(reason) => refused::<R>(&reason),
            }
        },
    )
    .with_description(
        "One edit as a typed JSON value: {action: id, ...its params}. The named action does the same and says more.",
    )
    // A typed edit is how a surface, an agent or a script says what should
    // happen without a named declaration; a person picks named actions.
    .programmatic()
    // Offered to whoever may take the least of its edits; each value needs
    // what that edit needs.
    .requiring(R::least_permission())
    .with_permission_of(|args| {
        let Some(tilo_actions::ArgValue::Object(value)) = args.get(APPLY_PARAM) else {
            return PermissionRequirement::Write;
        };
        read_apply::<R>(serde_json::Value::Object(value.clone()))
            .map_or(PermissionRequirement::Write, |action| R::permission(&action))
    })
    // History names each value after what it did, not after `apply`.
    .with_history_label(apply_label::<R>)
    // Each value is classified: deleting is destructive, trimming is not, so
    // a destructive value gets the confirmation a declared one would.
    .with_risk(|args| {
        let Some(tilo_actions::ArgValue::Object(value)) = args.get(APPLY_PARAM) else {
            return tilo_actions::RiskLevel::Low;
        };
        serde_json::from_value::<R::Action>(serde_json::Value::Object(value.clone()))
            .map_or(tilo_actions::RiskLevel::Low, |action| {
                tilo_actions::Action::risk(&action)
            })
    })
}

/// How much a requirement asks, least first.
pub(crate) fn permission_rank(requirement: PermissionRequirement) -> u8 {
    match requirement {
        PermissionRequirement::None => 0,
        PermissionRequirement::Read => 1,
        PermissionRequirement::Comment => 2,
        PermissionRequirement::Write => 3,
        PermissionRequirement::Invite => 4,
        PermissionRequirement::Admin => 5,
    }
}

fn group_spec<R: Resource>() -> ActionSpec {
    #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
    let _phase = crate::profiling::Phase::start("resource.group_spec");
    ActionSpec::typed(
        crate::GROUP,
        format!("Apply several {} edits as one", R::LABEL),
        "Invocations as JSON",
        ExecutionLocus::ClientLocal,
        vec![group_param::<R>()],
        |args| match crate::group::build_group::<R>(&args.to_json(), origin_now_ms()) {
            Ok(group) => EncodedAction::encode(crate::group::group_type::<R>(), &group)
                .unwrap_or_else(|error| refused::<R>(&error.to_string())),
            Err(reason) => refused::<R>(&reason),
        },
    )
    .with_description(
        "Several actions as one all-or-nothing step: one undo, and if any member refuses nothing is applied. actions: [{action: id, args: {..}}].",
    )
    // One group is one undo step, like any other document action.
    .reversible()
    // A group of one kind of edit is named after it.
    .with_history_label(group_label::<R>)
    // A gesture's edits, from a surface, a script or an agent; a person picks
    // the named actions it is made of.
    .programmatic()
    // Needs what its most demanding member needs.
    .requiring(R::least_permission())
    .with_permission_of(|args| {
        crate::group::build_group::<R>(&args.to_json(), 0).map_or(
            PermissionRequirement::Write,
            |group| {
                group
                    .actions
                    .iter()
                    .map(R::permission)
                    .max_by_key(|requirement| permission_rank(*requirement))
                    .unwrap_or(PermissionRequirement::Write)
            },
        )
    })
    // As consequential as its most consequential member.
    .with_risk(|args| {
        crate::group::build_group::<R>(&args.to_json(), 0)
            .map_or(tilo_actions::RiskLevel::Low, |group| {
                tilo_actions::Action::risk(&group)
            })
    })
}

/// The resource tree published for one open resource: every declared and
/// state-bound action, the generic [`APPLY`] and [`GROUP`](crate::GROUP)
/// actions, the named [`Resource::views`], and the nested resources.
///
/// This is the only bridge from a [`Resource`] type to the
/// [`tilo_actions::Resource`] tree the palette, agents, scripts and the shell
/// door see. Apps do not construct a second tree for the same state.
///
/// An action offered on the items of a collection (see [`crate::items`])
/// stays published on the record, with its id parameter, for scripts and
/// agents; a person picks it on the item, so it is not a palette row here.
pub fn publish<R: Resource>(id: &str, label: &str, state: &R::State) -> tilo_actions::Resource {
    publish_parts::<R>(id, label, Some(state))
}

/// [`publish`], or without a state only what does not depend on one: the
/// declared actions, [`APPLY`], [`GROUP`](crate::GROUP) and the local
/// operations.
pub(crate) fn publish_parts<R: Resource>(
    id: &str,
    label: &str,
    state: Option<&R::State>,
) -> tilo_actions::Resource {
    let children = state.map(R::children).unwrap_or_default();
    let mut resource = tilo_actions::Resource::new(id, R::KIND, label);
    // Describe the declared domain ID before any host dispatch alias projection.
    resource.semantic_identity = tilo_actions::ResourceSemanticIdentity::new(R::KIND, id);
    for decl in R::declarations() {
        #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
        let _phase = crate::profiling::Phase::start("resource.declaration_spec");
        let spec = declaration_spec(&decl);
        // Offered on the items it acts on, with some of its arguments bound
        // ([`items::bound`](crate::items::bound)), it stays published here
        // for scripts that name the item, out of the palette's way.
        resource = resource.with_action(if crate::items::offered_bound(&children, &decl) {
            spec.programmatic()
        } else {
            spec
        });
    }
    for decl in state.map(R::bound_declarations).unwrap_or_default() {
        if resource
            .actions
            .iter()
            .any(|existing| existing.id == decl.id)
        {
            continue;
        }
        resource = resource.with_action(declaration_spec(&decl));
    }
    for spec in state.map(R::bound_actions).unwrap_or_default() {
        // Document actions and state-dependent views or effects. Static views
        // and effects stay on `local_operations`.
        resource = resource.with_action(spec);
    }
    for view in state.map(R::views).unwrap_or_default() {
        resource = resource.with_view(view);
    }
    for spec in R::local_operations() {
        if spec.kind == OperationKind::Document {
            continue;
        }
        resource = resource.with_action(spec);
    }
    resource = resource
        .with_action(apply_spec::<R>())
        .with_action(group_spec::<R>());
    if R::AUDITED_LOW_MUTATIONS {
        resource = resource
            .with_action(crate::low_mutation::apply_spec::<R>())
            .with_action(crate::low_mutation::group_spec::<R>());
    }
    resource.with_children(children)
}

/// Build the action a declaration produces for JSON arguments, validating them
/// against its parameters exactly as the host does.
///
/// # Errors
/// Returns why the action is unknown or the arguments do not bind.
pub fn build_action<R: Resource>(
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    now_ms: i64,
) -> Result<R::Action, String> {
    if id == crate::GROUP {
        return Err("a group is several actions; build it with `group::build_group`".into());
    }
    if id == crate::APPLY_LOW {
        return crate::low_mutation::build_action::<R>(arguments);
    }
    if id == APPLY {
        let value = arguments
            .get(APPLY_PARAM)
            .cloned()
            .ok_or_else(|| "missing action".to_owned())?;
        return read_apply::<R>(value);
    }
    let decl = R::declarations()
        .into_iter()
        .find(|decl| decl.id == id)
        .ok_or_else(|| format!("{} has no action `{id}`", R::LABEL))?;
    let args = ActionArgs::bind_json(&decl.params, arguments).map_err(|error| error.to_string())?;
    (decl.build)(&args, now_ms)
}

/// `resource` with the bound edit `decl` published on it.
///
/// An id the resource does not declare is added. One it declares is kept,
/// unless `decl` says it refines it ([`ActionDecl::refining`]) and takes the
/// same parameters with the same permission and risk: then it builds the
/// same edit from the same arguments and only adds where it was invoked from
/// (an append that leaves its writer's caret after the text), and replaces
/// the declared spec while the binding lasts.
pub fn bind_declaration<R: Resource>(
    resource: tilo_actions::Resource,
    decl: &ActionDecl<R>,
) -> tilo_actions::Resource {
    let spec = declaration_spec(decl);
    let mut resource = resource;
    match resource
        .actions
        .iter_mut()
        .find(|existing| existing.id == decl.id)
    {
        Some(declared) => {
            if decl.refines && same_shape(&spec, declared) {
                *declared = spec;
            }
            resource
        }
        None => resource.with_action(spec),
    }
}

fn same_shape(bound: &ActionSpec, declared: &ActionSpec) -> bool {
    let shape = |spec: &ActionSpec| {
        spec.params()
            .iter()
            .map(|param| (param.name.to_string(), param.ty, param.required))
            .collect::<Vec<_>>()
    };
    bound.kind == declared.kind
        && shape(bound) == shape(declared)
        && bound.contract.permission == declared.contract.permission
        && bound.contract.risk == declared.contract.risk
}

/// A resource of the person's own that one app keeps for every app of
/// theirs: their accounts, which Settings keeps and Mail signs in to. Each
/// app holds its copy in its own store; the copies are one record, the same
/// on every device, and converge over the person's own room. Only the
/// [`OWNER`](Self::OWNER) and the [`EDITORS`](Self::EDITORS) change it; every
/// other app reads it (`ViewCx::shared_person` in `tilo-app`).
pub trait SharedRecord: Resource {
    /// The app that keeps the record.
    const OWNER: &'static str;
    /// The other apps whose edits the record takes.
    const EDITORS: &'static [&'static str] = &[];

    /// Whether the app `app_id` changes the record.
    #[must_use]
    fn edited_by(app_id: &str) -> bool {
        app_id == Self::OWNER || Self::EDITORS.contains(&app_id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchRow {
    /// The dimmer line under the title (an address, a path, a date).
    pub subtitle: Option<String>,
    /// A named icon from the icon set (`"mail"`, `"calendar"`); `None` draws
    /// the app's own.
    pub icon: Option<String>,
    /// What the hit is, for the row's shape.
    pub shape: SearchRowShape,
}

impl SearchRow {
    /// A generic row with `subtitle` under the title.
    pub fn subtitle(subtitle: impl Into<String>) -> Self {
        Self {
            subtitle: Some(subtitle.into()),
            ..Self::default()
        }
    }

    /// A row of `shape`.
    #[must_use]
    pub fn shaped(shape: SearchRowShape) -> Self {
        Self {
            shape,
            ..Self::default()
        }
    }

    /// The row's named icon.
    #[must_use]
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SearchRowShape {
    /// Icon, title and subtitle.
    #[default]
    Plain,
    /// A message: who sent it, a snippet, when, and whether it is unread.
    Message {
        sender: String,
        snippet: Option<String>,
        date: Option<String>,
        unread: bool,
    },
    /// A video, track or other media item: who made it and how long it
    /// runs. (A provider with artwork answers its own rows, with
    /// `AppImage::from_image_store` thumbnails.)
    Media {
        byline: Option<String>,
        duration: Option<String>,
    },
    /// A match inside a file: the path, the line and the matched line.
    FileHit {
        path: String,
        line: Option<u32>,
        snippet: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct ScopeDecl {
    pub anchor: String,
    pub label: String,
}

impl ScopeDecl {
    pub fn new(anchor: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            anchor: anchor.into(),
            label: label.into(),
        }
    }

    /// Whether the hit anchored at `anchor` lies inside this scope.
    #[must_use]
    pub fn contains(&self, anchor: &str) -> bool {
        scope_contains(&self.anchor, anchor)
    }
}

/// Whether `anchor` is `scope` or lies beneath it in the anchor path.
#[must_use]
pub fn scope_contains(scope: &str, anchor: &str) -> bool {
    anchor
        .strip_prefix(scope)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod schema_sharing_tests {
    use super::*;

    #[test]
    fn shared_schema_reloads_do_not_retain_or_repopulate_stale_generation() {
        let cache = Mutex::new(CompiledSchemas::default());
        let key = (TypeId::of::<()>(), TypeSchema::Apply);
        let first = cached_schema(&cache, key, || Some(serde_json::json!({"minimum":1}))).unwrap();
        let reused = cached_schema(&cache, key, || panic!("cached type must not rebuild")).unwrap();
        assert!(Arc::ptr_eq(&first, &reused));
        {
            let mut store = cache.lock().unwrap();
            store.epoch += 1;
            store.entries.clear();
        }
        let old_build = cached_schema(&cache, key, || {
            let mut store = cache.lock().unwrap();
            store.epoch += 1;
            store.entries.clear();
            drop(store);
            Some(serde_json::json!({"minimum":2}))
        })
        .unwrap();
        assert!(
            cache.lock().unwrap().entries.is_empty(),
            "old build cannot repopulate new generation"
        );
        let current =
            cached_schema(&cache, key, || Some(serde_json::json!({"minimum":3}))).unwrap();
        assert_eq!(first["minimum"], 1);
        assert_eq!(old_build["minimum"], 2);
        assert_eq!(current["minimum"], 3);
        assert!(!Arc::ptr_eq(&first, &current));
    }
}
