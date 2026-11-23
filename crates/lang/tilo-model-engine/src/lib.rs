//! The device's side of an app's model: one WebAssembly module per app
//! version, from the app's package, executed without building the app.
//!
//! A model module (`model.wasm` in a `.tapp`'s payload) imports nothing but
//! the calls [`ABI`] names and exports [`EXPORT`]. This crate compiles the
//! bytes on the engine the rest of the system executes WebAssembly on
//! ([`tilolang_runtime::engine`]).
//!
//! The action framework is this side's, executed natively once per device
//! rather than inside every app's module. [`Model::apply`] checks an
//! invocation's arguments against the verb the package's signed surface
//! declares, has the module build the typed action from them through the
//! app's own declarations ([`tilo_model_guest::Bound`]: its recorded bytes
//! and what history files it under), builds the envelope (its id from the
//! request, the base heads, the invoker's actor and the origin time), and
//! writes the history entry and commits the change through the action
//! executor, exactly as an action of an app compiled in is. [`Model::replay`]
//! reproduces an invocation another holder recorded, at the heads it was made
//! on, through the same executor. [`Model::initialize`] commits a new
//! resource's genesis from the content the module writes.
//!
//! The device holds the document. The module reads and writes it through the
//! device's document calls (`automerge::model_device`): every write lands in
//! this side's Automerge document, in the change the executor pins. The module
//! cannot write history of its own: on the request's document the device refuses a
//! commit, an empty change, an actor and changes loaded or merged in, so the
//! only history a module's verb makes is the entry the device writes for it.
//!
//! The engine links only [`ABI`]: a module importing anything else fails to
//! link, which is how "no imports beyond the device's" is enforced rather
//! than checked.
//!
//! Compiled modules are kept by the engine's process-wide cache, so the second
//! action of an installed app costs a call, not a compile.
//!
//! # Split modules
//!
//! An app built in this repository carries only its own code: every app's
//! model is linked into one module and split (tools/ci/app-models.sh,
//! `wasm-split --models`) into the model runtime the apps share (automerge,
//! std, the resource layer, `serde_json`), which the shared runtime bundle
//! carries once, and one module per app. Such a module names the runtime it
//! was split against in its [`RUNTIME_SECTION`], and the export to call. It
//! is linked after that runtime ([`Engine::compile_linked`]): the runtime's
//! start module with [`ABI`], its shared modules, then the app's module, each
//! import answered by the exports of the modules before it. The runtime is
//! found by its hash ([`Runtime::find`]); a module whose runtime is not here
//! is refused, naming the runtime it needs and the ones that are. A module
//! with no such section (an app built alone, `tilolang model`) is whole and
//! is called as it is.

#![deny(rust_2018_idioms)]

mod runtime;

use std::cell::RefCell;
use std::rc::Rc;

pub use runtime::{RUNTIME_DIR, RUNTIME_SECTION, RUNTIME_VARIABLE, Runtime, runtime_dirs};

use automerge::AutoCommit;
use automerge::model_device::Documents;
use automerge::transaction::CommitOptions;
use tilo_actions::surface::VerbSurface;
use tilo_actions::{
    Acting, ActionArgs, ActionContext, ActionEnvelope, ActionError, ActionId, ActionInvoker,
    ActionParam, EncodedAction, FrameworkChange, MemberId,
};
use tilo_model_guest::{Answer as Said, Bound, Op, Request};
use tilolang_runtime::engine::{
    DEFAULT_LIMITS, Engine, GuestAnswer, Host, HostFail, Imports, Wasm,
};
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
    #[error("the model runtime: {0}")]
    Runtime(String),
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
    /// What the module is called through: [`EXPORT`], or a split module's
    /// entry in the runtime's start module.
    export: String,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("export", &self.export)
            .finish_non_exhaustive()
    }
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
    /// The change the action added, against the document it was given:
    /// empty when the verb changed nothing.
    pub changes: Vec<u8>,
    /// The invocation as the action API recorded it: what a peer replays.
    pub envelope: ActionEnvelope,
}

impl Model {
    /// Compile `bytes`, an app's `model.wasm`: a whole module, or a split one
    /// linked after the runtime it names, found by [`Runtime::find`].
    ///
    /// # Errors
    /// Returns [`ModelError::Load`] when the engine refuses the module, which
    /// is what an import the device does not answer looks like, and
    /// [`ModelError::Runtime`] when the runtime a split module names is not
    /// here.
    pub fn compile(bytes: &[u8]) -> Result<Self, ModelError> {
        match runtime::named(bytes)? {
            None => {
                let engine = Self::engine()?;
                let module = engine
                    .compile(bytes)
                    .map_err(|error| ModelError::Load(error.to_string()))?;
                Ok(Self {
                    engine,
                    module,
                    export: EXPORT.to_owned(),
                })
            }
            Some(named) => {
                let runtime = Runtime::find(&named.runtime)?;
                Self::link(bytes, &runtime, named.entry)
            }
        }
    }

    /// Compile `bytes`, a split app module, linked after `runtime`.
    ///
    /// # Errors
    /// As [`Self::compile`]; a module that is not split, or was split against
    /// another runtime, is [`ModelError::Runtime`], naming both.
    pub fn compile_with(bytes: &[u8], runtime: &Runtime) -> Result<Self, ModelError> {
        let named = runtime::named(bytes)?.ok_or_else(|| {
            ModelError::Runtime(format!(
                "the module is whole: it links against no runtime, not {}",
                runtime.hash()
            ))
        })?;
        if named.runtime != runtime.hash() {
            return Err(ModelError::Runtime(format!(
                "the module was split against model runtime {}, not {} ({})",
                named.runtime,
                runtime.hash(),
                runtime.dir().display()
            )));
        }
        Self::link(bytes, runtime, named.entry)
    }

    fn link(bytes: &[u8], runtime: &Runtime, export: String) -> Result<Self, ModelError> {
        let engine = Self::engine()?;
        let mut modules: Vec<&[u8]> = runtime.modules().collect();
        modules.push(bytes);
        let module = engine
            .compile_linked(&modules)
            .map_err(|error| ModelError::Load(error.to_string()))?;
        Ok(Self {
            engine,
            module,
            export,
        })
    }

    fn engine() -> Result<Engine, ModelError> {
        let engine = Engine::new(DEFAULT_LIMITS).map_err(ModelError::Load)?;
        engine.link(ABI).map_err(ModelError::Load)?;
        Ok(engine)
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
        let asked = Request {
            op: Some(Op::Initialize),
            kind: kind.to_owned(),
            ..Request::default()
        };
        let (mut document, answer) = self.call(&asked, document);
        answer?;
        document.commit_with(
            CommitOptions::default()
                .with_message(FrameworkChange::Genesis.change_message())
                .with_time(0),
        );
        Ok(document.save())
    }

    /// Apply `invocation`: check its arguments against its verb, have the
    /// module build the action, and apply it under an envelope whose id, base
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
        let document =
            AutoCommit::load(document).map_err(|error| ModelError::Document(error.to_string()))?;
        let invoker = ActionInvoker::admitted(acting);
        let asked = Request {
            op: Some(Op::Bind),
            kind: kind.to_owned(),
            action: verb.id.clone(),
            arguments,
            now,
            context: Some(ActionContext::default().with_invoker(invoker.clone())),
            ..Request::default()
        };
        let (mut document, answer) = self.call(&asked, document);
        let bound = answer?
            .bound
            .ok_or_else(|| ModelError::Answer("the model did not say what the action is".into()))?;
        let action = Guest::bound(self, kind, &bound);
        let encoded = EncodedAction {
            type_id: bound.action_type,
            payload: bound.payload.into_bytes(),
        };
        let heads = document.get_heads();
        let context = ActionContext::for_resource(kind, kind, &bound.label)
            .with_app(app, &bound.label)
            .with_invoker(invoker)
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
        // The module computes no undo: the entry of an action whose own code
        // computes it names no action, rather than one a device with the app
        // compiled in would find its undo missing from.
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
        let payload = String::from_utf8(envelope.action.payload.clone())
            .map_err(|error| ModelError::Refused(format!("the recorded action: {error}")))?;
        let asked = Request {
            op: Some(Op::Read),
            kind: kind.to_owned(),
            payload,
            ..Request::default()
        };
        let (_, answer) = self.call(&asked, AutoCommit::new());
        let bound = answer?
            .bound
            .ok_or_else(|| ModelError::Answer("the model did not say what the action is".into()))?;
        let action = Guest::bound(self, kind, &bound);
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
        request: &Request,
        document: AutoCommit,
    ) -> (AutoCommit, Result<Said, ModelError>) {
        let request = match serde_json::to_vec(request) {
            Ok(bytes) => bytes,
            Err(error) => return (document, Err(ModelError::Answer(error.to_string()))),
        };
        let serving = Rc::new(Serving {
            request,
            answer: RefCell::new(None),
            documents: RefCell::new(Documents::new(document)),
            document_answer: RefCell::new(Vec::new()),
        });
        let outcome = self
            .engine
            .call(&self.module, &self.export, &[], serving.clone());
        let document = serving
            .documents
            .replace(Documents::new(AutoCommit::new()))
            .into_document();
        let answer = serving.answer.borrow_mut().take();
        let said = |bytes: &[u8]| serde_json::from_slice::<Said>(bytes);
        let answer = match (outcome, answer) {
            // A module that panicked wrote why before it trapped.
            (Err(error), answer) => Err(answer
                .as_deref()
                .and_then(|bytes| said(bytes).ok()?.error)
                .map_or_else(|| ModelError::Call(error.to_string()), ModelError::Refused)),
            (Ok(_), None) => Err(ModelError::Silent),
            (Ok(_), Some(bytes)) => match said(&bytes) {
                Ok(Said {
                    error: Some(error), ..
                }) => Err(ModelError::Refused(error)),
                Ok(answer) => Ok(answer),
                Err(error) => Err(ModelError::Answer(format!(
                    "{error}: {}",
                    String::from_utf8_lossy(&bytes)
                ))),
            },
        };
        (document, answer)
    }
}

/// The hash of the model runtime `module` was split against, or `None` for a
/// whole module, which links against nothing.
///
/// # Errors
/// Bytes that are not a WebAssembly module, or whose runtime section does not
/// read.
pub fn linked_runtime(module: &[u8]) -> Result<Option<String>, ModelError> {
    Ok(runtime::named(module)?.map(|named| named.runtime))
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
/// it, and what the module said of it is what the envelope keeps.
#[derive(Debug)]
struct Guest<'a> {
    model: &'a Model,
    kind: &'a str,
    /// The action as it is recorded, which the module reads to apply it.
    payload: String,
    change: tilo_actions::ChangeKind,
    action_kind: tilo_actions::ActionKind,
    incoming_message: bool,
    irreversible: bool,
    locus: tilo_actions::ExecutionLocus,
    described: Option<tilo_actions::Described>,
}

impl<'a> Guest<'a> {
    /// The action the module said `bound` is.
    fn bound(model: &'a Model, kind: &'a str, bound: &Bound) -> Self {
        Self {
            model,
            kind,
            payload: bound.payload.clone(),
            change: bound.change,
            action_kind: bound.kind,
            incoming_message: bound.incoming_message,
            irreversible: bound.irreversible,
            locus: bound.locus,
            described: bound.described.clone(),
        }
    }
}

impl tilo_actions::Action for Guest<'_> {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        let asked = Request {
            op: Some(Op::Apply),
            kind: self.kind.to_owned(),
            payload: self.payload.clone(),
            context: tilo_actions::invoking_context(),
            replaying: tilo_actions::executor::is_replaying(),
            ..Request::default()
        };
        let document = std::mem::replace(doc, AutoCommit::new());
        let (document, answer) = self.model.call(&asked, document);
        *doc = document;
        answer
            .map(drop)
            .map_err(|error| ActionError::Refused(error.to_string()))
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        self.locus
    }

    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.change
    }

    fn action_kind(&self) -> tilo_actions::ActionKind {
        self.action_kind
    }

    fn incoming_message(&self) -> bool {
        self.incoming_message
    }

    fn irreversible(&self) -> bool {
        self.irreversible
    }

    fn describe(&self, _doc: &AutoCommit) -> Option<tilo_actions::Described> {
        self.described.clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A module with no code whose runtime section names `runtime`.
    fn split_against(runtime: &str) -> Vec<u8> {
        let data = format!(r#"{{"runtime":"{runtime}","entry":"tilo_model_apply_x"}}"#);
        let mut contents = vec![u8::try_from(RUNTIME_SECTION.len()).unwrap()];
        contents.extend_from_slice(RUNTIME_SECTION.as_bytes());
        contents.extend_from_slice(data.as_bytes());
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.push(0);
        module.push(u8::try_from(contents.len()).unwrap());
        module.extend_from_slice(&contents);
        module
    }

    #[test]
    fn a_module_split_against_another_runtime_is_refused_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join(RUNTIME_DIR);
        std::fs::create_dir(&runtime_dir).unwrap();
        let main = b"\0asm\x01\0\0\0".to_vec();
        std::fs::write(runtime_dir.join("main.wasm"), &main).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(main.len() as u64).to_le_bytes());
        hasher.update(&main);
        let hash = hasher.finalize().to_hex().to_string();
        std::fs::write(
            runtime_dir.join("runtime.json"),
            serde_json::json!({"schema": 1, "hash": hash, "modules": ["main.wasm"]}).to_string(),
        )
        .unwrap();
        let runtime = Runtime::read(&runtime_dir).unwrap();

        let other = "f".repeat(64);
        let module = split_against(&other);
        assert_eq!(linked_runtime(&module).unwrap(), Some(other.clone()));
        let Err(refused) = Model::compile_with(&module, &runtime) else {
            panic!("a module split against another runtime linked");
        };
        let refused = refused.to_string();
        assert!(
            refused.contains(&other) && refused.contains(&hash),
            "{refused}"
        );

        // Nothing here holds the runtime it names: refused, naming it.
        let Err(missing) = Model::compile(&module) else {
            panic!("a module whose runtime is not here linked");
        };
        assert!(missing.to_string().contains(&other), "{missing}");
    }
}

/// A module writes into the request's document through the device's
/// document calls; history there is the device's alone, so a module's
/// attempt to write a change of its own is refused at the device.
#[cfg(test)]
mod history_is_the_devices {
    use automerge::model_device::wire::{Reader, Writer};
    use automerge::model_device::{ANSWERED, Documents, REFUSED, op};
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, Prop, ROOT, ReadDoc as _, ScalarValue};

    /// `call` on `handle`, followed by `rest`, as a module writes it.
    fn call(
        documents: &mut Documents,
        call: u8,
        handle: u32,
        rest: impl Fn(&mut Writer),
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.byte(call);
        w.put(&handle);
        rest(&mut w);
        documents.answer(&w.0)
    }

    fn commit(w: &mut Writer) {
        w.put(&Some(
            "tilo.ctx/1 a history entry of the module's own".to_owned(),
        ))
        .put(&Some(7_i64));
    }

    #[test]
    fn a_model_module_writes_content_but_records_nothing_on_the_requests_document() {
        let mut device = AutoCommit::new();
        device.put(ROOT, "seed", 1).unwrap();
        device.commit();
        let heads = device.get_heads();
        let mut documents = Documents::new(device);

        // Content is the module's to write.
        let wrote = call(&mut documents, op::PUT, 0, |w| {
            w.put(&ROOT)
                .put(&Prop::Map("title".into()))
                .put(&ScalarValue::from("verb"));
        });
        assert_eq!(wrote.first(), Some(&ANSWERED), "a write is answered");

        // History is not: a commit, an empty change, an actor of its choosing,
        // changes from elsewhere, or a document merged in are all refused.
        let mut other = AutoCommit::new();
        other.put(ROOT, "forged", true).unwrap();
        let forged = other.save();
        let refused = [
            call(&mut documents, op::COMMIT_WITH, 0, commit),
            call(&mut documents, op::EMPTY_CHANGE, 0, commit),
            call(&mut documents, op::SET_ACTOR, 0, |w| {
                w.put(&automerge::ActorId::from(&b"someone else"[..]));
            }),
            call(&mut documents, op::LOAD_INCREMENTAL, 0, |w| {
                w.bytes(&forged);
            }),
            call(&mut documents, op::MERGE, 0, |w| {
                w.put(&0_u32);
            }),
        ];
        for answer in &refused {
            assert_eq!(answer.first(), Some(&REFUSED), "{answer:?}");
        }

        // A document the module opens is its scratch, where it may commit.
        let mut new = Writer::new();
        new.byte(op::NEW);
        new.put(&AutoCommit::new().text_encoding());
        let opened = documents.answer(&new.0);
        assert_eq!(opened.first(), Some(&ANSWERED));
        let mut r = Reader::new(&opened[1..]);
        let scratch: u32 = r.take().unwrap();
        assert_ne!(scratch, 0);
        let committed = call(&mut documents, op::COMMIT_WITH, scratch, commit);
        assert_eq!(committed.first(), Some(&ANSWERED));

        // What the device keeps is the module's write, still uncommitted, on
        // the history it had: the device commits it under its own envelope.
        let kept = documents.into_document();
        assert!(
            kept.pending_ops() > 0,
            "the write waits for the device's commit"
        );
        assert_eq!(kept.committed_heads(), heads);
        assert!(kept.get(ROOT, "forged").unwrap().is_none());
    }
}
