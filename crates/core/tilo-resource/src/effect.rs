//! Effects: work an app does that changes no document — reloading a list,
//! running a job, talking to a server, opening a system picker.
//!
//! An app names its effects as one typed enum and derives [`Effect`]:
//!
//! ```
//! use tilo_resource::Effect;
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Effect)]
//! #[effect(operation = "example.backups")]
//! pub enum Backups {
//!     /// Back up now.
//!     #[effect(kind = "outside")]
//!     #[effect(job, cancellable)]
//!     BackUp,
//!     /// Restore a snapshot.
//!     #[effect(kind = "outside")]
//!     #[effect(job, destructive)]
//!     Restore {
//!         #[param("Id of the snapshot")]
//!         snapshot: String,
//!     },
//! }
//!
//! let restore = Backups::Restore { snapshot: "monday".into() };
//! assert_eq!(restore.id(), "restore");
//! let decoded = Backups::decode("restore", &restore.arguments()).unwrap();
//! assert!(matches!(decoded, Backups::Restore { snapshot } if snapshot == "monday"));
//! ```
//!
//! Each variant is one published action of kind `Effect`, with typed
//! parameters, so the palette, agents and scripts invoke it like the app's
//! buttons do. Unlike a resource's `Action`, an effect is never replayed: it
//! runs where it was invoked, and what it changes in the document it changes
//! through ordinary actions. `tilo-app` runs the handler.
//!
//! A parameter naming a file or folder on this device is an
//! [`AbsolutePath`](crate::AbsolutePath): a relative one is refused as the
//! arguments are decoded, so no effect writes where its process happens to
//! run.
//!
//! A variant that only changes what this presence sees — open a snippet,
//! reveal a tag, show a result — is a `view`: published as a `View`
//! operation (any reader may invoke it, a member may not move someone
//! else's view, repeating it is harmless), answered by the same handler.
//!
//! Who may run an effect is decided by its kind alone, like any action: an
//! effect that reaches the files, hardware or services of the device it
//! runs on — set this computer up, kill a process here, read a local
//! folder — reaches outside the record, so the person's Allow, Ask first or
//! Deny for outside effects decides it.
//!
//! An effect whose whole scope is the invoking person's own device —
//! copy to their clipboard, apply their own appearance, measure their own
//! connection — and that touches nothing shared and no one else is
//! `own_device`: a workspace's grants have nothing to consent to, so a
//! guest invokes it as the workspace's owner does, and it runs only for the
//! person at the device it acts on ([`tilo_actions::OWN_DEVICE_SCOPE`]). An
//! effect that reaches other people (a direct message, a call) is not.
//!
//! An effect that needs its person attending — it opens a chooser for
//! them, asks them for a credential or a sign-in, or shows something only
//! they may see — is `attended`: whoever acts for the person (an agent, a
//! bot) is always asked first, and no answer allows it unasked
//! ([`tilo_actions::ATTENDED_TAG`]).
//!
//! An effect that acts on one item — open *this* snippet, connect *this*
//! connection — is offered on the item itself with its arguments bound
//! ([`Effect::bound`]): a published child or a live row carries it, and
//! invoking it there reaches the handler as that value. Such an effect is
//! often `row`: offered only bound, never on the resource with an id for
//! the caller to type.
//!
//! An effect's label is a key of the catalogue beside its crate
//! (`locales/en-US.ftl`): `<operation>-<id>` with the operation's separators
//! as dashes (`notes-effect-export` for `export` of `notes.effect`), or the
//! key `label = "…"` names where the crate already says the same thing. Its
//! doc comment is for developers and describes it to agents; it is never the
//! label.
//!
//! Variant options: `label = "key"`, `view` (changes
//! only what is shown; answers at once), `row` (offered only bound to the
//! items it acts on), `job` (runs in the background and reports progress,
//! then its result), `cancellable` (a job the caller may cancel), `returns`
//! (answers with a JSON value), `content` (answers with content another
//! app's operation takes: bytes in this device's content store, named by
//! their hash as a `tilo.content` resource; an operation that takes content
//! collects it with `ActionSpec::prompted_content`, so a caller chains the
//! two by type), `destructive` (asks for confirmation),
//! `risk = "path"` (a `fn(&Self) -> RiskLevel` classifying one invocation:
//! one it finds destructive asks for confirmation, so formatting the system
//! disk can, and another disk need not), `phrase = "path"` (a
//! `fn(&Self) -> String` naming what one bound invocation of a `destructive`
//! `row` effect loses, such as the drive it erases: its caller types it to
//! confirm, [`Effect::bound`] asks for it), `privileged` (hands out or shows a
//! credential, such as copying a one-time
//! code: asks for confirmation too), `sensitive` (discloses a copy of private
//! data, such as exporting an address book: asks for confirmation too), `idempotent` (safe to repeat after an
//! uncertain outcome), `own_device` (its scope is the invoker's own device),
//! `attended` (needs its person attending: anyone acting for them asks),
//! `audience = "agents"` (listed for agents and programs, never in the
//! palette; `"people"` lists it for people only),
//! `untrusted` (answers with content from outside the user's data, such as a
//! web page's text: the caller reasons about it and never follows it),
//! `opens_path` (opens the file its `path` names in this instance, which is
//! how an app launched with a file opens it; `opens_path = "csv,tsv"` only
//! those types),
//! `presence` (a view that moves only the invoking presence's own
//! view, such as an agent's caret, so a remote presence may invoke it),
//! `member` (any member of a workspace may ask the workspace's host to run it
//! for themselves on the workspace's record, as linking their own account
//! elsewhere),
//! `host_render` (a host answers it in a render service of its own, never in
//! the record's confined app worker: [`tilo_actions::HOST_RENDER_TAG`]),
//! `tag = EXPR` (repeatable: a further contract tag, a `&'static str`
//! constant, for how a host treats the effect),
//! `outside_with = "path"` (an effect of `kind = "read"` that reaches outside
//! the record only when that parameter is given: an export that writes the
//! file it names, else keeps what it made as content; each invocation is
//! admitted as what it does),
//! `item = field` (offered on each item of the collection, its first
//! parameter bound to the item's id, beside the item's own verbs) or
//! `on = RowType` (offered on each row of a `#[derive(Row)]` type, its first
//! parameter bound to the row's id; `LiveRow::typed` publishes them; write it
//! more than once to offer the effect on several types, and leave the effect
//! without parameters to offer it on the type as it is)
//! and
//! `kind = "read"` (an effect that only reads, which whoever may read may
//! invoke), `kind = "undoable"` (an undoable change) or `kind = "outside"`
//! (a change checked against the caller's grants: every effect states its
//! kind, while a `view` reads and an `edit` is undoable),
//! and `offload(cpu_ms = N, memory_mb = N,
//! output_mb = N, gpu)` (a `content`, `kind = "read"` transform of content
//! inputs whose outputs depend on the inputs alone: the system may dispatch it
//! to the invoker's best-placed device, where the action runs through that
//! device's provider, and the handler is its local run; [`tilo_actions::Offload`]). Field
//! `#[param("…")]` describes a parameter; `#[param(secret, "…")]` makes it a
//! secret the host keeps out of logs, history and confirmations;
//! `#[param(decimal, "…")]` makes it an identity that travels as canonical
//! decimal text and is refused otherwise; and `#[param("…", choices = EXPR)]`
//! names the values it takes when they are a fixed list (`EXPR` yields
//! strings: a constant table, an array of names). `#[param(label = "key")]`
//! takes the description from the vocabulary's catalogue instead, read in the
//! person's language where the action is published.

use std::sync::Arc;

use tilo_actions::{
    ActionAddress, ActionContract, ActionKind, ActionParam, ActionSpec, ActionType, ApiError,
    ConfirmationPolicy, EncodedAction, ExecutionMode, OperationKind, ResourceSpec, ResultSpec,
    RetryPolicy, RiskLevel, Target,
};

/// `resource` offering every action of the vocabulary `E`: a device's
/// readings and the desktop's verbs are published this way.
///
/// It sits beside [`Effect::specs`] so that a crate publishing a vocabulary
/// needs only the vocabulary, not the service host that runs one.
#[must_use]
pub fn offer<E: Effect>(mut resource: ResourceSpec) -> ResourceSpec {
    resource.actions.extend(E::specs());
    resource
}

/// The effect of `E` that invoking `action` of `resource` with `arguments`
/// names, once the spec `published` offers for it checked the arguments
/// and that it is available now. A spec offered bound to an item (a
/// network, an accessory) adds the item's arguments.
///
/// It sits beside [`offer`] for the same reason: whatever answers a
/// vocabulary's actions decodes them without the service host.
///
/// # Errors
/// [`ApiError::Unknown`] when nothing published names it,
/// [`ApiError::Unavailable`], or the arguments' refusal.
pub fn decode<E: Effect>(
    published: &[ResourceSpec],
    resource: &str,
    action: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<E, ApiError> {
    let address = ActionAddress::new(resource, action);
    let (_, spec) = tilo_actions::api::locate(published, &address)
        .ok_or_else(|| ApiError::Unknown(address.clone()))?;
    tilo_actions::api::encode_json(published, &address, arguments)?;
    let mut all = arguments.clone();
    for (name, value) in spec.bound() {
        all.insert(name.clone(), value.clone());
    }
    E::decode(&spec.id, &all).map_err(|error| ApiError::InvalidArguments(error.to_string()))
}

/// Why a caller's invocation does not name an effect of a vocabulary.
#[derive(Debug, thiserror::Error)]
pub enum EffectDecodeError {
    /// The vocabulary offers no effect of that id.
    #[error("there is no effect `{0}`")]
    Unknown(String),
    /// The payload the invocation travels in does not decode.
    #[error("the effect's payload does not decode: {0}")]
    Payload(#[source] serde_json::Error),
    /// One argument does not have its field's type, as the field's own type
    /// says (`AbsolutePath`'s refusal of a relative path, say). `name` is
    /// its path in the invocation: a flattened struct's field by its own
    /// name (`path`), a field of one with its parent's (`adjust.grayscale`).
    #[error("the effect's `{name}` argument does not decode: {source}")]
    Argument {
        name: String,
        #[source]
        source: serde_json::Error,
    },
    /// One argument names no identity: it is not canonical decimal text.
    #[error(
        "the effect's `{name}` argument is `{value}`, which names no id: ids travel as decimal text"
    )]
    Identity { name: String, value: String },
}

/// A typed vocabulary of effects. Derive it (`#[derive(Effect)]`); see the
/// [module docs](self).
pub trait Effect: Sized + 'static {
    /// The operation type every effect of the vocabulary travels under:
    /// unique per vocabulary, like a resource's kind.
    const OPERATION: &'static str;

    /// The app whose resource offers the vocabulary's effects, which an
    /// effect's typed reference names its action by: defaults to none of its
    /// own, for a vocabulary that is not an app's (a test's, a host's).
    const APP_ID: &'static str = crate::kind::DEFAULT_APP;

    /// The vocabulary's name, as its effects are labelled when they are
    /// offered on their own.
    const LABEL: &'static str = Self::OPERATION;

    /// One declaration per effect, in order.
    fn declarations() -> Vec<EffectDecl>;

    /// The vocabulary's declarations where they are kept, for a surface that
    /// publishes an app without running it: every area's, for a vocabulary
    /// that gathers several.
    fn rows() -> Vec<EffectRow>;

    /// The declaration of the effect `id`, built alone: what binding one
    /// effect to an item reads, so offering it on many items costs one
    /// declaration each, not the whole vocabulary.
    fn declaration(id: &str) -> Option<EffectDecl> {
        first_declaration(Self::declarations(), id)
    }

    /// This effect's id among the declarations.
    fn id(&self) -> &'static str;

    /// What its caller types to confirm this invocation, when its effect
    /// declares `phrase`: the name of what it erases.
    fn confirmation_phrase(&self) -> Option<String> {
        None
    }

    /// The kind the vocabulary's effects are published under, which a typed
    /// reference to one names: the operation they travel as, the vocabulary's
    /// name, and the app that offers it.
    fn kind() -> crate::kind::ResourceKind {
        crate::kind::ResourceKind::of_effect::<Self>(Self::OPERATION, Self::LABEL, Self::APP_ID)
    }

    /// The key this effect is offered under: the app that offers the
    /// vocabulary, the operation its effects travel as, and this effect's id.
    fn key(&self) -> tilo_actions::ActionKey {
        tilo_actions::ActionKey::new(Self::APP_ID, Self::OPERATION, self.id())
    }

    /// The spec `id` declares: what a control binding it reads, so binding one
    /// effect costs one declaration rather than the vocabulary's.
    fn spec(id: &str) -> Option<tilo_actions::ActionSpec> {
        Self::declaration(id).map(|decl| decl.spec_checked(Self::OPERATION, check::<Self>))
    }

    /// This effect's arguments, named as its declaration's parameters.
    fn arguments(&self) -> serde_json::Map<String, serde_json::Value>;

    /// The effect `id` with `arguments`, as a caller invoked it.
    fn decode(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, EffectDecodeError>;

    /// The published action of every effect offered on the resource: all
    /// but the `row` ones, which are offered only [bound](Self::bound).
    fn specs() -> Vec<ActionSpec> {
        kept_specs(
            check::<Self>,
            std::any::TypeId::of::<Self>(),
            Self::OPERATION,
            Self::declarations,
        )
    }

    /// This effect with its arguments as one action that takes none: what a
    /// published child or a live row offers (open *this* snippet, stop
    /// *this* process), answered by the vocabulary's handler like any other
    /// invocation of the effect, under the same contract as its
    /// declaration.
    fn bound(&self) -> ActionSpec {
        let id = self.id();
        let arguments = self.arguments();
        let spec = kept_type((std::any::TypeId::of::<Self>(), id, None), || {
            bound_type(check::<Self>, Self::OPERATION, id, Self::declaration(id))
        })
        .bind(Target::new(arguments.clone()))
        .with_bound(arguments.into_iter().collect());
        asking_phrase(spec, self.confirmation_phrase())
    }

    /// This effect bound to the item it acts on, as [`bound`](Self::bound),
    /// still asking its caller for the arguments named in `asking`: rename
    /// *this* file, to a name the caller gives. The bound arguments are this
    /// effect's own; an asked one the caller supplies replaces it.
    fn bound_asking(&self, asking: &[&str]) -> ActionSpec {
        let params = Self::declaration(self.id())
            .map(|decl| {
                decl.params
                    .into_iter()
                    .filter(|param| asking.contains(&&*param.name))
                    .collect()
            })
            .unwrap_or_default();
        self.bound_asking_params(params)
    }

    /// [`bound_asking`](Self::bound_asking) with the asked parameters given
    /// whole, narrowed for this item: the applications that can open *this*
    /// file as the choices of `app`.
    fn bound_asking_params(&self, params: Vec<ActionParam>) -> ActionSpec {
        let id = self.id();
        match Self::declaration(id) {
            Some(decl) => asking_phrase(
                asking_spec(
                    check::<Self>,
                    Self::OPERATION,
                    &decl,
                    params,
                    self.arguments(),
                ),
                self.confirmation_phrase(),
            ),
            None => self.bound(),
        }
    }

    /// The effects offered on the item `item` of the collection `field`
    /// (`#[effect(item = field)]`), each with its first parameter bound to
    /// the item and the rest still asked.
    fn item_specs(field: &str, item: &crate::Id) -> Vec<ActionSpec> {
        target_specs(
            check::<Self>,
            std::any::TypeId::of::<Self>(),
            Self::OPERATION,
            Self::declarations,
            &|target| target.is_field(field),
            item,
        )
    }

    /// The effects offered on the row `row` of the
    /// [`RowType`](crate::words::RowType) `kind` (`#[effect(on = Row)]`),
    /// each with its first parameter bound to the row and the rest still
    /// asked. [`RowType::effects`](crate::words::RowType::effects) calls it.
    fn row_specs(kind: &'static str, row: &crate::Id) -> Vec<ActionSpec> {
        target_specs(
            check::<Self>,
            std::any::TypeId::of::<Self>(),
            Self::OPERATION,
            Self::declarations,
            &|target| target.is_row(kind),
            row,
        )
    }

    /// The effects offered on the row `row` of the
    /// [`RowType`](crate::words::RowType) `kind`, those `bindings` names
    /// among them, each with exactly the arguments they give bound and the
    /// rest still asked. [`RowType::effects_with`](crate::words::RowType::effects_with)
    /// calls it.
    fn row_specs_with(
        kind: &'static str,
        _row: &crate::Id,
        bindings: &crate::row::RowBindings,
    ) -> Vec<ActionSpec> {
        target_specs_with(
            check::<Self>,
            Self::OPERATION,
            Self::declarations,
            &|target| target.is_row(kind),
            bindings,
        )
    }

    /// This effect as the published resource `resource` offers it (the
    /// record, or a child or live row it is bound on, by its published id):
    /// what a search hit invokes or a caller addresses through the host's
    /// API. Its id comes from the effect itself.
    fn address_on(&self, resource: impl Into<String>) -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(resource, self.id())
    }

    /// Decode one invocation's payload, as the operation's specs encode it.
    ///
    /// Read where the effect runs, as an action's arguments are read where
    /// it is invoked: an effect is never replayed, so content its arguments
    /// name — a reference, or the data inline — is read from this window's
    /// store and kept, sealed, in it ([`crate::content::invoking`]).
    fn from_payload(payload: &[u8]) -> Result<Self, EffectDecodeError> {
        crate::content::invoking(|| {
            let Payload { effect, args } = decode_payload(payload)?;
            Self::decode(&effect, &args)
        })
    }
}

/// `spec`, asking its caller to type `phrase` when there is one. Not
/// generic, so every vocabulary shares it.
#[inline(never)]
fn asking_phrase(mut spec: ActionSpec, phrase: Option<String>) -> ActionSpec {
    if let Some(phrase) = phrase {
        spec.type_mut().contract.confirmation = ConfirmationPolicy::Phrase { phrase };
    }
    spec
}

/// How the effect's specs carry an invocation: its id and its arguments.
#[derive(serde::Serialize, serde::Deserialize)]
struct Payload {
    effect: String,
    #[serde(default)]
    args: serde_json::Map<String, serde_json::Value>,
}

/// The [`Payload`] in `payload`. Not generic over the effect, so its decoder
/// is compiled once rather than in every app's `from_payload`.
#[inline(never)]
fn decode_payload(payload: &[u8]) -> Result<Payload, EffectDecodeError> {
    tilo_actions::decode_json(payload).map_err(EffectDecodeError::Payload)
}

/// How an encoded invocation's arguments are made of the caller's and the
/// ones bound to the item the effect was offered on.
#[derive(Clone, Copy)]
enum Bound {
    /// The caller's alone.
    None,
    /// The bound ones alone: the spec asks for nothing.
    Only,
    /// The bound ones, any the caller gives replacing them.
    UnderCaller,
    /// The caller's, the bound ones replacing them.
    OverCaller,
}

/// The arguments an effect spec's [`Target`] binds: the ones of the item it
/// was offered on.
type BoundArguments = serde_json::Map<String, serde_json::Value>;

/// Why the effect `id` of a vocabulary does not take `arguments`, as the
/// vocabulary reads them back; `None` when it does.
type Check = fn(&str, &BoundArguments) -> Option<String>;

/// [`Check`] of the vocabulary `E`: the only part of its specs compiled for it.
fn check<E: Effect>(id: &str, arguments: &BoundArguments) -> Option<String> {
    E::decode(id, arguments)
        .err()
        .map(|problem| problem.to_string())
}

/// The one encoder of every effect spec of a vocabulary, which `check`s: the invocation
/// of effect `effect` under `operation`, with its arguments made as `mode`
/// says of the ones its target binds ([`BoundArguments`]) and the caller's.
///
/// The arguments are read back as `E` before they are encoded, so a client
/// that names an id or a value the effect does not take is refused at the
/// API, [`INVALID_ARGUMENTS_TYPE`](tilo_actions::resource::INVALID_ARGUMENTS_TYPE)
/// with why, rather than handed to a host that cannot run it.
fn encoder(
    check: Check,
    operation: &'static str,
    effect: &'static str,
    mode: Bound,
) -> impl Fn(&Target, &tilo_actions::ActionArgs) -> EncodedAction + Send + Sync + 'static {
    move |target, args| {
        let args = effect_arguments(mode, target, args);
        if let Some(problem) = check(effect, &args) {
            return EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: problem.into_bytes(),
            };
        }
        EncodedAction::encode_operation(
            operation,
            &Payload {
                effect: effect.to_owned(),
                args,
            },
        )
        .unwrap_or_else(|_| EncodedAction {
            type_id: operation.to_owned(),
            payload: Vec::new(),
        })
    }
}

/// The arguments an invocation of an effect offered with `mode` carries: the
/// ones its target binds, the caller's, or both, as [`Bound`] says.
fn effect_arguments(
    mode: Bound,
    target: &Target,
    args: &tilo_actions::ActionArgs,
) -> BoundArguments {
    let empty = BoundArguments::new();
    let bound = target.get::<BoundArguments>().unwrap_or(&empty);
    match mode {
        Bound::None => args.to_json(),
        Bound::Only => bound.clone(),
        Bound::UnderCaller => {
            let mut merged = bound.clone();
            merged.extend(args.to_json());
            merged
        }
        Bound::OverCaller => {
            let mut merged = args.to_json();
            merged.extend(
                bound
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
            merged
        }
    }
}

/// [`Effect::specs`] of the vocabulary `key`. They depend on its type alone
/// and are published on every refresh of a view's actions, so each thread
/// builds them once per generation of app code
/// ([`reset_type_schemas`](crate::reset_type_schemas)) and hands out copies.
fn kept_specs(
    check: Check,
    key: std::any::TypeId,
    operation: &'static str,
    declarations: fn() -> Vec<EffectDecl>,
) -> Vec<ActionSpec> {
    type Kept = (
        u64,
        std::collections::HashMap<std::any::TypeId, std::rc::Rc<[ActionSpec]>>,
    );
    thread_local! {
        static KEPT: std::cell::RefCell<Kept> =
            std::cell::RefCell::new((0, std::collections::HashMap::new()));
    }
    let epoch = crate::resource::words_epoch();
    let found = KEPT.with(|kept| {
        let kept = kept.borrow();
        if kept.0 == epoch {
            kept.1.get(&key).cloned()
        } else {
            None
        }
    });
    let specs = found.unwrap_or_else(|| {
        let built: std::rc::Rc<[ActionSpec]> =
            offered_specs(check, declarations(), operation).into();
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.0 != epoch {
                *kept = (epoch, std::collections::HashMap::new());
            }
            kept.1.insert(key, std::rc::Rc::clone(&built));
        });
        built
    });
    specs.to_vec()
}

/// The declaration `id` of `declarations`, as [`Effect::declaration`] reads
/// one. Not generic over the vocabulary, so the search is compiled once rather
/// than in every app that declares effects.
#[inline(never)]
fn first_declaration(declarations: Vec<EffectDecl>, id: &str) -> Option<EffectDecl> {
    declarations.into_iter().find(|decl| decl.id == id)
}

fn offered_specs(check: Check, decls: Vec<EffectDecl>, operation: &'static str) -> Vec<ActionSpec> {
    // Offered on an item or a row (`item = field`, `on = Row`) as well as
    // here, where it acts on the item a caller names: its audience is the
    // declaration's, and it says it needs an item, so a listing holding none
    // leaves it to the items that offer it bound.
    decls
        .into_iter()
        .filter(|decl| !decl.row)
        .map(|decl| {
            let mut spec = decl.spec_checked(operation, check);
            if let Some(target) = decl.items.first() {
                spec.type_mut().contract.item = Some(target.key().to_owned());
            }
            spec
        })
        .collect()
}

/// The type of [`Effect::bound`] of the effect `id`, declared as `decl`: its
/// target binds every argument.
fn bound_type(
    check: Check,
    operation: &'static str,
    id: &'static str,
    decl: Option<EffectDecl>,
) -> Arc<ActionType> {
    let build = encoder(check, operation, id, Bound::Only);
    let spec = match decl {
        Some(decl) => decl.spec_with_boxed(operation, Vec::new(), Arc::new(build)),
        None => ActionSpec::typed_operation_on(
            id,
            id,
            id,
            OperationKind::Effect,
            operation,
            Vec::new(),
            Arc::new(build),
        ),
    };
    Arc::clone(spec.action_type())
}

/// [`Effect::bound_asking_params`] of the effect `decl` declares, bound to
/// `args`, asking `params`.
fn asking_spec(
    check: Check,
    operation: &'static str,
    decl: &EffectDecl,
    params: Vec<ActionParam>,
    args: BoundArguments,
) -> ActionSpec {
    let fixed = args
        .iter()
        .filter(|(name, _)| !params.iter().any(|param| param.name == name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let build = encoder(check, operation, decl.id, Bound::UnderCaller);
    decl.spec_with_boxed(operation, params, Arc::new(build))
        .action_type()
        .bind(Target::new(args))
        .with_bound(fixed)
}

/// A copy of `params`, made by one function for every effect vocabulary.
#[inline(never)]
fn params_from(params: &[ActionParam]) -> Vec<ActionParam> {
    params.to_vec()
}

/// [`Effect::item_specs`] and [`Effect::row_specs`] of the vocabulary `key`,
/// whose declarations `declarations` are: every effect `matches` offers on an
/// entity, its first parameter bound here to `entity` — or, for an effect
/// with no parameters, offered as it is, since the entity changes nothing
/// about it. Each type is kept per target, so offering the same effect on
/// many items costs one type.
fn target_specs(
    check: Check,
    key: std::any::TypeId,
    operation: &'static str,
    declarations: fn() -> Vec<EffectDecl>,
    matches: &dyn Fn(&EffectTarget) -> bool,
    entity: &crate::Id,
) -> Vec<ActionSpec> {
    declarations()
        .into_iter()
        .filter_map(|decl| {
            let target = *decl.items.iter().find(|target| matches(target))?;
            let mut refused = None;
            let bound = match decl.params.first() {
                Some(first) => BoundArguments::from_iter([(
                    first.name.to_string(),
                    match (target, decl.row_id) {
                        (EffectTarget::Row(_), Some(parse)) => match parse(entity.as_str()) {
                            Ok(value) => value,
                            // The row's id is not what the effect acts on: it
                            // is published, refused with the reason, so an
                            // agent sees why rather than a broken call.
                            Err(reason) => {
                                refused = Some(reason);
                                serde_json::Value::String(entity.as_str().to_owned())
                            }
                        },
                        _ => serde_json::Value::String(entity.as_str().to_owned()),
                    },
                )]),
                None => BoundArguments::new(),
            };
            let ty = kept_type((key, decl.id, Some(target.key())), || {
                let rest = params_from(decl.params.get(1..).unwrap_or_default());
                let build = encoder(check, operation, decl.id, Bound::OverCaller);
                Arc::clone(
                    decl.spec_with_boxed(operation, rest, Arc::new(build))
                        .action_type(),
                )
            });
            let fixed: Vec<(String, serde_json::Value)> = bound.clone().into_iter().collect();
            let arguments: serde_json::Map<String, serde_json::Value> =
                fixed.iter().cloned().collect();
            let mut spec = ty.bind(Target::new(bound)).with_bound(fixed);
            if let Some(phrase) = decl.phrase {
                spec = asking_phrase(spec, phrase(&arguments));
            }
            if let Some(reason) = refused {
                spec.type_mut().contract.availability = tilo_actions::Availability::Unavailable {
                    code: "row_id".to_owned(),
                    reason,
                };
            }
            Some(spec)
        })
        .collect()
}

/// [`Effect::row_specs_with`] of the vocabulary `key`, whose declarations
/// `declarations` are: every effect `matches` offers on an entity that
/// `bindings` names among them, with exactly the arguments it gives bound and
/// the rest asked.
fn target_specs_with(
    check: Check,
    operation: &'static str,
    declarations: fn() -> Vec<EffectDecl>,
    matches: &dyn Fn(&EffectTarget) -> bool,
    bindings: &crate::row::RowBindings,
) -> Vec<ActionSpec> {
    declarations()
        .into_iter()
        .filter(|decl| decl.items.iter().any(matches))
        .flat_map(|decl| {
            bindings
                .arguments(decl.id)
                .unwrap_or_default()
                .iter()
                .map(|given| {
                    // A parameter the row offers with its own value as the
                    // default is asked, not taken: the caller may give
                    // another, and leaving it out takes the row's value.
                    let asking = bindings.asks(decl.id);
                    let asks = |name: &str| asking.contains(&name);
                    // An argument the row binds to null is the argument left
                    // unset, as a declaration's own binding leaves an optional
                    // one: it is taken, not asked, and travels as absent.
                    let mut bound = BoundArguments::new();
                    let mut defaults: Vec<(&str, serde_json::Value)> = Vec::new();
                    let mut taken: Vec<&str> = Vec::new();
                    for (name, value) in given {
                        if !decl
                            .params
                            .iter()
                            .any(|param| param.name.as_ref() == name.as_str())
                        {
                            continue;
                        }
                        if asks(name.as_str()) {
                            defaults.push((name.as_str(), value.clone()));
                            continue;
                        }
                        taken.push(name.as_str());
                        if !value.is_null() {
                            bound.insert(name.clone(), value.clone());
                        }
                    }
                    let mut asked = decl
                        .params
                        .iter()
                        .filter(|param| !taken.contains(&param.name.as_ref()))
                        .cloned()
                        .collect::<Vec<_>>();
                    for param in &mut asked {
                        if let Some((_, value)) = defaults
                            .iter()
                            .find(|(name, _)| *name == param.name.as_ref())
                            && !value.is_null()
                        {
                            *param = param.clone().with_default(value.clone());
                        }
                        // The values the row's own state says this parameter
                        // takes (a display's modes) stand where the
                        // declaration's do.
                        if let Some(choices) = bindings.chosen(decl.id, param.name.as_ref()) {
                            param.choices = choices.iter().cloned().map(Into::into).collect();
                        }
                    }
                    let asked = params_from(&asked);
                    let mode = if asking.is_empty() {
                        Bound::OverCaller
                    } else {
                        Bound::UnderCaller
                    };
                    let build = encoder(check, operation, decl.id, mode);
                    let fixed: Vec<(String, serde_json::Value)> =
                        bound.clone().into_iter().collect();
                    // The row's own values travel in the target too, so an
                    // argument the caller leaves out takes them.
                    let mut target = bound.clone();
                    for (name, value) in &defaults {
                        target.insert((*name).to_owned(), value.clone());
                    }
                    let arguments: serde_json::Map<String, serde_json::Value> =
                        target.clone().into_iter().collect();
                    let mut spec = decl.spec_with_boxed(operation, asked, Arc::new(build));
                    if let Some(phrase) = decl.phrase {
                        spec = asking_phrase(spec, phrase(&arguments));
                    }
                    spec.action_type()
                        .bind(Target::new(target))
                        .with_bound(fixed)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Where a kept effect type is offered: the vocabulary, the effect, and for
/// an offered effect the field or row kind whose entities offer it.
type TypeKey = (std::any::TypeId, &'static str, Option<&'static str>);

/// The action type `key` names, kept once per thread and generation of app
/// code, as [`kept_specs`] keeps a vocabulary's specs: what every item
/// offering the effect shares.
fn kept_type(key: TypeKey, build: impl FnOnce() -> Arc<ActionType>) -> Arc<ActionType> {
    thread_local! {
        static KEPT: std::cell::RefCell<crate::kept::KeptTypes<TypeKey>> =
            std::cell::RefCell::default();
    }
    crate::kept::kept_type(&KEPT, key, build)
}

/// What an effect is offered on, bound to it: a collection's items, or a
/// declared row type's rows. The two are one thing — an effect offered on an
/// entity of a declared type, with the entity's id bound — declared either
/// beside the document that holds the items, or on the type of rows that live
/// outside any document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectTarget {
    /// The collection field whose items the effect is offered on
    /// (`#[effect(item = connections)]`).
    Field(&'static str),
    /// The kind of the [`RowType`](crate::words::RowType) whose rows the
    /// effect is offered on (`#[effect(on = Process)]`).
    Row(&'static str),
}

impl EffectTarget {
    /// The key the offered type is kept under: the field or the row kind.
    fn key(self) -> &'static str {
        match self {
            Self::Field(field) | Self::Row(field) => field,
        }
    }

    /// Whether this is the collection `field`.
    fn is_field(self, field: &str) -> bool {
        matches!(self, Self::Field(named) if named == field)
    }

    /// Whether this is the row type of kind `kind`.
    fn is_row(self, kind: &str) -> bool {
        matches!(self, Self::Row(named) if named == kind)
    }
}

/// One effect of a derived vocabulary, as a constant: its declaration but for
/// the parameters, which [`Self::params`] builds. `#[derive(Effect)]` writes
/// one per variant; [`EffectDecl::of_rows`] and [`EffectDecl::of_row`] read
/// them, the same code for every vocabulary.
#[doc(hidden)]
#[derive(Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property a declaration states, as the derive's options do"
)]
pub struct EffectRow {
    pub id: &'static str,
    /// What a person reads it by: a key of its crate's catalogue.
    pub label: crate::label::Word,
    /// The other words people use for it (`keywords = "…"`), comma
    /// separated: search finds it by them as by its label.
    pub keywords: &'static [&'static str],
    /// Its own picture (`icon = "…"`), by its Lucide name.
    pub icon: Option<&'static str>,
    pub description: &'static str,
    pub params: fn() -> Vec<ActionParam>,
    /// The surface types of the parameters whose types describe one, by name.
    pub param_types: fn() -> Vec<(&'static str, crate::surface::FieldTypeOf)>,
    /// The value it answers with, when the code declares one
    /// (`returns = Type`): its JSON Schema and its type.
    pub result: Option<EffectResult>,
    pub kind: OperationKind,
    pub row: bool,
    pub answers: Answers,
    pub risk: RiskLevel,
    pub idempotent: bool,
    pub action_kind: ActionKind,
    pub own_device: bool,
    pub attended: bool,
    pub risk_of: Option<fn(&tilo_actions::ActionArgs) -> RiskLevel>,
    pub audience: tilo_actions::Audience,
    pub untrusted: bool,
    pub opens: Option<&'static [&'static str]>,
    pub presence: bool,
    pub edits: bool,
    pub member: bool,
    /// The kinds of entity its rows are offered on
    /// ([`EffectTarget`]): none for an effect offered on the resource alone.
    pub items: &'static [EffectTarget],
    /// How the row id becomes the first parameter's value when the effect is
    /// offered on a row (`#[effect(on = Row)]`): the row id parsed as that
    /// field's own type (`u64` counts, an identity parses), so a row whose id
    /// is not one is offered unavailable with the reason. Absent for an
    /// effect offered on a collection's items, whose id is bound as it is.
    pub row_id: Option<RowIdOf>,
    /// The phrase its caller types for a bound invocation (offered on a row
    /// or an item), from the arguments it is bound with, when the effect
    /// declares `phrase`: `#[effect(phrase = "Self::data_name")]` names what
    /// one invocation loses wherever the effect is offered.
    pub phrase: Option<PhraseOf>,
    pub offload: Option<EffectOffload>,
    pub tags: &'static [&'static str],
    pub outside_with: Option<&'static str>,
}

/// The phrase a bound invocation's caller types, from the arguments it is
/// bound with: what one invocation loses.
pub type PhraseOf = fn(&serde_json::Map<String, serde_json::Value>) -> Option<String>;

/// An effect's row id read as the field it acts with: the value bound, or
/// why this row's id is not one.
pub type RowIdOf = fn(&str) -> Result<serde_json::Value, String>;

/// An effect's `#[effect(offload(...))]`: what the transform needs of a
/// device, so the system may dispatch it to the invoker's best-placed one
/// ([`tilo_actions::Offload`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectOffload {
    /// Whether it needs a GPU.
    pub gpu: bool,
    /// CPU time ceiling, in milliseconds.
    pub cpu_time_ms: u64,
    /// Memory ceiling, in bytes.
    pub memory_bytes: u64,
    /// Ceiling on all outputs together, in bytes.
    pub output_bytes: u64,
}

impl EffectOffload {
    /// The declaration as the action contract carries it.
    #[must_use]
    pub fn contract(&self) -> tilo_actions::Offload {
        tilo_actions::Offload {
            gpu: self.gpu,
            cpu_time_ms: self.cpu_time_ms,
            memory_bytes: self.memory_bytes,
            output_bytes: self.output_bytes,
        }
    }
}

/// The parameters of an effect that takes none.
#[doc(hidden)]
#[must_use]
pub fn no_params() -> Vec<ActionParam> {
    Vec::new()
}

/// The parameter types of an effect whose parameters describe none.
#[doc(hidden)]
#[must_use]
pub fn no_param_types() -> Vec<(&'static str, crate::surface::FieldTypeOf)> {
    Vec::new()
}

/// The value an effect answers with, as its code declares it
/// (`#[effect(returns = Type)]`): its JSON Schema, and its type with the
/// records and enums it names.
#[derive(Clone, Copy, Debug)]
pub struct EffectResult {
    /// Its schema, from [`JsonSchema`](crate::JsonSchema).
    pub schema: fn() -> serde_json::Value,
    /// Its surface type, declaring the records and enums it names.
    pub ty: Option<crate::surface::FieldTypeOf>,
}

/// The declaration of one effect: what callers see of it.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property a declaration states, as the derive's options do"
)]
pub struct EffectDecl {
    pub id: &'static str,
    /// What a person reads it by, read in their language when it is
    /// published.
    pub label: crate::label::Word,
    /// The other words people use for it (`keywords = "…"`), comma
    /// separated: search finds it by them as by its label.
    pub keywords: &'static [&'static str],
    /// Its own picture (`icon = "…"`), by its Lucide name.
    pub icon: Option<&'static str>,
    pub description: &'static str,
    pub params: Vec<ActionParam>,
    /// The surface type of each parameter whose Rust type describes one
    /// ([`JsonSchema`](crate::JsonSchema)), by name: a record or enum a
    /// parameter's wire type ([`ParamType::Object`](tilo_actions::ParamType))
    /// says nothing of, which a package of another language names.
    pub param_types: Vec<(&'static str, crate::surface::FieldTypeOf)>,
    /// The value it answers with, when the code declares one
    /// (`returns = Type`); see [`EffectResult`].
    pub result: Option<EffectResult>,
    /// `View` for an effect that only changes what its caller sees, else
    /// `Effect`.
    pub kind: OperationKind,
    /// Offered only bound to the items it acts on, not on the resource.
    pub row: bool,
    pub answers: Answers,
    /// How much harm invoking it can do: `Destructive` (deletes,
    /// overwrites, sends) and `Privileged` (hands out or shows a credential)
    /// ask for confirmation; a view is `None`.
    pub risk: RiskLevel,
    /// Safe to repeat after an uncertain outcome.
    pub idempotent: bool,
    /// What it is able to do: reach outside the record by default, read for
    /// a view or an effect declared `kind = "read"`.
    pub action_kind: ActionKind,
    /// Its whole scope is the invoking person's own device: it touches
    /// nothing shared and no one else, so a workspace's grants have nothing to
    /// consent to ([`tilo_actions::OWN_DEVICE_SCOPE`]).
    pub own_device: bool,
    /// Needs its person attending: it opens a chooser, asks for a credential
    /// or a sign-in, or shows what only they may see, so whoever acts for
    /// them is always asked first ([`tilo_actions::ATTENDED_TAG`]).
    pub attended: bool,
    /// How much harm one invocation can do, classified from its arguments
    /// (formatting the system disk, against any other): an invocation the
    /// classifier finds destructive asks for the confirmation a declared
    /// destructive effect does.
    pub risk_of: Option<fn(&tilo_actions::ActionArgs) -> RiskLevel>,
    /// Who listings offer it to (`#[effect(audience = "agents")]`): an agent
    /// reading a page's structure is never a person's palette row.
    pub audience: tilo_actions::Audience,
    /// Answers with content from outside the user's own data (a web page's
    /// text and logs): data to reason about, never instructions to follow.
    pub untrusted: bool,
    /// Opens the file its `path` names in this instance (`#[effect(opens_path)]`),
    /// of any type when empty, else of these extensions: how a caller that
    /// launched the app with a file opens it ([`tilo_actions::OPENS_PATH_TAG`]).
    pub opens: Option<&'static [&'static str]>,
    /// A view that changes only the invoking presence's own view (an
    /// agent's caret, its Find), so a remote presence may invoke it
    /// without moving anybody else's.
    pub presence: bool,
    /// A view of the invoker that submits only edits of this record (typing
    /// at the invoker's caret): its kind is Undoable, offered to whoever may
    /// edit, and the edits it submits are checked like any other.
    pub edits: bool,
    /// Any member of a workspace may ask the workspace's host to run it for
    /// themselves on the workspace's record (`#[effect(member)]`,
    /// [`tilo_actions::MEMBER_OWN_TAG`]): it runs there as that member.
    pub member: bool,
    /// What the effect is offered on, its first parameter bound to it: the
    /// collection field whose items offer it (`#[effect(item = connections)]`,
    /// published on each item resource as an item verb is), or the kind of a
    /// [`RowType`](crate::words::RowType) whose rows offer it
    /// (`#[effect(on = Process)]`, published on each of the type's rows). An
    /// effect declared on several of them (`#[effect(on = QueueItem, on =
    /// HistoryItem)]`) is offered on each, bound to the row it was offered on;
    /// one with no parameters is offered on them as it is.
    pub items: &'static [EffectTarget],
    /// How a row id becomes the first parameter's value, when the effect is
    /// offered on rows ([`EffectRow::row_id`]).
    pub row_id: Option<RowIdOf>,
    /// The phrase its caller types for a bound invocation
    /// ([`EffectRow::phrase`]).
    pub phrase: Option<PhraseOf>,
    /// A content transform the system may run on the invoker's best-placed
    /// device (`#[effect(offload(...))]`); the handler is its local run.
    pub offload: Option<EffectOffload>,
    /// Further contract tags, each naming how a host treats the effect
    /// (`#[effect(host_render)]`, `#[effect(tag = ...)]`): an agent's host
    /// routes a [`tilo_actions::HOST_RENDER_TAG`] effect to its render
    /// service rather than the app's worker.
    pub tags: &'static [&'static str],
    /// The parameter that, given, makes an invocation reach outside the
    /// record (`#[effect(kind = "read", outside_with = "path")]`): an export
    /// writing the file it names. Without it the invocation only reads and
    /// keeps what it made as content, and is admitted as a read.
    pub outside_with: Option<&'static str>,
}

/// The contract tag of an effect that submits only edits of its record
/// ([`EffectDecl::edits`]): its kind is Undoable.
pub const EDITS_TAG: &str = "submits-edits";

/// How an effect answers its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answers {
    /// At once, with nothing to report.
    Nothing,
    /// At once, with a JSON value.
    Value,
    /// At once, with content: one `tilo.content` resource another app's
    /// content-collecting operation takes.
    Content,
    /// Later: a job that reports progress, then its result.
    Later {
        cancellable: bool,
        /// The completed job returns a JSON value, independently of its job id.
        returns: bool,
    },
}

impl EffectDecl {
    /// The published action of this effect under `operation`, of the
    /// vocabulary `E` it is declared in.
    ///
    /// An effect whose fields are all optional runs when a person chooses
    /// it, asked nothing: a field left out is `None`, which the effect gives
    /// a meaning ("About"'s `open` left out switches the sheet). Asking
    /// instead put the first field's help text in the search field. One
    /// that takes an entry or content still asks for it.
    pub fn spec<E: Effect>(&self, operation: &'static str) -> ActionSpec {
        self.spec_checked(operation, check::<E>)
    }

    /// [`Self::spec`] of a vocabulary that reads arguments back with `check`.
    fn spec_checked(&self, operation: &'static str, check: Check) -> ActionSpec {
        let build = encoder(check, operation, self.id, Bound::None);
        let spec = self.spec_with_boxed(operation, params_from(&self.params), Arc::new(build));
        let asks_words = matches!(
            spec.input(),
            tilo_actions::ActionInput::Prompted {
                collects: tilo_actions::Collects::Words,
                ..
            }
        );
        if asks_words && !self.params.iter().any(|param| param.entry) {
            spec.with_immediate_per_target()
        } else {
            spec
        }
    }

    /// The published action of this effect, as the vocabulary `E` that
    /// declares it publishes it: [`Self::spec`] under `E`'s own operation.
    pub fn spec_of<E: Effect>(&self) -> ActionSpec {
        self.spec::<E>(E::OPERATION)
    }

    /// Every effect of a derived vocabulary, declared as `rows` say.
    #[doc(hidden)]
    #[must_use]
    pub fn of_rows(rows: &'static [EffectRow]) -> Vec<Self> {
        rows.iter().map(Self::of_row).collect()
    }

    /// The effect `id` of a derived vocabulary declared as `rows` say.
    #[doc(hidden)]
    #[must_use]
    pub fn find_row(rows: &'static [EffectRow], id: &str) -> Option<Self> {
        rows.iter().find(|row| row.id == id).map(Self::of_row)
    }

    pub fn of_row(row: &EffectRow) -> Self {
        Self {
            id: row.id,
            label: row.label.clone(),
            keywords: row.keywords,
            icon: row.icon,
            description: row.description,
            params: (row.params)(),
            param_types: (row.param_types)(),
            result: row.result,
            kind: row.kind,
            row: row.row,
            answers: row.answers,
            risk: row.risk,
            idempotent: row.idempotent,
            action_kind: row.action_kind,
            own_device: row.own_device,
            attended: row.attended,
            risk_of: row.risk_of,
            audience: row.audience,
            untrusted: row.untrusted,
            opens: row.opens,
            presence: row.presence,
            edits: row.edits,
            member: row.member,
            items: row.items,
            row_id: row.row_id,
            phrase: row.phrase,
            offload: row.offload,
            tags: row.tags,
            outside_with: row.outside_with,
        }
    }

    /// What an invocation of this effect answers with: the value its type
    /// describes, the content it made, or a job's result.
    fn result_contract(&self) -> ResultSpec {
        match self.answers {
            Answers::Later { returns: false, .. } => ResultSpec::Job,
            Answers::Value | Answers::Later { returns: true, .. } => ResultSpec::Json {
                schema: self
                    .result
                    .map_or_else(|| serde_json::json!({}), |result| (result.schema)()),
            },
            Answers::Content => ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
            },
            Answers::Nothing => ResultSpec::None,
        }
    }

    /// The published action of this effect, taking `params` and encoding
    /// an invocation with `build`.
    /// What kind of action the effect is: a view reads, anything else
    /// reaches outside the document.
    #[must_use]
    pub fn action_kind(&self) -> crate::ActionKind {
        if self.edits {
            crate::ActionKind::Undoable
        } else if self.kind == OperationKind::View {
            crate::ActionKind::Read
        } else {
            crate::ActionKind::Outside
        }
    }

    /// The published action of this effect, taking `params` and encoding an
    /// invocation with `build`. The builder is passed boxed: boxing at the
    /// call site keeps this out of every app's vocabulary instance.
    fn spec_with_boxed(
        &self,
        operation: &'static str,
        params: Vec<ActionParam>,
        build: tilo_actions::resource::ArgsBuilder,
    ) -> ActionSpec {
        let view = self.kind == OperationKind::View;
        let label = self.label.read();
        let params = crate::label::titled(&self.label, operation, params);
        let mut spec = ActionSpec::typed_operation_on(
            self.id,
            label.clone(),
            label,
            if view {
                OperationKind::View
            } else {
                OperationKind::Effect
            },
            operation,
            params,
            build,
        );
        if !self.description.is_empty() {
            spec = spec.with_description(self.description);
        }
        if !self.keywords.is_empty() {
            spec = spec.with_keywords(self.keywords.iter().copied());
        }
        spec.type_mut().icon = self.icon.map(str::to_owned);
        let base = if view {
            ActionContract::view()
        } else {
            ActionContract::default()
        };
        spec.type_mut().contract = ActionContract {
            kind: self.action_kind,
            risk: self.risk,
            confirmation: if matches!(
                self.risk,
                RiskLevel::Destructive | RiskLevel::Privileged | RiskLevel::Sensitive
            ) {
                ConfirmationPolicy::Confirm
            } else {
                ConfirmationPolicy::None
            },
            execution: match self.answers {
                Answers::Later { cancellable, .. } => ExecutionMode::Background { cancellable },
                Answers::Nothing | Answers::Value | Answers::Content => ExecutionMode::Immediate,
            },
            result: self.result_contract(),
            retry: if view || self.idempotent {
                RetryPolicy::Idempotent
            } else {
                base.retry
            },
            tags: {
                let mut tags = base.tags.clone();
                if self.own_device {
                    tags.push(tilo_actions::OWN_DEVICE_SCOPE.to_owned());
                }
                if self.attended {
                    tags.push(tilo_actions::ATTENDED_TAG.to_owned());
                }
                if let Some(tag) = self.audience.tag() {
                    tags.push(tag.to_owned());
                }
                if self.untrusted {
                    tags.push(tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned());
                }
                if let Some(extensions) = self.opens {
                    tags.push(tilo_actions::OPENS_PATH_TAG.to_owned());
                    tags.extend(extensions.iter().map(|extension| {
                        format!("{}{extension}", tilo_actions::OPENS_PATH_EXTENSION_PREFIX)
                    }));
                }
                if self.presence {
                    tags.push(tilo_actions::PRESENCE_VIEW_TAG.to_owned());
                }
                if self.edits {
                    tags.push(EDITS_TAG.to_owned());
                }
                if self.member {
                    tags.push(tilo_actions::MEMBER_OWN_TAG.to_owned());
                }
                tags.extend(self.tags.iter().map(|tag| (*tag).to_owned()));
                tags
            },
            offload: self.offload.as_ref().map(EffectOffload::contract),
            ..base
        };
        let spec = match self.outside_with {
            Some(param) => spec.with_need_of(outside_with(param, self.action_kind)),
            None => spec,
        };
        match self.risk_of {
            Some(classify) => spec.with_risk(classify),
            None => spec,
        }
    }

    /// Whether the effect runs in the background, reporting its result later.
    pub fn is_job(&self) -> bool {
        matches!(self.answers, Answers::Later { .. })
    }
}

/// What one invocation of an effect declared `outside_with = param` is:
/// reaching outside when `param` is given (the file an export writes), else
/// the `least` it was declared as.
fn outside_with(
    param: &'static str,
    least: ActionKind,
) -> impl Fn(&tilo_actions::ActionArgs) -> tilo_actions::ActionNeed + Send + Sync + 'static {
    move |args| {
        let given = args
            .to_json()
            .get(param)
            .is_some_and(|value| !value.is_null());
        tilo_actions::ActionNeed::of(if given { ActionKind::Outside } else { least })
    }
}

/// The vocabulary of a surface that has no effects.
#[derive(Clone, Copy, Debug)]
pub enum NoEffect {}

impl Effect for NoEffect {
    const OPERATION: &'static str = "tilo.no-effect";

    fn declarations() -> Vec<EffectDecl> {
        Vec::new()
    }

    fn rows() -> Vec<EffectRow> {
        Vec::new()
    }

    // No value of the type exists to ask.
    fn id(&self) -> &'static str {
        ""
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn decode(
        id: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, EffectDecodeError> {
        Err(EffectDecodeError::Unknown(id.to_owned()))
    }
}

/// Read one argument of an effect being decoded. An absent argument reads
/// as `null`, which an `Option` field takes as `None`.
#[doc(hidden)]
pub fn argument<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<T, EffectDecodeError> {
    // Only the decode is per type: the lookup and the message are compiled
    // once for every argument of every effect.
    T::deserialize(argument_value(arguments, name)).map_err(|error| EffectDecodeError::Argument {
        name: name.to_owned(),
        source: error,
    })
}

/// The effect's `#[param(flatten)]` struct, decoded from the invocation's
/// whole object. A field that does not decode is named by its path within
/// that object (`path`, or `adjust.grayscale` for one of a struct's
/// fields), so the caller hears the argument to fix rather than the struct
/// that carries it; `struct_name` names the field itself for a failure that
/// is not one field's, such as an unknown one.
#[doc(hidden)]
pub fn flattened_argument<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Map<String, serde_json::Value>,
    struct_name: &str,
) -> Result<T, EffectDecodeError> {
    // The path and the message live here, not in every effect's generated
    // decode.
    serde_path_to_error::deserialize(serde_json::Value::Object(arguments.clone())).map_err(
        |error| {
            let path = error.path().to_string();
            EffectDecodeError::Argument {
                name: if path.is_empty() {
                    struct_name.to_owned()
                } else {
                    path
                },
                source: error.into_inner(),
            }
        },
    )
}

/// The identity argument `name`: canonical decimal text, as a record and a
/// surface name their windows, strips, groups and workspaces by. A salted id
/// outgrows what a JSON number carries exactly, so it travels as text; any
/// other text names no id, and is refused here rather than taken for one.
pub fn identity_argument(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<String, EffectDecodeError> {
    match argument_value(arguments, name) {
        serde_json::Value::String(text) => identity(name, &text),
        serde_json::Value::Null => Err(EffectDecodeError::Argument {
            name: name.to_owned(),
            source: serde::de::Error::custom("is required"),
        }),
        other => Err(not_an_identity(name, other.to_string())),
    }
}

/// [`identity_argument`] for an identity that need not be given.
pub fn optional_identity_argument(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<String>, EffectDecodeError> {
    match argument_value(arguments, name) {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(text) => identity(name, &text).map(Some),
        other => Err(not_an_identity(name, other.to_string())),
    }
}

/// `text` as an identity of `name`, its decimal form being its own.
fn identity(name: &str, text: &str) -> Result<String, EffectDecodeError> {
    let canonical = text.parse::<u64>().is_ok_and(|id| id.to_string() == text);
    if canonical {
        return Ok(text.to_owned());
    }
    Err(not_an_identity(name, text.to_owned()))
}

/// Why `value`, given for the identity `name`, names no id.
fn not_an_identity(name: &str, value: String) -> EffectDecodeError {
    EffectDecodeError::Identity {
        name: name.to_owned(),
        value,
    }
}

/// The argument `name`, or `null` when it is absent.
#[inline(never)]
fn argument_value(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> serde_json::Value {
    arguments
        .get(name)
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Put one argument of an effect being encoded; an absent optional one
/// (`null`) is left out, as a caller leaves it out.
#[doc(hidden)]
pub fn put<T: serde::Serialize>(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    value: &T,
) {
    match crate::__private::json_via_bytes(value) {
        Ok(serde_json::Value::Null) | Err(_) => {}
        Ok(value) => {
            arguments.insert(name.to_owned(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Effect;

    /// What the derive must produce, checked through the spec a caller sees.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.effects", catalogue = "tests/locales")]
    enum Sample {
        /// Reload everything.
        #[effect(kind = "outside")]
        Reload,
        /// Rename a thing.
        ///
        /// It keeps its id.
        #[effect(returns, kind = "read")]
        RenameThing {
            #[param("The thing")]
            thing: String,
            label: Option<String>,
        },
        #[effect(kind = "outside")]
        #[effect(job, cancellable, destructive)]
        Wipe { passes: u32 },
        /// Copy a thing's key.
        #[effect(privileged, kind = "read")]
        CopyKey { thing: String },
        /// Paint a thing.
        #[effect(kind = "outside")]
        Paint {
            #[param("The colour", choices = ["red", "green"])]
            colour: String,
            // `Self` in an attribute reaches the rows' static as the enum.
            #[param(choices = Self::SHADES)]
            shade: Option<String>,
        },
        /// Widen a column.
        #[effect(kind = "outside")]
        Widen {
            #[param(decimal, "The column")]
            column: String,
            #[param(decimal, "The table")]
            table: Option<String>,
            #[param("The width")]
            width: u32,
        },
    }

    impl Sample {
        const SHADES: [&str; 2] = ["light", "dark"];
    }

    /// A content transform declared offloadable.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.transforms", catalogue = "tests/locales")]
    enum Transforms {
        /// Shrink images.
        #[effect(
            content,
            idempotent,
            kind = "read",
            offload(cpu_ms = 60000, memory_mb = 512, output_mb = 8)
        )]
        Shrink {
            #[param(content, "The images")]
            images: Vec<String>,
        },
        /// Copy an image.
        #[effect(content, kind = "read")]
        Copy {
            #[param(content, "The image")]
            image: String,
        },
    }

    tilo_test_env::scenario! {
        fn an_effect_declares_publishes_and_binds_what_its_type_says;

        phase an_offload_option_declares_a_valid_offloadable_contract;
        {
            let specs = Transforms::specs();
            let shrink = &specs[0];
            assert_eq!(
                shrink.contract.offload,
                Some(tilo_actions::Offload {
                    gpu: false,
                    cpu_time_ms: 60_000,
                    memory_bytes: 512 * 1024 * 1024,
                    output_bytes: 8 * 1024 * 1024,
                })
            );
            assert_eq!(shrink.contract.kind, ActionKind::Read);
            assert!(
                shrink.contract.validate_declaration().is_empty(),
                "{:?}",
                shrink.contract.validate_declaration()
            );
            assert_eq!(specs[1].contract.offload, None, "only where declared");
            assert!(shrink.params()[0].is_content_list());
            assert!(specs[1].params()[0].is_content());
        }

        phase a_derived_vocabulary_publishes_each_variant_and_round_trips_through_its_spec;
        {
            let specs = Sample::specs();
            let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    "reload",
                    "rename-thing",
                    "wipe",
                    "copy-key",
                    "paint",
                    "widen"
                ]
            );

            let rename = &specs[1];
            assert_eq!(rename.label, "Rename a thing");
            assert_eq!(
                rename.description.as_deref(),
                Some("Rename a thing. It keeps its id.")
            );
            assert_eq!(rename.kind, OperationKind::Effect);
            assert_eq!(rename.contract.kind, ActionKind::Read);
            assert!(matches!(rename.contract.result, ResultSpec::Json { .. }));
            assert_eq!(rename.contract.execution, ExecutionMode::Immediate);

            // A parameter carries its type's schema, as a verb's does: it is
            // what a caller of another language reads of the argument.
            let schema_type = |param: &tilo_actions::ActionParam| {
                param
                    .schema
                    .as_deref()
                    .and_then(|schema| schema.get("type"))
                    .cloned()
            };
            assert_eq!(
                schema_type(&rename.params()[0]),
                Some(serde_json::json!("string"))
            );
            assert_eq!(
                schema_type(&specs[2].params()[0]),
                Some(serde_json::json!("integer"))
            );

            let paint = &specs[4];
            assert_eq!(paint.params()[0].description, "The colour");
            let choices = |index: usize| -> Vec<&str> {
                paint.params()[index]
                    .choices
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect()
            };
            assert_eq!(choices(0), ["red", "green"]);
            assert_eq!(choices(1), ["light", "dark"]);
            assert!(!paint.params()[1].required);

            let wipe = &specs[2];
            assert_eq!(wipe.label, "Wipe");
            assert_eq!(wipe.contract.risk, RiskLevel::Destructive);
            assert_eq!(wipe.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(
                wipe.contract.execution,
                ExecutionMode::Background { cancellable: true }
            );

            // A caller's arguments, encoded by the published spec, decode to the
            // value the handler receives.
            let value = Sample::RenameThing {
                thing: "a".into(),
                label: None,
            };
            let encoded = rename.build_json(&value.arguments()).unwrap();
            assert_eq!(encoded.type_id, "test.effects");
            assert_eq!(Sample::from_payload(&encoded.payload).unwrap(), value);
            assert_eq!(
                Sample::from_payload(
                    &specs[0]
                        .build_json(&serde_json::Map::new())
                        .unwrap()
                        .payload
                )
                .unwrap(),
                Sample::Reload
            );
            assert!(Sample::decode("wipe", &serde_json::Map::new()).is_err());

            let bound = Sample::Wipe { passes: 3 }.bound();
            assert_eq!(bound.id, "wipe");
            assert_eq!(bound.label, "Wipe");
            assert!(bound.params().is_empty());
            assert_eq!(bound.contract.confirmation, ConfirmationPolicy::Confirm);
            let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(
                Sample::from_payload(&encoded.payload).unwrap(),
                Sample::Wipe { passes: 3 }
            );
            assert!(Sample::decode("format", &serde_json::Map::new()).is_err());

            let asking = Sample::RenameThing {
                thing: "a".into(),
                label: None,
            }
            .bound_asking(&["label"]);
            assert_eq!(asking.id, "rename-thing");
            let asked: Vec<&str> = asking.params().iter().map(|param| &*param.name).collect();
            assert_eq!(asked, ["label"]);
            let mut label = serde_json::Map::new();
            label.insert("label".into(), "b".into());
            let encoded = asking.build_json(&label).unwrap();
            assert_eq!(
                Sample::from_payload(&encoded.payload).unwrap(),
                Sample::RenameThing {
                    thing: "a".into(),
                    label: Some("b".into()),
                }
            );
        }

        phase an_identity_argument_is_decimal_text_and_is_refused_otherwise;
        // An identity travels as decimal text, and one that names no id is
        // refused when the invocation is encoded: the API answers a caller that
        // names something the effect cannot act on, rather than a host failing
        // to read it.
        {
            let widen = Sample::spec("widen").unwrap();
            let encoded = |arguments: serde_json::Value| -> Result<String, String> {
                let arguments: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_value(arguments).expect("a test's call is an object");
                widen
                    .build_json(&arguments)
                    .map(|built| built.type_id)
                    .map_err(|error| error.to_string())
            };
            assert_eq!(
                encoded(serde_json::json!({"column": "9007199254740993", "width": 80})),
                Ok("test.effects".to_owned()),
                "a salted id outgrows a JSON number, and travels as text"
            );
            for refused in [
                serde_json::json!({"column": "left", "width": 80}),
                serde_json::json!({"column": "07", "width": 80}),
                serde_json::json!({"column": "1", "table": "wide", "width": 80}),
            ] {
                assert_eq!(
                    encoded(refused.clone()),
                    Ok(tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned()),
                    "{refused}"
                );
            }
            let not_text = encoded(serde_json::json!({"column": 7, "width": 80}));
            assert!(
                not_text.is_err(),
                "an id is text, never a number: {not_text:?}"
            );
        }

        phase views_publish_as_views_and_row_effects_only_bound_to_their_item;
        {
            let specs = Rows::specs();
            let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
            assert_eq!(
                ids,
                ["search"],
                "a row effect is not offered on the resource"
            );
            let search = &specs[0];
            assert_eq!(search.kind, OperationKind::View);
            assert_eq!(search.contract.kind, ActionKind::Read);
            assert_eq!(search.contract.risk, RiskLevel::None);
            assert_eq!(search.contract.retry, RetryPolicy::Idempotent);

            let open = Rows::Open { item: "a".into() };
            let bound = open.bound();
            assert_eq!(
                (bound.id.as_str(), bound.kind),
                ("open", OperationKind::View)
            );
            assert!(bound.params().is_empty());
            let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(encoded.type_id, "test.rows");
            assert_eq!(Rows::from_payload(&encoded.payload).unwrap(), open);

            let connect = Rows::Connect { item: "b".into() }.bound();
            assert_eq!(connect.kind, OperationKind::Effect);
            assert_eq!(connect.contract.kind, ActionKind::Outside);
            assert_eq!(connect.contract.retry, RetryPolicy::Idempotent);
            assert_eq!(
                connect.contract.execution,
                ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(
                Sample::specs()[0].contract.retry,
                RetryPolicy::AtMostOnce,
                "an effect is not repeated unless it says it may be"
            );
        }

        phase a_privileged_effect_asks_first_and_binds_to_the_entity_it_names;
        {
            let copy = Sample::CopyKey { thing: "a".into() };
            let bound = copy.bound();
            assert_eq!(bound.id, "copy-key");
            assert_eq!(bound.label, "Copy a thing's key");
            assert_eq!(bound.kind, OperationKind::Effect);
            assert!(bound.params().is_empty(), "the row names the thing already");
            assert_eq!(bound.contract.risk, RiskLevel::Privileged);
            assert_eq!(bound.contract.confirmation, ConfirmationPolicy::Confirm);
            assert_eq!(bound.contract.kind, ActionKind::Read);
            let encoded = bound.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(encoded.type_id, "test.effects");
            assert_eq!(Sample::from_payload(&encoded.payload).unwrap(), copy);
        }

        phase a_sensitive_effect_asks_first;
        {
            let spec = Exports::ExportBook.bound();
            assert_eq!(spec.contract.risk, RiskLevel::Sensitive);
            assert_eq!(spec.contract.confirmation, ConfirmationPolicy::Confirm);
        }

        phase a_secret_parameter_is_published_as_a_secret;
        {
            let specs = Pairing::specs();
            let params = specs[0].params();
            assert_eq!(params[0].ty, tilo_actions::ParamType::Secret);
            assert_eq!(&*params[0].description, "The invite the other device shows");
            assert_eq!(params[1].ty, tilo_actions::ParamType::Secret);
            assert!(!params[1].required);
            let value = Pairing::Pair {
                invite: "grayjay://invite".into(),
                pin: None,
            };
            let encoded = specs[0].build_json(&value.arguments()).unwrap();
            assert_eq!(Pairing::from_payload(&encoded.payload).unwrap(), value);
        }

        phase a_path_parameter_refuses_a_relative_path;
        {
            let specs = Files::specs();
            let params = specs[0].params();
            assert_eq!(params[0].ty, tilo_actions::ParamType::Text);
            assert!(params[0].required);
            assert_eq!(params[1].ty, tilo_actions::ParamType::Text);
            assert!(!params[1].required);
            let arguments = |path: &str, folder: Option<&str>| {
                let mut arguments = serde_json::Map::new();
                arguments.insert("path".into(), path.into());
                if let Some(folder) = folder {
                    arguments.insert("folder".into(), folder.into());
                }
                arguments
            };
            let error = Files::decode("save-copy", &arguments("copy.txt", None)).unwrap_err();
            assert!(
                matches!(
                    &error,
                    EffectDecodeError::Argument { name, source }
                        if name == "path" && source.to_string().contains("absolute")
                ),
                "{error}"
            );
            assert!(Files::decode("save-copy", &arguments("/a/copy.txt", Some("b"))).is_err());
            let value = Files::decode("save-copy", &arguments("/a/copy.txt", Some("/b"))).unwrap();
            let encoded = specs[0].build_json(&value.arguments()).unwrap();
            assert_eq!(Files::from_payload(&encoded.payload).unwrap(), value);
        }

        phase variant_refs_bind_controls_by_id;
        {
            assert_eq!(Sample::RENAME_THING.id, "rename-thing");
            assert_eq!(
                Sample::RENAME_THING
                    .arguments(&("b".to_owned(), Some("B".to_owned())))
                    .unwrap(),
                Sample::RenameThing {
                    thing: "b".into(),
                    label: Some("B".into())
                }
                .arguments()
            );
        }
    }

    /// A vocabulary with presentation-only effects and effects offered only
    /// on the items they act on.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.rows", catalogue = "tests/locales")]
    enum Rows {
        /// Open an item.
        #[effect(view, row)]
        Open { item: String },
        /// Connect to an item.
        #[effect(kind = "outside")]
        #[effect(row, job, idempotent)]
        Connect { item: String },
        /// Search.
        #[effect(view)]
        Search,
    }

    /// A copy of private data leaving the app asks first, without being a
    /// credential or a deletion.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.exports", catalogue = "tests/locales")]
    enum Exports {
        /// Export the address book.
        #[effect(kind = "outside")]
        #[effect(job, sensitive)]
        ExportBook,
    }

    /// A secret argument is published as one, and still reaches the handler.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.secrets", catalogue = "tests/locales")]
    enum Pairing {
        /// Pair a device.
        #[effect(kind = "outside")]
        Pair {
            #[param(secret, "The invite the other device shows")]
            invite: String,
            #[param(secret)]
            pin: Option<String>,
        },
    }

    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.files", catalogue = "tests/locales")]
    enum Files {
        /// Save a copy.
        #[effect(kind = "outside")]
        SaveCopy {
            #[param("Where to write it")]
            path: crate::AbsolutePath,
            folder: Option<crate::AbsolutePath>,
        },
    }

    /// The ids a device mints: a desktop id carries its salt in the high 16
    /// bits, so more than half of them outgrow an `i64`.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.ids", catalogue = "tests/locales")]
    enum Ids {
        /// Stand a resource in a strip.
        #[effect(kind = "undoable")]
        Place {
            #[param("The strip it stands in")]
            strip: u64,
            #[param("The place's own id; a new one when left out")]
            place: Option<u64>,
            #[param(resource = "", "The resource it shows")]
            resource: String,
        },
    }

    /// A caller encodes an invocation through the published spec and its
    /// handler decodes it there: one declaration writes both sides, so what a
    /// caller gives — an id past `i64::MAX` included — is what the effect reads.
    #[test]
    fn an_effects_named_arguments_round_trip_through_its_spec() {
        let spec = Ids::specs().remove(0);
        let salted = 0xB3A1_0000_0000_0001_u64;
        let arguments = serde_json::json!({
            "strip": salted,
            "place": salted,
            "resource": "notes:1",
        })
        .as_object()
        .expect("arguments")
        .clone();
        let encoded = spec.build_json(&arguments).expect("the place encodes");
        assert_eq!(
            encoded.type_id,
            Ids::OPERATION,
            "the place is encoded as its own operation, not a refusal: {:?}",
            String::from_utf8_lossy(&encoded.payload)
        );
        assert_eq!(
            Ids::from_payload(&encoded.payload).expect("the place decodes"),
            Ids::Place {
                strip: salted,
                place: Some(salted),
                resource: "notes:1".into(),
            }
        );
    }

    /// A content transform whose inputs are the bytes themselves.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.readers", catalogue = "tests/locales")]
    enum Readers {
        /// Open documents.
        #[effect(kind = "read")]
        Open {
            #[param(content, "The documents")]
            documents: Vec<crate::Bytes>,
        },
    }

    /// An effect is never replayed: the content its arguments name is read
    /// where the effect runs. A caller's address — a `tilo.content:<hash>`
    /// an operation collecting content answers, or the reference alone, as
    /// requests named it before content was sealed — decodes into the
    /// reference that opens exactly those bytes.
    #[test]
    fn an_effects_content_argument_is_read_where_the_effect_runs() {
        let window = tilo_wake::WakeScope::new();
        let workspace = tilo_content::ContentStore::memory_in_workspace().sealing();
        crate::content::register(&window.handle(), workspace.clone());
        let _entered = window.enter();
        let documents: Vec<Vec<u8>> = [7_u8, 8].into_iter().map(|byte| vec![byte; 4096]).collect();
        let references: Vec<String> = documents
            .iter()
            .map(|bytes| workspace.put(bytes).expect("kept"))
            .collect();
        let arguments = serde_json::Map::from_iter([(
            "documents".to_owned(),
            serde_json::json!([
                tilo_actions::content_resource(&references[0]).expect("an address"),
                references[1].clone(),
            ]),
        )]);
        let spec = Readers::specs().remove(0);
        assert!(spec.params()[0].is_content_list());
        let encoded = crate::content::invoking(|| spec.build_json(&arguments).unwrap());

        let Readers::Open { documents: read } =
            Readers::from_payload(&encoded.payload).expect("read where it runs");
        assert_eq!(read.len(), 2);
        for (read, kept) in read.iter().zip(&references) {
            assert_eq!(
                read.reference().as_deref(),
                Some(kept.as_str()),
                "the reference that opens exactly those bytes"
            );
            assert!(read.read(&workspace).is_some(), "and the bytes themselves");
        }
    }
}
