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
//! its [`Refusal`] — with `answer` or `fail`. Everything crosses as JSON in the module's own
//! memory: the host never sees a Rust type of the model's, and the model never sees a key, a
//! transport or a file.
//!
//! # Determinism
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
//!
//! ```ignore
//! tilo_model_guest::serve_record!(Tracker, record_tasks_tracker);
//! ```

use base64::Engine as _;
use serde_json::Value as Json;
use tilo_actions::{
    ActionAddress, ActionContext, ActionInvoker, ActionRegistry, ActionSpec, Availability,
    DoorError, ParamType, ResourceSpec,
};
use tilo_model_abi::{
    ActionNode, Answer, Asked, BUFFER_START, NounNode, Op, ParamNode, Refusal, RefusalKind,
    Request, ResourceNode, StateNode, TypeNode,
};
use tilo_resource::{Doc, Resource, ResourceKind};

pub use tilo_model_abi as abi;

/// `error` as the refusal the host reports.
fn refused_model(error: impl std::fmt::Display) -> Refusal {
    Refusal {
        kind: RefusalKind::Model,
        message: error.to_string(),
    }
}

/// `error` as the door's refusal the host reports.
fn refused_door(error: impl std::fmt::Display) -> Refusal {
    Refusal {
        kind: RefusalKind::Door,
        message: error.to_string(),
    }
}

/// Run `request` against the model `R` declares, and answer it.
///
/// # Errors
///
/// A [`Refusal`] when the model or the door refused; the host reports it as the call's
/// failure and keeps no change.
pub fn serve<R: Resource>(request: &Request) -> Result<Answer, Refusal> {
    // The module has no clock: the time a change is made at is the host's, read once for the
    // invocation and carried by the request. Both readings are pinned to it — the resource
    // layer's origin clock, which stamps what a verb builds, and the wall clock itself, which
    // stamps the change the envelope records — so nothing here asks the machine what time it
    // is. A bare wasm module has no clock to ask, and the answer has to be the host's anyway
    // for a replay to reproduce the change.
    tilo_resource::with_origin_clock(request.now_ms, || {
        tilo_clock::with_wall_ms(u64::try_from(request.now_ms).unwrap_or(0), || {
            serve_at::<R>(request)
        })
    })
}

fn serve_at<R: Resource>(request: &Request) -> Result<Answer, Refusal> {
    let kind = ResourceKind::of::<R>();
    if kind.kind() != request.kind {
        return Err(Refusal {
            kind: RefusalKind::Unknown,
            message: format!("this module serves {}, not {}", kind.kind(), request.kind),
        });
    }
    let mut doc = match &request.document {
        Some(text) => Doc::load(&decode(text)?).map_err(refused_model)?,
        None => Doc::new(),
    };
    let mut answer = Answer::default();
    match request.op {
        Op::Open => {
            answer.created = open(&kind, &mut doc)?;
            answer.offers = Some(tree(&kind, &request.address, &doc)?);
            answer.reading = Some(reading(&kind, request, &doc)?);
        }
        Op::Migrate => {
            if kind.needs_migrate(&doc) {
                kind.migrate(&mut doc).map_err(refused_model)?;
                doc.commit();
            }
        }
        Op::Read => {
            answer.reading = Some(reading(&kind, request, &doc)?);
        }
        Op::Invoke => {
            let asked = request.action.as_ref().ok_or_else(|| Refusal {
                kind: RefusalKind::Unknown,
                message: "an invocation names no action".to_owned(),
            })?;
            invoke(&kind, request, &mut doc, asked)?;
            answer.offers = Some(tree(&kind, &request.address, &doc)?);
            answer.reading = Some(reading(&kind, request, &doc)?);
        }
    }
    answer.document = Some(encode(&doc.save()));
    Ok(answer)
}

/// Open the record `kind` keeps in `doc`, writing its genesis when it holds none and
/// migrating one from before the model as the model asks. Answers whether it was made.
fn open(kind: &ResourceKind, doc: &mut Doc) -> Result<bool, Refusal> {
    if !kind.holds_or_older(doc) {
        kind.initialize(doc).map_err(refused_model)?;
        doc.commit();
        return Ok(true);
    }
    if kind.needs_migrate(doc) {
        kind.migrate(doc).map_err(refused_model)?;
        doc.commit();
    }
    // A record the model cannot read even after its migration is refused here, rather than
    // answered as current state.
    kind.read(doc).map_err(refused_model)?;
    Ok(false)
}

/// What the record `address` offers, as the tree the host publishes: the resource, its
/// actions, and each item it collects with its own.
fn tree(kind: &ResourceKind, address: &str, doc: &Doc) -> Result<ResourceNode, Refusal> {
    let spec = kind.publish(address, doc).map_err(refused_model)?;
    Ok(node(&spec))
}

/// `spec` as data, with each item's tree beneath it.
fn node(spec: &ResourceSpec) -> ResourceNode {
    ResourceNode {
        id: spec.id.as_str().to_owned(),
        kind: spec.kind.clone(),
        label: spec.label.clone(),
        actions: spec.actions.iter().map(action_node).collect(),
        children: spec.children.iter().map(node).collect(),
        resource_type: spec.resource_type.as_ref().map(|what| TypeNode {
            noun: NounNode {
                one: what.noun.one.clone(),
                many: what.noun.many.clone(),
            },
            app: NounNode {
                one: what.app.one.clone(),
                many: what.app.many.clone(),
            },
            root: what.root,
        }),
    }
}

/// `spec` as data: what a caller may invoke, and nothing it can run.
fn action_node(spec: &ActionSpec) -> ActionNode {
    ActionNode {
        id: spec.id.clone(),
        label: spec.label.clone(),
        keywords: spec.keywords.clone(),
        icon: spec.icon.clone(),
        description: spec.description.clone(),
        params: spec.params().iter().map(param_node).collect(),
        state: (!spec.state.is_empty()).then(|| StateNode {
            badge: spec.state.badge.clone(),
            toggles: spec.state.toggles,
        }),
        unavailable: match &spec.contract.availability {
            Availability::Available => None,
            Availability::Unavailable { code, reason } => Some(format!("{code}: {reason}")),
        },
        // What the door says taking it away is: what the action declared.
        destructive: !matches!(
            spec.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ),
        audience: None,
    }
}

/// One parameter as data.
fn param_node(param: &tilo_actions::ActionParam) -> ParamNode {
    ParamNode {
        name: param.name.to_string(),
        kind: param_type(param.ty).to_owned(),
        description: Some(param.description.to_string()).filter(|text| !text.is_empty()),
        optional: !param.required,
    }
}

/// The wire name of `ty`.
fn param_type(ty: ParamType) -> &'static str {
    ty.label()
}

/// The reading of the record, or of the item `request.item` names: the record is never handed
/// over.
fn reading(kind: &ResourceKind, request: &Request, doc: &Doc) -> Result<Json, Refusal> {
    match &request.item {
        Some(item) => kind.content(doc, Some(item)).map_err(|error| Refusal {
            kind: RefusalKind::Model,
            message: error.to_string(),
        }),
        None => {
            let state = kind.read(doc).map_err(refused_model)?;
            let mut reading = kind.to_json(state.as_ref()).map_err(refused_model)?;
            let spec = kind.publish(&request.address, doc).map_err(refused_model)?;
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
) -> Result<(), Refusal> {
    let tree_spec = kind.publish(&request.address, doc).map_err(refused_model)?;
    let address = ActionAddress::new(request.address.as_str(), asked.action.as_str());
    // An action about one of the record's items is offered by the item's own tree; one about
    // the record is the record's.
    let known = if request.record {
        tree_spec
    } else {
        kind.child(doc, &request.address)
            .map_err(refused_model)?
            .ok_or_else(|| refused_door(DoorError::Unknown(address.clone())))?
    };
    let encoded = tilo_actions::door::encode_json(&[known], &address, &asked.arguments)
        .map_err(refused_door)?;
    let invoker = request_invoker(request);
    let context = ActionContext::for_resource(&request.address, &asked.action, &asked.action)
        .with_invoker(invoker.clone());
    let mut registry = ActionRegistry::new();
    kind.register(&mut registry);
    let action = registry.decode(&encoded).map_err(refused_door)?;
    tilo_actions::apply_local_with_context(
        doc,
        action.as_ref(),
        encoded,
        writer(&invoker),
        context,
    )
    .map_err(refused_door)?;
    doc.commit();
    Ok(())
}

/// The invoker the host says is making this call: the person, an agent acting for them, or
/// the app. The module never chooses it.
fn request_invoker(request: &Request) -> ActionInvoker {
    let actor = request
        .invoker
        .clone()
        .unwrap_or_else(|| "the app".to_owned());
    match &request.on_behalf_of {
        Some(person) => ActionInvoker::actor(actor).acting_for(person.clone()),
        None => ActionInvoker::actor(actor),
    }
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
fn decode(text: &str) -> Result<Vec<u8>, Refusal> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| Refusal {
            kind: RefusalKind::Model,
            message: format!("the record's document is not base64: {error}"),
        })
}

/// Serve `R`'s records for the call the host made: pull the request, run the model, publish
/// the answer.
pub fn serve_export<R: Resource>() {
    // A panic in the model would trap the module with no word of why; the host's log has one.
    std::panic::set_hook(Box::new(|panic| {
        let message = format!("the model panicked: {panic}");
        unsafe { host::log(message.as_ptr(), message.len()) };
    }));
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
/// `ask` is given a pointer and a capacity and answers how many bytes it wrote, or the
/// negated size it needs when the capacity was too small.
pub fn pull(mut ask: impl FnMut(*mut u8, usize) -> i32) -> Vec<u8> {
    let mut buffer = vec![0_u8; BUFFER_START];
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
            Err(getrandom::Error::new_custom(1))
        }
    }
}
