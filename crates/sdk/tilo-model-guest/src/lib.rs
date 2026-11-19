//! An app's model as a WebAssembly module: the guest side of the document
//! device's model ABI.
//!
//! The device that holds a workspace's keys executes an installed app's model from
//! the app's package (`model.wasm`), with no build of its own. The module
//! imports nothing but the device's calls below and the device's document
//! calls, and exports [`EXPORT`]; the verbs, declarations and typed arguments
//! are the app's own, from its model crate.
//!
//! The device holds the document. A module built with `--cfg
//! tilo_model_guest` reads and writes it through the device: its
//! `tilo_resource::Doc` is a handle on the device's document, every read and
//! write one call (`automerge::model_device`), so a verb acts on the
//! resource at its current heads exactly as it does natively, and the change
//! it makes is the device's Automerge's, which the device signs and keeps. The
//! module carries the app's own code and none of Automerge's storage.
//!
//! An invocation is the one the action API makes: the action is built from
//! the resource's declarations for the JSON arguments exactly as the device
//! binds them, then applied through the action executor under an envelope
//! whose id, base heads, actor and time all come from the request. So two
//! holders of the resource given the same request write the same change, and
//! a device that receives it replays the envelope to the same change, through
//! the same calls against the document at the change's parents: the
//! determinism every key holder needs to verify another's work.
//!
//! An app's module serves every resource it declares: a request names the
//! resource kind it is for, and the module routes it to that resource's verbs.
//! An app's guest crate declares its entry with [`model!`], naming the app (its
//! directory under `apps/`) and its resources:
//!
//! ```text
//! tilo_model_guest::model!("tube", tube_model::Library, tube_model::WatchHistory);
//! ```
//!
//! Built alone (`tilo-app model`, an app outside this repository), that is a
//! whole module exporting [`EXPORT`]. The repository builds every app's guest
//! into one module instead (crates/sdk/tilo-models-wasm, with
//! `--cfg tilo_model_split`), where each entry is `tilo_model_apply_<app>`
//! and is named in the `tilo_split_exports` section; `wasm-split --models`
//! then leaves what every app shares (automerge, std, the resource layer) in
//! one model runtime and only the app's own code in its `model.wasm`. A device
//! links that module after the runtime it names (`tilo-model-engine`).

#![deny(rust_2018_idioms, missing_docs)]

use serde::Deserialize;
use tilo_actions::{
    Acting, Action, ActionContext, ActionEnvelope, ActionId, ActionInvoker, MemberId,
};
use tilo_resource::Resource;

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
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Op {
    /// Make the resource's genesis: the app's initial content, committed as
    /// one change at time zero under the actor the resource's identity
    /// derives.
    Initialize,
    /// Apply `action`, with `arguments`, to the device's document.
    Apply,
    /// Replay `envelope`, an invocation another holder recorded, against the
    /// device's document: what a holder that receives the change checks it by.
    Replay,
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
    /// The handler that answers a request for it.
    pub handler: fn(&Request) -> Result<Answer, String>,
}

impl Served {
    /// Serve the resource `R` through its own verbs.
    #[must_use]
    pub fn of<R: Resource>() -> Self {
        Self {
            kind: R::KIND,
            handler: serve_resource::<R>,
        }
    }
}

/// One request from the device.
#[derive(Debug, Deserialize)]
pub struct Request {
    op: Op,
    /// The resource kind the request is for: which of the app's resources the
    /// module routes it to ([`Resource::KIND`]).
    pub kind: String,
    /// The resource's document id: `@tilo:package-primary` for what an app
    /// keeps per workspace.
    id: String,
    /// The workspace the resource is minted in.
    home: String,
    /// The invoking member, as [`MemberId::of_actor_bytes`] reads it: a
    /// person's key, `agent:<id>` or `bot:<id>`.
    #[serde(default)]
    member: String,
    /// Who acts, and for whom, as the device's action API recorded it
    /// ([`tilo_actions::Acting`]): an agent a person's session handed to, for
    /// that person, writes changes that say so. Absent for a member acting for
    /// themselves, which [`Request::member`] alone names.
    #[serde(default)]
    acting: Option<serde_json::Value>,
    /// The action's id in the resource's vocabulary.
    #[serde(default)]
    action: String,
    /// Its arguments, as the action API binds them.
    #[serde(default)]
    arguments: serde_json::Map<String, serde_json::Value>,
    /// The invocation's origin clock, milliseconds since the Unix epoch.
    #[serde(default)]
    now: i64,
    /// The invocation [`Op::Replay`] replays, as the action API recorded it.
    #[serde(default)]
    envelope: Option<serde_json::Value>,
}

/// What the model answers. What it wrote is in the device's document, which
/// the device saves, signs and keeps.
#[derive(Debug, Default, serde::Serialize)]
pub struct Answer {
    /// The invocation as the action API recorded it: what a peer replays.
    #[serde(skip_serializing_if = "Option::is_none")]
    envelope: Option<serde_json::Value>,
    /// Why the model did not do it, in the device's words for the caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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
    let answer = match asked(resources) {
        Ok(answer) => answer,
        Err(error) => Answer {
            error: Some(error),
            ..Answer::default()
        },
    };
    write(&answer)
}

fn asked(resources: &[Served]) -> Result<Answer, String> {
    let request: Request =
        serde_json::from_slice(&read()?).map_err(|error| format!("the request: {error}"))?;
    let served = resources
        .iter()
        .find(|served| served.kind == request.kind)
        .ok_or_else(|| format!("the module serves no `{}` resource", request.kind))?;
    (served.handler)(&request)
}

/// One request for the resource `R`, answered by its own verbs.
fn serve_resource<R: Resource>(request: &Request) -> Result<Answer, String> {
    match request.op {
        Op::Initialize => genesis::<R>(request),
        Op::Apply => apply::<R>(request),
        Op::Replay => replay::<R>(request),
    }
}

/// The resource's genesis: the initializer's content in one change at time
/// zero, under the actor every device derives from its identity
/// ([`tilo_resource::genesis::actor`]).
fn genesis<R: Resource>(request: &Request) -> Result<Answer, String> {
    let actor = tilo_resource::genesis::actor(&request.id, R::KIND, &request.home);
    let mut doc = device_document()?;
    doc.set_actor(actor);
    R::initialize(&mut doc).map_err(|error| format!("initializing the resource: {error}"))?;
    Ok(genesis_answer(&mut doc))
}

/// The genesis change committed: the same for every resource, so one function
/// every app's module shares (in the model runtime when the apps are built
/// together), not a copy per resource. What it wrote is in the device's
/// document, which the device saves.
#[inline(never)]
fn genesis_answer(doc: &mut tilo_resource::Doc) -> Answer {
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
            .with_time(0),
    );
    Answer::default()
}

/// Who acts, for whom: what an action's invocation reads of the request
/// before the resource's own code, shared by every resource.
struct Invocation {
    member: MemberId,
    acting: Acting,
}

#[inline(never)]
fn invocation(request: &Request) -> Result<Invocation, String> {
    let member = MemberId::of_actor_bytes(request.member.as_bytes())
        .ok_or_else(|| format!("`{}` names no member", request.member))?;
    // The actor the device admitted, when it named one: an agent acting for
    // the person whose session handed it the action says so, and the receipt
    // decides the change by what that agent may do for them.
    let acting = match &request.acting {
        Some(acting) => serde_json::from_value::<Acting>(acting.clone())
            .map_err(|error| format!("who acts: {error}"))?,
        None => Acting::member(member.clone()),
    };
    Ok(Invocation { member, acting })
}

/// The answer to an applied action: the envelope a peer replays, shared by
/// every resource.
#[inline(never)]
fn applied(envelope: &ActionEnvelope) -> Result<Answer, String> {
    let envelope =
        serde_json::to_value(envelope).map_err(|error| format!("the invocation: {error}"))?;
    Ok(Answer {
        envelope: Some(envelope),
        ..Answer::default()
    })
}

/// One action, applied as an invocation of its actor against the document
/// the device gave: what the envelope names makes the change the same for
/// every holder.
fn apply<R: Resource>(request: &Request) -> Result<Answer, String> {
    let Invocation { member, acting } = invocation(request)?;
    let action =
        tilo_resource::build_action::<R>(&request.action, &request.arguments, request.now)?;
    let encoded = tilo_resource::encode::<R>(&action)
        .map_err(|error| format!("{}: {error}", request.action))?;
    let mut replica = tilo_resource::replica::Replica::<R>::from_doc(device_document()?);
    let heads = replica.doc_mut().get_heads();
    let context = ActionContext::for_resource(R::KIND, R::KIND, R::label())
        .with_app(R::APP_ID, R::label())
        .with_invoker(ActionInvoker::admitted(&acting))
        .with_change(action.change_kind());
    let envelope = ActionEnvelope {
        id: invocation_id(&request.id, &member, &encoded, &heads, request.now),
        action: encoded,
        base_heads: heads.iter().map(|head| head.0).collect(),
        timestamp: request.now / 1_000,
        locus: action.locus(),
        actor: Some(member.actor_bytes()),
        context: Some(context),
    };
    replica
        .replay(&envelope)
        .map_err(|error| format!("{}: {error}", request.action))?;
    applied(&envelope)
}

/// An invocation another holder recorded, replayed against the device's
/// document through the resource's own verbs: at the envelope's base heads it
/// makes the same change, which is how the holder that receives it checks it.
fn replay<R: Resource>(request: &Request) -> Result<Answer, String> {
    let envelope: ActionEnvelope = request
        .envelope
        .clone()
        .ok_or_else(|| "a replay names no invocation".to_owned())
        .and_then(|envelope| {
            serde_json::from_value(envelope).map_err(|error| format!("the invocation: {error}"))
        })?;
    let mut replica = tilo_resource::replica::Replica::<R>::from_doc(device_document()?);
    replica
        .replay(&envelope)
        .map_err(|error| format!("replaying the invocation: {error}"))?;
    Ok(Answer::default())
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

/// The invocation's id: a function of what the request asks and the resource
/// it asks it of, so every holder making the same request mints the same one
/// (its bytes are also the change's actor when no member is named).
fn invocation_id(
    id: &str,
    member: &MemberId,
    encoded: &tilo_actions::EncodedAction,
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
/// `wasm32-unknown-unknown` has nothing to ask, and needs nothing. Every id it
/// writes comes from the request, and every actor is the device's or the
/// invocation's. So this fills zeros and says it worked; a zero draw is a
/// value, and no change depends on it. Built with
/// `--cfg getrandom_backend="custom"`, which is what makes getrandom compile
/// into a module that links no browser glue.
///
/// # Safety
/// Called by getrandom with a writable buffer of `len` bytes.
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
