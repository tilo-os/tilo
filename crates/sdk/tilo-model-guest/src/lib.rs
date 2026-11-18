//! An app's model as a WebAssembly module: the guest side of the device's
//! model ABI.
//!
//! The device that holds a workspace's keys executes an installed app's
//! model from the app's package (`model.wasm`), with no build of its own.
//! The module imports nothing but the device's calls below and the device's
//! document calls, and exports [`EXPORT`]; the verbs are the app's own, from
//! its model crate.
//!
//! The device does everything about an action but the verb. It binds the
//! action from its JSON arguments against the verbs the package's signed
//! surface declares, builds its envelope (its id, base heads, actor and
//! time), writes its history entry and commits its change, and replays an
//! invocation another holder recorded at the heads it was made on: the action
//! framework is executed natively there, once per device rather than once per app.
//! The module gets the action in a compact typed form ([`wire`]), not JSON,
//! and only decodes it and applies the verb:
//!
//! - [`Op::Initialize`] writes the resource's initial content;
//! - [`Op::Bind`] reads an action and answers what it is (its recorded
//!   bytes, the kinds history files it under, the resource's names): the
//!   facts only the app's types know;
//! - [`Op::Apply`] applies the action to the device's document, as the
//!   invoker the envelope pins.
//!
//! The device holds the document. A module built with `--cfg
//! tilo_model_guest` reads and writes it through the device: its
//! `tilo_resource::Doc` is a handle on the device's document, every read and
//! write one call (`automerge::model_device`), so a verb acts on the
//! resource exactly as it does natively, and what it writes is the device's
//! Automerge's, which the device commits under the envelope and signs.
//!
//! An app's module serves every resource it declares: a request names the
//! resource kind it is for, and the module routes it to that resource's verbs.
//! An app builds one module per version by exporting [`serve`] over its
//! resources:
//!
//! ```text
//! #[unsafe(no_mangle)]
//! pub extern "C" fn tilo_model_apply() -> i32 {
//!     tilo_model_guest::serve(&[
//!         tilo_model_guest::Served::of::<tube_model::Library>(),
//!         tilo_model_guest::Served::of::<tube_model::WatchHistory>(),
//!     ])
//! }
//! ```

#![deny(rust_2018_idioms, missing_docs)]

pub mod wire;

use serde::de::DeserializeOwned;
use tilo_actions::{Action as _, ActionContext, ActionInvoker, InvocationId};
use tilo_resource::{Resource, Stamped};

/// The export the device calls, with no arguments, returning `0` once it wrote
/// its answer and anything else when it could not.
pub const EXPORT: &str = "tilo_model_apply";

/// The import module these calls are under.
pub const IMPORT_MODULE: &str = "tilo_model";

#[cfg(target_family = "wasm")]
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

/// What the device asks the model to do: the request's first byte. The
/// resource kind follows it as [`wire::text`], then what the op reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Write the resource's initial content into the device's document, which
    /// the device commits as the resource's genesis.
    Initialize = 0,
    /// Read the action that follows and answer what it is ([`Bound`]).
    Bind = 1,
    /// Apply the action that follows to the device's document. After it: the
    /// invoker the envelope pins (or null), the invocation it is part of (or
    /// null) and whether the device reproduces a recorded change.
    Apply = 2,
}

impl Op {
    /// The op a request's first byte names.
    #[must_use]
    pub const fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Initialize),
            1 => Some(Self::Bind),
            2 => Some(Self::Apply),
            _ => None,
        }
    }
}

/// What a module answers [`Op::Bind`] with: what only the app's types know
/// about an action.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Bound {
    /// The type id the action is recorded under ([`Resource::ACTION_TYPE`]).
    pub action_type: String,
    /// The action as it is recorded: its JSON, written by its own type, so
    /// every device keeps and replays the same bytes.
    pub payload: String,
    /// What the resource is called ([`Resource::label`]).
    pub label: String,
    /// Whether the change is a person's step or housekeeping.
    pub change: tilo_actions::ChangeKind,
    /// What the action is able to do.
    pub kind: tilo_actions::ActionKind,
    /// Whether it is a message someone received.
    pub incoming_message: bool,
    /// Whether its undo is computed by its own code
    /// ([`tilo_actions::Action::compensates`]).
    pub compensates: bool,
}

/// An action a module reads from its compact form.
pub trait Recorded: Sized {
    /// The action the reader is at.
    ///
    /// # Errors
    /// Why the value there is not one.
    fn read(reader: &mut wire::Reader<'_>) -> Result<Self, wire::Error>;
}

/// A `#[resource]` type's action: its operation and the origin time, as the recorded
/// JSON names them.
#[derive(serde::Deserialize)]
struct StampedForm<O> {
    op: O,
    #[serde(default)]
    at: i64,
}

impl<O: DeserializeOwned> Recorded for Stamped<O> {
    fn read(reader: &mut wire::Reader<'_>) -> Result<Self, wire::Error> {
        let form: StampedForm<O> = reader.read()?;
        Ok(Self {
            operation: form.op,
            now_ms: form.at,
        })
    }
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
    /// The handler that answers a request for it: the op and the rest of the
    /// request, answering what the device reads back.
    pub handler: fn(Op, &mut wire::Reader<'_>) -> Result<Vec<u8>, String>,
}

impl Served {
    /// Serve the resource `R` through its own verbs.
    #[must_use]
    pub fn of<R: Resource>() -> Self
    where
        R::Action: Recorded,
    {
        Self {
            kind: R::KIND,
            handler: serve_resource::<R>,
        }
    }
}

/// What a module answers when it could not do what it was asked.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Refused {
    /// Why, in the device's words for the caller.
    pub error: String,
}

/// Serve the device's requests for every resource an app declares: what its
/// model module exports as [`EXPORT`]. Each request names the resource kind it
/// is for, and only that resource's verbs answer it.
#[cfg(target_family = "wasm")]
pub fn serve(resources: &[Served]) -> i32 {
    // A module aborts on a panic; what it said is its answer, so the device
    // reports why rather than only that the module trapped.
    std::panic::set_hook(Box::new(|panic| {
        write(&refusal(panic.to_string()));
    }));
    let answer = match read().and_then(|request| asked(resources, &request)) {
        Ok(answer) => answer,
        Err(error) => refusal(error),
    };
    write(&answer)
}

/// Answer the request `bytes` with the resource it names.
///
/// # Errors
/// A request the module cannot read, for a kind it does not serve, or one the
/// resource refuses.
pub fn asked(resources: &[Served], bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = wire::Reader::new(bytes);
    let op = reader
        .byte()
        .ok()
        .and_then(Op::of)
        .ok_or("the request names no op")?;
    let kind = reader.text().map_err(|error| error.to_string())?;
    let served = resources
        .iter()
        .find(|served| served.kind == kind)
        .ok_or_else(|| format!("the module serves no `{kind}` resource"))?;
    (served.handler)(op, &mut reader)
}

/// `error` as the module answers it.
#[cfg(target_family = "wasm")]
fn refusal(error: String) -> Vec<u8> {
    serde_json::to_vec(&Refused { error }).unwrap_or_default()
}

/// One request for the resource `R`, answered by its own verbs.
fn serve_resource<R: Resource>(op: Op, reader: &mut wire::Reader<'_>) -> Result<Vec<u8>, String>
where
    R::Action: Recorded,
{
    let action = match op {
        Op::Initialize => {
            let mut doc = device_document()?;
            R::initialize(&mut doc)
                .map_err(|error| format!("initializing the resource: {error}"))?;
            return Ok(b"{}".to_vec());
        }
        Op::Bind | Op::Apply => R::Action::read(reader).map_err(|error| error.to_string())?,
    };
    if op == Op::Bind {
        let payload = serde_json::to_string(&action).map_err(|error| error.to_string())?;
        let bound = Bound {
            action_type: R::ACTION_TYPE.to_owned(),
            payload,
            label: R::label(),
            change: action.change_kind(),
            kind: action.action_kind(),
            incoming_message: action.incoming_message(),
            compensates: action.compensates(),
        };
        return serde_json::to_vec(&bound).map_err(|error| error.to_string());
    }
    let invoker: Option<ActionInvoker> = reader.read().map_err(|error| error.to_string())?;
    let invocation: Option<InvocationId> = reader.read().map_err(|error| error.to_string())?;
    let replaying: bool = reader.read().map_err(|error| error.to_string())?;
    let context = ActionContext {
        invoker,
        source_invocation: invocation,
        ..ActionContext::default()
    };
    let mut doc = device_document()?;
    tilo_actions::replaying_as(replaying, || {
        tilo_actions::with_invoking_context(Some(context), || action.apply(&mut doc))
    })
    .map_err(|error| error.to_string())?;
    Ok(b"{}".to_vec())
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
#[cfg(target_family = "wasm")]
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
#[cfg(target_family = "wasm")]
fn write(bytes: &[u8]) -> i32 {
    let Ok(length) = i32::try_from(bytes.len()) else {
        return 1;
    };
    let offset = i32::try_from(bytes.as_ptr() as usize).unwrap_or(i32::MAX);
    unsafe { answer(offset, length) };
    0
}

/// A model has no entropy of its own: a module built for
/// `wasm32-unknown-unknown` has nothing to ask, and needs nothing. Every id it
/// writes comes from the request, and every actor is the device's or the
/// invocation's. So this fills zeros and says it worked; a zero draw is a
/// value, and no change depends on it. Built with
/// `--cfg getrandom_backend="custom"`, which is what makes getrandom compile
/// into a module that links no browser glue.
///
/// # Safety
/// Called by getrandom with a writable buffer of `len` bytes.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
// getrandom's custom backend calls this signature; it can only answer with an
// error it never makes, and no module build can change that contract.
#[allow(clippy::unnecessary_wraps, reason = "getrandom's custom backend ABI")]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // SAFETY: getrandom hands over a buffer it allocated of exactly `len` bytes.
    unsafe { std::ptr::write_bytes(dest, 0, len) };
    Ok(())
}
