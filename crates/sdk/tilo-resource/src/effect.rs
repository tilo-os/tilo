//! Effects: work an app does that changes no document — reloading a list,
//! running a job, talking to a server, opening a system picker.
//!
//! An app names its effects as one typed enum and derives [`Effect`]:
//!
//! ```
//! use tilo_resource::Effect;
//!
//! #[derive(Clone, Debug, Effect)]
//! #[effect(operation = "example.backups")]
//! pub enum Backups {
//!     /// Back up now.
//!     #[effect(job, cancellable)]
//!     BackUp,
//!     /// Restore a snapshot.
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
//! A variant that only changes what this participant sees — open a snippet,
//! reveal a tag, show a result — is a `view`: published as a `View`
//! operation (any reader may invoke it, a collaborator may not move someone
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
//! `own_device`: a room's grants have nothing to consent to, so a guest
//! invokes it as the room's owner does, and it runs only for the person at
//! the device it acts on ([`tilo_actions::OWN_DEVICE_SCOPE`]). An effect
//! that reaches other people (a direct message, a call) is not.
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
//! Variant options: `label = "…"` (else the first doc line), `view` (changes
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
//! disk can, and another disk need not), `privileged` (hands out or shows a
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
//! `participant` (a view that moves only the invoking participant's own
//! view, such as an agent's caret, so a remote participant may invoke it),
//! `member` (any member of a room may ask the room's host to run it for
//! themselves on the room's record, as linking their own account elsewhere),
//! `host_render` (a host answers it in a render service of its own, never in
//! the record's confined app worker: [`tilo_actions::HOST_RENDER_TAG`]),
//! `tag = EXPR` (repeatable: a further contract tag, a `&'static str`
//! constant, for how a host treats the effect),
//! `outside_with = "path"` (an effect of `kind = "read"` that reaches outside
//! the record only when that parameter is given: an export that writes the
//! file it names, else keeps what it made as content; each invocation is
//! admitted as what it does)
//! and
//! `kind = "read"` (an effect that only reads, which whoever may read may
//! invoke; an effect reaches outside the record by default, a view reads),
//! and `offload(app = "…", entrypoint = "…", cpu_ms = N, memory_mb = N,
//! output_mb = N, gpu)` (a `content`, `kind = "read"` transform of content
//! inputs whose outputs depend on the inputs alone: the system may run it on
//! the invoker's best-placed device as that signed package's catalogue
//! entrypoint, and the handler is its local run; [`tilo_actions::Offload`]). Field
//! `#[param("…")]` describes a parameter; `#[param(secret, "…")]` makes it a
//! secret the host keeps out of logs, history and confirmations, and
//! `#[param("…", choices = EXPR)]` names the values it takes when they are a
//! fixed list (`EXPR` yields strings: a constant table, an array of names).

use tilo_actions::{
    ActionContract, ActionKind, ActionParam, ActionSpec, ConfirmationPolicy, EncodedAction,
    ExecutionMode, OperationKind, ResultSpec, RetryPolicy, RiskLevel,
};

/// A typed vocabulary of effects. Derive it (`#[derive(Effect)]`); see the
/// [module docs](self).
pub trait Effect: Sized + 'static {
    /// The operation type every effect of the vocabulary travels under:
    /// unique per vocabulary, like a resource's kind.
    const OPERATION: &'static str;

    /// One declaration per effect, in order.
    fn declarations() -> Vec<EffectDecl>;

    /// The declaration of the effect `id`, built alone: what binding one
    /// effect to an item reads, so offering it on many items costs one
    /// declaration each, not the whole vocabulary.
    fn declaration(id: &str) -> Option<EffectDecl> {
        Self::declarations().into_iter().find(|decl| decl.id == id)
    }

    /// This effect's id among the declarations.
    fn id(&self) -> &'static str;

    /// This effect's arguments, named as its declaration's parameters.
    fn arguments(&self) -> serde_json::Map<String, serde_json::Value>;

    /// The effect `id` with `arguments`, as a caller invoked it.
    ///
    /// # Errors
    /// Returns why the id or the arguments do not name an effect.
    fn decode(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String>;

    /// The published action of every effect offered on the resource: all
    /// but the `row` ones, which are offered only [bound](Self::bound).
    fn specs() -> Vec<ActionSpec> {
        kept_specs(
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
        bound_spec(Self::OPERATION, id, Self::declaration(id), self.arguments())
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
            Some(decl) => asking_spec(Self::OPERATION, &decl, params, self.arguments()),
            None => self.bound(),
        }
    }

    /// The effects offered on the item `item` of the collection `field`
    /// (`#[effect(item = field)]`), each with its first parameter bound to
    /// the item and the rest still asked.
    fn item_specs(field: &str, item: &crate::Id) -> Vec<ActionSpec> {
        item_specs(Self::declarations(), Self::OPERATION, field, item)
    }

    /// This effect as the published resource `resource` offers it (the
    /// record, or a child or live row it is bound on, by its published id):
    /// what a search hit invokes or a caller addresses through the host's
    /// door. Its id comes from the effect itself.
    fn address_on(&self, resource: impl Into<String>) -> tilo_actions::ActionAddress {
        tilo_actions::ActionAddress::new(resource, self.id())
    }

    /// Decode one invocation's payload, as the operation's specs encode it.
    ///
    /// # Errors
    /// Returns why the payload is not an effect of this vocabulary.
    fn from_payload(payload: &[u8]) -> Result<Self, String> {
        let Payload { effect, args } = decode_payload(payload)?;
        Self::decode(&effect, &args)
    }

    /// This effect encoded as its operation's payload: the one encoding every
    /// action spec, bound control and console door produces, for a caller
    /// that already holds the typed effect rather than the arguments a caller
    /// would bind.
    #[must_use]
    fn encoded(&self) -> EncodedAction {
        encode_effect(Self::OPERATION, self.id(), self.arguments())
    }
}

/// [`Effect::encoded`]: the one invocation payload of `id` under `operation`.
fn encode_effect(
    operation: &'static str,
    id: &str,
    args: serde_json::Map<String, serde_json::Value>,
) -> EncodedAction {
    EncodedAction::encode_operation(
        operation,
        &Payload {
            effect: id.to_owned(),
            args,
        },
    )
    .unwrap_or_else(|_| EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: Vec::new(),
    })
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
fn decode_payload(payload: &[u8]) -> Result<Payload, String> {
    // Report the category only: serde's message can quote the offending
    // input, and an effect may carry a secret.
    tilo_actions::decode_json(payload).map_err(|error| {
        let category = match error.classify() {
            serde_json::error::Category::Io => "unreadable payload",
            serde_json::error::Category::Syntax => "malformed payload",
            serde_json::error::Category::Data => "unexpected fields or values",
            serde_json::error::Category::Eof => "truncated payload",
        };
        format!("not an effect invocation: {category}")
    })
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

/// The one encoder of every effect spec: the invocation of effect `effect`
/// under `operation`, with its arguments made as `mode` says of `bound` and
/// the caller's.
fn encoder(
    operation: &'static str,
    effect: &'static str,
    bound: serde_json::Map<String, serde_json::Value>,
    mode: Bound,
) -> impl Fn(&tilo_actions::ActionArgs) -> EncodedAction + Send + Sync + 'static {
    move |args| {
        let args = match mode {
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
        };
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

/// [`Effect::specs`] of the declarations `decls`.
/// [`Effect::specs`] of the vocabulary `key`. They depend on its type alone
/// and are published on every refresh of a view's actions, so each thread
/// builds them once per generation of app code
/// ([`reset_type_schemas`](crate::reset_type_schemas)) and hands out copies.
fn kept_specs(
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
    let epoch = crate::resource::type_epoch();
    let found = KEPT.with(|kept| {
        let kept = kept.borrow();
        if kept.0 == epoch {
            kept.1.get(&key).cloned()
        } else {
            None
        }
    });
    let specs = found.unwrap_or_else(|| {
        let built: std::rc::Rc<[ActionSpec]> = offered_specs(declarations(), operation).into();
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

fn offered_specs(decls: Vec<EffectDecl>, operation: &'static str) -> Vec<ActionSpec> {
    decls
        .into_iter()
        .filter(|decl| !decl.row)
        .map(|decl| decl.spec(operation))
        .collect()
}

/// [`Effect::bound`] of the effect `id`, declared as `decl`, with `args`.
fn bound_spec(
    operation: &'static str,
    id: &'static str,
    decl: Option<EffectDecl>,
    args: serde_json::Map<String, serde_json::Value>,
) -> ActionSpec {
    let fixed = args.clone().into_iter().collect();
    let build = encoder(operation, id, args, Bound::Only);
    match decl {
        Some(decl) => decl.spec_with(operation, Vec::new(), build),
        None => ActionSpec::typed_operation(
            id,
            id,
            id,
            OperationKind::Effect,
            operation,
            Vec::new(),
            build,
        ),
    }
    .with_bound(fixed)
}

/// [`Effect::bound_asking_params`] of the effect `decl` declares, bound to
/// `args`, asking `params`.
fn asking_spec(
    operation: &'static str,
    decl: &EffectDecl,
    params: Vec<ActionParam>,
    args: serde_json::Map<String, serde_json::Value>,
) -> ActionSpec {
    let fixed = args
        .iter()
        .filter(|(name, _)| !params.iter().any(|param| param.name == name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let build = encoder(operation, decl.id, args, Bound::UnderCaller);
    decl.spec_with(operation, params, build).with_bound(fixed)
}

/// [`Effect::item_specs`] of the declarations `decls`.
fn item_specs(
    decls: Vec<EffectDecl>,
    operation: &'static str,
    field: &str,
    item: &crate::Id,
) -> Vec<ActionSpec> {
    decls
        .into_iter()
        .filter(|decl| decl.item == Some(field))
        .filter_map(|decl| {
            let (first, rest) = decl.params.split_first()?;
            let bound = serde_json::Map::from_iter([(
                first.name.to_string(),
                serde_json::Value::String(item.as_str().to_owned()),
            )]);
            let fixed = bound.clone().into_iter().collect();
            let build = encoder(operation, decl.id, bound, Bound::OverCaller);
            Some(
                decl.spec_with(operation, rest.to_vec(), build)
                    .with_bound(fixed),
            )
        })
        .collect()
}

/// One effect of a derived vocabulary, as a constant: its declaration but for
/// the parameters, which [`Self::params`] builds. `#[derive(Effect)]` writes
/// one per variant; [`EffectDecl::of_rows`] and [`EffectDecl::of_row`] read
/// them, the same code for every vocabulary.
#[doc(hidden)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property a declaration states, as the derive's options do"
)]
pub struct EffectRow {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: fn() -> Vec<ActionParam>,
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
    pub participant: bool,
    pub edits: bool,
    pub member: bool,
    pub item: Option<&'static str>,
    pub offload: Option<EffectOffload>,
    pub tags: &'static [&'static str],
    pub outside_with: Option<&'static str>,
}

/// An effect's `#[effect(offload(...))]`: the signed package entrypoint
/// that performs the same content transform on another of the invoker's
/// devices, and the bounds of the work ([`tilo_actions::Offload`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectOffload {
    /// The signed package whose catalogue entrypoint performs the transform.
    pub app: &'static str,
    /// The package's catalogue (CLI) command that performs it.
    pub entrypoint: &'static str,
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
            app: self.app.to_owned(),
            entrypoint: self.entrypoint.to_owned(),
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

/// The declaration of one effect: what callers see of it.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent property a declaration states, as the derive's options do"
)]
pub struct EffectDecl {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: Vec<ActionParam>,
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
    /// nothing shared and no one else, so a room's grants have nothing to
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
    /// A view that changes only the invoking participant's own view (an
    /// agent's caret, its Find), so a remote participant may invoke it
    /// without moving anybody else's.
    pub participant: bool,
    /// A view of the invoker that submits only edits of this record (typing
    /// at the invoker's caret): its kind is Undoable, offered to whoever may
    /// edit, and the edits it submits are checked like any other.
    pub edits: bool,
    /// Any member of a room may ask the room's host to run it for
    /// themselves on the room's record (`#[effect(member)]`,
    /// [`tilo_actions::MEMBER_OWN_TAG`]): it runs there as that member.
    pub member: bool,
    /// The collection field whose items the effect is offered on
    /// (`#[effect(item = connections)]`), its first parameter bound to the
    /// item's id: published on each item resource, as an item verb is.
    pub item: Option<&'static str>,
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
    /// The published action of this effect under `operation`.
    pub fn spec(&self, operation: &'static str) -> ActionSpec {
        let build = encoder(operation, self.id, serde_json::Map::new(), Bound::None);
        self.spec_with(operation, self.params.clone(), build)
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

    fn of_row(row: &EffectRow) -> Self {
        Self {
            id: row.id,
            label: row.label,
            description: row.description,
            params: (row.params)(),
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
            participant: row.participant,
            edits: row.edits,
            member: row.member,
            item: row.item,
            offload: row.offload,
            tags: row.tags,
            outside_with: row.outside_with,
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

    fn spec_with(
        &self,
        operation: &'static str,
        params: Vec<ActionParam>,
        build: impl Fn(&tilo_actions::ActionArgs) -> EncodedAction + Send + Sync + 'static,
    ) -> ActionSpec {
        let view = self.kind == OperationKind::View;
        let mut spec = ActionSpec::typed_operation(
            self.id,
            self.label,
            self.label,
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
        let base = if view {
            ActionContract::view()
        } else {
            ActionContract::default()
        };
        spec.contract = ActionContract {
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
            result: match self.answers {
                Answers::Later { returns: false, .. } => ResultSpec::Job,
                Answers::Value | Answers::Later { returns: true, .. } => ResultSpec::Json {
                    schema: serde_json::json!({}),
                },
                Answers::Content => ResultSpec::Resource {
                    resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
                },
                Answers::Nothing => ResultSpec::None,
            },
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
                if self.participant {
                    tags.push(tilo_actions::PARTICIPANT_VIEW_TAG.to_owned());
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

    // No value of the type exists to ask.
    fn id(&self) -> &'static str {
        ""
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    fn decode(id: &str, _: &serde_json::Map<String, serde_json::Value>) -> Result<Self, String> {
        Err(format!("there is no effect `{id}`"))
    }
}

/// Read one argument of an effect being decoded. An absent argument reads
/// as `null`, which an `Option` field takes as `None`.
///
/// # Errors
/// Returns why the argument does not have the field's type.
#[doc(hidden)]
pub fn argument<T: serde::de::DeserializeOwned>(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<T, String> {
    serde_json::from_value(
        arguments
            .get(name)
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|error| format!("`{name}`: {error}"))
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
    #[effect(operation = "test.effects")]
    enum Sample {
        /// Reload everything.
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
        #[effect(label = "Wipe", job, cancellable, destructive)]
        Wipe { passes: u32 },
        /// Copy a thing's key.
        #[effect(privileged, kind = "read")]
        CopyKey { thing: String },
        /// Paint a thing.
        Paint {
            #[param("The colour", choices = ["red", "green"])]
            colour: String,
            // `Self` in an attribute reaches the rows' static as the enum.
            #[param(choices = Self::SHADES)]
            shade: Option<String>,
        },
    }

    impl Sample {
        const SHADES: [&str; 2] = ["light", "dark"];
    }

    /// A content transform declared offloadable.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.transforms")]
    enum Transforms {
        /// Shrink images.
        #[effect(
            content,
            idempotent,
            kind = "read",
            offload(
                app = "so.tilo.shrink",
                entrypoint = "shrink",
                cpu_ms = 60000,
                memory_mb = 512,
                output_mb = 8
            )
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

    #[test]
    fn an_offload_option_declares_a_valid_offloadable_contract() {
        let specs = Transforms::specs();
        let shrink = &specs[0];
        assert_eq!(
            shrink.contract.offload,
            Some(tilo_actions::Offload {
                app: "so.tilo.shrink".into(),
                entrypoint: "shrink".into(),
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

    #[test]
    fn a_derived_vocabulary_publishes_each_variant_and_round_trips_through_its_spec() {
        let specs = Sample::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, ["reload", "rename-thing", "wipe", "copy-key", "paint"]);

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

    /// A vocabulary with presentation-only effects and effects offered only
    /// on the items they act on.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.rows")]
    enum Rows {
        /// Open an item.
        #[effect(view, row)]
        Open { item: String },
        /// Connect to an item.
        #[effect(row, job, idempotent)]
        Connect { item: String },
        /// Search.
        #[effect(view)]
        Search,
    }

    #[test]
    fn views_publish_as_views_and_row_effects_only_bound_to_their_item() {
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

    #[test]
    fn a_privileged_effect_asks_first_and_binds_to_the_entity_it_names() {
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

    /// A copy of private data leaving the app asks first, without being a
    /// credential or a deletion.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.exports")]
    enum Exports {
        /// Export the address book.
        #[effect(job, sensitive)]
        ExportBook,
    }

    #[test]
    fn a_sensitive_effect_asks_first() {
        let spec = Exports::ExportBook.bound();
        assert_eq!(spec.contract.risk, RiskLevel::Sensitive);
        assert_eq!(spec.contract.confirmation, ConfirmationPolicy::Confirm);
    }

    /// A secret argument is published as one, and still reaches the handler.
    #[derive(Clone, Debug, PartialEq, Effect)]
    #[effect(operation = "test.secrets")]
    enum Pairing {
        /// Pair a device.
        Pair {
            #[param(secret, "The invite the other device shows")]
            invite: String,
            #[param(secret)]
            pin: Option<String>,
        },
    }

    #[test]
    fn a_secret_parameter_is_published_as_a_secret() {
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

    #[test]
    fn variant_refs_bind_controls_by_id() {
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
