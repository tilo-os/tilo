//! One record of one resource kind, kept by the document host: its document, the tree it
//! offers, the readings it answers, and the actions it admits.
//!
//! See the [crate docs](crate) for what this is and why an app's process does not hold it.
//!
//! The model runs in the app's own WebAssembly module ([`crate::Module`]), never linked into
//! the session: this record asks it, and decides whether to keep what it answered. The module
//! never hands over the host's types — it answers a [`ResourceNode`] tree and a reading as
//! JSON — so the tree a caller is offered is built here, with every verb addressing the
//! resource it is invoked on, and the change the module made is checked here before it is
//! adopted.

use std::rc::Rc;

use serde_json::{Map, Value as Json};
use tilo_actions::{
    ActionArgs, ActionInvoker, ActionOutput, DoorError, EncodedAction, ExecutionLocus, ParamType,
    ResourceSpec, Target,
};
use tilo_model_abi::{ActionNode, Answer, Asked, Op, ParamNode, Request, ResourceNode};

use crate::module::Module;

/// Who an action is recorded as: the session decides it
/// (`docs/architecture/apps.md` §4), never the app.
///
/// A view submits an action over its connection; the session says whether the person was
/// acting in the view (a gesture token), or the app acting on its own. The invoker that comes
/// with the action carries that decision, and this record keeps it: what the change is
/// attributed to, whose grant is checked, and the actor bytes the change is committed under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invoker {
    /// The invoker as the door recorded it: the person, or an agent acting for them, or the
    /// app.
    pub invoker: ActionInvoker,
    /// The actor the change is committed under. `None` commits it under the document's own
    /// actor, for a record the person has not signed.
    pub writer: Option<Vec<u8>>,
}

impl Invoker {
    /// `invoker`'s change, committed under the actor `writer` names.
    #[must_use]
    pub fn new(invoker: ActionInvoker, writer: Option<Vec<u8>>) -> Self {
        Self { invoker, writer }
    }

    /// `invoker`'s change (the session's own decision of who acted: the person in a gesture,
    /// an agent, or the app), committed under the actor `writer`.
    #[must_use]
    pub fn of(invoker: ActionInvoker, writer: Option<Vec<u8>>) -> Self {
        Self { invoker, writer }
    }

    /// The person seated at this device acting for themselves, committed as `person`.
    #[must_use]
    pub fn person(person: impl Into<String>) -> Self {
        let person = person.into();
        Self {
            writer: Some(person.clone().into_bytes()),
            invoker: ActionInvoker::actor(person),
        }
    }
}

/// Why a record could not be opened, read or changed.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The module does not serve the record's kind.
    #[error("no model host runs a {0} record")]
    NoModel(String),
    /// The door refused the action.
    #[error(transparent)]
    Door(#[from] DoorError),
    /// The record is kept on disk and the store refused it.
    #[error("{0}")]
    Store(String),
    /// The document refused a change, or does not read as the model.
    #[error("{0}")]
    Document(String),
    /// The module refused the call.
    #[error("{0}")]
    Module(String),
}

/// What [`Record::open`] answers: the record, and whether opening it wrote its genesis (a
/// brand-new record) or read one that was already there — so a caller knows whether anything
/// of the person's was touched.
#[derive(Debug)]
pub struct Opened {
    pub record: Record,
    pub created: bool,
}

/// One record of one resource kind, kept here.
///
/// A record is opened once per app and workspace. The model runs in the app's module, so what
/// this holds is only the document the module answered and the tree the host built from it:
/// the module keeps nothing between calls, and this is the one copy of what the record holds.
pub struct Record {
    /// The kind the record holds (`tasks.tracker`).
    kind: String,
    /// The resource address this record is published at: readings and actions name it.
    address: String,
    /// The app whose module runs the model.
    app: String,
    /// The module the model runs in.
    module: Rc<Module>,
    /// The document, the one copy of what the record holds.
    doc: tilo_resource::Doc,
    /// The tree the module last answered, built into what this host offers, and the heads it
    /// was built at.
    published: std::cell::RefCell<Option<(Vec<automerge::ChangeHash>, ResourceSpec)>>,
    /// Where it is kept, when it is on disk ([`Record::open_kept`]).
    #[cfg(feature = "store")]
    kept: Option<crate::store::Kept>,
    /// Whether anything changed since a reader last asked.
    moved: bool,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Record")
            .field("kind", &self.kind)
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Record {
    /// The record of `kind` that `app`'s module keeps at `address`, over `doc`, in memory: the
    /// module is asked to open it, which writes its genesis when the document holds nothing
    /// yet.
    ///
    /// A document that does not read as the kind is brought up with the model's own migration,
    /// never rewritten by this host: what it held keeps its ids and nothing is deleted (the
    /// data invariants).
    pub fn open(
        app: &str,
        kind: &str,
        address: &str,
        doc: tilo_resource::Doc,
        module: Rc<Module>,
    ) -> Result<Opened, RecordError> {
        let mut record = Self::holding(app, kind, address, doc, module)?;
        record.load()?;
        Ok(Opened {
            record,
            created: false,
        })
    }

    /// The record `app`'s module keeps at `address` from where it is kept, in `workspace`: the
    /// same place and key a record the app's own process kept was written to, so a record the
    /// person already has opens unchanged. A record the store holds none of is kept there as
    /// the model's genesis.
    ///
    /// # Errors
    /// As [`Self::open`], and the store's own refusal.
    #[cfg(feature = "store")]
    pub fn open_kept(
        app: &str,
        kind: &str,
        address: &str,
        workspace: &str,
        record: &str,
        module: Rc<Module>,
    ) -> Result<Opened, RecordError> {
        let mut kept =
            crate::store::Kept::open(app, workspace, record).map_err(RecordError::Store)?;
        let doc = kept.read().map_err(RecordError::Store)?.unwrap_or_default();
        let mut record = Record {
            kept: Some(kept),
            ..Self::holding(app, kind, address, doc, module)?
        };
        record.load()?;
        record.persist()?;
        Ok(Opened {
            record,
            created: false,
        })
    }

    /// The record, not yet opened: the module is asked by [`Self::load`].
    fn holding(
        app: &str,
        kind: &str,
        address: &str,
        doc: tilo_resource::Doc,
        module: Rc<Module>,
    ) -> Result<Self, RecordError> {
        if !module.serves(kind) {
            return Err(RecordError::NoModel(kind.to_owned()));
        }
        Ok(Self {
            kind: kind.to_owned(),
            address: address.to_owned(),
            app: app.to_owned(),
            module,
            doc,
            published: std::cell::RefCell::new(None),
            #[cfg(feature = "store")]
            kept: None,
            moved: true,
        })
    }

    /// Ask the module to open the record, and adopt its document.
    fn load(&mut self) -> Result<(), RecordError> {
        let answer = self.ask(&self.request(Op::Open, None))?;
        let reading = answer.reading.clone();
        self.adopt(&answer, None)?;
        self.offers_of(answer)?;
        // The reading the module answered says what the open made, which a caller that wants
        // it reads through `reading`; nothing else is done with it here.
        let _ = reading;
        Ok(())
    }

    /// The kind the record holds.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The app whose module runs it.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The resource address readings and actions name it at.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The document, for a host that keeps or syncs it. What a view is offered never reaches
    /// it: a view sees [`Self::offers`] and [`Self::read`].
    #[must_use]
    pub fn document(&self) -> &tilo_resource::Doc {
        &self.doc
    }

    /// Take in what another replica of the same record holds. The change is whoever made it:
    /// it is merged, not re-signed.
    pub fn merge(&mut self, other: &mut tilo_resource::Doc) -> Result<bool, RecordError> {
        use tilo_automerge::ApplyIncoming as _;
        let before = self.doc.get_heads();
        self.doc
            .merge_incoming(other)
            .map_err(|error| RecordError::Document(error.to_string()))?;
        let changed = self.doc.get_heads() != before;
        if changed {
            self.doc.commit();
            self.changed();
        }
        Ok(changed)
    }

    /// What the record offers now: the tree the module answered, rebuilt as this host publishes
    /// it, with every verb addressing [`Self::address`]. Recomputed only when the document
    /// moved.
    pub fn offers(&self) -> Result<ResourceSpec, RecordError> {
        let heads = self.doc.committed_document().get_heads();
        if let Some((at, spec)) = &*self.published.borrow()
            && *at == heads
        {
            return Ok(spec.clone());
        }
        let answer = self.ask(&self.request(Op::Open, None))?;
        let spec = self.offers_of(answer)?;
        Ok(spec)
    }

    /// Build the tree the module answered into what this host publishes, and keep it as the
    /// tree this record offers at these heads.
    fn offers_of(&self, answer: Answer) -> Result<ResourceSpec, RecordError> {
        let node = answer
            .offers
            .ok_or_else(|| RecordError::Module("the module answered no tree".to_owned()))?;
        let spec = tree(&node, &self.address, &self.app);
        let heads = self.doc.committed_document().get_heads();
        *self.published.borrow_mut() = Some((heads, spec.clone()));
        Ok(spec)
    }

    /// The record's own reading: what it holds, as typed data, and the ids of the parts it
    /// collects, each of which a view reads as its own item.
    pub fn reading(&self) -> Result<Json, RecordError> {
        self.reading_of(None)
    }

    /// The reading of the record, or of the item `item` names. `None` when `resource` is no
    /// part of this record — a view is offered readings, never a document, and a part it was
    /// not offered answers nothing.
    pub fn read(&self, resource: &str) -> Option<Result<Json, RecordError>> {
        if resource == self.address {
            return Some(self.reading());
        }
        Some(self.reading_of(Some(resource)))
    }

    /// The reading of `item` of this record, or the record itself.
    fn reading_of(&self, item: Option<&str>) -> Result<Json, RecordError> {
        let answer = self.ask(&self.request(Op::Read, item))?;
        answer
            .reading
            .ok_or_else(|| RecordError::Module("the module answered no reading".to_owned()))
    }

    /// A request about the record, or about the item `item` of it.
    fn request(&self, op: Op, item: Option<&str>) -> Request {
        Request {
            op,
            kind: self.kind.clone(),
            address: item.unwrap_or(&self.address).to_owned(),
            document: Some(self.saved()),
            action: None,
            item: item.map(str::to_owned),
            record: item.is_none(),
            invoker: None,
            on_behalf_of: None,
            // Every request carries the host's time, a reading as much as an invocation: the
            // module has no clock of its own, and one it is handed is always the host's.
            now_ms: tilo_resource::origin_now_ms(),
        }
    }

    /// Admit `action` of `resource` with `arguments` as `invoker`'s step: the model's own verb,
    /// run in its module, and the change it made kept — checked here first, so the module
    /// cannot keep a change this host did not ask for.
    ///
    /// The app's process neither runs the verb nor signs the change: it submits the action,
    /// and this host runs the model and commits it.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
        invoker: &Invoker,
    ) -> Result<ActionOutput, RecordError> {
        let request = Request {
            op: Op::Invoke,
            kind: self.kind.clone(),
            address: resource.to_owned(),
            document: Some(self.saved()),
            action: Some(Asked {
                action: action.to_owned(),
                arguments: arguments.clone(),
            }),
            item: None,
            record: resource == self.address,
            invoker: Some(invoker.invoker.actor.clone()),
            on_behalf_of: invoker.invoker.on_behalf_of.clone(),
            // The one clock: the host reads it here and the module reads it from the request.
            now_ms: tilo_resource::origin_now_ms(),
        };
        let answer = self.ask(&request)?;
        self.adopt(&answer, Some(invoker))?;
        self.offers_of(answer)?;
        // What it made is published before it is answered, so a view's next reading names it.
        *self.published.borrow_mut() = None;
        Ok(ActionOutput {
            value: None,
            resources: Vec::new(),
        })
    }

    /// Ask the module `request`, as JSON: a refusal is the model's own words.
    fn ask(&self, request: &Request) -> Result<Answer, RecordError> {
        self.module
            .run(&self.kind, request)
            .map_err(|error| RecordError::Module(error.to_string()))
    }

    /// Adopt the document a call answered, when it is the one change the host asked for.
    ///
    /// A module's document is a whole record of its own, so the host compares it with what it
    /// held: everything the host held must still be its history, and at most one change may be
    /// on top of it. For an invocation, that change must be the invoker's, committed as the
    /// writer the session chose. A module that made other changes, or a change someone else's,
    /// is refused here, and the host's document is left as it was.
    fn adopt(&mut self, answer: &Answer, invoker: Option<&Invoker>) -> Result<(), RecordError> {
        let Some(document) = &answer.document else {
            return Ok(());
        };
        let mut made = tilo_resource::Doc::load(&base64_decode(document)?)
            .map_err(|error| RecordError::Document(error.to_string()))?;
        let held = self.doc.get_heads();
        // The changes the module made that the host did not hold: at most one, and only a
        // genesis when the record held nothing.
        let added = made.get_changes(&held);
        match added.as_slice() {
            // Nothing changed: a no-op verb, or a reading. The host's document stands.
            [] => return Ok(()),
            // A record that held nothing: its genesis is the model's, and the host adopts it.
            [genesis] if held.is_empty() => {
                if genesis.deps().iter().any(|dep| !held.contains(dep)) {
                    return Err(RecordError::Module(
                        "the model's genesis builds on a record this host does not hold".into(),
                    ));
                }
                self.doc = made;
                self.doc.commit();
                self.changed();
                self.persist()?;
                return Ok(());
            }
            [change] => {
                if let Some(invoker) = invoker {
                    let expected = invoker.writer.as_deref();
                    if let Some(expected) = expected
                        && change.actor_id().to_bytes() != expected
                    {
                        return Err(RecordError::Module(format!(
                            "the model committed its change as {}, not as the invoker",
                            change.actor_id().to_hex_string()
                        )));
                    }
                }
                if change.deps().iter().any(|dep| !held.contains(dep)) {
                    return Err(RecordError::Module(
                        "the model's change builds on a record this host does not hold".into(),
                    ));
                }
            }
            many => {
                return Err(RecordError::Module(format!(
                    "the model made {} changes, not the one it was asked for",
                    many.len()
                )));
            }
        }
        self.doc = made;
        self.doc.commit();
        self.changed();
        self.persist()?;
        Ok(())
    }

    /// The document as base64, for a request.
    fn saved(&self) -> String {
        let mut doc = self.doc.clone();
        base64_encode(&doc.save())
    }

    /// Whether anything changed since a reader last asked, and clear it.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }

    /// Note that the record moved: a view following it reads again, and the tree it offers is
    /// published again.
    fn changed(&mut self) {
        self.moved = true;
        *self.published.borrow_mut() = None;
    }

    /// Keep the document where it is kept, when it is on disk.
    #[cfg(feature = "store")]
    fn persist(&mut self) -> Result<(), RecordError> {
        let Some(kept) = &mut self.kept else {
            return Ok(());
        };
        let mut doc = self.doc.clone();
        kept.write(&mut doc).map_err(RecordError::Store)
    }

    /// Nothing to keep: a record in memory.
    #[cfg(not(feature = "store"))]
    #[allow(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "a record on disk answers as one in memory does, whether or not there is anything to keep"
    )]
    fn persist(&self) -> Result<(), RecordError> {
        Ok(())
    }
}

/// `node` as the tree this host publishes, under `address`: every verb addresses the resource
/// it is invoked on, so a module cannot offer a caller an address the host does not publish.
fn tree(node: &ResourceNode, address: &str, app: &str) -> ResourceSpec {
    let id = if node.id.is_empty() {
        address
    } else {
        node.id.as_str()
    };
    let mut spec = ResourceSpec::new(id, node.kind.as_str(), node.label.as_str());
    spec.app_id = Some(app.to_owned());
    if let Some(kind) = &node.resource_type {
        spec.resource_type = Some(tilo_actions::ResourceType {
            noun: tilo_actions::Noun::new(kind.noun.one.clone(), kind.noun.many.clone()),
            app: tilo_actions::Noun::new(kind.app.one.clone(), kind.app.many.clone()),
            root: kind.root,
        });
    }
    for action in &node.actions {
        spec = spec.with_action(action_spec(action, id));
    }
    for child in &node.children {
        spec = spec.with_child(tree(child, child.id.as_str(), app));
    }
    spec
}

/// The action `node` is, bound to the resource `id`: one that binds the arguments the host
/// passes it, and hands them to the module that runs the verb.
fn action_spec(node: &ActionNode, id: &str) -> tilo_actions::ActionSpec {
    let resource = id.to_owned();
    let action = node.id.clone();
    let params = node.params.iter().map(param).collect::<Vec<_>>();
    let mut spec = tilo_actions::ActionSpec::typed_on(
        node.id.clone(),
        node.label.clone(),
        node.description.clone().unwrap_or_default(),
        ExecutionLocus::ClientLocal,
        params,
        move |_target: &Target, args: &ActionArgs| {
            // What this host runs is the verb's name on its resource: the arguments travel as
            // the module reads them, and the module runs the model's own verb.
            let carried = serde_json::json!({
                "resource": resource,
                "action": action,
                "arguments": args_json(args),
            });
            EncodedAction {
                type_id: tilo_model_abi::VERB_TYPE.to_owned(),
                payload: serde_json::to_vec(&carried).unwrap_or_default(),
            }
        },
    );
    if !node.keywords.is_empty() {
        spec = spec.with_keywords(node.keywords.clone());
    }
    if let Some(icon) = &node.icon {
        spec = spec.with_icon(icon.clone());
    }
    if let Some(description) = &node.description {
        spec = spec.with_description(description.clone());
    }
    if let Some(why) = &node.unavailable {
        spec.type_mut().contract.availability = tilo_actions::Availability::Unavailable {
            code: "model".to_owned(),
            reason: why.clone(),
        };
    }
    spec
}

/// The arguments `args` bound, as JSON: what the module reads them as.
fn args_json(args: &ActionArgs) -> Map<String, Json> {
    args.to_json()
}

/// One parameter of an action, as the host offers it.
fn param(node: &ParamNode) -> tilo_actions::ActionParam {
    let ty = match node.kind.as_str() {
        "integer" => ParamType::Integer,
        "number" => ParamType::Number,
        "bool" | "boolean" => ParamType::Boolean,
        "object" => ParamType::Object,
        "array" => ParamType::Array,
        "id" | "resource" => ParamType::Resource,
        "datetime" | "date" => ParamType::DateTime,
        "secret" => ParamType::Secret,
        _ => ParamType::Text,
    };
    let description = node.description.clone().unwrap_or_default();
    if node.optional {
        tilo_actions::ActionParam::optional_owned(node.name.clone(), ty, description)
    } else {
        tilo_actions::ActionParam::required_owned(node.name.clone(), ty, description)
    }
}

/// `bytes` as base64, for JSON.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `text`'s bytes, from the base64 JSON carries.
fn base64_decode(text: &str) -> Result<Vec<u8>, RecordError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| RecordError::Document(format!("the document is not base64: {error}")))
}
