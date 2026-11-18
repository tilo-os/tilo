//! The device's side of an app's model: one WebAssembly module per app
//! version, from the app's package, executed without building the app.
//!
//! A model module (`model.wasm` in a `.tapp`'s payload) imports nothing but
//! the calls [`ABI`] names and exports [`EXPORT`]. This crate compiles the
//! bytes on the engine the rest of the system executes WebAssembly on
//! ([`tilolang_runtime::engine`]).
//!
//! The action framework is this side's, executed natively once per device rather
//! than compiled into every app's module. [`Model::apply`] binds an
//! invocation's JSON arguments against the verb the package's signed surface
//! declares, has the module read the typed action (its recorded bytes and the
//! facts history files it under, [`tilo_model_guest::Bound`]), builds the
//! envelope (its id from the request, the base heads, the invoker's actor and
//! the origin time), writes the history entry and commits the change through
//! the action executor, exactly as an action of an app compiled in would be.
//! [`Model::replay`] reproduces an invocation another holder recorded, at the
//! heads it was made on, through the same executor. The module is asked only
//! to read the action and apply its verb, given in a compact typed form
//! ([`tilo_model_guest::wire`]), not JSON.
//!
//! The device holds the document. The module reads and writes it through the
//! device's document calls (`automerge::model_device`): every write lands in
//! this side's Automerge document, under the actor and in the change the
//! executor pins, so the change is made the same way on every holder.
//!
//! The engine links only [`ABI`]: a module importing anything else fails to
//! link, which is how "no imports beyond the device's" is enforced rather
//! than checked.
//!
//! Compiled modules are kept by the engine's process-wide cache, so the second
//! action of an installed app costs a call, not a compile.

#![deny(rust_2018_idioms)]

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use automerge::model_device::Documents;
use automerge::transaction::CommitOptions;
use tilo_actions::surface::VerbSurface;
use tilo_actions::{
    Acting, ActionArgs, ActionContext, ActionEnvelope, ActionError, ActionId, ActionInvoker,
    ActionParam, ChangeKind, EncodedAction, FrameworkChange, MemberId,
};
use tilo_model_guest::{Bound, Op, Refused, wire};
use tilolang_runtime::engine::{Engine, EngineLimits, GuestAnswer, Host, HostFail, Imports, Wasm};
use tilolang_runtime::ids::ModuleHash;

/// The functions a model module imports, as `(module, name, parameters, result)`:
/// its request and answer, and the calls on the documents the device keeps
/// for it.
pub const ABI: &[Imports] = &[
    ("tilo_model", "request_length", "", "i"),
    ("tilo_model", "request", "ii", "i"),
    ("tilo_model", "answer", "ii", ""),
    ("tilo_model", "document", "ii", "i"),
    ("tilo_model", "document_answer", "i", ""),
];

/// The export a model module is called through.
pub const EXPORT: &str = tilo_model_guest::EXPORT;

/// Why a model could not be loaded, asked or understood.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("the model module: {0}")]
    Load(String),
    #[error("the model: {0}")]
    Call(String),
    #[error("the model refused: {0}")]
    Refused(String),
    #[error("the model answered nothing")]
    Silent,
    #[error("the model's answer: {0}")]
    Answer(String),
    #[error("the document: {0}")]
    Document(String),
    #[error("the arguments: {0}")]
    Arguments(String),
}

impl From<ActionError> for ModelError {
    fn from(error: ActionError) -> Self {
        Self::Refused(error.to_string())
    }
}

/// One app version's model, compiled and ready to apply its actions.
pub struct Model {
    engine: Engine,
    module: ModuleHash,
}

/// One invocation of a verb, as the action API admitted it.
#[derive(Debug)]
pub struct Invocation<'a> {
    /// The app the package is: what history and the receipt name the change
    /// by.
    pub app: &'a str,
    /// The resource kind the verb is of: an app's module serves every
    /// resource the app declares, and routes the request by this.
    pub kind: &'a str,
    /// The verb, as the package's signed surface declares it.
    pub verb: &'a VerbSurface,
    /// The resource's document id: the empty string for what an app keeps per
    /// workspace, or a named document's id.
    pub id: &'a str,
    /// The bytes of the document the action acts on.
    pub document: &'a [u8],
    /// The invoking member: the person, agent or bot the change is authored
    /// by.
    pub member: &'a MemberId,
    /// Who acts, and for whom, as the action API admitted it.
    pub acting: &'a Acting,
    /// The arguments, as the caller gave them.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The origin clock, milliseconds since the Unix epoch.
    pub now: i64,
}

/// What applying an invocation made.
#[derive(Debug)]
pub struct Applied {
    /// The change the action added, against the document it was given.
    pub changes: Vec<u8>,
    /// The invocation as the action API recorded it: what a peer replays.
    pub envelope: ActionEnvelope,
}

impl Model {
    /// Compile `bytes`, an app's `model.wasm`.
    ///
    /// # Errors
    /// Returns [`ModelError::Load`] when the engine refuses the module, which
    /// is what an import the device does not answer looks like.
    pub fn compile(bytes: &[u8]) -> Result<Self, ModelError> {
        let engine = Engine::new(EngineLimits::default()).map_err(ModelError::Load)?;
        engine.link(ABI).map_err(ModelError::Load)?;
        let module = engine
            .compile(bytes)
            .map_err(|error| ModelError::Load(error.to_string()))?;
        Ok(Self { engine, module })
    }

    /// The genesis of the resource `kind` with the document id `id` (`""` for
    /// what an app keeps per workspace) minted in the workspace `home`: the
    /// app's initial content in one change at time zero, under the actor
    /// every device derives from the resource's identity
    /// ([`tilo_resource::genesis::actor`]), so every holder of it writes the
    /// same change. Answers the document's bytes.
    ///
    /// # Errors
    /// See [`ModelError`].
    pub fn initialize(&self, kind: &str, id: &str, home: &str) -> Result<Vec<u8>, ModelError> {
        let mut document = AutoCommit::new();
        document.set_actor(tilo_resource::genesis::actor(&document_id(id), kind, home));
        let (mut document, answer) = self.call(request(Op::Initialize, kind), document);
        answer?;
        document.commit_with(
            CommitOptions::default()
                .with_message(FrameworkChange::Genesis.change_message())
                .with_time(0),
        );
        Ok(document.save())
    }

    /// Apply `invocation`: bind its arguments against its verb, have the
    /// module read the action, and apply it under an envelope whose id, base
    /// heads, actor and time all come from the request, so two holders given
    /// the same request write the same change.
    ///
    /// # Errors
    /// Arguments the verb does not take, an action the module refuses, and a
    /// document that does not load.
    pub fn apply(&self, invocation: Invocation<'_>) -> Result<Applied, ModelError> {
        let Invocation {
            app,
            kind,
            verb,
            id,
            document,
            member,
            acting,
            arguments,
            now,
        } = invocation;
        let arguments = bind(verb, arguments)?;
        let typed = serde_json::json!({
            "op": { "action": verb.id, "args": arguments },
            "at": now,
        });
        let mut asked = request(Op::Bind, kind);
        wire::encode(&typed, &mut asked);
        let (_, answer) = self.call(asked, AutoCommit::new());
        let bound: Bound = serde_json::from_slice(&answer?)
            .map_err(|error| ModelError::Answer(error.to_string()))?;
        let action = Guest::recorded(self, kind, bound.payload.as_bytes())?.with_facts(&bound);
        let encoded = EncodedAction {
            type_id: bound.action_type,
            payload: bound.payload.into_bytes(),
        };
        let mut document =
            AutoCommit::load(document).map_err(|error| ModelError::Document(error.to_string()))?;
        let heads = document.get_heads();
        let context = ActionContext::for_resource(kind, kind, &bound.label)
            .with_app(app, &bound.label)
            .with_invoker(ActionInvoker::admitted(acting))
            .with_change(bound.change);
        let pinned = (
            invocation_id(&document_id(id), member, &encoded, &heads, now),
            now / 1_000,
        );
        let mut envelope = tilo_actions::envelope_at(
            &mut document,
            &action,
            encoded,
            (&member.actor_bytes(), context),
            pinned,
        )?;
        // The module computes no undo: an action whose own code does writes
        // none, rather than an entry a device with the app compiled in would
        // find its undo missing from.
        if bound.compensates
            && let Some(context) = envelope.context.as_mut()
        {
            context.action = None;
        }
        tilo_actions::replay_decoded(&mut document, &envelope, &action, None)?;
        Ok(Applied {
            changes: document.save_after(&heads),
            envelope,
        })
    }

    /// Replay `envelope`, an invocation another holder recorded, against the
    /// resource `kind` whose document is `document`: at the envelope's base
    /// heads it makes the change the holder made, which is how a receiver
    /// checks it. Answers the changes it added.
    ///
    /// # Errors
    /// An action the module does not read or refuses, and a document that
    /// does not load or lacks the envelope's base.
    pub fn replay(
        &self,
        kind: &str,
        document: &[u8],
        envelope: &ActionEnvelope,
    ) -> Result<Vec<u8>, ModelError> {
        let action = Guest::recorded(self, kind, &envelope.action.payload)?;
        let mut document =
            AutoCommit::load(document).map_err(|error| ModelError::Document(error.to_string()))?;
        let heads = document.get_heads();
        tilo_actions::replay_decoded(&mut document, envelope, &action, None)?;
        Ok(document.save_after(&heads))
    }

    /// Call the module with `request` about `document`, answering the
    /// document as the module left it and what it answered.
    fn call(
        &self,
        request: Vec<u8>,
        document: AutoCommit,
    ) -> (AutoCommit, Result<Vec<u8>, ModelError>) {
        let serving = Rc::new(Serving {
            request,
            answer: RefCell::new(None),
            documents: RefCell::new(Documents::new(document)),
            document_answer: RefCell::new(Vec::new()),
        });
        let outcome = self.engine.call(&self.module, EXPORT, &[], serving.clone());
        let document = serving
            .documents
            .replace(Documents::new(AutoCommit::new()))
            .into_document();
        let answer = serving.answer.borrow_mut().take();
        let refused = |bytes: &[u8]| serde_json::from_slice::<Refused>(bytes).ok();
        let answer = match (outcome, answer) {
            // A module that panicked wrote why before it trapped.
            (Err(error), answer) => Err(answer.as_deref().and_then(refused).map_or_else(
                || ModelError::Call(error.to_string()),
                |said| ModelError::Refused(said.error),
            )),
            (Ok(_), None) => Err(ModelError::Silent),
            (Ok(_), Some(bytes)) => match refused(&bytes) {
                Some(said) => Err(ModelError::Refused(said.error)),
                None => Ok(bytes),
            },
        };
        (document, answer)
    }
}

/// A request's head: the op and the resource kind it is for.
fn request(op: Op, kind: &str) -> Vec<u8> {
    let mut bytes = vec![op as u8];
    wire::text(kind, &mut bytes);
    bytes
}

/// `arguments` bound against the parameters `verb` declares, as the action
/// API binds an installed app's: an argument it does not take, or one of the
/// wrong type, is refused by name. A parameter published as a field of
/// another is nested back under it.
fn bind(
    verb: &VerbSurface,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, ModelError> {
    let arguments = tilo_actions::surface::nest_arguments(verb, arguments.into_iter().collect());
    let mut params: Vec<ActionParam> = Vec::with_capacity(verb.params.len());
    for param in &verb.params {
        let (name, ty, schema) = match &param.within {
            Some(outer) => (outer.clone(), tilo_actions::ParamType::Object, None),
            None => (
                param.name.clone(),
                tilo_actions::params::param_type_of(&param.schema),
                Some(param.schema.clone()).filter(|schema| !schema.is_null()),
            ),
        };
        if let Some(known) = params.iter_mut().find(|known| known.name == name) {
            known.required |= param.required;
            continue;
        }
        let declared = if param.required {
            ActionParam::required_owned(name.clone(), ty, name)
        } else {
            ActionParam::optional_owned(name.clone(), ty, name)
        };
        params.push(match schema {
            Some(schema) => declared.with_schema(schema),
            None => declared,
        });
    }
    ActionArgs::bind_json(&params, &arguments)
        .map(|args| args.to_json())
        .map_err(|error| ModelError::Arguments(error.to_string()))
}

/// The document id as the module reads it: the empty route is what an app
/// keeps per workspace, whose ownership key is the package primary's.
#[must_use]
pub fn document_id(id: &str) -> String {
    if id.is_empty() {
        "@tilo:package-primary".to_owned()
    } else {
        id.to_owned()
    }
}

/// The invocation's id: a function of what the request asks and the resource
/// it asks it of, so every holder making the same request mints the same one.
fn invocation_id(
    id: &str,
    member: &MemberId,
    encoded: &EncodedAction,
    heads: &[automerge::ChangeHash],
    now: i64,
) -> ActionId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.model.invocation\0");
    hasher.update(id.as_bytes());
    hasher.update(b"\0");
    hasher.update(&member.actor_bytes());
    hasher.update(&now.to_le_bytes());
    hasher.update(&encoded.payload);
    for head in heads {
        hasher.update(&head.0);
    }
    let digest = hasher.finalize();
    let short = digest
        .as_bytes()
        .first_chunk::<16>()
        .copied()
        .unwrap_or_default();
    ActionId(uuid::Uuid::from_bytes(short))
}

/// A verb of an app's model as the executor applies it: the module applies
/// it, and the facts the module read from it are what the envelope keeps.
#[derive(Debug)]
struct Guest<'a> {
    model: &'a Model,
    kind: &'a str,
    /// The action in the compact form the module reads.
    action: Vec<u8>,
    change: ChangeKind,
    action_kind: tilo_actions::ActionKind,
    incoming_message: bool,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model").finish_non_exhaustive()
    }
}

impl<'a> Guest<'a> {
    /// The action whose recorded JSON is `payload`.
    fn recorded(model: &'a Model, kind: &'a str, payload: &[u8]) -> Result<Self, ModelError> {
        let value: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|error| ModelError::Refused(format!("the recorded action: {error}")))?;
        let mut action = Vec::with_capacity(payload.len());
        wire::encode(&value, &mut action);
        Ok(Self {
            model,
            kind,
            action,
            change: ChangeKind::default(),
            action_kind: tilo_actions::ActionKind::Undoable,
            incoming_message: false,
        })
    }

    /// With the facts the module read from the action.
    fn with_facts(mut self, bound: &Bound) -> Self {
        self.change = bound.change;
        self.action_kind = bound.kind;
        self.incoming_message = bound.incoming_message;
        self
    }
}

impl tilo_actions::Action for Guest<'_> {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        let context = tilo_actions::invoking_context();
        let mut asked = request(Op::Apply, self.kind);
        asked.extend_from_slice(&self.action);
        let invoker = context
            .as_ref()
            .and_then(|context| context.invoker.as_ref());
        let invocation = context
            .as_ref()
            .and_then(|context| context.source_invocation);
        for value in [
            serde_json::to_value(invoker),
            serde_json::to_value(invocation),
        ] {
            wire::encode(&value.map_err(ActionError::Encode)?, &mut asked);
        }
        wire::encode(
            &serde_json::Value::Bool(tilo_actions::executor::is_replaying()),
            &mut asked,
        );
        let document = std::mem::replace(doc, AutoCommit::new());
        let (document, answer) = self.model.call(asked, document);
        *doc = document;
        answer
            .map(drop)
            .map_err(|error| ActionError::Refused(error.to_string()))
    }

    fn change_kind(&self) -> ChangeKind {
        self.change
    }

    fn action_kind(&self) -> tilo_actions::ActionKind {
        self.action_kind
    }

    fn incoming_message(&self) -> bool {
        self.incoming_message
    }
}

/// This side of one call: the request the module reads, the answer it
/// writes, and the documents it reads and writes through the device.
struct Serving {
    request: Vec<u8>,
    answer: RefCell<Option<Vec<u8>>>,
    documents: RefCell<Documents>,
    /// The answer to the module's last document call, until it takes it.
    document_answer: RefCell<Vec<u8>>,
}

/// The `length` bytes of the module's memory at `offset`.
fn guest_bytes(memory: &[u8], offset: i32, length: i32) -> Result<&[u8], HostFail> {
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    start
        .checked_add(usize::try_from(length).unwrap_or(usize::MAX))
        .and_then(|end| memory.get(start..end))
        .ok_or_else(|| HostFail::Trap("the model's call lies outside its memory".into()))
}

impl Host for Serving {
    fn import(&self, id: usize, _: &[u8], _: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap(format!("the model called import {id}")))
    }

    fn guest_abi(&self) -> &'static [Imports] {
        ABI
    }

    fn guest_import(
        &self,
        module: &str,
        name: &str,
        memory: &[u8],
        args: &[Wasm],
    ) -> Result<GuestAnswer, HostFail> {
        if module != tilo_model_guest::IMPORT_MODULE {
            return Err(HostFail::Trap(format!(
                "the model imports {module}::{name}"
            )));
        }
        match (name, args) {
            ("request_length", []) => Ok(GuestAnswer::Value(Wasm::I32(
                i32::try_from(self.request.len()).unwrap_or(i32::MAX),
            ))),
            ("document", [Wasm::I32(offset), Wasm::I32(length)]) => {
                let call = guest_bytes(memory, *offset, *length)?;
                let answer = self.documents.borrow_mut().answer(call);
                let length = i32::try_from(answer.len())
                    .map_err(|_| HostFail::Trap("the document's answer is too large".into()))?;
                *self.document_answer.borrow_mut() = answer;
                Ok(GuestAnswer::Value(Wasm::I32(length)))
            }
            ("document_answer", [Wasm::I32(offset)]) => Ok(GuestAnswer::Bytes {
                offset: u32::try_from(*offset).unwrap_or(u32::MAX),
                bytes: std::mem::take(&mut *self.document_answer.borrow_mut()),
                result: None,
            }),
            ("request", [Wasm::I32(offset), Wasm::I32(capacity)]) => {
                let capacity = usize::try_from(*capacity).unwrap_or(0);
                let count = capacity.min(self.request.len());
                let bytes = self.request.get(..count).unwrap_or_default().to_vec();
                Ok(GuestAnswer::Bytes {
                    offset: u32::try_from(*offset).unwrap_or(u32::MAX),
                    bytes,
                    result: Some(Wasm::I32(i32::try_from(count).unwrap_or(i32::MAX))),
                })
            }
            ("answer", [Wasm::I32(offset), Wasm::I32(length)]) => {
                let answer = guest_bytes(memory, *offset, *length)?.to_vec();
                *self.answer.borrow_mut() = Some(answer);
                Ok(GuestAnswer::Nothing)
            }
            _ => Err(HostFail::Trap(format!(
                "the model called {module}::{name} wrongly"
            ))),
        }
    }
}
