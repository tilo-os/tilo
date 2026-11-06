//! A Rust app's model and verbs, built as the WebAssembly module the document host runs.
//!
//! `docs/architecture/apps.md` §2 gives the session's document host every document, and §1
//! makes an app's model and verbs a WebAssembly module that runs there with no imports
//! beyond the host's. A Rust app's model is the same types with the same verbs as a tilolang
//! package's (`docs/designs/app-language/one-app-model.md` §4), so this crate is the module
//! side of the same boundary: it holds the document only for the length of a call, keeps
//! nothing between calls, and reaches nothing but the imports [`abi`] names.
//!
//! # What a call is
//!
//! The host loads the module, finds the export [`abi::export`] names for a record's kind, and
//! calls it with no arguments. The module pulls its [`Request`] with the `request` import,
//! runs the model against the document the request carried, and publishes its [`Answer`] — or
//! its [`Refused`] — with `answer` or `fail`. Everything crosses as JSON in the module's own
//! memory: the host never sees a Rust type of the model's, and the model never sees a key, a
//! transport or a file.
//!
//! The clock, the invoker and entropy are the host's too. Entropy matters most: the
//! document's actor id and the `uuid` crate both draw it through `getrandom`, whose custom
//! backend this module answers with the host's `entropy` import. A model therefore never
//! reaches a system random source — it has none — and what the host replays with the same
//! request produces the same change.
//!
//! # How an app declares its records
//!
//! [`serve_record!`] names the resource type the module serves at one export, and the export's
//! symbol is [`abi::export`] of its kind. Nothing else has to be kept in step with it.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{
    ActionAddress, ActionContext, ActionInvoker, ActionRegistry, DoorError, ResourceSpec,
};
use tilo_resource::{Doc, ModelError, Resource, ResourceKind};

pub use tilo_model_abi as abi;

/// What the host asks the module to do.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    /// Which operation this is.
    pub op: Op,
    /// The record's kind (`tasks.tracker`).
    pub kind: String,
    /// The resource address the record is published at.
    pub address: String,
    /// The record's document, saved, as base64: `None` for one that does not exist yet.
    #[serde(default)]
    pub document: Option<String>,
    /// The action the host admits, for [`Op::Invoke`].
    #[serde(default)]
    pub action: Option<Asked>,
    /// What the reading is of: nothing for the record itself, or the id of one of its items.
    #[serde(default)]
    pub item: Option<String>,
    /// Whether `address` names the record itself, rather than one of its items.
    #[serde(default)]
    pub record: bool,
    /// Who invokes, as the session decided it: the person, an agent acting for them, or the
    /// app. The module never chooses it.
    #[serde(default)]
    pub invoker: Option<ActionInvoker>,
    /// The time the change is made at, in milliseconds since the Unix epoch, as the session
    /// read it once for this invocation.
    #[serde(default)]
    pub now_ms: i64,
}

/// What the host asks the model to do with the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Open the record, or write its genesis when it holds none, and answer its document.
    Open,
    /// Bring the record up to the model's version ([`ResourceKind::migrate`]).
    Migrate,
    /// Answer what the record offers: its tree of actions and items.
    Offers,
    /// Answer a reading of the record, or of one of its items.
    Read,
    /// Run an action of the record and answer its document and reading.
    Invoke,
}

/// An action the host admits, as the door named it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asked {
    /// The action's name (`create`).
    pub action: String,
    /// Its arguments, by parameter name.
    #[serde(default)]
    pub arguments: serde_json::Map<String, Json>,
}

/// What the module answers.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Answer {
    /// The record's document, saved, as base64: the host keeps it as it is now.
    #[serde(default)]
    pub document: Option<String>,
    /// Whether the record was made by this call, having held nothing before.
    #[serde(default)]
    pub created: bool,
    /// What the record offers, for [`Op::Offers`].
    #[serde(default)]
    pub offers: Option<Json>,
    /// The reading, for [`Op::Read`] and after [`Op::Invoke`].
    #[serde(default)]
    pub reading: Option<Json>,
}

/// Why the module refused a call: what the host reports as the door's answer.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Refused {
    /// What kind of refusal it is.
    pub kind: RefusalKind,
    /// What it says, in words a person reads.
    pub message: String,
}

/// The kinds of refusal a model answers with, as the door names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The record does not read as the model.
    Model,
    /// The door refused the action: no such action, bad arguments, or the verb's own refusal.
    Door,
    /// The call named an operation or a record the module does not serve.
    Unknown,
}

impl From<ModelError> for Refused {
    fn from(error: ModelError) -> Self {
        Self {
            kind: RefusalKind::Model,
            message: error.to_string(),
        }
    }
}

impl From<DoorError> for Refused {
    fn from(error: DoorError) -> Self {
        Self {
            kind: RefusalKind::Door,
            message: error.to_string(),
        }
    }
}

impl From<tilo_actions::ActionError> for Refused {
    fn from(error: tilo_actions::ActionError) -> Self {
        Self {
            kind: RefusalKind::Door,
            message: error.to_string(),
        }
    }
}

impl From<automerge::AutomergeError> for Refused {
    fn from(error: automerge::AutomergeError) -> Self {
        Self {
            kind: RefusalKind::Model,
            message: error.to_string(),
        }
    }
}

impl From<serde_json::Error> for Refused {
    fn from(error: serde_json::Error) -> Self {
        Self {
            kind: RefusalKind::Model,
            message: error.to_string(),
        }
    }
}

/// Run `request` against the model `R` declares, and answer it.
///
/// # Errors
///
/// A [`Refused`] when the model or the door refused; the host reports it as the call's
/// failure, and keeps no change.
pub fn serve<R: Resource>(request: &Request) -> Result<Answer, Refused> {
    let kind = ResourceKind::of::<R>();
    if kind.kind() != request.kind {
        return Err(Refused {
            kind: RefusalKind::Unknown,
            message: format!("this module serves {}, not {}", kind.kind(), request.kind),
        });
    }
    let mut doc = match &request.document {
        Some(text) => Doc::load(&decode(text)?)?,
        None => Doc::new(),
    };
    let mut answer = Answer::default();
    match request.op {
        Op::Open => answer.created = open(&kind, &mut doc)?,
        Op::Migrate => {
            if kind.needs_migrate(&doc) {
                kind.migrate(&mut doc)?;
                doc.commit();
            }
        }
        Op::Offers => {
            answer.offers = Some(serde_json::to_value(kind.publish(&request.address, &doc)?)?);
        }
        Op::Read => {
            answer.reading = Some(reading(&kind, request, &doc)?);
        }
        Op::Invoke => {
            let asked = request.action.as_ref().ok_or_else(|| Refused {
                kind: RefusalKind::Unknown,
                message: "an invocation names no action".to_owned(),
            })?;
            invoke(&kind, request, &mut doc, asked)?;
            answer.reading = Some(reading(&kind, request, &doc)?);
        }
    }
    answer.document = Some(encode(&doc.save()));
    Ok(answer)
}

/// Open the record `kind` keeps in `doc`, writing its genesis when it holds none and
/// migrating one from before the model as the model asks. Answers whether it was made.
fn open(kind: &ResourceKind, doc: &mut Doc) -> Result<bool, Refused> {
    if !kind.holds_or_older(doc) {
        kind.initialize(doc)?;
        doc.commit();
        return Ok(true);
    }
    if kind.needs_migrate(doc) {
        kind.migrate(doc)?;
        doc.commit();
    }
    // A record the model cannot read even after its migration is refused here, rather than
    // answered as current state.
    kind.read(doc)?;
    Ok(false)
}

/// The reading of the record, or of the item `request.item` names: the record is never handed
/// over.
fn reading(kind: &ResourceKind, request: &Request, doc: &Doc) -> Result<Json, Refused> {
    match &request.item {
        Some(item) => kind.content(doc, Some(item)).map_err(|error| Refused {
            kind: RefusalKind::Model,
            message: error.to_string(),
        }),
        None => {
            let state = kind.read(doc)?;
            let mut reading = kind.to_json(state.as_ref())?;
            let spec = kind.publish(&request.address, doc)?;
            if let Some(object) = reading.as_object_mut() {
                let parts = parts_of(&spec);
                if !parts.is_empty() {
                    object.insert("parts".to_owned(), Json::Array(parts));
                }
            }
            Ok(reading)
        }
    }
}

/// Run `asked`'s action against `doc`, committed as the request's invoker.
fn invoke(
    kind: &ResourceKind,
    request: &Request,
    doc: &mut Doc,
    asked: &Asked,
) -> Result<(), Refused> {
    let tree = kind.publish(&request.address, doc)?;
    let address = ActionAddress::new(request.address.as_str(), asked.action.as_str());
    // An action about one of the record's items is offered by the item's own tree, the way
    // a view of that item is offered the item's actions; one about the record is the
    // record's.
    let known = if request.record {
        tree
    } else {
        kind.child(doc, &request.address)?
            .ok_or_else(|| Refused::from(DoorError::Unknown(address.clone())))?
    };
    let encoded =
        tilo_actions::door::encode_json(std::slice::from_ref(&known), &address, &asked.arguments)?;
    let invoker = request.invoker.clone().unwrap_or_else(app_invoker);
    let context = ActionContext::for_resource(&request.address, &asked.action, &asked.action)
        .with_invoker(invoker.clone());
    let mut registry = ActionRegistry::new();
    kind.register(&mut registry);
    let action = registry.decode(&encoded)?;
    tilo_actions::apply_local_with_context(
        doc,
        action.as_ref(),
        encoded,
        writer(&invoker),
        context,
    )?;
    doc.commit();
    Ok(())
}

/// Who the change is committed under, as Automerge names the actor: the person the invoker
/// acts for, else the invoker itself.
fn writer(invoker: &ActionInvoker) -> Option<&[u8]> {
    Some(
        invoker
            .on_behalf_of
            .as_deref()
            .unwrap_or(&invoker.actor)
            .as_bytes(),
    )
}

/// The invoker of a call the request named none for: the app, acting on its own.
fn app_invoker() -> ActionInvoker {
    ActionInvoker::actor("the app")
}

/// The addresses of the items `spec` publishes.
fn parts_of(spec: &ResourceSpec) -> Vec<Json> {
    fn walk(spec: &ResourceSpec, out: &mut Vec<Json>) {
        for child in &spec.children {
            out.push(Json::String(child.id.as_str().to_owned()));
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(spec, &mut out);
    out
}

/// The bytes of `doc`, base64 for JSON.
fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `text`'s bytes, base64 as JSON carries them.
fn decode(text: &str) -> Result<Vec<u8>, Refused> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| Refused {
            kind: RefusalKind::Model,
            message: format!("the record's document is not base64: {error}"),
        })
}

/// Serve `R`'s records for the call the host made: pull the request, run the model, publish
/// the answer.
pub fn serve_export<R: Resource>() {
    let bytes = pull(|pointer, capacity| unsafe { host::request(pointer, capacity) });
    let request: Request = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(error) => fail(&format!("this request is not one: {error}")),
    };
    match serve::<R>(&request) {
        Ok(answer) => match serde_json::to_vec(&answer) {
            Ok(bytes) => unsafe { host::answer(bytes.as_ptr(), bytes.len()) },
            Err(error) => fail(&format!("this answer is not one: {error}")),
        },
        Err(refused) => match serde_json::to_vec(&refused) {
            Ok(bytes) => unsafe { host::fail(bytes.as_ptr(), bytes.len()) },
            Err(error) => fail(&error.to_string()),
        },
    }
}

/// Pull bytes from the host into a buffer this module owns, growing it until the host's
/// answer fits.
///
/// `ask` is given a pointer and a capacity, and answers how many bytes it wrote, or the
/// negated size it needs when the capacity was too small.
pub fn pull(mut ask: impl FnMut(*mut u8, usize) -> i32) -> Vec<u8> {
    let mut buffer = vec![0_u8; 8192];
    loop {
        let wrote = ask(buffer.as_mut_ptr(), buffer.len());
        if wrote >= 0 {
            let length = usize::try_from(wrote).unwrap_or(0).min(buffer.len());
            buffer.truncate(length);
            return buffer;
        }
        let need = usize::try_from(wrote.unsigned_abs()).unwrap_or(0);
        if need <= buffer.len() {
            return Vec::new();
        }
        buffer = vec![0_u8; need];
    }
}

/// Fail the call, with what the host reports as its refusal.
pub fn fail(message: &str) -> ! {
    unsafe { host::fail(message.as_ptr(), message.len()) };
    // The host does not return from `fail`. A module runs only as wasm, where a call that
    // reaches here is a trap; a host that links the model to test it reads the message it
    // recorded before this.
    #[cfg(target_arch = "wasm32")]
    core::arch::wasm32::unreachable();
    #[cfg(not(target_arch = "wasm32"))]
    panic!("{message}");
}

/// The imports the module calls, as the host provides them.
pub mod host {
    #[link(wasm_import_module = "tilo_model")]
    unsafe extern "C" {
        /// The request the host made, written into the buffer if it fits; answers the
        /// length, or the negated length it needs.
        pub fn request(pointer: *mut u8, capacity: usize) -> i32;
        /// `count` bytes of the host's entropy, written at `pointer`; answers how many.
        pub fn entropy(pointer: *mut u8, count: usize) -> i32;
        /// This call's answer, or its refusal, as JSON.
        pub fn answer(pointer: *const u8, length: usize);
        pub fn fail(pointer: *const u8, length: usize);
        /// What a person reading the host's log should see.
        pub fn log(pointer: *const u8, length: usize);
    }
}

/// Serve `$ty`'s records at the export named `$symbol`: what [`tilo_model_abi::export`] names
/// for the kind `$ty` keeps.
///
/// ```ignore
/// tilo_model_guest::serve_record!(Tracker, record_tasks_tracker);
/// ```
#[macro_export]
macro_rules! serve_record {
    ($ty:ty, $symbol:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $symbol() {
            $crate::serve_export::<$ty>();
        }
    };
}

#[cfg(target_arch = "wasm32")]
mod entropy_source {
    /// getrandom's custom backend: the host's entropy, through [`crate::host::entropy`].
    ///
    /// # Safety
    ///
    /// Called by `getrandom` with `dest` valid for `len` bytes to be written.
    #[unsafe(no_mangle)]
    unsafe extern "Rust" fn __getrandom_v03_custom(
        dest: *mut u8,
        len: usize,
    ) -> Result<(), getrandom::Error> {
        let filled = unsafe { crate::host::entropy(dest, len) };
        if usize::try_from(filled) == Ok(len) {
            Ok(())
        } else {
            Err(getrandom::Error::new_custom(
                1,
                "the document host supplied no entropy",
            ))
        }
    }
}
