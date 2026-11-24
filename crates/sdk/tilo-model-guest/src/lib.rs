//! An app's model as a WebAssembly module: the guest side of the document
//! device's model ABI.
//!
//! The device that holds a workspace's keys executes an installed app's model from
//! the app's package (`model.wasm`), with no build of its own. The module
//! imports nothing but the device's calls below and the device's document
//! calls, and exports [`EXPORT`]; the verbs, declarations and typed arguments
//! are the app's own, from its model crate.
//!
//! The device does everything about an action but what only the app's types
//! know. It checks the invocation's arguments against the verb the package's
//! signed surface declares, builds the envelope (its id, base heads, actor and
//! time), writes the history entry and commits the change, and replays an
//! invocation another holder recorded at the heads it was made on, with its own
//! action executor, exactly as for an app compiled in. The module
//! answers four requests ([`Op`]):
//!
//! - [`Op::Initialize`] writes a new resource's initial content, which the
//!   device commits as its genesis;
//! - [`Op::Bind`] builds the action from the arguments the device checked,
//!   through the resource's own declarations, and answers what it is
//!   ([`Bound`]): the bytes it is recorded as and the kinds history files it
//!   under;
//! - [`Op::Read`] answers the same of an action another holder recorded;
//! - [`Op::Apply`] applies a recorded action's verb to the device's document,
//!   under the invoking context the envelope pins.
//!
//! The device holds the document. A module built with `--cfg
//! tilo_model_guest` reads and writes it through the device: its
//! `tilo_resource::Doc` is a handle on the device's document, every read and
//! write one call (`automerge::model_device`), so a verb acts on the
//! resource exactly as it does natively. The module writes content only: the
//! device refuses a commit, an actor or changes loaded into the request's
//! document from a module, so what history keeps is the device's.
//!
//! An app's module serves every resource it declares: a request names the
//! resource kind it is for, and the module routes it to that resource's verbs.
//! An app's guest crate declares its entry with [`model!`], naming the app (its
//! directory under `apps/`) and its resources:
//!
//! ```text
//! tilo_model_guest::model!("assistant", assistant_model::Window);
//! ```
//!
//! Built alone (`tilolang model`, an app outside this repository), that is a
//! whole module exporting [`EXPORT`]. The repository builds every app's guest
//! into one module instead (crates/sdk/tilo-models-wasm, with
//! `--cfg tilo_model_split`), where each entry is `tilo_model_apply_<app>`
//! and is named in the `tilo_split_exports` section; `wasm-split --models`
//! then leaves what every app shares (automerge, std, the resource layer) in
//! one model runtime and only the app's own code in its `model.wasm`. A device
//! links that module after the runtime it names (`tilo-model-engine`).

#![deny(rust_2018_idioms, missing_docs)]

use serde::{Deserialize, Serialize};
use tilo_actions::{Action as _, ActionContext, ActionKind, ChangeKind, Described, ExecutionLocus};
use tilo_resource::{Doc, ModelError, Resource};

/// The export the device calls, with no arguments, returning `0` once it wrote
/// its answer and anything else when it could not.
pub const EXPORT: &str = "tilo_model_apply";

/// The import module these calls are under.
pub const IMPORT_MODULE: &str = "tilo_model";

/// Declare the app's entry: what the device calls to serve a request for one
/// of the resources `$resource` of app `$app` (its directory under `apps/`).
///
/// A module built alone exports it as [`EXPORT`]. Built into the combined
/// module (`--cfg tilo_model_split`), it is `tilo_model_apply_<app>`, so every
/// app's entry has a name of its own there, and the `tilo_split_exports`
/// section names it as the export of the module `<app>`, which is what
/// `wasm-split` splits that app's own code into.
#[cfg(not(tilo_model_split))]
#[macro_export]
macro_rules! model {
    ($app:literal, $($resource:ty),+ $(,)?) => {
        /// Serve one request from the device that holds the workspace's keys.
        #[unsafe(no_mangle)]
        pub extern "C" fn tilo_model_apply() -> i32 {
            $crate::serve(&[$($crate::Served::of::<$resource>()),+])
        }
    };
}

/// Declare the app's entry ([`model!`] as the combined module builds it).
#[cfg(tilo_model_split)]
#[macro_export]
macro_rules! model {
    ($app:literal, $($resource:ty),+ $(,)?) => {
        /// Serve one request from the device that holds the workspace's keys.
        #[unsafe(export_name = concat!("tilo_model_apply_", $app))]
        pub extern "C" fn tilo_model_apply() -> i32 {
            $crate::serve(&[$($crate::Served::of::<$resource>()),+])
        }

        const _: () = {
            const LINE: &str = concat!($app, " tilo_model_apply_", $app, "\n");
            /// The split's lazy export: `<module> <export>`.
            #[unsafe(link_section = "tilo_split_exports")]
            #[used]
            static ENTRY: [u8; LINE.len()] = $crate::bytes(LINE);
        };
    };
}

/// `text`'s bytes as an array, for a static a section holds.
#[doc(hidden)]
#[must_use]
pub const fn bytes<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0_u8; N];
    out.copy_from_slice(text.as_bytes());
    out
}

#[link(wasm_import_module = "tilo_model")]
unsafe extern "C" {
    /// How many bytes the request holds.
    fn request_length() -> i32;
    /// Copy up to `capacity` bytes of the request to `offset`, answering how
    /// many it copied.
    fn request(offset: i32, capacity: i32) -> i32;
    /// Take the `length` bytes of the answer at `offset`.
    fn answer(offset: i32, length: i32);
}

/// What the device asks the model to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Write the resource's initial content into the device's document, which
    /// the device commits as the resource's genesis.
    Initialize,
    /// Build the action [`Request::action`] names from [`Request::arguments`],
    /// which the device checked against the signed surface, and answer what
    /// it is ([`Bound`]), described against the device's document as it is
    /// before the action, under [`Request::context`].
    Bind,
    /// Answer what the recorded action [`Request::payload`] is ([`Bound`]).
    Read,
    /// Apply the recorded action [`Request::payload`] to the device's
    /// document, under [`Request::context`].
    Apply,
}

/// One request from the device.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Request {
    /// What the model is asked to do.
    pub op: Option<Op>,
    /// The resource kind the request is for: which of the app's resources the
    /// module routes it to ([`Resource::KIND`]).
    pub kind: String,
    /// The verb [`Op::Bind`] builds, in the resource's vocabulary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// Its arguments, as the device bound them.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The invocation's origin clock, milliseconds since the Unix epoch.
    #[serde(default)]
    pub now: i64,
    /// The action [`Op::Read`] and [`Op::Apply`] are about, in the bytes it
    /// is recorded as ([`Bound::payload`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub payload: String,
    /// The invoking context: the one the envelope pins, which [`Op::Apply`]'s
    /// verb reads as a natively applied verb reads it
    /// ([`tilo_actions::invoking_context`]), and the invoker [`Op::Bind`]
    /// describes the action under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ActionContext>,
    /// Whether the device reproduces a recorded change rather than making a
    /// new one ([`tilo_actions::executor::is_replaying`]).
    #[serde(default)]
    pub replaying: bool,
}

/// What a module answers [`Op::Bind`] and [`Op::Read`] with: what only the
/// app's types know about an action.
#[derive(Debug, Serialize, Deserialize)]
pub struct Bound {
    /// The type id the action is recorded under ([`Resource::ACTION_TYPE`]).
    pub action_type: String,
    /// The action as it is recorded: its JSON, written by its own type, so
    /// every device keeps and replays the same bytes.
    pub payload: String,
    /// What the resource is called, in the person's language
    /// ([`Resource::label`]).
    pub label: String,
    /// Whether the change is a person's step or housekeeping.
    pub change: ChangeKind,
    /// What the action is able to do.
    pub kind: ActionKind,
    /// Whether it is a message someone received.
    pub incoming_message: bool,
    /// Whether its undo is computed by its own code
    /// ([`tilo_actions::Action::compensates`]).
    pub compensates: bool,
    /// Whether it loses something Undo cannot bring back.
    pub irreversible: bool,
    /// Where it is done.
    pub locus: ExecutionLocus,
    /// What history says it does and to which item, read from the
    /// device's document before it applies, for [`Op::Bind`]
    /// ([`tilo_actions::Action::describe`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub described: Option<Described>,
}

/// What the module answers.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Answer {
    /// What the action is, for [`Op::Bind`] and [`Op::Read`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<Bound>,
    /// Why the model did not do it, in the device's words for the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One resource an app's module serves, and the code that answers a request
/// for it.
///
/// A module is one per app, and an app declares one resource per document
/// kind it keeps ([`Resource::KIND`]), so a request names the kind it is for
/// and the module routes it here.
#[derive(Clone, Copy)]
pub struct Served {
    /// The resource kind this serves, as the app declares it.
    pub kind: &'static str,
    /// The resource's own code, and nothing else: what a request does about
    /// it — parsing it, routing it, writing the answer — is the same for
    /// every resource, so it is one function every app's module shares in the
    /// runtime.
    model: Model,
}

impl Served {
    /// Serve the resource `R` through its own verbs.
    #[must_use]
    pub fn of<R: Resource>() -> Self {
        Self {
            kind: R::KIND,
            model: Model {
                initialize: R::initialize,
                bind: bind::<R>,
                read: read_recorded::<R>,
                apply: apply::<R>,
            },
        }
    }
}

/// An invocation's arguments, as the device binds them.
type Arguments = serde_json::Map<String, serde_json::Value>;

/// The resource's own code, as the calls a request makes of it.
#[derive(Clone, Copy)]
struct Model {
    /// Its initial content.
    initialize: fn(&mut Doc) -> Result<(), ModelError>,
    /// Its action, built from bound arguments through its declarations and
    /// described under the invoking context.
    bind: fn(&str, &Arguments, i64, Option<ActionContext>) -> Result<Bound, String>,
    /// A recorded action of it, read.
    read: fn(&str) -> Result<Bound, String>,
    /// A recorded action of it, applied to the device's document.
    apply: fn(&str, Option<ActionContext>, bool) -> Result<(), String>,
}

/// `R`'s action `id` for `arguments`, built as the action API builds an
/// action of an app compiled in, and described from the device's document
/// under `context`, as the executor describes a native one.
fn bind<R: Resource>(
    id: &str,
    arguments: &Arguments,
    now: i64,
    context: Option<ActionContext>,
) -> Result<Bound, String> {
    let action = tilo_resource::build_action::<R>(id, arguments, now)?;
    let mut bound = bound::<R>(&action).map_err(|error| format!("{id}: {error}"))?;
    let doc = device_document()?;
    bound.described = tilo_actions::with_invoking_context(context, || action.describe(&doc));
    Ok(bound)
}

/// What `R`'s recorded `payload` is.
fn read_recorded<R: Resource>(payload: &str) -> Result<Bound, String> {
    bound::<R>(&recorded::<R>(payload)?)
}

/// What `action` is, as its own types say.
fn bound<R: Resource>(action: &R::Action) -> Result<Bound, String> {
    let encoded = tilo_resource::encode::<R>(action).map_err(|error| error.to_string())?;
    Ok(Bound {
        action_type: encoded.type_id,
        payload: String::from_utf8(encoded.payload).map_err(|error| error.to_string())?,
        label: R::label(),
        change: action.change_kind(),
        kind: action.action_kind(),
        incoming_message: action.incoming_message(),
        compensates: action.compensates(),
        irreversible: action.irreversible(),
        locus: action.locus(),
        described: None,
    })
}

/// `R`'s action recorded as `payload`.
fn recorded<R: Resource>(payload: &str) -> Result<R::Action, String> {
    serde_json::from_str(payload).map_err(|error| format!("the recorded action: {error}"))
}

/// Apply `R`'s recorded `payload` to the device's document under `context`,
/// as the device's executor applies it: reproducing a recorded change when
/// `replaying`.
fn apply<R: Resource>(
    payload: &str,
    context: Option<ActionContext>,
    replaying: bool,
) -> Result<(), String> {
    let action = recorded::<R>(payload)?;
    let mut doc = device_document()?;
    tilo_actions::replaying_as(replaying, || {
        tilo_actions::with_invoking_context(context, || action.apply(&mut doc))
    })
    .map_err(|error| error.to_string())
}

/// Serve the device's requests for every resource an app declares: what its
/// model module exports as [`EXPORT`]. Each request names the resource kind it
/// is for, and only that resource's verbs answer it.
pub fn serve(resources: &[Served]) -> i32 {
    // A module aborts on a panic; what it said is its answer, so the device
    // reports why rather than only that the module trapped.
    std::panic::set_hook(Box::new(|panic| {
        write(&Answer {
            error: Some(panic.to_string()),
            ..Answer::default()
        });
    }));
    let answer = match read().and_then(|request| asked(resources, &request)) {
        Ok(answer) => answer,
        Err(error) => Answer {
            error: Some(error),
            ..Answer::default()
        },
    };
    write(&answer)
}

/// Answer the request `bytes` with the resource it names: every app's module
/// calls this one function, from the model runtime when the apps are built
/// together.
///
/// # Errors
/// A request the module cannot read, for a kind it does not serve, or one the
/// resource refuses.
#[inline(never)]
pub fn asked(resources: &[Served], bytes: &[u8]) -> Result<Answer, String> {
    let request: Request =
        serde_json::from_slice(bytes).map_err(|error| format!("the request: {error}"))?;
    let served = resources
        .iter()
        .find(|served| served.kind == request.kind)
        .ok_or_else(|| format!("the module serves no `{}` resource", request.kind))?;
    let model = served.model;
    let bound = match request.op.ok_or("the request names no op")? {
        Op::Initialize => {
            let mut doc = device_document()?;
            (model.initialize)(&mut doc)
                .map_err(|error| format!("initializing the resource: {error}"))?;
            None
        }
        Op::Bind => Some((model.bind)(
            &request.action,
            &request.arguments,
            request.now,
            request.context,
        )?),
        Op::Read => Some((model.read)(&request.payload)?),
        Op::Apply => {
            (model.apply)(&request.payload, request.context, request.replaying)?;
            None
        }
    };
    Ok(Answer { bound, error: None })
}

/// The document the device's request is about.
#[cfg(tilo_model_guest)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "outside a module there is no device, which the other build says"
)]
fn device_document() -> Result<tilo_resource::Doc, String> {
    Ok(tilo_resource::Doc::of_device())
}

/// A model module is built with `--cfg tilo_model_guest`, whose documents are
/// the device's; outside one there is no device to ask.
#[cfg(not(tilo_model_guest))]
fn device_document() -> Result<tilo_resource::Doc, String> {
    Err(
        "a model module is built with `--cfg tilo_model_guest`, whose documents are the device's"
            .into(),
    )
}

/// The request the device wrote for this call.
fn read() -> Result<Vec<u8>, String> {
    let length = usize::try_from(unsafe { request_length() })
        .map_err(|_| "the request has no length".to_string())?;
    let mut buffer = vec![0_u8; length];
    let offset = i32::try_from(buffer.as_mut_ptr() as usize).unwrap_or(i32::MAX);
    let capacity = i32::try_from(length).unwrap_or(i32::MAX);
    let copied = usize::try_from(unsafe { request(offset, capacity) })
        .map_err(|_| "the request did not arrive".to_string())?;
    buffer.truncate(copied.min(length));
    Ok(buffer)
}

/// Hand the model's answer to the device.
fn write(reply: &Answer) -> i32 {
    let Ok(bytes) = serde_json::to_vec(reply) else {
        return 1;
    };
    let Ok(length) = i32::try_from(bytes.len()) else {
        return 1;
    };
    let offset = i32::try_from(bytes.as_ptr() as usize).unwrap_or(i32::MAX);
    unsafe { answer(offset, length) };
    0
}

/// A model has no entropy of its own: a module built for
/// `wasm32-unknown-unknown` has nothing to ask, and needs nothing. Every id
/// and actor is the device's, which builds the envelope. So this fills zeros
/// and says it worked; a zero draw is a value, and no change depends on it.
/// Built with `--cfg getrandom_backend="custom"`, which is what makes
/// getrandom compile into a module that links no browser glue.
///
/// # Safety
/// Called by getrandom with a writable buffer of `len` bytes.
#[unsafe(no_mangle)]
// getrandom's custom backend calls this signature; it can only answer with an
// error it never makes, and no module build can change that contract.
#[allow(
    clippy::unnecessary_wraps,
    reason = "getrandom's ABI requires a `Result`, which this backend never errs in"
)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // SAFETY: getrandom hands over a buffer it allocated of exactly `len` bytes.
    unsafe { std::ptr::write_bytes(dest, 0, len) };
    Ok(())
}

/// [`__getrandom_v03_custom`] for getrandom 0.2, which `rand_core` 0.6 asks: the
/// same zeros, for the same reason.
#[cfg(target_arch = "wasm32")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "getrandom 0.2's custom backend ABI"
)]
fn fill_zeros(dest: &mut [u8]) -> Result<(), getrandom_02::Error> {
    dest.fill(0);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
getrandom_02::register_custom_getrandom!(fill_zeros);
